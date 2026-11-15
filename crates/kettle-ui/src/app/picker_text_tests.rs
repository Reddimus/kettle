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
        .rows[..2]
            .iter()
            .map(|row| (row.label.as_str(), row.heading))
            .collect::<Vec<_>>(),
        [("Pestañas", true), ("Nueva pestaña", false)]
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
            let header = picker_card_geometry(surface, cell, 46.0, 1, 1.0)
                .unwrap()
                .header;
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

/// The described rows the palette shows, in `tr`'s language, as the painter
/// receives them at 1280x820 under a 32 px tab bar, at the `font_px` default
/// font (13 px on macOS, 14 elsewhere; the bundled font is 0.6 em wide), with
/// each row's hint in either notation: `macos` glyphs or `Ctrl+Shift` text. A
/// row's action is read from the same view entries the rows are ranked from.
fn described_palette_rows(
    tr: &Translator,
    macos: bool,
    font_px: f32,
) -> Vec<(
    kettle_config::keybinds::Action,
    kettle_render::ContextMenuRow,
    kettle_render::ContextMenu,
)> {
    let bindings = kettle_config::keybinds::defaults();
    let entries = super::palette_view_entries(
        None,
        tr,
        &[],
        kettle_config::BroadcastDefault::default(),
        &bindings,
        false,
    );
    let list = command_picker_list(
        "",
        0,
        &bindings,
        tr,
        None,
        &[],
        false,
        kettle_config::BroadcastDefault::default(),
    );
    // The empty palette's commands, past its group headings.
    let mut rows: Vec<Option<kettle_render::ContextMenuRow>> = list
        .rows
        .into_iter()
        .filter(|row| !row.heading)
        .map(Some)
        .collect();
    assert_eq!(rows.len(), entries.len());
    let mut out = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if kettle_config::palette::description(&entry.action).is_none() {
            continue;
        }
        let mut row = rows[index].take().unwrap();
        assert_eq!(row.label, entry.label.shown(tr));
        row.hint =
            kettle_config::keybinds::hint_label_for_platform(&bindings, &entry.action, macos)
                .unwrap_or_default();
        let list = super::PickerList {
            card_scale: 1.0,
            rows: vec![row],
            selected: 0,
            footer: None,
        };
        let cell = (font_px * 0.6, font_px * 1.25);
        let mut menu = super::picker_context_menu(list, (1280.0, 820.0), cell, 32.0).unwrap();
        let row = menu.rows.remove(0);
        out.push((entry.action.clone(), row, menu));
    }
    out
}

/// Every described command shows its description whole in the 1280x820
/// palette at either platform's default font, in English, Spanish and the
/// expanded pseudo-locale, with either notation of the platform's default
/// shortcuts beside it. The width is
/// measured by the painter's own function on the text it paints.
#[test]
fn every_description_fits_the_default_palette() {
    for font_px in [13.0, 14.0] {
        for tr in [EN, ES, Translator::pseudo()] {
            for macos in [true, false] {
                let rows = described_palette_rows(&tr, macos, font_px);
                assert_eq!(rows.len(), 12, "described commands");
                for (action, row, menu) in rows {
                    assert!(!row.description.is_empty(), "{action:?}");
                    let painted = kettle_render::menu_row_description_left(
                        &row,
                        menu.anchor.0,
                        menu.panel_w_clamped,
                        font_px * 0.6,
                    );
                    assert!(
                        painted.is_some(),
                        "{font_px} px {:?} {action:?} (macos hints {macos}): {} | {} | {}",
                        tr.language(),
                        row.label,
                        row.description,
                        row.hint
                    );
                }
            }
        }
    }
}

