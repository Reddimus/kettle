use super::*;
use crate::profile_inventory::test_support::BlockedInventory;
use std::sync::{atomic::Ordering, mpsc};
use std::time::Duration;

fn menu(items: Vec<ContextMenuItem>) -> ContextMenuState {
    ContextMenuState {
        anchor: (100.0, 120.0),
        anchor_scale: 1.0,
        items,
        highlight: 0,
        drill_stack: Vec::new(),
        scroll_offset: 0,
        typeahead_buf: String::new(),
        typeahead_until: None,
    }
}

fn launch_names(items: &[ContextMenuItem]) -> Vec<&str> {
    items
        .iter()
        .flat_map(|item| match item {
            ContextMenuItem::Submenu { items, .. } => launch_names(items),
            ContextMenuItem::DynamicItem {
                action: Action::LaunchProfile(name),
                ..
            } => vec![name.as_str()],
            _ => Vec::new(),
        })
        .collect()
}

fn complete(fixture: &mut BlockedInventory) -> bool {
    fixture.release.send(()).unwrap();
    fixture.wake.recv_timeout(Duration::from_secs(5)).unwrap();
    fixture.inventory.complete()
}

fn start_refresh(fixture: &BlockedInventory, call: usize) {
    fixture.inventory.refresh();
    assert_eq!(
        fixture
            .entered
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        call
    );
}

fn assert_pending(items: &[ContextMenuItem], tr: &Translator) {
    assert!(matches!(items, [ContextMenuItem::Info { label, .. }]
        if label == tr.text(T::MenuProfilesPending)));
    assert!(!item_is_dispatchable(&items[0]));
    assert!(item_to_click(&items[0], 0).is_none());
}

#[test]
fn actual_new_tab_and_palette_snapshots_return_while_profile_enumeration_is_blocked() {
    let mut fixture = BlockedInventory::new();
    assert_eq!(
        fixture
            .entered
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        0
    );
    assert!(fixture.inventory.snapshot().is_none());
    let shells = vec![("Test shell".into(), vec!["test-shell".into()])];
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = Translator::new(language);
        let items = filter_disabled(App::shell_menu_items(
            Some(&shells),
            fixture.inventory.snapshot(),
            &tr,
        ));
        assert!(launch_names(&items).is_empty());
        let mut pending = menu(items);
        assert!(pending.enter_submenu(shells.len() + 1));
        assert_pending(&pending.items, &tr);
        let mut choices = Vec::new();
        append_profile_choices(&mut choices, fixture.inventory.snapshot(), &tr);
        let choices = filter_disabled(choices);
        assert!(matches!(&choices[0], ContextMenuItem::Submenu { items, .. }
            if matches!(items.as_slice(), [ContextMenuItem::Info { .. }])));
        let mut ws = WindowState::new(1, Mux::new());
        open_profile_palette(&mut ws, fixture.inventory.snapshot());
        assert!(ws.palette_profiles.is_none());
        let palette = palette_entries(
            None,
            &tr,
            ws.palette_profiles.as_deref(),
            kettle_config::BroadcastDefault::default(),
        );
        assert!(!palette.iter().any(|(_, action)| matches!(
            action,
            Action::LaunchProfile(_) | Action::OpenProfilesDirectory
        )));
        assert_eq!(
            palette_profile_status(ws.palette_profiles.as_deref(), &tr),
            Some(tr.text(T::MenuProfilesPending))
        );
    }
    assert_eq!(fixture.completed.load(Ordering::SeqCst), 0);
    assert!(complete(&mut fixture));
    start_refresh(&fixture, 1);
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = Translator::new(language);
        let profiles = fixture.inventory.snapshot();
        for _ in 0..30 {
            let items = App::shell_menu_items(Some(&shells), profiles, &tr);
            assert_eq!(launch_names(&items), ["profile0"]);
            let mut snapshot = menu(filter_disabled(items));
            assert!(snapshot.enter_submenu(shells.len() + 1));
            assert_eq!(snapshot.items.len(), 1);
            assert!(matches!(item_to_click(&snapshot.items[0], 0),
                Some(ContextMenuClick::Action(Action::LaunchProfile(name))) if name == "profile0"));
            assert!(
                matches!(&snapshot.items[0], ContextMenuItem::DynamicItem { label, .. }
                if label == &tr.palette_launch_profile("profile0"))
            );
            let mut choices = Vec::new();
            append_profile_choices(&mut choices, profiles, &tr);
            assert!(matches!(&choices[1], ContextMenuItem::Submenu { items, .. }
                if matches!(items.as_slice(), [ContextMenuItem::ProfileChoice { profile, .. }] if profile == "profile0")));
            assert!(
                palette_entries(
                    None,
                    &tr,
                    profiles,
                    kettle_config::BroadcastDefault::default()
                )
                .iter()
                .any(|(_, action)| *action == Action::LaunchProfile("profile0".into()))
            );
        }
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        fixture.completed.load(Ordering::SeqCst),
        1,
        "menu projections must return before the blocked refresh is released"
    );
    assert!(complete(&mut fixture));
    fixture.join();
}

