use super::*;
use kettle_config::BroadcastDefault;

const DEFAULTS: [BroadcastDefault; 3] = [
    BroadcastDefault::Group,
    BroadcastDefault::All,
    BroadcastDefault::Off,
];

fn scopes() -> [BroadcastScope; 5] {
    [
        BroadcastScope::Off,
        BroadcastScope::Tab,
        BroadcastScope::All,
        BroadcastScope::Group("fleet".into()),
        BroadcastScope::Group("other".into()),
    ]
}

/// Characterisation: `broadcast-default = off` keeps the chord from turning
/// broadcast on, as CONFIG.md documents. From off it changes nothing; from any
/// other scope it can only turn broadcast off.
#[test]
fn the_chord_never_turns_broadcast_on_under_broadcast_default_off() {
    for current in scopes() {
        for group in [None, Some("fleet")] {
            assert_eq!(
                broadcast_scope_after(
                    &Action::ToggleBroadcastAll,
                    &current,
                    BroadcastDefault::Off,
                    group,
                ),
                Some(BroadcastScope::Off),
                "{current:?} with group {group:?}"
            );
        }
    }
}

/// The chord turns on its `broadcast-default` scope and turns it off again;
/// from any other scope it switches to its own.
#[test]
fn the_chord_toggles_the_broadcast_default_scope() {
    for (default, on) in [
        (BroadcastDefault::Group, BroadcastScope::Tab),
        (BroadcastDefault::All, BroadcastScope::All),
    ] {
        for current in scopes() {
            let expected = if current == on {
                BroadcastScope::Off
            } else {
                on.clone()
            };
            assert_eq!(
                broadcast_scope_after(&Action::ToggleBroadcastAll, &current, default, None),
                Some(expected),
                "{default:?} from {current:?}"
            );
        }
    }
}

#[test]
fn the_other_broadcast_actions_ignore_broadcast_default() {
    for default in DEFAULTS {
        for current in scopes() {
            let after =
                |action: Action, group| broadcast_scope_after(&action, &current, default, group);
            assert_eq!(
                after(Action::ToggleBroadcastOff, None),
                Some(BroadcastScope::Off)
            );
            assert_eq!(
                after(Action::ToggleBroadcastWindow, None),
                Some(if current == BroadcastScope::All {
                    BroadcastScope::Off
                } else {
                    BroadcastScope::All
                })
            );
            assert_eq!(
                after(Action::ToggleBroadcastGroup, Some("fleet")),
                Some(if current == BroadcastScope::Group("fleet".into()) {
                    BroadcastScope::Off
                } else {
                    BroadcastScope::Group("fleet".into())
                })
            );
            // The tab toggle ignores `broadcast-default`, which the chord's
            // action does not.
            assert_eq!(
                after(Action::ToggleBroadcastPanesInTab, None),
                Some(if current == BroadcastScope::Tab {
                    BroadcastScope::Off
                } else {
                    BroadcastScope::Tab
                })
            );
            // No group on the focused pane: nothing to toggle.
            assert_eq!(after(Action::ToggleBroadcastGroup, None), None);
            assert_eq!(after(Action::NewTab, None), None);
        }
    }
}

/// Every `.rs` file of this crate's production code, test files excluded.
fn production_files() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with("tests.rs"))
            {
                out.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut paths = Vec::new();
    walk(&root, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).expect("read source");
            let name = path
                .strip_prefix(&root)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            // A file the guard helpers cannot lex is scanned whole: its
            // tests, comments and strings can then only add matches, so the
            // guard still fails closed.
            let code = std::panic::catch_unwind(|| {
                kettle_test_support::code_only(&kettle_test_support::production_source(&text))
            })
            .unwrap_or_else(|_| text.clone());
            (name, code)
        })
        .collect()
}

/// Byte offsets in `code` where a `broadcast` field is written: assigned,
/// `clone_from`ed, or borrowed mutably (which covers `mem::take`, `replace`
/// and `swap`).
fn broadcast_writes(code: &str) -> Vec<usize> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut writes = Vec::new();
    for (at, _) in code.match_indices(".broadcast") {
        let after = &code[at + ".broadcast".len()..];
        if after.chars().next().is_some_and(ident) {
            continue;
        }
        let rest = after.trim_start();
        let assigned = rest.starts_with('=') && !rest.starts_with("==");
        let cloned_into = rest.starts_with(".clone_from(");
        let receiver_start = code[..at]
            .char_indices()
            .rev()
            .find(|&(_, c)| !(ident(c) || c == '.'))
            .map_or(0, |(i, c)| i + c.len_utf8());
        let borrowed = code[..receiver_start].trim_end().ends_with("&mut");
        if assigned || cloned_into || borrowed {
            writes.push(at);
        }
    }
    writes
}

