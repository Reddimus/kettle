use super::{App, ContextMenuItem, remote_reconnect_label};
use kettle_i18n::{Language, Translator};
use kettle_remote::{ContainerOptions, ContainerRuntime, RemoteContext, SshOptions};

const EN: Translator = Translator::new(Language::En);
const ES: Translator = Translator::new(Language::Es);

fn ssh(host: &str, user: Option<&str>) -> RemoteContext {
    RemoteContext::Ssh {
        host: host.to_string(),
        user: user.map(str::to_string),
        options: SshOptions::default(),
    }
}

fn container(runtime: ContainerRuntime, name: &str) -> RemoteContext {
    RemoteContext::Container {
        runtime,
        container: name.to_string(),
        options: ContainerOptions::default(),
    }
}

/// The reconnect row names the session as detected, in every language.
#[test]
fn the_reconnect_row_names_every_session_shape() {
    let me_box = ssh("box", Some("me"));
    let pod = container(ContainerRuntime::Kubectl, "my-pod");
    assert_eq!(remote_reconnect_label(&me_box, &EN), "Reconnect ssh me@box");
    assert_eq!(
        remote_reconnect_label(&ssh("box", None), &EN),
        "Reconnect ssh box"
    );
    assert_eq!(
        remote_reconnect_label(&container(ContainerRuntime::Docker, "foo"), &EN),
        "Re-attach docker foo"
    );
    assert_eq!(
        remote_reconnect_label(&pod, &EN),
        "Re-attach kubectl my-pod"
    );
    assert_eq!(
        remote_reconnect_label(&container(ContainerRuntime::Lxc, "c1"), &EN),
        "Re-attach lxc c1"
    );
    assert_eq!(
        remote_reconnect_label(&me_box, &ES),
        "Volver a conectar por ssh a me@box"
    );
    assert_eq!(
        remote_reconnect_label(&pod, &ES),
        "Volver a conectar a my-pod en kubectl"
    );
}

