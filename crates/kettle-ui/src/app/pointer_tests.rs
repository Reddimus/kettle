#[test]
fn link_hits_require_cursor_containment() {
    let rect = (400.0, 40.0, 400.0, 560.0);
    assert!(!super::rect_contains(rect, 200.0, 80.0));
    assert!(!super::rect_contains(rect, 800.0, 80.0));
    assert!(super::rect_contains(rect, 400.0, 80.0));

    let src = super::production_source();
    let hit = src
        .split("fn link_at_cursor")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("link hit test");
    let guard = hit
        .find("if !rect_contains(rect, ws.cursor.x as f32, ws.cursor.y as f32)")
        .expect("link hits reject pointers outside the focused terminal");
    let cell = hit
        .find("self.cursor_cell_in_rect(ws, rect)")
        .expect("cell coordinates are resolved after containment");
    assert!(guard < cell);
}

#[test]
fn multi_click_count_is_scoped_to_a_pane() {
    let (mux, target, sibling) = title_test_split_mux();
    let mut ws = WindowState::new(1, mux);
    assert_eq!(App::click_count(&mut ws, 5, 7), 1);
    assert_eq!(App::click_count(&mut ws, 5, 7), 2);
    assert!(ws.mux.focus_pane(sibling));
    assert_eq!(App::click_count(&mut ws, 5, 7), 1);
    assert_eq!(App::click_count(&mut ws, 5, 7), 2);
    assert!(ws.mux.focus_pane(target));
    assert_eq!(App::click_count(&mut ws, 5, 7), 1);
}

#[test]
fn control_selection_cap_preserves_utf8_boundaries() {
    let exact = "x".repeat(super::MAX_CTL_SELECTION_BYTES);
    assert_eq!(super::cap_ctl_selection(exact.clone()), (exact, false));

    let oversized = format!("{}é", "x".repeat(super::MAX_CTL_SELECTION_BYTES - 1));
    let (capped, truncated) = super::cap_ctl_selection(oversized);
    assert!(truncated);
    assert_eq!(capped.len(), super::MAX_CTL_SELECTION_BYTES - 1);
    assert!(capped.is_char_boundary(capped.len()));
}

/// A preview lane owns every press and wheel over it, on the native and
/// the ctl paths alike, ahead of inline cards, the receipt and the
/// terminal; only the primary button acts on its controls.
#[test]
fn a_preview_lane_owns_presses_and_wheels_over_it() {
    let source = super::production_source();
    let body = |name: &str| {
        source
            .split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .unwrap_or_else(|| panic!("{name} present"))
    };
    for name in ["ctl_mouse_press", "window_event_inner"] {
        let press = body(name);
        let lane = press
            .find("self.press_preview_lane(ws, px, py, bcode)")
            .unwrap_or_else(|| panic!("{name} asks the lanes"));
        let card = press
            .find("self.press_card(ws, px, py, bcode)")
            .unwrap_or_else(|| panic!("{name} asks the cards"));
        let receipt = press
            .find("self.activate_media_paste_receipt_at(ws, px, py, bcode)")
            .unwrap_or_else(|| panic!("{name} asks the receipt"));
        assert!(
            lane < card && lane < receipt,
            "{name}: the lane is asked first"
        );
    }
    assert!(
        body("window_event_inner").contains("if !self.press_preview_lane(ws, px, py, sgr)"),
        "a side button over a lane goes nowhere"
    );
    assert!(body("dispatch_wheel").contains("if self.lane_takes_wheel(ws) {"));
    let press = body("press_preview_lane");
    assert!(
        press.contains("if bcode != 0 {\n            return true;\n        }"),
        "every press over a lane is consumed; only the primary button acts"
    );
}

/// Wheel motion over a lane is dropped before it is banked, so no part of
/// it scrolls the terminal beside it.
#[test]
fn a_preview_lane_drops_wheel_motion_before_it_is_banked() {
    let source = super::production_source();
    let ctl = source
        .split("fn ctl_mouse_wheel_delta(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("ctl_mouse_wheel_delta");
    let native = source
        .split("WindowEvent::MouseWheel { delta, phase, .. } => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::DroppedFile(").next())
        .expect("native wheel");
    for (path, body) in [("ctl", ctl), ("native", native)] {
        let body = if path == "native" {
            let (modal, terminal) = body
                .split_once("// A preview lane owns the wheel over it.")
                .expect("native modal routing precedes terminal scrolling");
            assert!(
                modal.contains("self.confirm_wheel(ws, steps.lines);\n                    return;")
            );
            terminal
        } else {
            body
        };
        let gate = body
            .find("if self.lane_takes_wheel(ws) {\n")
            .unwrap_or_else(|| panic!("{path} asks the lanes"));
        let feed = body
            .find("ws.wheel.feed(")
            .unwrap_or_else(|| panic!("{path} banks motion"));
        assert!(gate < feed, "{path}: the lanes are asked before banking");
        assert!(
            body[gate..feed].contains("ws.wheel.reset();"),
            "{path}: the residue is dropped"
        );
    }
}

