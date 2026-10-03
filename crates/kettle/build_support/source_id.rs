//! The source identity shared by Kettle's build scripts: a hash of the Rust
//! sources a binary is built from. It is read without git, so it is the same
//! in a checkout, a tarball and a Nix sandbox, staged or not. Two builds of
//! different sources differ; a rebuild or reinstall of the same source does
//! not. This file is under `crates/`, so it hashes itself too.

use std::path::{Path, PathBuf};

/// A hash of the workspace's Rust sources: every file under `crates/`
/// (through symlinks), and the workspace `Cargo.toml` and `Cargo.lock`, by
/// path and contents, in path order. About 9 MB, read in a few tens of
/// milliseconds.
pub fn source_hash(repo_root: &Path) -> u64 {
    use std::hash::{DefaultHasher, Hash as _, Hasher as _};
    // Symlinks are followed, so a linked source counts by what it holds;
    // the depth bound stops a link loop.
    fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match std::fs::metadata(&path) {
                Ok(kind) if kind.is_dir() && depth < 32 => collect(&path, depth + 1, out),
                Ok(kind) if kind.is_file() => out.push(path),
                _ => {}
            }
        }
    }
    let mut files = Vec::new();
    collect(&repo_root.join("crates"), 0, &mut files);
    files.sort();
    files.push(repo_root.join("Cargo.toml"));
    files.push(repo_root.join("Cargo.lock"));
    let mut hasher = DefaultHasher::new();
    for file in &files {
        if let Ok(bytes) = std::fs::read(file) {
            file.strip_prefix(repo_root)
                .unwrap_or(file)
                .hash(&mut hasher);
            bytes.hash(&mut hasher);
        }
    }
    hasher.finish()
}