#[test]
fn first_empty_completion_publishes_readiness_and_open_pending_snapshots_stay_honest() {
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = Translator::new(language);
        let mut fixture = BlockedInventory::with_results(|call| {
            if call < 2 {
                Vec::new()
            } else {
                vec!["work".into()]
            }
        });
        fixture
            .entered
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let mut pending = menu(filter_disabled(App::new_tab_menu_items(
            &[],
            fixture.inventory.snapshot(),
            &tr,
        )));
        assert!(pending.enter_submenu(0));
        let mut palette = WindowState::new(1, Mux::new());
        open_profile_palette(&mut palette, fixture.inventory.snapshot());
        assert!(
            complete(&mut fixture),
            "None to ready-empty must notify native menu synchronization"
        );
        assert_eq!(fixture.inventory.snapshot(), Some([].as_slice()));
        assert_pending(&pending.items, &tr);
        assert!(palette.palette_profiles.is_none());
        assert_eq!(
            palette_profile_status(palette.palette_profiles.as_deref(), &tr),
            Some(tr.text(T::MenuProfilesPending))
        );
        let reopened = profile_launch_items(fixture.inventory.snapshot(), &tr);
        assert!(matches!(
            reopened.as_slice(),
            [ContextMenuItem::DynamicItem {
                action: Action::OpenProfilesDirectory,
                ..
            }]
        ));
        open_profile_palette(&mut palette, fixture.inventory.snapshot());
        assert_eq!(palette.palette_profiles, Some(Vec::new()));
        assert!(palette_profile_status(palette.palette_profiles.as_deref(), &tr).is_none());
        assert_eq!(
            profile_launch_entries(palette.palette_profiles.as_deref(), &tr)[0].1,
            Action::OpenProfilesDirectory
        );
        start_refresh(&fixture, 1);
        assert_eq!(fixture.inventory.snapshot(), Some([].as_slice()));
        assert!(
            !complete(&mut fixture),
            "an unchanged ready-empty result is not a new model"
        );
        start_refresh(&fixture, 2);
        assert_eq!(fixture.inventory.snapshot(), Some([].as_slice()));
        assert!(complete(&mut fixture));
        assert_pending(&pending.items, &tr);
        assert_eq!(palette.palette_profiles, Some(Vec::new()));
        open_profile_palette(&mut palette, fixture.inventory.snapshot());
        assert_eq!(
            palette.palette_profiles.as_deref(),
            Some(["work".to_string()].as_slice())
        );
        fixture.join();
    }
}

#[derive(Debug, PartialEq)]
struct LevelSnapshot {
    anchor: (f32, f32),
    scale: f64,
    items: Vec<String>,
    highlight: usize,
    offset: usize,
}

fn item_snapshot(items: &[ContextMenuItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| match item {
            ContextMenuItem::Item {
                label,
                action,
                enabled,
            } => format!("{label}:{action:?}:{enabled}"),
            ContextMenuItem::DynamicItem {
                label,
                action,
                enabled,
                ..
            } => format!("{label}:{action:?}:{enabled}"),
            ContextMenuItem::Submenu { label, items } => {
                format!("{label}:{:?}", item_snapshot(items))
            }
            ContextMenuItem::NewTabShell { label, argv } => format!("{label}:{argv:?}"),
            ContextMenuItem::ProfileChoice { label, profile } => format!("{label}:{profile}"),
            ContextMenuItem::Info { label, .. } => format!("info:{label}"),
            ContextMenuItem::Separator => "separator".into(),
            _ => panic!("unexpected fixture row"),
        })
        .collect()
}

