//! Opening and closing windows, tearing tabs off, docking them and moving them between windows.

use super::*;

#[cfg(any(target_os = "linux", test))]
fn wayland_drag_destination(
    performed: bool,
    source: u64,
    carrier: u64,
    source_index: usize,
    target: Option<(u64, usize)>,
) -> Option<(u64, usize)> {
    if !performed && source != carrier {
        Some((source, source_index))
    } else if performed {
        target
    } else {
        None
    }
}

impl App {
    /// Tear this window down for good: flush the recording, save the session,
    /// and flag the close for `finish_window_dispatch`.
    ///
    /// Every window-close path ends here, whether the user was asked first or
    /// not, so no gesture can close the window without flushing the PTY output
    /// still in flight. (`Drop for App` still finishes the recorder as a
    /// backstop for crashes; it cannot do this fan-out, because by then there
    /// is no window to fan out from.)
    pub(super) fn close_window_now(&mut self, ws: &mut WindowState, drop_panes: DropPanes) {
        // Fan out in-flight shared PTY output, then hand finalization to
        // the writer before exit. Finalization can drain recorder events
        // already admitted; it cannot pull the shared sidechannel or
        // deliver its tail to Lua.
        //
        // This MUST run before anything drops panes: the flush walks
        // `ws.mux.panes` and reads each one's output sidechannel, so a caller
        // that cleared the mux first handed it an empty map and lost the whole
        // tail. `DropPanes` is why the clearing lives here rather than at the
        // call sites — the order is the point.
        self.flush_recorder_output(ws);
        // Every pane ends with its window, whether or not the mux is kept for
        // the saved session; plugins see each one go.
        self.fire_closed_pane_events(ws);
        let mut remaining: Vec<u64> = ws.mux.panes.keys().copied().collect();
        remaining.sort_unstable();
        for pane_id in remaining {
            self.fire_pane_close_event(ws, pane_id);
        }
        // The recorder spans the whole session, so finish it only when the LAST
        // window goes (this one is checked out of the map, so empty == last).
        if self.windows.is_empty()
            && let Some(rec) = self.recorder.as_mut()
        {
            // The process may be leaving, but a vanished mount must still not
            // park winit inside a final flush or close.
            rec.begin_finish();
        }
        if drop_panes == DropPanes::Yes {
            // `close_window` means "this window is finished, do not bring it
            // back": clear the mux so the session saved below is the empty
            // one. Otherwise the multi-tab state from before the close stays
            // in session.json and silently restores on next launch.
            ws.mux.close_window();
        }
        self.save_session(ws);
        self.request_window_close(ws.seq);
    }

    pub(super) fn promote_tab_drag_if_needed(&mut self, ws: &mut WindowState) {
        if let Some((ox, oy)) = ws.tab_drag_press {
            let dx = ws.cursor.x as f32 - ox;
            let dy = ws.cursor.y as f32 - oy;
            if (dx * dx + dy * dy).sqrt() > tab_reorder_drag_threshold_px(self.tab_bar_h(ws)) {
                ws.tab_drag_press = None;
                ws.tab_pressed_idx = None;
            }
        }
    }

    /// Move the active tab to the segment under the cursor, hit-testing the
    /// same rendered segments that clicks and painting use, on whichever axis
    /// the bar is stacked. Returns whether the tab moved.
    pub(super) fn reorder_active_tab_for_cursor(&mut self, ws: &mut WindowState) -> bool {
        let bar = self.tab_bar(ws);
        if bar.height <= 0.0 || bar.segments.is_empty() {
            return false;
        }
        let target = tab_drag_target_index(ws.cursor.x as f32, ws.cursor.y as f32, &bar.segments);
        let delta = target as i32 - ws.mux.active as i32;
        let moved = delta != 0 && ws.mux.move_active_tab(delta);
        if moved {
            ws.mux.touch_active_tab_seen();
        }
        moved
    }

