use super::*;
use kettle_i18n::Language;

fn ready() -> Context {
    Context {
        window: true,
        pane: true,
        tabs: 2,
        panes: 2,
        zoomed: false,
        clipboard: true,
        selection: true,
        smart_copy: true,
        writable: true,
        renderer: true,
    }
}

fn titles(model: &[Menu]) -> Vec<Vec<&str>> {
    model
        .iter()
        .map(|menu| {
            menu.rows
                .iter()
                .map(|row| match row {
                    Row::Item { title, .. } => title.as_str(),
                    Row::Services(title) | Row::Submenu { title, .. } => *title,
                    Row::Separator => "|",
                })
                .collect()
        })
        .collect()
}

#[test]
fn complete_structure_in_english_and_spanish() {
    let en = menu_model(&Translator::new(Language::En), &Bindings::new(), ready());
    let es = menu_model(&Translator::new(Language::Es), &Bindings::new(), ready());
    assert_eq!(
        en.iter().map(|menu| menu.title).collect::<Vec<_>>(),
        ["Kettle", "Shell", "Edit", "View", "Window"]
    );
    assert_eq!(
        es.iter().map(|menu| menu.title).collect::<Vec<_>>(),
        ["Kettle", "Shell", "Editar", "Ver", "Ventana"]
    );
    assert_eq!(
        titles(&en),
        vec![
            vec![
                "About Kettle",
                "|",
                "Settings…",
                "|",
                "Services",
                "|",
                "Hide Kettle",
                "Hide Others",
                "Show All",
                "|",
                "Quit Kettle"
            ],
            vec![
                "New Window",
                "New Tab",
                "New Tab with Profile",
                "Save Layout…",
                "Open Layout…",
                "SSH launcher",
                "|",
                "Split Right",
                "Split Down",
                "|",
                "Close Pane",
                "Close Tab",
                "Close Window"
            ],
            vec![
                "Copy",
                "Paste",
                "Select All",
                "|",
                "Find…",
                "Quick-select hints",
                "|",
                "Clear Scrollback"
            ],
            vec![
                "Command Palette…",
                "Select Theme…",
                "|",
                "Increase Font Size",
                "Decrease Font Size",
                "Reset Font Size",
                "|",
                "Zoom Pane",
                "Enter Full Screen",
                "Toggle quick terminal"
            ],
            vec!["Next Tab", "Previous Tab"],
        ]
    );
    assert_eq!(
        titles(&es),
        vec![
            vec![
                "Acerca de Kettle",
                "|",
                "Configuración…",
                "|",
                "Servicios",
                "|",
                "Ocultar Kettle",
                "Ocultar otros",
                "Mostrar todo",
                "|",
                "Salir de Kettle"
            ],
            vec![
                "Nueva ventana",
                "Nueva pestaña",
                "Nueva pestaña con perfil",
                "Guardar diseño…",
                "Abrir diseño…",
                "Lanzador SSH",
                "|",
                "Dividir a la derecha",
                "Dividir hacia abajo",
                "|",
                "Cerrar panel",
                "Cerrar pestaña",
                "Cerrar ventana"
            ],
            vec![
                "Copiar",
                "Pegar",
                "Seleccionar todo",
                "|",
                "Buscar…",
                "Selección rápida con etiquetas",
                "|",
                "Borrar el historial de la terminal"
            ],
            vec![
                "Paleta de comandos…",
                "Seleccionar tema…",
                "|",
                "Aumentar el tamaño de letra",
                "Reducir el tamaño de letra",
                "Restablecer el tamaño de letra",
                "|",
                "Ampliar panel",
                "Entrar en pantalla completa",
                "Mostrar u ocultar la terminal rápida"
            ],
            vec!["Pestaña siguiente", "Pestaña anterior"],
        ]
    );
    let commands = |menus: Vec<Menu>| {
        menus
            .into_iter()
            .map(|menu| {
                (
                    menu.windows,
                    menu.rows
                        .into_iter()
                        .map(|row| match row {
                            Row::Item {
                                command, enabled, ..
                            } => Some((command, enabled)),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(commands(en.clone()), commands(es));
    assert_eq!(
        en.iter().map(|menu| menu.windows).collect::<Vec<_>>(),
        [false, false, false, false, true]
    );
    assert_eq!(
        en.iter()
            .flat_map(|menu| &menu.rows)
            .filter_map(|row| match row {
                Row::Item {
                    command: Command::Kettle(action),
                    ..
                } => Some(action.clone()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        [
            Action::OpenSettings,
            Action::NewWindow,
            Action::NewTab,
            Action::SaveLayout,
            Action::OpenLayoutPicker,
            Action::OpenSsh,
            Action::SplitRight,
            Action::SplitDown,
            Action::ClosePane,
            Action::CloseTab,
            Action::CloseWindow,
            Action::Copy,
            Action::Paste,
            Action::SelectAll,
            Action::StartSearch,
            Action::HintMode,
            Action::ClearHistory,
            Action::CommandPalette,
            Action::OpenThemePicker,
            Action::IncreaseFontSize,
            Action::DecreaseFontSize,
            Action::ResetFontSize,
            Action::ToggleZoom,
            Action::ToggleFullscreen,
            Action::ToggleQuickTerminal,
            Action::NextTab,
            Action::PrevTab,
        ]
    );
}

#[test]
fn shortcuts_follow_config_overrides_unbinds_and_command_filter() {
    let tr = Translator::new(Language::En);
    let original = Trigger::new(Mods::SUPER, Key::Char('t'));
    let remapped = Trigger::new(Mods::SUPER | Mods::ALT, Key::Char('y'));
    let ctrl = Trigger::new(Mods::CTRL, Key::Char('t'));
    let mut bindings = Bindings::from([(original, Action::NewTab), (ctrl, Action::NewTab)]);
    let shortcut = |bindings: &Bindings| {
        menu_model(&tr, bindings, ready())
            .into_iter()
            .flat_map(|menu| menu.rows)
            .find_map(|row| match row {
                Row::Item {
                    command: Command::Kettle(Action::NewTab),
                    shortcut,
                    shortcut_label,
                    ..
                } => Some((shortcut, shortcut_label)),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(
        shortcut(&bindings),
        (Some(original), Some(original.label()))
    );
    bindings.remove(&original);
    bindings.insert(remapped, Action::NewTab);
    assert_eq!(
        shortcut(&bindings),
        (Some(remapped), Some(remapped.label()))
    );
    bindings.remove(&remapped);
    assert_eq!(shortcut(&bindings), (None, None));
    bindings.clear();
    assert_eq!(shortcut(&bindings), (None, None));
}

#[test]
fn enabled_state_follows_existing_action_semantics() {
    let one = Context {
        tabs: 1,
        panes: 1,
        selection: false,
        writable: false,
        ..ready()
    };
    let model = menu_model(&Translator::new(Language::En), &Bindings::new(), one);
    for row in model.into_iter().flat_map(|menu| menu.rows) {
        if let Row::Item {
            command: Command::Kettle(action),
            enabled,
            ..
        } = row
        {
            assert_eq!(
                enabled,
                !matches!(
                    action,
                    Action::Copy
                        | Action::Paste
                        | Action::NextTab
                        | Action::PrevTab
                        | Action::ToggleZoom
                ),
                "{action:?}"
            );
        }
    }
    assert!(
        Context {
            zoomed: true,
            ..one
        }
        .enables(&Action::ToggleZoom)
    );
    assert!(
        Context {
            smart_copy: false,
            ..one
        }
        .enables(&Action::Copy)
    );
    assert!(one.enables(&Action::ClosePane)); // closes the tab with one pane
    assert!(one.enables(&Action::StartSearch));
    for row in menu_model(
        &Translator::new(Language::En),
        &Bindings::new(),
        Context::default(),
    )
    .into_iter()
    .flat_map(|m| m.rows)
    {
        if let Row::Item {
            command: Command::Kettle(_),
            enabled,
            ..
        } = row
        {
            assert!(!enabled);
        }
    }
}

#[test]
fn native_equivalents_cannot_shadow_configured_chords() {
    let trigger = Trigger::new(Mods::SUPER, Key::Char('q'));
    let bindings = Bindings::from([(trigger, Action::NewTab)]);
    let menu = menu_model(&Translator::new(Language::En), &bindings, ready());
    assert!(menu[0].rows.iter().any(|row| matches!(
        row,
        Row::Item {
            command: Command::Native(NativeAction::Quit),
            shortcut: None,
            ..
        }
    )));
}

/// AppKit never performs Kettle equivalents. The original event continues to
/// the existing dispatcher, whose yielding branch ends at the pane writer.
#[test]
fn menu_equivalents_leave_yielded_keys_on_the_original_pane_path() {
    let native = kettle_test_support::production_source(include_str!("native.rs"));
    let equivalents = kettle_test_support::production_source(include_str!("equivalents.rs"));
    let route = equivalents
        .split("fn perform_key_equivalent(")
        .nth(1)
        .unwrap()
        .split("\n    }")
        .next()
        .unwrap();
    assert!(route.contains("self.ivars().performKeyEquivalent(event)"));
    assert!(route.contains(".servicesMenu()"));
    assert!(route.contains("services.performKeyEquivalent(event)"));
    assert!(!route.contains("super"));
    assert!(native.contains("native.addItem(&make_item(mtm, title, selector, *shortcut))"));
    assert!(native.contains("if let Command::Kettle(action) = command"));
    assert!(native.contains("let submenu = new_menu(mtm, &native)"));
    assert!(native.contains("item.setRepresentedObject(Some(target_obj))"));
    let events = kettle_test_support::production_source(include_str!("../app/window_events.rs"));
    let dispatch = events
        .split("if let Some((trigger, act)) = resolve_keybind_action(")
        .nth(1)
        .unwrap();
    assert!(dispatch.contains("!self.chord_falls_through(ws, trigger, &act)"));
    assert!(
        dispatch
            .find("self.handle_action(ws, act, event_loop)")
            .unwrap()
            < dispatch
                .find("self.write_terminal_key_event(ws, &event, true)")
                .unwrap()
    );
    assert!(dispatch.contains("track_terminal_key_press("));
}

#[test]
fn resume_dispatch_and_reload_reconcile_menus_and_registrations() {
    let app = crate::app::app_source();
    assert!(app.contains("self.sync_macos_menu(None);"));
    assert!(app.contains("self.sync_macos_menu(Some(&ws));"));
    let reload = include_str!("../app/config_reload.rs");
    assert!(reload.contains("self.sync_macos_menu(None);"));
    let native = include_str!("native.rs");
    assert!(native.contains("s.model == model"));
    assert!(native.contains("app.setServicesMenu(Some(&services))"));
    assert!(native.contains("app.setWindowsMenu(Some(&submenu))"));
    assert!(
        include_str!("../macos_menu.rs").contains("cfg.language.resolve(kettle_i18n::os_locale)")
    );
}

#[test]
fn shell_profiles_are_dynamic_and_native_dispatch_includes_submenus() {
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        for names in [vec![], vec!["dev".into(), "ops".into()]] {
            let menus = menu_model_with_profiles(&tr, &Bindings::new(), ready(), &names);
            let actions = kettle_actions(&menus[1].rows);
            for action in [
                Action::SaveLayout,
                Action::OpenLayoutPicker,
                Action::OpenSsh,
            ] {
                assert!(actions.contains(&&action));
            }
            let launches: Vec<_> = actions
                .iter()
                .filter_map(|action| match action {
                    Action::LaunchProfile(name) => Some(name.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(launches, names);
            assert_eq!(
                actions.contains(&&Action::OpenProfilesDirectory),
                names.is_empty()
            );
        }
    }
}

#[test]
fn native_items_retain_the_command_they_were_built_with() {
    let native = kettle_test_support::production_source(include_str!("native.rs"));
    assert!(native.contains("action: action.clone()"));
    assert!(native.contains("item.setRepresentedObject(Some(target_obj))"));
    assert!(native.contains("invocation.action.clone()"));
    assert!(!native.contains("sender.tag()"));
}

#[test]
fn edit_menu_hints_follow_rebinding_and_require_a_pane() {
    let trigger = Trigger::new(Mods::SUPER | Mods::SHIFT, Key::Char('u'));
    let bindings = [(trigger, Action::HintMode)].into_iter().collect();
    for context in [
        ready(),
        Context {
            pane: false,
            ..ready()
        },
    ] {
        let model = menu_model(&Translator::new(Language::En), &bindings, context);
        let row = model[2]
            .rows
            .iter()
            .find(|row| {
                matches!(
                    row,
                    Row::Item {
                        command: Command::Kettle(Action::HintMode),
                        ..
                    }
                )
            })
            .unwrap();
        if let Row::Item {
            shortcut, enabled, ..
        } = row
        {
            assert_eq!(*shortcut, Some(trigger));
            assert_eq!(*enabled, context.pane);
        }
    }
}

#[test]
fn hint_menu_displays_its_control_shortcut_when_no_command_binding_exists() {
    let trigger = Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('h'));
    let bindings = [(trigger, Action::HintMode)].into_iter().collect();
    let model = menu_model(&Translator::new(Language::En), &bindings, ready());
    assert!(model[2].rows.iter().any(|row| matches!(row,Row::Item {
        command:Command::Kettle(Action::HintMode), shortcut:Some(found), ..
    } if *found == trigger)));
}