/// A settled card under the pointer gets the pointing hand and the
/// hover outline, after dialogs and chrome but before split seams, and
/// never with Shift or under the viewer; a card under a still pointer gets them when
/// it settles or moves there, and a pointer that left the window has
/// none.
#[test]
fn a_settled_card_under_the_pointer_shows_it_takes_a_click() {
    let source = super::production_source()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(source.contains(
        "let card = ws .native_pointer .filter(|_| !ws.mods.shift_key() && !self.pointer_modal_open(ws))"
    ));
    assert!(source.contains(
        "let settled = card.filter(|card| ws.card_sightings.settled(Some(*card), now).is_some());"
    ));
    assert!(source.contains(".and_then(|card| ws.card_sightings.settles_at(&card, now));"));
    assert!(source.contains(
        ".or(chrome) // A card takes a press before a split seam does, so its hand // wins where the two overlap. .or(card_hover) // A lane takes every press over it. .or_else(|| self.lane_cursor_icon(ws)) .or_else(|| self.split_seam_hover_icon(ws)) .unwrap_or_else("
    ));
    assert!(source.contains(
        "if ws.card_hover_stale || ws.card_settle_wake.is_some_and(|at| at <= now) { ws.card_hover_stale = false; self.sync_cursor_icon(ws); }"
    ));
    assert!(source.contains(
        "if let Some(next) = card_settle_wait { wait = Some(wait.map_or(next, |current| current.min(next))); }"
    ));
    assert!(
        source.contains(
            "ws.native_pointer = None; ws.card_hover = None; ws.card_settle_wake = None;"
        )
    );
    assert!(source.contains("ws.accessibility_pending = true; ws.card_hover_stale = true;"));
    assert!(source.contains("let card_hover = ws.card_hover;"));
    // The icon, native buttons and the wheel all follow the physical
    // pointer, even after a control client moved the cursor.
    assert!(source.contains(
        "fn sync_cursor_icon(&mut self, ws: &mut WindowState) { let Some(pointer) = ws.native_pointer else { return; }; let control = std::mem::replace(&mut ws.cursor, pointer); let media_receipt_hovered = self.media_paste_receipt_hovered(ws); self.sync_cursor_icon_with_media_receipt_hover(ws, media_receipt_hovered); ws.cursor = control; }"
    ));
    for handler in [
        "WindowEvent::MouseInput { state: ElementState::Pressed, button, .. } => {",
        "WindowEvent::MouseInput { state: ElementState::Released, button, .. } => {",
        "WindowEvent::MouseWheel { delta, phase, .. } => {",
    ] {
        assert!(
            source.contains(&format!("{handler} ws.resume_native_pointer();")),
            "{handler}"
        );
    }
    // The viewer opens on the item published last.
    assert!(source.contains(".and_then(|state| state.media_shelf.latest())"));
    // A control client's moves never light a card the user is not
    // pointing at.
    let ctl = source
        .split("fn ctl_send_mouse_for_window(")
        .nth(1)
        .and_then(|rest| rest.split(" fn ").next())
        .expect("ctl send_mouse");
    assert!(!ctl.contains("native_pointer"));
    assert!(source.contains("ws.cursor = position; ws.native_pointer = Some(position);"));
}

/// Agent/control-plane mouse presses must stay in lock-step with the real
/// winit mouse path for app chrome. The live interaction smoke drives these
/// branches through `kettle ctl send_mouse`; this source guard prevents a
/// future refactor from dropping the modal gate or turning the new-tab
/// dropdown arrow back into a plain `+` click.
#[test]
fn ctl_mouse_press_handles_app_chrome_before_panes() {
    let src = production_source();
    let body = src
        .split("fn ctl_mouse_press(")
        .nth(1)
        .and_then(|s| s.split("fn ctl_mouse_release").next())
        .expect("ctl_mouse_press body present");
    let report = body
        .find("self.send_mouse(ws, bcode, true, false)")
        .expect("terminal mouse reporting");
    let menu = body
        .find("self.open_context_menu(ws, px, py, offer_copied)")
        .expect("local context menu");
    assert!(
        report < menu,
        "terminal reporting must precede the right-click menu"
    );
    assert!(
        body.contains("&& self.settings_mouse(ws, if bcode == 2 { -1 } else { 1 }, true)"),
        "ctl_mouse_press must route Settings modal clicks through settings_mouse"
    );
    assert!(
        body.contains(
            "if modal_swallows_pointer(self.pointer_modal_open(ws), ws.context_menu.is_some())"
        ),
        "ctl_mouse_press must swallow pane/tab clicks behind pointer-owning modals"
    );
    assert!(
        !body.contains("modal_swallows_pointer(self.any_modal_open(ws)"),
        "the search bar is not a pointer-owning modal; ctl_mouse_press must not gate on it"
    );
    assert!(
        body.contains("rect_contains(bar.new_tab_menu, px, py)")
            && body.contains("self.open_new_tab_menu(ws, ax, ay + ah);")
            && body.find("rect_contains(bar.new_tab_menu, px, py)")
                < body.find("rect_contains(bar.new_tab, px, py)"),
        "ctl_mouse_press must check the new-tab dropdown arrow before the + button"
    );
    assert!(
        body.contains("tab_bar_pointer_region_contains(&bar, px, py)"),
        "ctl_mouse_press must use the full vertical tab-bar band, not only its first row"
    );
    assert_eq!(
        src.matches("tab_bar_pointer_region_contains(&bar, px, py)")
            .count(),
        2,
        "both the control-plane and real-window mouse paths must use the full tab-bar band"
    );
}