    /// Window attributes shared by window 1 (`resumed_inner`) and
    /// windows 2..N (`open_window`), so a second window honors borderless /
    /// always-on-top / hide-from-taskbar / geometry-hinting / WM_CLASS
    /// exactly like the first. Always returns `visible(false)`; callers
    /// decide when to reveal.
    /// `monitor` fits the default size (see `default_startup_inner_size`);
    /// a restored geometry or explicit size supplied later still overrides it.
    pub(super) fn window_attributes(
        &self,
        state: kettle_config::WindowState,
        monitor: Option<StartupMonitor>,
    ) -> winit::window::WindowAttributes {
        let mut attrs = Window::default_attributes()
            .with_title("kettle")
            // Alpha-capable surfaces can cost compositor work and subtly alter
            // text blending even when every rendered pixel is opaque. Ask for
            // one only when this window can actually expose the desktop or a
            // native material. A runtime toggle takes full effect in newly
            // opened windows, as documented by the setting itself.
            .with_transparent(
                kettle_render::window_requires_alpha_surface(&self.cfg) || self.cfg.window_blur,
            )
            // Show kettle's icon in the title bar / taskbar / Alt-Tab
            // for the running window (winit leaves it unset by default).
            .with_window_icon(load_window_icon(self.cfg.theme.is_dark()))
            // Seed the native titlebar (Windows DWM caption, Wayland
            // Adwaita CSD) to match the active palette from the first frame;
            // runtime changes go through `maybe_sync_native_theme`.
            .with_theme(creation_theme_hint(
                self.native_theme_hint(),
                cfg!(target_os = "macos"),
            ));
        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
        {
            // Wayland implements this through KWin's blur protocol. Other
            // compositors ignore the hint and retain alpha-only transparency.
            attrs = attrs.with_blur(self.cfg.window_blur);
        }
        #[cfg(target_os = "windows")]
        if self.cfg.window_blur {
            use winit::platform::windows::{BackdropType, WindowAttributesExtWindows as _};
            attrs = attrs.with_system_backdrop(BackdropType::TransientWindow);
        }
        #[cfg(target_os = "macos")]
        {
            // Keep AppKit's title, traffic lights, rounded mask and drag region.
            // Native material stays below this caption; AppKit draws the
            // opaque control strip in the selected light or dark appearance.
            // Full-size content would instead put tabs and cells under the
            // traffic lights, changing geometry and pointer hit-testing.
            attrs = apply_macos_window_chrome(attrs, self.cfg.borderless);
        }
        // Terminator parity, terminatorlib/config.py:75 +
        // 78. `borderless` removes OS chrome; `always-on-top` keeps
        // the window above other windows. Best-effort per OS; failure
        // modes degrade silently (e.g. Wayland respects compositor
        // rules over our hint).
        if self.cfg.borderless && !cfg!(target_os = "macos") {
            attrs = attrs.with_decorations(false);
        }
        if self.cfg.always_on_top {
            attrs = attrs.with_window_level(winit::window::WindowLevel::AlwaysOnTop);
        }
        // Terminator parity, terminatorlib/config.py:79
        // `hide_from_taskbar`: on Windows, winit 0.30 exposes
        // `WindowAttributesExtWindows::with_skip_taskbar`. Other platforms
        // log instead (below).
        #[cfg(target_os = "windows")]
        if self.cfg.hide_from_taskbar {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = WindowAttributesExtWindows::with_skip_taskbar(attrs, true);
        }
        // On non-Windows the key parses but there's no winit API to
        // honor it (X11 would need a `_NET_WM_STATE_SKIP_TASKBAR` atom write).
        // Log so a user porting a Terminator config knows it's recognized but
        // not yet applied here, like the X11/Wayland `sticky` log below. A
        // silent no-op would leave them unable to tell parse-failed from
        // not-implemented.
        #[cfg(not(target_os = "windows"))]
        if self.cfg.hide_from_taskbar {
            log::info!(
                "kettle: `hide_from_taskbar = true` is not yet applied on this \
                 platform (winit 0.30 exposes the API on Windows only); the \
                 window will still appear in the taskbar/dock"
            );
        }
        // Terminator parity, terminatorlib/config.py:75
        // `window_state`. Apply initial window state at creation.
        match state {
            kettle_config::WindowState::Normal => {}
            kettle_config::WindowState::Maximise => {
                attrs = attrs.with_maximized(true);
            }
            kettle_config::WindowState::Fullscreen => {
                attrs = attrs.with_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
            }
            kettle_config::WindowState::Hidden => {
                attrs = attrs.with_visible(false);
            }
        }
        // Logical pixels on purpose: the grid is the contract, and winit
        // scales a logical size per monitor, so a 2x display opens with the
        // same columns a 1x display gets for the same config.
        let (w, h) = startup_inner_size(&self.cfg, monitor);
        attrs = attrs.with_inner_size(winit::dpi::LogicalSize::new(w, h));
        if self.cfg.window_position_x.is_some() || self.cfg.window_position_y.is_some() {
            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(
                self.cfg.window_position_x.unwrap_or(0),
                self.cfg.window_position_y.unwrap_or(0),
            ));
        }
        // Terminator parity, terminatorlib/config.py:74
        // `geometry_hinting`: when true, request that the WM resize
        // the window in font-cell increments (so a drag-resize lands
        // on exact column/row boundaries vs sub-cell sliver). Uses
        // an approximate cell size — actual font metrics aren't
        // available yet at attrs-build time, so we use 8x16 px as a
        // typical Mono baseline. Honored best-effort per OS (X11
        // honors via WM_SIZE_HINTS; Wayland varies; macOS doesn't).
        if self.cfg.geometry_hinting {
            attrs = attrs.with_resize_increments(winit::dpi::LogicalSize::new(8.0_f64, 16.0_f64));
        }
        // Set WM_CLASS / Wayland app_id explicitly so GNOME / KDE
        // task switchers, dock pins, and the `StartupWMClass=kettle`
        // line in `packaging/linux/kettle.desktop` all line up. Without
        // it, winit sets no Wayland app_id and Wayland windows show up as
        // generic "Unknown" in the activities overview.
        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
        let attrs = {
            // Both `WindowAttributesExtWayland::with_name` and
            // `WindowAttributesExtX11::with_name` write to the same
            // `platform_specific.name` field — calling either suffices.
            // We pick X11 here and fully-qualify the call so the import
            // doesn't put both methods in scope (which would make
            // `attrs.with_name(…)` ambiguous).
            use winit::platform::x11::WindowAttributesExtX11;
            // Derive WM_CLASS from the running binary's stem
            // (default "kettle") instead of hardcoding, so a fork or renamed
            // binary groups correctly in GNOME/KDE task switchers without
            // editing code. The canonical build is `kettle`, so this matches
            // `StartupWMClass=kettle` in packaging/linux/kettle.desktop exactly.
            let wm_class = std::env::current_exe()
                .ok()
                .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "kettle".to_string());
            WindowAttributesExtX11::with_name(attrs, wm_class.clone(), wm_class)
        };
        attrs.with_visible(false)
    }

    /// Post-creation window setup shared by both window-creation paths.
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    pub(super) fn apply_post_create(&self, window: &Window) {
        window.set_ime_allowed(true);
        #[cfg(target_os = "macos")]
        {
            set_macos_option_as_alt(window, self.cfg.macos_option_as_alt);
        }
        // Terminator parity, terminatorlib/config.py:81
        // `sticky`: show window on every workspace. winit 0.30 dropped
        // `WindowExtMacOS::set_visible_on_all_workspaces`, so macOS sets
        // `NSWindowCollectionBehavior::CanJoinAllSpaces | Stationary` through
        // the raw NSWindow handle after construction (see
        // `set_visible_on_all_spaces`).
        #[cfg(target_os = "macos")]
        if self.cfg.sticky {
            set_visible_on_all_spaces(window);
        }
        // X11/Wayland sticky needs a `_NET_WM_STATE_STICKY` atom
        // write that winit 0.30 doesn't expose; log (don't silently no-op) so
        // the user knows the key is recognized but not yet applied here.
        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
        if self.cfg.sticky {
            log::info!(
                "kettle: `sticky = true` is not yet applied on X11/Wayland \
                 (winit 0.30 exposes no API for `_NET_WM_STATE_STICKY`); the \
                 window stays on its current workspace"
            );
        }
    }

    pub(super) fn start_window_frames(&mut self, ws: &mut WindowState) {
        let Some(window) = ws.window.clone() else {
            return;
        };
        // Winit draws Wayland CSD before RedrawRequested. An earlier client
        // buffer clips the unmapped decorations out of the window geometry.
        if !window_is_wayland(&window) {
            self.redraw(ws);
        }
        window.request_redraw();
    }

    /// Open another OS window in this process (the multi-window core).
    /// Returns the new window's seq on success; returns the `WindowOpen`
    /// back on failure so an `AdoptTab` caller can re-attach the tab to its
    /// source window instead of losing live PTYs. `size` overrides the
    /// platform-default inner size (a torn-off window inherits the source
    /// window's dimensions, Windows Terminal parity).
    pub(super) fn open_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        open: WindowOpen,
        pos: Option<winit::dpi::Position>,
        size: Option<winit::dpi::PhysicalSize<u32>>,
    ) -> Result<u64, Box<WindowOpen>> {
        let Some(gpu) = self.gpu.clone() else {
            log::warn!("open_window: GPU context not ready (window 1 still initializing)");
            return Err(Box::new(open));
        };
        let source_color = match &open {
            WindowOpen::AdoptTab { source_color, .. } => *source_color,
            _ => None,
        };
        // A tear-off window opens Normal (it must be visible at the drop
        // point); a fresh window honors the configured window-state except
        // Hidden (an invisible NewWindow helps nobody).
        let state = match (&open, self.cfg.window_state) {
            (WindowOpen::AdoptTab { .. }, _) | (WindowOpen::Restore(_), _) => {
                kettle_config::WindowState::Normal
            }
            (_, kettle_config::WindowState::Hidden) => kettle_config::WindowState::Normal,
            (_, s) => s,
        };
        let monitor = startup_monitor(event_loop);
        let mut attrs = self.window_attributes(state, monitor);
        // Load the fonts before the window, as the first window does: a fresh
        // normal window then opens at exactly the configured grid, and the
        // renderer takes the fonts over.
        let fonts =
            kettle_render::StartupFonts::load(&self.cfg, monitor.map_or(1.0, |m| m.scale) as f32);
        if matches!(open, WindowOpen::Fresh { .. })
            && matches!(state, kettle_config::WindowState::Normal)
            && pos.is_none()
            && size.is_none()
        {
            let mut probe = WindowState::new(0, Mux::new());
            probe.startup_cell = Some(fonts.cell);
            if let Some((width, height)) = self.startup_surface(&probe, monitor) {
                attrs = attrs.with_inner_size(winit::dpi::PhysicalSize::new(width, height));
            }
        }
        // A restored window lands at its saved geometry, clamped to the
        // live monitor layout (its monitor may be unplugged).
        if let WindowOpen::Restore(sw) = &open
            && let Some(g) = sw.geometry
        {
            let g = crate::session::clamp_geometry_to_monitors(g, &monitor_rects(event_loop));
            attrs = attrs
                .with_position(winit::dpi::PhysicalPosition::new(g.x, g.y))
                .with_inner_size(winit::dpi::PhysicalSize::new(g.w, g.h));
        }
        if let Some(p) = pos {
            attrs = attrs.with_position(p);
        }
        if let Some(s) = size {
            attrs = attrs.with_inner_size(s);
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("open_window: window creation failed: {e}");
                return Err(Box::new(open));
            }
        };
        self.apply_post_create(&window);
        let native_material =
            crate::native_material::NativeMaterial::install(&window, &self.cfg, self.proxy.clone());
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        // Synchronous renderer init against the shared device — no block_on,
        // no watchdog (those are window-1 costs; see Renderer::new_with_gpu).
        let renderer = match Renderer::new_with_gpu(
            &gpu,
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            scale,
            &self.cfg,
            Some(fonts),
        ) {
            Ok(r) => r,
            Err(e) => {
                log::error!("open_window: renderer init failed: {e}");
                return Err(Box::new(open));
            }
        };
        let seq = self.next_window_seq;
        self.next_window_seq += 1;
        let accessibility = Self::new_accessibility_adapter(
            event_loop,
            &window,
            seq,
            self.proxy.clone(),
            &self.ui_text,
        );
        // Same Mux construction flags as run_with — process-global decisions.
        let mut mux = Mux::new();
        mux.lua_output_subscribed = self.lua_engine.is_some();
        mux.osc52_copy_allowed = osc52_copy_is_available(self.cfg.osc52, self.clipboard.is_some());
        mux.autoclean_groups = self.cfg.autoclean_groups;
        {
            // The policy resolved at startup covers config-driven recording
            // too; a later config change does not start a recorder.
            let recording_requested = self.recorder.is_some()
                || matches!(
                    self.startup.recording,
                    crate::RecordingPolicy::Enabled { .. }
                );
            mux.lua_output_subscribed |= recording_requested;
            mux.record_lossless = recording_requested;
        }
        let mut ws = WindowState::new(seq, mux);
        ws.window_shown = !should_reveal_after_renderer_init(state);
        ws.renderer = Some(renderer);
        ws.accessibility = Some(accessibility);
        ws.window = Some(window);
        ws.native_caption_drag = ws
            .window
            .as_deref()
            .map(crate::native_drag::NativeCaptionDrag::install);
        ws.native_fullscreen = ws.window.as_deref().map(|window| {
            crate::native_fullscreen::NativeFullscreen::install(window, seq, self.proxy.clone())
        });
        ws.native_material = Some(native_material);
        // A torn-off tab keeps its window's font zoom, before the grid is
        // sized: the drag anchor was measured with those metrics.
        if let WindowOpen::AdoptTab {
            font_size: Some(font_size),
            ..
        } = &open
            && let Some(renderer) = ws.renderer.as_mut()
        {
            renderer.set_font_size(*font_size);
        }
        self.sync_output_frame_budget(&mut ws, true);
        let area = self.area(&ws);
        let (cols, rows) = self.grid_of(&ws, area);
        let geometry = self.pty_geometry_for_grid(&ws, cols, rows);
        match open {
            WindowOpen::Fresh { cwd } => {
                if let Err(e) = ws.mux.new_tab_with_geometry(
                    &self.cfg,
                    geometry,
                    self.waker(),
                    &[],
                    cwd.as_deref(),
                ) {
                    // `ws` (and its OS window) drop here; nothing was adopted.
                    log::error!("open_window: shell spawn failed: {e}");
                    return Err(Box::new(WindowOpen::Fresh { cwd }));
                }
            }
            WindowOpen::AdoptTab {
                tab: dt,
                paste,
                scaled_zoom_prev_font_size,
                ..
            } => {
                ws.scaled_zoom_prev_font_size = scaled_zoom_prev_font_size;
                ws.mux.attach_tab(dt, None);
                ws.adopt_moved_paste_state(*paste);
            }
            WindowOpen::Restore(sw) => {
                let sess = crate::session::Session {
                    tabs: sw.tabs.clone(),
                    active: sw.active,
                    theme: None,
                    windows: Vec::new(),
                };
                // Route through the LATCHED waker constructor. A hand-rolled
                // closure bypasses the wakeup-dedup latch, so restored panes
                // would bring back the one-wakeup-per-read flood it eliminates.
                let mk = || self.waker();
                let geometries = self.restore_geometries(&ws, &sess);
                let outcome = ws.mux.restore_geometry(&sess, &self.cfg, &geometries, &mk);
                if !outcome.restored_any {
                    // `ws` (and its OS window) drop here; nothing restored.
                    log::error!("open_window: saved-window restore failed");
                    return Err(Box::new(WindowOpen::Restore(sw)));
                }
                if outcome.skipped > 0 {
                    self.stash_unrestorable_session(&format!(
                        "{} tab(s) could not be rebuilt",
                        outcome.skipped
                    ));
                }
            }
        }
        // Apply a due scheduled theme switch first, as the first redraw would,
        // so a torn window excludes its source's color from the pool it will
        // paint with rather than from the outgoing theme's pool.
        self.poll_theme_schedule(&mut ws);
        if self.cfg.accent_auto && self.cfg.accent_color.is_none() {
            self.assign_window_accent(&mut ws, source_color);
        }
        self.resize_all(&mut ws);
        self.sync_output_wake_gate(&ws);
        self.focused_seq = seq;
        if should_reveal_before_first_surface_frame(
            state,
            ws.window_shown,
            cfg!(target_os = "macos"),
        ) {
            if let Some(w) = &ws.window {
                w.set_visible(true);
            }
            ws.window_shown = true;
        }
        // A successful presentation reveals the restored surface. Wayland
        // waits for Winit's CSD paint; macOS is revealed above so Metal can
        // acquire a drawable from its visible NSWindow.
        #[cfg(target_os = "linux")]
        if let (Some(bridge), Some(window)) = (self.wayland.as_mut(), ws.window.clone()) {
            bridge.attach_drag_carrier(ws.seq, window);
        }
        self.start_window_frames(&mut ws);
        self.windows.insert(seq, ws);
        Ok(seq)
    }

    /// Chromium-style tear-off. The moment the dragged tab crosses the band
    /// threshold, detach it into a live window under the cursor, inheriting
    /// the source dimensions and positioned so the pointer keeps holding the
    /// tab, then hand the drag to the OS via `drag_window()` (Windows:
    /// ReleaseCapture + WM_NCLBUTTONDOWN/HTCAPTION posted to the torn window,
    /// the exact Chromium handoff; X11: _NET_WM_MOVERESIZE). macOS follows the
    /// pointer manually instead. Returns true when the source window no longer
    /// owns the gesture.
    pub(super) fn maybe_tear_off(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        if !self.cfg.detachable_tabs {
            return false;
        }
        if !matches!(
            ws.detach_drag,
            crate::detach::DragState::DraggingInside { .. }
                | crate::detach::DragState::DraggingOutside { .. }
        ) {
            return false;
        }
        let Some(src) = ws.window.clone() else {
            return false;
        };
        let (cx, cy) = (ws.cursor.x as f32, ws.cursor.y as f32);
        let Some(band) = self.dock_band(ws) else {
            return false;
        };
        // 1.5× the bar thickness of slop in EVERY direction away from the
        // band (uniform hysteresis): drag along the strip = reorder, drag
        // past the slop = tear. ~36px at the default font.
        let threshold = 1.5 * self.tab_bar_h(ws);
        if !tear_threshold_crossed(cx, cy, band, threshold) {
            return false;
        }
        if window_is_wayland(&src) {
            #[cfg(target_os = "linux")]
            return self.begin_wayland_tab_drag(ws, event_loop, src);
            #[cfg(not(target_os = "linux"))]
            return false;
        }
        // Lone tab: the tab IS the window — drag the whole window instead
        // of tearing (Chromium semantics; tearing would just re-create the
        // same window). The torn-drag tracking still attaches, so the
        // full re-dock path applies: this is how a previously torn-off
        // window merges back into a sibling.
        if ws.mux.tabs.len() <= 1 {
            let press = ws
                .drag_press
                .map(|(x, y)| (f64::from(x), f64::from(y)))
                .unwrap_or((ws.cursor.x, ws.cursor.y));
            ws.tab_drag_active = false;
            ws.tab_drag_press = None;
            ws.tab_pressed_idx = None;
            ws.detach_drag = crate::detach::DragState::default();
            ws.drag_press = None;
            let origins = (src.inner_position(), src.outer_position());
            // AppKit requires the original mouse-down event for performDrag;
            // winit passes currentEvent here, which is already mouseDragged.
            let native = if cfg!(target_os = "macos") || tear_native_handoff_disabled() {
                false
            } else {
                match src.drag_window() {
                    Ok(()) => true,
                    Err(e) => {
                        log::info!(
                            "tab-drag window move: native drag unavailable ({e}); manual follow"
                        );
                        false
                    }
                }
            };
            // Manual follow keeps the original press under the pointer, even
            // when a native handoff failed or the first motion is the drop.
            let pointer = if native {
                (ws.cursor.x, ws.cursor.y)
            } else {
                press
            };
            let pointer_scale = drag_coordinate_scale(&src);
            let pointer = (pointer.0 / pointer_scale, pointer.1 / pointer_scale);
            let grab = match origins {
                (Ok(ip), Ok(op)) => crate::detach::frame_grab_offset(
                    drag_position(&src, ip),
                    drag_position(&src, op),
                    pointer,
                ),
                _ => (40.0, 12.0),
            };
            self.torn_drag = Some(TornDrag {
                seq: ws.seq,
                carrier: ws.seq,
                started: std::time::Instant::now(),
                grab,
                dock: None,
                native,
                caption: false,
                saw_move: false,
                last_signal: std::time::Instant::now(),
                hwnd: window_hwnd(&src),
                signal_cursor: None,
            });
            self.follow_torn_drag(ws);
            return true;
        }
        // The torn window inherits the source dimensions (WT tear-off
        // parity). A maximized/fullscreen source approximates its restored
        // size at 60% — winit exposes no restore bounds (Chromium uses
        // them; this is the closest available).
        let isz = src.inner_size();
        let scale = if src.is_maximized() || src.fullscreen().is_some() {
            0.6
        } else {
            1.0
        };
        let size = winit::dpi::PhysicalSize::new(
            ((f64::from(isz.width) * scale) as u32).max(400),
            ((f64::from(isz.height) * scale) as u32).max(300),
        );
        // Grab offset: where the pointer holds the torn window, relative to
        // its FRAME top-left (winit positions windows AND reports `Moved`
        // by the frame origin — verified against the vendored 0.30.13
        // WM_WINDOWPOSCHANGED handler). Two parts: the frame→client
        // decoration delta (caption + border, measured on the source —
        // same decoration config — so the pointer lands INSIDE the client,
        // on the strip, not on the native title bar), plus the in-strip
        // hold point in the TORN window's own layout: the strip sits at
        // the torn window's `tab-bar-pos` edge, so a Bottom/Right bar
        // anchors the pointer at the bottom/right of the torn frame —
        // without this the window hangs off the wrong side of the pointer.
        // Along the strip the pointer keeps its offset INTO the dragged
        // segment (WT's stashed drag offset, microsoft/terminal PR #14935),
        // clamped inside the torn frame.
        let ftc = match (src.inner_position(), src.outer_position()) {
            (Ok(ip), Ok(op)) => (f64::from(ip.x - op.x), f64::from(ip.y - op.y)),
            _ => (0.0, 0.0),
        };
        let bar = self.tab_bar(ws);
        let bar_h = f64::from(self.tab_bar_h(ws)).max(8.0);
        let seg = bar.segments.iter().find(|s| s.active).map(|s| s.rect);
        let in_seg_x = seg
            .map(|(sx, _, seg_w, _)| f64::from((cx - sx).clamp(16.0, (seg_w - 16.0).max(16.0))))
            .unwrap_or(40.0)
            .min(f64::from(size.width) - 24.0);
        let in_seg_y = seg
            .map(|(_, sy, _, seg_h)| f64::from((cy - sy).clamp(8.0, (seg_h - 8.0).max(8.0))))
            .unwrap_or(12.0)
            .min(f64::from(size.height) - 24.0);
        let strip_w = f64::from(self.cfg.tab_bar_width).max(8.0);
        let destination_band = self.tear_off_tab_band(ws, (size.width, size.height), bar_h as f32);
        let grab = match self.cfg.tab_bar_pos {
            TabBarPos::Top => (ftc.0 + in_seg_x, ftc.1 + bar_h * 0.5),
            TabBarPos::Bottom => (
                ftc.0 + in_seg_x,
                ftc.1 + f64::from(destination_band.1) + bar_h * 0.5,
            ),
            TabBarPos::Left => (ftc.0 + strip_w * 0.5, ftc.1 + in_seg_y),
            TabBarPos::Right => (
                ftc.0 + f64::from(size.width) - strip_w * 0.5,
                ftc.1 + in_seg_y,
            ),
        };
        let coordinate_scale = drag_coordinate_scale(&src);
        let grab = (grab.0 / coordinate_scale, grab.1 / coordinate_scale);
        // Frame origin = screen cursor − grab. `inner_position` is the
        // client origin in screen coords, so client cursor + it = screen
        // cursor exactly; `outer_position` is the (rare) fallback.
        let pos = src
            .inner_position()
            .or_else(|_| src.outer_position())
            .ok()
            .map(|p| {
                let origin = drag_position(&src, p);
                let cursor = crate::detach::client_to_desktop(
                    origin,
                    (ws.cursor.x, ws.cursor.y),
                    coordinate_scale,
                );
                drag_native_position((cursor.0 - grab.0, cursor.1 - grab.1))
            });
        // The dragged tab is the ACTIVE tab. The drag-to-reorder gesture keeps
        // it active, while the FSM's armed index can go stale across reorders.
        let closing_idx = ws.mux.active;
        self.release_held_buttons_in_tab(ws, closing_idx);
        let pre_focus = self.focus_key(ws);
        let Some(dt) = ws.mux.detach_tab(closing_idx) else {
            return false;
        };
        // The gesture leaves this window no matter how open_window goes.
        ws.tab_drag_active = false;
        ws.tab_drag_press = None;
        ws.tab_pressed_idx = None;
        ws.detach_drag = crate::detach::DragState::default();
        ws.drag_press = None;
        self.release_focus_report_to(ws, &dt);
        let paste = ws.take_moved_paste_state(&dt.tab, std::time::Instant::now());
        match self
            .open_window(
                event_loop,
                WindowOpen::AdoptTab {
                    tab: dt,
                    source_color: self.tear_off_source_color(ws),
                    font_size: ws.renderer.as_ref().map(|r| r.font_size()),
                    scaled_zoom_prev_font_size: ws.scaled_zoom_prev_font_size,
                    paste: Box::new(paste),
                },
                pos,
                Some(size),
            )
            .map_err(|open| *open)
        {
            Ok(torn_seq) => {
                self.note_focus_change(ws, pre_focus);
                self.fire_tab_close_event(ws, closing_idx);
                self.ctl_broadcast(
                    "tab_moved",
                    None,
                    serde_json::json!({
                        "from_window": ws.seq,
                        "to_window": torn_seq,
                        "tab": closing_idx,
                    }),
                );
                self.resize_all(ws);
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
                // Re-anchor to the LIVE cursor: the pointer kept moving
                // during window creation, and the Windows modal move loop
                // anchors at the cursor's CURRENT position — without this
                // the grab offset drifts by however far the pointer slid
                // (see `cursor_screen_pos`).
                #[cfg(target_os = "windows")]
                if let Some((lx, ly)) = cursor_screen_pos()
                    && let Some(tw) = self.windows.get(&torn_seq).and_then(|t| t.window.as_ref())
                {
                    tw.set_outer_position(winit::dpi::PhysicalPosition::new(
                        (lx - grab.0) as i32,
                        (ly - grab.1) as i32,
                    ));
                }
                // The native handoff. On Err, manual-follow: the source
                // still holds mouse capture, so its CursorMoved stream
                // repositions the torn window until release. macOS goes
                // straight to manual-follow: performWindowDragWithEvent on
                // the TORN window would consume NSApp.currentEvent — a
                // mouseDragged belonging to the SOURCE window — which is
                // unsound for a window that never saw the press. A lone-tab
                // drag uses manual-follow too: currentEvent is a mouseDragged,
                // while AppKit requires the original mouse-down event.
                let native = if cfg!(target_os = "macos") || tear_native_handoff_disabled() {
                    false
                } else {
                    self.windows
                        .get(&torn_seq)
                        .and_then(|t| t.window.as_ref())
                        .map(|w| match w.drag_window() {
                            Ok(()) => true,
                            Err(e) => {
                                log::info!(
                                    "tear-off: native drag unavailable ({e}); manual follow"
                                );
                                false
                            }
                        })
                        .unwrap_or(false)
                };
                self.torn_drag = Some(TornDrag {
                    seq: torn_seq,
                    carrier: ws.seq,
                    started: std::time::Instant::now(),
                    grab,
                    dock: None,
                    native,
                    caption: false,
                    saw_move: false,
                    last_signal: std::time::Instant::now(),
                    hwnd: self
                        .windows
                        .get(&torn_seq)
                        .and_then(|t| t.window.as_deref())
                        .and_then(window_hwnd),
                    signal_cursor: None,
                });
                self.follow_torn_drag(ws);
            }
            Err(WindowOpen::AdoptTab { tab: dt, paste, .. }) => {
                log::warn!("tear-off: open_window failed; tab kept in source window");
                ws.mux.attach_tab(dt, Some(closing_idx));
                ws.restore_moved_paste_state(*paste);
            }
            Err(_) => unreachable!("open_window returns the WindowOpen it was given"),
        }
        true
    }

    /// Process the threshold-crossing motion too: a coalesced drag can release
    /// before another CursorMoved arrives to move the window and latch a dock.
    pub(super) fn follow_torn_drag(&mut self, ws: &mut WindowState) -> bool {
        let Some(td) = self.torn_drag.as_ref() else {
            return false;
        };
        if td.native || td.carrier != ws.seq {
            return false;
        }
        let (grab, torn_seq) = (td.grab, td.seq);
        let Some(source_window) = ws.window.as_ref() else {
            return true;
        };
        let Ok(ip) = source_window.inner_position() else {
            return true;
        };
        let scale = drag_coordinate_scale(source_window);
        let step = crate::detach::manual_drag_step(
            drag_position(source_window, ip),
            (ws.cursor.x / scale, ws.cursor.y / scale),
            grab,
        );
        let torn_win = if torn_seq == ws.seq {
            ws.window.as_ref()
        } else {
            self.windows.get(&torn_seq).and_then(|t| t.window.as_ref())
        };
        let torn_hwnd = torn_win.map(|w| w.as_ref()).and_then(window_hwnd);
        if let Some(tw) = torn_win {
            tw.set_outer_position(drag_native_position(step.frame_origin));
        }
        if let Some(td) = self.torn_drag.as_mut() {
            td.saw_move = true;
            td.last_signal = std::time::Instant::now();
        }
        let extra = if torn_seq == ws.seq { None } else { Some(ws) };
        self.update_dock_target(step.cursor, torn_hwnd, extra);
        true
    }

    /// The client-px rect where a torn window can dock: the tab band,
    /// INCLUDING the would-be band of a hidden single-tab
    /// `auto` bar (the dock preview materializes it on hover). `None` when
    /// `tab-bar = off` (no strip exists to dock onto). Doubles as the tear
    /// threshold's band on the source window, where the bar is always
    /// visible (a tear needs ≥ 2 tabs).
    pub(super) fn dock_band(&self, ws: &WindowState) -> Option<(f32, f32, f32, f32)> {
        if matches!(self.cfg.tab_bar, TabBarMode::Off) {
            return None;
        }
        let height = ws.renderer.as_ref().map(|r| r.cell_h + 8.0).unwrap_or(24.0);
        Some(self.tab_band(ws, height))
    }

    /// This window's tab band at `height`, shared by painting and docking.
    pub(super) fn tab_band(&self, ws: &WindowState, height: f32) -> Rect {
        let surface = ws
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        let bottom_status_height =
            if matches!(self.cfg.status_bar, kettle_config::StatusBarMode::Bottom) {
                self.status_bar_h(ws)
            } else {
                0.0
            };
        tab_band_rect(
            surface,
            self.cfg.tab_bar_pos,
            height,
            self.cfg.tab_bar_width,
            self.search_bar_h(ws),
            bottom_status_height,
        )
    }

    fn tear_off_tab_band(&self, ws: &WindowState, surface: (u32, u32), height: f32) -> Rect {
        let bottom_status = if self.cfg.status_bar == kettle_config::StatusBarMode::Bottom {
            self.status_bar_h(ws)
        } else {
            0.0
        };
        tab_band_rect(
            surface,
            self.cfg.tab_bar_pos,
            height,
            self.cfg.tab_bar_width,
            0.0,
            bottom_status,
        )
    }

    /// Insertion slot if the (approximated) screen
    /// cursor is over this window's tab band; `None` when it isn't (or
    /// the window can't host a dock right now).
    pub(super) fn dock_index_at(&self, w: &WindowState, cursor: (f64, f64)) -> Option<usize> {
        let win = w.window.as_ref()?;
        if win.is_minimized().unwrap_or(false) || !win.is_visible().unwrap_or(true) {
            return None;
        }
        let ip = win.inner_position().ok()?;
        let client = crate::detach::desktop_to_client(
            drag_position(win, ip),
            cursor,
            drag_coordinate_scale(win),
        );
        self.dock_index_in_client(w, client)
    }

    pub(super) fn dock_index_in_client(
        &self,
        w: &WindowState,
        client: (f64, f64),
    ) -> Option<usize> {
        if !client.0.is_finite() || !client.1.is_finite() {
            return None;
        }
        let (cx, cy) = (client.0 as f32, client.1 as f32);
        let band = self.dock_band(w)?;
        if dist_to_rect(cx, cy, band) > 0.0 {
            return None;
        }
        let height = Self::cell_size(w).map_or(24.0, |(_, ch)| ch + 8.0);
        let bar = self.tab_bar_at_height(w, height);
        let vertical = self.cfg.tab_bar_pos.is_vertical();
        dock_insertion_tab(&bar.segments, vertical, if vertical { cy } else { cx })
    }

    /// Which window's tab band is under the screen
    /// cursor, and at which insertion slot. `extra` is the checked-out
    /// source window during manual-follow — it isn't in the map but is a
    /// legal (and common: drag out, change your mind, drop back) target.
    /// On Windows the candidates are resolved against the real z-order;
    /// elsewhere newest-window-first is the best approximation available
    /// (winit exposes no z-order).
    pub(super) fn dock_hit_test(
        &self,
        cursor: (f64, f64),
        torn_seq: u64,
        torn_hwnd: Option<isize>,
        extra: Option<&WindowState>,
    ) -> Option<(u64, usize)> {
        let mut hits: Vec<(Option<isize>, u64, usize)> = Vec::new();
        for (seq, w) in extra
            .map(|w| (w.seq, w))
            .into_iter()
            .chain(self.windows.iter().rev().map(|(s, w)| (*s, w)))
        {
            if seq == torn_seq {
                continue;
            }
            if let Some(idx) = self.dock_index_at(w, cursor) {
                let hwnd = w.window.as_deref().and_then(window_hwnd);
                hits.push((hwnd, seq, idx));
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(th) = torn_hwnd {
            let cands: Vec<(isize, u64, usize)> = hits
                .iter()
                .filter_map(|&(h, s, i)| h.map(|h| (h, s, i)))
                .collect();
            return zorder_pick(th, cursor, &cands);
        }
        #[cfg(not(target_os = "windows"))]
        let _ = torn_hwnd;
        hits.first().map(|&(_, s, i)| (s, i))
    }

    /// Re-run the hit-test for the live cursor and move
    /// the insertion preview (and the latched dock target) accordingly.
    /// `extra` = the checked-out source window in manual-follow, so the
    /// preview can land on it even though it's out of the map.
    pub(super) fn update_dock_target(
        &mut self,
        cursor: (f64, f64),
        torn_hwnd: Option<isize>,
        mut extra: Option<&mut WindowState>,
    ) {
        let Some(td) = self.torn_drag.as_ref() else {
            return;
        };
        let torn_seq = td.seq;
        let prev = td.dock;
        let hit = self.dock_hit_test(cursor, torn_seq, torn_hwnd, extra.as_deref());
        if prev == hit {
            return;
        }
        if let Some((ps, _)) = prev
            && hit.map(|(s, _)| s) != Some(ps)
        {
            match extra.as_deref_mut() {
                Some(x) if x.seq == ps => self.apply_dock_preview_ws(x, None),
                _ => self.apply_dock_preview(ps, None),
            }
        }
        if let Some((ns, idx)) = hit {
            match extra {
                Some(x) if x.seq == ns => self.apply_dock_preview_ws(x, Some(idx)),
                _ => self.apply_dock_preview(ns, Some(idx)),
            }
        }
        // Dock-hover translucency (Windows): the full-size torn window is
        // covering the very strip the insertion marker draws in — go
        // see-through while a target is latched so the user can watch the
        // marker under the pointer; restore on unlatch.
        #[cfg(target_os = "windows")]
        if let Some(th) = self.torn_drag.as_ref().and_then(|t| t.hwnd) {
            match (prev.is_some(), hit.is_some()) {
                (false, true) => set_window_alpha(th, DOCK_HOVER_ALPHA),
                (true, false) => set_window_alpha(th, 255),
                _ => {}
            }
        }
        if let Some(td) = self.torn_drag.as_mut() {
            td.dock = hit;
        }
    }

    /// Does the latched dock target still hold at the
    /// moment a heuristic pointer-event commit fires? A real drop ended
    /// with the button UP; a WM-cancelled move (X11 Esc) ends its grab
    /// with the button still physically HELD — the same tell the Windows
    /// release path uses (`primary_button_physically_held`), read here
    /// from the X11 `QueryPointer` button mask. Position heuristics can't
    /// make this call: Esc doesn't move the pointer (a live-cursor
    /// hit-test still sees the band) and the WM's restore `Moved` re-syncs
    /// any frame-anchor estimate before the commit event arrives, so only
    /// the button state separates the two. `ws` = the torn window
    /// (checked out of the map).
    pub(super) fn revalidate_dock_latch(&self, ws: &WindowState) -> bool {
        let Some(td) = self.torn_drag.as_ref() else {
            return false;
        };
        if td.caption
            && ws
                .native_caption_drag
                .as_ref()
                .is_some_and(|drag| drag.cancelled())
        {
            return false;
        }
        let Some((latched, _)) = td.dock else {
            // Nothing latched — commit and abandon are equivalent.
            return false;
        };
        // Esc-cancel: the WM ended its grab while the user still holds
        // the button. Abandon — the user's release hasn't happened, so
        // nothing was dropped.
        if primary_button_physically_held() {
            return false;
        }
        // Real drop: re-run the hit-test from the best cursor estimate —
        // live when available, else the legacy frame+grab approximation
        // (macOS manual-follow keeps its original self-consistent bias).
        let cursor = cursor_screen_pos().or_else(|| {
            ws.window.as_ref().and_then(|w| {
                w.outer_position().ok().map(|op| {
                    let origin = drag_position(w, op);
                    (origin.0 + td.grab.0, origin.1 + td.grab.1)
                })
            })
        });
        let Some(cursor) = cursor else {
            return true; // can't tell — trust the latch
        };
        self.dock_hit_test(cursor, td.seq, td.hwnd, None)
            .is_some_and(|(seq, _)| seq == latched)
    }

    /// The frozen-drag rescue tick, run from `about_to_wait` while torn-drag
    /// tracking is live. It covers two holes in event-driven tracking on
    /// GNOME/Mutter:
    /// - A native handoff the WM accepted but never (or no longer) acts on.
    ///   `Moved` events stop while the REAL pointer keeps travelling (e.g.
    ///   `_NET_WM_MOVERESIZE` racing a just-created, not-yet-mapped window, or
    ///   a WM that dropped the grab after a single incidental placement
    ///   `Moved`). That travel without `Moved` demotes the drag to
    ///   manual-follow.
    /// - Manual-follow starving because the pointer left the capture-holding
    ///   source window's bounds (its CursorMoved stream stops at the border).
    ///   The tick carries the torn window straight from the live pointer.
    ///
    /// Healthy native tracking (fresh `Moved` signals) is left strictly alone.
    /// Returns the wait budget (ms) that keeps the tick alive, or `None` when
    /// there is no drag or no live-cursor source (Wayland).
    pub(super) fn torn_drag_pointer_tick(&mut self, event_loop: &ActiveEventLoop) -> Option<u64> {
        let td = self.torn_drag.as_ref()?;
        if td.caption {
            let seq = td.seq;
            let cancelled = self
                .windows
                .get(&seq)
                .and_then(|ws| ws.native_caption_drag.as_ref())
                .is_some_and(|drag| drag.cancelled());
            if cancelled || !primary_button_physically_held() {
                if !cancelled && let Some(cursor) = cursor_screen_pos() {
                    self.update_dock_target(cursor, None, None);
                }
                if let Some(mut ws) = self.windows.remove(&seq) {
                    let commit = !cancelled && self.revalidate_dock_latch(&ws);
                    self.finalize_torn_drag(&mut ws, commit);
                    self.finish_window_dispatch(event_loop, seq, ws);
                } else {
                    self.abandon_torn_drag(None);
                }
                return None;
            }
            if let Some(cursor) = cursor_screen_pos() {
                self.update_dock_target(cursor, None, None);
            }
            return Some(TORN_TICK_MS);
        }
        let cursor = cursor_screen_pos()?;
        if !primary_button_physically_held() {
            let (seq, hwnd) = (td.seq, td.hwnd);
            self.update_dock_target(cursor, hwnd, None);
            if let Some(mut ws) = self.windows.remove(&seq) {
                let commit = self.revalidate_dock_latch(&ws);
                self.finalize_torn_drag(&mut ws, commit);
                self.finish_window_dispatch(event_loop, seq, ws);
            } else {
                self.abandon_torn_drag(None);
            }
            return None;
        }
        if td.native {
            // Demotion evidence: the pointer travelled while the WM sent
            // no `Moved`. `signal_cursor` is the pointer at the last
            // tracking signal (or this tick's first look); `saw_move`
            // alone is NOT proof of health — a single incidental
            // placement `Moved` must not disable the rescue forever.
            let Some(reference) = td.signal_cursor else {
                if let Some(td) = self.torn_drag.as_mut() {
                    td.signal_cursor = Some(cursor);
                }
                return Some(TORN_TICK_MS);
            };
            let travelled =
                ((cursor.0 - reference.0).powi(2) + (cursor.1 - reference.1).powi(2)).sqrt();
            let waited = td.last_signal.elapsed() >= std::time::Duration::from_millis(400)
                && td.started.elapsed() >= std::time::Duration::from_millis(400);
            if !waited || travelled < DEMOTION_TRAVEL_PX {
                return Some(TORN_TICK_MS);
            }
            log::info!(
                "tear-off: pointer moved {travelled:.0}px with no WM move; demoting to manual follow"
            );
            if let Some(td) = self.torn_drag.as_mut() {
                td.native = false;
            }
        }
        let td = self.torn_drag.as_ref()?;
        // A CursorMoved-driven follow ran a moment ago — let the higher-
        // rate event stream win while it flows; the tick fills the gaps.
        if td.saw_move && td.last_signal.elapsed() < std::time::Duration::from_millis(30) {
            return Some(TORN_TICK_MS);
        }
        let (grab, torn_seq) = (td.grab, td.seq);
        let torn_win = self.windows.get(&torn_seq).and_then(|t| t.window.clone())?;
        torn_win.set_outer_position(drag_native_position((cursor.0 - grab.0, cursor.1 - grab.1)));
        let torn_hwnd = window_hwnd(&torn_win);
        if let Some(td) = self.torn_drag.as_mut() {
            td.saw_move = true;
            td.last_signal = std::time::Instant::now();
        }
        self.update_dock_target(cursor, torn_hwnd, None);
        Some(TORN_TICK_MS)
    }

    /// Set/clear a mapped window's dock preview (the
    /// checked-out variant below does the actual work).
    pub(super) fn apply_dock_preview(&mut self, seq: u64, idx: Option<usize>) {
        if let Some(mut w) = self.windows.remove(&seq) {
            self.apply_dock_preview_ws(&mut w, idx);
            self.windows.insert(seq, w);
        }
    }

    /// Set/clear a window's dock preview. The preview
    /// that MATERIALIZES a hidden single-tab auto bar is render-only — the
    /// strip overlays the top of the content for the hover's duration and
    /// the PTY grids are NOT resized (hovering across the band
    /// edge would otherwise SIGWINCH-spam the target's shells with
    /// resize/restore pairs; the real resize happens once, in
    /// `dock_tab_into`, when a drop actually merges).
    pub(super) fn apply_dock_preview_ws(&mut self, w: &mut WindowState, idx: Option<usize>) {
        if w.dock_preview == idx {
            return;
        }
        log::debug!("dock preview: window {} -> {idx:?}", w.seq);
        w.dock_preview = idx;
        if let Some(win) = &w.window {
            win.request_redraw();
        }
    }

    /// Drop torn-drag tracking without merging (stale
    /// tracking, cancel, or focus loss) — clears any latched insertion
    /// preview. `ws` = the checked-out window, in case the preview is on it.
    pub(super) fn abandon_torn_drag(&mut self, ws: Option<&mut WindowState>) {
        if let Some(td) = self.torn_drag.take()
            && let Some((seq, _)) = td.dock
        {
            #[cfg(target_os = "windows")]
            if let Some(th) = td.hwnd {
                set_window_alpha(th, 255);
            }
            match ws {
                Some(x) if x.seq == seq => self.apply_dock_preview_ws(x, None),
                _ => self.apply_dock_preview(seq, None),
            }
        }
    }

    /// The drop. Commit the latched dock target —
    /// move the torn window's tab into the target at the insertion slot and
    /// close the emptied torn window — or just clear tracking + preview.
    /// `ws` is whatever window's dispatch observed the drop: the torn
    /// window itself for the native OS loop (winit synthesizes its
    /// left-release when the Windows modal move loop exits), or the
    /// capture-holding source for manual-follow.
    pub(super) fn finalize_torn_drag(&mut self, ws: &mut WindowState, commit: bool) {
        let committable = self
            .torn_drag
            .as_ref()
            .is_some_and(|td| commit && td.saw_move && td.dock.is_some());
        if !committable {
            self.abandon_torn_drag(Some(ws));
            return;
        }
        let td = self.torn_drag.take().expect("checked committable above");
        let (target_seq, idx) = td.dock.expect("checked committable above");
        // The torn window is about to close (merge) — restoring opacity is
        // moot then, but the defensive multi-tab branch below keeps it
        // alive, and a no-op restore on a dying HWND is harmless.
        #[cfg(target_os = "windows")]
        if let Some(th) = td.hwnd {
            set_window_alpha(th, 255);
        }
        if td.seq == ws.seq {
            // Native loop: `ws` IS the torn window; the target is mapped.
            let donor_active = ws.mux.active;
            self.release_held_buttons_in_tab(ws, donor_active);
            let Some(dt) = ws.mux.detach_tab(donor_active) else {
                // Every early return after take() must clear the
                // latched preview, or the marker (and a materialized auto
                // bar) sticks to the target forever.
                self.clear_dock_preview_at(ws, target_seq);
                return;
            };
            let Some(mut target) = self.windows.remove(&target_seq) else {
                // Target vanished between latch and drop — keep the tab.
                ws.mux.attach_tab(dt, Some(donor_active));
                return;
            };
            self.release_focus_report_to(ws, &dt);
            let paste = ws.take_moved_paste_state(&dt.tab, std::time::Instant::now());
            self.dock_tab_into(&mut target, dt, paste, idx, td.seq);
            // The docked tab becomes the target's active tab; report it now,
            // since an already focused target gets no new focus event.
            self.sync_pane_focus_reports(&mut target);
            self.windows.insert(target_seq, target);
            self.focused_seq = target_seq;
            if ws.mux.tabs.is_empty() {
                // The emptied torn window closes through the normal funnel
                // (finish_window_dispatch drops it; other windows remain,
                // so no event-loop exit). Session-save pairing: every other
                // window-close path saves right before flagging the close.
                self.save_session(ws);
                self.request_window_close(ws.seq);
            }
        } else {
            // Manual-follow: `ws` is the SOURCE (capture holder); the torn
            // window is mapped. The target may be `ws` itself or another
            // mapped window (the hit-test excludes the torn window).
            let Some(mut torn) = self.windows.remove(&td.seq) else {
                // Torn window died mid-drag (reap) — the latch outlived it.
                self.clear_dock_preview_at(ws, target_seq);
                return;
            };
            let donor_active = torn.mux.active;
            self.release_held_buttons_in_tab(&mut torn, donor_active);
            let Some(dt) = torn.mux.detach_tab(donor_active) else {
                self.windows.insert(td.seq, torn);
                self.clear_dock_preview_at(ws, target_seq);
                return;
            };
            if target_seq == ws.seq {
                self.release_focus_report_to(&mut torn, &dt);
                let paste = torn.take_moved_paste_state(&dt.tab, std::time::Instant::now());
                self.dock_tab_into(ws, dt, paste, idx, td.seq);
            } else if let Some(mut target) = self.windows.remove(&target_seq) {
                self.release_focus_report_to(&mut torn, &dt);
                let paste = torn.take_moved_paste_state(&dt.tab, std::time::Instant::now());
                self.dock_tab_into(&mut target, dt, paste, idx, td.seq);
                self.sync_pane_focus_reports(&mut target);
                self.windows.insert(target_seq, target);
            } else {
                torn.mux.attach_tab(dt, Some(donor_active));
                self.windows.insert(td.seq, torn);
                return;
            }
            self.focused_seq = target_seq;
            if torn.mux.tabs.is_empty() {
                // Dropping outside finish_window_dispatch is safe here: the
                // map keeps the target (and source), so the "exit only when
                // the map is empty" invariant can't trip. Save first so the
                // session reflects the post-merge window set (the close-path
                // pairing every other close keeps).
                self.save_session(ws);
                self.close_update_toast_window(torn.seq);
                drop(torn);
            } else {
                self.windows.insert(td.seq, torn);
            }
        }
    }

    /// Clear a latched dock preview wherever its window lives (the
    /// checked-out `ws` or the map).
    pub(super) fn clear_dock_preview_at(&mut self, ws: &mut WindowState, target_seq: u64) {
        if ws.seq == target_seq {
            self.apply_dock_preview_ws(ws, None);
        } else {
            self.apply_dock_preview(target_seq, None);
        }
    }

    pub(super) fn open_tab_window_chooser(&mut self, ws: &mut WindowState) {
        if !self.cfg.detachable_tabs {
            return;
        }
        let panes: Arc<[u64]> = ws.mux.tab_anchor_panes(ws.mux.active).into();
        if panes.is_empty() {
            return;
        }
        let mut destinations: Vec<_> = self
            .windows
            .iter()
            .filter(|(seq, window)| {
                **seq != ws.seq && window.window.is_some() && !self.window_close_pending(**seq)
            })
            .map(|(seq, window)| (*seq, window.last_title.clone()))
            .collect();
        destinations.sort_unstable_by_key(|(seq, _)| *seq);
        let mut items = vec![ContextMenuItem::Info {
            label: self
                .ui_text
                .text(kettle_i18n::Text::PaletteMoveTabToWindow)
                .to_owned(),
        }];
        for (window, title) in destinations {
            let title: String = sanitize_title(&title).chars().take(200).collect();
            items.push(ContextMenuItem::WindowChoice {
                label: format!("{window}: {title}"),
                window,
                panes: Arc::clone(&panes),
            });
        }
        if items.len() == 1 {
            items.push(ContextMenuItem::Info {
                label: self
                    .ui_text
                    .text(kettle_i18n::Text::WindowTransferNoTargets)
                    .to_owned(),
            });
        }
        self.close_all_modals(ws);
        self.show_context_menu(ws, items, 0.0, 0.0);
        if let Some((_, panel)) = self.context_menu_geometry(ws) {
            let surface = ws
                .renderer
                .as_ref()
                .map(|renderer| {
                    let size = renderer.surface_size();
                    (size.0 as f32, size.1 as f32)
                })
                .unwrap_or((800.0, 600.0));
            if let Some(menu) = ws.context_menu.as_mut() {
                menu.anchor = clamp_context_menu_anchor(
                    ((surface.0 - panel.0) * 0.5, (surface.1 - panel.1) * 0.5),
                    panel,
                    surface,
                );
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn poll_wayland(&mut self, event_loop: &ActiveEventLoop) {
        let (files, update) = if let Some(bridge) = self.wayland.as_mut() {
            let live: std::collections::BTreeMap<_, _> = self
                .windows
                .iter()
                .filter(|(seq, _)| {
                    !self.quit_requested && !self.pending_window_closes.contains(seq)
                })
                .filter_map(|(&seq, window)| Some((seq, window.window.clone()?)))
                .collect();
            bridge.poll(&live);
            (bridge.file_updates(), bridge.drag_update())
        } else {
            (Vec::new(), None)
        };
        for file in files {
            self.deliver_wayland_files(event_loop, file);
        }
        let Some(update) = update else { return };
        let Some(drag) = self.wayland_tab_drag.as_ref() else {
            return;
        };
        let carrier = drag.carrier;
        let (point, ended, activation) = match update {
            crate::wayland::Update::Hover(point) => (point, None, None),
            crate::wayland::Update::End {
                performed,
                drop,
                activation,
            } => (drop, Some(performed), activation),
        };
        let target = point.and_then(|point| self.wayland_dock_target(point, carrier));
        let previous = self.wayland_tab_drag.as_mut().unwrap().preview.take();
        if let Some(previous) = previous
            && target.map(|(seq, _)| seq) != Some(previous)
        {
            self.apply_dock_preview(previous, None);
        }
        if ended.is_none() {
            if let Some((seq, insertion)) = target {
                self.apply_dock_preview(seq, Some(insertion));
                self.wayland_tab_drag.as_mut().unwrap().preview = Some(seq);
            }
            return;
        }
        if let Some(previous) = previous {
            self.apply_dock_preview(previous, None);
        }
        let drag = self.wayland_tab_drag.take().unwrap();
        let destination = wayland_drag_destination(
            ended == Some(true),
            drag.source,
            drag.carrier,
            drag.source_index,
            target,
        );
        if let Some((seq, insertion)) = destination
            && let Some(mut carrier) = self.windows.remove(&drag.carrier)
        {
            if self.transfer_tab_to_window(&mut carrier, seq, &drag.panes, Some(insertion))
                && let (Some(bridge), Some(permit), Some(target)) = (
                    self.wayland.as_mut(),
                    activation,
                    self.windows
                        .get(&seq)
                        .and_then(|window| window.window.clone()),
                )
            {
                bridge.activate_drag(permit, seq, target);
            }
            self.finish_window_dispatch(event_loop, carrier.seq, carrier);
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn deliver_wayland_files(
        &mut self,
        event_loop: &ActiveEventLoop,
        update: crate::wayland::FileUpdate,
    ) {
        match update {
            crate::wayland::FileUpdate::Start { id, window } => {
                if !self.quit_requested
                    && !self.window_close_pending(window)
                    && let Some(ws) = self.windows.get(&window)
                    && !self.any_modal_open(ws)
                    && ws.mux.active_focus().is_some()
                {
                    self.wayland_file_drops
                        .insert(id, (window, WaylandDropTarget::capture(ws)));
                }
                log::debug!(
                    "Wayland file drop context captured: {}",
                    self.wayland_file_drops.contains_key(&id)
                );
            }
            crate::wayland::FileUpdate::Ready { id, paths } => {
                let context = self.wayland_file_drops.remove(&id);
                let valid = context.as_ref().is_some_and(|(seq, target)| {
                    !self.quit_requested
                        && !self.window_close_pending(*seq)
                        && self.windows.get(seq).is_some_and(|ws| {
                            !self.any_modal_open(ws) && *target == WaylandDropTarget::capture(ws)
                        })
                });
                log::debug!(
                    "Wayland file drop ready: parsed={}, context_current={valid}",
                    paths.is_ok()
                );
                let accepted = valid && paths.is_ok();
                if let Some(bridge) = self.wayland.as_mut() {
                    bridge.finish_files(id, accepted);
                }
                if !valid {
                    return;
                }
                let (seq, _) = context.unwrap();
                let Ok(paths) = paths else {
                    fire_notify(
                        self.ui_text
                            .text(kettle_i18n::Text::NotifyTitleInputNotDelivered),
                        self.ui_text
                            .text(kettle_i18n::Text::NotifyBodyDropUnreadable),
                    );
                    return;
                };
                if let Some(mut ws) = self.windows.remove(&seq) {
                    self.exit_preview_focus(&mut ws);
                    let video = self.cfg.paste_video_preview.then(|| {
                        crate::video_preview::VideoPasteRequest::from_user_paths(
                            &paths,
                            crate::video_preview::VideoPasteSource::Drop,
                        )
                    });
                    self.paste_paths(&mut ws, paths, true, video.flatten());
                    if let Some(window) = &ws.window {
                        window.request_redraw();
                    }
                    self.finish_window_dispatch(event_loop, seq, ws);
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn wayland_dock_target(
        &self,
        point: crate::wayland::Point,
        carrier: u64,
    ) -> Option<(u64, usize)> {
        if point.window == carrier
            || self.window_close_pending(point.window)
            || self.quit_requested
            || !self.cfg.detachable_tabs
        {
            return None;
        }
        let target = self.windows.get(&point.window)?;
        if self.any_modal_open(target) {
            return None;
        }
        let scale = target.window.as_ref()?.scale_factor();
        self.dock_index_in_client(target, (point.x * scale, point.y * scale))
            .map(|index| (point.window, index))
    }

    #[cfg(target_os = "linux")]
    pub(super) fn begin_wayland_tab_drag(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        window: Arc<winit::window::Window>,
    ) -> bool {
        if self.wayland_tab_drag.is_some()
            || self.window_close_pending(ws.seq)
            || self.quit_requested
            || self.any_modal_open(ws)
        {
            return false;
        }
        let index = ws.mux.active;
        let panes = ws.mux.tab_anchor_panes(index);
        if panes.is_empty() {
            return false;
        }
        let lone = ws.mux.tabs.len() == 1;
        let press = ws
            .drag_press
            .map(|(x, y)| (f64::from(x), f64::from(y)))
            .unwrap_or((ws.cursor.x, ws.cursor.y));
        let bar = self.tab_bar(ws);
        let grab = if lone {
            press
        } else {
            let segment = bar
                .segments
                .iter()
                .find(|segment| segment.active)
                .map(|segment| segment.rect);
            let (x, y, width, height) = segment.unwrap_or((0.0, 0.0, 80.0, 24.0));
            let along_x = (press.0 - f64::from(x)).clamp(8.0, f64::from(width.max(16.0)) - 8.0);
            let along_y = (press.1 - f64::from(y)).clamp(4.0, f64::from(height.max(8.0)) - 4.0);
            let size = window.inner_size();
            let destination_band = self.tear_off_tab_band(ws, (size.width, size.height), height);
            match self.cfg.tab_bar_pos {
                TabBarPos::Top => (along_x, along_y),
                TabBarPos::Bottom => (along_x, f64::from(destination_band.1) + along_y),
                TabBarPos::Left => (along_x, along_y),
                TabBarPos::Right => (
                    f64::from(window.inner_size().width) - f64::from(width) + along_x,
                    along_y,
                ),
            }
        };
        if !self.wayland.as_mut().is_some_and(|bridge| {
            bridge.begin_drag(
                ws.seq,
                window.clone(),
                (!lone).then_some(self.next_window_seq),
                grab,
            )
        }) {
            return false;
        }
        self.wayland_tab_drag = Some(WaylandTabDrag {
            source: ws.seq,
            source_index: index,
            panes,
            carrier: ws.seq,
            preview: None,
        });
        ws.tab_drag_active = false;
        ws.tab_drag_press = None;
        ws.tab_pressed_idx = None;
        ws.detach_drag = crate::detach::DragState::default();
        ws.drag_press = None;
        if lone {
            return true;
        }
        self.release_held_buttons_in_tab(ws, index);
        let Some(dt) = ws.mux.detach_tab(index) else {
            if let Some(bridge) = self.wayland.as_mut() {
                bridge.abandon_drag_carrier();
            }
            return true;
        };
        self.release_focus_report_to(ws, &dt);
        let paste = ws.take_moved_paste_state(&dt.tab, std::time::Instant::now());
        let color = self.tear_off_source_color(ws);
        let font_size = ws.renderer.as_ref().map(|r| r.font_size());
        match self
            .open_window(
                event_loop,
                WindowOpen::AdoptTab {
                    tab: dt,
                    source_color: color,
                    font_size,
                    scaled_zoom_prev_font_size: ws.scaled_zoom_prev_font_size,
                    paste: Box::new(paste),
                },
                None,
                Some(window.inner_size()),
            )
            .map_err(|open| *open)
        {
            Ok(seq) => {
                self.wayland_tab_drag.as_mut().unwrap().carrier = seq;
                self.fire_tab_close_event(ws, index);
                self.ctl_broadcast(
                    "tab_moved",
                    None,
                    serde_json::json!({"from_window":ws.seq,"to_window":seq,"tab":index}),
                );
                self.resize_all(ws);
            }
            Err(WindowOpen::AdoptTab { tab: dt, paste, .. }) => {
                if let Some(bridge) = self.wayland.as_mut() {
                    bridge.abandon_drag_carrier();
                }
                ws.mux.attach_tab(dt, Some(index));
                ws.restore_moved_paste_state(*paste);
                self.resize_all(ws);
            }
            Err(_) => unreachable!("only an adopted tab was opened"),
        }
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        true
    }

    pub(super) fn move_tab_to_window(
        &mut self,
        source: &mut WindowState,
        window: u64,
        panes: &[u64],
    ) {
        if self.cfg.detachable_tabs && self.transfer_tab_to_window(source, window, panes, None) {
            #[cfg(target_os = "linux")]
            if let (Some(bridge), Some(origin), Some(target)) = (
                self.wayland.as_mut(),
                source.window.clone(),
                self.windows
                    .get(&window)
                    .and_then(|target| target.window.clone()),
            ) {
                bridge.activate(source.seq, origin, window, target);
            }
        }
    }

    pub(super) fn transfer_tab_to_window(
        &mut self,
        source: &mut WindowState,
        window: u64,
        panes: &[u64],
        insertion: Option<usize>,
    ) -> bool {
        if self.quit_requested
            || source.seq == window
            || self.window_close_pending(source.seq)
            || self.window_close_pending(window)
        {
            return false;
        }
        let Some(index) = source.mux.tab_index_of_any_pane(panes) else {
            return false;
        };
        let Some(mut target) = self.windows.remove(&window) else {
            return false;
        };
        if target.window.is_none() {
            self.windows.insert(window, target);
            return false;
        }
        self.release_held_buttons_in_tab(source, index);
        let Some(dt) = source.mux.detach_tab(index) else {
            self.windows.insert(window, target);
            return false;
        };
        self.release_focus_report_to(source, &dt);
        let paste = source.take_moved_paste_state(&dt.tab, std::time::Instant::now());
        let insertion = insertion
            .unwrap_or(target.mux.tabs.len())
            .min(target.mux.tabs.len());
        self.dock_tab_into(&mut target, dt, paste, insertion, source.seq);
        self.sync_pane_focus_reports(&mut target);
        self.windows.insert(window, target);
        if source.mux.tabs.is_empty() {
            self.save_session(source);
            self.request_window_close(source.seq);
        } else {
            self.resize_all(source);
            if let Some(window) = &source.window {
                window.request_redraw();
            }
        }
        true
    }

    /// Receive a docked tab: attach at the insertion
    /// slot (attach_tab makes it active + seen), clear the preview, resize,
    /// take focus, notify agents.
    pub(super) fn dock_tab_into(
        &mut self,
        target: &mut WindowState,
        dt: crate::mux::DetachedTab,
        paste: crate::window_state::MovedPasteState,
        idx: usize,
        from_seq: u64,
    ) {
        let pre_focus = self.focus_key(target);
        let landed = target.mux.attach_tab(dt, Some(idx));
        target.adopt_moved_paste_state(paste);
        self.note_focus_change(target, pre_focus);
        target.dock_preview = None;
        self.resize_all(target);
        if let Some(w) = &target.window {
            w.set_visible(true);
            w.set_minimized(false);
            w.focus_window();
            w.request_redraw();
        }
        self.ctl_broadcast(
            "tab_moved",
            None,
            serde_json::json!({
                "from_window": from_seq,
                "to_window": target.seq,
                "tab": landed,
            }),
        );
    }

    /// Peacock: resolve + claim this window's accent — walk the theme's
    /// pool from the cwd-seed slot, skipping hues live windows already use:
    /// in-process siblings (authoritative) plus other kettle processes via
    /// the presence registry (best-effort; see kettle-ctl/src/presence.rs).
    pub(super) fn assign_window_accent(
        &self,
        ws: &mut WindowState,
        avoid: Option<kettle_config::Rgb>,
    ) {
        let pool = kettle_config::peacock_pool(&self.cfg.theme);
        if pool.is_empty() {
            return;
        }
        let mut in_use = Vec::new();
        let dir = kettle_ctl::presence::presence_dir();
        let me = std::process::id();
        for e in kettle_ctl::presence::live_entries(&dir) {
            // Own-process claims are authoritative in the local registry,
            // including windows checked out during event dispatch.
            if e.pid == me {
                continue;
            }
            if let Some(c) = kettle_config::Rgb::parse(&e.rgb) {
                in_use.push(c);
            }
        }
        let Some((slot, local_claim)) =
            self.accent_registry
                .claim(&pool, self.cfg.accent_seed, &in_use, avoid)
        else {
            return;
        };
        let color = pool[slot];
        let presence = crate::window_accent::AccentPresence::claim(
            &dir,
            kettle_ctl::presence::PresenceEntry::claiming(
                me,
                ws.seq,
                crate::window_accent::rgb_hex(color),
                true,
            ),
        );
        ws.accent = Some(crate::window_state::WindowAccent {
            local_claim,
            color,
            slot,
            theme_name: self.cfg.theme_name.clone(),
            pool_candidates: kettle_config::peacock_candidates(&self.cfg.theme),
            presence,
        });
        crate::window_accent::apply_accent(ws);
    }

    /// The source window's accent for a tab leaving it, after applying any
    /// theme or palette change made since its last redraw.
    pub(super) fn tear_off_source_color(&self, ws: &mut WindowState) -> Option<kettle_config::Rgb> {
        self.sync_window_accent(ws);
        ws.accent.as_ref().map(|accent| accent.color)
    }

    /// Peacock: keep this window's accent in sync, called once per frame
    /// from `redraw` — cheap steady state (an Option check + string compare).
    /// Covers: the first frame (claim), a theme switch (re-resolve the SAME
    /// pool slot against the new theme, so every window shifts consistently),
    /// and reconfiguration to a pinned hex / `accent-color = theme` (drops
    /// the claim and clears the renderer override).
    pub(super) fn sync_window_accent(&self, ws: &mut WindowState) {
        crate::window_accent::refresh_accent(ws, &self.cfg);
        if ws.accent.is_none() && self.cfg.accent_auto && self.cfg.accent_color.is_none() {
            self.assign_window_accent(ws, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_performed_then_cancelled_wayland_drop_restores_the_source_tab() {
        let mut outcome = crate::wayland::DragOutcome::default();
        outcome.release();
        assert!(outcome.succeeded(), "the drop was initially performed");
        outcome.cancel();
        assert_eq!(
            wayland_drag_destination(outcome.succeeded(), 7, 8, 2, None),
            Some((7, 2)),
            "cancelled drop left the tab in its detached carrier",
        );
        assert_eq!(
            wayland_drag_destination(outcome.succeeded(), 7, 8, 2, Some((9, 0))),
            Some((7, 2)),
            "cancellation overrides the last hovered target",
        );
        assert!(
            !outcome.succeeded(),
            "cancelled drop must not authorize activation"
        );
    }
}

#[cfg(any(test, target_os = "macos"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MacosTitlebarPolicy {
    /// Keep AppKit's geometry and controls on an opaque native surface whose
    /// light or dark appearance follows Kettle's theme hint.
    ThemeMatchedNative,
    /// Borderless windows have no native titlebar to configure.
    NotApplicable,
}

#[cfg(any(test, target_os = "macos"))]
pub(super) fn macos_titlebar_policy(borderless: bool) -> MacosTitlebarPolicy {
    if borderless {
        MacosTitlebarPolicy::NotApplicable
    } else {
        MacosTitlebarPolicy::ThemeMatchedNative
    }
}

#[cfg(target_os = "macos")]
pub(super) fn apply_macos_window_chrome(
    attrs: winit::window::WindowAttributes,
    borderless: bool,
) -> winit::window::WindowAttributes {
    use winit::platform::macos::WindowAttributesExtMacOS as _;

    match macos_titlebar_policy(borderless) {
        // Create the caption opaque from the first frame. The material effect
        // stays inside the content rect below it.
        MacosTitlebarPolicy::ThemeMatchedNative => attrs.with_titlebar_transparent(false),
        MacosTitlebarPolicy::NotApplicable => attrs.with_decorations(false),
    }
}

/// Decode kettle's embedded 256px PNG (the dark or light variant) into the
/// *running* window's icon: the title-bar system-menu glyph (top-left), the
/// taskbar button, and the Alt-Tab thumbnail. winit leaves this icon unset by
/// default. `build.rs` embeds the same art as an `.exe` resource, but that
/// covers only Explorer, the file glyph and pinned shortcuts, not the live
/// window's `WM_SETICON`. Without this, Windows shows a generic placeholder.
/// The OS downscales the source for the small title-bar icon and picks the
/// right size for the taskbar and switcher. Best-effort: a decode failure
/// leaves the icon unset rather than aborting startup. No-op on Wayland (uses
/// the `.desktop` app_id); effective on Windows and X11.
///
/// macOS returns `None` without decoding. An AppKit window has no icon of its
/// own (the Dock and the app switcher use the bundle's), and winit's macOS
/// backend drops whatever it is given, at creation and in `set_window_icon`,
/// so the decode would only delay the first window.
pub(super) fn load_window_icon(dark: bool) -> Option<winit::window::Icon> {
    #[cfg(target_os = "macos")]
    {
        let _ = dark;
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        const DARK_ICON_PNG: &[u8] = include_bytes!("../../../../packaging/linux/kettle-256.png");
        const LIGHT_ICON_PNG: &[u8] =
            include_bytes!("../../../../packaging/linux/kettle-light-256.png");
        let icon_png = if dark { DARK_ICON_PNG } else { LIGHT_ICON_PNG };
        let img = image::load_from_memory(icon_png).ok()?.into_rgba8();
        let (w, h) = img.dimensions();
        winit::window::Icon::from_rgba(img.into_raw(), w, h).ok()
    }
}

/// macOS `sticky = true` — make the window appear on every Space
/// (Mission Control workspace). winit 0.30 dropped the native method, so we
/// reach the underlying `NSWindow` through the raw AppKit handle and set its
/// collection behavior — exactly what `set_visible_on_all_workspaces` did. The
/// objc2 / objc2-app-kit versions are pinned to winit's own (0.5 / 0.2) so the
/// ObjC class layout matches. Best-effort: a missing handle just leaves the
/// window on its current Space.
#[cfg(target_os = "macos")]
fn set_visible_on_all_spaces(window: &winit::window::Window) {
    use objc2_app_kit::{NSView, NSWindowCollectionBehavior};
    // winit re-exports raw-window-handle, so we don't need it as a direct dep.
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    // SAFETY: on macOS winit hands us a live `NSView*`, and `resumed` runs on
    // the main thread where AppKit requires UI mutations. We only borrow it.
    let view: &NSView = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
    if let Some(ns_window) = view.window() {
        // SAFETY: `setCollectionBehavior:` is an objc2 `unsafe fn` (any ObjC
        // message send is). The behavior bits are valid and the window is live.
        unsafe {
            ns_window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary,
            );
        }
    }
}

/// Turn off AppKit's persistent UI ("Resume") before the application
/// launches. Kettle restores its own session, and AppKit cannot rebuild a
/// winit window anyway. Left on, a bundled Kettle kept encoding its window
/// state for the persistent-UI service while idle: 0.04 % CPU and 0.2 extra
/// wakeups a second. `ApplePersistence` goes in the registration domain,
/// which lives only in memory and loses to anything the user sets.
#[cfg(target_os = "macos")]
pub(super) fn disable_app_state_restoration() {
    use objc2::rc::Retained;
    use objc2_foundation::{NSDictionary, NSNumber, NSString, NSUserDefaults};

    let key = NSString::from_str("ApplePersistence");
    let off = Retained::into_super(Retained::into_super(Retained::into_super(
        NSNumber::numberWithBool(false),
    )));
    let defaults = NSDictionary::from_vec(&[&*key], vec![off]);
    // SAFETY: `registerDefaults:` copies a property-list dictionary of
    // NSString keys, and this runs on the main thread before AppKit starts.
    unsafe { NSUserDefaults::standardUserDefaults().registerDefaults(&defaults) };
}

pub(super) fn rounded_window_corner_policy(
    is_macos: bool,
    live_decorated: bool,
    live_fullscreen: bool,
) -> bool {
    is_macos && live_decorated && !live_fullscreen
}

/// The hint winit gets at window creation, which on macOS is not the hint the
/// window ends up with.
///
/// macOS gets `None` here and picks its appearance up later, from
/// [`native_theme_sync_is_due`]. winit applies a creation-time hint as an
/// `NSWindow` appearance override before AppKit has built the titlebar's
/// container view, and the native material view kettle adds to the frame view
/// right afterwards then locks in the container-less layout: AppKit centres
/// the title over the 72pt traffic-light cluster instead of the caption, so
/// `~ — kettle` is drawn across the red and yellow buttons and clipped at the
/// window edge (Reddimus/kettle#251).
///
/// Every other platform still seeds the caption at creation, where it costs
/// nothing and avoids a first-frame flash.
///
/// Pure — unit-testable without a window.
pub(super) fn creation_theme_hint(
    hint: Option<WindowTheme>,
    is_macos: bool,
) -> Option<WindowTheme> {
    if is_macos { None } else { hint }
}

/// Whether this window may be handed the native theme hint yet.
///
/// Only the *first* appearance a macOS window is given has to wait for the
/// window to be on screen and key, which is when AppKit builds its titlebar
/// container. That one can leave the window permanently without a caption —
/// removing the material view again does not bring it back — so a window that
/// has never been focused keeps the system appearance rather than a broken
/// caption. See [`creation_theme_hint`].
///
/// Every later change is unconditional. The caption exists by then, and this
/// is the path the runtime light/dark toggle always took. Holding those back
/// too would strand a background window on a stale appearance until someone
/// clicked it, since a schedule boundary, Lua, or a config reload can move the
/// hint while another window has focus.
///
/// Pure — unit-testable without a window.
pub(super) fn native_theme_sync_is_due(
    window_is_key: bool,
    already_synced: bool,
    is_macos: bool,
) -> bool {
    !is_macos || window_is_key || already_synced
}

pub(super) fn take_window_close_request(
    pending: &mut std::collections::BTreeSet<u64>,
    seq: u64,
) -> bool {
    pending.remove(&seq)
}

/// Peacock: a STABLE per-window seed for `accent-color = auto`,
/// hashed from the window's working directory (the launch `-d DIR`, else the
/// process cwd). Same project → same seed → same accent across launches;
/// different projects → different accents. Pure given the cwd.
pub(super) fn accent_seed_from_cwd(cwd: Option<&std::path::Path>) -> u64 {
    use std::hash::{Hash, Hasher};
    let dir = cwd
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    // Canonicalize so every spelling of the same project
    // (`-d .`, `-d C:\proj`, a relative path, a trailing slash) hashes to the
    // SAME seed — the documented "same project → same accent" stability.
    // Falls back to the raw path when it doesn't resolve (nonexistent dir).
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut h);
    h.finish()
}

/// Pure: which segment index the tab-bar cursor falls in,
/// using the same rendered segment rects that hit-testing and painting use.
/// Used by the drag-to-reorder handler — the user grabs a tab,
/// drags, and the bar reorders to keep the dragged segment under the cursor.
/// Clamped to the first/last segment on screen (scrolled-out tabs are never
/// targets) so a cursor that overshoots either edge still produces a valid
/// target. Returns 0 for an empty bar.
pub(super) fn tab_drag_target_index(
    cursor_x: f32,
    cursor_y: f32,
    segments: &[kettle_render::TabSeg],
) -> usize {
    let Some(first) = segments.first() else {
        return 0;
    };
    // Which way the strip runs is a property of what was drawn: a vertical bar
    // stacks its segments down a shared column, a horizontal one lays them
    // across a shared row. Reading it back off the segments means the drag can
    // never disagree with the geometry the user is looking at.
    //
    // Testing `cursor_x` alone would break vertical bars, where every segment
    // shares one x span and the first one would always match.
    let vertical = segments.iter().any(|seg| seg.rect.1 != first.rect.1);
    // Only tabs on screen are targets: a pointer past the last one shown
    // (onto a scroll arrow or `+`) stops there rather than at a hidden tab.
    let mut last_shown = None;
    for seg in segments.iter().filter(|seg| tab_shown(seg)) {
        last_shown = Some(seg.idx);
        let (x, y, w, h) = seg.rect;
        let (cursor, end) = if vertical {
            (cursor_y, y + h)
        } else {
            (cursor_x, x + w)
        };
        if cursor < end {
            return seg.idx;
        }
    }
    last_shown
        .or_else(|| segments.last().map(|seg| seg.idx))
        .unwrap_or(0)
}

/// Pixel distance a held tab press must move before Kettle promotes the click
/// into the in-window reorder gesture. This is intentionally larger than the
/// low-level detachable-tab FSM threshold: a tab click should stay visually
/// anchored to the selected segment through normal hand jitter, while a real
/// drag still starts quickly at typical tab-bar heights.
pub(super) fn tab_reorder_drag_threshold_px(tab_bar_h: f32) -> f32 {
    (tab_bar_h * 0.5).clamp(8.0, 16.0)
}

/// Euclidean distance from a point to the nearest
/// edge of a rect — `0.0` when the point is inside. The tear decision is
/// "distance from the tab band ≥ threshold", which gives UNIFORM hysteresis
/// in every direction away from the band (the Chromium model): drag along
/// the strip = reorder, drag perpendicular past the slop = tear. Because a
/// `Top` band's upper edge coincides with the window's top edge, "above the
/// window" falls out of the same math — no separate outside-the-window
/// clause needed.
pub(super) fn dist_to_rect(cx: f32, cy: f32, rect: (f32, f32, f32, f32)) -> f32 {
    let (rx, ry, rw, rh) = rect;
    let dx = (rx - cx).max(cx - (rx + rw)).max(0.0);
    let dy = (ry - cy).max(cy - (ry + rh)).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

/// Has the cursor moved far enough from the tab
/// band to tear the dragged tab off into its own window? Pure so the
/// per-orientation cases are unit-testable without a window.
pub(super) fn tear_threshold_crossed(
    cx: f32,
    cy: f32,
    band: (f32, f32, f32, f32),
    threshold: f32,
) -> bool {
    threshold > 0.0 && dist_to_rect(cx, cy, band) >= threshold
}

/// How far (0.0..=1.0) the cursor has travelled from
/// the tab band toward the tear threshold. 0.0 at/inside the band, 1.0 at
/// (or past) the exact distance `tear_threshold_crossed` fires at, so the
/// ghost's visual lift saturates precisely where the tear happens. A
/// non-positive threshold can never tear, so it also never lifts. Pure for
/// the same unit-testability reason as `tear_threshold_crossed`.
pub(super) fn tear_lift_ratio(cx: f32, cy: f32, band: (f32, f32, f32, f32), threshold: f32) -> f32 {
    if threshold <= 0.0 {
        return 0.0;
    }
    (dist_to_rect(cx, cy, band) / threshold).clamp(0.0, 1.0)
}

/// Re-dock insertion index for a torn-off tab dropped at
/// `cursor` (main-axis coordinate) given the existing segments' main-axis
/// midpoints, in order. Negative midpoints mark hidden segments.
/// First visible midpoint past the cursor wins; otherwise insert after the
/// last visible tab. Distinct from `tab_drag_target_index`
/// (which picks the segment UNDER the cursor for reorder): docking inserts
/// BETWEEN segments, so a strip of n tabs has n+1 valid slots.
pub(super) fn dock_insertion_index(seg_mids: &[f32], cursor: f32) -> usize {
    let mut insertion = 0;
    for (idx, &mid) in seg_mids.iter().enumerate() {
        if mid < 0.0 {
            continue;
        }
        if cursor < mid {
            return idx;
        }
        insertion = idx + 1;
    }
    insertion
}

/// The tab index a torn-off tab dropped at `cursor` (main-axis coordinate)
/// lands at, among the tabs on screen: before the first whose midpoint is
/// past the cursor, else right after the last one shown, never after tabs
/// scrolled out of view. `None` for a bar with no tab on screen.
pub(super) fn dock_insertion_tab(
    segments: &[kettle_render::TabSeg],
    vertical: bool,
    cursor: f32,
) -> Option<usize> {
    let shown: Vec<&kettle_render::TabSeg> = segments.iter().filter(|seg| tab_shown(seg)).collect();
    let mids: Vec<f32> = shown
        .iter()
        .map(|seg| {
            let (x, y, w, h) = seg.rect;
            if vertical { y + h * 0.5 } else { x + w * 0.5 }
        })
        .collect();
    let slot = dock_insertion_index(&mids, cursor);
    match shown.get(slot) {
        Some(seg) => Some(seg.idx),
        None => shown.last().map(|seg| seg.idx + 1),
    }
}

/// Rescue-tick cadence (one frame at 60Hz) while a torn drag is in flight or
/// a silent native handoff is being watched.
const TORN_TICK_MS: u64 = 16;

/// Diagnostics/test override: `KETTLE_TEAR_MANUAL_FOLLOW=1` skips the native
/// `drag_window()` handoff so a tear always takes the manual-follow/rescue-tick
/// path. Otherwise demotion is a nondeterministic race (the WM silently
/// dropping `_NET_WM_MOVERESIZE` for a just-mapped window) that cannot be
/// triggered on demand, so the live tear-off smoke pins BOTH paths by
/// launching one instance per mode. Read per tear, not cached: trivially
/// cheap, and it keeps the override in one place.
fn tear_native_handoff_disabled() -> bool {
    std::env::var_os("KETTLE_TEAR_MANUAL_FOLLOW").is_some_and(|v| v == "1")
}

/// Real pointer travel (px) required, alongside the 400ms grace, before a
/// silent native handoff is demoted to manual follow. A WM that took the grab
/// moves the frame with the pointer, so travel-without-`Moved` is proof it
/// never did; the floor keeps a stationary hover from demoting a
/// healthy-but-quiet grab.
const DEMOTION_TRAVEL_PX: f64 = 24.0;

#[cfg(any(target_os = "linux", test))]
#[derive(PartialEq, Eq)]
pub(super) struct WaylandDropTarget {
    pane: Option<u64>,
    broadcast: crate::mux::BroadcastScope,
    input_generation: u64,
    preview: Option<crate::media::PreviewFocus>,
    pane_input: Option<u64>,
}

#[cfg(any(target_os = "linux", test))]
impl WaylandDropTarget {
    pub(super) fn capture(ws: &WindowState) -> Self {
        Self {
            pane: ws.mux.active_focus(),
            broadcast: ws.mux.broadcast.clone(),
            input_generation: ws.ime_focus_generation,
            preview: ws.preview_focus,
            pane_input: ws
                .mux
                .active_focus()
                .and_then(|id| ws.mux.panes.get(&id))
                .map(|pane| pane.input_generation()),
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) struct WaylandTabDrag {
    source: u64,
    source_index: usize,
    panes: Vec<u64>,
    carrier: u64,
    preview: Option<u64>,
}

pub(super) struct TornDrag {
    /// Seq of the torn-off window being dragged.
    pub(super) seq: u64,
    /// Seq of the mouse-capture holder: the tear's SOURCE window (it keeps
    /// streaming CursorMoved while the button is held), or `seq` itself
    /// for a lone-tab whole-window drag. Manual-follow only listens to the
    /// carrier — without this gate, stale tracking would hijack EVERY
    /// window's cursor stream.
    pub(super) carrier: u64,
    /// Handoff instant. The X11/macOS pointer-event drop heuristics are
    /// suppressed for a short window after it (a stray client motion can
    /// race the WM actually taking the move grab), and the native→manual
    /// demotion fires off it when the WM never takes the drag at all.
    pub(super) started: std::time::Instant,
    /// Cursor offset from the torn window's FRAME top-left in drag coordinates
    /// (macOS desktop points, physical pixels on Windows/X11),
    /// chosen at tear time so the pointer holds the tab. `Moved(pos) + grab`
    /// approximates the live screen cursor during the native move loop
    /// (winit's `Moved` reports the frame position on Windows — verified in
    /// the vendored 0.30.13 WM_WINDOWPOSCHANGED handler).
    pub(super) grab: (f64, f64),
    /// Latched dock target: (sibling window seq, insertion index). Updated
    /// by every hit-test; the drop commits whatever is latched.
    pub(super) dock: Option<(u64, usize)>,
    /// `drag_window()` succeeded — the OS carries the window. The drop then
    /// arrives as winit's synthesized post-WM_EXITSIZEMOVE left-release
    /// (Windows) or as the first client pointer event after the WM's pointer
    /// grab ends (X11/macOS — the client receives NO pointer events while
    /// the WM moves the window). `false` = manual-follow: the SOURCE window
    /// still holds mouse capture, repositions the torn window from its own
    /// CursorMoved stream, and its left-release is the drop.
    pub(super) native: bool,
    /// A native caption gesture is OS-owned and must never be demoted to
    /// the manual-follow rescue used for failed tab-drag handoffs.
    pub(super) caption: bool,
    /// At least one `Moved` arrived since the handoff. Gates the
    /// pointer-event drop heuristic against the PostMessage race on
    /// Windows (a stray client mouse-move can slip in between
    /// `drag_window()`'s posted WM_NCLBUTTONDOWN and the modal loop
    /// actually starting) and against an X11 tear the user never moved.
    pub(super) saw_move: bool,
    /// Last signal (tear or `Moved`) — the stale-tracking failsafe: a
    /// torn drag with no movement for 120s is abandoned by `about_to_wait`
    /// (covers an X11 tear whose release we never observe). The rescue
    /// tick also arbitrates on it (a fresh CursorMoved-driven follow
    /// suppresses the tick for 30ms).
    pub(super) last_signal: std::time::Instant,
    /// The torn window's HWND (Windows; `None` elsewhere) — the dock-hover
    /// translucency and the z-order walk both key off it, and tracking
    /// teardown needs it to restore full opacity from any code path.
    /// Only Windows code reads it; populated unconditionally so the
    /// struct shape stays identical across platforms.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(super) hwnd: Option<isize>,
    /// X11: the real cursor position at the last tracking signal, refreshed
    /// by every `Moved` that pairs with a live-cursor query, or by the rescue
    /// tick's first look. Demotion evidence: if the pointer has since
    /// travelled while no further `Moved` arrived, the WM is not actually
    /// moving the window (an accepted-but-ignored `_NET_WM_MOVERESIZE`, an
    /// unmapped-window race, or a grab dropped after one incidental placement
    /// `Moved`) and the tick takes over. Purely time-based demotion would
    /// fight a WM that DID take the grab over a pointer simply holding still.
    pub(super) signal_cursor: Option<(f64, f64)>,
}

/// How a new in-process window starts life.
pub(super) enum WindowOpen {
    /// A fresh window with one shell tab (`Action::NewWindow`).
    Fresh { cwd: Option<String> },
    /// Adopt a tab detached from another window — the live tab move /
    /// tear-off. PTYs keep running; nothing respawns.
    AdoptTab {
        tab: crate::mux::DetachedTab,
        source_color: Option<kettle_config::Rgb>,
        /// The source window's runtime font size, so the torn-off window
        /// draws its tab and status bars at the size the drag anchor was
        /// measured with.
        font_size: Option<f32>,
        /// Font size to restore when the adopted tab leaves scaled zoom.
        scaled_zoom_prev_font_size: Option<f32>,
        /// The moved tab's paste receipt and pending video preview.
        paste: Box<crate::window_state::MovedPasteState>,
    },
    /// Respawn one saved window of a multi-window session (tabs + geometry)
    /// on `--restore` / `restore-session = true` startup.
    Restore(crate::session::SWindow),
}

/// Is this window's backend Wayland? A runtime check, since X11 vs Wayland is
/// a runtime choice on Linux and `cfg!` can't answer it. Wayland gets the
/// tear-at-release fallback (no client-side window positioning, and
/// `xdg_toplevel.move` is compositor-validated against a press serial a
/// just-created surface never saw).
pub(super) fn window_is_wayland(w: &winit::window::Window) -> bool {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    matches!(
        w.window_handle().map(|h| h.as_raw()),
        Ok(RawWindowHandle::Wayland(_))
    )
}

/// Tear-off UX, Windows: the LIVE screen cursor. The tear
/// positions the torn window from the CursorMoved event's coordinates,
/// but window creation takes ~50-150ms — by the time `drag_window()`'s
/// posted WM_NCLBUTTONDOWN starts the modal move loop, a fast-moving
/// pointer has slid well past the event position, and DefWindowProc
/// anchors the loop at the CURRENT cursor. Without re-anchoring, the
/// pointer ends up holding the window mid-body instead of at the grab
/// point (measured: ~97px of slide under a scripted drag). Re-positioning
/// from the live cursor immediately before the handoff closes the gap to
/// sub-millisecond.
#[cfg(target_os = "windows")]
pub(super) fn cursor_screen_pos() -> Option<(f64, f64)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    // SAFETY: p is writable POINT storage for the synchronous cursor query.
    unsafe { GetCursorPos(&mut p) }
        .ok()
        .map(|()| (f64::from(p.x), f64::from(p.y)))
}

/// Tear-off UX, X11: one `QueryPointer` round-trip on a lazy
/// side connection — the LIVE screen cursor (root coords ARE screen
/// coords) plus the primary button's held state from the same reply's
/// button mask. Needed because the WM's move-grab anchor drifts from the
/// tear-time `grab` offset (measured 55-86px under Mutter — enough to miss
/// the whole dock band), pointer motion stops reaching the client entirely
/// while the WM holds the move grab, and the Esc-cancel tell needs the
/// physical button state (see `primary_button_physically_held`). A Wayland
/// session (no X server for this client) fails the one-time connect and
/// yields `None` forever — callers keep their approximations. Only ever
/// exercised while torn-drag tracking is live.
#[cfg(all(unix, not(target_os = "macos")))]
fn x11_pointer_state() -> Option<((f64, f64), bool)> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, KeyButMask};
    use x11rb::rust_connection::RustConnection;
    static X11: std::sync::OnceLock<Option<(RustConnection, u32)>> = std::sync::OnceLock::new();
    let (conn, root) = X11
        .get_or_init(|| {
            x11rb::connect(None).ok().map(|(conn, screen)| {
                let root = conn.setup().roots[screen].root;
                (conn, root)
            })
        })
        .as_ref()?;
    let reply = conn.query_pointer(*root).ok()?.reply().ok()?;
    // The server applies the pointer-button mapping before reporting the
    // state mask, so BUTTON1 here is the LOGICAL primary — a left-handed
    // mapping is already accounted for.
    let held = reply.mask.contains(KeyButMask::BUTTON1);
    Some(((f64::from(reply.root_x), f64::from(reply.root_y)), held))
}

/// Tear-off UX, X11: the LIVE screen cursor, counterpart of the Windows
/// `GetCursorPos` path above. See `x11_pointer_state`.
#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn cursor_screen_pos() -> Option<(f64, f64)> {
    x11_pointer_state().map(|(pos, _)| pos)
}

#[cfg(target_os = "macos")]
pub(super) fn cursor_screen_pos() -> Option<(f64, f64)> {
    crate::native_drag::cursor_position()
}

#[cfg(not(any(target_os = "windows", unix)))]
pub(super) fn cursor_screen_pos() -> Option<(f64, f64)> {
    None
}

// AppKit's global desktop uses points; winit's physical positions multiply
// the entire desktop origin by each window's scale. Normalize before comparing
// windows so a Retina donor can dock onto a non-Retina target.
fn drag_coordinate_scale(window: &Window) -> f64 {
    if cfg!(target_os = "macos") {
        window.scale_factor()
    } else {
        1.0
    }
}

pub(super) fn drag_position(
    window: &Window,
    position: winit::dpi::PhysicalPosition<i32>,
) -> (f64, f64) {
    let scale = drag_coordinate_scale(window);
    (f64::from(position.x) / scale, f64::from(position.y) / scale)
}

fn drag_native_position(position: (f64, f64)) -> winit::dpi::Position {
    if cfg!(target_os = "macos") {
        winit::dpi::LogicalPosition::new(position.0, position.1).into()
    } else {
        winit::dpi::PhysicalPosition::new(position.0 as i32, position.1 as i32).into()
    }
}

/// Re-dock, Windows: per-window alpha for the dock-hover
/// preview. The torn window rides UNDER the pointer, so at the moment a
/// dock target latches, the full-size torn window is covering the very
/// strip the insertion marker draws in — the user would never see it.
/// Going translucent while a target is latched shows the target strip
/// (and marker) through the dragged window, Chromium-style. Verified
/// live against kettle's wgpu flip-model swapchain (renders fine under
/// WS_EX_LAYERED + LWA_ALPHA). 255 restores and drops the layered bit.
#[cfg(target_os = "windows")]
fn set_window_alpha(hwnd: isize, alpha: u8) {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW,
    };
    let h = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: h comes from a live winit window on its UI thread; only integer style and alpha
    // values are passed.
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        if alpha == 255 {
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex & !(WS_EX_LAYERED_BIT));
        } else {
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_LAYERED_BIT);
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), alpha, LWA_ALPHA);
        }
    }
}

/// Paint a freshly created window's background in the configured terminal
/// background colour, and report whether it took.
///
/// Sets the window class brush that Win32 erases with on `WM_ERASEBKGND`, so
/// the pre-paint window would match what the renderer is about to draw. This
/// is best effort only. A light-background probe showed the pre-paint window
/// stays black whatever the brush is, so the early reveal does not depend on
/// it (see `background_is_dark_enough_to_reveal_early`).
///
/// Returns false if the brush cannot be created.
#[cfg(target_os = "windows")]
pub(super) fn set_window_background_brush(hwnd: isize, rgb: kettle_config::Rgb) -> bool {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, HBRUSH, HGDIOBJ};
    use windows::Win32::UI::WindowsAndMessaging::{GCLP_HBRBACKGROUND, SetClassLongPtrW};

    let kettle_config::Rgb { r, g, b } = rgb;
    // COLORREF is 0x00BBGGRR, not RGB order.
    let colorref = COLORREF(u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16));
    let h = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: h is a live winit HWND on the UI thread. Its class starts with no
    // brush; only this function installs owned brushes, and replacement/deletion
    // is serialized with window painting on that thread.
    unsafe {
        let brush = CreateSolidBrush(colorref);
        if brush.is_invalid() {
            return false;
        }
        let previous = SetClassLongPtrW(h, GCLP_HBRBACKGROUND, brush.0 as isize);
        if previous != 0 {
            let _ = DeleteObject(HGDIOBJ(previous as *mut core::ffi::c_void));
        }
        let _ = HBRUSH(brush.0);
        true
    }
}

/// Non-Windows platforms have no equivalent cheap guarantee that the first
/// composited frame is the right colour, so they keep hiding until painted.
#[cfg(not(target_os = "windows"))]
pub(super) fn set_window_background_brush(_hwnd: isize, _rgb: kettle_config::Rgb) -> bool {
    false
}

/// Current process working set in MiB, for startup phase accounting.
///
/// Kettle's resident footprint is large and barely moves with scrollback, font
/// or features, so it is a FIXED cost. Sampling across the phases that allocate
/// is the only way to attribute it (docs/PERFORMANCE.md has the numbers).
///
/// `0.0` where unavailable; this is diagnostics, never control flow.
#[cfg(target_os = "windows")]
pub(super) fn process_working_set_mb() -> f64 {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let Ok(size) = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()) else {
        // A zero size would be accepted by the signature and rejected -- or
        // worse, partially honoured -- by the API. Refuse instead of asking for
        // a struct of no bytes.
        return 0.0;
    };
    let mut counters = PROCESS_MEMORY_COUNTERS {
        // `cb` is an in-parameter: the API uses it to decide how much of the
        // struct it may write. `Default` leaves it 0, which is not a smaller
        // request, it is a malformed one.
        cb: size,
        ..Default::default()
    };
    // SAFETY: counters is writable for size bytes and cb matches; GetCurrentProcess returns a
    // borrowed pseudo-handle.
    unsafe {
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size).is_ok() {
            return counters.WorkingSetSize as f64 / (1024.0 * 1024.0);
        }
    }
    0.0
}

