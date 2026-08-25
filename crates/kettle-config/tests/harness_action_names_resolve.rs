//! Every action name the verification scripts dispatch must still resolve.
//!
//! The live-UI harnesses drive kettle through `perform_action`, which takes an
//! action by name. A name that no longer resolves fails at run time with
//! `unknown action`, and these scenarios are manual rather than gated, so the
//! break is invisible until someone runs them months later and has to work out
//! whether their own change caused it.
//!
//! This existed as a real defect. `scripts/check-live-ui-smoke.py` dispatched
//! `toggle_broadcast_all`, which was never a valid name, and `broadcast_all`
//! was later deliberately re-pointed from tab scope to window scope. The
//! split-titlebar scenario had been dead for long enough that three other
//! defects had piled up behind it.
//!
//! Mechanical on purpose: pull every literal the scripts hand to
//! `perform_action` and feed it to the same parser the control plane uses.
//!
//! `ctl_perform_action` also answers a few CONTROL-ONLY names itself, before
//! it consults the keybind catalogue, and those resolve through no `Action`.
//! `focus_window` is one, and it is the only activation step that works on
//! Linux and Windows. They are read out of the production source rather than
//! listed here, so adding another cannot make this guard reject it.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn scripts_dir() -> PathBuf {
    repo_root().join("scripts")
}

/// Names `ctl_perform_action` answers itself, before the keybind catalogue.
///
/// Read out of the production source so a new one does not have to be repeated
/// here. Extraction stops at the `Action::from_name` call, which is where the
/// control-only prefix of that function ends.
fn control_only_action_names() -> Vec<String> {
    let source = std::fs::read_to_string(
        repo_root()
            .join("crates")
            .join("kettle-ui")
            .join("src")
            .join("app.rs"),
    )
    .expect("kettle-ui/src/app.rs is readable");
    let body = source
        .split("fn ctl_perform_action(")
        .nth(1)
        .and_then(|rest| rest.split("Action::from_name(name)").next())
        .expect("ctl_perform_action's control-only prefix");
    let mut names = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find("name == \"") {
        rest = &rest[at + "name == \"".len()..];
        if let Some(close) = rest.find('"') {
            names.push(rest[..close].to_string());
        }
    }
    assert!(
        !names.is_empty(),
        "extraction found no control-only action names; `ctl_perform_action` \
         changed shape and this guard was about to reject a valid name"
    );
    names
}

/// Action names a script hands to `perform_action`, in the two shapes the
/// harnesses use: `{"action": "name"}` from Python and `--text name` from
/// shell.
fn dispatched_action_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (marker, quoted) in [("\"action\":", true), ("perform_action --text", false)] {
        let mut rest = source;
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            let tail = rest.trim_start();
            let name = if quoted {
                let Some(open) = tail.strip_prefix('"') else {
                    continue;
                };
                match open.find('"') {
                    Some(close) => &open[..close],
                    None => continue,
                }
            } else {
                tail.split_whitespace().next().unwrap_or("")
            };
            // Skip interpolated names; only literals can be checked here.
            if name.is_empty()
                || name.contains('{')
                || name.contains('$')
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ':')
            {
                continue;
            }
            names.push(name.to_string());
        }
    }
    names
}

#[test]
fn every_action_name_the_scripts_dispatch_still_resolves() {
    let dir = scripts_dir();
    let control_only = control_only_action_names();
    let mut checked = 0usize;
    let mut bad: Vec<String> = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(ext, "py" | "sh") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&p) else {
                continue;
            };
            for name in dispatched_action_names(&source) {
                checked += 1;
                if !control_only.contains(&name)
                    && kettle_config::keybinds::Action::from_name(&name).is_none()
                {
                    bad.push(format!("{}: unknown action {name:?}", p.display()));
                }
            }
        }
    }
    assert!(
        checked >= 10,
        "expected the harnesses to dispatch actions by name; found {checked}, \
         so this guard stopped reading the scripts and was about to pass \
         against anything"
    );
    assert!(
        bad.is_empty(),
        "action names dispatched by the verification scripts no longer resolve:\n{}",
        bad.join("\n")
    );
}