/// Drift guard. Mouse drag-to-resize of split dividers is
/// wired across three event handlers (press starts the drag, move applies
/// the new ratio, up/focus-loss ends it) plus a hover resize-cursor. A
/// behavioral test needs a window + a real mouse drag (and the geometry math
/// is already unit-tested in `mux::node_tests`), so pin the wiring at the
/// source level.
///
/// Every needle below is matched against the production slice only, so a
/// signature change that leaves the guard behind fails it instead of
/// feeding it its own text.
#[test]
fn split_divider_drag_is_wired() {
    let src = production_source();
    let flat = src.split_whitespace().collect::<Vec<_>>().join(" ");
    // Press starts the drag from a seam hit-test.
    assert!(
        flat.contains("let Some(drag) = self.split_drag_at(")
            && src.contains("if self.press_split_divider(ws, bcode) {"),
        "left-press must start a split-divider drag on a seam hit"
    );
    assert!(
        src.contains("ws.dragging_split = Some(drag);"),
        "the seam hit must actually latch the drag"
    );
    // Move applies the new ratio.
    assert!(
        src.contains("if self.drag_split_divider(ws) {")
            && src.contains("ws.mux.set_split_ratio(ws.mux.active, &path, ratio);")
            && finding_body(include_str!("pointer.rs"), "drag_split_divider")
                .contains("self.resize_all(ws);"),
        "CursorMoved must apply the dragged split ratio"
    );
    // Up + focus-loss end the drag.
    assert!(
        src.contains("End any split-divider drag on left-button up."),
        "the split drag must be cleared on left-button up"
    );
    assert!(
        src.contains("A focus loss also ends any split-divider drag."),
        "the split drag must be cleared on focus loss"
    );
    // Hover shows a resize cursor.
    assert!(
        src.contains(".or_else(|| self.split_seam_hover_icon(ws))"),
        "hovering a divider must show the resize cursor"
    );
}

/// Source guards for event-loop states that require live winit gestures.
#[test]
fn event_state_leaks_are_gated() {
    let src = production_source();
    // 1. Dropped-file modal gate, at the top of the arm.
    let dropped = src
        .split("WindowEvent::DroppedFile(path) => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::Focused").next())
        .expect("DroppedFile event arm");
    let modal_gate = dropped
        .find("if self.any_modal_open(ws) {\n                    return;\n                }")
        .expect("DroppedFile must return while a modal is open");
    let first_path_operation = dropped
        .find("let paths = vec![path];")
        .expect("DroppedFile path delivery");
    assert!(
        modal_gate < first_path_operation,
        "the modal gate must precede all dropped-path work"
    );
    // 2. Focus loss disarms the latched drag and completion state.
    let focused_arm = src
        .split("WindowEvent::Focused(f) => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("Focused event arm");
    assert!(
        focused_arm.contains("self.finish_selection_gesture(ws);")
            && focused_arm.contains("ws.search.dragging_editor = false;")
            && focused_arm.contains("ws.mux.panes.values().chain(")
            && focused_arm.contains("self.windows\n                        .values()")
            && focused_arm.contains("pane.term.invalidate_completion();")
            && focused_arm.contains("ws.tab_drag_active = false;\n                    ws.tab_drag_press = None;\n                    ws.tab_pressed_idx = None;\n                    // A focus loss also ends any split-divider drag.\n                    ws.dragging_split = None;\n                    // Releases now land in another window: each program\n                    // with a held button's press gets its release here.\n                    self.release_held_buttons(ws, |_| true);"),
        "the Focused `!f` arm must disarm the latched drag and completion state"
    );
    // 3. Side-button dismisses a lone context menu instead of leaking SGR.
    assert!(
        src.contains(
            "if ws.context_menu.is_some() {\n                        ws.context_menu = None;"
        ),
        "a side-button press must dismiss a lone context menu, not forward SGR"
    );
}

/// Mouse motion reports only when it crosses into a new cell; a press or
/// release always reports.
#[test]
fn motion_coalesces_to_cell_crossings() {
    // Press/release always report, regardless of the last cell.
    assert!(App::motion_should_report(false, Some((4, 4)), (4, 4)));
    assert!(App::motion_should_report(false, None, (4, 4)));
    // Motion into a NEW cell reports; motion staying in the same cell does not.
    assert!(App::motion_should_report(true, Some((4, 4)), (4, 5)));
    assert!(App::motion_should_report(true, None, (4, 4)));
    assert!(!App::motion_should_report(true, Some((4, 4)), (4, 4)));
}

