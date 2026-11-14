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

/// The index of the `)` that closes the `(` at `open`, or the end of `code`.
fn closing_parenthesis(code: &str, open: usize) -> usize {
    let mut depth = 0usize;
    for (offset, byte) in code.bytes().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return offset;
                }
            }
            _ => {}
        }
    }
    code.len()
}

/// Where each `Command::new` that may start `kettle` begins in `code`, the
/// output of `code_only`: every one whose program is not a lone string
/// literal, which `code_only` has blanked to spaces.
fn launches(code: &str) -> Vec<usize> {
    code.match_indices("Command::new(")
        .filter(|(at, call)| {
            let open = at + call.len() - 1;
            !code[open + 1..closing_parenthesis(code, open)]
                .trim()
                .is_empty()
        })
        .map(|(at, _)| at)
        .collect()
}

/// Why `raw`, a test file, starts a `kettle` that is not isolated, if it
/// does. Each launch must be followed by its own `isolate_ctl_registry`,
/// after any `env_clear`, before the next launch and before its function
/// ends: one isolated launch does not cover another in the same function.
fn unisolated(raw: &str) -> Option<String> {
    let reaches = ["\"mcp\"", "\"ctl\"", "\"show\"", "\"agent-setup\""];
    if !raw.contains("CARGO_BIN_EXE_kettle\"") || !reaches.iter().any(|arg| raw.contains(arg)) {
        return None;
    }
    let code = kettle_test_support::code_only(raw);
    let starts = function_starts(&code);
    let launches = launches(&code);
    for (index, &at) in launches.iter().enumerate() {
        let function_end = starts.iter().copied().find(|&start| start > at).unwrap();
        let end = launches
            .get(index + 1)
            .copied()
            .filter(|&next| next < function_end)
            .unwrap_or(function_end);
        let own = &code[at..end];
        let line = code[..at].matches('\n').count() + 1;
        let Some(isolated) = own.rfind("isolate_ctl_registry(") else {
            return Some(format!("line {line} starts a kettle that is not isolated"));
        };
        if own
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
    const KETTLE: &str = "Command::new(env!(\"CARGO_BIN_EXE_kettle\"))";
    const ISOLATE: &str = "isolate_ctl_registry(&mut c, r);";
    let bare = format!("fn t() {{\n    let mut c = {KETTLE};\n    c.arg(\"mcp\");\n}}\n");
    assert_eq!(
        unisolated(&bare).as_deref(),
        Some("line 2 starts a kettle that is not isolated")
    );
    let other_fn = format!(
        "fn t() {{\n    let c = {KETTLE}.arg(\"mcp\");\n}}\nfn u(c: &mut Command) {{\n    isolate_ctl_registry(c, r);\n}}\n"
    );
    assert!(
        unisolated(&other_fn).is_some(),
        "isolation in another function"
    );
    let cleared = format!(
        "fn t() {{\n    let mut c = {KETTLE};\n    {ISOLATE}\n    c.env_clear().arg(\"ctl\");\n}}\n"
    );
    assert!(unisolated(&cleared).is_some(), "env_clear after isolating");
    let isolated = format!(
        "fn t() {{\n    let ps = Command::new(\"ps\");\n    let mut c = {KETTLE};\n    c.env_clear().arg(\"ctl\");\n    {ISOLATE}\n    let sh = Command::new(\"/bin/sh\");\n}}\n"
    );
    assert_eq!(unisolated(&isolated), None);
    let unrelated = format!("fn t() {{\n    {KETTLE}.args([\"exec\"]);\n}}\n");
    assert_eq!(unisolated(&unrelated), None, "exec reaches no registry");
    // Each launch needs its own isolation, whichever of two has one.
    let first = format!("    let mut c = {KETTLE};\n    c.arg(\"mcp\");\n");
    let second = format!("    let mut c = {KETTLE};\n    c.arg(\"ctl\");\n");
    let two = |after_first: &str, after_second: &str| {
        format!("fn t() {{\n{first}{after_first}{second}{after_second}}}\n")
    };
    let isolate = format!("    {ISOLATE}\n");
    assert_eq!(
        unisolated(&two("", &isolate)).as_deref(),
        Some("line 2 starts a kettle that is not isolated"),
        "only the second of two launches is isolated"
    );
    assert_eq!(
        unisolated(&two(&isolate, "")).as_deref(),
        Some("line 5 starts a kettle that is not isolated"),
        "only the first of two launches is isolated"
    );
    assert_eq!(unisolated(&two(&isolate, &isolate)), None);
}
