use super::*;
use crate::text_editor::TextEditor;

fn field(kind: ModalKind) -> WindowState {
    let mut ws = WindowState::new(0, Mux::new());
    let editor = TextEditor::from("a\u{301}\u{1f469}\u{200d}\u{1f4bb}\u{65e5}z");
    match kind {
        ModalKind::Palette => ws.palette_input = Some((editor, 3)),
        ModalKind::Theme => {
            ws.theme_picker_input = Some(crate::window_state::ThemePickerState {
                query: editor,
                selected: 3,
                opened_on: "Builtin Dark".into(),
            })
        }
        ModalKind::Layout => ws.layout_picker_input = Some((editor, 3)),
        ModalKind::Ssh => ws.ssh_input = Some((editor, 3)),
        ModalKind::Title => {
            ws.editing_title = Some(TitleEditState {
                input: editor,
                scope: TitleEditScope::Window,
                target: TitleEditTarget::Window,
                bulk: GroupBulkScope::Single,
            })
        }
        ModalKind::SettingsCapture => {
            ws.settings_nav = Some(crate::settings::SettingsNav::default());
            ws.settings_text_edit = Some(crate::settings::SettingsTextEdit {
                key: "background-image",
                buf: editor,
            });
        }
        ModalKind::Settings => ws.settings_nav = Some(crate::settings::SettingsNav::default()),
        ModalKind::Hint => ws.hint_state = Some((Vec::new(), String::new())),
        ModalKind::ContextMenu => {
            ws.context_menu = Some(ContextMenuState {
                anchor: (10.0, 10.0),
                anchor_scale: 1.0,
                items: Vec::new(),
                highlight: 0,
                drill_stack: Vec::new(),
                scroll_offset: 0,
                typeahead_buf: String::new(),
                typeahead_until: None,
            })
        }
        ModalKind::Confirmation => {
            ws.confirm_dialog = Some(ConfirmDialogState {
                heading: String::new(),
                body: String::new(),
                buttons: vec![ConfirmButton::Cancel],
                focus_idx: 0,
                on_confirm: ConfirmAction::ClosePane(1),
                scroll: 0,
                armed_press: None,
                space_pressed: None,
            })
        }
    }
    ws
}

const FIELDS: [ModalKind; 6] = [
    ModalKind::Palette,
    ModalKind::Theme,
    ModalKind::Layout,
    ModalKind::Ssh,
    ModalKind::Title,
    ModalKind::SettingsCapture,
];

#[test]
fn review_settings_rejects_oversized_initial_paths() {
    let cap = crate::modal_input::MAX_MODAL_INPUT_BYTES;
    for path in ["x".repeat(cap + 1), "\u{e9}".repeat(cap / 2 + 1)] {
        assert!(
            modals::begin_settings_text_edit("background-image", &path).is_none(),
            "an oversized saved path must not become an empty committable editor"
        );
    }
    for path in [String::new(), "x".repeat(cap), "\u{e9}".repeat(cap / 2)] {
        let edit = modals::begin_settings_text_edit("background-image", &path).unwrap();
        assert_eq!(edit.key, "background-image");
        assert_eq!(edit.buf.text(), path);
        assert_eq!(edit.buf.cursor(), path.len());
    }
}

#[test]
fn review_settings_value_slot_uses_natural_font_width() {
    let set = kettle_render::SettingsOverlay {
        categories: vec!["Background".into()],
        label_cols: 12,
        value_cols: 20,
        active_category: 0,
        background_page: true,
        rows: vec![kettle_render::SettingsRow {
            label: "Image path".into(),
            value: String::new(),
            disabled: false,
        }],
        focused_row: 0,
        title: "Settings".into(),
        hints: String::new(),
        footer_note: None,
    };
    let text_cw = 10.0;
    for (scale, prefix_cols) in [(0.5, 14.0), (1.0, 16.0), (2.0, 16.0)] {
        let cw = text_cw * scale;
        let panel = kettle_render::settings_panel_geometry(&set, cw, 20.0, 1600.0, 600.0);
        let value = kettle_render::settings_value_rect(&set, cw, 20.0, 1600.0, 600.0, text_cw);
        // Narrow panels shorten the label to ten columns. The marker and
        // gap add four more, all in the font's natural advance.
        assert_eq!(
            value.0,
            panel.rect.0 + 16.0 + prefix_cols * text_cw,
            "scale {scale}"
        );
        assert!(value.2 > 0.0);
        assert!(value.0 + value.2 <= panel.rect.0 + panel.rect.2 - 16.0);
        assert_eq!(value.1, panel.focused_rect.1);
    }
}

