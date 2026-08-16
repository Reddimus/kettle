//! Per-window state (C1 of the in-process multi-window refactor).
//!
//! Everything that belongs to ONE OS window — the winit window handle, its
//! GPU renderer, its tab/pane multiplexer, and all input/overlay/animation
//! state — lives here. `App` (app.rs) keeps only process-global state (config,
//! event-loop proxy, control server, Lua engine, watchers) plus a
//! `BTreeMap<u64, WindowState>` keyed by a stable per-window sequence number.
//!
//! Dispatch contract: the `ApplicationHandler` entry points in app.rs REMOVE
//! the addressed window from the map, run the inner handler with disjoint
//! `&mut App` (globals) + `&mut WindowState` borrows, then reinsert it. That
//! take-out/put-back shape is what lets helper methods stay on `impl App`
//! (they receive `ws: &mut WindowState`) without aliasing the map.

use std::sync::Arc;

use winit::dpi::PhysicalPosition;
use winit::event::ElementState;
use winit::keyboard::{ModifiersState, PhysicalKey};
use winit::window::{CursorIcon, Window};

use kettle_render::Renderer;

use crate::app::{
    ConfirmDialogState, ContextMenuState, HintTarget, LinksScanKey, PaneDrag, SplitDrag,
    TitleEditState, ViState,
};
use crate::mux::Mux;
use crate::search_input::SearchState;

/// Track key presses consumed by Kettle UI and swallow only their matching
/// release. Kitty's event-type reporting makes releases observable by the
/// child process, so letting a UI-owned press disappear while its release leaks
/// would leave applications with an impossible input sequence.
pub(crate) fn track_consumed_key_release(
    suppressed: &mut std::collections::HashSet<PhysicalKey>,
    physical_key: PhysicalKey,
    state: ElementState,
    press_consumed: bool,
) -> bool {
    match state {
        ElementState::Pressed => {
            if press_consumed {
                suppressed.insert(physical_key);
            } else {
                // A held key can change ownership between auto-repeat presses:
                // adaptive Alt+Arrow consumes while a neighbour exists, then
                // reaches the PTY after focus arrives at the edge. Clear the
                // earlier UI press here so that terminal-owned repeat receives
                // its matching release. If this press is consumed later in the
                // dispatch, the second call below re-inserts it.
                suppressed.remove(&physical_key);
            }
            false
        }
        ElementState::Released => suppressed.remove(&physical_key),
    }
}

