/// Confirmation buttons are hit-tested with the metrics the renderer paints
/// them with: the chrome font's advance and line height, not the terminal
/// cell. With `cell-width = 0.5` the two differ by half, and a click on empty
/// space could confirm a destructive action.
#[test]
fn confirmation_hits_use_the_painted_chrome_metrics() {
    let source = include_str!("modals.rs");
    let layout = source
        .split("pub(super) fn confirmation_layout(")
        .nth(1)
        .unwrap()
        .split("pub(super) fn confirm_button_hovered(")
        .next()
        .unwrap();
    assert!(layout.contains("self.overlay_text_cell_width(ws)"));
    assert!(layout.contains("self.overlay_text_line_height(ws)"));
    assert!(layout.contains("window.scale_factor()"));
    assert!(!layout.contains("cell_px("));
    for name in [
        "confirm_button_hovered",
        "confirm_mouse_press",
        "confirm_mouse_release",
    ] {
        let body = source
            .split(&format!("pub(super) fn {name}("))
            .nth(1)
            .unwrap()
            .split("\n    pub(super) fn ")
            .next()
            .unwrap();
        assert!(body.contains("self.confirmation_layout(ws)"), "{name}");
    }
}

/// A modal question has to outrank the overlay that asked it. Rebinding a
/// key onto a chord that is already taken raises a confirm dialog from
/// inside the Settings overlay. If the Settings arms claimed the keyboard
/// first, in either key path, the dialog could not be answered while the
/// panel that raised it stayed open. The panel's dim backdrop must also
/// stop above the dialog, or it greys out the question.
#[test]
fn a_confirm_dialog_outranks_every_overlay_including_the_one_that_raised_it() {
    let src = production_source();

    // Named-key path: the confirm branch comes before every other overlay,
    // and before the settings branches in particular.
    // Slice the router itself first: these same branch conditions appear
    // in the mouse handlers, so comparing whole-file positions would
    // compare the wrong pair and pass for the wrong reason.
    let router = src
        .split("let text = event.text.as_ref().map(|s| s.as_str());")
        .nth(1)
        .expect("the key router opens by binding `text`");
    let confirm = router
        .find("if ws.confirm_dialog.is_some() {")
        .expect("the confirm-modal key handler is present");
    for later in [
        "if ws.context_menu.is_some() {",
        "if ws.settings_nav.is_some() {",
        "if ws.palette_input.is_some() {",
    ] {
        let at = router
            .find(later)
            .unwrap_or_else(|| panic!("expected overlay branch {later:?} in the key router"));
        assert!(
            confirm < at,
            "the confirm modal must be handled before {later:?}, or the \
                 question it asks cannot be answered"
        );
    }

    // Character path: the same, so typed text cannot land in a settings
    // field while a modal is waiting on an answer.
    let chars = src
        .split("let key = Key::Character(text.into());")
        .nth(1)
        .expect("the character router builds a Character key first");
    let confirm_arm = chars
        .find("if ws.confirm_dialog.is_some() {")
        .expect("the character router must swallow text while a modal is up");
    for later in ["ws.palette_input.is_some()", "ws.settings_nav.is_some()"] {
        let at = chars
            .find(later)
            .unwrap_or_else(|| panic!("expected {later:?} in the character router"));
        assert!(
            confirm_arm < at,
            "the modal arm must precede {later:?} in the character router"
        );
    }

    let render = [
        kettle_test_support::production_source(include_str!("../../../kettle-render/src/lib.rs")),
        kettle_test_support::production_source(include_str!(
            "../../../kettle-render/src/batching.rs"
        )),
    ]
    .join("\n");
    let pass = render
        .split("fn encode_scene_pass(")
        .nth(1)
        .unwrap()
        .split("\n    }\n")
        .next()
        .unwrap();
    let settings_text = pass.find("self.menu_text_renderer").unwrap();
    let surfaces = pass.rfind("self.rounded_surfaces").unwrap();
    let modal_text = pass.find("self.confirmation_text_renderer").unwrap();
    assert!(settings_text < surfaces && surfaces < modal_text);
    assert!(
        !render
            .contains("menu_areas.retain(|area| std::ptr::eq(area.buffer, &self.search_buffer))")
    );
}