#[test]
fn review_pointer_cancels_platform_composition_before_moving_caret() {
    use std::cell::{Cell, RefCell};

    for kind in FIELDS {
        let mut ws = field(kind);
        *modal_editor_mut(&mut ws).unwrap() = "ab".into();
        update_ime_preedit(&mut ws, "phrase".into(), None);
        let generation = ws.ime_focus_generation;
        let calls = RefCell::new(Vec::new());
        let allowed = Cell::new(true);
        let platform_phrase = Cell::new(Some("phrase"));
        modals::cancel_modal_pointer_composition(
            &mut ws,
            |value| {
                calls
                    .borrow_mut()
                    .push(if value { "enable" } else { "disable" });
                allowed.set(value);
                // winit 0.30.13 clears its own buffer on disable, but AppKit's
                // input context retains the unfinished phrase across the toggle.
            },
            || {
                assert!(!allowed.get());
                calls.borrow_mut().push("discard");
                platform_phrase.set(None);
            },
        );
        assert_eq!(modal_editor(&ws).unwrap().cursor(), 2);
        assert!(ws.ime_preedit.is_none());
        assert_ne!(ws.ime_focus_generation, generation);
        modal_editor_mut(&mut ws)
            .unwrap()
            .set_cursor_at(crate::text_editor::QueryPoint::Byte(0), false);
        // An uncancelled input method can continue its phrase after the click.
        if let Some(phrase) = platform_phrase.get() {
            update_ime_preedit(&mut ws, phrase.into(), None);
            update_ime_preedit(&mut ws, String::new(), None);
            if take_ime_commit(&mut ws) {
                crate::modal_input::push_text(modal_editor_mut(&mut ws).unwrap(), phrase);
            }
        }
        assert_eq!(modal_editor(&ws).unwrap().text(), "ab", "{kind:?}");
        assert_eq!(
            calls.into_inner(),
            ["disable", "discard", "enable"],
            "{kind:?}: discard the native phrase before re-enabling IME"
        );
        assert!(allowed.get());
        assert!(
            !take_ime_commit(&mut ws),
            "a queued old commit must be dropped"
        );

        // After the platform discards its old phrase, the next composition
        // belongs to the new caret location and commits there normally.
        update_ime_preedit(&mut ws, "new".into(), None);
        update_ime_preedit(&mut ws, String::new(), None);
        assert!(take_ime_commit(&mut ws));
        crate::modal_input::push_text(modal_editor_mut(&mut ws).unwrap(), "new");
        assert_eq!(modal_editor(&ws).unwrap().text(), "newab");
        let generation = ws.ime_focus_generation;
        modals::cancel_modal_pointer_composition(
            &mut ws,
            |_| panic!("no active phrase"),
            || panic!("no native phrase to discard"),
        );
        assert_eq!(ws.ime_focus_generation, generation);
    }
}

#[test]
fn every_modal_field_edits_and_projects_mid_string_composition() {
    for kind in FIELDS {
        let mut ws = field(kind);
        let editor = modal_editor_mut(&mut ws).unwrap();
        editor.move_left(false, false);
        editor.move_left(true, false);
        assert_eq!(editor.selected_text(), Some("\u{65e5}"));
        let preedit = ("\u{4e2d}\u{6587}".to_string(), Some((0, 3)));
        let (text, cursor, selection) = project_modal_editor(editor, Some(&preedit));
        assert_eq!(text, "a\u{301}\u{1f469}\u{200d}\u{1f4bb}\u{4e2d}\u{6587}z");
        assert_eq!(&text[cursor..], "\u{6587}z");
        assert_eq!(selection, None);
        assert_eq!(
            editor.selected_text(),
            Some("\u{65e5}"),
            "projection does not commit"
        );
        editor.insert(&preedit.0, 4096);
        assert_eq!(editor.text(), text);
        editor.move_home(false);
        editor.delete();
        assert!(editor.text().starts_with('\u{1f469}'));
        editor.delete();
        assert_eq!(editor.text(), "\u{4e2d}\u{6587}z");
    }
}

#[test]
fn modal_fields_share_keyboard_commands_and_clipboard_failure_semantics() {
    use winit::keyboard::{Key, NamedKey};
    let shortcut = if cfg!(target_os = "macos") {
        ModifiersState::SUPER
    } else {
        ModifiersState::CONTROL
    };
    let word = if cfg!(target_os = "macos") {
        ModifiersState::ALT
    } else {
        ModifiersState::CONTROL
    };
    for kind in FIELDS {
        let mut ws = field(kind);
        let editor = modal_editor_mut(&mut ws).unwrap();
        let edit = |editor: &mut TextEditor, key: Key, mods, copy_ok| {
            crate::modal_input::edit_key(editor, &key, mods, || Some("one two".into()), |_| copy_ok)
        };
        assert!(edit(editor, Key::Character("a".into()), shortcut, true));
        let original = editor.text().to_string();
        edit(editor, Key::Character("x".into()), shortcut, false);
        assert_eq!(editor.text(), original);
        edit(editor, Key::Character("v".into()), shortcut, true);
        assert_eq!(editor.text(), "one two");
        edit(editor, Key::Named(NamedKey::ArrowLeft), word, false);
        assert_eq!(&editor.text()[editor.cursor()..], "two");
        edit(editor, Key::Named(NamedKey::Delete), word, false);
        assert_eq!(editor.text(), "one ");
        edit(
            editor,
            Key::Named(NamedKey::Home),
            ModifiersState::empty(),
            false,
        );
        edit(
            editor,
            Key::Named(NamedKey::ArrowRight),
            ModifiersState::SHIFT,
            false,
        );
        assert_eq!(editor.selected_text(), Some("o"));
        edit(editor, Key::Character("x".into()), shortcut, true);
        assert_eq!(editor.text(), "ne ");
    }
}