#[test]
fn the_broadcast_scope_has_one_writer() {
    // The scanner sees every kind of write it claims to.
    for (snippet, writes) in [
        ("ws.mux.broadcast = BroadcastScope::Off;", 1),
        ("ws.mux.broadcast\n    = scope;", 1),
        ("std::mem::take(&mut ws.mux.broadcast);", 1),
        ("let s = &mut target.mux.broadcast;", 1),
        ("ws.mux.broadcast.clone_from(&scope);", 1),
        ("if ws.mux.broadcast == scope {}", 0),
        ("let s = ws.mux.broadcast.clone();", 0),
        ("let s = &ws.mux.broadcast;", 0),
        ("ws.mux.broadcast_target_ids();", 0),
    ] {
        assert_eq!(broadcast_writes(snippet).len(), writes, "{snippet}");
    }

    let mut found = Vec::new();
    for (name, code) in production_files() {
        for at in broadcast_writes(&code) {
            let line = code[..at].matches('\n').count() + 1;
            found.push((name.clone(), line, at, code.clone()));
        }
    }
    let sites: Vec<String> = found
        .iter()
        .map(|(name, line, ..)| format!("{name}:{line}"))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "only App::set_broadcast_scope may write a broadcast scope; found {sites:?}"
    );
    let (name, _, at, code) = &found[0];
    assert_eq!(name, "app/broadcast.rs", "{sites:?}");
    let body = code
        .find("fn set_broadcast_scope(")
        .expect("the helper exists");
    let end = body + code[body..].find("\n    }\n").expect("the helper's end");
    assert!(
        (body..end).contains(at),
        "the one write must be inside set_broadcast_scope"
    );
    let helper = &code[body..end];
    for step in ["request_redraw()", "self.sync_macos_menu(Some(ws))"] {
        assert!(helper.contains(step), "set_broadcast_scope must {step}");
    }
}

/// The group cleanup runs every frame, so the helper must be cheap when the
/// scope is already set, and every reset it makes goes through the helper.
#[test]
fn group_cleanup_goes_through_the_helper_and_is_free_when_unchanged() {
    let source = production_source();
    let helper = source
        .split("fn set_broadcast_scope(")
        .nth(1)
        .expect("helper");
    let first = helper
        .find("if ws.mux.broadcast == scope {")
        .expect("early return");
    assert!(first < helper.find("request_redraw").expect("redraw"));
    // A group toggle with nothing to toggle skips the rest of the dispatch.
    let arm = source
        .split("| Action::ToggleBroadcastPanesInTab => {")
        .nth(1)
        .expect("broadcast arm");
    assert!(arm.trim_start().starts_with(
        "if !self.apply_broadcast_action(ws, &action) {\n                    return;"
    ));
    for owner in ["fn hoover_groups(", "fn hoover_mapped_groups("] {
        let body = source.split(owner).nth(1).expect(owner);
        let body = &body[..body.find("\n    }\n").unwrap_or(body.len())];
        assert!(
            body.contains("self.set_window_broadcast_scope(seq, BroadcastScope::Off)"),
            "{owner} must reset other windows through the helper"
        );
    }
}

/// The palette the window paints and runs lists the broadcast rows
/// `broadcast-default` picks: the chord's action carries the label of the
/// scope it turns on, in both languages, and no scope is listed twice.
#[test]
fn the_palette_labels_the_chord_with_the_scope_it_turns_on() {
    use kettle_i18n::{Language, Text, Translator};
    for tr in [Translator::new(Language::En), Translator::new(Language::Es)] {
        for (default, chord, tab) in [
            (
                BroadcastDefault::Group,
                Some(Text::PaletteToggleBroadcastAll),
                Action::ToggleBroadcastAll,
            ),
            (
                BroadcastDefault::All,
                Some(Text::PaletteToggleBroadcastWindow),
                Action::ToggleBroadcastPanesInTab,
            ),
            (
                BroadcastDefault::Off,
                None,
                Action::ToggleBroadcastPanesInTab,
            ),
        ] {
            let entries = palette_entries(None, &tr, Some(&[]), default);
            let label = |action: &Action| {
                entries
                    .iter()
                    .find(|(_, a)| a == action)
                    .map(|(label, _)| label.shown(&tr).to_owned())
            };
            assert_eq!(
                label(&Action::ToggleBroadcastAll),
                chord.map(|text| tr.text(text).to_owned()),
                "{default:?}"
            );
            assert_eq!(
                label(&tab),
                Some(tr.text(Text::PaletteToggleBroadcastAll).to_owned()),
                "{default:?}"
            );
            let shown: Vec<_> = entries.iter().map(|(label, _)| label.shown(&tr)).collect();
            for text in [
                Text::PaletteToggleBroadcastAll,
                Text::PaletteToggleBroadcastWindow,
            ] {
                assert_eq!(
                    shown.iter().filter(|s| **s == tr.text(text)).count(),
                    1,
                    "{default:?} {text:?}"
                );
            }
        }
    }
}

