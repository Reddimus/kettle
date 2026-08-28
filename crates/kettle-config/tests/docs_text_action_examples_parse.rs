//! Every `text:` binding printed in the docs must parse to the payload it claims.
//!
//! `#` opens a comment only at the START of a line, so a `keybind` example
//! written with a trailing annotation silently absorbs it: the documented
//! `keybind = cmd+backspace = text:\x15   # ^U — delete to start of line`
//! parsed as `Send text "\x15   # ^U — delete to start of line"`, and a reader
//! copying it would have sent that comment to their shell. The examples are
//! copy-paste material, so they are held to the parser rather than to review.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Pull every uncommented `keybind = … = text:…` line out of a docs file.
fn documented_text_bindings(source: &str) -> Vec<String> {
    source
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter(|line| line.starts_with("keybind") && line.contains("text:"))
        .map(str::to_string)
        .collect()
}

#[test]
fn documented_text_bindings_carry_only_their_payload() {
    let mut checked = 0usize;
    for relative in ["docs/CONFIG.md", "docs/kettle.example.config", "README.md"] {
        let path = repo_root().join(relative);
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in documented_text_bindings(&source) {
            let (_, value) = line.split_once('=').expect("a keybind line has a value");
            let (trigger, action) = value
                .rsplit_once('=')
                .unwrap_or_else(|| panic!("{relative}: not a trigger=action line: {line}"));
            assert!(
                kettle_config::keybinds::parse_trigger(trigger.trim()).is_some(),
                "{relative}: trigger does not parse: {line}"
            );
            let action = action.trim();
            let parsed = kettle_config::keybinds::Action::from_name(action)
                .unwrap_or_else(|| panic!("{relative}: action does not parse: {line}"));
            let kettle_config::keybinds::Action::SendText(payload) = parsed else {
                panic!("{relative}: expected a text: action: {line}");
            };
            // The whole point: a payload that swallowed a trailing comment is
            // long, contains `#`, or contains whitespace runs that no control
            // sequence needs.
            assert!(
                !payload.contains('#'),
                "{relative}: the payload absorbed a trailing comment — `#` only \
                 opens a comment at the start of a line: {line}"
            );
            assert!(
                payload.len() <= 8,
                "{relative}: documented payload is suspiciously long ({:?}); a \
                 trailing annotation belongs on its own line: {line}",
                payload
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 3,
        "expected the documented cmd+backspace / cmd+left / cmd+right examples \
         to be found; got {checked}"
    );
}
