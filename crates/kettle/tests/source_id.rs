//! The source hash every Kettle binary embeds (`KETTLE_SOURCE_ID` and
//! `KETTLE_SOURCE_HASH`). The GUI and its workers must agree on it, so it has
//! to be the same wherever one source is built and differ for any other.

#[path = "../build_support/source_id.rs"]
mod source_id;

use std::fs;
use std::path::Path;

use source_id::source_hash;

/// A small workspace: two crates, a manifest and a lock file.
fn workspace(root: &Path) {
    for (path, text) in [
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
        ("Cargo.lock", "version = 4\n"),
        ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
        ("crates/a/src/lib.rs", "pub fn a() {}\n"),
        ("crates/b/src/main.rs", "fn main() {}\n"),
        ("crates/b/build_support/shared.rs", "pub fn shared() {}\n"),
    ] {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

fn edit(root: &Path, path: &str, text: &str) {
    fs::write(root.join(path), text).unwrap();
}

#[test]
fn source_hash_matches_in_checkout_and_source_export() {
    let checkout = tempfile::tempdir().unwrap();
    let export = tempfile::tempdir().unwrap();
    workspace(checkout.path());
    workspace(export.path());
    // What a checkout has and an exported source tree does not: git's own
    // files, build output and files outside the sources.
    for path in [".git/HEAD", "target/debug/kettle", "README.md", "docs/a.md"] {
        let path = checkout.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "not source").unwrap();
    }
    assert_eq!(source_hash(checkout.path()), source_hash(export.path()));
}

#[test]
fn source_edit_changes_both_build_ids() {
    // Every binary takes its identity from this one function, so a source
    // edit changes the GUI's and each worker's alike.
    let root = tempfile::tempdir().unwrap();
    workspace(root.path());
    let before = source_hash(root.path());
    for (path, text) in [
        ("crates/a/src/lib.rs", "pub fn a() { }\n"),
        ("crates/b/build_support/shared.rs", "pub fn shared() { }\n"),
        ("Cargo.lock", "version = 4\n\n"),
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n\n"),
    ] {
        let original = fs::read_to_string(root.path().join(path)).unwrap();
        edit(root.path(), path, text);
        assert_ne!(source_hash(root.path()), before, "{path}");
        edit(root.path(), path, &original);
    }
    // A renamed source changes it too, even with the same contents.
    fs::rename(
        root.path().join("crates/b/src/main.rs"),
        root.path().join("crates/b/src/bin.rs"),
    )
    .unwrap();
    assert_ne!(source_hash(root.path()), before);
}

#[test]
fn rebuild_preserves_build_id() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    workspace(first.path());
    let id = source_hash(first.path());
    assert_eq!(source_hash(first.path()), id);
    // Rewriting a file with the same bytes, or building the same source in
    // another directory, keeps it.
    edit(first.path(), "crates/a/src/lib.rs", "pub fn a() {}\n");
    assert_eq!(source_hash(first.path()), id);
    workspace(second.path());
    assert_eq!(source_hash(second.path()), id);
    // Files outside the sources do not count.
    edit(first.path(), "README.md", "notes");
    assert_eq!(source_hash(first.path()), id);
}

#[cfg(unix)]
#[test]
fn a_linked_source_counts_by_its_contents() {
    let linked = tempfile::tempdir().unwrap();
    let copied = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    workspace(linked.path());
    workspace(copied.path());
    let target = elsewhere.path().join("lib.rs");
    fs::write(&target, "pub fn a() {}\n").unwrap();
    fs::remove_file(linked.path().join("crates/a/src/lib.rs")).unwrap();
    std::os::unix::fs::symlink(&target, linked.path().join("crates/a/src/lib.rs")).unwrap();
    assert_eq!(source_hash(linked.path()), source_hash(copied.path()));
    fs::write(&target, "pub fn a() { }\n").unwrap();
    assert_ne!(source_hash(linked.path()), source_hash(copied.path()));
}

#[test]
fn this_build_embeds_the_hash_it_reports() {
    let hash = env!("KETTLE_SOURCE_HASH");
    assert_eq!(hash.len(), 16);
    assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(
        env!("KETTLE_SOURCE_ID"),
        format!("{} ({hash})", env!("CARGO_PKG_VERSION"))
    );
}
