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
    ConfirmDialogState, ContextMenuState, HintTarget, LinksScanKey, SplitDrag, TitleEditState,
    ViState,
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
            }
            false
        }
        ElementState::Released => suppressed.remove(&physical_key),
    }
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
    /// Last `(row, col)` reported to a mouse-tracking app, so cell-motion
    /// reports (1002/1003) fire only on a cell crossing — xterm coalesces
    /// intra-cell moves; a fast drag would otherwise flood one SGR report
    /// per pixel of travel.
    pub(crate) last_mouse_cell: Option<(usize, usize)>,
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
    /// Coalesce output-driven repaints (R2). `last_paint` is when
    /// the last frame painted; `coalescing_paint` marks a deferred output
    /// paint whose frame budget has not elapsed yet. Input/cursor paints
    /// bypass this (they call `request_redraw` directly).
    pub(crate) last_paint: Option<std::time::Instant>,
    pub(crate) coalescing_paint: bool,
    /// v2.21.1 (throughput): consecutive output-coalesced frames — i.e. how
    /// sustained the current PTY-output flood is. `effective_output_budget`
    /// stretches the paint budget (60→30→20 fps) as this climbs, so fewer
    /// per-frame snapshots are taken under the `Term` lock the PTY reader needs;
    /// reset to 0 by any non-coalesced paint (`redraw`).
    pub(crate) flood_paints: u32,
    pub(crate) last_click: Option<(std::time::Instant, usize, usize, u8)>,
    /// Last OS window title set (dedupe `set_title` syscalls).
    pub(crate) last_title: String,
    /// Pane ids whose shell exited + cfg.exit_action requested
    /// restart. Drained AFTER drain_events; dedup'd on push.
    pub(crate) pending_pane_restarts: Vec<u64>,
    /// Whether a visible-state window has already been shown. Startup creates
    /// the OS window hidden during renderer init, then reveals visible states
    /// once the surface is configured; `window_state = hidden` keeps this true
    /// so fallback reveal paths do not show it.
    pub(crate) window_shown: bool,
    /// C4: per-pane `Terminal::output_generation` values as of this window's
    /// last paint (snapshotted at the top of `redraw`, before drain_events).
    /// The fan-out `UserEvent::Wakeup` compares against the live counters to
    /// decide whether THIS window has anything new to paint.
    pub(crate) seen_output_gen: std::collections::HashMap<u64, u64>,
    /// Multi-window effort (Peacock): this window's resolved accent claim.
    /// `None` while unresolved (first frame) or when the user opted out
    /// (`accent-color = theme`/`off`/`none` or a pinned hex). Kept in sync
    /// each frame by `App::sync_window_accent`.
    pub(crate) accent: Option<WindowAccent>,
    /// PERF (key-repeat stutter fix): when the user last typed bytes into a
    /// PTY in this window. Output arriving within `TYPING_ECHO_WINDOW` of a
    /// keystroke paints IMMEDIATELY (request_redraw is vsync-coalesced, so
    /// this can't outpace the display) instead of through the
    /// output coalescer (`coalescing_paint`) — whose WaitUntil deadline has ~16ms timer
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
            suppressed_key_releases: std::collections::HashSet::new(),
            ime_preedit: None,
            ime_preedit_owner: None,
            ime_focus_generation: 0,
            fullscreen,
            cursor: PhysicalPosition::new(0.0, 0.0),
            wheel: crate::input::WheelAccum::default(),
            selecting: false,
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
            dock_preview: None,
            hovered_close_idx: None,
            vi_mode: None,
            scaled_zoom_prev_font_size: None,
            last_emitted_focus: None,
            last_emitted_titles: std::collections::HashMap::new(),
            blink_on: true,
            last_blink: std::time::Instant::now(),
            last_bell: None,
            last_paint: None,
            coalescing_paint: false,
            flood_paints: 0,
            last_click: None,
            last_title: String::new(),
            pending_pane_restarts: Vec::new(),
            window_shown: false,
            seen_output_gen: std::collections::HashMap::new(),
            accent: None,
            last_typed: None,
            resize_overlay: None,
            seen_first_resize: false,
            spawned_at: std::time::Instant::now(),
            pane_snapshots: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{KeyCode, PhysicalKey};

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
}