#[test]
fn shared_outside_dispatch_dismisses_each_transient_and_consumes_the_click() {
    for kind in
        FIELDS
            .into_iter()
            .chain([ModalKind::Settings, ModalKind::Hint, ModalKind::ContextMenu])
    {
        for button in [0, 1, 2, 8] {
            let mut ws = field(kind);
            let generation = ws.ime_focus_generation;
            let consumed = dispatch_modal_press(&mut ws, kind, false, button);
            assert_eq!(
                consumed,
                kind != ModalKind::ContextMenu || button != 2,
                "{kind:?}/{button}"
            );
            assert!(modal_editor(&ws).is_none());
            assert!(ws.hint_state.is_none());
            assert!(ws.context_menu.is_none());
            assert_ne!(ws.ime_focus_generation, generation);
            if kind == ModalKind::Title {
                assert!(ws.pending_resize);
            }
            if kind == ModalKind::SettingsCapture {
                assert!(ws.settings_nav.is_some());
            }
        }
    }
}

#[test]
fn inside_presses_keep_fields_and_outside_presses_keep_confirmation() {
    for kind in FIELDS {
        let mut ws = field(kind);
        assert!(!dispatch_modal_press(&mut ws, kind, true, 0));
        assert!(modal_editor(&ws).is_some());
    }
    let mut ws = field(ModalKind::Confirmation);
    assert!(dispatch_modal_press(
        &mut ws,
        ModalKind::Confirmation,
        false,
        0
    ));
    assert!(ws.confirm_dialog.is_some());
}

#[cfg(target_os = "macos")]
#[test]
fn modal_word_editing_uses_physical_option_after_native_masking() {
    for kind in FIELDS {
        for policy in [
            MacosOptionAsAlt::None,
            MacosOptionAsAlt::Left,
            MacosOptionAsAlt::Right,
            MacosOptionAsAlt::Both,
        ] {
            for left in [false, true] {
                for shift in [false, true] {
                    for (key, cursor, expected_cursor, expected_text) in [
                        (NamedKey::ArrowLeft, 7, 4, "one two"),
                        (NamedKey::ArrowRight, 0, 4, "one two"),
                        (NamedKey::Backspace, 7, 4, "one "),
                        (NamedKey::Delete, 0, 0, "two"),
                    ] {
                        let mut ws = field(kind);
                        ws.macos_raw_mods = ModifiersState::ALT;
                        ws.macos_raw_mods.set(ModifiersState::SHIFT, shift);
                        ws.macos_left_option_pressed = left;
                        ws.macos_right_option_pressed = !left;
                        refresh_macos_modifiers(&mut ws, policy);
                        let editor = modal_editor_mut(&mut ws).unwrap();
                        *editor = "one two".into();
                        editor.set_cursor_at(crate::text_editor::QueryPoint::Byte(cursor), false);
                        assert!(modals::edit_modal_text(
                            &mut ws,
                            &Key::Named(key),
                            || None,
                            |_| false
                        ));
                        let editor = modal_editor(&ws).unwrap();
                        assert_eq!(editor.text(), expected_text, "{kind:?}/{policy:?}/{key:?}");
                        assert_eq!(
                            editor.cursor(),
                            expected_cursor,
                            "{kind:?}/{policy:?}/{key:?}"
                        );
                        if shift && matches!(key, NamedKey::ArrowLeft | NamedKey::ArrowRight) {
                            assert_eq!(
                                editor.selection(),
                                Some(cursor.min(expected_cursor)..cursor.max(expected_cursor))
                            );
                        }
                        assert!(!modals::edit_modal_text(
                            &mut ws,
                            &Key::Character("\u{e9}".into()),
                            || None,
                            |_| false
                        ));
                        assert_eq!(
                            ws.mods,
                            macos_effective_modifiers(ws.macos_raw_mods, policy, left, !left)
                        );
                    }
                }
            }
        }
    }
}
