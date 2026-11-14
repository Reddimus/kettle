fn empty_menu_model(tr: &Translator, bindings: &Bindings, context: Context) -> Vec<Menu> {
    menu_model_with_profiles(tr, bindings, context, &[])
}

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
        detachable_tabs: true,
        fullscreen: false,
        ..Context::default()
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
    let en = empty_menu_model(&Translator::new(Language::En), &Bindings::new(), ready());
    let es = empty_menu_model(&Translator::new(Language::Es), &Bindings::new(), ready());
    assert_eq!(
        en.iter().map(|menu| menu.title).collect::<Vec<_>>(),
        ["Kettle", "Shell", "Edit", "View", "Window", "Help"]
    );
    assert_eq!(
        es.iter().map(|menu| menu.title).collect::<Vec<_>>(),
        ["Kettle", "Shell", "Editar", "Ver", "Ventana", "Ayuda"]
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
                "New Window with Profile",
                "New Tab",
                "Save Layout…",
                "Open Layout…",
                "SSH Launcher…",
                "Broadcast Input",
                "|",
                "Split Right",
                "Split Left",
                "Split Down",
                "Split Up",
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
                "Quick-Select Hints",
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
                "Toggle Quick Terminal",
                "|",
                "Enter Full Screen"
            ],
            vec![
                "Minimize",
                "Zoom",
                "|",
                "Next Tab",
                "Previous Tab",
                "|",
                "Rename Tab…",
                "Move Tab Left",
                "Move Tab Right",
                "Move Tab to New Window",
                "|",
                "Bring All to Front"
            ],
            vec!["Kettle Help", "Keyboard Shortcuts"],
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
                "Nueva ventana con perfil",
                "Nueva pestaña",
                "Guardar diseño…",
                "Abrir diseño…",
                "Lanzador SSH…",
                "Enviar la entrada a varios paneles",
                "|",
                "Dividir a la derecha",
                "Dividir a la izquierda",
                "Dividir hacia abajo",
                "Dividir hacia arriba",
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
                "Mostrar u ocultar la terminal rápida",
                "|",
                "Entrar en pantalla completa"
            ],
            vec![
                "Minimizar",
                "Zoom",
                "|",
                "Pestaña siguiente",
                "Pestaña anterior",
                "|",
                "Cambiar el nombre de la pestaña…",
                "Mover la pestaña a la izquierda",
                "Mover la pestaña a la derecha",
                "Mover la pestaña a una ventana nueva",
                "|",
                "Traer todo al frente"
            ],
            vec!["Ayuda de Kettle", "Atajos de teclado"],
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
        [false, false, false, false, true, false]
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
            Action::SplitLeft,
            Action::SplitDown,
            Action::SplitUp,
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
            Action::ToggleQuickTerminal,
            Action::NextTab,
            Action::PrevTab,
            Action::EditTabTitle,
            Action::MoveTabLeft,
            Action::MoveTabRight,
            Action::MoveTabToNewWindow,
            Action::ShowHelp,
            Action::ShowKeybinds,
        ]
    );
}

