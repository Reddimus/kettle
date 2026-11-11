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
fn surface_restart_hint_requires_a_successful_write_and_a_missing_capability() {
    let src = production_source();
    let normalized = src
        .split("let saved = self.persist_pref(key_str, &new_val);")
        .nth(1)
        .expect("settings persistence arm")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // Up to the end of `settings_store`: the next item starts with `fn`.
    let arm = normalized
        .split(" fn ")
        .next()
        .expect("settings_store body");
    assert!(
        arm.contains("\"background-opacity\" | \"background-type\" | \"window-blur\""),
        "the surface settings are background opacity, type and blur"
    );
    // The hint is decided against the reloaded config, after the reload, and
    // only when the window was created without what that config needs.
    let (_, after_reload) = arm
        .split_once("self.reload_config(ws);")
        .expect("reload in the persistence arm");
    assert!(
        after_reload.contains(
            "if saved && surface_setting && ws.surface_caps.lacks_for(&self.cfg) { \
             ws.settings_restart_pending = true; }"
        ),
        "a surface change the window can already show must not ask for a new window, \
         and a failed write must not promise one"
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
        ConfirmKeyResult::Activate(0),
        "Enter on the highlighted Cancel button must cancel, not confirm"
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Enter),
        ConfirmKeyResult::Activate(1),
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

#[test]
fn ssh_creation_reports_tab_add_only_on_success() {
    let body = finding_body(include_str!("modals.rs"), "ssh_key");
    let creation = body
        .split(".new_ssh_tab_geometry(")
        .nth(1)
        .unwrap()
        .split("self.resize_all(ws);")
        .next()
        .unwrap();
    assert_eq!(
        creation.matches("self.fire_tab_add_event(ws)").count(),
        1,
        "successful SSH tabs must emit one tab-add event"
    );
    assert!(
        creation.contains("Ok(_) => self.fire_tab_add_event(ws)"),
        "failed SSH creation must not emit tab-add"
    );
}

#[test]
fn failed_settings_saves_do_not_claim_session_application_or_discard_text() {
    let source = include_str!("modals.rs");
    for function in ["settings_adjust", "settings_text_key"] {
        let body = finding_body(source, function);
        assert!(
            !body.contains("Text::NotifyBodyNotSaved"),
            "{function} must not claim an unsaved edit was applied for this session"
        );
    }
    let body = finding_body(source, "settings_text_key");
    let enter = body
        .split("Key::Named(NamedKey::Enter)")
        .nth(1)
        .unwrap()
        .split("Key::Character")
        .next()
        .unwrap();
    assert!(
        enter
            .find("ws.settings_text_edit = None")
            .is_some_and(|close| enter.find("self.persist_pref").unwrap() < close),
        "the inline editor must retain its buffer until persistence succeeds"
    );
    assert!(
        enter
            .split("if !self.persist_pref")
            .nth(1)
            .unwrap()
            .split("ws.settings_text_edit = None")
            .next()
            .unwrap()
            .contains("return;")
    );
}

#[test]
fn paste_confirmation_enter_activates_each_of_three_buttons() {
    for index in 0..3 {
        assert_eq!(
            super::confirm_dialog_keypress(index, 3, super::ConfirmKey::Enter),
            super::ConfirmKeyResult::Activate(index)
        );
    }
    assert_eq!(
        super::confirm_dialog_keypress(2, 3, super::ConfirmKey::Tab),
        super::ConfirmKeyResult::Move(0)
    );
    assert_eq!(
        super::confirm_dialog_keypress(0, 3, super::ConfirmKey::ShiftTab),
        super::ConfirmKeyResult::Move(2)
    );
}

/// The default policy asks before closing a lone pane that a job owns, and
/// only then: an idle prompt, a pane the PTY cannot read, and every explicit
/// mode keep their old answers. `busy` is 1 throughout, as for a shell with no
/// OSC 133 integration, which is why it cannot decide this by itself.
#[test]
fn a_lone_busy_pane_prompts_only_under_the_default_policy() {
    use kettle_config::AskBeforeClosing;
    let default = kettle_config::Config::default().ask_before_closing;
    assert!(super::close_needs_prompt(default, 1, 1, || super::LonePaneJob::Busy));
    assert!(!super::close_needs_prompt(default, 1, 1, || super::LonePaneJob::Idle));
    assert!(!super::close_needs_prompt(default, 1, 1, || super::LonePaneJob::Unknown));
    // OSC 133 calling the pane idle does not hide a job the PTY can see.
    assert!(super::close_needs_prompt(default, 1, 0, || super::LonePaneJob::Busy));
    // Several panes keep the count rule and never ask the PTY.
    assert!(super::close_needs_prompt(default, 2, 1, || unreachable!()));
    assert!(!super::close_needs_prompt(default, 3, 0, || super::LonePaneJob::Idle));

    for (policy, single_busy) in [
        (AskBeforeClosing::Never, false),
        (AskBeforeClosing::MultipleTerminals, false),
        (AskBeforeClosing::Always, true),
    ] {
        let asked = std::cell::Cell::new(false);
        let job = || {
            asked.set(true);
            super::LonePaneJob::Busy
        };
        assert_eq!(super::close_needs_prompt(policy, 1, 1, job), single_busy, "{policy:?}");
        assert!(!asked.get(), "{policy:?} must not sample the PTY");
    }
    assert!(!super::close_needs_prompt(AskBeforeClosing::Never, 1, 0, || super::LonePaneJob::Busy));
    assert!(!super::close_needs_prompt(AskBeforeClosing::Never, 5, 5, || super::LonePaneJob::Busy));
}

/// The titlebar ✕ and Alt+F4 ask for a one-pane window that runs a program
/// under the default policy, as Terminal.app and iTerm2 do on their close
/// button, and only then: an idle or unreadable lone pane closes at once, a
/// window of several panes still closes without asking unless the policy is
/// `always`, and `multiple-terminals` and `never` keep the old exemption.
#[test]
fn the_titlebar_close_asks_for_a_lone_busy_pane_under_the_default() {
    use super::LonePaneJob::{Busy, Idle, Unknown};
    use kettle_config::AskBeforeClosing;
    let titlebar = |policy: AskBeforeClosing, scope: usize, busy: usize, job| {
        super::titlebar_close_consults_policy(policy, scope)
            && super::close_needs_prompt(policy, scope, busy, || job)
    };
    let default = kettle_config::Config::default().ask_before_closing;
    assert!(titlebar(default, 1, 1, Busy));
    assert!(titlebar(default, 1, 0, Busy));
    assert!(!titlebar(default, 1, 1, Idle));
    assert!(!titlebar(default, 1, 1, Unknown));
    assert!(!titlebar(default, 3, 3, Busy), "several panes close without asking");

    assert!(titlebar(AskBeforeClosing::Always, 1, 1, Idle));
    assert!(titlebar(AskBeforeClosing::Always, 3, 1, Idle));
    for policy in [AskBeforeClosing::MultipleTerminals, AskBeforeClosing::Never] {
        assert!(!titlebar(policy, 1, 1, Busy), "{policy:?}");
        assert!(!titlebar(policy, 3, 3, Busy), "{policy:?}");
    }
}

#[test]
fn confirm_close_decides_through_the_shared_policy() {
    let src = include_str!("modals.rs");
    let body = src
        .split("pub(super) fn confirm_close(")
        .nth(1)
        .unwrap()
        .split("\n    }\n")
        .next()
        .unwrap();
    assert!(body.contains("close_needs_prompt(self.cfg.ask_before_closing, scope, busy"));
    assert!(body.contains("lone_pane_job(&pane.term, pane.path_argv(), configured_shell)"));
    assert!(!body.contains("should_prompt("), "the decision lives in close_needs_prompt");
}

#[test]
fn the_lone_pane_is_the_one_the_gesture_named() {
    let ws = WindowState::new(1, Mux::new());
    assert_eq!(super::lone_pane_in_scope(&ws, 1, &super::ConfirmAction::ClosePane(7)), Some(7));
    assert_eq!(super::lone_pane_in_scope(&ws, 2, &super::ConfirmAction::ClosePane(7)), None);
    assert_eq!(
        super::lone_pane_in_scope(&ws, 1, &super::ConfirmAction::CloseTabHolding(vec![3])),
        Some(3)
    );
    assert_eq!(
        super::lone_pane_in_scope(&ws, 1, &super::ConfirmAction::CloseTabHolding(vec![3, 4])),
        None
    );
    assert_eq!(
        super::lone_pane_in_scope(
            &ws,
            1,
            &super::ConfirmAction::CloseWindow {
                drop_panes: super::DropPanes::Yes
            }
        ),
        None
    );
}

#[test]
fn a_foreground_group_other_than_the_spawned_one_is_a_job() {
    use kettle_core::ForegroundJob;
    let never = |_: u32| -> bool { unreachable!("the leader is read only for a wrapper") };
    assert_eq!(
        super::classify_lone_pane_job(ForegroundJob::Child, false, || unreachable!(), never),
        super::LonePaneJob::Idle
    );
    assert_eq!(
        super::classify_lone_pane_job(ForegroundJob::Unknown, false, || unreachable!(), never),
        super::LonePaneJob::Unknown
    );
    // The launch is the shell itself: any other group is a job, even one
    // that is a shell (a nested `bash` the user started).
    assert_eq!(
        super::classify_lone_pane_job(ForegroundJob::Other(42), false, || true, never),
        super::LonePaneJob::Busy
    );
    // A wrapper that did not exec: its shell in its own group is idle...
    assert_eq!(
        super::classify_lone_pane_job(ForegroundJob::Other(42), false, || false, |pid| pid == 42),
        super::LonePaneJob::Idle
    );
    // ...unless OSC 133 says that shell is running a command,
    assert_eq!(
        super::classify_lone_pane_job(ForegroundJob::Other(42), true, || false, never),
        super::LonePaneJob::Busy
    );
    // and a non-shell leader is a job.
    assert_eq!(
        super::classify_lone_pane_job(ForegroundJob::Other(42), false, || false, |_| false),
        super::LonePaneJob::Busy
    );
}

#[test]
fn only_an_interactive_shell_leader_counts_as_the_prompt() {
    let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let shells = "# /etc/shells\n/bin/bash\n/bin/zsh\n/bin/sh\n/opt/local/bin/oil\n";
    for idle in [
        &["-zsh"][..],
        &["/bin/zsh"],
        &["zsh", "-l"],
        &["-bash"],
        &["/opt/local/bin/oil"],
        &["fish"],
    ] {
        assert!(super::argv_is_interactive_shell(&argv(idle), shells, None), "{idle:?}");
    }
    for busy in [
        &["vim", "notes.txt"][..],
        &["sleep", "600"],
        &["/bin/bash", "build.sh"],
        &["sh", "-c", "make"],
        &["-zsh", "-c", "make"],
        &["/opt/local/bin/oil", "script.oil"],
        &["login", "-pfl", "me", "/bin/zsh"],
        &[],
        &["-"],
    ] {
        assert!(!super::argv_is_interactive_shell(&argv(busy), shells, None), "{busy:?}");
    }
    // The configured shell counts even when /etc/shells does not list it.
    assert!(!super::argv_is_interactive_shell(&argv(&["myshell"]), "", None));
    assert!(super::argv_is_interactive_shell(
        &argv(&["myshell"]),
        "",
        Some("/home/me/bin/myshell")
    ));
}

/// The close question is read by people who do not know which terminal they
/// are using, so neither locale may name the product in it, including the
/// one-pane wording a lone busy pane now shows.
#[test]
fn close_dialog_text_never_names_the_product() {
    use kettle_i18n::Text;
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let mut lines: Vec<String> = [
            Text::ConfirmTitleCloseWindow,
            Text::ConfirmTitleCloseTab,
            Text::ConfirmTitleClosePane,
            Text::ConfirmButtonCloseWindow,
            Text::ConfirmButtonCloseTab,
            Text::ConfirmButtonClosePane,
            Text::ConfirmButtonCancel,
            Text::ConfirmCloseConsequences,
            Text::ConfirmCloseBothPanes,
        ]
        .into_iter()
        .map(|text| tr.text(text).to_string())
        .collect();
        for count in [1, 2, 3] {
            lines.push(tr.confirm_close_scope(count));
            lines.push(tr.confirm_close_tab(count));
            lines.push(tr.confirm_close_window(count));
        }
        for line in lines {
            assert!(!line.is_empty());
            assert!(
                !line.to_lowercase().contains("kettle"),
                "{language:?}: {line:?}"
            );
        }
    }
}