#[cfg(not(target_os = "windows"))]
pub(super) fn process_working_set_mb() -> f64 {
    0.0
}

/// May the window be shown BEFORE the first painted frame?
///
/// Only when the terminal's background is dark enough that the difference from
/// black is imperceptible, because black is what an unpainted window actually
/// shows. The class brush set alongside this does NOT decide what appears. With
/// a light background configured (`#f0e8d8`) the pre-paint window still sampled
/// `0,0,0`, because winit owns `WM_ERASEBKGND`. Revealing a light-themed window
/// early would trade a late window for a black flash.
///
/// Dark backgrounds have no such trade. `#101010` is 16 levels from black on
/// each channel, which nobody can see, and revealing early shows the window
/// during the ~730ms of renderer init instead of after it.
///
/// The threshold is deliberately strict. This is not "dark theme" detection; it
/// is "indistinguishable from black", so anything with visible colour keeps
/// hiding until painted.
pub(super) fn background_is_dark_enough_to_reveal_early(bg: kettle_config::Rgb) -> bool {
    // Windows only. "An unpainted window is black" was observed on DWM and is
    // NOT a portable fact; on another compositor the pre-paint frame could as
    // easily be white or transparent.
    if !cfg!(target_os = "windows") {
        return false;
    }
    const MAX_CHANNEL: u8 = 24;
    bg.r <= MAX_CHANNEL && bg.g <= MAX_CHANNEL && bg.b <= MAX_CHANNEL
}