/// The scope a Broadcast Input row turns on when it is not checked.
fn row_scope(kind: BroadcastRowKind, focused_group: Option<&str>) -> BroadcastScope {
    match kind {
        BroadcastRowKind::Off => BroadcastScope::Off,
        BroadcastRowKind::Tab => BroadcastScope::Tab,
        BroadcastRowKind::Window => BroadcastScope::All,
        BroadcastRowKind::Group => {
            BroadcastScope::Group(focused_group.expect("an enabled group row").into())
        }
    }
}

/// Every `broadcast-default`, scope and focused-pane group: exactly one row is
/// checked, the one for the scope; choosing it, through the dispatch's own
/// transition, turns broadcast off; choosing any other available row turns its
/// scope on; only the Group row is ever unavailable, and only while unchecked
/// with no group on the focused pane; and the chord's action sits on Tab
/// under `group`, Window under `all` and no row under `off`.
#[test]
fn every_broadcast_row_does_what_its_checkmark_says() {
    use BroadcastKind as K;
    use BroadcastRowKind as R;
    let fleet = || BroadcastScope::Group("fleet".into());
    for default in DEFAULTS {
        for (kind, scope, focused_group) in [
            (K::Off, BroadcastScope::Off, None),
            (K::Off, BroadcastScope::Off, Some("fleet")),
            (K::Tab, BroadcastScope::Tab, None),
            (K::Tab, BroadcastScope::Tab, Some("fleet")),
            (K::Window, BroadcastScope::All, None),
            (K::Window, BroadcastScope::All, Some("fleet")),
            (K::FocusedGroup, fleet(), Some("fleet")),
            (K::OtherGroup, fleet(), Some("other")),
            (K::OtherGroup, fleet(), None),
        ] {
            let case = format!("{default:?} {scope:?} focused in {focused_group:?}");
            assert_eq!(BroadcastKind::of(&scope, focused_group), kind, "{case}");
            let rows = broadcast_rows(default, kind, focused_group.is_some());
            assert_eq!(
                rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
                [R::Off, R::Tab, R::Window, R::Group],
                "{case}"
            );
            let checked: Vec<_> = rows
                .iter()
                .filter(|row| row.checked)
                .map(|row| row.kind)
                .collect();
            let expected = match kind {
                K::Off => R::Off,
                K::Tab => R::Tab,
                K::Window => R::Window,
                K::FocusedGroup | K::OtherGroup => R::Group,
            };
            assert_eq!(checked, [expected], "{case}");
            for row in &rows {
                let after = broadcast_scope_after(&row.action, &scope, default, focused_group);
                if row.checked {
                    assert!(row.enabled, "{case}: the checked {:?} row", row.kind);
                    assert_eq!(after, Some(BroadcastScope::Off), "{case}: {row:?}");
                } else if row.enabled {
                    assert_eq!(
                        after,
                        Some(row_scope(row.kind, focused_group)),
                        "{case}: {row:?}"
                    );
                } else {
                    assert_eq!(row.kind, R::Group, "{case}");
                    assert_eq!(focused_group, None, "{case}");
                }
            }
            let chord = rows
                .iter()
                .filter(|row| row.action == Action::ToggleBroadcastAll)
                .map(|row| row.kind)
                .collect::<Vec<_>>();
            let chord_row = match default {
                BroadcastDefault::Group => vec![R::Tab],
                BroadcastDefault::All => vec![R::Window],
                BroadcastDefault::Off => vec![],
            };
            assert_eq!(chord, chord_row, "{case}");
        }
    }
}

