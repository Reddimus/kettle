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
    for owner in ["fn hoover_groups(", "fn hoover_mapped_groups("] {
        let body = source.split(owner).nth(1).expect(owner);
        let body = &body[..body.find("\n    }\n").unwrap_or(body.len())];
        assert!(
            body.contains("self.set_window_broadcast_scope(seq, BroadcastScope::Off)"),
            "{owner} must reset other windows through the helper"
        );
    }
}
