use super::*;

impl App {
    pub(super) fn window_event_inner(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        event: WindowEvent,
    ) {
        let preserves_menu_redraw_hint = matches!(
            &event,
            WindowEvent::RedrawRequested | WindowEvent::CursorMoved { .. }
        ) && ws.context_menu.is_some();
        if !preserves_menu_redraw_hint {
            ws.reuse_pane_snapshots_once = false;
        }
        if let (Some(adapter), Some(window)) = (&mut ws.accessibility, &ws.window) {
            adapter.process_event(window, &event);
        }
        match event {
            WindowEvent::CloseRequested => {
                // The titlebar ✕ and Alt+F4 ask the window manager to destroy a
                // window; they are not Kettle commands. Ordinary applications
                // veto that only for unsaved document state, and a terminal has
                // no authoritative dirty bit ("a process exists" is not "work
                // would be lost"). A veto would also make the gesture
                // nondeterministic, and because `shell_idle()` reports busy for
                // any pane without OSC 133 integration, EVERY multi-pane window
                // would prompt. So this gesture closes.
                //
                // `ask-before-closing = always` is the opt-in for people who
                // want the question here, and the only policy that still asks
                // it. The keybind close actions (`ClosePane`, `CloseTab`,
                // `CloseWindow`) are Kettle commands, not OS requests, and keep
                // the full policy. Ask BEFORE any of the teardown below, so the
                // session is not saved and the recorder not finished while the
                // user is still deciding.
                if self.cfg.ask_before_closing == kettle_config::AskBeforeClosing::Always {
                    let (scope, busy) = window_close_scope(ws);
                    if self.confirm_close(
                        ws,
                        scope,
                        busy,
                        ConfirmAction::CloseWindow {
                            drop_panes: DropPanes::No,
                        },
                    ) {
                        return;
                    }
                }
                self.close_window_now(ws, DropPanes::No);
            }
            WindowEvent::Resized(size) => {
                self.confirm_cancel_press(ws);
                self.confirm_scroll(ws, 0, true);
                self.sync_output_frame_budget(ws, false);
                // Minimizing a window delivers Resized(0, 0) on Windows.
                // Reconfiguring the surface + `resize_all` to a 0×0 area would
                // collapse every PTY to a 1×1 grid (`grid_of`'s `.max(1)`) and
                // fire a SIGWINCH storm that reflows/redraws every TUI in every
                // pane, then reflows them all back on restore. Skip a
                // degenerate size entirely: keep the last good grid so PTYs
                // hold their real dimensions; the genuine restore carries the
                // true non-zero size and reflows once.
                if size.width == 0 || size.height == 0 {
                    ws.dpi_resize_surface_suspended = true;
                    return;
                }
                // AppKit can briefly restore its stale standard frame. Wait
                // for the window-local restore before reflowing terminal grids.
                if ws
                    .native_fullscreen
                    .as_ref()
                    .is_some_and(|native| native.pending())
                {
                    ws.dpi_resize_surface_suspended = true;
                    return;
                }
                ws.dpi_resize_surface_suspended = false;
                // The Metal layer stretches its last frame to the new size
                // until the resize's frame presents, and that frame may have
                // to retry. The cursor patch would keep blinking where the
                // cell was.
                if ws
                    .renderer
                    .as_ref()
                    .is_none_or(|r| r.surface_size() != (size.width, size.height))
                {
                    Self::hide_cursor_layer(ws);
                }
                let renderer_ready = self.dpi_resize_renderer_ready(ws);
                if ws.dpi_resize.on_event(DpiResizeEvent::Resized {
                    width: size.width,
                    height: size.height,
                    renderer_ready,
                }) == DpiResizeAction::ApplyLayout
                {
                    self.commit_window_resize(ws, size.width, size.height);
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // The patch was drawn at the old scale, and the new-scale frame
                // may wait for the paired resize.
                Self::hide_cursor_layer(ws);
                self.sync_output_frame_budget(ws, true);
                // Apply the new DPI scale so text follows >100% Windows scaling
                // and rescales when dragged to a different-DPI monitor.
                // set_scale re-derives physical font metrics + cell size. Do
                // not re-grid yet: on Windows this event precedes the
                // SetWindowPos-driven Resized, so the physical size is still
                // from the old monitor. The coalescer commits once at the
                // following usable Resized; about_to_wait is the fallback for
                // backends that do not emit one.
                ws.dpi_resize.on_event(DpiResizeEvent::ScaleFactorChanged);
                ws.frame_recovery.expedite(std::time::Instant::now());
                if !ws.frame_recovery.renderer_rebuild_pending()
                    && let Some(r) = ws.renderer.as_mut()
                {
                    r.set_scale(scale_factor as f32);
                }
            }
            WindowEvent::ThemeChanged(theme) => {
                self.apply_os_theme_preference(ws, theme);
            }
            WindowEvent::ModifiersChanged(m) => {
                #[cfg(not(target_os = "macos"))]
                {
                    ws.mods = m.state();
                }
                #[cfg(target_os = "macos")]
                {
                    use winit::keyboard::ModifiersKeyState;

                    ws.macos_raw_mods = m.state();
                    ws.macos_left_option_pressed = m.lalt_state() == ModifiersKeyState::Pressed;
                    ws.macos_right_option_pressed = m.ralt_state() == ModifiersKeyState::Pressed;
                    refresh_macos_modifiers(ws, self.cfg.macos_option_as_alt);
                }
                // Modifier change can flip the URL hover affordance from
                // text-I-beam to pointing-hand without the mouse moving
                // (Ctrl held = "this click would open"). Re-sync the
                // cursor icon so the affordance updates the moment Ctrl
                // is pressed/released over a link.
                self.sync_cursor_icon(ws);
            }
            // Terminator parity: winit CursorLeft/Entered events transition the
            // detach FSM. CursorLeft → DraggingOutside; CursorEntered →
            // DraggingInside (the user brought the cursor back, which cancels
            // the cross-window flow). Mouse-down arming on the tab bar enters
            // the FSM, and a transition on `Idle` is a harmless no-op.
            //
            // The torn window streams `Moved` during the OS move loop (Windows:
            // WM_WINDOWPOSCHANGED inside the NC modal loop; X11:
            // ConfigureNotify from the WM move, both verified). Approximate the
            // live cursor as frame origin + grab offset and drive the re-dock
            // hit-test against every sibling window's tab band.
            WindowEvent::Moved(pos) => {
                self.sync_output_frame_budget(ws, false);
                if let Some(started) = ws
                    .native_caption_drag
                    .as_ref()
                    .and_then(|drag| drag.take_start())
                    && self.torn_drag.is_none()
                    && crate::native_drag::may_dock_caption(
                        self.cfg.detachable_tabs,
                        !matches!(self.cfg.tab_bar, TabBarMode::Off),
                        ws.mux.tabs.len(),
                        self.pointer_modal_open(ws),
                    )
                    && let Some(cursor) = cursor_screen_pos()
                    && let Some(window) = ws.window.as_ref()
                    && !window.is_maximized()
                    && window.fullscreen().is_none()
                {
                    let origin = drag_position(window, pos);
                    self.torn_drag = Some(TornDrag {
                        seq: ws.seq,
                        carrier: ws.seq,
                        started,
                        grab: (cursor.0 - origin.0, cursor.1 - origin.1),
                        dock: None,
                        native: true,
                        caption: true,
                        saw_move: false,
                        last_signal: std::time::Instant::now(),
                        hwnd: window_hwnd(window),
                        signal_cursor: Some(cursor),
                    });
                }
                let Some(td) = self.torn_drag.as_mut() else {
                    return;
                };
                if td.seq != ws.seq {
                    return;
                }
                td.saw_move = true;
                td.last_signal = std::time::Instant::now();
                let grab = td.grab;
                // Frame + grab approximates the pointer, but the move
                // loop's anchor drifts from `grab` by however far the
                // pointer slid between press/tear and loop start — a
                // session recording measured 55-86px under Mutter, enough
                // to miss the whole band. The LIVE cursor is one call away
                // on Windows (GetCursorPos) AND on X11 (QueryPointer via
                // the side connection); the approximation remains the
                // fallback for a failed query. Each real-cursor + `Moved`
                // pair also refreshes `signal_cursor`, the rescue tick's
                // travel-without-`Moved` demotion reference.
                let origin = ws
                    .window
                    .as_deref()
                    .map(|window| drag_position(window, pos))
                    .unwrap_or((f64::from(pos.x), f64::from(pos.y)));
                let approx = (origin.0 + grab.0, origin.1 + grab.1);
                let real = cursor_screen_pos();
                if let Some(cur) = real {
                    td.signal_cursor = Some(cur);
                }
                let cursor = real.unwrap_or(approx);
                let torn_hwnd = ws.window.as_deref().and_then(window_hwnd);
                self.update_dock_target(cursor, torn_hwnd, None);
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(dialog) = ws.confirm_dialog.as_mut() {
                    dialog.armed_press = None;
                }
                let prev = std::mem::take(&mut ws.detach_drag);
                ws.detach_drag = prev.on_cursor_leave_window();
                if ws.selecting
                    && let Some(window) = &ws.window
                {
                    let size = window.inner_size();
                    ws.selection_autoscroll_edge = selection_leave_edge(
                        ws.selection_dragged,
                        ws.cursor.x as f32,
                        ws.cursor.y as f32,
                        size.width as f32,
                        size.height as f32,
                    );
                }
                // Pointer left the window → drop the scrollbar hover so
                // the overlay bar relaxes to its dim state.
                ws.scrollbar_hover = false;
                ws.hovered_close_idx = None;
                ws.hovered_new_tab = false;
                ws.hovered_new_tab_menu = false;
                ws.search.hovered_control = None;
                // No card is under a pointer that left.
                ws.native_pointer = None;
                ws.card_hover = None;
                ws.card_settle_wake = None;
                if let Some(receipt) = ws.media_paste_receipt.as_mut() {
                    receipt.set_hover(false, std::time::Instant::now());
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorEntered { .. } => {
                let prev = std::mem::take(&mut ws.detach_drag);
                ws.detach_drag = prev.on_cursor_reenter_window();
                ws.selection_autoscroll_edge = 0;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                ws.cursor = position;
                ws.native_pointer = Some(position);
                ws.selection_autoscroll_edge = 0;
                // A press the lane took owns the pointer's motion until its
                // release: it resizes the lane or pans its item, and nothing
                // else sees it.
                if self.resize_preview_lane(ws) || self.drag_preview_lane(ws) {
                    self.sync_cursor_icon(ws);
                    return;
                }
                if ws.dragging_modal_editor && self.modal_text_pointer(ws, false) {
                    return;
                }
                self.update_search_hover(ws);
                if ws.selecting {
                    note_selection_drag_motion(ws);
                }
                // Client pointer events do NOT reach the torn window while the
                // OS moves it (Windows: NC modal loop; X11: the WM holds an
                // active pointer grab). The first CursorMoved after movement is
                // therefore the post-drop signal on platforms without Windows'
                // synthesized release (which arrives first there and wins).
                // Guards: a short post-handoff blackout absorbs a stray client
                // motion racing the WM actually taking the grab, and the latch
                // is REVALIDATED against the torn window's final resting
                // position — a WM-cancelled move (Esc) snaps the window back to
                // its origin, where frame+grab no longer hits the latched band,
                // so the cancel abandons instead of committing. Then fall
                // through — this event is real input for the now-free window.
                if let Some(td) = self.torn_drag.as_ref()
                    && td.seq == ws.seq
                    && td.native
                    && td.saw_move
                    && (!td.caption || !primary_button_physically_held())
                    && td.started.elapsed() >= std::time::Duration::from_millis(300)
                {
                    let commit = self.revalidate_dock_latch(ws);
                    self.finalize_torn_drag(ws, commit);
                    // A committed merge empties this window — it is closing;
                    // don't process the event against an empty mux.
                    if self.window_close_pending(ws.seq) {
                        return;
                    }
                }
                // Self-healing demotion. The drag_window() call succeeded as an
                // API call, but the WM never actually took the move (no Moved
                // has arrived) and the capture holder is still streaming
                // motion. An X11 WM that ignores _NET_WM_MOVERESIZE would
                // otherwise leave the torn window frozen mid-air with no path
                // to carry it.
                if let Some(td) = self.torn_drag.as_mut()
                    && td.native
                    && !td.saw_move
                    && td.carrier == ws.seq
                    && td.started.elapsed() >= std::time::Duration::from_millis(400)
                {
                    log::info!("tear-off: WM never took the drag; demoting to manual follow");
                    td.native = false;
                }
                // Manual-follow fallback. The native handoff failed, so the
                // capture holder (the tear's source window, or the dragged
                // window itself for a lone-tab whole-window drag) carries the
                // torn window. Reposition it from this cursor stream and drive
                // the re-dock hit-test. Gated on the CARRIER, so only the
                // capture holder's stream drives the follow. Without the gate,
                // stale tracking would hijack every window's cursor stream and
                // keep refreshing the failsafe forever.
                if self.follow_torn_drag(ws) {
                    return;
                }
                // An editor drag that started inside the search bar keeps the
                // motion; otherwise the grid above the bar stays live for
                // hover, selection, and mouse reporting, matching the ctl path.
                if search_pointer_route(ws.search.open, None, ws.search.pointer_captured())
                    == SearchPointerRoute::Bar
                {
                    self.show_mouse_cursor(ws);
                    self.sync_cursor_icon(ws);
                    self.search_mouse_drag(ws);
                    return;
                }
                // Dragging a split divider — recompute the
                // addressed split's ratio from the cursor and apply. The split
                // rect is re-fetched from the live seams so a mid-drag layout
                // change (a pane elsewhere closing) can't desync the math; if
                // the split has vanished, cancel the drag.
                if self.drag_split_divider(ws) {
                    return;
                }
                // Any real mouse movement undoes the hide-while-typing
                // state. Sub-pixel movements that winit *might* coalesce
                // are fine to ignore — the next "real" motion will fire.
                self.show_mouse_cursor(ws);
                let media_receipt_hovered = self.update_media_paste_receipt_hover(ws);
                self.sync_cursor_icon_with_media_receipt_hover(ws, media_receipt_hovered);
                // Terminator menu UX, hover-to-highlight: hovering a
                // context-menu row highlights it immediately, matching
                // GTK/NSMenu/Win32 menu conventions.
                if ws.context_menu.is_some() {
                    self.update_menu_highlight_from_cursor(ws);
                } else {
                    self.picker_pointer(ws, PickerPointer::Hover);
                }
                // Terminator parity, terminatorlib/config.py:73
                // `focus = sloppy`: focus-follows-mouse. The pane
                // under the cursor becomes focused on every cursor
                // movement (vs default `click` mode where click is
                // required). `system` is treated like `click` for
                // kettle — winit doesn't expose the OS-level focus
                // policy.
                if matches!(self.cfg.focus, kettle_config::FocusMode::Sloppy)
                    && !ws.tab_drag_active
                    && !ws.selecting
                    && ws.scrollbar_drag_offset.is_none()
                    // Don't let focus-follows-mouse
                    // reassign pane focus while a modal is open — typing into
                    // search/palette while the cursor drifts over another pane
                    // would otherwise silently steal focus.
                    && !self.any_modal_open(ws)
                {
                    let area = self.area(ws);
                    let pre = self.focus_key(ws);
                    ws.mux
                        .focus_at(area, ws.cursor.x as f32, ws.cursor.y as f32);
                    self.note_focus_change(ws, pre);
                }
                // Drag-to-reorder tabs (kitty / iTerm2 / Ghostty parity), on
                // every tab bar position: once a left press in the bar has
                // moved past the click slop, the active tab follows the
                // segment under the cursor.
                self.promote_tab_drag_if_needed(ws);
                if ws.tab_drag_active
                    && ws.tab_drag_press.is_none()
                    && self.reorder_active_tab_for_cursor(ws)
                    && let Some(w) = &ws.window
                {
                    w.request_redraw();
                }
                // Terminator parity: a titlebar press that has since moved far
                // enough is a pane drag. Recompute the drop target every move
                // from the CURRENT layout rather than caching it at press time
                // — a pane closing mid-drag reshuffles every rect, and a stale
                // target would highlight a pane that is no longer there.
                self.drag_pane(ws);
                // Drive the detach FSM from the press origin.
                // Distance decides click-vs-drag; WINDOW-BOUNDS CONTAINMENT
                // decides inside/outside — Windows' SetCapture keeps
                // streaming CursorMoved (with out-of-client coordinates)
                // while the button is held but suppresses CursorLeft, so
                // position is the reliable outside signal. The
                // CursorLeft/Entered arms above remain as supplementary
                // signals for platforms that do deliver them mid-drag.
                if let Some((ox, oy)) = ws.drag_press {
                    let (cx, cy) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    let next = std::mem::take(&mut ws.detach_drag).on_mouse_move(cx - ox, cy - oy);
                    let (sw, sh) = ws
                        .renderer
                        .as_ref()
                        .map(|r| {
                            let (w, h) = r.surface_size();
                            (w as f32, h as f32)
                        })
                        .unwrap_or((800.0, 600.0));
                    let outside = cx < 0.0 || cy < 0.0 || cx >= sw || cy >= sh;
                    ws.detach_drag = if outside {
                        next.on_cursor_leave_window()
                    } else {
                        next.on_cursor_reenter_window()
                    };
                    // Tear the moment the cursor crosses the band threshold,
                    // not at release (Chromium-style). The torn window appears
                    // live under the cursor and rides the OS move loop from
                    // here.
                    if self.maybe_tear_off(ws, event_loop) {
                        return;
                    }
                }
                {
                    // Motion. `MOUSE_NO_BUTTON` is xterm's code 3, which with
                    // the motion bit is the `CSI < 35 ; x ; y M` report DEC
                    // 1003 exists to deliver; `send_mouse` decides per mode
                    // whether this motion is reportable at all, so 1002 still
                    // needs a held button and 1000 still gets nothing.
                    if !self.pointer_modal_open(ws)
                        && self.send_mouse(ws, input::MOUSE_NO_BUTTON, true, true)
                    {
                        return;
                    }
                }
                if let Some(grab) = ws.scrollbar_drag_offset {
                    let area = self.area(ws);
                    let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    self.scrollbar_at(ws, area, px, py, false, Some(grab));
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Overlay-scrollbar hover-brighten. Update the focused
                // pane's hover flag and repaint only on the enter/leave edge so it
                // costs nothing while the pointer is elsewhere.
                {
                    let area = self.area(ws);
                    let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    let hover = self.scrollbar_zone_contains(ws, area, px, py);
                    if hover != ws.scrollbar_hover {
                        ws.scrollbar_hover = hover;
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                    }
                }
                if ws.selecting {
                    let area = self.area(ws);
                    self.update_selection(ws, area);
                }
                if (ws.selecting
                    || !ws.links.is_empty()
                    || ws.confirm_dialog.is_some()
                    || ws.preview_overlay.is_some())
                    && let Some(w) = &ws.window
                {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                ws.resume_native_pointer();
                // A fresh press while torn-drag tracking is live. For a NATIVE
                // drag any client press means the tracking is stale (the OS
                // modal loop / WM grab swallows presses mid-drag) — abandon,
                // never merge on a click. A LEFT press during manual-follow is
                // equally impossible mid-gesture (left is held the whole
                // drag) — stale, abandon. But an OTHER-button press routed to
                // the manual-follow capture holder is live mid-drag input:
                // swallow it (Chromium swallows stray presses during a tab
                // drag) instead of killing the gesture.
                if let Some(td) = self.torn_drag.as_ref() {
                    if td.native || button == MouseButton::Left {
                        self.finalize_torn_drag(ws, false);
                    } else if td.carrier == ws.seq {
                        return;
                    }
                }
                // Forward Back / Forward (buttons 8 / 9) to a mouse-tracking
                // app rather than dropping them. No local UI meaning, so they
                // no-op when tracking is off. The same `modal_swallows_pointer`
                // gate as L/M/R + wheel keeps SGR out of a tracking TUI
                // *behind* a palette/settings/… dialog. A lone context menu
                // isn't a modal here.
                // A press of any button is the pointer's: focus is no longer
                // on a card.
                if ws.card_focus.take().is_some() {
                    ws.accessibility_pending = true;
                }
                if let Some(sgr) = extra_mouse_sgr(button) {
                    // Any press ends a lane's drag or edge drag, as below.
                    self.end_lane_resize(ws);
                    ws.lane_drag = None;
                    // One away from a lane holding the keyboard gives it back.
                    self.leave_preview_focus_for_press(ws);
                    // A press of a button still noted as held means its
                    // release went elsewhere: its program gets it now. The
                    // press is Kettle's until a program receives it.
                    self.release_held_buttons(ws, |held| held.button == sgr);
                    ws.held_buttons.pressed(sgr, None, (0, 0));
                    if self.confirm_mouse_press(ws, event_loop, sgr) {
                        return;
                    }
                    if self.dismiss_modal_outside(ws, sgr) {
                        return;
                    }
                    // A lone context menu must swallow the side-button press
                    // too. Dismiss the menu and DON'T forward.
                    // `modal_swallows_pointer` returns false for a lone menu,
                    // so without this a Back/Forward click would leak SGR to
                    // the tracking app *behind* the menu AND leave the menu
                    // open (every other button dismisses it).
                    if ws.context_menu.is_some() {
                        ws.context_menu = None;
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    // A preview lane owns every press over it; a side button
                    // does nothing there.
                    let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    if !self.press_preview_lane(ws, px, py, sgr)
                        && !modal_swallows_pointer(
                            self.pointer_modal_open(ws),
                            ws.context_menu.is_some(),
                        )
                    {
                        self.report_mouse(ws, sgr, true, false);
                    }
                    return;
                }
                let bcode = match button {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => return,
                };
                // Any press while a lane's drag is held means that drag's
                // release went elsewhere, or another gesture takes the
                // pointer: the drag is over. Likewise a primary press while a
                // card still holds one: the card's gesture is over.
                self.end_lane_resize(ws);
                ws.lane_drag = None;
                self.leave_preview_focus_for_press(ws);
                if bcode == 0 {
                    ws.card_press = None;
                }
                // Likewise a program still noted as holding this button gets
                // the release it missed, and the press is Kettle's own
                // gesture until a program receives it.
                self.release_held_buttons(ws, |held| held.button == bcode);
                ws.held_buttons.pressed(bcode, None, (0, 0));
                if self.confirm_mouse_press(ws, event_loop, bcode) {
                    return;
                }
                if self.dismiss_modal_outside(ws, bcode) {
                    return;
                }
                // Context menu: if the menu is open, a left-click
                // either fires the row that was hit or — if the click landed
                // outside the panel — closes the menu. Middle-click outside
                // dismisses too; only right-click falls through so it can
                // reopen/relocate the menu.
                if bcode == 0 && self.modal_text_pointer(ws, true) {
                    return;
                }
                if bcode == 0 && self.picker_pointer(ws, PickerPointer::Press) {
                    return;
                }
                if bcode == 0
                    && let Some(delta) = self.context_menu_scroll_edge(ws)
                {
                    self.scroll_context_menu(ws, delta);
                    return;
                }
                if ws.context_menu.is_some()
                    && let Some(click) = self.context_menu_click_action(ws, bcode)
                {
                    // Route through the shared sink. It closes the menu per
                    // leaf and keeps it open for DrillIntoSubmenu, so clearing
                    // `ws.context_menu` before the dispatch would turn submenu
                    // clicks into plain dismissals.
                    self.dispatch_context_menu_click(ws, click, event_loop);
                    return;
                }
                if ws.context_menu.is_some() && bcode != 2 {
                    // Left/middle outside the panel — dismiss without firing
                    // anything (matches every modern menu).
                    ws.context_menu = None;
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Search controls are real click targets inside the reserved
                // lane; every button inside the lane stays there. A press above
                // the lane is ordinary grid input: the bar is not an overlay,
                // so selecting, opening links, and mouse reporting keep working
                // while a query is being typed (Terminator parity).
                if search_pointer_route(
                    ws.search.open,
                    Some(self.cursor_in_search_bar(ws)),
                    ws.search.dragging_editor,
                ) == SearchPointerRoute::Bar
                {
                    if bcode == 0 {
                        self.search_mouse_press(ws);
                        if let Some(window) = &ws.window {
                            window.request_redraw();
                        }
                    }
                    return;
                }
                // The settings overlay is mouse-driven (click-to-cycle).
                // Left-click a field cycles its value forward, right-click
                // backward; clicking a category tab switches category; a click
                // outside the panel closes settings. Handled before the generic
                // modal swallow below so the click actually does something.
                if ws.settings_nav.is_some()
                    && (bcode == 0 || bcode == 2)
                    && self.settings_mouse(ws, if bcode == 2 { -1 } else { 1 }, true)
                {
                    return;
                }
                // With any *other* modal open
                // (palette / ssh / settings / layout-picker / hint / confirm
                // dialog / inline title-edit / vi copy-mode) the click must be
                // consumed. Otherwise it falls through to the
                // tab-bar / pane-focus / mouse-tracking logic below, switching
                // tabs and injecting mouse events into the terminal *behind* a
                // dialog that looks like it has focus. The context menu is
                // excluded (handled + returned above; a right-click below
                // relocates it), and so is the search bar, whose lane was
                // routed above and whose grid stays live.
                if ws.preview_overlay.is_some() && !self.non_search_modal_open(ws) {
                    self.press_preview_lane(ws, ws.cursor.x as f32, ws.cursor.y as f32, bcode);
                    return;
                }
                if modal_swallows_pointer(self.pointer_modal_open(ws), ws.context_menu.is_some()) {
                    return;
                }
                let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                // A preview lane owns every press over it, so the terminal
                // beside it never gets one.
                if self.press_update_toast(ws, px, py, bcode) {
                    return;
                }
                if self.press_preview_lane(ws, px, py, bcode) {
                    return;
                }
                // An inline card takes a primary press before anything else
                // in the window does; its release opens the card's item.
                if self.press_card(ws, px, py, bcode) {
                    return;
                }
                // Tab-bar interactions (left = switch / close-✕ / new-+;
                // middle = close that tab).
                let bar = self.tab_bar(ws);
                let in_bar = |r: kettle_render::Rect4, px: f32, py: f32| {
                    px >= r.0 && px < r.0 + r.2 && py >= r.1 && py < r.1 + r.3
                };
                if tab_bar_pointer_region_contains(&bar, px, py) && (bcode == 0 || bcode == 1) {
                    if bcode == 0 && bar.new_tab_menu.2 > 0.0 && in_bar(bar.new_tab_menu, px, py) {
                        // The `▾` dropdown — open the shell chooser
                        // anchored at the arrow's bottom-left. Checked BEFORE the
                        // `+` so the arrow region isn't swallowed by the button.
                        let (ax, ay, _, ah) = bar.new_tab_menu;
                        self.open_new_tab_menu(ws, ax, ay + ah);
                    } else if bcode == 0 && in_bar(bar.new_tab, px, py) {
                        let area = self.area(ws);
                        let (cols, rows) = self.grid_of(ws, area);
                        let geometry = self.pty_geometry_for_grid(ws, cols, rows);
                        // Log a `+`-button new-tab spawn failure rather than
                        // swallowing it, and fire TabAdd on success so Lua
                        // `TabAdd` and dev-record see the new tab.
                        let pre = self.focus_key(ws);
                        match ws.mux.new_tab_geometry(&self.cfg, geometry, self.waker()) {
                            Ok(()) => {
                                self.note_focus_change(ws, pre);
                                self.fire_tab_add_event(ws);
                            }
                            Err(e) => log::warn!("could not open a new tab (+ button): {e}"),
                        }
                    } else if bcode == 0
                        && bar.scroll_left.2 > 0.0
                        && in_bar(bar.scroll_left, px, py)
                    {
                        // `‹` overflow arrow — step to an earlier tab
                        // (scrolls it into view via tab_strip_layout).
                        let pre = self.focus_key(ws);
                        ws.mux.prev_tab();
                        self.note_focus_change(ws, pre);
                    } else if bcode == 0
                        && bar.scroll_right.2 > 0.0
                        && in_bar(bar.scroll_right, px, py)
                    {
                        let pre = self.focus_key(ws);
                        ws.mux.next_tab();
                        self.note_focus_change(ws, pre);
                    } else if let Some(seg) = bar.segments.iter().find(|s| in_bar(s.rect, px, py)) {
                        let close = bcode == 1 || in_bar(seg.close, px, py);
                        if close {
                            // Both gestures that land here — the ✕ button and
                            // middle-click — destroy as much as
                            // `Action::CloseTab` and so must ask the same
                            // question. They can also target a tab that is NOT
                            // the focused one, so the prompt names the tab by a
                            // pane it holds rather than by the focused index.
                            let (panes, busy) = tab_close_scope(ws, seg.idx);
                            if let Some(anchor) =
                                Some(ws.mux.tab_anchor_panes(seg.idx)).filter(|a| !a.is_empty())
                                && self.confirm_close(
                                    ws,
                                    panes,
                                    busy,
                                    ConfirmAction::CloseTabHolding(anchor),
                                )
                            {
                                return;
                            }
                            // Closing a tab (middle-click or
                            // ✕) can shift focus to a different tab
                            // (`reap_tabs`'s bookkeeping).
                            // Treat it like any other focus-changing
                            // action so the cursor on the now-active
                            // tab lands visible immediately.
                            let pre = self.focus_key(ws);
                            // Fire TabClose so plugins see
                            // the ✕-click close the same as Action::CloseTab.
                            let closing_idx = seg.idx;
                            if ws.mux.close_tab_at(seg.idx) {
                                // Save the (empty) session
                                // before exit so next launch starts
                                // fresh rather than restoring the
                                // *previous* multi-tab state. Other
                                // exit paths (Action::CloseTab on the
                                // last tab, WindowEvent::CloseRequested)
                                // save too.
                                self.fire_tab_close_event(ws, closing_idx);
                                self.save_session(ws);
                                self.request_window_close(ws.seq);
                                return;
                            }
                            self.fire_tab_close_event(ws, closing_idx);
                            self.note_focus_change(ws, pre);
                        } else {
                            let pre = self.focus_key(ws);
                            ws.mux.active = seg.idx;
                            ws.mux.touch_active_tab_seen();
                            self.note_focus_change(ws, pre);
                            // Arm the drag-to-reorder
                            // handler so a subsequent CursorMoved
                            // event with the left button still held
                            // can swap the active tab toward the
                            // cursor. Cleared in the Released arm
                            // below. Only on bare left-click (bcode 0
                            // == left, not middle / close).
                            if bcode == 0 {
                                ws.tab_drag_active = true;
                                ws.tab_drag_press = Some((px, py));
                                ws.tab_pressed_idx = Some(seg.idx);
                                // Arm the detach FSM alongside the in-window
                                // reorder, for a LONE tab too. Dragging it past
                                // the band threshold moves the whole window
                                // (Chromium semantics), which is how a torn-off
                                // window re-docks into a sibling.
                                if self.cfg.detachable_tabs {
                                    ws.detach_drag =
                                        crate::detach::DragState::on_mouse_down_on_tab(seg.idx);
                                    ws.drag_press = Some((px, py));
                                } else {
                                    ws.detach_drag = crate::detach::DragState::default();
                                    ws.drag_press = None;
                                }
                            }
                        }
                    }
                    self.resize_all(ws);
                    if let Some(win) = &ws.window {
                        win.request_redraw();
                    }
                    return;
                }
                let area = self.area(ws);
                // The receipt is chrome over terminal cells. The primary
                // button acts; every other button is consumed without acting
                // so hidden terminal content cannot receive the press.
                if self.activate_media_paste_receipt_at(ws, px, py, bcode) {
                    return;
                }
                // The detached card covers terminal cells but is not a shell
                // editor. A press dismisses it and stops here so selecting text,
                // opening a link, or forwarding mouse tracking cannot act on
                // content the card hid.
                let completion_hit = {
                    let overlay = self.overlay(ws, true);
                    completion_overlay_hit_test(
                        overlay.completion.as_ref(),
                        self.menu_cell(ws),
                        (px, py),
                    )
                };
                if completion_hit {
                    if let Some(pane_id) = ws.mux.active_focus()
                        && let Some(pane) = ws.mux.panes.get(&pane_id)
                    {
                        pane.term.clear_completion();
                    }
                    if let Some(window) = &ws.window {
                        window.request_redraw();
                    }
                    return;
                }
                // Ctrl/Cmd + left-click opens a hyperlink under the cursor.
                //
                // Terminator parity, terminatorlib/config.py:120
                // `link_single_click`: when true, single-click (no
                // modifier) is enough to open URLs. Default keeps
                // kettle's Ctrl-click guard so accidental drags don't
                // navigate.
                let url_modifier =
                    self.cfg.link_single_click || ws.mods.control_key() || ws.mods.super_key();
                if bcode == 0
                    && url_modifier
                    && let Some(uri) = self.link_at_cursor(ws).map(|l| l.uri.clone())
                {
                    // Route through helper so the pane's origin and the custom
                    // URL handler config are honored.
                    self.open_pane_link(ws, links_pane(ws), &uri);
                    return;
                }
                // As in Terminator, a left-click on a pane titlebar focuses the
                // pane, and a second click opens the EditPaneTitle overlay. The
                // two-click model (focus first, edit second) avoids accidental
                // title edits on focus transitions.
                //
                // The titlebar is also the grab handle for dragging the pane
                // elsewhere in the tab, so the press only FOCUSES and arms the
                // gesture; the release decides between editing the title and
                // moving the pane. Opening the editor on press would put a text
                // field over the pane the user just picked up.
                if self.press_pane_drag(ws, bcode) {
                    return;
                }
                // A left-press on a split divider seam starts
                // a drag-to-resize gesture rather than focusing a pane / starting
                // a selection. Seams are hit-tested over the same content `area`
                // the panes lay out in, with a small grab tolerance.
                if self.press_split_divider(ws, bcode) {
                    return;
                }
                let pre = self.focus_key(ws);
                ws.mux
                    .focus_at(area, ws.cursor.x as f32, ws.cursor.y as f32);
                self.note_focus_change(ws, pre);
                if self.send_mouse(ws, bcode, true, false) {
                    return;
                }
                // Middle-click in the content area pastes the PRIMARY
                // selection when the platform exposes one; otherwise it falls
                // back to the regular clipboard.
                //
                // Mirrors Terminator's `disable_mouse_paste`
                // (terminatorlib/config.py:88): when true, middle-click does
                // not paste. Useful for terminal-of-last-resort use
                // cases where accidental middle-clicks shouldn't leak
                // clipboard content into commands.
                if bcode == 1 && !self.cfg.disable_mouse_paste {
                    self.paste_primary(ws);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Mirrors Terminator's `putty_paste_style`
                // (terminatorlib/config.py:89): right-click pastes (PuTTY/Windows
                // convention) instead of opening the context menu. The
                // companion `putty_paste_style_source_clipboard` decides
                // whether the source is CLIPBOARD or PRIMARY.
                if bcode == 2 && self.cfg.putty_paste_style {
                    match putty_paste_source(self.cfg.putty_paste_style_source_clipboard) {
                        PasteSource::Clipboard => self.paste_clipboard(ws),
                        PasteSource::Primary => self.paste_primary(ws),
                    }
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Click the scrollbar to jump the viewport, then drag it.
                // Keep this behind pane gestures and terminal mouse reporting;
                // the receipt excludes the same gutter in its own hit test.
                if bcode == 0
                    && let Some(grab) = self.scrollbar_jump(ws, area, px, py)
                {
                    ws.scrollbar_drag_offset = Some(grab);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Right-click handling, layered:
                //
                // 1. `Shift + right-click` *with* selected text, off it, keeps
                //    the extend-selection behavior (xterm convention; muscle
                //    memory for kettle's power users).
                // 2. Any other right-click opens the context menu at the
                //    click point (Terminator / GNOME / iTerm2 default),
                //    Shift+right-click on the selection or with nothing
                //    selected included, so its rows can act on them.
                //
                // Mouse-tracking already short-circuited above when the
                // focused program is consuming mouse events, so this only
                // fires for the kettle chrome; Shift bypasses tracking, so
                // Shift+right-click always reaches it.
                if bcode == 2 {
                    if ws.mods.shift_key()
                        && self.pointer_on_selection(ws, area) == Some(false)
                        && self.extend_selection_to_cursor(ws, area, bcode)
                    {
                        if self.cfg.copy_on_select {
                            self.copy_selection(ws);
                        }
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    let offer_copied = ws.mods.shift_key();
                    self.open_context_menu(ws, px, py, offer_copied);
                    return;
                }
                if bcode == 0 {
                    self.begin_or_extend_mouse_selection(ws, area);
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button,
                ..
            } => {
                ws.resume_native_pointer();
                // Release report for the side buttons, so a tracking app sees
                // the matching button-up after the press. Only a press the
                // app saw gets one: a press that a context menu, a modal or
                // the media viewer took leaves no release behind, even when
                // that surface closed while the button was held.
                if let Some(sgr) = extra_mouse_sgr(button) {
                    ws.held_buttons.end_own(sgr);
                    if self.confirm_mouse_release(ws, sgr) {
                        return;
                    }
                    self.send_mouse(ws, sgr, false, false);
                    return;
                }
                let bcode = match button {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => return,
                };
                // A gesture of Kettle's own ends with this release, whichever
                // handler below takes it; a program's goes on to get its
                // release from `report_mouse`.
                ws.held_buttons.end_own(bcode);
                // End any split-divider drag on left-button up.
                if bcode == 0 {
                    ws.dragging_split = None;
                }
                if self.confirm_mouse_release(ws, bcode) {
                    return;
                }
                // A release ends an editor drag inside the search bar; any
                // other release while the bar is open belongs to the grid
                // gesture it started (selection end, copy-on-select, mouse
                // reporting), like the ctl path.
                if bcode == 0
                    && search_pointer_route(ws.search.open, None, ws.search.pointer_captured())
                        == SearchPointerRoute::Bar
                {
                    ws.search.dragging_editor = false;
                    self.search_button_release(ws);
                    return;
                }
                // The release of a press a lane took is the lane's.
                if bcode == 0 && ws.lane_resize.is_some() {
                    self.end_lane_resize(ws);
                    self.sync_cursor_icon(ws);
                    return;
                }
                if bcode == 0
                    && let Some(drag) = ws.lane_drag.take()
                {
                    self.end_lane_drag(ws, drag);
                    self.sync_cursor_icon(ws);
                    return;
                }
                // The release of a press a card took is the card's too.
                let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                if self.release_card(ws, px, py, bcode) {
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Terminator parity: the release is where a titlebar press
                // finally means something. Moved past the slop -> move the pane
                // to the latched drop target; never moved -> the
                // second-click-renames behaviour the press deferred.
                //
                // FIRST among the left-release handlers, ahead of the tear-off
                // and mouse-reporting branches that can return early. Those are
                // all mutually exclusive with a pane drag today -- the titlebar
                // press returns before any button is noted as held -- but "today"
                // is exactly the kind of reasoning that leaves a gesture armed
                // when one of them grows a new early return.
                if bcode == 0 && std::mem::take(&mut ws.dragging_modal_editor) {
                    return;
                }
                if self.release_pane_drag(ws, bcode, event_loop) {
                    return;
                }
                // A left-release while a torn window is tracked is the DROP.
                // Two shapes: (a) the synthesized release winit posts to the
                // TORN window when the Windows modal move loop exits
                // (WM_EXITSIZEMOVE → WM_LBUTTONUP — verified in the vendored
                // 0.30.13 source); (b) the real release on the capture-holding
                // SOURCE in manual-follow. Commit the latched dock either way.
                // A left-release on an unrelated window while tracking is live
                // means the tracking went stale (an X11 drop we never
                // observed) — abandon it and process the release normally.
                //
                // Esc-cancel guard: WM_EXITSIZEMOVE
                // fires for EVERY modal-loop exit — Esc-cancel included —
                // and the synthesized release is indistinguishable from a
                // drop, while the latch survives the snap-back (the live
                // cursor is still over the band). The tell is PHYSICAL
                // button state: an Esc-cancel exits the loop with the
                // primary button still held; a real drop only exits after
                // it went up. Held ⇒ abandon (the user's real release later
                // arrives with tracking already cleared and flows through
                // normal processing harmlessly).
                if bcode == 0
                    && let Some(td) = self.torn_drag.as_ref()
                {
                    if td.seq == ws.seq || !td.native {
                        let commit = !primary_button_physically_held();
                        let commit = commit && (!td.caption || self.revalidate_dock_latch(ws));
                        self.finalize_torn_drag(ws, commit);
                        return;
                    }
                    self.finalize_torn_drag(ws, false);
                }
                // The release goes where its press did, if a program got it.
                if self.send_mouse(ws, bcode, false, false) {
                    return;
                }
                if selection_release_matches(ws.selection_button, bcode) {
                    self.finish_selection_gesture(ws);
                }
                if bcode == 0 {
                    ws.search.dragging_editor = false;
                    ws.scrollbar_drag_offset = None;
                    // End the drag-to-reorder gesture on
                    // left-button release. Any swaps that happened
                    // during the drag are already committed; this just
                    // disarms the CursorMoved handler.
                    ws.tab_drag_active = false;
                    ws.tab_drag_press = None;
                    ws.tab_pressed_idx = None;
                    // On Wayland only, a release while the detach FSM is
                    // OUTSIDE the window tears the dragged tab off into a new
                    // window. Everywhere else the tear fires the moment the
                    // cursor crosses the band threshold (`maybe_tear_off`), so
                    // the FSM is already Idle by release, and a release still
                    // inside the hysteresis slop deliberately does NOT tear
                    // (Chromium's within-slop = click/reorder semantics).
                    // Wayland can't position windows client-side nor hand off a
                    // drag to a surface that never saw the press, so it keeps
                    // the at-release tear.
                    let dropped_outside = matches!(
                        ws.detach_drag,
                        crate::detach::DragState::DraggingOutside { .. }
                    );
                    ws.detach_drag = std::mem::take(&mut ws.detach_drag).on_mouse_up();
                    ws.drag_press = None;
                    let wayland = ws.window.as_deref().is_some_and(window_is_wayland);
                    if self.cfg.detachable_tabs
                        && dropped_outside
                        && wayland
                        && ws.mux.tabs.len() > 1
                    {
                        let closing_idx = ws.mux.active;
                        self.release_held_buttons_in_tab(ws, closing_idx);
                        if let Some(dt) = ws.mux.detach_tab(closing_idx) {
                            // `outer_position` errs on Wayland — `None` lets
                            // the compositor place the window.
                            let pos = ws
                                .window
                                .as_ref()
                                .and_then(|w| w.outer_position().ok())
                                .map(|p| {
                                    winit::dpi::PhysicalPosition::new(
                                        p.x + ws.cursor.x as i32,
                                        p.y + ws.cursor.y as i32,
                                    )
                                    .into()
                                });
                            self.release_focus_report_to(ws, &dt);
                            let paste =
                                ws.take_moved_paste_state(&dt.tab, std::time::Instant::now());
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
                                    None,
                                )
                                .map_err(|open| *open)
                            {
                                Ok(torn_seq) => {
                                    self.fire_tab_close_event(ws, closing_idx);
                                    // Agents see the tear-off too.
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
                                }
                                Err(WindowOpen::AdoptTab { tab: dt, paste, .. }) => {
                                    log::warn!(
                                        "tear-off: open_window failed; tab kept in source window"
                                    );
                                    ws.mux.attach_tab(dt, Some(closing_idx));
                                    ws.restore_moved_paste_state(*paste);
                                }
                                Err(_) => {
                                    unreachable!("open_window returns the WindowOpen it was given")
                                }
                            }
                        }
                    }
                }
            }
            // A pinch over a preview lane zooms its rendered item at the
            // pointer; anywhere else it does nothing.
            WindowEvent::PinchGesture { delta, .. } => {
                ws.resume_native_pointer();
                if self.lane_takes_wheel(ws) {
                    self.pinch_over_lane(ws, delta);
                }
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                ws.resume_native_pointer();
                // A gesture ending (macOS momentum scroll, and any backend that
                // reports phases) drops leftover sub-detent residue so a partial
                // step can't leak into the next, unrelated gesture.
                if matches!(
                    phase,
                    winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                ) {
                    ws.wheel.reset();
                    ws.lane_wheel.reset();
                }
                if ws.confirm_dialog.is_some() {
                    ws.lane_wheel.reset();
                    let steps = ws.wheel.feed(&delta, self.cfg.scroll_multiplier);
                    self.confirm_wheel(ws, steps.lines);
                    return;
                }
                // A preview lane owns the wheel over it. Its motion is dropped
                // from the terminal's account rather than banked there, so no
                // part of it reaches the terminal once the lane closes; the
                // lane keeps its own, to scroll a source. Motion over the
                // terminal is dropped from the lane's the same way.
                if self.lane_takes_wheel(ws) {
                    ws.wheel.reset();
                    self.wheel_over_lane(ws, &delta);
                    return;
                }
                ws.lane_wheel.reset();
                let steps = ws.wheel.feed(&delta, self.cfg.scroll_multiplier);
                // Numbers only — never terminal content. Makes the
                // precision-touchpad class of bug diagnosable from a user's
                // `RUST_LOG=debug` run instead of requiring a custom build.
                log::debug!(
                    "wheel: delta={delta:?} phase={phase:?} -> notches={} lines={} cols={}",
                    steps.notches,
                    steps.lines,
                    steps.cols
                );
                if steps.is_zero() {
                    // Motion too small to move anything yet — it is banked in
                    // `ws.wheel`, not discarded.
                    return;
                }
                self.dispatch_wheel(ws, steps);
            }
            WindowEvent::DroppedFile(path) => {
                // A drop is the pointer's, as a press elsewhere: the keyboard
                // goes back to the terminal first.
                self.exit_preview_focus(ws);
                // A file dropped while a modal (search /
                // palette / settings / confirm dialog / inline title-edit / vi
                // copy-mode / …) is open must NOT inject its path into the PTY
                // behind the dialog — the same pointer-leak class
                // `modal_swallows_pointer` already closes for clicks. Unlike
                // that gate, an open context menu blocks the drop too.
                if self.any_modal_open(ws) {
                    return;
                }
                // Insert a shell-quoted path plus a trailing space. The shared
                // paste path preserves bracketed paste, per-pane shell
                // formatting, broadcast scope, and WSL translation.
                let paths = vec![path];
                let video = self.cfg.paste_video_preview.then(|| {
                    crate::video_preview::VideoPasteRequest::from_user_paths(
                        &paths,
                        crate::video_preview::VideoPasteSource::Drop,
                    )
                });
                self.paste_paths(ws, paths, true, video.flatten());
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Focused(f) => {
                ws.window_focused = f;
                ws.os_focus_known = true;
                if f {
                    self.update_toast_unavailable = None;
                    ws.frame_recovery.expedite(std::time::Instant::now());
                    // Another process may have installed or dismissed an
                    // update while this one was in the background.
                    self.request_update_facts(false);
                }
                // Track the OS focus in `focused_seq` so window-less events
                // (RemoteCommand, ctl "focused-window" ops, update notices)
                // and the agent-facing focused_window/to_window JSON
                // route to the window the user actually clicked — not whichever
                // window was last opened or ctl-focused. `ws.seq` is this
                // window's BTreeMap key; finish_window_dispatch re-inserts it
                // after dispatch.
                if f {
                    self.focused_seq = ws.seq;
                }
                if !f {
                    self.confirm_cancel_press(ws);
                    ws.confirm_pointer_release = false;
                    // An unfocused window draws no cursor, and the Metal layer
                    // beneath already shows the off phase.
                    Self::hide_cursor_layer(ws);
                    // Its release may never come here.
                    ws.lane_drag = None;
                    self.end_lane_resize(ws);
                    // The keyboard goes back to the terminal.
                    self.exit_preview_focus(ws);
                }
                // Non-interactive UI-state marker (OS-driven focus
                // change — a transition the PTY output stream can't show).
                if let Some(rec) = self.recorder.as_mut() {
                    rec.record_marker(if f {
                        "kettle:focus_in"
                    } else {
                        "kettle:focus_out"
                    });
                }
                // Broadcast Tab arms every target pane, including named-group
                // targets in other windows. Disarm the whole in-process fleet
                // together so a reply buffered before this focus boundary
                // cannot reopen when any sibling is focused later. The next
                // Tab explicitly arms a fresh generation.
                let completion_panes = ws.mux.panes.values().chain(
                    self.windows
                        .values()
                        .flat_map(|window| window.mux.panes.values()),
                );
                clear_completions_on_focus_loss(f, completion_panes, |pane| {
                    pane.term.invalidate_completion();
                });
                // A focus loss can swallow the button-UP that
                // ends an in-progress drag (the release lands on whatever window
                // took focus), latching `selecting` / `scrollbar_drag_offset` /
                // `tab_drag_active` / a held button. The next CursorMoved
                // then kept extending the selection or dragging a tab with no
                // button down. Disarm them on focus loss, committing any pending
                // copy-on-select first (mirrors the left-button-up path).
                if !f {
                    // The compositor may deliver held-key releases to the
                    // newly focused window. Drop UI suppression state here so
                    // a later unrelated release cannot be mistaken for the
                    // missing half of an old shortcut.
                    ws.suppressed_key_releases.clear();
                    let mut releases = Vec::new();
                    release_terminal_owned_keys(ws, |pane, key, bytes| {
                        releases.push((pane, key, bytes))
                    });
                    self.send_or_defer_key_releases(ws, releases);
                    ws.closing_keys.clear();
                    ws.ime_preedit = None;
                    ws.ime_focus_generation = ws.ime_focus_generation.wrapping_add(1);
                    ws.search.dragging_editor = false;
                    ws.dragging_modal_editor = false;
                    ws.context_menu = None;
                    ws.search.pressed_control = None;
                    self.finish_selection_gesture(ws);
                    ws.scrollbar_drag_offset = None;
                    ws.scrollbar_hover = false;
                    ws.tab_drag_active = false;
                    ws.tab_drag_press = None;
                    ws.tab_pressed_idx = None;
                    // A focus loss also ends any split-divider drag.
                    ws.dragging_split = None;
                    // Releases now land in another window: each program
                    // with a held button's press gets its release here.
                    self.release_held_buttons(ws, |_| true);
                    // And a card's press: its release may land elsewhere.
                    ws.card_press = None;
                    // A focus loss also cancels an in-flight tab tear-off
                    // (the release will land on whatever window took focus).
                    ws.detach_drag = crate::detach::DragState::default();
                    ws.drag_press = None;
                    ws.pane_drag = None;
                    if let Some(receipt) = ws.media_paste_receipt.as_mut()
                        && receipt.set_hover(false, std::time::Instant::now())
                    {
                        ws.accessibility_pending = true;
                    }
                    // Torn-drag tracking deliberately survives
                    // this disarm — the tear itself moves OS focus to the
                    // torn window (firing Focused(false) on the source), and
                    // manual-follow rides the source's mouse CAPTURE, which
                    // outlives focus. Stale tracking is cleaned by the
                    // press/release handlers + the about_to_wait failsafe.
                }
                // Mirrors Terminator's `hide_on_lose_focus`
                // (terminatorlib/config.py:77): Quake-style auto-hide. When
                // the user clicks away to another window, hide the
                // kettle window. Reappears via `kettle --toggle`
                // or whatever global hotkey the user
                // bound. Honors only on focus-LOSS (f == false).
                if !f
                    && self.cfg.hide_on_lose_focus
                    && let Some(w) = &ws.window
                {
                    w.set_visible(false);
                }
                // Route through the shared helper so all
                // user-driven blink-reset paths share one implementation. The
                // CursorBlinkingChange handler still inlines the body
                // because it runs inside `ws.mux.panes.values_mut()`
                // and can't borrow `self` again — that one's documented.
                self.reset_blink_phase(ws);
                // Focus-event reporting (DEC private mode ?1004) follows
                // `window_focused` in `sync_pane_focus_reports`, which runs
                // when this dispatch finishes and also covers focus moving
                // between panes and tabs.
                // winit's `request_user_attention(None)` alone does
                // not reliably stop the Win11 taskbar flash once started, so
                // when an attention request is outstanding, clear it directly
                // (FlashWindowEx FLASHW_STOP via Taskbar) on focus-gain and
                // reset the tracker.
                if f && ws.attention_active {
                    ws.attention_active = false;
                    if let Some(w) = &ws.window {
                        w.request_user_attention(None);
                        ws.taskbar.clear_attention(w);
                    }
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Occluded(occluded) => {
                // Suppress animation wakes while occluded; repaint on return.
                ws.window_occluded = occluded;
                if occluded {
                    Self::hide_cursor_layer(ws);
                }
                if !occluded {
                    ws.frame_recovery.expedite(std::time::Instant::now());
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::Ime(ime) => {
                // Text through an input method returns focus from a card to
                // its pane, as a key press does; the method handles those
                // presses, so none arrives.
                let typing = match &ime {
                    winit::event::Ime::Preedit(text, _) => !text.is_empty(),
                    winit::event::Ime::Commit(_) => true,
                    _ => false,
                };
                if typing && ws.card_focus.take().is_some() {
                    ws.accessibility_pending = true;
                }
                // A preview lane holding the keyboard takes no text: what an
                // input method composes or commits meanwhile goes nowhere.
                if ws.preview_focus.is_some()
                    && matches!(
                        ime,
                        winit::event::Ime::Preedit(..) | winit::event::Ime::Commit(_)
                    )
                {
                    return;
                }
                let redraw = match ime {
                    winit::event::Ime::Enabled => {
                        if let Some(window) = &ws.window {
                            window.set_ime_allowed(ws.preview_focus.is_none());
                        }
                        self.update_ime_cursor_area(ws);
                        false
                    }
                    winit::event::Ime::Preedit(text, selection) => {
                        if !update_ime_preedit(ws, text, selection) {
                            return;
                        }
                        self.update_ime_cursor_area(ws);
                        true
                    }
                    winit::event::Ime::Commit(text) => {
                        let redraw = ws.ime_preedit.is_some() || !text.is_empty();
                        if take_ime_commit(ws) {
                            self.commit_ime_text(ws, &text, event_loop);
                        }
                        redraw
                    }
                    winit::event::Ime::Disabled => {
                        ws.ime_preedit_owner = None;
                        ws.ime_preedit.take().is_some()
                    }
                };
                if redraw && let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // A key press returns focus from a card that assistive
                // technology focused to the pane, where the key goes.
                if event.state == ElementState::Pressed && ws.card_focus.take().is_some() {
                    ws.accessibility_pending = true;
                }
                let toast_owns_key = update_toast::owns_key(
                    self.update_toast_focus.is_some() && self.update_toast_projection(ws).is_some(),
                    &event.logical_key,
                    ws.mods,
                );
                let ui_owns_key = toast_owns_key
                    || !matches!(ws.detach_drag, crate::detach::DragState::Idle)
                    || ws.context_menu.is_some()
                    || ws.vi_mode.is_some()
                    || ws.hint_state.is_some()
                    || ws.palette_input.is_some()
                    || ws.settings_nav.is_some()
                    || ws.layout_picker_input.is_some()
                    || ws.theme_picker_input.is_some()
                    || ws.ssh_input.is_some()
                    || ws.confirm_dialog.is_some()
                    || ws.editing_title.is_some()
                    || ws.search.open
                    || ws.preview_focus.is_some()
                    || ws.ime_preedit.is_some();
                // The modal that gets this press, if any; a press after which
                // it is no longer the top modal closed or replaced it.
                let key_modal = top_modal(ws);
                if drop_closing_key_repeat(
                    &mut ws.closing_keys,
                    event.physical_key,
                    event.state,
                    event.repeat,
                ) {
                    return;
                }
                let release_consumed = track_consumed_key_release(
                    &mut ws.suppressed_key_releases,
                    &mut ws.terminal_owned_key_releases,
                    event.physical_key,
                    event.state,
                    ui_owns_key,
                );
                if event.state == ElementState::Released
                    && ws.confirm_dialog.is_some()
                    && confirmation_space_key(&event.logical_key)
                {
                    self.confirm_space_release(ws, release_consumed);
                    if release_consumed {
                        return;
                    }
                }
                if release_consumed {
                    return;
                }
                if event.state == ElementState::Released {
                    self.write_terminal_key_event(ws, &event, false);
                    return;
                }
                if !toast_owns_key && self.update_toast_focus.take().is_some() {
                    ws.accessibility_pending = true;
                    if let Some(window) = &ws.window {
                        window.request_redraw();
                    }
                }
                if toast_owns_key {
                    update_toast::guard_activation(
                        &event.logical_key,
                        event.physical_key,
                        &mut ws.closing_keys,
                    );
                    match event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.dismiss_update_toast(ws);
                            return;
                        }
                        Key::Named(NamedKey::Tab) => {
                            self.update_toast_focus = Some(
                                if self.update_toast_focus == Some(ACCESSIBILITY_UPDATE_DETAILS_ID)
                                {
                                    ACCESSIBILITY_UPDATE_DISMISS_ID
                                } else {
                                    ACCESSIBILITY_UPDATE_DETAILS_ID
                                },
                            );
                            ws.accessibility_pending = true;
                            if let Some(window) = &ws.window {
                                window.request_redraw();
                            }
                            return;
                        }
                        key if confirmation_space_key(&key)
                            || matches!(key, Key::Named(NamedKey::Enter)) =>
                        {
                            let details =
                                self.update_toast_focus == Some(ACCESSIBILITY_UPDATE_DETAILS_ID);
                            self.dismiss_update_toast(ws);
                            if details {
                                self.open_about_panel(ws);
                            }
                            return;
                        }
                        _ => {
                            self.update_toast_focus = None;
                            ws.accessibility_pending = true;
                        }
                    }
                }
                if ws.ime_preedit.is_some() {
                    return;
                }
                // Typing in the torn window right after an X11 drop whose
                // release the WM swallowed commits the latched dock first (same
                // shape as the CursorMoved post-drop heuristic, including the
                // post-handoff blackout and the final-position latch
                // revalidation; on Windows the synthesized release already
                // cleared the tracking before any key can arrive).
                if let Some(td) = self.torn_drag.as_ref()
                    && td.seq == ws.seq
                    && td.native
                    && td.saw_move
                    && td.started.elapsed() >= std::time::Duration::from_millis(300)
                {
                    let commit = self.revalidate_dock_latch(ws);
                    self.finalize_torn_drag(ws, commit);
                    // A committed merge empties this window — it is closing;
                    // don't process the key against an empty mux.
                    if self.window_close_pending(ws.seq) {
                        return;
                    }
                }
                let cursor_exit_key = if ws.cursor_exit_log.enabled {
                    ws.cursor_exit_log.accepted_key(
                        ws.seq,
                        ws.mux.tabs.get(ws.mux.active).map(|tab| tab.focus),
                        event.state,
                        &event.logical_key,
                        event.physical_key,
                        ws.blink_layer.is_active(),
                    )
                } else {
                    None
                };
                // Record before the remaining modal handlers consume the key.
                // Paste uses separate markers.
                {
                    let mods = ws.mods;
                    if let Some(rec) = self.recorder.as_mut() {
                        dev_record_key(rec, &event.logical_key, mods);
                    }
                }
                // Terminator parity: Esc abandons an in-flight pane drag, and
                // is consumed like the tab-drag cancel below it. Only a LIVE
                // drag claims the key — an armed-but-unmoved press is still
                // just a click, and swallowing Esc for it would eat the key
                // whenever the pointer happened to rest on a titlebar.
                //
                // Dropping the state is the whole cancel: nothing has been
                // committed yet. The move only happens at release, off
                // `pane_drag.target`, so a cleared gesture releases into a
                // no-op with the tree untouched.
                if ws.pane_drag.as_ref().is_some_and(|d| d.live)
                    && matches!(&event.logical_key, Key::Named(NamedKey::Escape))
                {
                    ws.pane_drag = None;
                    // Its repeats must not go on to close a modal or reach
                    // the terminal.
                    ws.closing_keys.insert(event.physical_key);
                    if let Some(w) = &ws.window {
                        if cursor_exit_key.is_some() {
                            ws.cursor_exit_log.request_exit(cursor_exit_key);
                        }
                        w.request_redraw();
                    }
                    return;
                }
                // Esc cancels an in-flight tab drag and is consumed, so it
                // never leaks to the PTY or closes a modal.
                if !matches!(ws.detach_drag, crate::detach::DragState::Idle)
                    && matches!(&event.logical_key, Key::Named(NamedKey::Escape))
                {
                    let (next, _restored) = std::mem::take(&mut ws.detach_drag).cancel();
                    ws.detach_drag = next;
                    ws.drag_press = None;
                    ws.tab_drag_active = false;
                    ws.tab_drag_press = None;
                    ws.tab_pressed_idx = None;
                    ws.closing_keys.insert(event.physical_key);
                    if let Some(w) = &ws.window {
                        if cursor_exit_key.is_some() {
                            ws.cursor_exit_log.request_exit(cursor_exit_key);
                        }
                        w.request_redraw();
                    }
                    return;
                }
                // Esc also cancels a MANUAL-FOLLOW torn drag (native cancels
                // belong to the WM's own move loop, whose grab-end the
                // button-state revalidation sorts out). Without it, the
                // release would commit whatever dock target happened to be
                // latched. Consumed, like the pre-tear cancel above; the torn
                // window stays where the drag left it.
                if self.torn_drag.as_ref().is_some_and(|t| !t.native)
                    && matches!(&event.logical_key, Key::Named(NamedKey::Escape))
                {
                    self.abandon_torn_drag(Some(ws));
                    ws.closing_keys.insert(event.physical_key);
                    return;
                }
                // Keep the cursor solid while actively typing.
                // Routes through the shared helper so the eight
                // user-driven blink-reset paths (Reset / focus changes /
                // modal close / typing / tab close / window focus /
                // DEC ?12 toggle) stay in lock-step.
                if cursor_exit_key.is_some() {
                    ws.cursor_exit_log.request_exit(cursor_exit_key);
                }
                self.reset_blink_phase(ws);
                // Hide the OS mouse cursor (configurable; default on, like
                // every modern terminal). Re-shown on the next CursorMoved.
                self.hide_mouse_cursor(ws);
                let text = event.text.as_ref().map(|s| s.as_str());

                // The confirmation modal owns keyboard input; non-navigation keys are consumed.
                // Tab/Shift+Tab and arrows move focus; Enter activates the focused button.
                // Esc closes the modal without dispatching.
                if ws.confirm_dialog.is_some() {
                    if !modal_repeat_is_inert(
                        ModalRepeat::ConfirmDialog,
                        event.repeat,
                        &event.logical_key,
                        ws.mods.control_key(),
                    ) {
                        self.confirm_dialog_key(ws, &event.logical_key, event_loop);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    return;
                }
                if ws.context_menu.is_some() {
                    if !modal_repeat_is_inert(
                        ModalRepeat::ContextMenu {
                            vim_nav: self.cfg.vim_menu_nav,
                        },
                        event.repeat,
                        &event.logical_key,
                        ws.mods.control_key(),
                    ) {
                        self.context_menu_key(ws, &event.logical_key, text, event_loop);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // Vi-mode key dispatch. When
                // vi_mode is Some, intercept keys for vi-style
                // navigation before they reach the PTY. h/j/k/l move
                // the vi cursor; 0/$/g/G jump; Esc exits, and so does the
                // chord that entered it.
                if ws.vi_mode.is_some() {
                    if is_vi_mode_toggle_chord(
                        &self.cfg.keybinds,
                        &event.logical_key,
                        &event.physical_key,
                        ws.mods,
                        Some(&event.key_without_modifiers()),
                    ) {
                        // A held chord leaves once, not on every repeat.
                        if !event.repeat {
                            self.exit_vi_mode(ws);
                        }
                    } else if !modal_repeat_is_inert(
                        ModalRepeat::ViMode,
                        event.repeat,
                        &event.logical_key,
                        ws.mods.control_key(),
                    ) {
                        self.vi_mode_key(ws, &event.logical_key, text);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.hint_state.is_some() {
                    if !modal_repeat_is_inert(
                        ModalRepeat::Hint,
                        event.repeat,
                        &event.logical_key,
                        ws.mods.control_key(),
                    ) {
                        self.hint_key(ws, &event.logical_key, text);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.palette_input.is_some() {
                    if !modal_repeat_is_inert(
                        ModalRepeat::Palette,
                        event.repeat,
                        &event.logical_key,
                        ws.mods.control_key(),
                    ) {
                        self.palette_key(ws, &event.logical_key, text, event_loop);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // The theme picker sits over Settings when the Theme row
                // opened it, so it takes the keys first.
                if ws.theme_picker_input.is_some() {
                    if !modal_repeat_is_inert(
                        ModalRepeat::Palette,
                        event.repeat,
                        &event.logical_key,
                        ws.mods.control_key(),
                    ) {
                        self.theme_picker_key(ws, &event.logical_key, text);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // While the inline path prompt is open it owns the keyboard
                // (typed text → the buffer; Enter/Esc finish it).
                if ws.settings_nav.is_some() && ws.settings_text_edit.is_some() {
                    self.settings_text_key(ws, &event.logical_key, text);
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Settings overlay key handling (exclusive modal).
                if ws.settings_nav.is_some() {
                    // Chord capture waits for a new press. Auto-repeat of the
                    // Space or Enter that started it would otherwise be
                    // captured, or refused with a notification, once per repeat.
                    let inert = if settings_capture_active(ws) {
                        event.repeat
                    } else {
                        // Only a repeat needs the focused row's kind.
                        let steps = event.repeat && self.settings_row_steps(ws);
                        modal_repeat_is_inert(
                            ModalRepeat::Settings { steps },
                            event.repeat,
                            &event.logical_key,
                            ws.mods.control_key(),
                        )
                    };
                    if !inert {
                        self.settings_key(ws, &event.logical_key, event_loop);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.layout_picker_input.is_some() {
                    self.layout_picker_key(ws, &event.logical_key, text);
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.ssh_input.is_some() {
                    self.ssh_key(ws, &event.logical_key, text);
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // Edit-title overlay key handler. Esc
                // cancels; Enter applies via apply_title_edit;
                // Backspace removes one char; printable text appends.
                if ws.editing_title.is_some() {
                    self.title_edit_key(ws, &event.logical_key, text);
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.search.open {
                    if event.repeat
                        && search_button_ignores_repeat(
                            ws.search.focused_control,
                            &event.logical_key,
                        )
                    {
                        return;
                    }
                    if !self.search_key(
                        ws,
                        &event.logical_key,
                        Some(&event.physical_key),
                        text,
                        Some(&event.key_without_modifiers()),
                    ) {
                        self.search_bar_shortcut(
                            ws,
                            &event.logical_key,
                            Some(&event.physical_key),
                            event.repeat,
                            event_loop,
                            Some(&event.key_without_modifiers()),
                        );
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // A preview lane holding the keyboard takes every key, the
                // application's shortcuts included: Esc gives it back.
                if ws.preview_focus.is_some() {
                    self.preview_focus_key(ws, &event.logical_key, event.repeat);
                    remember_closing_key(ws, key_modal, event.physical_key);
                    return;
                }

                if let Some((trigger, act)) = resolve_keybind_action(
                    &self.cfg.keybinds,
                    Some(&event.logical_key),
                    Some(&event.physical_key),
                    ws.mods,
                    Some(&event.key_without_modifiers()),
                ) && !self.chord_falls_through(ws, trigger, &act)
                {
                    track_consumed_key_release(
                        &mut ws.suppressed_key_releases,
                        &mut ws.terminal_owned_key_releases,
                        event.physical_key,
                        event.state,
                        true,
                    );
                    if !(event.repeat && toggles_once_per_press(&act)) {
                        self.handle_action(ws, act, event_loop);
                    }
                    return;
                }

                track_terminal_key_press(
                    &mut ws.suppressed_key_releases,
                    &mut ws.terminal_owned_key_releases,
                    event.physical_key,
                );
                self.write_terminal_key_event(ws, &event, true);
            }
            WindowEvent::RedrawRequested => self.redraw(ws),
            _ => {}
        }
    }
}