/// `WS_EX_LAYERED` as the isize `GetWindowLongPtrW` works in.
#[cfg(target_os = "windows")]
const WS_EX_LAYERED_BIT: isize = 0x0008_0000;

/// Torn-window alpha while a dock target is latched. Translucent enough to
/// read the target strip + insertion marker through the dragged window,
/// opaque enough that the window still reads as "the thing you're holding"
/// (probed live at several values).
#[cfg(target_os = "windows")]
const DOCK_HOVER_ALPHA: u8 = 150;

/// Is the primary mouse button PHYSICALLY held right now? Distinguishes an
/// Esc-CANCELLED move loop (button still down when the WM ends its grab)
/// from a real drop (button up); the client-visible events look identical
/// for both. Windows honors a swapped-button mouse (`SM_SWAPBUTTON`:
/// GetAsyncKeyState reports PHYSICAL buttons, so the primary button of a
/// left-handed mouse is VK_RBUTTON); X11 reads the button mask from the
/// same `QueryPointer` the live-cursor path uses (the server pre-applies
/// the pointer mapping there). macOS uses the combined session button state.
/// False elsewhere (Wayland never produces a release-while-held; an X11 query failure also
/// degrades to false, which errs toward committing a real drop rather
/// than dropping user data on a cancel we cannot prove).
pub(super) fn primary_button_physically_held() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        x11_pointer_state().is_some_and(|(_, held)| held)
    }
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON,
        };
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
        // SAFETY: SM_SWAPBUTTON is a valid metric index and this query takes no pointers.
        let vk = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 {
            VK_RBUTTON
        } else {
            VK_LBUTTON
        };
        // SAFETY: vk is one of the valid left/right mouse virtual-key codes; no pointers are
        // passed.
        (unsafe { GetAsyncKeyState(i32::from(vk.0)) } as u16) & 0x8000 != 0
    }
    #[cfg(target_os = "macos")]
    {
        crate::native_drag::primary_held()
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    false
}