/// A lane is no modal: dialogs, menus and other modals sit over it and
/// leave it where it is, and the event loop closes a lane only when it
/// has nothing to show.
#[test]
fn a_modal_leaves_the_preview_lanes_in_place() {
    let source = super::production_source();
    let body = |name: &str| {
        source
            .split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .unwrap_or_else(|| panic!("{name} present"))
    };
    for name in [
        "close_all_modals",
        "install_confirm_dialog",
        "show_context_menu",
        "commit_ime_text",
    ] {
        let modal = body(name);
        assert!(
            !modal.contains("preview_panels"),
            "{name} keeps pinned panels"
        );
        if name == "close_all_modals" {
            assert!(modal.contains("if let Some(open) = ws.preview_overlay"));
            assert!(modal.contains("self.close_preview(ws, open.pane)"));
        } else {
            assert!(
                !modal.contains("close_preview"),
                "{name} keeps pinned lanes"
            );
        }
    }
    assert!(body("about_to_wait_inner").contains("self.prune_previews(ws);"));
    let prune = body("prune_previews");
    assert!(prune.contains("&& self.preview_item(ws, *pane).is_some()"));
    assert!(prune.contains("ws.mux.retain_lanes(|pane| live.contains(&pane))"));
}

/// Drift guard for the whole family of append-only modal text fields.
///
/// Each of these handlers owns a plain `String`. `KeyEvent::text` is
/// **not** filtered by Command on macOS, so a raw append types `⌘V` as
/// `"v"`. Pasting into a tab rename then yields a tab named `v`, and in the
/// command palette it rewrites the query and re-ranks the list, so the next
/// Enter runs a command the user never chose. `String::pop` splits
/// grapheme clusters, so one Backspace on `👩‍🚀` leaves a dangling
/// zero-width joiner. A raw buffer also has no length cap.
///
/// `modal_input` owns modifier filtering, grapheme-correct deletion, the
/// byte cap, and paste in one place. This pins that every handler goes
/// through it, so a new modal cannot quietly reintroduce the raw-append
/// pattern. Needs full App state, so it pins at the source like its
/// neighbours.
#[test]
fn every_modal_text_field_routes_through_the_shared_input_rules() {
    let src = production_source();

    // Every handler that consumes typed text shares the modifier rule.
    // Search is included: it owns a real editor and does its own paste and
    // deletion, but its catch-all arm still has to reject Command chords,
    // because every chord it does not claim explicitly (⌘Q, ⌘W, …) used to
    // land in the query as a bare letter.
    let modal_fields = [
        "fn palette_key(",
        "fn ssh_key(",
        "fn layout_picker_key(",
        "fn settings_text_key(",
        "fn title_edit_key(",
        "fn theme_picker_key(",
    ];
    for name in modal_fields.iter().copied().chain(["fn search_key("]) {
        let body = src
            .split(name)
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .unwrap_or_else(|| panic!("missing production body for {name}"));

        assert!(
            body.contains("crate::modal_input::accept_text(text, ws.mods)"),
            "{name} must filter text through modal_input::accept_text, \
                 or a Command chord types its letter into the field"
        );
        // ANY direct mutation of the buffer sidesteps the cap and the
        // filter. A `state.input.push_str(...)` next to the helper call
        // would drop the 4 KiB bound on that path.
        for banned in [
            "push_str(t)",
            ".buf.pop()",
            "input.pop()",
            "input.push(",
            "input.push_str(",
            "input.extend(",
            ".buf.push(",
            ".buf.push_str(",
            ".buf.extend(",
            "q.push(",
            "q.push_str(",
            "q.extend(",
        ] {
            assert!(
                !body.contains(banned),
                "{name} mutates its buffer directly with `{banned}`; every \
                     write must go through modal_input so the cap and the \
                     control-character filter apply"
            );
        }
        if !modal_fields.contains(&name) {
            continue;
        }
        assert!(
            body.contains("self.modal_editor_key(ws, key)"),
            "{name} must route editing commands to the shared editor"
        );
        assert!(
            body.contains("crate::modal_input::push_text("),
            "{name} must append through modal_input::push_text so the \
                 byte cap and control-character filter apply"
        );
        assert!(
            body.contains("crate::modal_input::backspace("),
            "{name} must delete through modal_input::backspace so a \
                 grapheme cluster leaves whole"
        );
        assert!(
            body.contains("crate::modal_input::is_paste_chord("),
            "{name} must honour the platform paste chord"
        );
    }
}

