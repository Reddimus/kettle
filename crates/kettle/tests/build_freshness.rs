//! Cargo must cache source identity builds without leaving a stale worker ID.
#![cfg(unix)]

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

struct Build {
    hash: String,
    fresh: HashMap<String, bool>,
}

impl Build {
    #[track_caller]
    fn libraries_are_fresh(&self, expected: bool) {
        for name in ["kettle_update", "kettle_media_worker"] {
            assert_eq!(self.fresh.get(name), Some(&expected), "{name}");
        }
    }
}

#[track_caller]
fn build(root: &Path) -> Build {
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["build", "--offline", "--message-format=json"])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .output()
        .expect("start fixture Cargo");
    assert!(
        output.status.success(),
        "fixture Cargo failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut fresh = HashMap::new();
    let mut hashes = Vec::new();
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        match event["reason"].as_str() {
            Some("compiler-artifact") => {
                fresh.insert(
                    event["target"]["name"].as_str().unwrap().to_string(),
                    event["fresh"].as_bool().unwrap(),
                );
            }
            Some("build-script-executed") => {
                for pair in event["env"].as_array().unwrap() {
                    if pair[0] == "KETTLE_SOURCE_HASH" {
                        hashes.push(pair[1].as_str().unwrap().to_string());
                    }
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        hashes.len(),
        3,
        "all three build scripts must report identity"
    );
    assert!(hashes.iter().all(|hash| hash == &hashes[0]), "{hashes:?}");
    Build {
        hash: hashes.remove(0),
        fresh,
    }
}

#[track_caller]
fn unchanged(root: &Path, hash: &str) {
    let next = build(root);
    next.libraries_are_fresh(true);
    assert_eq!(next.hash, hash);
}

#[track_caller]
fn changed(root: &Path, hash: &mut String) {
    let next = build(root);
    next.libraries_are_fresh(false);
    assert_ne!(&next.hash, hash);
    *hash = next.hash;
}

#[test]
fn source_libraries_rebuild_only_when_inputs_change() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("source");
    let original = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in ["kettle", "kettle-update", "kettle-media-worker"] {
        let directory = root.join("crates").join(name);
        fs::create_dir_all(directory.join("src")).unwrap();
        fs::write(
            directory.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"1.0.0\"\nedition = \"2024\"\n"),
        )
        .unwrap();
        fs::write(directory.join("src/lib.rs"), "pub fn marker() {}\n").unwrap();
        fs::copy(
            original.join("crates").join(name).join("build.rs"),
            directory.join("build.rs"),
        )
        .unwrap();
    }
    fs::create_dir_all(root.join("crates/kettle/build_support")).unwrap();
    fs::copy(
        original.join("crates/kettle/build_support/source_id.rs"),
        root.join("crates/kettle/build_support/source_id.rs"),
    )
    .unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n",
    )
    .unwrap();
    let first = build(&root);
    first.libraries_are_fresh(false);
    let mut hash = first.hash;
    unchanged(&root, &hash);

    fs::write(root.join("README.md"), "not a protocol input\n").unwrap();
    unchanged(&root, &hash);
    fs::write(
        root.join("crates/kettle-update/src/lib.rs"),
        "pub fn edited() {}\n",
    )
    .unwrap();
    changed(&root, &mut hash);
    unchanged(&root, &hash);
    let unused = root.join("crates/kettle-update/src/unused.rs");
    fs::write(&unused, "// new input\n").unwrap();
    changed(&root, &mut hash);
    let renamed = unused.with_file_name("renamed.rs");
    fs::rename(&unused, &renamed).unwrap();
    changed(&root, &mut hash);
    fs::remove_file(&renamed).unwrap();
    changed(&root, &mut hash);
    for name in ["Cargo.toml", "Cargo.lock"] {
        let path = root.join(name);
        let contents = fs::read_to_string(&path).unwrap() + "\n# changed\n";
        fs::write(path, contents).unwrap();
        let next = build(&root);
        next.libraries_are_fresh(false);
        // Cargo may normalize lockfile comments before the build scripts run.
        if name == "Cargo.toml" {
            assert_ne!(next.hash, hash);
        }
        hash = next.hash;
        unchanged(&root, &hash);
    }

    let external = temporary.path().join("external");
    fs::create_dir(&external).unwrap();
    let target = external.join("linked.rs");
    fs::write(&target, "// first\n").unwrap();
    std::os::unix::fs::symlink(
        "../../../../external/linked.rs",
        root.join("crates/kettle-update/src/linked.rs"),
    )
    .unwrap();
    changed(&root, &mut hash);
    unchanged(&root, &hash);
    fs::write(&target, "// second\n").unwrap();
    changed(&root, &mut hash);
    fs::remove_file(&target).unwrap();
    changed(&root, &mut hash);
    fs::write(&target, "// restored\n").unwrap();
    changed(&root, &mut hash);

    let directory = external.join("directory");
    fs::create_dir(&directory).unwrap();
    std::os::unix::fs::symlink(&directory, root.join("crates/kettle-update/src/external")).unwrap();
    // Empty directories do not enter the source hash.
    let linked = build(&root);
    assert_eq!(linked.hash, hash);
    fs::write(directory.join("new.rs"), "// external child\n").unwrap();
    changed(&root, &mut hash);
    unchanged(&root, &hash);
    fs::write(directory.join("new.rs"), "// edited external child\n").unwrap();
    changed(&root, &mut hash);
    unchanged(&root, &hash);
    fs::rename(directory.join("new.rs"), directory.join("renamed.rs")).unwrap();
    changed(&root, &mut hash);
    fs::remove_file(directory.join("renamed.rs")).unwrap();
    changed(&root, &mut hash);
    unchanged(&root, &hash);
    let missing = external.join("missing.rs");
    std::os::unix::fs::symlink(&missing, root.join("crates/kettle-update/src/dangling.rs"))
        .unwrap();
    assert_eq!(build(&root).hash, hash);
    fs::write(&missing, "// appeared\n").unwrap();
    changed(&root, &mut hash);
    unchanged(&root, &hash);
}