#[test]
fn the_new_tab_dropdown_follows_the_ui_language() {
    let shells = vec![("zsh".to_string(), vec!["zsh".to_string()])];
    let labels = |tr: &Translator| {
        App::new_tab_menu_items(&shells, tr)
            .into_iter()
            .filter_map(|item| match item {
                ContextMenuItem::Item { label, .. } => Some(label.to_string()),
                ContextMenuItem::NewTabShell { label, .. } => Some(label),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        labels(&EN),
        [
            "zsh",
            "Save Layout…",
            "Open Layout…",
            "SSH launcher",
            "Settings…",
            "Command palette",
            "Keyboard shortcuts",
            "Help",
            "About kettle"
        ]
    );
    // Shell names are shown as detected.
    assert_eq!(
        labels(&ES),
        [
            "zsh",
            "Guardar diseño…",
            "Abrir diseño…",
            "Lanzador SSH",
            "Configuración…",
            "Paleta de comandos",
            "Atajos de teclado",
            "Ayuda",
            "Acerca de kettle"
        ]
    );
}

#[test]
fn session_routes_follow_profiles_in_both_languages() {
    use super::{Action, profile_launch_entries};
    for tr in [EN, ES] {
        for names in [vec![], vec!["home".into(), "Work".into()]] {
            let menu = App::new_tab_menu_with_profiles(&[], &names, &tr);
            let commands = super::palette_entries(
                None,
                &tr,
                &names,
                kettle_config::BroadcastDefault::default(),
            );
            for action in [
                Action::SaveLayout,
                Action::OpenLayoutPicker,
                Action::OpenSsh,
            ] {
                assert!(commands.iter().any(|(_, a)| *a == action));
                assert!(menu.iter().any(|row| matches!(row, ContextMenuItem::Item { action: a, enabled: true, .. } if *a == action)));
            }
            let submenu = menu
                .iter()
                .find_map(|row| match row {
                    ContextMenuItem::Submenu { items, .. } => Some(items),
                    _ => None,
                })
                .unwrap();
            let palette = profile_launch_entries(&names, &tr);
            for entry in &palette {
                assert!(
                    commands
                        .iter()
                        .any(|row| row.1 == entry.1 && row.0.shown(&tr) == entry.0.shown(&tr))
                );
            }
            assert_eq!(submenu.len(), names.len().max(1));
            for (row, (label, action)) in submenu.iter().zip(&palette) {
                assert!(
                    matches!(row, ContextMenuItem::DynamicItem { label: shown, action: a, enabled: true, toggle: None } if shown == label.shown(&tr) && a == action)
                );
            }
            if names.is_empty() {
                assert_eq!(palette[0].1, Action::OpenProfilesDirectory);
                assert!(palette[0].0.shown(&tr).contains(
                    if tr.language() == kettle_i18n::Language::Es {
                        "Crear perfil"
                    } else {
                        "Create Profile"
                    }
                ));
            } else {
                for (entry, name) in palette.iter().zip(&names) {
                    assert_eq!(entry.1, Action::LaunchProfile(name.clone()));
                    assert!(entry.0.shown(&tr).contains(name));
                }
            }
        }
    }
}

#[test]
fn saved_layout_is_listed_by_picker_in_an_isolated_config_directory() {
    const CHILD: &str = "KETTLE_C19_LAYOUT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        use super::{
            Action, GroupBulkScope, TitleEditScope, begin_title_edit, clear_stale_title_edit,
            layout_picker_list, palette_entries,
        };
        let ws = super::WindowState::new(1, super::Mux::new());
        let mut prompt = begin_title_edit(
            &ws.mux,
            TitleEditScope::SaveLayout,
            GroupBulkScope::Single,
            "ignored",
        );
        assert!(prompt.is_some());
        assert!(!clear_stale_title_edit(&ws.mux, &mut prompt));
        let mut s = ws.mux.snapshot();
        // Use a portable serialized split fixture, without creating a PTY/window.
        s.tabs = vec![crate::session::STab {
            root: crate::session::SNode::Split {
                vertical: false,
                ratio: 0.5,
                a: Box::new(crate::session::SNode::Leaf {
                    cwd: Some("/tmp".into()),
                    cmd: vec![],
                    group: None,
                }),
                b: Box::new(crate::session::SNode::Leaf {
                    cwd: Some("/".into()),
                    cmd: vec![],
                    group: None,
                }),
            },
            focus: 0,
            title_override: None,
            zoomed: false,
        }];
        for invalid in ["", "../escape", ".", "..", "two words", &"x".repeat(129)] {
            assert!(s.save_layout_result(invalid).is_err());
        }
        s.save_layout_result("panel-test").unwrap();
        let layouts = crate::session::Session::list_layouts();
        assert_eq!(layouts, ["panel-test"]);
        let loaded = crate::session::Session::load_layout_with_identity("panel-test")
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(
            serde_json::to_value(&loaded).unwrap(),
            serde_json::to_value(&s).unwrap()
        );
        assert_eq!(
            layout_picker_list("", 0, &layouts, &ES).rows[0].label,
            "panel-test"
        );
        assert!(!layout_picker_list("", 0, &[], &ES).rows[0].enabled);
        let profiles = kettle_config::Config::default_path()
            .unwrap()
            .parent()
            .unwrap()
            .join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        for name in ["Work", "home"] {
            std::fs::write(profiles.join(format!("{name}.config")), "").unwrap();
        }
        let entries = palette_entries(
            None,
            &ES,
            &kettle_config::Config::list_profiles(),
            kettle_config::BroadcastDefault::default(),
        );
        let launches: Vec<_> = entries
            .iter()
            .filter_map(|(_, action)| match action {
                Action::LaunchProfile(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(launches, ["home", "Work"]);
        return;
    }
    let dir = kettle_test_support::private_tempdir("kettle-layout-picker-");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "app::menu_text_tests::saved_layout_is_listed_by_picker_in_an_isolated_config_directory", "--nocapture"])
        .env(CHILD, "1").env("XDG_CONFIG_HOME", dir.path()).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A menu panel showing `items`, each row built as the menu builds it, with
/// `hint` in every non-submenu row's shortcut column.
fn menu_panel(items: &[ContextMenuItem], hint: &str) -> kettle_render::ContextMenu {
    kettle_render::ContextMenu {
        anchor: (40.0, 60.0),
        rows: items
            .iter()
            .map(|item| {
                let hint = match item {
                    ContextMenuItem::Submenu { .. } => super::SUBMENU_SIGN,
                    ContextMenuItem::Separator => "",
                    _ => hint,
                };
                super::context_menu_row(item, hint.to_string())
            })
            .collect(),
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 400.0,
        panel_h_clamped: 600.0,
        a11y_label: None,
        picker_header_height: 0.0,
    }
}

/// The open menu's item nodes in row order, each with its painted label.
fn menu_item_nodes(
    menu: kettle_render::ContextMenu,
    tr: &Translator,
) -> Vec<(String, accesskit::Node)> {
    let rows: Vec<_> = menu
        .rows
        .iter()
        .map(|row| (row.separator, row.label.clone()))
        .collect();
    let overlay = kettle_render::Overlay {
        context_menu: Some(menu),
        ..kettle_render::Overlay::default()
    };
    let projection =
        super::modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, tr);
    rows.into_iter()
        .enumerate()
        .filter(|(_, (separator, _))| !separator)
        .map(|(index, (_, painted))| {
            let id = super::accessibility_modal_id(2, index + 1);
            let node = projection
                .nodes
                .iter()
                .find(|(at, _)| *at == id)
                .map(|(_, node)| node.clone())
                .unwrap_or_else(|| panic!("no node for row {index} ({painted:?})"));
            (painted, node)
        })
        .collect()
}

/// A toggle row's accessible name on the host running the test: on macOS
/// it keeps the painted marker, since VoiceOver gets no other state for a
/// menu item there; elsewhere it is the plain name.
fn toggle_name<'a>(painted: &'a str, plain: &'a str) -> &'a str {
    if cfg!(target_os = "macos") {
        painted
    } else {
        plain
    }
}

/// A row as a screen reader gets it next to how it is painted.
type SpokenRow = (String, Option<String>, Option<accesskit::Toggled>);

fn spoken(painted: &str, name: &str, on: bool) -> SpokenRow {
    (
        painted.to_string(),
        Some(toggle_name(painted, name).to_string()),
        Some(accesskit::Toggled::from(on)),
    )
}

fn spoken_rows(nodes: Vec<(String, accesskit::Node)>, role: accesskit::Role) -> Vec<SpokenRow> {
    nodes
        .into_iter()
        .filter(|(_, node)| node.role() == role)
        .map(|(painted, node)| (painted, node.label().map(str::to_string), node.toggled()))
        .collect()
}

/// Read only is a check box to a screen reader: its state is a property, and
/// its name has no check mark except on macOS, while the painted row keeps
/// the mark.
#[test]
fn the_read_only_row_is_a_check_box_with_its_state() {
    use accesskit::{Role, Toggled};
    for (tr, name) in [(EN, "Read only"), (ES, "Solo lectura")] {
        for (on, mark, toggled) in [(true, "✓ ", Toggled::True), (false, "  ", Toggled::False)] {
            let item = super::read_only_menu_item(on, &tr);
            assert!(matches!(
                item,
                ContextMenuItem::DynamicItem {
                    action: super::Action::TogglePaneReadOnly,
                    enabled: true,
                    ..
                }
            ));
            let menu = menu_panel(std::slice::from_ref(&item), "Ctrl+Shift+R");
            let painted = format!("{mark}{name}");
            assert_eq!(menu.rows[0].label, painted);
            let nodes = menu_item_nodes(menu, &tr);
            let (_, node) = &nodes[0];
            assert_eq!(node.role(), Role::MenuItemCheckBox, "read only = {on}");
            assert_eq!(node.toggled(), Some(toggled), "read only = {on}");
            assert_eq!(node.label(), Some(toggle_name(&painted, name)));
            // The shortcut is both the property and the spoken description.
            assert_eq!(node.keyboard_shortcut(), Some("Ctrl+Shift+R"));
            assert_eq!(node.description(), Some("Ctrl+Shift+R"));
        }
    }
}

/// The Preferences radio groups are radio items with exactly the configured
/// choice selected, and the check boxes report their own states. Painted
/// labels keep their marks.
#[test]
fn preferences_radios_and_check_boxes_carry_their_state() {
    use accesskit::Role;
    // Off the defaults, so each group's selection comes from the config.
    let cfg = kettle_config::Config {
        scrollbar: kettle_config::ScrollbarMode::Never,
        ask_before_closing: kettle_config::AskBeforeClosing::Always,
        bell: kettle_config::BellMode::Visual,
        cursor_blink: false,
        copy_on_select: true,
        mouse_hide_while_typing: false,
        ..kettle_config::Config::default()
    };
    let ContextMenuItem::Submenu { items, .. } = super::preferences_submenu(&cfg, EN) else {
        panic!("Preferences is a submenu");
    };
    let radios = |index: usize| {
        let ContextMenuItem::Submenu { items: group, .. } = &items[index] else {
            panic!("Preferences row {index} is not a radio group");
        };
        let nodes = menu_item_nodes(menu_panel(group, ""), &EN);
        assert_eq!(nodes.len(), group.len());
        spoken_rows(nodes, Role::MenuItemRadio)
    };
    assert_eq!(
        radios(0),
        [
            spoken("○ Scrollbar always", "Scrollbar always", false),
            spoken("○ Scrollbar auto", "Scrollbar auto", false),
            spoken("● Scrollbar hidden", "Scrollbar hidden", true),
        ]
    );
    assert_eq!(
        radios(1),
        [
            spoken("● Confirm close: always", "Confirm close: always", true),
            spoken(
                "○ Confirm close: running programs or multiple terminals",
                "Confirm close: running programs or multiple terminals",
                false
            ),
            spoken(
                "○ Confirm close: multiple terminals",
                "Confirm close: multiple terminals",
                false
            ),
            spoken("○ Confirm close: never", "Confirm close: never", false),
        ]
    );
    assert_eq!(
        radios(2),
        [
            spoken("○ Bell off", "Bell off", false),
            spoken("● Bell visual flash", "Bell visual flash", true),
            spoken("○ Bell attention", "Bell attention", false),
            spoken(
                "○ Bell visual + attention",
                "Bell visual + attention",
                false
            ),
        ]
    );
    let nodes = menu_item_nodes(menu_panel(&items, ""), &EN);
    assert_eq!(
        spoken_rows(nodes.clone(), Role::MenuItemCheckBox),
        [
            spoken("  Cursor blink", "Cursor blink", false),
            spoken("✓ Copy on select", "Copy on select", true),
            spoken(
                "  Mouse-hide while typing",
                "Mouse-hide while typing",
                false
            ),
        ]
    );
    // Each toggle row keeps its own action.
    use super::Action as A;
    let actions: Vec<_> = items
        .iter()
        .flat_map(|item| match item {
            ContextMenuItem::Submenu { items, .. } => items.clone(),
            other => vec![other.clone()],
        })
        .filter_map(|item| match item {
            ContextMenuItem::DynamicItem { label, action, .. } => Some((label, action)),
            _ => None,
        })
        .collect();
    let expected = [
        ("○ Scrollbar always", A::SetScrollbarAlways),
        ("○ Scrollbar auto", A::SetScrollbarAuto),
        ("● Scrollbar hidden", A::SetScrollbarNever),
        ("● Confirm close: always", A::SetAskBeforeClosingAlways),
        (
            "○ Confirm close: running programs or multiple terminals",
            A::SetAskBeforeClosingBusyOrMultiple,
        ),
        (
            "○ Confirm close: multiple terminals",
            A::SetAskBeforeClosingMultiple,
        ),
        ("○ Confirm close: never", A::SetAskBeforeClosingNever),
        ("○ Bell off", A::SetBellOff),
        ("● Bell visual flash", A::SetBellVisual),
        ("○ Bell attention", A::SetBellAttention),
        ("○ Bell visual + attention", A::SetBellBoth),
        ("  Cursor blink", A::ToggleCursorBlink),
        ("✓ Copy on select", A::ToggleCopyOnSelect),
        ("  Mouse-hide while typing", A::ToggleMouseHide),
    ]
    .map(|(label, action)| (label.to_string(), action));
    assert_eq!(actions, expected);
    // Rows that toggle nothing stay plain menu items with no state.
    let plain = items
        .iter()
        .filter(|item| {
            matches!(
                item,
                ContextMenuItem::Item { .. } | ContextMenuItem::Submenu { .. }
            )
        })
        .count();
    let menu_items: Vec<_> = nodes
        .iter()
        .filter(|(_, node)| node.role() == Role::MenuItem)
        .collect();
    assert_eq!(menu_items.len(), plain);
    assert!(menu_items.iter().all(|(_, node)| node.toggled().is_none()));
}

/// The close-confirmation group offers all four modes as radio items, from
/// the most cautious to none, and whichever mode is configured, that one
/// radio alone is on.
#[test]
fn every_close_confirmation_mode_is_a_radio_that_is_on_when_configured() {
    use accesskit::Role;
    use kettle_config::AskBeforeClosing;
    let modes = [
        AskBeforeClosing::Always,
        AskBeforeClosing::BusyOrMultiple,
        AskBeforeClosing::MultipleTerminals,
        AskBeforeClosing::Never,
    ];
    let names = [
        "Confirm close: always",
        "Confirm close: running programs or multiple terminals",
        "Confirm close: multiple terminals",
        "Confirm close: never",
    ];
    for (selected, mode) in modes.into_iter().enumerate() {
        let cfg = kettle_config::Config {
            ask_before_closing: mode,
            ..kettle_config::Config::default()
        };
        let ContextMenuItem::Submenu { items, .. } = super::preferences_submenu(&cfg, EN) else {
            panic!("Preferences is a submenu");
        };
        let ContextMenuItem::Submenu { items: group, .. } = &items[1] else {
            panic!("Preferences row 1 is not the close-confirmation group");
        };
        let nodes = menu_item_nodes(menu_panel(group, ""), &EN);
        assert_eq!(nodes.len(), names.len(), "{mode:?}");
        let expected: Vec<SpokenRow> = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let on = index == selected;
                let painted = format!("{}{name}", if on { "● " } else { "○ " });
                spoken(&painted, name, on)
            })
            .collect();
        assert_eq!(
            spoken_rows(nodes, Role::MenuItemRadio),
            expected,
            "{mode:?}"
        );
    }
}