/// The menus read the focused pane, the one the group toggle reads.
#[test]
fn the_menu_snapshot_reads_the_focused_panes_group() {
    let (mux, focused, other) = crate::app::tests::title_test_split_mux();
    let mut ws = WindowState::new(1, mux);
    assert_eq!(ws.mux.active_focus(), Some(focused));
    assert_eq!(BroadcastKind::of_window(&ws), (BroadcastKind::Off, false));
    ws.mux.panes.get_mut(&other).unwrap().group_name = Some("fleet".into());
    ws.mux.broadcast = BroadcastScope::Group("fleet".into());
    assert_eq!(
        BroadcastKind::of_window(&ws),
        (BroadcastKind::OtherGroup, false)
    );
    ws.mux.panes.get_mut(&focused).unwrap().group_name = Some("ops".into());
    assert_eq!(
        BroadcastKind::of_window(&ws),
        (BroadcastKind::OtherGroup, true)
    );
    ws.mux.panes.get_mut(&focused).unwrap().group_name = Some("fleet".into());
    assert_eq!(
        BroadcastKind::of_window(&ws),
        (BroadcastKind::FocusedGroup, true)
    );
    ws.mux.broadcast = BroadcastScope::Tab;
    assert_eq!(BroadcastKind::of_window(&ws), (BroadcastKind::Tab, true));

    // The menu bar's snapshot is that reading plus `broadcast-default`.
    let source = production_source();
    let sync = source
        .split("fn sync_macos_menu(")
        .nth(1)
        .expect("sync_macos_menu");
    let sync = &sync[..sync.find("\n    }\n").unwrap_or(sync.len())];
    for needle in [
        "ws.map(BroadcastKind::of_window)",
        "broadcast: broadcast.0",
        "pane_group: broadcast.1",
        "broadcast_default: self.cfg.broadcast_default",
    ] {
        assert!(sync.contains(needle), "{needle}");
    }
}

/// The right-click menu offers the same rows as radio rows in a Broadcast
/// Input submenu, in English and Spanish: the label carries the radio marker
/// it paints, so the measured text is the painted text, and an unavailable
/// Group row is hidden while the checked row always stays.
#[test]
fn the_right_click_menu_has_the_broadcast_submenu() {
    use kettle_i18n::{Language, Text, Translator};
    use kettle_render::MenuToggle;
    let source = production_source();
    let body = source
        .split("fn context_menu_items(")
        .nth(1)
        .expect("context_menu_items");
    let body = &body[..body.find("\n    }\n").unwrap_or(body.len())];
    assert!(body.contains("BroadcastKind::of_window(ws)"));
    assert!(body.contains("broadcast_rows(self.cfg.broadcast_default, broadcast, pane_group)"));
    for tr in [Translator::new(Language::En), Translator::new(Language::Es)] {
        for (kind, pane_group, shown) in [
            (BroadcastKind::Off, false, 3),
            (BroadcastKind::Off, true, 4),
            (BroadcastKind::OtherGroup, false, 4),
            (BroadcastKind::Tab, false, 3),
        ] {
            let rows = broadcast_rows(BroadcastDefault::Group, kind, pane_group);
            let mut items = vec![ContextMenuItem::Item {
                label: tr.text(Text::MenuCopy),
                action: Action::Copy,
                enabled: true,
            }];
            items.extend(rows.iter().map(|row| broadcast_menu_item(row, &tr)));
            let grouped = grouped_terminal_menu(items, tr);
            let inner = grouped
                .iter()
                .find_map(|item| match item {
                    ContextMenuItem::Submenu { label, items }
                        if label == tr.text(Text::MenuBroadcastInput) =>
                    {
                        Some(items)
                    }
                    _ => None,
                })
                .expect("a Broadcast Input submenu");
            assert_eq!(inner.len(), shown, "{kind:?} {pane_group}");
            for (item, row) in inner.iter().zip(rows.iter().filter(|row| row.enabled)) {
                let ContextMenuItem::DynamicItem {
                    label,
                    action,
                    enabled,
                    toggle,
                } = item
                else {
                    panic!("a broadcast row is a radio row");
                };
                assert_eq!(*toggle, Some(MenuToggle::Radio(row.checked)));
                assert_eq!(
                    *label,
                    format!(
                        "{}{}",
                        MenuToggle::Radio(row.checked).marker(),
                        tr.text(row.kind.text())
                    )
                );
                assert_eq!(*action, row.action);
                assert!(*enabled);
            }
            assert_eq!(
                inner
                    .iter()
                    .filter(|item| matches!(
                        item,
                        ContextMenuItem::DynamicItem {
                            toggle: Some(MenuToggle::Radio(true)),
                            ..
                        }
                    ))
                    .count(),
                1
            );
        }
    }
}