/// Real PTYs: an idle shell is idle, `sleep` started a moment ago is busy,
/// and a wrapper that does not `exec` leaves its interactive shell idle.
#[cfg(unix)]
#[test]
fn a_lone_pane_is_busy_exactly_while_a_job_owns_its_terminal() {
    type Spawned = (
        kettle_core::Terminal,
        crossbeam_channel::Receiver<kettle_core::TermEvent>,
    );
    fn spawn(argv: &[String]) -> Option<Spawned> {
        let (tx, rx) = crossbeam_channel::unbounded();
        match kettle_core::Terminal::new(
            argv,
            None,
            0,
            80,
            24,
            8,
            16,
            false,
            Default::default(),
            None,
            tx,
            std::sync::Arc::new(|| {}),
        ) {
            Ok(terminal) => Some((terminal, rx)),
            Err(error) => {
                eprintln!("skipping lone-pane job test: no PTY ({error})");
                None
            }
        }
    }
    fn wait_until(
        terminal: &kettle_core::Terminal,
        events: &crossbeam_channel::Receiver<kettle_core::TermEvent>,
        mut done: impl FnMut(&kettle_core::Terminal) -> bool,
    ) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            while let Ok(event) = events.try_recv() {
                if let kettle_core::TermEvent::PtyWrite(reply) = event {
                    terminal.write(reply.as_bytes());
                }
            }
            if done(terminal) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }
    fn on_screen(terminal: &kettle_core::Terminal, needle: &str) -> bool {
        terminal
            .screen_text(0)
            .is_some_and(|screen| screen.text.contains(needle))
    }

    #[cfg(target_os = "macos")]
    let shell = vec!["/bin/zsh".to_string(), "-f".to_string()];
    #[cfg(not(target_os = "macos"))]
    let shell = vec![
        "/bin/bash".to_string(),
        "--noprofile".to_string(),
        "--norc".to_string(),
    ];
    // `; :` keeps sh from exec-ing the shell, so sh stays the pane's child
    // and the interactive shell runs in a process group of its own.
    let wrapper = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("{}; :", shell.join(" ")),
    ];
    for (launch, wrapped) in [(shell, false), (wrapper, true)] {
        let Some((terminal, rx)) = spawn(&launch) else {
            return;
        };
        let job = |terminal: &kettle_core::Terminal| super::lone_pane_job(terminal, &launch, None);
        // Split the marker so the echo of the typed line cannot match it.
        terminal.write(b"printf 'LONE_PANE''_READY\\n'\r");
        assert!(
            wait_until(&terminal, &rx, |t| on_screen(t, "LONE_PANE_READY")),
            "{launch:?}"
        );
        assert!(
            wait_until(&terminal, &rx, |t| job(t) == super::LonePaneJob::Idle),
            "{launch:?}: an idle shell must not count as busy"
        );
        // The wrapped shell is idle through the fallback, not because it is
        // the child: its group really is a different one.
        assert_eq!(
            matches!(terminal.foreground_job(), kettle_core::ForegroundJob::Other(_)),
            wrapped,
            "{launch:?}"
        );
        terminal.write(b"sleep 30\r");
        assert!(
            wait_until(&terminal, &rx, |t| job(t) == super::LonePaneJob::Busy),
            "{launch:?}: a running sleep must count as busy"
        );
        // Interrupt the job and leave the shell, so nothing outlives the test.
        terminal.write(b"\x03");
        assert!(
            wait_until(&terminal, &rx, |t| job(t) == super::LonePaneJob::Idle),
            "{launch:?}: the shell is idle again once the job ends"
        );
        terminal.write(b"exit\r");
    }
}