/// Drift guard. The single "Window padding" Settings
/// control must persist BOTH `window-padding-x` and `-y` so the result is
/// symmetric — persisting only X leaves Y at its default (lopsided). A
/// behavioral test needs the live overlay + persist path; pin the
/// dual-write at the source.
///
/// The Y mirror write's `persist_pref` result must also be checked and
/// surfaced via `fire_notify` on failure, like the general Settings write
/// path just above it. Otherwise a failed mirror write after a successful
/// X write leaves lopsided padding after restart with no error shown.
#[test]
fn window_padding_setting_writes_both_axes() {
    let src = production_source();
    assert!(
        src.contains(
            "if key_str == \"window-padding-x\" && !self.persist_pref(\"window-padding-y\", &new_val) {"
        ),
        "the Window-padding control must mirror its value to window-padding-y AND \
             check the mirror write's result"
    );
}

#[test]
fn surface_restart_hint_requires_a_successful_config_write() {
    let src = production_source();
    let arm = src
        .split("let saved = self.persist_pref(key_str, &new_val);")
        .nth(1)
        .and_then(|rest| rest.split("self.reload_config(ws);").next())
        .expect("settings persistence arm");
    assert!(
        arm.contains("if saved")
            && arm.contains("\"background-opacity\" | \"background-type\" | \"window-blur\"",)
            && arm.contains("ws.settings_restart_pending = true;"),
        "a failed surface-setting write must not promise that a new window will apply it"
    );
}

/// A side-button press reaches the terminal only past every surface that
/// owns the pointer, and its release only when the press did, once.
#[test]
fn side_button_forward_is_modal_gated_and_releases_pair_with_presses() {
    // Collapse all whitespace before scanning. rustfmt wraps the call
    // across lines, and the match must also survive a CRLF working tree
    // (which `.gitattributes eol=lf` normally prevents).
    let src = production_source()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        src.contains(
            "if !self.press_preview_lane(ws, px, py, sgr) \
                 && !modal_swallows_pointer( self.pointer_modal_open(ws), ws.context_menu.is_some(), ) \
                 { self.report_mouse(ws, sgr, true, false); }"
        ),
        "a side-button press is forwarded past the lanes and modals"
    );
    assert!(
        src.contains(
            "if let Some(sgr) = extra_mouse_sgr(button) { ws.held_buttons.end_own(sgr); if self.confirm_mouse_release(ws, sgr) { return; } self.send_mouse(ws, sgr, false, false); return; }"
        ),
        "its release goes only where a press went: report_mouse declines one no program had"
    );
    assert!(
        src.contains(
            "self.release_held_buttons(ws, |held| held.button == sgr); ws.held_buttons.pressed(sgr, None, (0, 0)); if self.confirm_mouse_press(ws, event_loop, sgr) { return; } if self.dismiss_modal_outside(ws, sgr) { return; } // A lone context menu"
        ),
        "a side press first releases one whose release went elsewhere, and is Kettle's until a program gets it"
    );
    // A press the search bar keeps from the terminal is not one it saw.
    let send = production_source();
    let send_to = send
        .split("fn send_mouse_to(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("send_mouse_to");
    assert!(send_to.contains(
        "ws.held_buttons.any(),\n        ) {\n            return MouseReport::Swallowed;"
    ));
    assert_eq!(send_to.matches("MouseReport::Written").count(), 1);
    assert!(super::MouseReport::Swallowed.consumed() && !super::MouseReport::Declined.consumed());
}

/// Regression coverage for confirmation-dialog keyboard navigation.
#[test]
fn confirm_dialog_keypress_walks_state_machine() {
    use super::{ConfirmKey, ConfirmKeyResult, confirm_dialog_keypress};
    // Escape always cancels.
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::Escape),
        ConfirmKeyResult::Cancel
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Escape),
        ConfirmKeyResult::Cancel
    );
    // Enter activates the FOCUSED button (buttons are
    // `[Cancel, Confirm]`). Focused on Cancel (idx 0) → Cancel; focused on
    // Confirm (idx 1, the last) → Confirm. This must match the highlighted
    // button so Enter on the safe default doesn't fire the destructive action.
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::Enter),
        ConfirmKeyResult::Cancel,
        "Enter on the highlighted Cancel button must cancel, not confirm"
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Enter),
        ConfirmKeyResult::Confirm,
        "Enter on the Confirm button confirms"
    );
    // Tab cycles forward with wrap.
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::Tab),
        ConfirmKeyResult::Move(1)
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Tab),
        ConfirmKeyResult::Move(0),
        "Tab wraps at end"
    );
    // Shift+Tab cycles backward with wrap.
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::ShiftTab),
        ConfirmKeyResult::Move(1),
        "Shift+Tab wraps at start"
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::ShiftTab),
        ConfirmKeyResult::Move(0)
    );
    // Left at idx 0 is a no-op (Ignore — caller suppresses).
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::Left),
        ConfirmKeyResult::Ignore
    );
    // Left at idx 1 moves to 0.
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Left),
        ConfirmKeyResult::Move(0)
    );
    // Right at last idx is a no-op.
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Right),
        ConfirmKeyResult::Ignore
    );
    // Right at idx 0 with 2 buttons moves to 1.
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::Right),
        ConfirmKeyResult::Move(1)
    );
    // Defensive: 0 buttons → any key cancels.
    assert_eq!(
        confirm_dialog_keypress(0, 0, ConfirmKey::Enter),
        ConfirmKeyResult::Cancel
    );
    // Single-button dialog: Tab cycles to itself (no-op move).
    assert_eq!(
        confirm_dialog_keypress(0, 1, ConfirmKey::Tab),
        ConfirmKeyResult::Move(0)
    );
}