/// The last menu is AppKit's Help menu, flagged so macOS adds its menu search
/// field: Kettle Help opens the README and Keyboard Shortcuts the searchable
/// shortcut list, in both languages.
#[test]
fn help_menu_is_last_and_flagged_with_help_and_shortcuts() {
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let model = empty_menu_model(&tr, &Bindings::new(), ready());
        let help = model.last().unwrap();
        assert!(help.help, "{language:?}");
        assert_eq!(model.iter().filter(|menu| menu.help).count(), 1);
        assert_eq!(help.title, tr.text(T::MacosMenuHelp));
        let items: Vec<_> = help
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Item {
                    title,
                    command: Command::Kettle(action),
                    enabled,
                    ..
                } => Some((title.as_str(), action.clone(), *enabled)),
                _ => None,
            })
            .collect();
        assert_eq!(
            items,
            [
                (tr.text(T::MacosMenuKettleHelp), Action::ShowHelp, true),
                (
                    tr.text(T::MacosMenuKeyboardShortcuts),
                    Action::ShowKeybinds,
                    true
                ),
            ]
        );
    }
    assert_eq!(
        Translator::new(Language::Es).text(T::MacosMenuHelp),
        "Ayuda"
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
        empty_menu_model(&tr, bindings, ready())
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
    let model = empty_menu_model(&Translator::new(Language::En), &Bindings::new(), one);
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
                        | Action::MoveTabLeft
                        | Action::MoveTabRight
                        | Action::MoveTabToNewWindow
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
    assert!(one.enables(&Action::EditTabTitle));
    assert!(one.enables(&Action::StartSearch));
    for row in empty_menu_model(
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
    let menu = empty_menu_model(&Translator::new(Language::En), &bindings, ready());
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
    // The key-equivalent menu holds `key_equivalent_rows`, which
    // `native_window_items_route_through_kettle_in_both_menus` checks holds
    // no Kettle command.
    assert!(native.contains("for row in super::key_equivalent_rows(&model)"));
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
    assert!(native.contains("if let Some(route) = command.route()"));
    assert!(native.contains("item.setRepresentedObject(Some(target_obj))"));
    assert!(native.contains("let event = match &self.route"));
    assert!(!native.contains("sender.tag()"));
    // The model's checkmark reaches AppKit; nothing else shows it.
    assert!(native.contains("item.setState(if *checked {"));
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
        let model = empty_menu_model(&Translator::new(Language::En), &bindings, context);
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
    let model = empty_menu_model(&Translator::new(Language::En), &bindings, ready());
    assert!(model[2].rows.iter().any(|row| matches!(row,Row::Item {
        command:Command::Kettle(Action::HintMode), shortcut:Some(found), ..
    } if *found == trigger)));
}

#[test]
fn profile_inventory_dispatch_stays_cached_during_blocked_refresh() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::Duration;
    let (entered_tx, entered) = mpsc::channel();
    let (release, releases) = mpsc::channel();
    let releases = std::sync::Mutex::new(releases);
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let completed = Arc::new(AtomicUsize::new(0));
    let finished = completed.clone();
    let (wakes, wake) = mpsc::channel();
    let mut inventory = ProfileInventory::with_enumerator(
        move || {
            let call = count.fetch_add(1, Ordering::SeqCst);
            entered_tx.send(call).unwrap();
            let _ = releases
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
            finished.fetch_add(1, Ordering::SeqCst);
            vec![format!("profile{call}")]
        },
        move || {
            let _ = wakes.send(());
        },
    )
    .unwrap();
    assert_eq!(entered.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
    // A blocked filesystem must not prevent dispatch using the empty startup cache.
    for _ in 0..30 {
        let model = menu_model(
            &Translator::new(Language::En),
            &Bindings::new(),
            ready(),
            &inventory,
        );
        assert!(
            !model
                .iter()
                .flat_map(|m| kettle_actions(&m.rows))
                .any(|a| matches!(a, Action::LaunchProfile(_)))
        );
    }
    assert_eq!(
        completed.load(Ordering::SeqCst),
        0,
        "dispatch must return while the startup enumerator is still blocked"
    );
    release.send(()).unwrap();
    wake.recv_timeout(Duration::from_secs(5)).unwrap();
    inventory.complete();
    assert_eq!(inventory.profiles(), ["profile0"]);
    inventory.refresh();
    assert_eq!(entered.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    for _ in 0..100 {
        inventory.refresh();
    }
    for _ in 0..30 {
        let model = menu_model(
            &Translator::new(Language::En),
            &Bindings::new(),
            ready(),
            &inventory,
        );
        assert!(
            model
                .iter()
                .flat_map(|m| kettle_actions(&m.rows))
                .any(|a| *a == Action::LaunchProfile("profile0".into()))
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        completed.load(Ordering::SeqCst),
        1,
        "dispatch must return while the refresh enumerator is still blocked"
    );
    release.send(()).unwrap();
    assert_eq!(entered.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
    wake.recv_timeout(Duration::from_secs(5)).unwrap();
    inventory.complete();
    release.send(()).unwrap();
    wake.recv_timeout(Duration::from_secs(5)).unwrap();
    inventory.complete();
    assert_eq!(inventory.profiles(), ["profile2"]);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let worker = inventory.worker.take().unwrap();
    drop(inventory);
    worker.join().unwrap();
}

/// Rename and move-to-new-window are reachable from the Window menu, in both
/// languages, through the same actions the keymap and palette dispatch. The
/// move is offered only when another tab stays behind and tabs may detach.
#[test]
fn window_menu_offers_rename_and_moving_the_tab() {
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let model = empty_menu_model(&tr, &Bindings::new(), ready());
        let window = model.iter().find(|menu| menu.windows).unwrap();
        assert_eq!(window.title, tr.text(T::MacosMenuWindow));
        for (text, action) in [
            (T::MacosMenuRenameTab, Action::EditTabTitle),
            (T::MacosMenuMoveTabToNewWindow, Action::MoveTabToNewWindow),
        ] {
            assert!(
                window.rows.iter().any(|row| matches!(
                    row,
                    Row::Item { title, command: Command::Kettle(a), enabled: true, .. }
                        if *a == action && title == tr.text(text)
                )),
                "{language:?}: {action:?}"
            );
        }
    }
    let enabled = |context: Context| context.enables(&Action::MoveTabToNewWindow);
    assert!(enabled(ready()));
    assert!(!enabled(Context { tabs: 1, ..ready() }));
    assert!(!enabled(Context {
        detachable_tabs: false,
        ..ready()
    }));
    assert!(
        Context {
            detachable_tabs: false,
            ..ready()
        }
        .enables(&Action::EditTabTitle)
    );
}

/// Move Tab Left and Right stay enabled on the first and last tab because
/// the action wraps there; the menu's tab count alone must agree with what
/// the mux does at every position, or the row would offer a silent no-op.
#[test]
fn move_tab_rows_agree_with_the_wrapping_mux_at_every_position() {
    let mux_with = |tabs: u64, active: usize| {
        let mut mux = crate::mux::Mux::new();
        for id in 1..=tabs {
            mux.tabs.push(crate::mux::Tab {
                root: crate::mux::Node::Leaf(id),
                focus: id,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
                scaled_zoom_owner: false,
                generated_group_name: None,
                lanes: Default::default(),
            });
        }
        mux.active = active;
        mux
    };
    for tabs in 1..=4u64 {
        let context = Context {
            tabs: tabs as usize,
            ..ready()
        };
        for active in 0..tabs as usize {
            for (action, delta) in [(Action::MoveTabLeft, -1), (Action::MoveTabRight, 1)] {
                assert_eq!(
                    context.enables(&action),
                    mux_with(tabs, active).nudge_active_tab(delta),
                    "{action:?} with {tabs} tabs, active {active}"
                );
            }
        }
    }
}

/// The menu bar follows macOS conventions: one full-screen item, AppKit's own,
/// on Control-Command-F unless the keymap owns that chord, titled for the
/// focused window's state; all four splits in Shell; the system's Minimize,
/// Zoom and Bring All to Front in Window; and English titles in title case.
#[test]
fn menu_bar_follows_macos_conventions() {
    let fullscreen_items = |model: &[Menu]| {
        model
            .iter()
            .flat_map(|menu| menu.rows.iter().map(move |row| (menu.title, row)))
            .filter_map(|(menu, row)| match row {
                Row::Item {
                    title,
                    command: Command::Native(NativeAction::ToggleFullScreen),
                    shortcut,
                    ..
                } => Some((menu, title.clone(), *shortcut)),
                Row::Item {
                    command: Command::Kettle(Action::ToggleFullscreen),
                    ..
                } => Some((menu, "duplicate".into(), None)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let ctrl_cmd_f = Trigger::new(Mods::SUPER | Mods::CTRL, Key::Char('f'));
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        for fullscreen in [false, true] {
            let context = Context {
                fullscreen,
                ..ready()
            };
            let model = empty_menu_model(&tr, &Bindings::new(), context);
            let title = tr.text(if fullscreen {
                T::MacosMenuExitFullscreen
            } else {
                T::MacosMenuFullscreen
            });
            assert_eq!(
                fullscreen_items(&model),
                [(
                    tr.text(T::MacosMenuView),
                    title.to_string(),
                    Some(ctrl_cmd_f)
                )],
                "{language:?} fullscreen={fullscreen}"
            );
        }
        let model = empty_menu_model(&tr, &Bindings::new(), ready());
        let shell = &model[1];
        let splits: Vec<_> = kettle_actions(&shell.rows)
            .into_iter()
            .filter(|action| {
                matches!(
                    action,
                    Action::SplitRight | Action::SplitLeft | Action::SplitDown | Action::SplitUp
                )
            })
            .collect();
        assert_eq!(
            splits,
            [
                &Action::SplitRight,
                &Action::SplitLeft,
                &Action::SplitDown,
                &Action::SplitUp
            ]
        );
        let window = model.iter().find(|menu| menu.windows).unwrap();
        let natives: Vec<_> = window
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Item {
                    command: Command::Native(action),
                    shortcut,
                    ..
                } => Some((*action, *shortcut)),
                _ => None,
            })
            .collect();
        assert_eq!(
            natives,
            [
                (
                    NativeAction::Minimize,
                    Some(Trigger::new(Mods::SUPER, Key::Char('m')))
                ),
                (NativeAction::Zoom, None),
                (NativeAction::BringAllToFront, None),
            ]
        );
    }
    // A chord the keymap owns is not taken by the native item.
    let bindings = Bindings::from([(ctrl_cmd_f, Action::NewTab)]);
    let model = empty_menu_model(&Translator::new(Language::En), &bindings, ready());
    assert_eq!(fullscreen_items(&model)[0].2, None);
    // English titles are in title case: every word but a short preposition,
    // article or conjunction starts with a capital, hyphenated parts too.
    let minor = [
        "a", "an", "and", "as", "at", "for", "in", "of", "on", "or", "the", "to", "with",
    ];
    let model = empty_menu_model(&Translator::new(Language::En), &Bindings::new(), ready());
    for menu in &model {
        for row in &menu.rows {
            let title = match row {
                Row::Item { title, .. } => title.as_str(),
                Row::Submenu { title, .. } | Row::Services(title) => title,
                Row::Separator => continue,
            };
            for (index, word) in title.split(' ').enumerate() {
                if index > 0 && minor.contains(&word) {
                    continue;
                }
                for part in word.split('-') {
                    assert!(
                        part.chars().next().is_some_and(|c| !c.is_lowercase()),
                        "{:?} in {title:?}",
                        menu.title
                    );
                }
            }
        }
    }
}

/// AppKit's own full-screen, Minimize and Zoom bypass winit and so fail on a
/// borderless window. Each item keeps the system's selector, so AppKit adds no
/// second full-screen item, but a Kettle target answers it and sends the
/// change through winit; the key-equivalent menu carries the same items, so
/// Control-Command-F and Command-M take that path too.
#[test]
fn native_window_items_route_through_kettle_in_both_menus() {
    use NativeAction as N;
    let all = [
        N::About,
        N::Hide,
        N::HideOthers,
        N::ShowAll,
        N::Quit,
        N::ToggleFullScreen,
        N::Minimize,
        N::Zoom,
        N::BringAllToFront,
    ];
    let routed = [
        (
            N::ToggleFullScreen,
            "toggleFullScreen:",
            Route::Action(Action::ToggleFullscreen),
        ),
        (
            N::Minimize,
            "performMiniaturize:",
            Route::Window(WindowCommand::Minimize),
        ),
        (N::Zoom, "performZoom:", Route::Window(WindowCommand::Zoom)),
    ];
    for action in all {
        let route = routed.iter().find(|(native, ..)| *native == action);
        let command = Command::Native(action);
        assert_eq!(
            command.route(),
            route.map(|(_, _, route)| route.clone()),
            "{action:?}"
        );
        if let Some((_, selector, _)) = route {
            assert_eq!(command.selector(), *selector);
        }
    }
    assert_eq!(
        Command::Kettle(Action::NewTab).route(),
        Some(Route::Action(Action::NewTab))
    );
    // The target answers every selector the model routes to it.
    let native = kettle_test_support::production_source(include_str!("native.rs"));
    for selector in routed
        .iter()
        .map(|(_, selector, _)| *selector)
        .chain(["kettleMenuAction:"])
    {
        assert!(
            native.contains(&format!("#[method({selector})]")),
            "{selector}"
        );
    }
    assert!(native.contains("Some(Sel::register(command.selector()))"));
    let output = kettle_test_support::production_source(include_str!("../app/output.rs"));
    assert!(output.contains("crate::macos_menu::perform(command, window)"));

    let natives = |model: &[Menu]| {
        model
            .iter()
            .flat_map(|menu| &menu.rows)
            .filter(|row| {
                matches!(
                    row,
                    Row::Item {
                        command: Command::Native(_),
                        ..
                    }
                )
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    let state = |rows: &[&Row], wanted: NativeAction| {
        rows.iter().find_map(|row| match row {
            Row::Item {
                command: Command::Native(action),
                shortcut,
                enabled,
                ..
            } if *action == wanted => Some((*shortcut, *enabled)),
            _ => None,
        })
    };
    let ctrl_cmd_f = Some(Trigger::new(Mods::SUPER | Mods::CTRL, Key::Char('f')));
    let cmd_m = Some(Trigger::new(Mods::SUPER, Key::Char('m')));
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        for (context, fullscreen, window_items) in [
            (ready(), true, true),
            (
                Context {
                    fullscreen: true,
                    ..ready()
                },
                true,
                false,
            ),
            (Context::default(), false, false),
        ] {
            let model = empty_menu_model(&tr, &Bindings::new(), context);
            let keys = key_equivalent_rows(&model);
            // The key-equivalent menu is exactly the native rows, each built
            // from the same row as its visible twin.
            assert_eq!(
                keys.iter().map(|row| (*row).clone()).collect::<Vec<_>>(),
                natives(&model)
            );
            assert!(keys.iter().all(|row| !matches!(
                row,
                Row::Item {
                    command: Command::Kettle(_),
                    ..
                }
            )));
            assert_eq!(
                state(&keys, N::ToggleFullScreen),
                Some((ctrl_cmd_f, fullscreen)),
                "{language:?} {context:?}"
            );
            assert_eq!(
                state(&keys, N::Minimize),
                Some((cmd_m, window_items)),
                "{language:?} {context:?}"
            );
            assert_eq!(
                state(&keys, N::Zoom),
                Some((None, window_items)),
                "{language:?} {context:?}"
            );
            for always in [N::Quit, N::Hide, N::BringAllToFront] {
                assert_eq!(state(&keys, always).map(|(_, on)| on), Some(true));
            }
        }
    }
}

/// The Shell menu's Broadcast Input submenu, for every `broadcast-default`
/// and scope: the checkmark is on the scope's row, the chord's hint is on the
/// row whose action is the chord's (Tab under `group`, Window under `all`, no
/// row under `off`), and Panes in This Group is greyed only while it is
/// unchecked and the focused pane has no group.
#[test]
fn broadcast_submenu_checks_the_scope_and_shows_the_chord_on_its_row() {
    use crate::app::BroadcastKind as K;
    use kettle_config::BroadcastDefault as D;
    let chord = Trigger::new(Mods::CTRL | Mods::SUPER, Key::Char('b'));
    let off = Trigger::new(Mods::CTRL | Mods::SHIFT | Mods::SUPER, Key::Char('b'));
    let bindings = Bindings::from([
        (chord, Action::ToggleBroadcastAll),
        (off, Action::ToggleBroadcastOff),
    ]);
    let broadcast_rows = |tr: &Translator, context: Context| {
        let shell = menu_model_with_profiles(tr, &bindings, context, &[])
            .into_iter()
            .nth(1)
            .expect("Shell menu");
        shell
            .rows
            .into_iter()
            .find_map(|row| match row {
                Row::Submenu { title, rows } if title == tr.text(T::MenuBroadcastInput) => {
                    Some(rows)
                }
                _ => None,
            })
            .expect("Broadcast Input submenu")
    };
    for (language, titles) in [
        (
            Language::En,
            [
                "Off",
                "Panes in This Tab",
                "Panes in This Window",
                "Panes in This Group",
            ],
        ),
        (
            Language::Es,
            [
                "Desactivado",
                "Paneles de esta pestaña",
                "Paneles de esta ventana",
                "Paneles de este grupo",
            ],
        ),
    ] {
        let tr = Translator::new(language);
        for default in [D::Group, D::All, D::Off] {
            for (kind, pane_group, checked_row) in [
                (K::Off, false, 0),
                (K::Tab, false, 1),
                (K::Window, true, 2),
                (K::FocusedGroup, true, 3),
                (K::OtherGroup, false, 3),
            ] {
                let context = Context {
                    broadcast: kind,
                    pane_group,
                    broadcast_default: default,
                    ..ready()
                };
                let rows = broadcast_rows(&tr, context);
                let case = format!("{language:?} {default:?} {kind:?}");
                let mut chord_rows = Vec::new();
                for (index, row) in rows.iter().enumerate() {
                    let Row::Item {
                        title,
                        command: Command::Kettle(_),
                        shortcut,
                        shortcut_label,
                        enabled,
                        checked,
                    } = row
                    else {
                        panic!("{case}: row {index} is a Kettle item");
                    };
                    assert_eq!(title, titles[index], "{case}");
                    assert_eq!(*checked, index == checked_row, "{case}: row {index}");
                    assert_eq!(
                        *enabled,
                        index != 3 || pane_group || index == checked_row,
                        "{case}: row {index}"
                    );
                    if *shortcut == Some(chord) {
                        assert_eq!(shortcut_label.as_deref(), Some(chord.label().as_str()));
                        chord_rows.push(index);
                    }
                }
                let expected: &[usize] = match default {
                    D::Group => &[1],
                    D::All => &[2],
                    D::Off => &[],
                };
                assert_eq!(chord_rows, expected, "{case}");
                assert!(
                    matches!(&rows[0], Row::Item { shortcut: Some(s), .. } if *s == off),
                    "{case}: Off shows the off chord"
                );
            }
        }
    }
    // Without a focused window nothing in it can be chosen.
    for row in broadcast_rows(&Translator::new(Language::En), Context::default()) {
        assert!(matches!(row, Row::Item { enabled: false, .. }));
    }
}

/// Each `broadcast-default` reaches every scope from the menu, and the
/// chord's action stands in for the scope it turns on.
#[test]
fn broadcast_submenu_reaches_every_scope_under_every_default() {
    use kettle_config::BroadcastDefault as D;
    for (default, tab, window) in [
        (
            D::Group,
            Action::ToggleBroadcastAll,
            Action::ToggleBroadcastWindow,
        ),
        (
            D::All,
            Action::ToggleBroadcastPanesInTab,
            Action::ToggleBroadcastAll,
        ),
        (
            D::Off,
            Action::ToggleBroadcastPanesInTab,
            Action::ToggleBroadcastWindow,
        ),
    ] {
        let context = Context {
            broadcast_default: default,
            pane_group: true,
            ..ready()
        };
        let model = empty_menu_model(&Translator::new(Language::En), &Bindings::new(), context);
        let actions = kettle_actions(&model[1].rows);
        for action in [
            &Action::ToggleBroadcastOff,
            &tab,
            &window,
            &Action::ToggleBroadcastGroup,
        ] {
            assert!(actions.contains(&action), "{default:?} {action:?}");
        }
    }
}