/// A row's shortcut is AccessKit's keyboard shortcut and stays its spoken
/// description. A submenu row has no shortcut and is described as a submenu,
/// not by the arrow it is painted with.
#[test]
fn a_menu_shortcut_is_both_the_shortcut_and_the_description() {
    let items = [
        ContextMenuItem::Item {
            label: EN.text(kettle_i18n::Text::MenuCopy),
            action: super::Action::Copy,
            enabled: true,
        },
        ContextMenuItem::Submenu {
            label: EN.text(kettle_i18n::Text::MenuPreferences).to_string(),
            items: Vec::new(),
        },
    ];
    let nodes = menu_item_nodes(menu_panel(&items, " Ctrl+Shift+C "), &EN);
    let (_, copy) = &nodes[0];
    assert_eq!(copy.role(), accesskit::Role::MenuItem);
    assert_eq!(copy.toggled(), None);
    assert_eq!(copy.keyboard_shortcut(), Some("Ctrl+Shift+C"));
    assert_eq!(copy.description(), Some("Ctrl+Shift+C"));
    let (_, submenu) = &nodes[1];
    assert_eq!(submenu.keyboard_shortcut(), None);
    assert_eq!(
        submenu.description(),
        Some(EN.text(kettle_i18n::Text::MenuA11ySubmenu))
    );
    assert_eq!(submenu.has_popup(), Some(accesskit::HasPopup::Menu));
    assert_eq!(submenu.is_expanded(), Some(false));
}

