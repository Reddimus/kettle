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
use winit::keyboard::ModifiersState;
use winit::window::{CursorIcon, Window};

use kettle_render::Renderer;

use crate::app::{
    ConfirmDialogState, ContextMenuState, HintTarget, LinksScanKey, SearchScanKey, SplitDrag,
    TitleEditState, ViState,
};
use crate::mux::Mux;

/// Multi-window cycle (Peacock): the accent this window resolved + claimed.
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
    /// Cycle 745: OS taskbar progress, driven by the focused pane's OSC 9;4
    /// state each frame (pwsh 7 / Windows Terminal parity). No-op off Windows.
    pub(crate) taskbar: crate::taskbar::Taskbar,
    /// Cycle 869: true while an OS attention request (taskbar flash / dock
    /// bounce) is outstanding. winit's `request_user_attention(None)` doesn't
    /// reliably stop the Win11 taskbar flash, so we track outstanding requests
    /// and clear them directly via `Taskbar::clear_attention` on focus-gain.
    pub(crate) attention_active: bool,
    /// This window's tab/pane multiplexer. Panes are self-contained (PTY +
    /// reader thread + channels), so a tab can move between windows (C5) by
    /// moving its panes between Muxes — the PTYs never notice.
    pub(crate) mux: Mux,
    pub(crate) mods: ModifiersState,
    pub(crate) fullscreen: bool,
    pub(crate) cursor: PhysicalPosition<f64>,
    pub(crate) selecting: bool,
    /// Dragging the focused pane's scrollbar thumb.
    pub(crate) dragging_scrollbar: bool,
    /// Cycle 904 (audit): in-progress mouse drag of a split divider. `Some`
    /// while the left button is held after a press landed on a divider seam;
    /// each CursorMoved recomputes the addressed split's ratio from the cursor.
    pub(crate) dragging_split: Option<SplitDrag>,
    /// `(query, index)` last scrolled-to, so the viewport follows search
    /// matches into scrollback without re-scrolling every frame.
    pub(crate) search_revealed: Option<(String, usize)>,
    /// Cycle 803: cache key for the last completed search scan — see app.rs
    /// (`update_search` re-scans only when query/focus/output changed).
    pub(crate) search_scan_key: Option<SearchScanKey>,
    /// Cycle 803: cache key for the last viewport link-autodetect scan — see
    /// app.rs (`update_links` re-scans only on output, scroll, or focus).
    pub(crate) links_scan_key: Option<LinksScanKey>,
    /// v2.20.0 P6: when the last link scan actually ran — output-only changes
    /// within `LINKS_SCAN_DEBOUNCE` of it are skipped (streaming floods moved
    /// `last_output_at` every painted frame).
    pub(crate) last_links_scan: Option<std::time::Instant>,
    pub(crate) mouse_btn: Option<u8>,
    /// Last `(row, col)` reported to a mouse-tracking app, so cell-motion
    /// reports (1002/1003) fire only on a cell crossing — xterm coalesces
    /// intra-cell moves; a fast drag would otherwise flood one SGR report
    /// per pixel of travel (cycle 842, audit).
    pub(crate) last_mouse_cell: Option<(usize, usize)>,
    pub(crate) links: Vec<kettle_core::Link>,
    pub(crate) ssh_input: Option<String>,
    /// `Some((query, selected))` while the command palette is open.
    pub(crate) palette_input: Option<(String, usize)>,
    /// Cycle 756: `Action::OpenSettings` overlay navigation. `Some` while the
    /// in-app settings panel is open.
    pub(crate) settings_nav: Option<crate::settings::SettingsNav>,
    /// Cycle 708 (Terminator parity): `Action::OpenLayoutPicker` modal state —
    /// (typed query, selected index) against `Session::list_layouts`.
    pub(crate) layout_picker_input: Option<(String, usize)>,
    /// Active quick-select hint mode: detected targets + typed prefix.
    pub(crate) hint_state: Option<(Vec<HintTarget>, String)>,
    /// Right-click context menu state (`Some` while open). Lives next
    /// to the other modal overlays — same close-all-modals discipline,
    /// same Esc-to-dismiss key route.
    pub(crate) context_menu: Option<ContextMenuState>,
    /// Cycle 369: when `Some`, the user is editing a window/tab/pane title via
    /// an inline overlay.
    pub(crate) editing_title: Option<TitleEditState>,
    /// Cycle 648: when `Some`, a confirm modal is open. Keyboard input routes
    /// to modal dispatch and the renderer paints the centered modal panel.
    pub(crate) confirm_dialog: Option<ConfirmDialogState>,
    pub(crate) window_focused: bool,
    /// True while the OS mouse cursor is hidden because the user is typing
    /// (`mouse-hide-while-typing`). Re-shown on the next mouse movement.
    pub(crate) mouse_hidden: bool,
    /// Last `CursorIcon` we pushed to the window — used to dedupe so we
    /// don't issue a `set_cursor` syscall on every CursorMoved event.
    pub(crate) last_cursor_icon: Option<CursorIcon>,
    /// Cycle 249: drag-to-reorder tab state. `Some(_)` while a left-
    /// mouse-button press in the tab bar is being held; cleared on release.
    pub(crate) tab_drag_active: bool,
    /// Cycle 402: tab tear-off drag FSM state. Distinct from the in-window
    /// `tab_drag_active` reorder (cycle 249); both fire from the same
    /// mouse-down on the tab bar. Wired live in C6 of the multi-window
    /// cycle: a release while DraggingOutside tears the tab off into a new
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
    /// Cycle 298 vi-mode (Alacritty parity): `Some(ViState)` while kettle is
    /// intercepting keys for vi-style navigation.
    pub(crate) vi_mode: Option<ViState>,
    /// Cycle 693 Terminator parity (`key_scaled_zoom`): font size saved on
    /// entering scaled zoom so leave-zoom restores it exactly.
    pub(crate) scaled_zoom_prev_font_size: Option<f32>,
    /// Cycle 703: the pane id we last fired the Lua focus-change event for.
    pub(crate) last_emitted_focus: Option<u64>,
    /// Cycle 704: last title we emitted a `title_changed` Lua event for,
    /// keyed by pane id. Self-prunes (only live panes are iterated).
    pub(crate) last_emitted_titles: std::collections::HashMap<u64, String>,
    pub(crate) blink_on: bool,
    pub(crate) last_blink: std::time::Instant,
    pub(crate) last_bell: Option<std::time::Instant>,
    /// Cycle 910 (R2): coalesce output-driven repaints. `last_paint` is when
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
    /// Cycle 412: pane ids whose shell exited + cfg.exit_action requested
    /// restart. Drained AFTER drain_events; dedup'd on push (cycle 452).
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
    /// Multi-window cycle (Peacock): this window's resolved accent claim.
    /// `None` while unresolved (first frame) or when the user opted out
    /// (`accent-color = theme`/`off`/`none` or a pinned hex). Kept in sync
    /// each frame by `App::sync_window_accent`.
    pub(crate) accent: Option<WindowAccent>,
    /// PERF (key-repeat stutter fix): when the user last typed bytes into a
    /// PTY in this window. Output arriving within `TYPING_ECHO_WINDOW` of a
    /// keystroke paints IMMEDIATELY (request_redraw is vsync-coalesced, so
    /// this can't outpace the display) instead of through the cycle-910
    /// output coalescer — whose WaitUntil deadline has ~16ms timer
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
            taskbar: crate::taskbar::Taskbar::new(),
            attention_active: false,
            mux,
            mods: ModifiersState::empty(),
            fullscreen,
            cursor: PhysicalPosition::new(0.0, 0.0),
            selecting: false,
            dragging_scrollbar: false,
            dragging_split: None,
            search_revealed: None,
            search_scan_key: None,
            links_scan_key: None,
            last_links_scan: None,
            mouse_btn: None,
            last_mouse_cell: None,
            links: Vec::new(),
            ssh_input: None,
            palette_input: None,
            settings_nav: None,
            layout_picker_input: None,
            hint_state: None,
            context_menu: None,
            editing_title: None,
            confirm_dialog: None,
            window_focused: true,
            mouse_hidden: false,
            last_cursor_icon: None,
            tab_drag_active: false,
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