#[test]
fn mouse_and_focus_reports_use_chronological_user_input() {
    let source = production_source();
    let mouse = source
        .split("fn send_mouse_to(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("target-aware mouse sender");
    // Presses and drags use the pointer path; a release takes the
    // user lane's room for releases. Both stay in order with typing.
    assert!(mouse.contains("pane.feed_pointer_input(&seq)"));
    assert!(mouse.contains("pane.feed_mouse_release(&seq)"));
    assert!(!mouse.contains("queue_protocol_reply"));

    let focus = source
        .split("fn sync_pane_focus_reports(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("focus report sync");
    assert!(focus.contains("let result = pane.feed_focus_report(focus);"));
    assert!(!focus.contains("queue_protocol_reply"));

    let mux = include_str!("../mux.rs").replace("\r\n", "\n");
    let focus_sender = mux
        .split("pub fn feed_focus_report(")
        .nth(1)
        .and_then(|body| body.split("\n    pub fn ").next())
        .expect("focus-report sender");
    assert!(focus_sender.contains("enqueue_focus_report"));
    let release_lane = mux
        .split("fn enqueue_mouse_release(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("mouse-release lane");
    assert!(release_lane.contains("&self.user_tx,"));
    assert!(!focus_sender.contains("with_completion_input_admission"));

    let terminal_replies = source
        .split("TermEvent::PtyWrite(s) => {")
        .nth(1)
        .and_then(|body| body.split("TermEvent::").next())
        .expect("terminal reply arm");
    assert!(terminal_replies.contains("pane.queue_protocol_reply"));
}

/// A press a program got keeps its pane: that button's drag and its one
/// release go there wherever focus has gone, Shift or not, and one whose
/// pane left the screen gets only the release, at the cell it last saw.
/// A press no program got leaves no button held; a release takes room
/// the program's input keeps for releases, so a full queue cannot refuse
/// it. The newest press owns the drag, and a gesture of Kettle's own ends
/// with its release whoever handles it. Kettle ending a program's gesture
/// early (focus loss, search, the menu, a tab leaving, the button pressed
/// again) releases it in place.
#[test]
fn a_held_buttons_reports_stay_with_the_pane_its_press_went_to() {
    let source = production_source();
    let body = |name: &str| {
        source
            .split(&format!("    fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .unwrap_or_else(|| panic!("{name}"))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let report = body("report_mouse");
    let release = report
        .find("(false, _) => match ws.held_buttons.take(btn) { Some(held) if held.pane.is_some() => Some(held),")
        .expect("a release takes its press's entry, and only a program's is reported");
    let drag = report
        .find("(true, true) => ws.held_buttons.dragging(),")
        .expect("a drag follows the newest held button");
    let routed = report
        .find("return self.send_mouse_to(ws, pane, at, button, pressed, motion);")
        .expect("held reports go to their pane");
    let focus = report.find("ws.mux.active_focus()").expect("focus");
    assert!(release < routed && drag < routed && routed < focus);
    assert!(report.contains(
        "(MouseReport::Written, Some((pane, row, col))) => (Some(pane), (row, col)), _ => (None, (0, 0)), }; if let Some(stale) = ws.held_buttons.pressed(btn, pane, cell) { self.release_held(ws, stale); }"
    ));
    let send = body("send_mouse_to");
    assert!(send.contains("if ws.mods.shift_key() && !held {"));
    assert!(send.contains(
        "if matches!(at, MouseAt::Held { rect: None, .. }) && pressed { return MouseReport::Swallowed; }"
    ));
    assert!(send.contains("MouseAt::Held { rect: None, cell } => cell,"));
    assert!(send.contains(
        "let result = if pressed || motion { pane.feed_pointer_input(&seq) } else { pane.feed_mouse_release(&seq) };"
    ));
    assert!(send.contains(
        "if !result.is_queued() { // The program never got it: a press it missed leaves no button // held, so no release follows. self.report_input_result(result); return MouseReport::Swallowed; }"
    ));
    assert!(send.contains("ws.held_buttons.told(pane_id, (row, col));"));
    // Kettle releases what it ends early.
    for ends in ["open_search", "show_context_menu"] {
        assert!(
            body(ends).contains("self.release_held_buttons(ws, |_| true);"),
            "{ends}"
        );
    }
    assert!(
        source.split_whitespace().collect::<Vec<_>>().join(" ").contains(
            "Action::TogglePaneReadOnly => { // A pane turned read-only gets no more input, so a held // button's release goes to it first. if let Some(pane) = ws.mux.active_focus() && ws.mux.focused().is_some_and(|focused| !focused.read_only) { self.release_held_buttons(ws, |held| held.pane == Some(pane)); }"
        ),
        "a pane turning read-only gets its held releases first"
    );
    let flat_source = source.split_whitespace().collect::<Vec<_>>().join(" ");
    for reset in [
        "self.release_held_buttons(ws, |held| held.pane == Some(pane_id)); apply_terminal_control(&ws.mux, &[pane_id], b\"\\x1bc\");",
        "self.release_held_buttons(ws, |held| held.pane == Some(pane_id)); // One request, so a full queue cannot accept RIS but reject // the paired history clear. apply_terminal_control(&ws.mux, &[pane_id], b\"\\x1bc\\x1b[3J\");",
    ] {
        assert!(
            flat_source.contains(reset),
            "a reset releases held buttons first: {reset}"
        );
    }
    let flat = source.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        flat.matches("self.release_held_buttons_in_tab(").count(),
        flat.matches(".mux.detach_tab(").count(),
        "every tab that leaves a window releases its held buttons first"
    );
    for needle in [
        // Each press first releases one whose release went elsewhere, and
        // is Kettle's own gesture until a program receives it.
        "self.release_held_buttons(ws, |held| held.button == bcode); ws.held_buttons.pressed(bcode, None, (0, 0)); if self.confirm_mouse_press(ws, event_loop, bcode) { return; } if self.dismiss_modal_outside(ws, bcode) { return; } // Context menu:",
        "if bcode == 0 { ws.card_press = None; } if ws.card_focus.take().is_some() { ws.accessibility_pending = true; } self.release_held_buttons(ws, |held| held.button == bcode); ws.held_buttons.pressed(bcode, None, (0, 0)); if self.confirm_mouse_press(ws, event_loop, bcode) { return true; } if self.dismiss_modal_outside(ws, bcode) { return true; } if bcode == 0 && self.modal_text_pointer(ws, true) { return true; } let bar",
        // Each release ends a gesture of Kettle's own first.
        "ws.held_buttons.end_own(bcode); // End any split-divider drag on left-button up. if bcode == 0 { ws.dragging_split = None; } if self.confirm_mouse_release(ws, bcode) { return; } // A release ends an editor drag inside the search bar;",
        "if let Some(sgr) = extra_mouse_sgr(button) { ws.held_buttons.end_own(sgr); if self.confirm_mouse_release(ws, sgr) { return; } self.send_mouse(ws, sgr, false, false); return; }",
        "// As natively: a gesture of Kettle's own ends with its release. ws.held_buttons.end_own(bcode);",
    ] {
        assert!(flat.contains(needle), "{needle}");
    }
}

#[test]
fn clicks_share_the_fully_visible_hover_row_contract() {
    let src = production_source();
    let start = src
        .find("fn context_menu_click_action(")
        .expect("context-menu click resolver");
    let end = src[start..]
        .find("\n    /// `(items, anchor, panel_w, panel_h)`")
        .map(|offset| start + offset)
        .expect("end of context-menu click resolver");
    let resolver = &src[start..end];
    assert!(
        resolver.contains("let idx = self.menu_row_at_cursor(ws)?;"),
        "clicks must use the clipped-row helper exercised above"
    );
    assert!(
        !resolver.contains("for (idx, item)"),
        "a second row walk can drift from hover/render clipping"
    );
}

/// A non-positive `scroll-multiplier` must never scroll BACKWARDS. The
/// config clamps to `[0.1, 50.0]` so this is unreachable in practice, but
/// the accumulator floors it at 0 for lines only. Detents still count, so
/// tab cycling and font zoom (which are deliberately multiplier-independent)
/// keep working.
#[test]
fn wheel_accum_ignores_negative_multiplier_for_lines_only() {
    use crate::input::WheelAccum;
    use winit::event::MouseScrollDelta;

    let steps = WheelAccum::default().feed(&MouseScrollDelta::LineDelta(0.0, 1.0), -5.0);
    assert_eq!(steps.lines, 0, "a bad multiplier must not invert scrolling");
    assert_eq!(
        steps.notches, 1,
        "detent-driven actions are multiplier-independent by design"
    );
}

#[test]
fn selection_drag_requires_real_dpi_scaled_travel() {
    use super::selection_drag_motion_started;
    use winit::dpi::PhysicalPosition;

    let origin = PhysicalPosition::new(100.0, 200.0);
    assert!(!selection_drag_motion_started(None, origin, 1.0));
    assert!(!selection_drag_motion_started(Some(origin), origin, 1.0));
    assert!(!selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(101.99, 200.0),
        1.0,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(102.0, 200.0),
        1.0,
    ));
    assert!(!selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(103.99, 200.0),
        2.0,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(104.0, 200.0),
        2.0,
    ));
    assert!(!selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(100.99, 200.0),
        0.5,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(101.0, 200.0),
        0.5,
    ));
    assert!(!selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(101.0, 201.0),
        1.0,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(101.5, 201.5),
        1.0,
    ));
    // Invalid scales use the conservative 1x fallback rather than making
    // every movement a drag or permanently disabling drag detection.
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(102.0, 200.0),
        0.0,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(102.0, 200.0),
        f64::NAN,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(102.0, 200.0),
        -1.0,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(102.0, 200.0),
        f64::INFINITY,
    ));
    assert!(!selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(f64::NAN, 200.0),
        1.0,
    ));
    assert!(!selection_drag_motion_started(
        Some(PhysicalPosition::new(f64::INFINITY, 200.0)),
        origin,
        1.0,
    ));
}

