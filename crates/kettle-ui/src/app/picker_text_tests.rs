use super::{
    command_picker_list, layout_picker_list, modal_accessibility_projection, picker_hint,
    ssh_picker_list,
};
use kettle_i18n::{Language, Text, Translator};

const EN: Translator = Translator::new(Language::En);
const ES: Translator = Translator::new(Language::Es);

#[test]
fn picker_hints_keep_their_keys_and_translate_their_actions() {
    let palette = [
        ("Enter", Text::PickerHintRun),
        ("Tab/↑↓", Text::PickerHintSelect),
        ("Esc", Text::PickerHintCancel),
    ];
    assert_eq!(
        picker_hint(&EN, &palette),
        "(Enter run · Tab/↑↓ select · Esc cancel)"
    );
    assert_eq!(
        picker_hint(&ES, &palette),
        "(Enter ejecutar · Tab/↑↓ seleccionar · Esc cancelar)"
    );
    let ssh = [
        ("Enter", Text::PickerHintConnect),
        ("Tab", Text::PickerHintComplete),
        ("↑↓", Text::PickerHintSelect),
        ("Esc", Text::PickerHintCancel),
    ];
    assert_eq!(
        picker_hint(&EN, &ssh),
        "(Enter connect · Tab complete · ↑↓ select · Esc cancel)"
    );
    let layouts = [
        ("Enter", Text::PickerHintSpawn),
        ("Tab/↑↓", Text::PickerHintSelect),
        ("Esc", Text::PickerHintCancel),
    ];
    assert_eq!(
        picker_hint(&EN, &layouts),
        "(Enter spawn · Tab/↑↓ select · Esc cancel)"
    );
}

/// Screen readers name each picker dialog in the UI's language.
#[test]
fn picker_accessibility_names_follow_the_ui_language() {
    let names = |overlay: kettle_render::Overlay, tr: &Translator| {
        let projection =
            modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, tr);
        projection
            .nodes
            .iter()
            .filter_map(|(_, node)| node.label().map(str::to_string))
            .collect::<Vec<_>>()
    };
    let palette = || kettle_render::Overlay {
        palette_query: Some("x".into()),
        ..kettle_render::Overlay::default()
    };
    assert!(names(palette(), &EN).iter().any(|n| n == "Command palette"));
    assert!(
        names(palette(), &ES)
            .iter()
            .any(|n| n == "Paleta de comandos")
    );
    let layouts = kettle_render::Overlay {
        layout_picker_query: Some(String::new()),
        ..kettle_render::Overlay::default()
    };
    assert!(
        names(layouts, &ES)
            .iter()
            .any(|n| n == "Selector de diseños")
    );
    let ssh = kettle_render::Overlay {
        ssh_query: Some(String::new()),
        ..kettle_render::Overlay::default()
    };
    assert!(names(ssh, &ES).iter().any(|n| n == "Lanzador SSH"));
}