/// HWND of a winit window.
/// `None` off-Windows (the `Win32` raw-handle variant exists on every
/// platform, so this compiles everywhere without cfg noise).
pub(super) fn window_hwnd(w: &winit::window::Window) -> Option<isize> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match w.window_handle().ok().map(|h| h.as_raw()) {
        Some(RawWindowHandle::Win32(h)) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Resolve overlapping dock candidates, and foreign windows covering a band,
/// against the REAL z-order. Walks down from the torn window (topmost while
/// dragged); the first visible, uncloaked window containing the cursor
/// decides: one of ours → that's the target; a foreign window → it covers the
/// band, no dock. The cloak check matters: suspended UWP apps park full-screen
/// DWM-cloaked windows in the z-order that would otherwise always read as
/// "covered".
///
/// Known limitation (accepted): always-on-top foreign windows
/// sit ABOVE the torn window and are invisible to a downward walk — a
/// band covered by one still shows the marker (the merge itself is
/// unaffected). GetWindowRect also includes the invisible Win10/11 resize
/// borders, which mirrors real input routing (those borders DO capture
/// clicks), so near-edge "covered" verdicts match what a click would do.
#[cfg(target_os = "windows")]
fn zorder_pick(
    torn_hwnd: isize,
    cursor: (f64, f64),
    candidates: &[(isize, u64, usize)],
) -> Option<(u64, usize)> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows::Win32::UI::WindowsAndMessaging::{
        GW_HWNDNEXT, GetWindow, GetWindowRect, IsWindowVisible,
    };
    let (px, py) = (cursor.0 as i32, cursor.1 as i32);
    // SAFETY: torn_hwnd comes from the winit window; GetWindow borrows it and returns an
    // OS-managed handle.
    let mut hwnd = unsafe { GetWindow(HWND(torn_hwnd as *mut core::ffi::c_void), GW_HWNDNEXT) };
    // Bounded walk: the desktop can hold hundreds of top-level windows but
    // a runaway loop must be impossible.
    for _ in 0..1024 {
        let Ok(h) = hwnd else { break };
        // SAFETY: h is an OS-managed window handle; this query takes no pointers and tolerates
        // a vanished window.
        if unsafe { IsWindowVisible(h) }.as_bool() {
            let mut cloaked: u32 = 0;
            // SAFETY: cloaked is writable u32 storage matching DWMWA_CLOAKED and the supplied
            // byte count.
            let _ = unsafe {
                DwmGetWindowAttribute(
                    h,
                    DWMWA_CLOAKED,
                    (&raw mut cloaked).cast(),
                    std::mem::size_of::<u32>() as u32,
                )
            };
            let mut r = RECT::default();
            if cloaked == 0
                // SAFETY: r is writable RECT storage for the synchronous query; a vanished
                // window returns an error.
                && unsafe { GetWindowRect(h, &mut r) }.is_ok()
                && px >= r.left
                && px < r.right
                && py >= r.top
                && py < r.bottom
            {
                let found = h.0 as isize;
                return candidates
                    .iter()
                    .find(|(ch, _, _)| *ch == found)
                    .map(|&(_, s, i)| (s, i));
            }
        }
        // SAFETY: h is borrowed from the OS window list; GetWindow returns another borrowed
        // handle or an error.
        hwnd = unsafe { GetWindow(h, GW_HWNDNEXT) };
    }
    None
}