fn level_snapshot(state: &ContextMenuState) -> Vec<LevelSnapshot> {
    state
        .drill_stack
        .iter()
        .map(|level| LevelSnapshot {
            anchor: level.anchor,
            scale: level.anchor_scale,
            items: item_snapshot(&level.items),
            highlight: level.highlight,
            offset: level.scroll_offset,
        })
        .chain(std::iter::once(LevelSnapshot {
            anchor: state.anchor,
            scale: state.anchor_scale,
            items: item_snapshot(&state.items),
            highlight: state.highlight,
            offset: state.scroll_offset,
        }))
        .collect()
}

#[test]
fn worker_completions_preserve_deep_menu_viewports_typeahead_and_palette_editor_until_reopen() {
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = Translator::new(language);
        let initial: Vec<String> = (0..64)
            .map(|i| format!("p{i:03}"))
            .chain(["work".into()])
            .collect();
        let first = initial.clone();
        let mut fixture = BlockedInventory::with_results(move |call| match call {
            0 => first.clone(),
            1 => std::iter::once("a-added".into())
                .chain(first.clone())
                .collect(),
            2 => vec!["p050".into(), "p051".into()],
            _ => Vec::new(),
        });
        fixture
            .entered
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(complete(&mut fixture));
        let shells: Vec<_> = (0..64)
            .map(|i| (format!("shell{i}"), vec!["test-shell".into()]))
            .collect();
        let mut state = menu(vec![ContextMenuItem::Submenu {
            label: "Window".into(),
            items: filter_disabled(App::new_tab_menu_items(
                &shells,
                fixture.inventory.snapshot(),
                &tr,
            )),
        }]);
        assert!(state.enter_submenu(0));
        state.scroll_offset = 40;
        state.anchor = (130.0, 140.0);
        assert!(state.enter_submenu(65));
        state.highlight = 0;
        state.scroll_offset = 20;
        state.typeahead_buf = "p05".into();
        state.typeahead_until = Some(std::time::Instant::now() + Duration::from_secs(1));
        let before = level_snapshot(&state);
        let typeahead = (state.typeahead_buf.clone(), state.typeahead_until);
        let captured = item_to_click(&state.items[64], 64).unwrap();
        let mut ws = WindowState::new(1, Mux::new());
        ws.context_menu = Some(state);
        ws.accessibility_pending = false;
        ws.reuse_pane_snapshots_once = false;
        let mut palette = WindowState::new(2, Mux::new());
        open_profile_palette(&mut palette, fixture.inventory.snapshot());
        let mut editor = crate::text_editor::TextEditor::from("work");
        editor.move_left(true, false);
        assert!(editor.selection().is_some());
        let commands = palette_entries(
            None,
            &tr,
            palette.palette_profiles.as_deref(),
            kettle_config::BroadcastDefault::default(),
        );
        let ranked = kettle_config::palette::rank_entries(&editor, &commands, &tr);
        let selected = ranked
            .iter()
            .position(|&i| commands[i].1 == Action::LaunchProfile("work".into()))
            .unwrap();
        palette.palette_input = Some((editor.clone(), selected));
        for call in 1..=3 {
            let last = fixture.inventory.profiles().to_vec();
            start_refresh(&fixture, call);
            assert_eq!(
                fixture.inventory.profiles(),
                last.as_slice(),
                "refresh keeps the last completion"
            );
            assert!(complete(&mut fixture));
            let state = ws.context_menu.as_ref().unwrap();
            assert_eq!(level_snapshot(state), before);
            assert_eq!(
                (&state.typeahead_buf, state.typeahead_until),
                (&typeahead.0, typeahead.1)
            );
            assert!(!ws.accessibility_pending && !ws.reuse_pane_snapshots_once);
            assert_eq!(
                palette.palette_profiles.as_deref(),
                Some(initial.as_slice())
            );
            assert_eq!(
                palette.palette_input.as_ref(),
                Some(&(editor.clone(), selected))
            );
            let commands = palette_entries(
                None,
                &tr,
                palette.palette_profiles.as_deref(),
                kettle_config::BroadcastDefault::default(),
            );
            assert_eq!(
                palette_enter_action(&editor, selected, &commands, &tr),
                Some(Action::LaunchProfile("work".into()))
            );
            let reopened = App::new_tab_menu_items(&[], fixture.inventory.snapshot(), &tr);
            assert_eq!(
                launch_names(&reopened),
                fixture
                    .inventory
                    .profiles()
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            );
        }
        let ContextMenuClick::Action(Action::LaunchProfile(name)) = captured else {
            panic!("expected captured work action")
        };
        assert_eq!(name, "work");
        let current = fixture.inventory.profiles().to_vec();
        let (failed_tx, failed) = mpsc::channel();
        spawn_profile_launch(
            &ProfileLaunchGate::default(),
            name,
            move || Ok(current),
            |_| panic!("stale row retargeted"),
            |_: ()| panic!("stale row reached reaper"),
            move |error| {
                failed_tx.send(error.kind()).unwrap();
            },
        )
        .unwrap()
        .join()
        .unwrap();
        assert_eq!(failed.recv().unwrap(), std::io::ErrorKind::NotFound);
        open_profile_palette(&mut palette, fixture.inventory.snapshot());
        assert_eq!(palette.palette_profiles, Some(Vec::new()));
        assert_eq!(palette.palette_input.unwrap().0.text(), "");
        fixture.join();
    }
    // App itself requires a native event loop. Guard its completion routing as
    // well as exercising the actual worker/result-consumption path above.
    let source = include_str!("../profiles.rs");
    let handler = source
        .split("fn profiles_ready(")
        .nth(1)
        .unwrap()
        .split("pub(super) fn open_profile_palette")
        .next()
        .unwrap();
    assert!(handler.contains("self.profile_inventory.complete()"));
    assert!(handler.contains("self.sync_macos_menu(None)"));
    assert!(!handler.contains("windows") && !handler.contains("WindowState"));
}