#[test]
fn selection_drag_state_measures_from_press_and_stays_latched() {
    use super::{
        Mux, WindowState, arm_selection_gesture, clear_selection_gesture,
        note_selection_drag_motion, selection_release_matches,
    };
    use winit::dpi::PhysicalPosition;

    let mut ws = WindowState::new(1, Mux::new());
    let origin = PhysicalPosition::new(100.0, 200.0);
    ws.cursor = origin;
    arm_selection_gesture(&mut ws, 42, 2);
    assert_eq!(ws.selecting_pane, Some(42));
    assert_eq!(ws.selection_button, Some(2));
    assert!(selection_release_matches(ws.selection_button, 2));
    assert!(!selection_release_matches(ws.selection_button, 0));

    note_selection_drag_motion(&mut ws);
    assert!(!ws.selection_dragged, "a duplicate native move stays inert");

    ws.cursor = PhysicalPosition::new(101.99, 200.0);
    note_selection_drag_motion(&mut ws);
    assert!(
        !ws.selection_dragged,
        "travel is measured from the press using the no-window 1x fallback"
    );

    ws.cursor = PhysicalPosition::new(102.0, 200.0);
    note_selection_drag_motion(&mut ws);
    assert!(ws.selection_dragged, "the exact threshold starts the drag");

    ws.cursor = origin;
    note_selection_drag_motion(&mut ws);
    assert!(
        ws.selection_dragged,
        "once real travel starts a drag, returning to the origin cannot disarm it"
    );

    clear_selection_gesture(&mut ws);
    assert!(!ws.selecting);
    assert!(!ws.selection_dragged);
    assert_eq!(ws.selection_drag_origin, None);
    assert_eq!(ws.selection_button, None);
    assert!(!selection_release_matches(ws.selection_button, 2));
}

#[test]
fn selection_copy_requires_the_gesture_owner() {
    use super::selection_copy_owner;

    assert_eq!(selection_copy_owner(true, Some(7)), Some(7));
    assert_eq!(selection_copy_owner(true, None), None);
    assert_eq!(selection_copy_owner(false, Some(7)), None);
}

