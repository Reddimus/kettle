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
    /// Cycle 402: cross-window drag FSM state. Distinct from the in-window
    /// `tab_drag_active` reorder (cycle 249); both fire from the same
    /// mouse-down on the tab bar. Wired live in C6 of the multi-window cycle.
    pub(crate) detach_drag: crate::detach::DragState,
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
    pub(crate) last_click: Option<(std::time::Instant, usize, usize, u8)>,
    /// Last OS window title set (dedupe `set_title` syscalls).
    pub(crate) last_title: String,
    /// Cycle 412: pane ids whose shell exited + cfg.exit_action requested
    /// restart. Drained AFTER drain_events; dedup'd on push (cycle 452).
    pub(crate) pending_pane_restarts: Vec<u64>,
    /// Cycle 785: the window is created hidden so the user never sees an
    /// unpainted rectangle during GPU init; `redraw` reveals it on the first
    /// composited frame and flips this to `true`.
    pub(crate) window_shown: bool,
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
            last_click: None,
            last_title: String::new(),
            pending_pane_restarts: Vec::new(),
            window_shown: false,
        }
    }
}
