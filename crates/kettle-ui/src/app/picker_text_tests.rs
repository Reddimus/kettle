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
        }),
        ..kettle_render::Overlay::default()
    };
    assert!(labels(menu(), &EN).iter().any(|l| l == "Terminal menu"));
    assert!(
        labels(menu(), &ES)
            .iter()
            .any(|l| l == "Menú de la terminal")
    );
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
        command_picker_list("", 0, &bindings, &ES, None).rows[0].label,
        "Nueva pestaña"
    );
    let none = command_picker_list("zzzqqq", 0, &bindings, &ES, None);
    assert_eq!(none.rows[0].label, "Ningún comando coincide");
    assert!(!none.rows[0].enabled);
    assert_eq!(
        command_picker_list("zzzqqq", 0, &bindings, &EN, None).rows[0].label,
        "No matching command"
    );

    let empty = layout_picker_list("", 0, &[], &ES);
    assert_eq!(empty.rows[0].label, "No hay diseños guardados");
    // The command stays as typed; only the workspace name's placeholder is
    // translated.
    assert_eq!(empty.rows[0].hint, "kettle --layout NOMBRE");
    assert_eq!(
        layout_picker_list("", 0, &[], &EN).rows[0].hint,
        "kettle --layout NAME"
    );
    let layouts = ["dev".to_string()];
    assert_eq!(
        layout_picker_list("zzz", 0, &layouts, &ES).rows[0].label,
        "Ningún diseño coincide"
    );

    let ssh = ssh_picker_list("", 0, &[], &ES);
    assert_eq!(ssh.rows[0].label, "No hay hosts configurados");
    assert_eq!(ssh.rows[0].hint, "Enter usa el destino escrito");
    let hosts = [("prod".to_string(), "me@prod".to_string())];
    assert_eq!(
        ssh_picker_list("zzz", 0, &hosts, &EN).rows[0].label,
        "No configured host matches"
    );
}