#[test]
fn native_and_control_selection_motion_share_the_drag_threshold() {
    let src = production_source();
    let control = src
        .split("fn ctl_mouse_move(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("ctl mouse move body");
    assert!(control.contains("note_selection_drag_motion(ws);"));

    let native = src
        .split("WindowEvent::CursorMoved { position, .. } => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("native cursor-moved arm");
    assert!(native.contains("note_selection_drag_motion(ws);"));
    let cursor_at = native
        .find("ws.cursor = position;")
        .expect("native motion stores the delivered position");
    let motion_at = native
        .find("note_selection_drag_motion(ws);")
        .expect("native motion applies the drag threshold");
    assert!(
        cursor_at < motion_at,
        "native motion must store the new cursor before measuring from the press origin"
    );
    assert!(
        !src.contains("ws.selection_dragged = true;"),
        "a raw move event must not bypass the travel threshold"
    );

    let cursor_left = src
        .split("WindowEvent::CursorLeft { .. } => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("native cursor-left arm");
    let cursor_left = cursor_left.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        cursor_left.contains("selection_leave_edge(")
            && cursor_left.contains("ws.selection_dragged"),
        "leaving the client area must not bypass measured drag travel"
    );

    let clear = src
        .split("fn clear_selection_gesture(")
        .nth(1)
        .and_then(|body| body.split("\n}\n").next())
        .expect("selection gesture reset");
    assert!(clear.contains("ws.selection_drag_origin = None;"));
    assert!(clear.contains("ws.selection_button = None;"));
    assert_eq!(
        src.matches("arm_selection_gesture(ws, pane_id,").count(),
        2,
        "fresh and extended selections must share origin and button ownership"
    );

    let close_modals = src
        .split("fn close_all_modals(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("shared modal transition");
    assert!(
        close_modals.contains("self.finish_selection_gesture(ws);"),
        "every modal transition must commit copy-on-select before ending pointer ownership"
    );
    assert!(
        close_modals.contains("ws.theme_picker_input = None;"),
        "opening another modal must close the theme picker, which reverts its preview"
    );

    let finish_selection = src
        .split("fn finish_selection_gesture(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("shared selection completion transition");
    let copy_at = finish_selection
        .find("self.copy_selection(ws);")
        .expect("copy-on-select is committed");
    let owner_at = finish_selection
        .find("selection_copy_owner(ws.selecting, ws.selecting_pane).is_some()")
        .expect("copy-on-select requires an explicit gesture owner");
    let clear_at = finish_selection
        .find("clear_selection_gesture(ws);")
        .expect("selection gesture is cleared");
    assert!(
        owner_at < copy_at && copy_at < clear_at,
        "copying requires selecting_pane and must precede the clear"
    );

    let install_confirm = src
        .split("fn install_confirm_dialog(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("shared confirmation transition");
    let finish_at = install_confirm
        .find("self.finish_selection_gesture(ws);")
        .expect("confirmation completes terminal selection ownership");
    let install_at = install_confirm
        .find("ws.confirm_dialog = Some(dialog);")
        .expect("confirmation stores the modal");
    let release_at = install_confirm
        .find("self.release_held_buttons(ws, |_| true);")
        .expect("program mouse gestures receive their releases before modal ownership");
    assert!(finish_at < release_at && release_at < install_at);
    assert!(
        finish_at < install_at,
        "selection ownership must end before a confirmation becomes visible"
    );
    let normalized_src = src.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        normalized_src
            .matches("self.install_confirm_dialog(")
            .count(),
        2,
        "every confirmation path (closing and keybind conflicts) must use the shared transition"
    );
    assert_eq!(
        normalized_src.matches("ws.confirm_dialog = Some(").count(),
        1,
        "confirmations must not bypass the shared pointer-ownership transition"
    );

    let show_context_menu = src
        .split("fn show_context_menu(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("shared context-menu transition");
    assert!(
        show_context_menu.contains("self.finish_selection_gesture(ws);"),
        "a context menu must commit copy-on-select before taking the pointer"
    );

    let update_selection = src
        .split("fn update_selection(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("selection update body");
    assert_eq!(
        update_selection
            .matches("self.finish_selection_gesture(ws);")
            .count(),
        3,
        "every selection-owner invalidation must commit copy-on-select before clearing"
    );
    assert!(
        !update_selection.contains("clear_selection_gesture(ws);"),
        "selection invalidation must not bypass copy-on-select"
    );

    let redraw = src
        .split("fn redraw(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("redraw body");
    assert!(
        redraw.contains("self.finish_selection_gesture(ws);")
            && !redraw.contains("clear_selection_gesture(ws);"),
        "frame-time pane invalidation must preserve copy-on-select"
    );

    let control_move = src
        .split("fn ctl_mouse_move(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("control mouse move body");
    assert!(
        control_move.contains("ws.selection_autoscroll_edge = 0;"),
        "a control move inside the client must clear a stale native edge latch"
    );

    let control_release = src
        .split("fn ctl_mouse_release(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("control mouse release body");
    let native_release = src
        .split("state: ElementState::Released,")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("native mouse release arm");
    for (name, body) in [("control", control_release), ("native", native_release)] {
        assert!(
            body.contains("selection_release_matches(ws.selection_button, bcode)")
                && body.contains("self.finish_selection_gesture(ws);"),
            "{name} release must match the owning button and commit copy-on-select before clearing it"
        );
    }
}

#[test]
fn selection_autoscroll_rate_scales_with_overshoot() {
    use super::selection_autoscroll_lines;
    let dragged = |y| selection_autoscroll_lines(y, 100.0, 200.0, 1.0, true);
    // Outside the six-point edge zones → no scroll.
    assert_eq!(dragged(150.0), 0);
    assert_eq!(dragged(107.0), 0);
    assert_eq!(dragged(193.0), 0);
    assert_eq!(dragged(100.0), 1);
    assert_eq!(dragged(200.0), -1);
    // Just past the top → 1 line/frame up into history (positive).
    assert_eq!(dragged(95.0), 1);
    // Moderate overshoot (10..40 px) → 2 lines/frame.
    assert_eq!(dragged(80.0), 2);
    // Big overshoot (≥40 px) → 3 lines/frame.
    assert_eq!(dragged(50.0), 3);
    // Past the bottom → negative (toward the present).
    assert_eq!(dragged(205.0), -1);
    assert_eq!(dragged(220.0), -2);
    assert_eq!(dragged(280.0), -3);
    // Holding on the first or last few pixels inside a full-height pane
    // must keep the selection moving; there may be no client area beyond
    // the bottom edge for the pointer to enter.
    assert_eq!(dragged(102.0), 1);
    assert_eq!(dragged(198.0), -1);
    // Every path is drag-only, including a coordinate just outside the
    // pane. A press on the boundary plus sub-threshold outward jitter must
    // not bypass the measured-travel gate.
    assert_eq!(
        selection_autoscroll_lines(100.0, 100.0, 200.0, 1.0, false),
        0
    );
    assert_eq!(
        selection_autoscroll_lines(95.0, 100.0, 200.0, 1.0, false),
        0
    );
    assert_eq!(
        selection_autoscroll_lines(205.0, 100.0, 200.0, 1.0, false),
        0
    );
    // The zone is specified in logical points and capped to a quarter of a
    // tiny pane so its top and bottom regions cannot overlap.
    assert_eq!(
        selection_autoscroll_lines(111.0, 100.0, 200.0, 2.0, true),
        1
    );
    assert_eq!(
        selection_autoscroll_lines(113.0, 100.0, 200.0, 2.0, true),
        0
    );
    assert_eq!(selection_autoscroll_lines(1.0, 0.0, 8.0, 1.0, true), 1);
    assert_eq!(selection_autoscroll_lines(4.0, 0.0, 8.0, 1.0, true), 0);
    assert_eq!(selection_autoscroll_lines(7.0, 0.0, 8.0, 1.0, true), -1);
    for invalid_scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert_eq!(
            selection_autoscroll_lines(105.0, 100.0, 200.0, invalid_scale, true),
            1,
            "invalid display scales use the 1x edge zone"
        );
        assert_eq!(
            selection_autoscroll_lines(195.0, 100.0, 200.0, invalid_scale, true),
            -1,
            "invalid display scales use the 1x edge zone"
        );
    }
    // A transient empty layout must not invent an edge direction.
    assert_eq!(
        selection_autoscroll_lines(100.0, 100.0, 100.0, 1.0, true),
        0
    );
    assert_eq!(
        selection_autoscroll_lines(100.0, 200.0, 100.0, 1.0, true),
        0
    );
}

#[test]
fn selection_leave_latches_only_the_nearest_vertical_edge() {
    use super::selection_leave_edge;

    // Leaving the window is not itself proof of a drag. A press on the
    // last pixel followed by sub-threshold outward jitter must stay inert.
    assert_eq!(selection_leave_edge(false, 400.0, 1.0, 800.0, 600.0), 0);
    assert_eq!(selection_leave_edge(false, 400.0, 599.0, 800.0, 600.0), 0);
    assert_eq!(selection_leave_edge(true, 400.0, 1.0, 800.0, 600.0), 1);
    assert_eq!(selection_leave_edge(true, 400.0, 599.0, 800.0, 600.0), -1);
    assert_eq!(selection_leave_edge(true, 1.0, 300.0, 800.0, 600.0), 0);
    assert_eq!(selection_leave_edge(true, 799.0, 300.0, 800.0, 600.0), 0);
    assert_eq!(selection_leave_edge(true, 1.0, 1.0, 800.0, 600.0), 1);
    assert_eq!(selection_leave_edge(true, 799.0, 599.0, 800.0, 600.0), -1);
    assert_eq!(selection_leave_edge(true, f32::NAN, 1.0, 800.0, 600.0), 0);
    assert_eq!(selection_leave_edge(true, 1.0, 1.0, 0.0, 600.0), 0);
}

#[test]
fn wheel_target_follows_hover_without_changing_focus_and_falls_back() {
    use super::wheel_target_at;

    let layout = [
        (10, (0.0, 0.0, 400.0, 600.0)),
        (20, (400.0, 0.0, 400.0, 600.0)),
    ];
    assert_eq!(
        wheel_target_at(&layout, Some(10), 650.0, 300.0),
        Some(layout[1]),
        "the non-focused pane under the pointer must own the wheel"
    );
    assert_eq!(
        wheel_target_at(&layout, Some(10), 900.0, 300.0),
        Some(layout[0]),
        "outside pane geometry must preserve focused-pane behavior"
    );
    assert_eq!(wheel_target_at(&layout, None, 900.0, 300.0), None);
}

#[test]
fn viewport_point_to_grid_applies_display_offset() {
    use super::viewport_point_to_grid;
    use kettle_core::{Column, Line, Point};
    // At the bottom (display_offset 0): viewport == grid-absolute.
    let p = viewport_point_to_grid(Point::new(Line(5), Column(3)), 0);
    assert_eq!(p.line, Line(5));
    assert_eq!(p.column, Column(3));
    // Scrolled back by 3: absolute = viewport − offset. Without the
    // subtraction, a scrolled selection reads the wrong row.
    assert_eq!(
        viewport_point_to_grid(Point::new(Line(5), Column(0)), 3).line,
        Line(2)
    );
    // Scrolled far enough that the visible row maps into history (negative
    // absolute line — alacritty's grid indexes history with negative lines).
    assert_eq!(
        viewport_point_to_grid(Point::new(Line(2), Column(0)), 10).line,
        Line(-8)
    );
}

#[test]
fn px_to_cell_reports_sub_cell_side() {
    use super::px_to_cell;
    use kettle_core::Side;
    let rect = (10.0, 20.0, 800.0, 600.0);
    let (cw, ch, pad) = (8.0_f32, 16.0_f32, 4.0_f32);
    // Content origin x = rx + pad = 14.0; each cell is `cw` wide.
    let ox = 10.0 + pad; // 14.0
    let y = 30.0; // any in-grid row

    // Left half of cell 0 → Left.
    let (c0, _, s0) = px_to_cell(ox + 1.0, y, rect, (cw, ch), (pad, pad));
    assert_eq!(c0, 0);
    assert_eq!(s0, Side::Left, "left half of the cell → Left");

    // Right half of cell 0 → Right.
    let (_, _, s1) = px_to_cell(ox + 6.0, y, rect, (cw, ch), (pad, pad));
    assert_eq!(s1, Side::Right, "right half of the cell → Right");

    // The exact midpoint counts as the right half (matches alacritty's
    // `< half ⇒ Left` boundary — anything ≥ half is Right).
    let (_, _, s2) = px_to_cell(ox + cw / 2.0, y, rect, (cw, ch), (pad, pad));
    assert_eq!(s2, Side::Right, "exact midpoint → Right");

    // The side is a within-cell property: the same sub-cell offset in a later
    // column yields the same side (no drift across the row).
    let (c3, _, s3) = px_to_cell(ox + cw * 5.0 + 1.0, y, rect, (cw, ch), (pad, pad));
    assert_eq!(c3, 5);
    assert_eq!(s3, Side::Left, "side is independent of the column index");

    // A pointer in the LEFT PADDING (left of column 0) clamps to col 0 AND
    // Side::Left, so a drag starting there still INCLUDES the first cell.
    // Deriving the side from the raw negative offset via `rem_euclid` would
    // wrap it into the right half and drop column 0.
    let (c4, _, s4) = px_to_cell(ox - 3.0, y, rect, (cw, ch), (pad, pad));
    assert_eq!(c4, 0, "left of origin clamps to column 0");
    assert_eq!(
        s4,
        Side::Left,
        "left of origin → Side::Left (first cell kept)"
    );
}

#[test]
fn extra_mouse_sgr_maps_side_buttons() {
    use super::extra_mouse_sgr;
    use winit::event::MouseButton;
    // Drift guard. Back = XBUTTON1 = SGR 128, Forward =
    // XBUTTON2 = SGR 129; the primary three + anything else are handled
    // locally (or dropped), so they return None here.
    assert_eq!(extra_mouse_sgr(MouseButton::Back), Some(128));
    assert_eq!(extra_mouse_sgr(MouseButton::Forward), Some(129));
    assert_eq!(extra_mouse_sgr(MouseButton::Left), None);
    assert_eq!(extra_mouse_sgr(MouseButton::Middle), None);
    assert_eq!(extra_mouse_sgr(MouseButton::Right), None);
    assert_eq!(extra_mouse_sgr(MouseButton::Other(5)), None);
}

/// With `close-button-on-tab = false`, the tab's close rect must be
/// unhittable, not just unpainted. A full-size hidden rect lets a click on
/// the tab's trailing square close it, and every pane in it, with no
/// visible button.
///
/// The empty rect keeps all three consumers (real press, ctl press, hover
/// cursor) safe without each having to consult the config.
#[test]
fn a_hidden_tab_close_button_is_not_clickable() {
    use super::{hovered_close_button, tab_close_rect};
    use kettle_render::TabSeg;

    let visible = tab_close_rect(76.0, 0.0, 24.0, true);
    let hidden = tab_close_rect(76.0, 0.0, 24.0, false);
    assert_eq!(visible, (76.0, 0.0, 24.0, 24.0), "shown: full square");
    assert_eq!(hidden.2, 0.0, "hidden: zero width");
    assert_eq!(hidden.3, 0.0, "hidden: zero height");

    // Probe the dead centre of where the hidden button would be.
    let seg = |close| TabSeg {
        idx: 0,
        rect: (0.0, 0.0, 100.0, 24.0),
        title_rect: (8.0, 0.0, 68.0, 24.0),
        close,
        title: String::new(),
        path: None,
        active: true,
        activity: kettle_render::TabActivity::Normal,
    };
    assert_eq!(
        hovered_close_button(&[seg(visible)], 88.0, 12.0),
        Some(0),
        "a visible close button must still be hittable"
    );
    assert_eq!(
        hovered_close_button(&[seg(hidden)], 88.0, 12.0),
        None,
        "a hidden close button must not be hittable"
    );
}

#[test]
fn mapped_mouse_closure_releases_the_closed_windows_toast() {
    let src = production_source();
    let mouse = src.split_once("fn ctl_send_mouse(").unwrap().1;
    let close = mouse.split_once("drop(target);").unwrap().0;
    let close = close
        .rsplit_once("if take_window_close_request(")
        .unwrap()
        .1;
    assert!(close.contains("self.close_update_toast_window(target_seq);"));
}

#[test]
fn selection_kind_maps_clicks_and_alt() {
    // Double/triple click → word/line regardless of Alt.
    assert!(matches!(selection_kind(2, false), SelectionType::Semantic));
    assert!(matches!(selection_kind(2, true), SelectionType::Semantic));
    assert!(matches!(selection_kind(3, false), SelectionType::Lines));
    // Single click: plain drag, or Alt → rectangular block.
    assert!(matches!(selection_kind(1, false), SelectionType::Simple));
    assert!(matches!(selection_kind(1, true), SelectionType::Block));
    // A 0 click-count (no cell) still behaves like a single click.
    assert!(matches!(selection_kind(0, false), SelectionType::Simple));
    assert!(matches!(selection_kind(0, true), SelectionType::Block));
}

#[test]
fn finding_tab_pointer_uses_painted_band() {
    let body = finding_body(include_str!("chrome.rs"), "cursor_in_tab_bar");
    assert!(
        body.contains("self.tab_band(ws, h)"),
        "tab pointer hits must include the bottom status reservation"
    );
    let band = super::tab_band_rect(
        (800, 600),
        kettle_config::TabBarPos::Bottom,
        24.0,
        180.0,
        0.0,
        22.0,
    );
    assert!(super::rect_contains(band, 400.0, 560.0));
    assert!(!super::rect_contains(band, 400.0, 590.0));
}

#[test]
fn finding_synthetic_divider_drag_uses_native_handlers() {
    let source = include_str!("ctl.rs");
    assert!(
        finding_body(source, "ctl_mouse_press").contains("self.press_split_divider(ws, bcode)"),
        "synthetic divider presses must arm a split drag"
    );
    assert!(
        finding_body(source, "ctl_mouse_move").contains("self.drag_split_divider(ws)"),
        "synthetic motion must resize split PTYs"
    );
    assert!(include_str!("window_events.rs").contains("if self.drag_split_divider(ws)"));
}