const FRAME_RETRY_BASE: std::time::Duration = std::time::Duration::from_millis(16);
const FRAME_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(5);
const OCCLUDED_RETRY_MIN: std::time::Duration = std::time::Duration::from_millis(250);
const RENDERER_REBUILD_BASE: std::time::Duration = std::time::Duration::from_millis(100);
const RENDERER_REBUILD_MAX: std::time::Duration = std::time::Duration::from_secs(5);
const PTY_RESIZE_RETRY_BASE: std::time::Duration = std::time::Duration::from_millis(16);
const PTY_RESIZE_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(1);
const PTY_RESIZE_RETRY_LIMIT: u32 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FrameRecoveryAction {
    Redraw,
    RebuildRenderer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FrameRecoveryPoll {
    Idle,
    /// A repair remains armed, but an invisible surface must not acquire or
    /// create GPU resources. A visibility/window-state event will poll again.
    Quiescent,
    Wait(std::time::Duration),
    Ready(FrameRecoveryAction),
}

#[derive(Clone, Copy, Debug)]
struct PendingFrameRecovery {
    action: FrameRecoveryAction,
    deadline: std::time::Instant,
}

/// Per-window frame liveness state.
///
/// Surface timeouts can themselves block inside a backend for close to a
/// second. Keeping retries as one-shot deadlines prevents PTY wakeups, cursor
/// animation, or already-queued redraw events from turning a persistent
/// timeout into an event-loop-blocking acquire loop. Renderer rebuilds use a
/// separate, longer backoff because allocating all retained GPU resources is
/// substantially more expensive than acquiring a frame.
#[derive(Debug, Default)]
pub(crate) struct FrameRecoveryState {
    pending: Option<PendingFrameRecovery>,
    transient_attempts: u32,
    renderer_attempts: u32,
}

impl FrameRecoveryState {
    pub(crate) fn schedule_transient_retry(&mut self, now: std::time::Instant) {
        self.transient_attempts = self.transient_attempts.saturating_add(1);
        let delay =
            capped_exponential_delay(FRAME_RETRY_BASE, FRAME_RETRY_MAX, self.transient_attempts);
        self.arm(FrameRecoveryAction::Redraw, now + delay);
    }

    pub(crate) fn schedule_occluded_retry(&mut self, now: std::time::Instant) {
        self.transient_attempts = self.transient_attempts.saturating_add(1);
        let delay =
            capped_exponential_delay(FRAME_RETRY_BASE, FRAME_RETRY_MAX, self.transient_attempts)
                .max(OCCLUDED_RETRY_MIN);
        self.arm(FrameRecoveryAction::Redraw, now + delay);
    }

    pub(crate) fn schedule_renderer_rebuild(&mut self, now: std::time::Instant) {
        self.renderer_attempts = self.renderer_attempts.saturating_add(1);
        // Recover the first isolated surface/resource failure on this event
        // turn. If rebuilding or the next frame fails again, progressively
        // back off until a frame actually presents.
        let delay = if self.renderer_attempts == 1 {
            std::time::Duration::ZERO
        } else {
            capped_exponential_delay(
                RENDERER_REBUILD_BASE,
                RENDERER_REBUILD_MAX,
                self.renderer_attempts - 1,
            )
        };
        self.arm(FrameRecoveryAction::RebuildRenderer, now + delay);
    }

    pub(crate) fn poll(
        &mut self,
        now: std::time::Instant,
        render_hidden: bool,
    ) -> FrameRecoveryPoll {
        let Some(pending) = self.pending else {
            return FrameRecoveryPoll::Idle;
        };
        if render_hidden {
            return FrameRecoveryPoll::Quiescent;
        }
        if now < pending.deadline {
            return FrameRecoveryPoll::Wait(pending.deadline.saturating_duration_since(now));
        }
        self.pending = None;
        FrameRecoveryPoll::Ready(pending.action)
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn renderer_rebuild_pending(&self) -> bool {
        self.pending
            .is_some_and(|pending| pending.action == FrameRecoveryAction::RebuildRenderer)
    }

    /// A resize, restore, or compositor un-occlusion is new evidence that an
    /// armed repair can succeed. Keep it state-machine-driven, but make its
    /// deadline the current event turn instead of waiting out stale backoff.
    pub(crate) fn expedite(&mut self, now: std::time::Instant) {
        if let Some(pending) = self.pending.as_mut() {
            pending.deadline = now;
        }
    }

    /// Presentation is the only normal success signal strong enough to reset
    /// retry history. A successful renderer construction deliberately does not
    /// reset it: if the next frame fails identically, rebuilding must back off.
    pub(crate) fn presented(&mut self) {
        self.clear();
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    fn arm(&mut self, action: FrameRecoveryAction, deadline: std::time::Instant) {
        match self.pending.as_mut() {
            // Rebuilding subsumes a redraw; never let a later transient result
            // downgrade the stronger repair.
            Some(pending)
                if pending.action == FrameRecoveryAction::RebuildRenderer
                    && action == FrameRecoveryAction::Redraw => {}
            Some(pending) if pending.action == action => {
                pending.deadline = pending.deadline.max(deadline);
            }
            _ => {
                self.pending = Some(PendingFrameRecovery { action, deadline });
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum OutputPaintPhase {
    #[default]
    Idle,
    /// Output is dirty but still inside the current frame budget.
    Deferred,
    /// A deferred output redraw has been requested from winit.
    Queued,
    /// A redraw callback is attempting to present the deferred damage.
    Presenting,
}

/// Behavioral output-paint state machine.
///
/// This replaces two independently mutable booleans whose invalid combinations
/// could erase the sustained-flood signal or schedule a 1 ns wake loop while a
/// redraw was already queued.
#[derive(Debug, Default)]
pub(crate) struct OutputPaintPacer {
    phase: OutputPaintPhase,
}

impl OutputPaintPacer {
    pub(crate) fn defer(&mut self) {
        if self.phase == OutputPaintPhase::Idle {
            self.phase = OutputPaintPhase::Deferred;
        }
    }

    /// Mark a redraw request that will flush deferred output. Returns whether
    /// this call transitioned from deferred to newly queued.
    pub(crate) fn queue_deferred_redraw(&mut self) -> bool {
        if self.phase == OutputPaintPhase::Deferred {
            self.phase = OutputPaintPhase::Queued;
            true
        } else {
            false
        }
    }

    /// Consume a redraw callback. The return value is retained until frame
    /// outcome so flood accounting only advances on presentation.
    pub(crate) fn begin_frame(&mut self) -> bool {
        match self.phase {
            OutputPaintPhase::Deferred | OutputPaintPhase::Queued => {
                self.phase = OutputPaintPhase::Presenting;
                true
            }
            OutputPaintPhase::Idle | OutputPaintPhase::Presenting => false,
        }
    }

    pub(crate) fn presented(&mut self) {
        self.phase = OutputPaintPhase::Idle;
    }

    /// A frame that did not present retains output damage for the recovery
    /// deadline, but no longer claims a winit redraw is outstanding.
    pub(crate) fn presentation_failed(&mut self) {
        if self.phase == OutputPaintPhase::Presenting {
            self.phase = OutputPaintPhase::Deferred;
        }
    }

    pub(crate) fn reset(&mut self) {
        self.phase = OutputPaintPhase::Idle;
    }

    pub(crate) fn is_deferred(&self) -> bool {
        matches!(
            self.phase,
            OutputPaintPhase::Deferred | OutputPaintPhase::Queued | OutputPaintPhase::Presenting
        )
    }

    pub(crate) fn redraw_queued(&self) -> bool {
        self.phase == OutputPaintPhase::Queued
    }
}

/// Pure per-window state machine for coalescing a monitor DPI transition with
/// its accompanying physical-size event.
///
/// On Windows, `WM_DPICHANGED` produces `ScaleFactorChanged` before the
/// `SetWindowPos`-driven `Resized`. Applying the new cell metrics to the old
/// physical size in the first event would transiently reflow every PTY, then
/// immediately reflow it back in the second event. Other winit backends are not
/// required to deliver that follow-up resize, so `AboutToWait` is a bounded
/// fallback once the renderer is usable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DpiResizeEvent {
    ScaleFactorChanged,
    Resized {
        width: u32,
        height: u32,
        renderer_ready: bool,
    },
    AboutToWait {
        width: u32,
        height: u32,
        renderer_ready: bool,
    },
    RendererRebuilt {
        width: u32,
        height: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DpiResizeAction {
    Defer,
    /// Keep the pending scale transition and schedule one more event-loop
    /// turn. This gives a backend's paired `Resized` event a chance to arrive
    /// before the bounded old-size fallback commits.
    DeferAndWake,
    ApplyLayout,
}

#[derive(Debug, Default)]
pub(crate) struct DpiResizeCoalescer {
    pending_scale_change: bool,
    fallback_turn_seen: bool,
}

impl DpiResizeCoalescer {
    pub(crate) fn on_event(&mut self, event: DpiResizeEvent) -> DpiResizeAction {
        match event {
            DpiResizeEvent::ScaleFactorChanged => {
                self.pending_scale_change = true;
                self.fallback_turn_seen = false;
                DpiResizeAction::Defer
            }
            DpiResizeEvent::Resized {
                width,
                height,
                renderer_ready,
            } => {
                if width == 0 || height == 0 || !renderer_ready {
                    return DpiResizeAction::Defer;
                }
                self.pending_scale_change = false;
                self.fallback_turn_seen = false;
                DpiResizeAction::ApplyLayout
            }
            DpiResizeEvent::AboutToWait {
                width,
                height,
                renderer_ready,
            } => {
                if !self.pending_scale_change || width == 0 || height == 0 || !renderer_ready {
                    return DpiResizeAction::Defer;
                }
                if !self.fallback_turn_seen {
                    self.fallback_turn_seen = true;
                    return DpiResizeAction::DeferAndWake;
                }
                self.pending_scale_change = false;
                self.fallback_turn_seen = false;
                DpiResizeAction::ApplyLayout
            }
            DpiResizeEvent::RendererRebuilt { width, height } => {
                if width == 0 || height == 0 {
                    return DpiResizeAction::Defer;
                }
                self.pending_scale_change = false;
                self.fallback_turn_seen = false;
                DpiResizeAction::ApplyLayout
            }
        }
    }

    pub(crate) fn render_allowed(&self) -> bool {
        !self.pending_scale_change
    }

    #[cfg(test)]
    fn is_pending(&self) -> bool {
        self.pending_scale_change
    }
}

/// Bounded retry state for a native PTY resize that failed after the local grid
/// accepted the desired geometry. Retries are deadline-driven so output/redraw
/// traffic cannot create a tight syscall loop, and stop after a finite burst;
/// any later real layout pass remains eligible to try again.
#[derive(Debug, Default)]
pub(crate) struct PtyResizeRetryState {
    deadline: Option<std::time::Instant>,
    attempts: u32,
}

impl PtyResizeRetryState {
    pub(crate) fn record_result(&mut self, now: std::time::Instant, failed: bool) {
        if !failed {
            *self = Self::default();
            return;
        }
        if self.deadline.is_some() || self.attempts >= PTY_RESIZE_RETRY_LIMIT {
            return;
        }
        self.attempts = self.attempts.saturating_add(1);
        let delay =
            capped_exponential_delay(PTY_RESIZE_RETRY_BASE, PTY_RESIZE_RETRY_MAX, self.attempts);
        self.deadline = Some(now + delay);
    }

    pub(crate) fn take_due(&mut self, now: std::time::Instant) -> bool {
        if self.deadline.is_none_or(|deadline| now < deadline) {
            return false;
        }
        self.deadline = None;
        true
    }

    pub(crate) fn remaining(&self, now: std::time::Instant) -> Option<std::time::Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }
}

fn capped_exponential_delay(
    base: std::time::Duration,
    cap: std::time::Duration,
    attempt: u32,
) -> std::time::Duration {
    let shift = attempt.saturating_sub(1).min(16);
    base.saturating_mul(1_u32 << shift).min(cap)
}

/// Multi-window effort (Peacock): the accent this window resolved + claimed.
pub(crate) struct WindowAccent {
    /// The live color (recomputed from `slot` when the theme changes).
    pub(crate) color: kettle_config::Rgb,
    /// Index into `kettle_config::peacock_pool(theme)` — kept across theme
    /// switches so a window holds its position in the new theme's pool.
    pub(crate) slot: usize,
    /// Theme name the color was resolved against (cheap change detector for
    /// the per-frame sync).
    pub(crate) theme_name: String,
    /// Cross-process presence claim; released when the window drops.
    pub(crate) presence: Option<kettle_ctl::presence::PresenceGuard>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImePreeditOwner {
    Search,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ImePreeditSession {
    pub(crate) owner: ImePreeditOwner,
    pub(crate) generation: u64,
}

pub(crate) struct WindowState {
    /// Stable per-window sequence number (1-based, process-lifetime unique).
    /// Exposed to agents via the ctl API (C8) and used as the map key — never
    /// reused, so an agent holding a seq can't be aliased onto a new window.
    pub(crate) seq: u64,
    /// Declared BEFORE `window`: the renderer owns the wgpu `Surface` created
    /// from this window, and struct fields drop in declaration order — the
    /// surface must go before the `Arc<Window>` it borrows from.
    pub(crate) renderer: Option<Renderer>,
    pub(crate) window: Option<Arc<Window>>,
    /// Native accessibility bridge. Constructed while the window is still
    /// hidden, before its first `set_visible(true)`.
    pub(crate) accessibility: Option<accesskit_winit::Adapter>,
    /// Accessibility updates are content-keyed and rate-limited so an active
    /// AT-SPI/UIA client cannot turn terminal repaint throughput into unbounded
    /// native accessibility traffic on the UI thread.
    pub(crate) accessibility_key: Option<u64>,
    pub(crate) accessibility_updated_at: Option<std::time::Instant>,
    pub(crate) accessibility_pending: bool,
    /// OS taskbar progress, driven by the focused pane's OSC 9;4
    /// state each frame (pwsh 7 / Windows Terminal parity). No-op off Windows.
    pub(crate) taskbar: crate::taskbar::Taskbar,
    /// True while an OS attention request (taskbar flash / dock
    /// bounce) is outstanding. winit's `request_user_attention(None)` doesn't
    /// reliably stop the Win11 taskbar flash, so we track outstanding requests
    /// and clear them directly via `Taskbar::clear_attention` on focus-gain.
    pub(crate) attention_active: bool,
    /// This window's tab/pane multiplexer. Panes are self-contained (PTY +
    /// reader thread + channels), so a tab can move between windows (C5) by
    /// moving its panes between Muxes — the PTYs never notice.
    pub(crate) mux: Mux,
    /// Scrollback search is OS-window chrome, not terminal/mux state. Keeping
    /// it here prevents one window's modal and editor focus from leaking into
    /// another and lets a tab move without carrying transient UI state.
    pub(crate) search: SearchState,
    /// Last query per pane, memory-only. Reopening search restores the pane's
    /// local query without persisting terminal contents to disk.
    pub(crate) search_queries: std::collections::HashMap<u64, String>,
    pub(crate) mods: ModifiersState,
    #[cfg(target_os = "macos")]
    pub(crate) macos_raw_mods: ModifiersState,
    #[cfg(target_os = "macos")]
    pub(crate) macos_left_option_pressed: bool,
    #[cfg(target_os = "macos")]
    pub(crate) macos_right_option_pressed: bool,
    /// Physical keys whose press was consumed by Kettle UI/keybindings. Their
    /// matching release must not leak to a Kitty-protocol client after the UI
    /// state that consumed the press has already closed.
    pub(crate) suppressed_key_releases: std::collections::HashSet<winit::keyboard::PhysicalKey>,
    /// Active input-method composition and its byte-indexed selection range.
    /// Committed text is written to the PTY and this preedit is cleared.
    pub(crate) ime_preedit: Option<(String, Option<(usize, usize)>)>,
    /// Surface that owned the active composition. If ownership changes before
    /// Commit, the old composition is discarded instead of escaping across a
    /// modal boundary into Search or the terminal PTY.
    pub(crate) ime_preedit_owner: Option<ImePreeditSession>,
    /// Incremented whenever keyboard focus moves between Kettle-owned input surfaces. A delayed
    /// IME Commit is accepted only by the exact generation that received its Preedit.
    pub(crate) ime_focus_generation: u64,
    pub(crate) fullscreen: bool,
    pub(crate) cursor: PhysicalPosition<f64>,
    /// Sub-detent wheel residue for this window. Precision touchpads and
    /// high-resolution wheels report a fraction of a detent per event; without
    /// carrying the remainder across events every one of them quantizes to zero
    /// and the wheel does nothing at all. Per-window because each OS window has
    /// its own independent pointer-event stream.
    pub(crate) wheel: crate::input::WheelAccum,
    pub(crate) selecting: bool,
    /// Pane that owns the active primary-button selection gesture. Focus can
    /// move through ctl/Lua while a drag is live; pinning the id prevents the
    /// next frame from extending or scrolling an unrelated split.
    pub(crate) selecting_pane: Option<u64>,
    /// Vertical outer-client edge crossed by an active selection drag:
    /// `1` = top/history, `-1` = bottom/present, `0` = pointer inside or a
    /// horizontal exit. `CursorLeft` has no coordinate, so this latches the
    /// direction inferred from the last in-client position.
    pub(crate) selection_autoscroll_edge: i8,
    /// Distance from the pointer to the dragged thumb's top edge. Keeping the
    /// grab offset prevents the thumb from jumping when a drag starts.
    pub(crate) scrollbar_drag_offset: Option<f32>,
    /// v2.26.0: the pointer is hovering the focused pane's scrollbar gutter.
    /// Drives the overlay scrollbar's bright (vs dim-at-rest) opacity with no
    /// fade timer, so it costs zero idle wakeups — just a single repaint on the
    /// hover-enter / hover-leave transition.
    pub(crate) scrollbar_hover: bool,
    /// In-progress mouse drag of a split divider. `Some`
    /// while the left button is held after a press landed on a divider seam;
    /// each CursorMoved recomputes the addressed split's ratio from the cursor.
    pub(crate) dragging_split: Option<SplitDrag>,
    /// Cache key for the last viewport link-autodetect scan — see
    /// app.rs (`update_links` re-scans only on output, scroll, or focus).
    pub(crate) links_scan_key: Option<LinksScanKey>,
    pub(crate) mouse_btn: Option<u8>,
    /// Last `(pane, row, col)` reported to a mouse-tracking app, so
    /// cell-motion reports (1002/1003) fire only on a cell crossing. Pane
    /// identity is part of the key because wheel input can target a hovered
    /// split without moving keyboard focus; a report in one pane must never
    /// suppress the first same-coordinate report in another.
    pub(crate) last_mouse_cell: Option<(u64, usize, usize)>,
    pub(crate) links: Vec<kettle_core::Link>,
    pub(crate) ssh_input: Option<String>,
    /// `Some((query, selected))` while the command palette is open.
    pub(crate) palette_input: Option<(String, usize)>,
    /// `Action::OpenSettings` overlay navigation. `Some` while the
    /// in-app settings panel is open.
    pub(crate) settings_nav: Option<crate::settings::SettingsNav>,
    /// v2.24.0: when `Some`, an inline text prompt (the image-path entry) is open
    /// over the settings panel; keystrokes route to it until Enter (persist) or
    /// Esc (cancel). Only meaningful while `settings_nav` is also `Some`.
    pub(crate) settings_text_edit: Option<crate::settings::SettingsTextEdit>,
    /// v2.23.0: set when a Settings change (a GPU pin / power-preference /
    /// backend / force-software) was persisted but can only take effect on the
    /// next launch. The settings overlay shows a "⚠ restart to apply" footer
    /// while this is true; cleared when the overlay closes.
    pub(crate) settings_restart_pending: bool,
    /// v2.23.1: the animated-background frame index last requested for paint.
    /// The event loop requests a bg redraw only when the live frame index
    /// differs from this (an edge-trigger), so an animated wallpaper repaints at
    /// the GIF's fps instead of continuously (the animated-idle-CPU fix).
    pub(crate) last_bg_frame: Option<usize>,
    /// Terminator parity: `Action::OpenLayoutPicker` modal state —
    /// (typed query, selected index) against `Session::list_layouts`.
    pub(crate) layout_picker_input: Option<(String, usize)>,
    /// Active quick-select hint mode: detected targets + typed prefix.
    pub(crate) hint_state: Option<(Vec<HintTarget>, String)>,
    /// Right-click context menu state (`Some` while open). Lives next
    /// to the other modal overlays — same close-all-modals discipline,
    /// same Esc-to-dismiss key route.
    pub(crate) context_menu: Option<ContextMenuState>,
    /// v2.24.0 live theme preview: while the cursor (or keyboard) is on a
    /// `ThemeChoice` row in the right-click → Theme submenu, the theme is applied
    /// ephemerally to `cfg`; this holds the `(theme_name, theme)` to restore on
    /// dismiss-without-select. Cleared (kept) on commit (`SetTheme`). Reverted by
    /// the single post-event chokepoint in `window_event` when the highlight
    /// leaves a theme row or the menu closes.
    pub(crate) theme_preview: Option<(String, kettle_config::Theme)>,
    /// When `Some`, the user is editing a window/tab/pane title via
    /// an inline overlay.
    pub(crate) editing_title: Option<TitleEditState>,
    /// Set when something changed the layout the PTYs are sized against, so
    /// they need re-sizing before the next frame.
    ///
    /// Deliberately NOT named for chrome: `handle_action`'s tail marks it after
    /// every action, not only after a chrome-strip transition. It began life as
    /// `chrome_geometry_dirty` and was renamed once that stopped being true —
    /// a flag whose name claims narrower semantics than its use invites exactly
    /// the wrong assumption at the next call site.
    ///
    /// Deferred rather than resized on the spot because `close_all_modals`
    /// runs immediately BEFORE most modal openers. Resizing there and again in
    /// the opener sent the child two `SIGWINCH`s — grow then shrink — for a net
    /// change of zero on a title-edit -> title-edit replacement. Kettle paints
    /// no intermediate frame, but vim, tmux and htop observe both and redraw.
    /// Coalescing to one flush per frame makes the intermediate state
    /// unobservable no matter what a caller does next, without threading a
    /// "will install another modal" flag through seventeen call sites.
    pub(crate) pending_resize: bool,
    /// When `Some`, a confirm modal is open. Keyboard input routes
    /// to modal dispatch and the renderer paints the centered modal panel.
    pub(crate) confirm_dialog: Option<ConfirmDialogState>,
    pub(crate) window_focused: bool,
    /// v2.24.0: `true` while the window is fully hidden behind other windows
    /// (winit `WindowEvent::Occluded(true)`). Gates the animated-background
    /// wake so a covered window costs zero idle (alongside an `is_minimized`
    /// probe). Set back to `false` on un-occlude, which also forces a repaint.
    pub(crate) window_occluded: bool,
    /// True while the OS mouse cursor is hidden because the user is typing
    /// (`mouse-hide-while-typing`). Re-shown on the next mouse movement.
    pub(crate) mouse_hidden: bool,
    /// Last `CursorIcon` we pushed to the window — used to dedupe so we
    /// don't issue a `set_cursor` syscall on every CursorMoved event.
    pub(crate) last_cursor_icon: Option<CursorIcon>,
    /// Drag-to-reorder tab state. `Some(_)` while a left-
    /// mouse-button press in the tab bar is being held; cleared on release.
    pub(crate) tab_drag_active: bool,
    /// Surface position where the in-window tab reorder gesture was armed.
    /// A click stays visually a click until movement crosses the drag-distance
    /// threshold; this prevents the drag ghost from flashing under a normal
    /// tab switch.
    pub(crate) tab_drag_press: Option<(f32, f32)>,
    /// Tab index currently held by a left-button click before release. This is
    /// a visual press state only; hit-test and drag behavior use the segment
    /// rects and `tab_drag_*` fields.
    pub(crate) tab_pressed_idx: Option<usize>,
    /// Tab tear-off drag FSM state. Distinct from the in-window
    /// `tab_drag_active` reorder; both fire from the same
    /// mouse-down on the tab bar. Wired live in C6 of the multi-window
    /// effort: a release while DraggingOutside tears the tab off into a new
    /// in-process window at the drop point.
    pub(crate) detach_drag: crate::detach::DragState,
    /// C6: surface position of the tab-bar mouse-down that armed
    /// `detach_drag` — the origin the FSM's click-vs-drag distance is
    /// measured from. `None` while no tear-off gesture is armed.
    pub(crate) drag_press: Option<(f32, f32)>,
    /// Terminator parity: drag a terminal to another position within its tab.
    /// `Some(_)` from a left-press on a per-pane titlebar until the matching
    /// release. Armed, not live: the press only becomes a move once the pointer
    /// clears the slop radius, so a plain click on the titlebar still means
    /// "focus, then edit the title".
    pub(crate) pane_drag: Option<PaneDrag>,
    /// v2.19.0 (tear-off UX, re-dock): `Some(insertion index)` while a
    /// torn-off window is hovering this window's tab band. Draws the
    /// accent insertion marker, and — key affordance — MATERIALIZES the
    /// tab bar on a single-tab `tab-bar = auto` window (`tab_bar_h`
    /// treats a live dock preview as "show the bar") so the drop target
    /// is visible before the drop. Cleared when the hover leaves or the
    /// drag ends.
    pub(crate) dock_preview: Option<usize>,
    /// Index of the tab whose close-button (`✕`) zone the mouse cursor
    /// is currently over (pointer-cursor swap + hover-background quad).
    pub(crate) hovered_close_idx: Option<usize>,
    /// Hovered half of the trailing horizontal new-tab control. Stored beside
    /// close hover so pointer motion repaints only when the visual state
    /// actually changes; `tab_bar()` forwards it to the renderer.
    pub(crate) hovered_new_tab: bool,
    pub(crate) hovered_new_tab_menu: bool,
    /// Vi-mode (Alacritty parity): `Some(ViState)` while kettle is
    /// intercepting keys for vi-style navigation.
    pub(crate) vi_mode: Option<ViState>,
    /// Terminator parity (`key_scaled_zoom`): font size saved on
    /// entering scaled zoom so leave-zoom restores it exactly.
    pub(crate) scaled_zoom_prev_font_size: Option<f32>,
    /// The pane id we last fired the Lua focus-change event for.
    pub(crate) last_emitted_focus: Option<u64>,
    /// Last title we emitted a `title_changed` Lua event for,
    /// keyed by pane id. Self-prunes (only live panes are iterated).
    pub(crate) last_emitted_titles: std::collections::HashMap<u64, String>,
    pub(crate) blink_on: bool,
    pub(crate) last_blink: std::time::Instant,
    pub(crate) last_bell: Option<std::time::Instant>,
    /// Coalesce output-driven repaints (R2). `last_paint` is when the last
    /// frame painted; `output_pacer` owns the deferred -> queued -> presenting
    /// transaction. Input/cursor paints bypass this state machine.
    pub(crate) last_paint: Option<std::time::Instant>,
    pub(crate) output_pacer: OutputPaintPacer,
    /// v2.21.1 (throughput): consecutive output-coalesced frames — i.e. how
    /// sustained the current PTY-output flood is. `effective_output_budget`
    /// stretches the monitor-derived paint budget as this climbs, so fewer
    /// per-frame snapshots are taken under the `Term` lock the PTY reader
    /// needs; reset to 0 by any non-coalesced paint (`redraw`).
    pub(crate) flood_paints: u32,
    /// Output-coalescing base period for this window's current monitor.
    /// Derived from winit's millihertz refresh value and refreshed on monitor,
    /// DPI, and size transitions; windows on different displays pace
    /// independently.
    pub(crate) output_frame_budget: std::time::Duration,
    /// Last synchronous current-monitor refresh probe. Windows implements this
    /// through MonitorFromWindow/GetMonitorInfo/EnumDisplaySettings, so move
    /// storms rate-limit it and retain a trailing probe deadline.
    pub(crate) output_monitor: Option<winit::monitor::MonitorHandle>,
    pub(crate) output_refresh_probe_at: Option<std::time::Instant>,
    pub(crate) output_refresh_probe_pending: bool,
    pub(crate) last_click: Option<(std::time::Instant, usize, usize, u8)>,
    /// Last OS window title set (dedupe `set_title` syscalls).
    pub(crate) last_title: String,
    /// A user-set window title that must survive redraws.
    ///
    /// `apply_title_edit` used to call `set_title` and stop there, but
    /// `sync_window_title` recomputes the title from `window-title-format` on
    /// every redraw and overwrites it — so "Edit window title" appeared to
    /// work and reverted within one frame. Terminator keeps an equivalent
    /// `forced` flag on its window so later `set_title` calls are ignored
    /// (window.py:1162-1198). `None` means "follow the format", which is how
    /// clearing the field restores automatic titles.
    pub(crate) window_title_override: Option<String>,
    /// Pane ids whose shell exited + cfg.exit_action requested
    /// restart. Drained AFTER drain_events; dedup'd on push.
    pub(crate) pending_pane_restarts: Vec<u64>,
    /// Whether a visible-state window has already been shown. Startup creates
    /// the OS window hidden during renderer init, then reveals visible states
    /// once the surface is configured; `window_state = hidden` keeps this true
    /// so fallback reveal paths do not show it.
    pub(crate) window_shown: bool,
    /// C4: per-pane `Terminal::output_generation` values consumed by this
    /// window's last successfully presented frame. During genuine device loss,
    /// the redraw guard intentionally snapshots these without presentation so
    /// streaming output quiesces until process-wide recovery forces a redraw.
    /// The fan-out `UserEvent::Wakeup` compares against the live counters to
    /// decide whether THIS window has anything new to paint.
    pub(crate) seen_output_gen: std::collections::HashMap<u64, u64>,
    /// Recycled candidate map for the frame currently being built. It swaps
    /// with `seen_output_gen` only after presentation, so an acquire timeout or
    /// lost/occluded surface cannot silently consume terminal damage.
    pub(crate) pending_output_gen: std::collections::HashMap<u64, u64>,
    /// Deadline-driven retry and per-renderer repair state. Kept per window so
    /// one lost swapchain cannot falsely escalate or stall the healthy
    /// renderers sharing the same GPU device.
    pub(crate) frame_recovery: FrameRecoveryState,
    /// Deadline-driven retries for native PTY resize failures. Local terminal
    /// geometry may already reflect the desired layout, while CSI 14t continues
    /// to expose the last geometry actually accepted by the PTY.
    pub(crate) pty_resize_retry: PtyResizeRetryState,
    /// Live CPU-owned renderer state retained while a process-wide GPU-device
    /// recovery rebuilds every surface. This outlives failed adapter attempts,
    /// so runtime font zoom, cell scaling, per-window accent, and a queued
    /// screenshot completion cannot be reset or lost between retries.
    pub(crate) renderer_recovery: Option<kettle_render::RendererRecoveryState>,
    /// Coalesces a monitor scale transition with its corresponding physical
    /// resize so PTYs observe one final grid, never an intermediate reflow.
    pub(crate) dpi_resize: DpiResizeCoalescer,
    /// Set by `Resized(0, 0)` while minimized. Some Windows APIs continue to
    /// expose a nonzero restore rect through `inner_size`; DPI fallback must
    /// not treat that stale rect as a usable surface.
    pub(crate) dpi_resize_surface_suspended: bool,
    /// Multi-window effort (Peacock): this window's resolved accent claim.
    /// `None` while unresolved (first frame) or when the user opted out
    /// (`accent-color = theme`/`off`/`none` or a pinned hex). Kept in sync
    /// each frame by `App::sync_window_accent`.
    pub(crate) accent: Option<WindowAccent>,
    /// PERF (key-repeat stutter fix): when the user last typed bytes into a
    /// PTY in this window. Output arriving within `TYPING_ECHO_WINDOW` of a
    /// keystroke paints IMMEDIATELY (request_redraw is vsync-coalesced, so
    /// this can't outpace the display) instead of through the
    /// output coalescer (`output_pacer`) — whose WaitUntil deadline has ~16ms timer
    /// granularity on Windows, which made held-key echo visibly stutter
    /// while Terminator (steady GTK frame clock) stayed smooth.
    pub(crate) last_typed: Option<std::time::Instant>,
    /// v2.20.0 (Ghostty `resize-overlay` parity): `Some((cols, rows, armed_at))`
    /// while the transient size chip is visible; expires
    /// `RESIZE_OVERLAY_DURATION` after the last resize event.
    pub(crate) resize_overlay: Option<(u16, u16, std::time::Instant)>,
    /// v2.20.0: the first `Resized` after window creation is the initial
    /// placement, not a user resize — `resize-overlay = after-first`
    /// (the default) skips it.
    pub(crate) seen_first_resize: bool,
    /// v2.20.0 (review fix): when this WindowState was created. Session
    /// restore / `window-state = maximised` / tear-off creation deliver a
    /// short STORM of placement `Resized` events, not just one —
    /// `after-first` also swallows everything in the first moments after
    /// birth so a restored window doesn't flash a spurious size chip.
    pub(crate) spawned_at: std::time::Instant,
    /// v2.20.0 P2 (perf): pooled per-pane render snapshots. `redraw` captures
    /// each visible pane's viewport into these UNDER the Term lock (a
    /// µs-scale flat copy) and drops the guard before the GPU frame, so the
    /// PTY reader threads no longer stall behind shaping / surface-acquire /
    /// present. High-water pooled: each snapshot's `cells` Vec keeps its
    /// capacity across frames; truncated to the visible pane count.
    pub(crate) pane_snapshots: Vec<kettle_render::PaneSnapshot>,
    /// Pane id, output generation, and grid dimensions represented by each
    /// pooled snapshot. This makes the context-menu-only redraw fast path fail
    /// closed when output, layout, or font metrics changed.
    pub(crate) pane_snapshot_keys: Vec<(u64, u64, usize, usize)>,
    /// One-shot hint set only by context-menu visual state changes. The next
    /// redraw may reuse pane snapshots after validating `pane_snapshot_keys`;
    /// every subsequent window/user event clears the hint first.
    pub(crate) reuse_pane_snapshots_once: bool,
}

impl WindowState {
    /// A fresh window's state, before its OS window / renderer exist
    /// (`resumed` / `open_window` fill those in). The `Mux` is built by the
    /// caller because its construction flags (`lua_output_subscribed`,
    /// `record_lossless`) are process-global decisions made in `run_with`.
    pub(crate) fn new(seq: u64, fullscreen: bool, mux: Mux) -> Self {
        Self {
            seq,
            renderer: None,
            window: None,
            accessibility: None,
            accessibility_key: None,
            accessibility_updated_at: None,
            accessibility_pending: false,
            taskbar: crate::taskbar::Taskbar::new(),
            attention_active: false,
            mux,
            search: SearchState::default(),
            search_queries: std::collections::HashMap::new(),
            mods: ModifiersState::empty(),
            #[cfg(target_os = "macos")]
            macos_raw_mods: ModifiersState::empty(),
            #[cfg(target_os = "macos")]
            macos_left_option_pressed: false,
            #[cfg(target_os = "macos")]
            macos_right_option_pressed: false,
            suppressed_key_releases: std::collections::HashSet::new(),
            ime_preedit: None,
            ime_preedit_owner: None,
            ime_focus_generation: 0,
            fullscreen,
            cursor: PhysicalPosition::new(0.0, 0.0),
            wheel: crate::input::WheelAccum::default(),
            selecting: false,
            selecting_pane: None,
            selection_autoscroll_edge: 0,
            scrollbar_drag_offset: None,
            scrollbar_hover: false,
            dragging_split: None,
            links_scan_key: None,
            mouse_btn: None,
            last_mouse_cell: None,
            links: Vec::new(),
            ssh_input: None,
            palette_input: None,
            settings_nav: None,
            settings_text_edit: None,
            settings_restart_pending: false,
            last_bg_frame: None,
            layout_picker_input: None,
            hint_state: None,
            context_menu: None,
            theme_preview: None,
            editing_title: None,
            pending_resize: false,
            confirm_dialog: None,
            window_focused: true,
            window_occluded: false,
            mouse_hidden: false,
            last_cursor_icon: None,
            tab_drag_active: false,
            tab_drag_press: None,
            tab_pressed_idx: None,
            detach_drag: crate::detach::DragState::default(),
            drag_press: None,
            pane_drag: None,
            dock_preview: None,
            hovered_close_idx: None,
            hovered_new_tab: false,
            hovered_new_tab_menu: false,
            vi_mode: None,
            scaled_zoom_prev_font_size: None,
            last_emitted_focus: None,
            last_emitted_titles: std::collections::HashMap::new(),
            blink_on: true,
            last_blink: std::time::Instant::now(),
            last_bell: None,
            last_paint: None,
            output_pacer: OutputPaintPacer::default(),
            flood_paints: 0,
            output_frame_budget: std::time::Duration::from_nanos(16_666_667),
            output_monitor: None,
            output_refresh_probe_at: None,
            output_refresh_probe_pending: false,
            last_click: None,
            last_title: String::new(),
            window_title_override: None,
            pending_pane_restarts: Vec::new(),
            window_shown: false,
            seen_output_gen: std::collections::HashMap::new(),
            pending_output_gen: std::collections::HashMap::new(),
            frame_recovery: FrameRecoveryState::default(),
            pty_resize_retry: PtyResizeRetryState::default(),
            renderer_recovery: None,
            dpi_resize: DpiResizeCoalescer::default(),
            dpi_resize_surface_suspended: false,
            accent: None,
            last_typed: None,
            resize_overlay: None,
            seen_first_resize: false,
            spawned_at: std::time::Instant::now(),
            pane_snapshots: Vec::new(),
            pane_snapshot_keys: Vec::new(),
            reuse_pane_snapshots_once: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{KeyCode, PhysicalKey};

    #[test]
    fn output_wake_stays_latched_through_defer_queue_and_one_restore_frame() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_for_gate = wakes.clone();
        let gate = kettle_core::event::OutputWakeGate::new(Arc::new(move || {
            wakes_for_gate.fetch_add(1, Ordering::SeqCst);
        }));
        let mut pacer = OutputPaintPacer::default();

        for _ in 0..10_000 {
            gate.request();
        }
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        pacer.defer();
        assert!(pacer.queue_deferred_redraw());
        assert!(!pacer.queue_deferred_redraw());
        for _ in 0..10_000 {
            gate.request();
        }
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "the pane latch must remain closed for the whole deferred frame budget"
        );

        // App::redraw acknowledges before it snapshots generations. Output
        // racing after that point owns a fresh wake and cannot be lost.
        gate.acknowledge();
        assert!(pacer.begin_frame());
        assert!(gate.request());
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        pacer.presented();

        // Hiding converts that racing pending wake to one dirty bit. Any
        // amount of hidden output stays silent, and restore publishes one
        // wake whose deferred frame can only be queued once.
        gate.set_enabled(false);
        pacer.reset();
        for _ in 0..10_000 {
            gate.request();
        }
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        gate.set_enabled(true);
        assert_eq!(wakes.load(Ordering::SeqCst), 3);
        pacer.defer();
        assert!(pacer.queue_deferred_redraw());
        assert!(!pacer.queue_deferred_redraw());
        gate.acknowledge();
        assert!(pacer.begin_frame());
        pacer.presented();
        assert!(!pacer.is_deferred());
    }

    #[test]
    fn frame_timeout_retries_are_one_shot_deadlines_with_a_cap() {
        let mut recovery = FrameRecoveryState::default();
        let mut now = std::time::Instant::now();
        let mut last_delay = std::time::Duration::ZERO;

        for _ in 0..12 {
            recovery.schedule_transient_retry(now);
            let FrameRecoveryPoll::Wait(delay) = recovery.poll(now, false) else {
                panic!("a timeout must not request an immediate redraw");
            };
            assert!(delay >= last_delay);
            assert!(delay <= FRAME_RETRY_MAX);
            assert_eq!(
                recovery.poll(now + delay, false),
                FrameRecoveryPoll::Ready(FrameRecoveryAction::Redraw)
            );
            assert_eq!(recovery.poll(now + delay, false), FrameRecoveryPoll::Idle);
            last_delay = delay;
            now += delay;
        }

        assert_eq!(last_delay, FRAME_RETRY_MAX);
    }

    #[test]
    fn frame_retry_stays_armed_but_quiescent_while_hidden_minimized_or_occluded() {
        let mut recovery = FrameRecoveryState::default();
        let now = std::time::Instant::now();
        recovery.schedule_transient_retry(now);
        let deadline = now + FRAME_RETRY_BASE;

        assert_eq!(recovery.poll(deadline, true), FrameRecoveryPoll::Quiescent);
        assert!(recovery.has_pending());
        assert_eq!(
            recovery.poll(deadline, false),
            FrameRecoveryPoll::Ready(FrameRecoveryAction::Redraw)
        );

        let mut occluded = FrameRecoveryState::default();
        occluded.schedule_occluded_retry(now);
        assert_eq!(
            occluded.poll(now, false),
            FrameRecoveryPoll::Wait(OCCLUDED_RETRY_MIN)
        );
        assert_eq!(
            occluded.poll(now + OCCLUDED_RETRY_MIN, true),
            FrameRecoveryPoll::Quiescent
        );
    }

    #[test]
    fn renderer_rebuilds_back_off_until_a_frame_presents() {
        let mut recovery = FrameRecoveryState::default();
        let now = std::time::Instant::now();

        recovery.schedule_renderer_rebuild(now);
        assert_eq!(
            recovery.poll(now, false),
            FrameRecoveryPoll::Ready(FrameRecoveryAction::RebuildRenderer)
        );

        // Constructing a replacement is not proof that it can present. A
        // repeated surface/render failure therefore receives the next delay.
        recovery.schedule_renderer_rebuild(now);
        assert_eq!(
            recovery.poll(now, false),
            FrameRecoveryPoll::Wait(RENDERER_REBUILD_BASE)
        );

        recovery.expedite(now);
        assert_eq!(
            recovery.poll(now, false),
            FrameRecoveryPoll::Ready(FrameRecoveryAction::RebuildRenderer)
        );
        recovery.presented();
        recovery.schedule_renderer_rebuild(now);
        assert_eq!(
            recovery.poll(now, false),
            FrameRecoveryPoll::Ready(FrameRecoveryAction::RebuildRenderer)
        );
    }

    #[test]
    fn renderer_rebuild_supersedes_a_pending_surface_retry() {
        let mut recovery = FrameRecoveryState::default();
        let now = std::time::Instant::now();
        recovery.schedule_transient_retry(now);
        recovery.schedule_renderer_rebuild(now);

        assert_eq!(
            recovery.poll(now, false),
            FrameRecoveryPoll::Ready(FrameRecoveryAction::RebuildRenderer)
        );
    }

    #[test]
    fn dpi_scale_then_resize_commits_exactly_one_layout() {
        let mut coalescer = DpiResizeCoalescer::default();
        let events = [
            DpiResizeEvent::ScaleFactorChanged,
            DpiResizeEvent::Resized {
                width: 3000,
                height: 2000,
                renderer_ready: true,
            },
        ];

        let layout_commits = events
            .into_iter()
            .filter(|event| coalescer.on_event(*event) == DpiResizeAction::ApplyLayout)
            .count();

        assert_eq!(layout_commits, 1);
        assert!(!coalescer.is_pending());
    }

    #[test]
    fn dpi_scale_suppresses_intermediate_redraw_until_resize_commit() {
        let mut coalescer = DpiResizeCoalescer::default();
        assert!(coalescer.render_allowed());
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::ScaleFactorChanged),
            DpiResizeAction::Defer
        );
        assert!(
            !coalescer.render_allowed(),
            "a queued RedrawRequested must not snapshot or paint new-scale metrics \
             against the old surface/grid"
        );
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::Resized {
                width: 3000,
                height: 2000,
                renderer_ready: true,
            }),
            DpiResizeAction::ApplyLayout
        );
        assert!(coalescer.render_allowed());
    }

    #[test]
    fn pty_resize_failures_retry_on_bounded_exponential_deadlines() {
        let mut retry = PtyResizeRetryState::default();
        let mut now = std::time::Instant::now();
        let mut previous = std::time::Duration::ZERO;

        for _ in 0..PTY_RESIZE_RETRY_LIMIT {
            retry.record_result(now, true);
            let wait = retry.remaining(now).expect("failure arms a retry");
            assert!(wait >= previous);
            assert!(wait <= PTY_RESIZE_RETRY_MAX);
            assert!(!retry.take_due(now));
            now += wait;
            assert!(retry.take_due(now));
            previous = wait;
        }

        retry.record_result(now, true);
        assert!(
            retry.remaining(now).is_none(),
            "a persistent dead PTY must not keep the event loop on a timer forever"
        );

        retry.record_result(now, false);
        retry.record_result(now, true);
        assert_eq!(
            retry.remaining(now),
            Some(PTY_RESIZE_RETRY_BASE),
            "a successful native resize resets retry history"
        );
    }

    #[test]
    fn dpi_resize_stays_pending_while_minimized_or_renderer_unavailable() {
        let mut coalescer = DpiResizeCoalescer::default();
        // A normal resize must not reflow against fallback 800x600/8x16
        // geometry while the renderer is absent during GPU recovery.
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::Resized {
                width: 3000,
                height: 2000,
                renderer_ready: false,
            }),
            DpiResizeAction::Defer
        );
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::ScaleFactorChanged),
            DpiResizeAction::Defer
        );
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::Resized {
                width: 0,
                height: 0,
                renderer_ready: true,
            }),
            DpiResizeAction::Defer
        );
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::Resized {
                width: 3000,
                height: 2000,
                renderer_ready: false,
            }),
            DpiResizeAction::Defer
        );
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::AboutToWait {
                width: 3000,
                height: 2000,
                renderer_ready: false,
            }),
            DpiResizeAction::Defer
        );
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::RendererRebuilt {
                width: 0,
                height: 0,
            }),
            DpiResizeAction::Defer
        );
        assert!(coalescer.is_pending());

        assert_eq!(
            coalescer.on_event(DpiResizeEvent::RendererRebuilt {
                width: 3000,
                height: 2000,
            }),
            DpiResizeAction::ApplyLayout
        );
        assert!(!coalescer.is_pending());
    }

    #[test]
    fn dpi_about_to_wait_is_only_a_pending_scale_fallback() {
        let usable_fallback = DpiResizeEvent::AboutToWait {
            width: 3000,
            height: 2000,
            renderer_ready: true,
        };
        let mut coalescer = DpiResizeCoalescer::default();

        assert_eq!(coalescer.on_event(usable_fallback), DpiResizeAction::Defer);
        coalescer.on_event(DpiResizeEvent::ScaleFactorChanged);
        assert_eq!(
            coalescer.on_event(usable_fallback),
            DpiResizeAction::DeferAndWake
        );
        assert_eq!(
            coalescer.on_event(usable_fallback),
            DpiResizeAction::ApplyLayout
        );
        assert_eq!(coalescer.on_event(usable_fallback), DpiResizeAction::Defer);
    }

    #[test]
    fn dpi_paired_resize_wins_over_one_turn_fallback() {
        let mut coalescer = DpiResizeCoalescer::default();
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::ScaleFactorChanged),
            DpiResizeAction::Defer
        );
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::AboutToWait {
                width: 2400,
                height: 1600,
                renderer_ready: true,
            }),
            DpiResizeAction::DeferAndWake
        );
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::Resized {
                width: 3000,
                height: 2000,
                renderer_ready: true,
            }),
            DpiResizeAction::ApplyLayout
        );
        assert!(!coalescer.is_pending());
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::AboutToWait {
                width: 3000,
                height: 2000,
                renderer_ready: true,
            }),
            DpiResizeAction::Defer,
            "the paired resize must consume the transition exactly once"
        );
    }

    #[test]
    fn dpi_minimized_restore_rect_never_drives_fallback_layout() {
        let mut coalescer = DpiResizeCoalescer::default();
        coalescer.on_event(DpiResizeEvent::ScaleFactorChanged);
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::Resized {
                width: 0,
                height: 0,
                renderer_ready: true,
            }),
            DpiResizeAction::Defer
        );
        // App marks the fallback renderer_ready=false while its last resize
        // event was zero, even if Window::inner_size exposes a restore rect.
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::AboutToWait {
                width: 3000,
                height: 2000,
                renderer_ready: false,
            }),
            DpiResizeAction::Defer
        );
        assert_eq!(
            coalescer.on_event(DpiResizeEvent::Resized {
                width: 3000,
                height: 2000,
                renderer_ready: true,
            }),
            DpiResizeAction::ApplyLayout
        );
    }

    #[test]
    fn consumed_press_swallows_exactly_its_matching_release() {
        let mut suppressed = std::collections::HashSet::new();
        let consumed = PhysicalKey::Code(KeyCode::KeyK);
        let unrelated = PhysicalKey::Code(KeyCode::KeyJ);

        assert!(!track_consumed_key_release(
            &mut suppressed,
            consumed,
            ElementState::Pressed,
            true
        ));
        assert!(suppressed.contains(&consumed));
        assert!(!track_consumed_key_release(
            &mut suppressed,
            unrelated,
            ElementState::Released,
            false
        ));
        assert!(track_consumed_key_release(
            &mut suppressed,
            consumed,
            ElementState::Released,
            false
        ));
        assert!(!suppressed.contains(&consumed));
        assert!(!track_consumed_key_release(
            &mut suppressed,
            consumed,
            ElementState::Released,
            false
        ));
    }

    #[test]
    fn unconsumed_press_does_not_suppress_release() {
        let mut suppressed = std::collections::HashSet::new();
        let key = PhysicalKey::Code(KeyCode::KeyA);
        assert!(!track_consumed_key_release(
            &mut suppressed,
            key,
            ElementState::Pressed,
            false
        ));
        assert!(!track_consumed_key_release(
            &mut suppressed,
            key,
            ElementState::Released,
            false
        ));
    }

    #[test]
    fn terminal_owned_repeat_clears_an_earlier_consumed_press() {
        let mut suppressed = std::collections::HashSet::new();
        let key = PhysicalKey::Code(KeyCode::ArrowUp);

        assert!(!track_consumed_key_release(
            &mut suppressed,
            key,
            ElementState::Pressed,
            true
        ));
        assert!(suppressed.contains(&key));

        // Adaptive focus can consume the first press, move to the edge, then
        // pass the next auto-repeat press through to the terminal.
        assert!(!track_consumed_key_release(
            &mut suppressed,
            key,
            ElementState::Pressed,
            false
        ));
        assert!(!suppressed.contains(&key));
        assert!(!track_consumed_key_release(
            &mut suppressed,
            key,
            ElementState::Released,
            false
        ));
    }
}