#[test]
fn pending_palette_header_does_not_shift_ranked_keyboard_or_pointer_actions() {
    let mut saw_elided_label = false;
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = Translator::new(language);
        let cfg = Config::default();
        let mut ws = WindowState::new(1, Mux::new());
        open_profile_palette(&mut ws, None);
        for query in ["", "split", "zzzzqqqq"] {
            ws.palette_input = Some((query.into(), 0));
            let commands = palette_entries(
                None,
                &tr,
                ws.palette_profiles.as_deref(),
                kettle_config::BroadcastDefault::default(),
            );
            let ranked = kettle_config::palette::rank_entries(query, &commands, &tr);
            let list = open_picker_list(&ws, &cfg, "dark-theme", &tr, None).unwrap();
            assert_eq!(
                palette_profile_status(ws.palette_profiles.as_deref(), &tr),
                Some(tr.text(T::MenuProfilesPending))
            );
            if ranked.is_empty() {
                assert_eq!(list.rows.len(), 1);
                assert!(!list.rows[0].enabled);
                continue;
            }
            assert_eq!(list.rows.len(), ranked.len());
            for (row, &index) in list.rows.iter().zip(&ranked) {
                assert_eq!(row.label, commands[index].0.shown(&tr));
                assert!(row.enabled && !row.separator);
            }
            for surface in [(360.0, 600.0), (800.0, 600.0)] {
                let menu = picker_context_menu(
                    open_picker_list(&ws, &cfg, "dark-theme", &tr, None).unwrap(),
                    surface,
                    (8.0, 16.0),
                    (8.0, 16.0),
                    0.0,
                )
                .unwrap();
                for index in 0..ranked.len().min(4) {
                    let row = menu.visible_row_rect(index).unwrap();
                    ws.cursor.x = (row.0 + 1.0) as f64;
                    ws.cursor.y = (row.1 + 1.0) as f64;
                    assert!(apply_picker_pointer(
                        &mut ws,
                        &cfg,
                        &menu,
                        16.0,
                        PickerPointer::Press
                    ));
                    let selected = ws.palette_input.as_ref().unwrap().1;
                    assert_eq!(selected, index);
                    assert_eq!(
                        palette_enter_action(query, selected, &commands, &tr),
                        Some(commands[ranked[index]].1.clone())
                    );
                    let full_label = commands[ranked[selected]].0.shown(&tr);
                    let displayed = menu.rows[index].label.as_str();
                    saw_elided_label |= displayed != full_label;
                    assert!(
                        displayed == full_label
                            || displayed.strip_suffix('\u{2026}').is_some_and(|prefix| {
                                !prefix.is_empty() && full_label.starts_with(prefix)
                            }),
                        "row {index} displayed {displayed:?}, expected {full_label:?} or its elided prefix"
                    );
                }
            }
        }
    }
    assert!(saw_elided_label, "fixture must exercise label elision");
}

