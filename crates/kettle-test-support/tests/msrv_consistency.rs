//! The workspace's Rust floor is declared once, in `Cargo.toml`. CI, Nix and
//! the build docs must agree with it, and the MSRV job must still run the
//! build and tests on exactly that toolchain.

use std::path::Path;

/// The `msrv` job's non-comment lines, trimmed, or nothing if it is missing.
fn msrv_job(ci: &str) -> Vec<&str> {
    ci.lines()
        .skip_while(|line| *line != "  msrv:")
        .skip(1)
        .take_while(|line| {
            line.trim().is_empty() || line.trim_start().starts_with('#') || line.starts_with("    ")
        })
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// Whether the `msrv` job names, installs and caches exactly `floor`, and
/// still runs the locked workspace build and tests.
fn msrv_job_matches(ci: &str, floor: &str) -> bool {
    let job = msrv_job(ci);
    let values = |key: &str| {
        job.iter()
            .filter_map(|line| line.strip_prefix(key))
            .map(|value| value.trim_matches('"'))
            .collect::<Vec<_>>()
    };
    values("name: ") == [format!("msrv (rust {floor})").as_str()]
        && values("toolchain: ") == [floor]
        && values("key: ") == [format!("msrv-{floor}").as_str()]
        && values("- run: ")
            == [
                "cargo build --locked --workspace --all-targets",
                "cargo test --locked --workspace",
            ]
}

/// Every `1.NN` version that follows "MSRV" or "Rust" within a few
/// characters, so a document cannot keep a stale floor beside the new one.
fn floors_mentioned(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for marker in ["MSRV", "Rust "] {
        for (at, _) in text.match_indices(marker) {
            let window: String = text[at + marker.len()..].chars().take(16).collect();
            if let Some(start) = window.find("1.") {
                let digits: String = window[start + 2..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                if !digits.is_empty() {
                    found.push(format!("1.{digits}"));
                }
            }
        }
    }
    found
}

fn read(root: &Path, relative: &str) -> String {
    std::fs::read_to_string(root.join(relative))
        .unwrap_or_else(|error| panic!("cannot read {relative}: {error}"))
}

#[test]
fn workspace_floor_agrees_with_ci_nix_and_current_build_docs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest: toml::Value = toml::from_str(&read(&root, "Cargo.toml")).unwrap();
    let floor = manifest["workspace"]["package"]["rust-version"]
        .as_str()
        .unwrap();
    assert!(
        msrv_job_matches(&read(&root, ".github/workflows/ci.yml"), floor),
        "the msrv CI job must name, install, cache and test Rust {floor}"
    );
    let nix = read(&root, "flake.nix");
    assert!(nix.contains(
        "(builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.\"rust-version\""
    ));
    assert!(nix.contains("rustToolchain = pkgs.rust-bin.stable.\"${workspaceMsrv}.0\".default;"));
    for (path, expected) in [
        ("README.md", format!("MSRV-{floor}-blue")),
        ("README.md", format!("requires Rust {floor} or newer")),
        ("CONTRIBUTING.md", format!("MSRV (Rust {floor})")),
        (
            "packaging/nix/README.md",
            format!("workspace MSRV ({floor})"),
        ),
        (
            "docs/INSTALL.md",
            format!("Requires Rust \u{2265} {floor} (the workspace MSRV)."),
        ),
    ] {
        let text = read(&root, path);
        assert!(text.contains(&expected), "{path} lost {expected}");
    }
    // No current-build document may keep another floor beside this one.
    for path in [
        "README.md",
        "CONTRIBUTING.md",
        "packaging/nix/README.md",
        "docs/INSTALL.md",
        "flake.nix",
        ".github/workflows/ci.yml",
    ] {
        for mentioned in floors_mentioned(&read(&root, path)) {
            assert_eq!(mentioned, floor, "{path} names Rust {mentioned} as a floor");
        }
    }
    for member in manifest["workspace"]["members"].as_array().unwrap() {
        let path = format!("{}/Cargo.toml", member.as_str().unwrap());
        let package: toml::Value = toml::from_str(&read(&root, &path)).unwrap();
        assert_eq!(
            package["package"]["rust-version"]["workspace"].as_bool(),
            Some(true),
            "{path} must inherit the workspace floor"
        );
    }
}

#[test]
fn stale_toolchain_check_name_cache_key_or_commands_cannot_pass_the_drift_guard() {
    let original = "jobs:\n  msrv:\n    name: msrv (rust 1.95)\n    steps:\n      - with:\n          toolchain: \"1.95\"\n          key: msrv-1.95\n      - run: cargo build --locked --workspace --all-targets\n      - run: cargo test --locked --workspace\n  build:\n    name: other\n";
    assert!(msrv_job_matches(original, "1.95"));
    for (current, stale) in [
        ("toolchain: \"1.95\"", "toolchain: \"1.89\""),
        ("name: msrv (rust 1.95)", "name: msrv (rust 1.89)"),
        ("key: msrv-1.95", "key: msrv-1.89"),
        ("  msrv:", "  removed-msrv:"),
        ("--all-targets", ""),
        (
            "cargo test --locked --workspace",
            "cargo test --locked -p kettle-vt",
        ),
        ("      - run: cargo test --locked --workspace\n", ""),
    ] {
        assert!(
            !msrv_job_matches(&original.replace(current, stale), "1.95"),
            "accepted stale {current}"
        );
    }
    let misplaced = "jobs:\n  msrv:\n    name: incomplete\n  build:\n    name: msrv (rust 1.95)\n    toolchain: \"1.95\"\n    key: msrv-1.95\n";
    assert!(!msrv_job_matches(misplaced, "1.95"));
}

#[test]
fn a_document_keeping_an_old_floor_is_caught() {
    assert_eq!(floors_mentioned("MSRV (Rust 1.95)"), ["1.95", "1.95"]);
    assert_eq!(
        floors_mentioned("requires Rust 1.95 or newer; see MSRV-1.89-blue"),
        ["1.89", "1.95"]
    );
    assert!(floors_mentioned("Rust is great; no version here").is_empty());
}