/// A palette option's accessible name is its label. Its description is the
/// shortcut in words, then the description, even where the row is too narrow
/// to paint either; a row with neither has none, and glyphs are never read.
#[test]
fn palette_options_describe_shortcut_then_description() {
    use kettle_config::keybinds::Action;
    let bindings = kettle_config::keybinds::defaults();
    let macos = cfg!(target_os = "macos");
    let spoken = |action: &Action, tr: &Translator| {
        let hint = kettle_config::keybinds::ui_hint_label(&bindings, action).unwrap();
        bindings
            .iter()
            .find(|(trigger, bound)| *bound == action && trigger.ui_label(macos) == hint)
            .map(|(trigger, _)| trigger.spoken_label(macos, tr))
            .unwrap()
    };
    for tr in [EN, ES] {
        for surface in [(1280.0, 820.0), (640.0, 820.0)] {
            let list = command_picker_list(
                "",
                0,
                &bindings,
                &tr,
                None,
                &[],
                false,
                kettle_config::BroadcastDefault::default(),
            );
            let menu = super::picker_context_menu(list, surface, (7.8, 16.25), 32.0).unwrap();
            let overlay = kettle_render::Overlay {
                palette_query: Some(String::new()),
                context_menu: Some(menu),
                ..Default::default()
            };
            let projection =
                modal_accessibility_projection(&overlay, surface, (7.8, 16.25), 1.0, &tr);
            let option = |label: &str| {
                projection
                    .nodes
                    .iter()
                    .map(|(_, node)| node)
                    .find(|node| {
                        node.role() == accesskit::Role::ListBoxOption && node.label() == Some(label)
                    })
                    .unwrap_or_else(|| panic!("{label}"))
            };
            // Split right is the first command under its heading, so the
            // group's name follows.
            let split_right = option(tr.text(Text::PaletteSplitRight));
            assert_eq!(
                split_right.description(),
                Some(
                    format!(
                        "{}. {}. {}",
                        spoken(&Action::SplitRight, &tr),
                        tr.text(Text::PaletteDescSplitRight),
                        tr.text(Text::PaletteGroupSplits)
                    )
                    .as_str()
                ),
                "{surface:?}"
            );
            let next = option(tr.text(Text::PaletteNextTab));
            assert_eq!(
                next.description(),
                Some(spoken(&Action::NextTab, &tr).as_str())
            );
            let left = option(tr.text(Text::PaletteSplitLeft));
            assert_eq!(left.description(), None);
            for (_, node) in &projection.nodes {
                if let Some(description) = node.description() {
                    assert!(!description.contains(['⌘', '⇧', '⌃', '⌥']), "{description}");
                }
            }
        }
    }
}

/// The empty palette opens on the Tabs heading over New tab, Close tab, Next
/// tab and Previous tab, each with its chord, then the Splits heading over
/// Split right with its own, all inside the twelve rows a 1280x820 window
/// shows, in English and Spanish. A typed query and the keyboard-shortcut
/// list show the flat list, with no heading.
#[test]
fn empty_palette_starts_with_tabs_then_splits_and_a_query_has_no_headings() {
    let bindings = kettle_config::keybinds::defaults();
    for tr in [EN, ES] {
        let list = command_picker_list(
            "",
            0,
            &bindings,
            &tr,
            None,
            &[],
            false,
            kettle_config::BroadcastDefault::default(),
        );
        let rows: Vec<(&str, bool, bool)> = list.rows[..12]
            .iter()
            .map(|row| (row.label.as_str(), row.heading, !row.hint.is_empty()))
            .collect();
        let text = |text| tr.text(text);
        assert_eq!(rows[0], (text(Text::PaletteGroupTabs), true, false));
        // Each shows its chord where the keymap binds one: all four on
        // macOS, all but Close tab (Ctrl+Shift+W closes a pane) elsewhere.
        for (row, (label, action)) in rows[1..5].iter().zip([
            (Text::PaletteNewTab, kettle_config::keybinds::Action::NewTab),
            (
                Text::PaletteCloseTab,
                kettle_config::keybinds::Action::CloseTab,
            ),
            (
                Text::PaletteNextTab,
                kettle_config::keybinds::Action::NextTab,
            ),
            (
                Text::PalettePrevTab,
                kettle_config::keybinds::Action::PrevTab,
            ),
        ]) {
            let bound = kettle_config::keybinds::ui_hint_label(&bindings, &action).is_some();
            assert_eq!(*row, (text(label), false, bound), "{:?}", tr.language());
            assert!(bound || action == kettle_config::keybinds::Action::CloseTab);
        }
        assert_eq!(rows[10], (text(Text::PaletteGroupSplits), true, false));
        assert_eq!(rows[11], (text(Text::PaletteSplitRight), false, true));
        assert_eq!(list.selected, 1, "opening selects New tab, not the heading");
        for query in ["tab", "split", "a"] {
            let list = command_picker_list(
                query,
                0,
                &bindings,
                &tr,
                None,
                &[],
                false,
                kettle_config::BroadcastDefault::default(),
            );
            assert!(list.rows.iter().all(|row| !row.heading), "{query:?}");
        }
        let shortcuts = command_picker_list(
            "",
            0,
            &bindings,
            &tr,
            None,
            &[],
            true,
            kettle_config::BroadcastDefault::default(),
        );
        assert!(shortcuts.rows.iter().all(|row| !row.heading));
    }
}