#[test]
fn launch_worker_validates_current_membership_before_launch_and_reports_failure() {
    let gate = ProfileLaunchGate::default();
    let caller = std::thread::current().id();
    let (entered_tx, entered) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let (launched_tx, launched) = mpsc::channel();
    let (failed_tx, failed) = mpsc::channel();
    let worker = spawn_profile_launch(
        &gate,
        "work".into(),
        move || {
            entered_tx.send(std::thread::current().id()).unwrap();
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(vec!["work".into()])
        },
        move |name| {
            launched_tx.send(name.to_string()).unwrap();
            Err::<(), _>(std::io::ErrorKind::PermissionDenied.into())
        },
        |_| panic!("failed spawn reached reaper"),
        move |error| {
            failed_tx.send(error.kind()).unwrap();
        },
    )
    .unwrap();
    assert_ne!(
        entered.recv_timeout(Duration::from_secs(5)).unwrap(),
        caller
    );
    assert_eq!(launched.try_recv(), Err(mpsc::TryRecvError::Empty));
    release.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(launched.recv().unwrap(), "work");
    assert_eq!(failed.recv().unwrap(), std::io::ErrorKind::PermissionDenied);
    for name in [
        "removed",
        "WORK",
        "../work",
        "two words",
        "--profile=work",
        "",
    ] {
        let (result_tx, result) = mpsc::channel();
        spawn_profile_launch(
            &gate,
            name.into(),
            || Ok(vec!["work".into()]),
            |_| panic!("unlisted name reached process launch"),
            |_: ()| panic!("unlisted name reached reaper"),
            move |error| {
                result_tx.send(error.kind()).unwrap();
            },
        )
        .unwrap()
        .join()
        .unwrap();
        assert_eq!(result.recv().unwrap(), std::io::ErrorKind::NotFound);
    }
    let (failed_tx, failed) = mpsc::channel();
    spawn_profile_launch(
        &gate,
        "work".into(),
        || Err(std::io::ErrorKind::PermissionDenied.into()),
        |_| panic!("failed scan reached spawn"),
        |_: ()| panic!("failed scan reached reaper"),
        move |error| {
            failed_tx.send(error.kind()).unwrap();
        },
    )
    .unwrap()
    .join()
    .unwrap();
    assert_eq!(failed.recv().unwrap(), std::io::ErrorKind::PermissionDenied);
    // All preceding failure paths must have returned admission.
    spawn_profile_launch(
        &gate,
        "work".into(),
        || Ok(vec!["work".into()]),
        |_| Ok(()),
        |_| Ok(()),
        |_| panic!("successful launch reported failure"),
    )
    .unwrap()
    .join()
    .unwrap();
}