/// With `vim-menu-nav` on, `y` confirms and `n` cancels regardless of
/// focus. They answer the QUESTION, unlike Enter, which fires the focused
/// button.
#[test]
fn confirm_dialog_y_and_n_answer_directly() {
    use super::{ConfirmKey, ConfirmKeyResult, confirm_dialog_keypress};
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::Yes),
        ConfirmKeyResult::Confirm,
        "y must confirm even with Cancel focused"
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Yes),
        ConfirmKeyResult::Confirm
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::No),
        ConfirmKeyResult::Cancel,
        "n must cancel even with Confirm focused"
    );
    // Degenerate zero-button dialog still cancels safely.
    assert_eq!(
        confirm_dialog_keypress(0, 0, ConfirmKey::Yes),
        ConfirmKeyResult::Cancel
    );
}

#[test]
fn confirm_dialog_pointer_targets_match_centered_translated_geometry() {
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let mut dialog = confirmation_test_overlay();
        dialog.buttons[0].label = tr.text(kettle_i18n::Text::ConfirmButtonCancel).into();
        dialog.buttons[1].label = "Cerrar ventana 界".into();
        for surface in [(800.0, 600.0), (192.0, 600.0), (80.0, 100.0)] {
            for index in 0..dialog.buttons.len() {
                let initial =
                    kettle_render::ConfirmDialogLayout::new(surface, (8.25, 16.5), 1.0, &dialog);
                dialog.scroll = initial.scroll_to_button(index);
                let layout =
                    kettle_render::ConfirmDialogLayout::new(surface, (8.25, 16.5), 1.0, &dialog);
                let (x, y, w, h) = layout.buttons[index];
                if w > 0.0 && h > 0.0 {
                    assert_eq!(
                        super::confirm_dialog_button_hit(&layout, x + w * 0.5, y + h * 0.5),
                        Some(index)
                    );
                }
                assert_eq!(super::confirm_dialog_button_hit(&layout, 0.0, 0.0), None);
            }
        }
    }
}

#[test]
fn confirmation_pointer_activation_requires_the_armed_target_and_unchanged_geometry() {
    let dialog = confirmation_test_overlay();
    let layout =
        kettle_render::ConfirmDialogLayout::new((800.0, 600.0), (8.25, 16.5), 1.0, &dialog);
    let center = |(x, y, w, h): (f32, f32, f32, f32)| (x + w / 2.0, y + h / 2.0);
    let (close_x, close_y) = center(layout.buttons[1]);
    let (cancel_x, cancel_y) = center(layout.buttons[0]);
    let armed = Some((1, layout.buttons[1]));
    assert_eq!(
        super::confirm_release_target(armed, &layout, close_x, close_y),
        Some(1)
    );
    assert_eq!(
        super::confirm_release_target(armed, &layout, cancel_x, cancel_y),
        None
    );
    assert_eq!(
        super::confirm_release_target(armed, &layout, 0.0, 0.0),
        None
    );
    assert_eq!(
        super::confirm_release_target(None, &layout, close_x, close_y),
        None
    );
    let resized =
        kettle_render::ConfirmDialogLayout::new((800.0, 700.0), (8.25, 16.5), 1.0, &dialog);
    let (x, y) = center(resized.buttons[1]);
    assert_eq!(super::confirm_release_target(armed, &resized, x, y), None);
}

#[test]
fn confirmation_space_accepts_native_and_control_key_forms() {
    use winit::keyboard::{Key, NamedKey};
    assert!(super::confirmation_space_key(&Key::Character(" ".into())));
    assert!(super::confirmation_space_key(&Key::Named(NamedKey::Space)));
    assert!(!super::confirmation_space_key(&Key::Named(NamedKey::Enter)));
    assert!(!super::confirmation_space_key(&Key::Character("x".into())));
}