/// The live monitor rects, for clamping a saved window geometry whose
/// monitor is gone (see `session::clamp_geometry_to_monitors`).
pub(super) fn monitor_rects(event_loop: &ActiveEventLoop) -> Vec<(i32, i32, u32, u32)> {
    event_loop
        .available_monitors()
        .map(|m| {
            let p = m.position();
            let s = m.size();
            (p.x, p.y, s.width, s.height)
        })
        .collect()
}

impl App {
    /// The map key of the WindowState whose OS window is `wid`, if any.
    /// Returns `WindowState::seq`, which is the map key by invariant.
    pub(super) fn seq_of_window(&self, wid: WindowId) -> Option<u64> {
        self.windows
            .values()
            .find_map(|w| (w.window.as_ref().map(|x| x.id()) == Some(wid)).then_some(w.seq))
    }

    pub(super) fn request_window_close(&mut self, seq: u64) {
        self.pending_window_closes.insert(seq);
    }

    pub(super) fn window_close_pending(&self, seq: u64) -> bool {
        self.pending_window_closes.contains(&seq)
    }

    /// The dispatch wrappers' epilogue. Reinserts the checked-out window
    /// — or, when the inner handler flagged this window for close
    /// or a quit, drops it (panes' PTYs die with their Mux) and exits the
    /// event loop once no windows remain. This is the ONLY place a window
    /// close reaches `event_loop.exit()`, so "exit only when the map is
    /// empty" holds by construction (drift-guarded in
    /// `event_loop_exit_sites_are_allowlisted`).
    pub(super) fn finish_window_dispatch(
        &mut self,
        event_loop: &ActiveEventLoop,
        seq: u64,
        mut ws: WindowState,
    ) {
        if modal_editor(&ws).is_some() {
            self.sync_modal_editor_scroll(&mut ws);
            self.update_ime_cursor_area(&mut ws);
        }
        if self.quit_requested {
            if let Some(owner) = self.update_toast.as_ref().map(|toast| toast.window) {
                self.close_update_toast_window(owner);
            }
            drop(ws);
            self.windows.clear();
            event_loop.exit();
            return;
        }
        if take_window_close_request(&mut self.pending_window_closes, seq) {
            self.close_update_toast_window(seq);
            // A dying window that is the torn window or the manual-follow
            // capture holder takes its drag with it. Abandon eagerly (clears
            // the latched preview on the still-mapped target and restores
            // opacity) instead of leaving stale tracking for the heuristics to
            // trip over.
            if self
                .torn_drag
                .as_ref()
                .is_some_and(|t| t.seq == seq || t.carrier == seq)
            {
                self.abandon_torn_drag(None);
            }
            drop(ws);
            self.hoover_mapped_groups();
            if self.windows.is_empty() {
                event_loop.exit();
            } else if self.focused_seq == seq
                && let Some(&next) = self.windows.keys().next_back()
            {
                // Focus falls to the most recently opened remaining window;
                // the OS will confirm with a Focused(true) shortly.
                self.focused_seq = next;
            }
            return;
        }
        let mut ws = ws;
        self.sync_pane_focus_reports(&mut ws);
        self.windows.insert(seq, ws);
    }

