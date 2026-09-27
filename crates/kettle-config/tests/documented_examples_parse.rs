//! Every `key = value` line printed in the docs must actually parse.
//!
//! `docs/` is where users copy configuration from, so a line that looks right
//! and silently no-ops is worse than a line that is obviously wrong — nothing
//! tells the user, and the setting simply never takes effect.
//!
//! `#` opens a comment only at the start of a line, so a `#` inside a value is
//! literal (that is what makes `background = #1a1b26` work). So a trailing
//! `value  # explanation` comment becomes part of the value, and
//! `font-size = 20  # bigger` yields 13pt, the default, because "20  # bigger"
//! is not a number.
//!
//! The guard is deliberately mechanical. It extracts every line that looks
//! like a config assignment and feeds it to the real parser, so it catches the
//! next wrong example without anyone having to remember the rule.

use std::path::{Path, PathBuf};

fn docs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
}

/// Whether `line` is config to copy, rather than prose or a deliberate
/// non-example (a `#` comment or `>` quote).
fn is_config_assignment(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with('>') {
        return false;
    }
    // `key = value`, key lowercase-with-dashes, and a non-empty value.
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    let key = key.trim();
    !key.is_empty()
        && !value.trim().is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[test]
fn every_documented_config_line_parses_without_a_malformed_diagnostic() {
    let mut checked = 0usize;
    let mut bad: Vec<String> = Vec::new();

    let mut files: Vec<PathBuf> = std::fs::read_dir(docs_dir())
        .expect("docs/ is readable")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no markdown found under docs/");

    for path in &files {
        let text = std::fs::read_to_string(path).expect("doc is readable");
        for (number, line) in text.lines().enumerate() {
            if !is_config_assignment(line) {
                continue;
            }
            checked += 1;
            let malformed = kettle_config::Config::detect_malformed_values(line);
            if !malformed.is_empty() {
                bad.push(format!(
                    "{}:{}: {line}  ->  {:?}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    number + 1,
                    malformed
                ));
            }
        }
    }

    assert!(
        bad.is_empty(),
        "documented config lines that the parser rejects — a user copying these \
         gets silence, not an error:\n  {}",
        bad.join("\n  ")
    );
    // A guard that scanned nothing would pass forever; pin that it found work.
    assert!(
        checked >= 20,
        "expected to find real config examples in docs/, found only {checked} — \
         the extractor probably stopped matching"
    );
}