#[test]
fn launch_admission_bounds_blocked_scans_but_not_child_lifetimes() {
    use std::sync::{Arc, atomic::AtomicUsize};
    let gate = ProfileLaunchGate::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let (entered_tx, entered) = mpsc::channel();
    let (release_scan, scan) = mpsc::channel();
    let (reaping_tx, reaping) = mpsc::channel();
    let (release_child, child) = mpsc::channel();
    let first = spawn_profile_launch(
        &gate,
        "work".into(),
        move || {
            count.fetch_add(1, Ordering::SeqCst);
            entered_tx.send(()).unwrap();
            scan.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(vec!["work".into()])
        },
        |_| Ok(()),
        move |_| {
            reaping_tx.send(()).unwrap();
            child.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        },
        |_| panic!("first launch failed"),
    )
    .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    for _ in 0..64 {
        let count = calls.clone();
        let rejected = spawn_profile_launch(
            &gate,
            "work".into(),
            move || {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(vec!["work".into()])
            },
            |_| Ok(()),
            |_| Ok(()),
            |_| panic!("rejected request started"),
        );
        assert_eq!(rejected.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release_scan.send(()).unwrap();
    reaping.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!first.is_finished());
    let count = calls.clone();
    spawn_profile_launch(
        &gate,
        "work".into(),
        move || {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(vec!["work".into()])
        },
        |_| Ok(()),
        |_| Ok(()),
        |_| panic!("second launch failed"),
    )
    .unwrap()
    .join()
    .unwrap();
    assert!(!first.is_finished(), "first child must still be reaping");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    release_child.send(()).unwrap();
    first.join().unwrap();
}

#[test]
fn scan_errors_preserve_unknown_or_last_successful_snapshot() {
    let mut fixture = BlockedInventory::with_fallible_results(|call| match call {
        0 | 3 => Err(std::io::ErrorKind::PermissionDenied.into()),
        1 => Ok(Vec::new()),
        _ => Ok(vec!["work".into()]),
    });
    fixture
        .entered
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(!complete(&mut fixture));
    assert!(fixture.inventory.snapshot().is_none());
    start_refresh(&fixture, 1);
    assert!(complete(&mut fixture));
    assert_eq!(fixture.inventory.snapshot(), Some([].as_slice()));
    start_refresh(&fixture, 2);
    assert!(complete(&mut fixture));
    let good = fixture.inventory.snapshot().unwrap().to_vec();
    start_refresh(&fixture, 3);
    assert!(!complete(&mut fixture));
    assert_eq!(fixture.inventory.snapshot(), Some(good.as_slice()));
    start_refresh(&fixture, 4);
    assert!(!complete(&mut fixture));
    fixture.join();
}

#[test]
fn scan_error_does_not_erase_an_unconsumed_success() {
    let mut fixture = BlockedInventory::with_fallible_results(|call| match call {
        0 => Ok(vec!["work".into()]),
        _ => Err(std::io::ErrorKind::PermissionDenied.into()),
    });
    fixture
        .entered
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    fixture.release.send(()).unwrap();
    fixture.wake.recv_timeout(Duration::from_secs(5)).unwrap();
    // Leave the successful result unconsumed while a later scan fails.
    start_refresh(&fixture, 1);
    fixture.release.send(()).unwrap();
    // Entry to scan 2 acknowledges publication/retention of scan 1's result.
    start_refresh(&fixture, 2);
    assert!(fixture.inventory.complete());
    assert_eq!(fixture.inventory.snapshot().unwrap(), ["work"]);
    assert_eq!(fixture.wake.try_recv(), Err(mpsc::TryRecvError::Empty));
    assert!(!complete(&mut fixture));
    assert_eq!(fixture.inventory.snapshot().unwrap(), ["work"]);
    fixture.join();
}

#[test]
fn profile_cycling_distinguishes_unavailable_empty_and_known_names() {
    let names = vec!["home".into(), "work".into()];
    for forward in [false, true] {
        assert_eq!(
            profile_cycle(None, None, forward),
            ProfileCycle::Unavailable
        );
        assert_eq!(profile_cycle(Some(&[]), None, forward), ProfileCycle::Empty);
        assert_eq!(
            profile_cycle(Some(&names), Some("home"), forward),
            ProfileCycle::Next("work".into())
        );
    }
    assert_eq!(
        profile_cycle(Some(&names), Some("work"), true),
        ProfileCycle::Next("home".into())
    );
    assert_eq!(
        profile_cycle(Some(&names), None, false),
        ProfileCycle::Next("work".into())
    );
}

#[test]
fn pending_palette_status_survives_long_queries_narrow_widths_and_result_scroll() {
    use unicode_width::UnicodeWidthStr as _;
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = Translator::new(language);
        let cfg = Config::default();
        let mut ws = WindowState::new(1, Mux::new());
        open_profile_palette(&mut ws, None);
        for (cell, text_cell) in [
            ((8.0, 16.0), (8.0, 16.0)),
            ((4.0, 16.0), (8.0, 16.0)),
            ((8.0, 8.0), (8.0, 16.0)),
            ((4.5, 9.0), (9.0, 18.0)),
            ((8.0, 16.0), (16.0, 32.0)),
        ] {
            for width in [328.0, 358.0, 720.0] {
                for query in [String::new(), "split".into(), "query".repeat(40)] {
                    ws.palette_input = Some((query.clone().into(), 0));
                    let list = open_picker_list(&ws, &cfg, "dark-theme", &tr, None).unwrap();
                    let ranked = kettle_config::palette::rank_entries(
                        &query,
                        &palette_entries(
                            None,
                            &tr,
                            None,
                            kettle_config::BroadcastDefault::default(),
                        ),
                        &tr,
                    );
                    let mut menu =
                        picker_context_menu(list, (width, 600.0), cell, text_cell, 0.0).unwrap();
                    let band = menu
                        .status_rect()
                        .expect("pending explanation must be visible");
                    let header = menu.header_rect();
                    assert_eq!(header.1 + header.3, band.1);
                    assert_eq!(band.1 + band.3, menu.rows_rect().1);
                    assert!(band.0 >= 0.0 && band.0 + band.2 <= width);
                    assert!(band.1 > 0.0 && band.1 + band.3 < 600.0);
                    let status = menu.status.as_ref().unwrap();
                    let shown = status.lines.join(" ");
                    assert!(shown.contains(tr.text(T::MenuProfilesPending)));
                    for key in ["Enter", "Tab/↑↓", "Esc"] {
                        assert!(shown.contains(key));
                    }
                    for (index, line) in status.lines.iter().enumerate() {
                        let slot = status.line_rect(band, index);
                        assert!(slot.3 >= text_cell.1);
                        assert!(line.width() as f32 * text_cell.0 <= slot.2);
                        assert!(slot.1 >= band.1 && slot.1 + slot.3 <= band.1 + band.3);
                    }
                    let editor = &mut ws.palette_input.as_mut().unwrap().0;
                    editor.ensure_cursor_visible(8);
                    editor.move_left(true, false);
                    ws.ime_preedit = Some(("composing".into(), Some((2, 5))));
                    let before_projection = project_modal_editor(
                        &ws.palette_input.as_ref().unwrap().0,
                        ws.ime_preedit.as_ref(),
                    );
                    ws.cursor.x = (band.0 + 20.0) as f64;
                    ws.cursor.y = (band.1 + 10.0) as f64;
                    assert!(!rect_contains(
                        header,
                        ws.cursor.x as f32,
                        ws.cursor.y as f32
                    ));
                    let before = ws.palette_input.clone();
                    let preedit = ws.ime_preedit.clone();
                    assert!(!dispatch_modal_press(&mut ws, ModalKind::Palette, true, 1));
                    assert!(!apply_picker_pointer(
                        &mut ws,
                        &cfg,
                        &menu,
                        cell.1,
                        PickerPointer::Press
                    ));
                    assert_eq!(ws.palette_input, before, "status cannot select or edit");
                    assert_eq!(ws.ime_preedit, preedit);
                    assert_eq!(
                        project_modal_editor(
                            &ws.palette_input.as_ref().unwrap().0,
                            ws.ime_preedit.as_ref()
                        ),
                        before_projection,
                        "status press preserves projected composition/caret/selection"
                    );
                    assert_eq!(menu.rows.len(), ranked.len().max(1));
                    if ranked.len() > 8 {
                        menu.scroll_offset = ranked.len() - 4;
                        assert_eq!(menu.status_rect(), Some(band), "status never scrolls");
                    }
                    let commands = palette_entries(
                        None,
                        &tr,
                        None,
                        kettle_config::BroadcastDefault::default(),
                    );
                    for (index, &entry) in ranked.iter().enumerate() {
                        let Some(row) = menu.visible_row_rect(index) else {
                            continue;
                        };
                        assert!(row.1 >= band.1 + band.3);
                        ws.cursor.x = (row.0 + 1.0) as f64;
                        ws.cursor.y = (row.1 + 1.0) as f64;
                        assert!(apply_picker_pointer(
                            &mut ws,
                            &cfg,
                            &menu,
                            cell.1,
                            PickerPointer::Press
                        ));
                        let selected = ws.palette_input.as_ref().unwrap().1;
                        assert_eq!(selected, index);
                        assert_eq!(
                            palette_enter_action(&query, selected, &commands, &tr),
                            Some(commands[entry].1.clone())
                        );
                    }
                    assert_eq!(ws.palette_input.as_ref().unwrap().0.text(), query);
                    let ready = command_picker_list(
                        &query,
                        0,
                        &cfg.keybinds,
                        &tr,
                        None,
                        Some(&[]),
                        false,
                        kettle_config::BroadcastDefault::default(),
                    );
                    let ready =
                        picker_context_menu(ready, (width, 600.0), cell, text_cell, 0.0).unwrap();
                    assert!(ready.status_rect().is_none());
                    let original =
                        picker_card_geometry((width, 600.0), cell, 0.0, ready.rows.len()).unwrap();
                    assert_eq!(ready.header_rect(), original.header);
                    assert_eq!(ready.rows_rect(), original.rows);
                }
            }
        }
    }
}