    pub(super) fn focus_or_open_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        cwd: Option<String>,
    ) -> bool {
        let target = self
            .windows
            .get(&self.focused_seq)
            .or_else(|| self.windows.values().max_by_key(|window| window.seq));
        if let Some(ws) = target
            && let Some(window) = &ws.window
        {
            self.focused_seq = ws.seq;
            window.set_visible(true);
            window.set_minimized(false);
            window.focus_window();
            return true;
        }
        self.open_window(event_loop, WindowOpen::Fresh { cwd }, None, None)
            .is_ok()
    }

    pub(super) fn drain_activation_requests(&mut self, event_loop: &ActiveEventLoop) {
        // Keep requests queued while window 1 is still creating the shared GPU
        // context. `resumed` calls this again immediately after initialization.
        if self.gpu.is_none() {
            return;
        }
        let pending = self
            .activation
            .as_ref()
            .map(crate::activation_server::ActivationInbox::drain)
            .unwrap_or_default();
        for request in pending {
            if !request.claim() {
                continue;
            }
            if !activation_recording_available(
                request.request.requires_recording(),
                self.recording_status(),
            ) {
                log::warn!(
                    "bare-launch activation rejected because the requested dev recorder is not active"
                );
                let _ = request.completion.send(false);
                continue;
            }
            let cwd = request.request.cwd().map(str::to_string);
            let opened = if request.request.focuses_existing() {
                self.focus_or_open_window(event_loop, cwd)
            } else {
                self.open_window(event_loop, WindowOpen::Fresh { cwd }, None, None)
                    .is_ok()
            };
            if opened {
                log::info!("bare-launch activation activated a window");
                if !request.request.focuses_existing()
                    && let Some(recorder) = self.recorder.as_mut()
                {
                    recorder.record_marker("kettle:activation_window");
                }
            }
            let _ = request.completion.send(opened);
        }
    }

    /// Lift the drag ghost toward the same threshold that triggers tear-off.
    /// A window without its own active drag has no lift.
    pub(super) fn tear_lift_for(&self, ws: &WindowState) -> f32 {
        if !self.cfg.detachable_tabs {
            return 0.0;
        }
        match ws.detach_drag {
            crate::detach::DragState::DraggingInside { .. }
            | crate::detach::DragState::DraggingOutside { .. } => self
                .dock_band(ws)
                .map(|band| {
                    tear_lift_ratio(
                        ws.cursor.x as f32,
                        ws.cursor.y as f32,
                        band,
                        1.5 * self.tab_bar_h(ws),
                    )
                })
                .unwrap_or(0.0),
            _ => 0.0,
        }
    }

    /// The window numbered `seq`: `ws` or another.
    pub(super) fn window_by_seq<'a>(
        &'a mut self,
        ws: &'a mut WindowState,
        seq: u64,
    ) -> Option<&'a mut WindowState> {
        if ws.seq == seq {
            Some(ws)
        } else {
            self.windows.get_mut(&seq)
        }
    }
}