/// The right-click menu, quick-select hints, the update banner and each
/// picker's result list are named in the UI's language.
#[test]
fn modal_accessibility_names_follow_the_ui_language() {
    let labels = |overlay: kettle_render::Overlay, tr: &Translator| {
        let projection =
            modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, tr);
        projection
            .nodes
            .iter()
            .flat_map(|(_, node)| {
                [node.label(), node.description()]
                    .into_iter()
                    .flatten()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let menu = || kettle_render::Overlay {
        context_menu: Some(kettle_render::ContextMenu {
            rows: Vec::new(),
            highlight: 0,
            anchor: (0.0, 0.0),
            scroll_offset: 0,
            panel_w_clamped: 100.0,
            panel_h_clamped: 100.0,
            a11y_label: None,
            picker_header_height: 0.0,
            picker_footer: None,
        }),
        ..kettle_render::Overlay::default()
    };
    assert!(labels(menu(), &EN).iter().any(|l| l == "Terminal menu"));
    assert!(
        labels(menu(), &ES)
            .iter()
            .any(|l| l == "Menú de la terminal")
    );
    // The About panel reuses the menu but is announced by its own name.
    for (tr, name) in [(&EN, "About Kettle"), (&ES, "Acerca de Kettle")] {
        let mut about = menu();
        if let Some(menu) = about.context_menu.as_mut() {
            menu.a11y_label = Some(tr.text(Text::MenuA11yAbout).to_string());
        }
        let labels = labels(about, tr);
        assert!(labels.iter().any(|l| l == name), "{labels:?}");
        assert!(!labels.iter().any(|l| l == tr.text(Text::MenuA11yTerminal)));
    }
    // Opening a menu clears its name, so the About panel names itself after.
    let source = super::production_source();
    let about = &source[source
        .find("    fn open_about_panel(")
        .expect("open_about_panel")..];
    let about = &about[..about.find("\n    }\n").expect("end of open_about_panel")];
    let shown = about
        .find("self.show_context_menu(ws, items,")
        .expect("the panel is a menu");
    let named = about
        .find("ws.context_menu_a11y_label = Some(tr.text(T::MenuA11yAbout).to_string());")
        .expect("the panel is named");
    assert!(shown < named);
    let hints = kettle_render::Overlay {
        hint_labels: vec![kettle_render::HintLabel {
            row: 0,
            col: 0,
            label: "a".into(),
            dim: false,
        }],
        ..kettle_render::Overlay::default()
    };
    assert!(
        labels(hints, &ES)
            .iter()
            .any(|l| l == "Etiquetas de selección rápida")
    );
    let palette = kettle_render::Overlay {
        palette_query: Some(String::new()),
        ..kettle_render::Overlay::default()
    };
    assert!(
        labels(palette, &ES)
            .iter()
            .any(|l| l == "Paleta de comandos: resultados")
    );
}

/// The title editor's hint is built like the pickers', from its keys and
/// what they do.
#[test]
fn the_title_editor_hint_keeps_its_keys() {
    let hint = [
        ("Enter", Text::EditHintApply),
        ("Esc", Text::PickerHintCancel),
    ];
    assert_eq!(picker_hint(&EN, &hint), "(Enter apply · Esc cancel)");
    assert_eq!(picker_hint(&ES, &hint), "(Enter aplicar · Esc cancelar)");
}

/// The input-method window opens at the painted caret, whatever the
/// label's language: the caret follows the prefix the renderer paints.
#[test]
fn the_title_editor_caret_follows_the_painted_prefix() {
    use super::{TitleEditScope, title_edit_caret_cols};
    use unicode_width::UnicodeWidthStr as _;
    for tr in [EN, ES] {
        for (scope, text) in [
            (TitleEditScope::Window, Text::EditTitleWindow),
            (TitleEditScope::Tab, Text::EditTitleTab),
            (TitleEditScope::Pane, Text::EditTitlePane),
            (TitleEditScope::Group, Text::EditTitleGroup),
            (TitleEditScope::SaveLayout, Text::EditLayoutName),
            (TitleEditScope::SaveSshHost, Text::EditSshHostName),
        ] {
            let painted = format!("  ✎ {} 界x", tr.text(text));
            assert_eq!(
                title_edit_caret_cols(scope, "界x", "", &tr),
                painted.width()
            );
        }
    }
    assert_eq!(
        title_edit_caret_cols(TitleEditScope::Window, "", "", &EN),
        23
    );
    assert_eq!(
        title_edit_caret_cols(TitleEditScope::Window, "", "", &ES),
        36
    );
}

#[test]
fn picker_rows_and_empty_states_follow_the_ui_language() {
    let bindings = kettle_config::keybinds::defaults();
    assert_eq!(
        command_picker_list(
            "",
            0,
            &bindings,
            &ES,
            None,
            &[],
            false,
            kettle_config::BroadcastDefault::default(),
        )
        .rows[0]
            .label,
        "Nueva pestaña"
    );
    let none = command_picker_list(
        "zzzqqq",
        0,
        &bindings,
        &ES,
        None,
        &[],
        false,
        kettle_config::BroadcastDefault::default(),
    );
    assert_eq!(none.rows[0].label, "Ningún comando coincide");
    assert!(!none.rows[0].enabled);
    assert_eq!(
        command_picker_list(
            "zzzqqq",
            0,
            &bindings,
            &EN,
            None,
            &[],
            false,
            kettle_config::BroadcastDefault::default(),
        )
        .rows[0]
            .label,
        "No matching command"
    );

    let empty = layout_picker_list("", 0, &[], &ES);
    assert_eq!(empty.rows[0].label, "No hay diseños guardados");
    // The command stays as typed; only the workspace name's placeholder is
    // translated.
    assert_eq!(
        empty.rows[0].hint,
        "Usar Guardar diseño… en el menú de nueva pestaña"
    );
    assert_eq!(
        layout_picker_list("", 0, &[], &EN).rows[0].hint,
        "Use Save Layout… in the new-tab menu"
    );
    let layouts = ["dev".to_string()];
    assert_eq!(
        layout_picker_list("zzz", 0, &layouts, &ES).rows[0].label,
        "Ningún diseño coincide"
    );

    let ssh = ssh_picker_list("", 0, &[], &[], &ES);
    assert_eq!(ssh.rows[0].label, "No hay hosts configurados");
    assert_eq!(ssh.rows[0].hint, "Enter usa el destino escrito");
    let hosts = [("prod".to_string(), "me@prod".to_string())];
    assert_eq!(
        ssh_picker_list("zzz", 0, &hosts, &[], &EN).rows[0].label,
        "Connect to \"zzz\""
    );
    assert_eq!(
        ssh_picker_list("zzz", 0, &hosts, &[], &ES).rows[0].label,
        "Conectar a \"zzz\""
    );
    for (tr, refusal) in [
        (
            &EN,
            "Targets can't start with - or contain spaces or control characters",
        ),
        (
            &ES,
            "El destino no puede empezar con - ni tener espacios ni caracteres de control",
        ),
    ] {
        let row = &ssh_picker_list("-oProxyCommand=x", 0, &hosts, &[], tr).rows[0];
        assert_eq!(row.label, refusal);
        assert!(!row.enabled);
    }
    // Imported rows say where they came from, by a fixed display path.
    let imported = ["web".to_string()];
    let source = |tr| {
        ssh_picker_list("", 0, &[], &imported, tr).rows[0]
            .hint
            .clone()
    };
    if cfg!(windows) {
        assert_eq!(source(&EN), r"from ~\.ssh\config");
        assert_eq!(source(&ES), r"de ~\.ssh\config");
    } else {
        assert_eq!(source(&EN), "from ~/.ssh/config");
        assert_eq!(source(&ES), "de ~/.ssh/config");
    }
}

/// The command palette's empty field says what to type, in the UI language;
/// the other pickers keep an empty field.
#[test]
fn palette_placeholder_is_translated_and_only_in_the_palette() {
    use super::{Mux, WindowState, picker_placeholder};
    for tr in [EN, ES] {
        let mut ws = WindowState::new(0, Mux::new());
        ws.palette_input = Some((Default::default(), 0));
        assert_eq!(
            picker_placeholder(&ws, &tr),
            tr.text(Text::PickerPlaceholderCommand)
        );
        // The keyboard-shortcut list says what it searches instead.
        ws.palette_bound_only = true;
        assert_eq!(
            picker_placeholder(&ws, &tr),
            tr.text(Text::PickerPlaceholderShortcuts)
        );
        ws.palette_input = None;
        ws.layout_picker_input = Some((Default::default(), 0));
        assert_eq!(picker_placeholder(&ws, &tr), "");
    }
    assert_eq!(EN.text(Text::PickerPlaceholderCommand), "Type a command…");
    assert_ne!(
        ES.text(Text::PickerPlaceholderCommand),
        EN.text(Text::PickerPlaceholderCommand)
    );
}

/// The keyboard-shortcut list opens saying what it searches, whole, in both
/// languages, in the header of a standard window's picker card: the 1285 px
/// window of the Spanish review, where it once drew nothing, and a 1024 px
/// one. It stays two columns clear of any hint (where both do not fit, the
/// hint gives way while the field is empty), and a typed query has the whole
/// hint. The header is built from the same pieces as the UI's.
#[test]
fn the_shortcut_list_shows_its_placeholder_in_a_standard_window() {
    use super::{Mux, WindowState, picker_card_geometry, picker_placeholder};
    use unicode_width::UnicodeWidthStr as _;
    let palette = [
        ("Enter", Text::PickerHintRun),
        ("Tab/↑↓", Text::PickerHintSelect),
        ("Esc", Text::PickerHintCancel),
    ];
    let cell = (8.4, 17.5);
    for tr in [EN, ES] {
        let mut ws = WindowState::new(0, Mux::new());
        ws.palette_input = Some((Default::default(), 0));
        ws.palette_bound_only = true;
        let placeholder = picker_placeholder(&ws, &tr);
        assert_eq!(placeholder, tr.text(Text::PickerPlaceholderShortcuts));
        for surface in [(1285.0, 841.0), (1024.0, 768.0)] {
            let header = picker_card_geometry(surface, cell, 46.0, 1).unwrap().header;
            let field = |text: &str| kettle_render::TextFieldOverlay {
                picker_header: true,
                placeholder: placeholder.clone(),
                text: text.into(),
                cursor_byte: text.len(),
                selection: None,
                horizontal_scroll: 0,
                rect: header,
                prefix: "  \u{2318} ".into(),
                hint: picker_hint(&tr, &palette),
                accent_palette: 5,
            };
            let empty = field("");
            let cw = kettle_render::text_field_cell_width(&empty, cell.0, cell.1);
            assert_eq!(
                kettle_render::text_field_placeholder(&empty, cw),
                placeholder,
                "{:?} {surface:?}",
                tr.language()
            );
            let [_, editor, hint] = kettle_render::text_field_rects(&empty, cw);
            let right = editor.0 + 2.0 + placeholder.width() as f32 * cw;
            assert!(right + 2.0 * cw <= hint.0 + 0.001, "{surface:?}");
            let typed = field("zoom");
            assert_eq!(
                kettle_render::text_field_labels(&typed, cw).1.trim_end(),
                picker_hint(&tr, &palette),
                "{surface:?}"
            );
            assert_eq!(kettle_render::text_field_placeholder(&typed, cw), "");
        }
    }
}

/// Screen readers hear an imported row's source as its description, the same
/// text the row paints, and never hear a disabled placeholder as selected.
#[test]
fn ssh_rows_project_their_painted_hint_and_never_select_the_placeholder() {
    use super::picker_context_menu;
    // Row 0's option, found by the label the row paints (fitted to the card).
    let option = |query: &str, imported: &[String]| {
        let list = ssh_picker_list(query, 0, &[], imported, &EN);
        let menu = picker_context_menu(list, (800.0, 600.0), (8.0, 16.0), 0.0).unwrap();
        let painted = menu.rows[0].hint.trim().to_string();
        let label = menu.rows[0].label.clone();
        let overlay = kettle_render::Overlay {
            ssh_query: Some(query.to_string()),
            context_menu: Some(menu),
            ..kettle_render::Overlay::default()
        };
        let projection =
            modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, &EN);
        let node = projection
            .nodes
            .into_iter()
            .map(|(_, node)| node)
            .find(|node| {
                node.role() == accesskit::Role::ListBoxOption
                    && node.label() == Some(label.as_str())
            })
            .unwrap_or_else(|| panic!("no option {label:?}"));
        (node, painted)
    };
    let (imported, painted) = option("", &["web".to_string()]);
    assert_eq!(imported.label(), Some("web"));
    assert!(!painted.is_empty());
    assert_eq!(imported.description(), Some(painted.as_str()));
    assert_eq!(imported.is_selected(), Some(true));
    let (placeholder, _) = option("-x", &[]);
    assert!(
        placeholder
            .label()
            .is_some_and(|label| label.starts_with("Targets can't start with -")),
        "{:?}",
        placeholder.label()
    );
    assert!(placeholder.is_disabled());
    assert_ne!(
        placeholder.is_selected(),
        Some(true),
        "a disabled row is never selected"
    );
}
