//! Drift guard: a workspace test that runs `kettle` as something that reaches
//! the control registry (`mcp`, `ctl`, `show`, `agent-setup`) keeps every
//! `kettle` it starts away from the account's own registry, through
//! `isolate_ctl_registry`. Without it the child reads, prunes and publishes
//! where the OS puts this account's registry, and can find the Kettle the
//! developer runs the tests in.

use std::path::{Path, PathBuf};

/// Where each function in `code` starts, then where `code` ends.
fn function_starts(code: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in code.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let trimmed = trimmed
            .strip_prefix("pub(crate) ")
            .or_else(|| trimmed.strip_prefix("pub "))
            .unwrap_or(trimmed);
        if trimmed.starts_with("fn ") {
            starts.push(offset);
        }
        offset += line.len();
    }
    starts.push(code.len());
    starts
}

/// Why `raw`, a test file, starts a `kettle` that is not isolated, if it
/// does: every `Command::new` with a program that is not a string literal
/// must be followed, in its own function and after any `env_clear`, by
/// `isolate_ctl_registry`.
fn unisolated(raw: &str) -> Option<String> {
    let reaches = ["\"mcp\"", "\"ctl\"", "\"show\"", "\"agent-setup\""];
    if !raw.contains("CARGO_BIN_EXE_kettle\"") || !reaches.iter().any(|arg| raw.contains(arg)) {
        return None;
    }
    let code = kettle_test_support::code_only(raw);
    let starts = function_starts(&code);
    for (at, call) in code.match_indices("Command::new(") {
        if raw[at + call.len()..].starts_with('"') {
            continue;
        }
        let end = starts.iter().copied().find(|&start| start > at).unwrap();
        let body = &code[at..end];
        let line = raw[..at].lines().count() + 1;
        let Some(isolated) = body.rfind("isolate_ctl_registry(") else {
            return Some(format!("line {line} starts a kettle that is not isolated"));
        };
        if body
            .rfind("env_clear()")
            .is_some_and(|clear| clear > isolated)
        {
            return Some(format!(
                "line {line} clears the environment after isolating its kettle"
            ));
        }
    }
    None
}

fn workspace_test_files() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for member in std::fs::read_dir(crates).unwrap().flatten() {
        let Ok(tests) = std::fs::read_dir(member.path().join("tests")) else {
            continue;
        };
        for file in tests.flatten() {
            if file.path().extension().is_some_and(|ext| ext == "rs") {
                files.push(file.path());
            }
        }
    }
    files
}

#[test]
fn tests_isolate_every_kettle_that_reaches_the_control_registry() {
    let mut checked = 0;
    for path in workspace_test_files() {
        let raw = std::fs::read_to_string(&path).unwrap();
        if raw.contains("CARGO_BIN_EXE_kettle\"") {
            checked += 1;
        }
        if let Some(problem) = unisolated(&raw) {
            panic!("{}: {problem}", path.display());
        }
    }
    assert!(
        checked >= 3,
        "found only {checked} test files that start kettle"
    );
}

/// The guard itself: what it must refuse and what it must let through.
#[test]
fn the_isolation_guard_refuses_a_bare_kettle() {
    let bare = "fn t() {\n    let mut c = Command::new(env!(\"CARGO_BIN_EXE_kettle\"));\n    c.arg(\"mcp\");\n}\n";
    assert!(unisolated(bare).is_some());
    let other_fn = "fn t() {\n    let c = Command::new(env!(\"CARGO_BIN_EXE_kettle\")).arg(\"mcp\");\n}\nfn u(c: &mut Command) {\n    isolate_ctl_registry(c, r);\n}\n";
    assert!(
        unisolated(other_fn).is_some(),
        "isolation in another function"
    );
    let cleared = "fn t() {\n    let mut c = Command::new(env!(\"CARGO_BIN_EXE_kettle\"));\n    isolate_ctl_registry(&mut c, r);\n    c.env_clear().arg(\"ctl\");\n}\n";
    assert!(unisolated(cleared).is_some(), "env_clear after isolating");
    let isolated = "fn t() {\n    let mut c = Command::new(env!(\"CARGO_BIN_EXE_kettle\"));\n    c.env_clear().arg(\"ctl\");\n    isolate_ctl_registry(&mut c, r);\n    let sh = Command::new(\"/bin/sh\");\n}\n";
    assert_eq!(unisolated(isolated), None);
    let unrelated =
        "fn t() {\n    Command::new(env!(\"CARGO_BIN_EXE_kettle\")).args([\"exec\"]);\n}\n";
    assert_eq!(unisolated(unrelated), None, "exec reaches no registry");
}
