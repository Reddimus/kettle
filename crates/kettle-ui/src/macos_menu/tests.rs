fn empty_menu_model(tr: &Translator, bindings: &Bindings, context: Context) -> Vec<Menu> {
    menu_model_with_profiles(tr, bindings, context, Some(&[]))
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
                    Row::Info(title) | Row::Services(title) | Row::Submenu { title, .. } => *title,
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
            vec![
                "Next Tab",
                "Previous Tab",
                "|",
                "Rename Tab…",
                "Move Tab Left",
                "Move Tab Right",
                "Move Tab to New Window"
            ],
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
            vec![
                "Pestaña siguiente",
                "Pestaña anterior",
                "|",
                "Cambiar el nombre de la pestaña…",
                "Mover la pestaña a la izquierda",
                "Mover la pestaña a la derecha",
                "Mover la pestaña a una ventana nueva"
            ],
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
            Action::EditTabTitle,
            Action::MoveTabLeft,
            Action::MoveTabRight,
            Action::MoveTabToNewWindow,
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
            let menus = menu_model_with_profiles(&tr, &Bindings::new(), ready(), Some(&names));
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
    use crate::profile_inventory::test_support::BlockedInventory;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let fixture = BlockedInventory::new();
    let BlockedInventory {
        mut inventory,
        calls,
        completed,
        entered,
        release,
        wake,
    } = fixture;
    assert_eq!(entered.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
    // A blocked filesystem must not prevent dispatch using the pending startup model.
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
    BlockedInventory {
        inventory,
        calls,
        completed,
        entered,
        release,
        wake,
    }
    .join();
}

#[test]
fn native_profile_model_distinguishes_pending_from_first_empty_completion() {
    use crate::profile_inventory::test_support::BlockedInventory;
    use std::time::Duration;

    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let mut fixture = BlockedInventory::with_results(|call| match call {
            0 | 1 => Vec::new(),
            2 => vec!["work".into()],
            _ => vec!["home".into()],
        });
        fixture
            .entered
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let pending = menu_model(&tr, &Bindings::new(), ready(), &fixture.inventory);
        let profile_rows = |model: &[Menu]| match &model[1].rows[2] {
            Row::Submenu { rows, .. } => rows.clone(),
            _ => panic!("expected profile submenu"),
        };
        assert_eq!(
            profile_rows(&pending),
            vec![Row::Info(tr.text(T::MenuProfilesPending))]
        );
        assert!(kettle_actions(&profile_rows(&pending)).is_empty());
        fixture.release.send(()).unwrap();
        fixture.wake.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            fixture.inventory.complete(),
            "first empty scan must synchronize the native model"
        );
        let ready_empty = menu_model(&tr, &Bindings::new(), ready(), &fixture.inventory);
        assert!(matches!(profile_rows(&ready_empty).as_slice(), [Row::Item {
            title, command: Command::Kettle(Action::OpenProfilesDirectory), ..
        }] if title == tr.text(T::MenuNoProfiles)));
        assert_eq!(
            profile_rows(&pending),
            vec![Row::Info(tr.text(T::MenuProfilesPending))]
        );
        for call in 1..=2 {
            fixture.inventory.refresh();
            assert_eq!(
                fixture
                    .entered
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap(),
                call
            );
            assert!(
                fixture.inventory.snapshot().is_some(),
                "later refresh is never pending"
            );
            fixture.release.send(()).unwrap();
            fixture.wake.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(fixture.inventory.complete(), call > 1);
        }
        let work = menu_model(&tr, &Bindings::new(), ready(), &fixture.inventory);
        let captured = profile_rows(&work);
        fixture.inventory.refresh();
        assert_eq!(
            fixture
                .entered
                .recv_timeout(Duration::from_secs(5))
                .unwrap(),
            3
        );
        fixture.release.send(()).unwrap();
        fixture.wake.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(fixture.inventory.complete());
        let home = menu_model(&tr, &Bindings::new(), ready(), &fixture.inventory);
        assert_eq!(
            kettle_actions(&captured),
            vec![&Action::LaunchProfile("work".into())]
        );
        assert_eq!(
            kettle_actions(&profile_rows(&home)),
            vec![&Action::LaunchProfile("home".into())]
        );
        fixture.join();
    }
}

/// Rename and move-to-new-window are reachable from the Window menu, in both
/// languages, through the same actions the keymap and palette dispatch. The
/// move is offered only when another tab stays behind and tabs may detach.
#[test]
fn window_menu_offers_rename_and_moving_the_tab() {
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let model = empty_menu_model(&tr, &Bindings::new(), ready());
        let window = model.last().unwrap();
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