/// Drilled into a radio group, the Preferences panel behind it is still in
/// the tree, and its check boxes there keep their state and the same names
/// as in an open panel.
#[test]
fn a_parent_panel_keeps_its_check_box_states() {
    use accesskit::{Role, Toggled};
    let cfg = kettle_config::Config {
        cursor_blink: false,
        copy_on_select: true,
        ..kettle_config::Config::default()
    };
    let ContextMenuItem::Submenu { items, .. } = super::preferences_submenu(&cfg, EN) else {
        panic!("Preferences is a submenu");
    };
    let ContextMenuItem::Submenu {
        items: scrollbar, ..
    } = items[0].clone()
    else {
        panic!("the first Preferences row is the scrollbar group");
    };
    let mut ws = super::WindowState::new(1, super::Mux::new());
    ws.context_menu = Some(super::ContextMenuState {
        anchor: (440.0, 60.0),
        anchor_scale: 1.0,
        items: scrollbar.clone(),
        highlight: 0,
        drill_stack: vec![super::ContextMenuLevel {
            anchor: (40.0, 60.0),
            anchor_scale: 1.0,
            items: items.clone(),
            highlight: 0,
            scroll_offset: 0,
        }],
        scroll_offset: 0,
        typeahead_buf: String::new(),
        typeahead_until: None,
    });
    let parent = menu_panel(&items, "");
    let painted: Vec<_> = parent.rows.iter().map(|row| row.label.clone()).collect();
    let mut current = menu_panel(&scrollbar, "");
    current.anchor = (440.0, 60.0);
    let overlay = kettle_render::Overlay {
        context_menu: Some(current),
        context_menu_parents: vec![parent],
        ..kettle_render::Overlay::default()
    };
    let mut projection =
        super::modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, &EN);
    super::nest_context_menu_accessibility(&mut projection, &ws, &overlay, 16.0, &EN);
    for (name, painted_label, toggled) in [
        ("Cursor blink", "  Cursor blink", Toggled::False),
        ("Copy on select", "✓ Copy on select", Toggled::True),
    ] {
        let index = painted
            .iter()
            .position(|label| label == painted_label)
            .unwrap_or_else(|| panic!("no {painted_label:?} row"));
        let id = super::accessibility_modal_id(40, index + 1);
        let (_, node) = projection
            .nodes
            .iter()
            .find(|(at, _)| *at == id)
            .unwrap_or_else(|| panic!("no parent-panel node for {name:?}"));
        assert_eq!(node.role(), Role::MenuItemCheckBox, "{name}");
        assert_eq!(node.toggled(), Some(toggled), "{name}");
        assert_eq!(node.label(), Some(toggle_name(painted_label, name)));
    }
}

/// The palette offers renaming the tab and moving it to a new window, so
/// neither needs a keybind or the right-click menu. Typing the label, in
/// either language, finds the action first.
#[test]
fn palette_offers_renaming_and_moving_the_tab() {
    use super::Action;
    use kettle_i18n::Text;
    for tr in [EN, ES] {
        let entries =
            super::palette_entries(None, &tr, &[], kettle_config::BroadcastDefault::default());
        for (text, action) in [
            (Text::PaletteRenameTab, Action::EditTabTitle),
            (Text::PaletteMoveTabToNewWindow, Action::MoveTabToNewWindow),
        ] {
            let label = tr.text(text);
            let row = entries
                .iter()
                .position(|(_, a)| *a == action)
                .unwrap_or_else(|| panic!("{action:?} is not in the palette"));
            assert_eq!(entries[row].0.shown(&tr), label);
            let ranked = kettle_config::palette::rank_entries(label, &entries, &tr);
            assert_eq!(ranked.first(), Some(&row), "{label}");
        }
    }
}