/// Every command selection of the empty palette lands on a command row below
/// the headings, the one Enter runs; the accessibility tree is one flat list
/// with an option per command and none per heading, numbered past the
/// headings, with the selected command selected; and the first command under
/// each heading ends its description with the group's name.
#[test]
fn selection_and_accessibility_skip_the_headings() {
    let bindings = kettle_config::keybinds::defaults();
    for tr in [EN, ES] {
        let entries = super::palette_view_entries(
            None,
            &tr,
            &[],
            kettle_config::BroadcastDefault::default(),
            &bindings,
            false,
        );
        for selected in 0..entries.len() {
            let list = command_picker_list(
                "",
                selected,
                &bindings,
                &tr,
                None,
                &[],
                false,
                kettle_config::BroadcastDefault::default(),
            );
            let row = &list.rows[list.selected];
            assert!(!row.heading, "selection {selected}");
            assert_eq!(
                kettle_render::menu_row_option(&list.rows, list.selected),
                Some(selected)
            );
            let action = super::palette_enter_action("", selected, &entries, &tr).unwrap();
            assert_eq!(
                row.label,
                entries
                    .iter()
                    .find(|e| e.action == action)
                    .unwrap()
                    .label
                    .shown(&tr)
            );
        }
        let list = command_picker_list(
            "",
            3,
            &bindings,
            &tr,
            None,
            &[],
            false,
            kettle_config::BroadcastDefault::default(),
        );
        let headings: Vec<(usize, String)> = list
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.heading)
            .map(|(index, row)| (index, row.label.clone()))
            .collect();
        assert_eq!(headings.len(), 14);
        let menu = super::picker_context_menu(list, (1280.0, 820.0), (7.8, 16.25), 32.0).unwrap();
        let overlay = kettle_render::Overlay {
            palette_query: Some(String::new()),
            context_menu: Some(menu),
            ..Default::default()
        };
        let projection =
            modal_accessibility_projection(&overlay, (1280.0, 820.0), (7.8, 16.25), 1.0, &tr);
        let options: Vec<_> = projection
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == accesskit::Role::ListBoxOption)
            .collect();
        assert_eq!(options.len(), entries.len(), "one option per command");
        assert!(
            !projection
                .nodes
                .iter()
                .any(|(_, node)| node.role() == accesskit::Role::Group)
        );
        let menu = overlay.context_menu.as_ref().unwrap();
        for (option, (id, node)) in options.iter().enumerate() {
            assert_eq!(
                *id,
                super::accessibility_modal_id(super::MODAL_KIND_PALETTE, option + 3)
            );
            assert_eq!(node.is_selected(), Some(option == 3), "option {option}");
            let row = kettle_render::menu_option_row(&menu.rows, option).unwrap();
            assert_eq!(node.label(), Some(menu.rows[row].label.as_str()));
            match headings.iter().find(|(index, _)| index + 1 == row) {
                Some((_, group)) => assert!(
                    node.description()
                        .is_some_and(|d| d.ends_with(group.as_str())),
                    "{:?} under {group}",
                    node.label()
                ),
                None => assert!(
                    !headings.iter().any(|(_, group)| node
                        .description()
                        .is_some_and(|d| d.ends_with(group.as_str()))),
                    "{:?}",
                    node.label()
                ),
            }
        }
    }
}
