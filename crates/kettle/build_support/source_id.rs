//! Source identity shared by the GUI and workers, independent of Git metadata.
//! Hashes logical paths and contents, including linked sources, in path order.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Path, PathBuf};

struct Inputs {
    files: Vec<PathBuf>,
    links: Vec<PathBuf>,
}

impl Inputs {
    fn collect(repo_root: &Path) -> Self {
        let mut inputs = Self {
            files: Vec::new(),
            links: Vec::new(),
        };
        inputs.visit(&repo_root.join("crates"), 0);
        inputs.files.sort();
        inputs.files.push(repo_root.join("Cargo.toml"));
        inputs.files.push(repo_root.join("Cargo.lock"));
        inputs.links.sort();
        inputs
    }

    fn visit(&mut self, dir: &Path, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                self.links.push(path.clone());
            }
            // Follow logical paths as before; bound directory-link loops.
            match std::fs::metadata(&path) {
                Ok(kind) if kind.is_dir() && depth < 32 => self.visit(&path, depth + 1),
                Ok(kind) if kind.is_file() => self.files.push(path),
                _ => {}
            }
        }
    }

    fn hash(&self, repo_root: &Path) -> u64 {
        let mut hasher = DefaultHasher::new();
        for file in &self.files {
            if let Ok(bytes) = std::fs::read(file) {
                file.strip_prefix(repo_root)
                    .unwrap_or(file)
                    .hash(&mut hasher);
                bytes.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

/// Hash all files under `crates/` plus the workspace manifest and lockfile,
/// and register their inputs with Cargo.
pub fn tracked_source_hash(repo_root: &Path) -> u64 {
    let inputs = Inputs::collect(repo_root);
    for path in [
        repo_root.join("crates"),
        repo_root.join("Cargo.toml"),
        repo_root.join("Cargo.lock"),
    ]
    .iter()
    .chain(&inputs.links)
    {
        // Explicit links also track deletion or creation of external targets.
        println!("cargo:rerun-if-changed={}", path.display());
    }
    inputs.hash(repo_root)
}

#[cfg(test)]
pub fn source_hash(repo_root: &Path) -> u64 {
    Inputs::collect(repo_root).hash(repo_root)
}
