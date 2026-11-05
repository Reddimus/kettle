//! Source identity shared by the GUI and workers, independent of Git metadata.
//! Hashes logical paths and contents, including linked sources, in path order.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 100_000;

#[cfg(unix)]
type DirectoryIdentity = (u64, u64);
#[cfg(not(unix))]
type DirectoryIdentity = PathBuf;

fn directory_identity(path: &Path) -> Option<DirectoryIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = std::fs::metadata(path).ok()?;
        Some((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    path.canonicalize().ok()
}

struct Inputs {
    files: Vec<PathBuf>,
    links: Vec<PathBuf>,
}

impl Inputs {
    fn collect(repo_root: &Path) -> Self {
        Self::collect_with_budget(repo_root, MAX_ENTRIES)
    }

    fn collect_with_budget(repo_root: &Path, mut budget: usize) -> Self {
        let mut inputs = Self {
            files: Vec::new(),
            links: Vec::new(),
        };
        inputs.visit(
            &repo_root.join("crates"),
            0,
            false,
            &mut Vec::new(),
            &mut budget,
        );
        inputs.visit(
            &repo_root.join("vendor"),
            0,
            true,
            &mut Vec::new(),
            &mut budget,
        );
        for asset in ["assets/themes", "assets/fonts"] {
            inputs.visit(
                &repo_root.join(asset),
                0,
                false,
                &mut Vec::new(),
                &mut budget,
            );
        }
        inputs.files.sort();
        inputs.files.push(repo_root.join("Cargo.toml"));
        inputs.files.push(repo_root.join("Cargo.lock"));
        inputs.links.sort();
        inputs
    }

    fn visit(
        &mut self,
        dir: &Path,
        depth: usize,
        vendored: bool,
        ancestors: &mut Vec<DirectoryIdentity>,
        budget: &mut usize,
    ) {
        let Some(identity) = directory_identity(dir) else {
            return;
        };
        if ancestors.contains(&identity) {
            return;
        }
        assert!(depth <= 32, "source traversal depth exceeded");
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        ancestors.push(identity);
        for entry in entries.flatten() {
            *budget = budget
                .checked_sub(1)
                .expect("source traversal budget exceeded");
            // Match generated vendor paths in .gitignore before following
            // links or reading contents. The vendor workspace lockfile counts.
            if vendored
                && ((depth <= 1 && entry.file_name() == "target")
                    || (depth == 1 && entry.file_name() == "Cargo.lock"))
            {
                continue;
            }
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                self.links.push(path.clone());
            }
            // Only ancestors are excluded: distinct logical aliases still count.
            match std::fs::metadata(&path) {
                Ok(kind) if kind.is_dir() => {
                    self.visit(&path, depth + 1, vendored, ancestors, budget)
                }
                Ok(kind) if kind.is_file() => self.files.push(path),
                _ => {}
            }
        }
        ancestors.pop();
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

/// Hash sources under `crates/`, `vendor/`, compiled assets, the manifest and lockfile,
/// excluding generated vendor paths, and register their inputs with Cargo.
pub fn tracked_source_hash(repo_root: &Path) -> u64 {
    let inputs = Inputs::collect(repo_root);
    for path in [
        repo_root.join("crates"),
        repo_root.join("vendor"),
        repo_root.join("assets/themes"),
        repo_root.join("assets/fonts"),
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

#[cfg(test)]
mod tests {
    #[test]
    #[should_panic(expected = "source traversal budget exceeded")]
    fn traversal_budget_fails_explicitly() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("crates")).unwrap();
        std::fs::write(root.path().join("crates/source.rs"), "source").unwrap();
        super::Inputs::collect_with_budget(root.path(), 0);
    }
}
