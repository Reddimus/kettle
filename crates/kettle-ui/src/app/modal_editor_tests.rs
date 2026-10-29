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