#[test]
fn confirmation_space_release_preserves_terminal_ownership() {
    use crate::window_state::{track_consumed_key_release, track_terminal_key_press};
    use std::collections::HashSet;
    use winit::event::ElementState;
    use winit::keyboard::{KeyCode, PhysicalKey};

    let key = PhysicalKey::Code(KeyCode::Space);
    for terminal_press in [false, true] {
        let mut suppressed = HashSet::new();
        let mut terminal = HashSet::new();
        if terminal_press {
            track_terminal_key_press(&mut suppressed, &mut terminal, key);
        }
        // A repeat after the modal opens cannot steal an existing PTY press.
        track_consumed_key_release(
            &mut suppressed,
            &mut terminal,
            key,
            ElementState::Pressed,
            true,
        );
        let ui_owned = track_consumed_key_release(
            &mut suppressed,
            &mut terminal,
            key,
            ElementState::Released,
            true,
        );
        assert_eq!(ui_owned, !terminal_press);
        let mut armed = Some(0);
        assert_eq!(
            super::confirmation_space_release_target(&mut armed, 0, ui_owned),
            (!terminal_press).then_some(0)
        );
        assert!(armed.is_none());
        assert!(suppressed.is_empty() && terminal.is_empty());
    }
    let mut armed = Some(0);
    assert_eq!(
        super::confirmation_space_release_target(&mut armed, 1, true),
        None
    );
    assert!(armed.is_none());
    let source = production_source();
    assert!(source.contains("self.confirm_space_release(ws, release_consumed);\n                    if release_consumed {\n                        return;\n                    }"));
}

#[test]
fn modal_pointer_ownership_blocks_terminal_motion() {
    let src = production_source();
    let movement = src
        .split("WindowEvent::CursorMoved { position, .. } => {")
        .nth(1)
        .unwrap()
        .split("WindowEvent::")
        .next()
        .unwrap();
    let report = movement
        .find("self.send_mouse(ws, input::MOUSE_NO_BUTTON, true, true)")
        .unwrap();
    let condition = &movement[report.saturating_sub(100)..report];
    assert!(
        condition.contains("!self.pointer_modal_open(ws)"),
        "modal pointer ownership must gate terminal motion reports"
    );
}

#[test]
fn finding_modal_blocks_terminal_motion_but_not_release() {
    let body = finding_body(include_str!("pointer.rs"), "send_mouse_to");
    assert!(
        body.contains("motion && self.pointer_modal_open(ws)"),
        "modal pointer ownership must block terminal motion"
    );
}

#[test]
fn review_modals_and_early_releases_end_divider_drags() {
    let src = production_source();
    for name in ["install_confirm_dialog", "close_all_modals"] {
        let body = finding_body(&src, name);
        assert!(body.contains("ws.dragging_split = None;"), "{name}");
    }
    let ctl = finding_body(include_str!("ctl.rs"), "ctl_mouse_release");
    assert!(
        ctl.find("ws.dragging_split = None;").unwrap()
            < ctl.find("self.confirm_mouse_release(").unwrap()
    );
    let native = src
        .split("state: ElementState::Released,\n                button,")
        .nth(1)
        .unwrap()
        .split("WindowEvent::CursorLeft")
        .next()
        .unwrap();
    assert!(
        native.find("ws.dragging_split = None;").unwrap()
            < native
                .find("self.confirm_mouse_release(ws, bcode)")
                .unwrap()
    );
}

#[test]
fn installing_a_preview_overlay_ends_a_divider_drag() {
    let mut ws = WindowState::new(1, Mux::new());
    ws.dragging_split = Some(SplitDrag {
        path: Vec::new(),
        dir: Dir::Horizontal,
    });
    ws.install_preview_overlay(crate::media::Overlay {
        pane: 7,
        origin: Some(3),
        control: None,
        theater: false,
        fullscreen: false,
    });
    assert!(ws.dragging_split.is_none());
    let overlay = ws.preview_overlay.unwrap();
    assert_eq!(overlay.pane, 7);
    assert_eq!(overlay.origin, Some(3));
}

#[test]
fn ready_preview_overlay_ends_divider_drags() {
    let src = production_source();
    let ready = finding_body(&src, "open_ready_previews");
    assert!(ready.contains("self.show_media_item(ws, pane, item)"));
    let show = finding_body(&src, "show_media_item");
    assert!(
        show.contains("ws.install_preview_overlay("),
        "an asynchronous preview must use the shared overlay transition"
    );
}

