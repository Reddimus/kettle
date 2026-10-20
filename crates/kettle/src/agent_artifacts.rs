//! A deliberately narrow fallback: only files placed in a private session inbox.
//! No repository discovery, transcript parsing, directory recursion or watching.
use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 32;
const MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DELIVERIES: usize = 128;

#[derive(Debug)]
pub(crate) struct Inbox {
    _directory: tempfile::TempDir,
    path: PathBuf,
    seen: HashMap<PathBuf, u64>,
    attempts: usize,
}

/// Metadata captured before display begins. A concurrent rewrite must remain
/// eligible for a later preview rather than being marked as already shown.
pub(crate) struct Artifact {
    path: PathBuf,
    fingerprint: u64,
}

impl Artifact {
    pub(crate) fn key(&self) -> String {
        key(&self.path)
    }
}

pub(crate) fn key(path: &Path) -> String {
    let mut hash = DefaultHasher::new();
    path.hash(&mut hash);
    format!("artifact:{:016x}", hash.finish())
}

impl Inbox {
    pub(crate) fn new() -> std::io::Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("kettle-previews-")
            .tempdir()?;
        Ok(Self {
            path: directory.path().canonicalize()?,
            _directory: directory,
            seen: HashMap::new(),
            attempts: 0,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn fingerprint(&self, path: &Path) -> Option<u64> {
        // Reject aliases, directories, incomplete writes and oversized media.
        if path.parent()? != self.path() {
            return None;
        }
        let meta = std::fs::symlink_metadata(path).ok()?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_BYTES {
            return None;
        }
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        if !matches!(
            extension.as_str(),
            "svg" | "png" | "jpg" | "jpeg" | "webp" | "gif" | "mp4" | "webm" | "pdf" | "mmd"
        ) {
            return None;
        }
        // Each hook inspects at most MAX_ENTRIES metadata records, with
        // no image reads on the agent's critical path. Media's descriptor
        // attestation is the final rendering boundary. Unix ctime also
        // detects a rewrite whose author preserved mtime and size.
        let mut hasher = DefaultHasher::new();
        meta.len().hash(&mut hasher);
        meta.modified().ok()?.hash(&mut hasher);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            meta.dev().hash(&mut hasher);
            meta.ino().hash(&mut hasher);
            meta.ctime().hash(&mut hasher);
            meta.ctime_nsec().hash(&mut hasher);
        }
        Some(hasher.finish())
    }

    pub(crate) fn capture(&self, path: &Path) -> Option<Artifact> {
        // Permit equivalent parent aliases (including /var on macOS), but
        // never follow a symlink used as the final artifact itself.
        if !path.is_absolute() || !std::fs::symlink_metadata(path).ok()?.is_file() {
            return None;
        }
        let path = path.canonicalize().ok()?;
        Some(Artifact {
            fingerprint: self.fingerprint(&path)?,
            path,
        })
    }

    fn paths(&self) -> Option<Vec<PathBuf>> {
        let entries = std::fs::read_dir(self.path())
            .ok()?
            .take(MAX_ENTRIES + 1)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        if entries.len() > MAX_ENTRIES {
            return None;
        }
        let mut paths = entries
            .into_iter()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        Some(paths)
    }

    fn prune(&mut self, paths: &[PathBuf]) {
        let live: HashSet<_> = paths.iter().collect();
        self.seen.retain(|path, _| live.contains(path));
    }

    pub(crate) fn mark(&mut self, artifact: Artifact) {
        let Some(paths) = self.paths() else { return };
        self.prune(&paths);
        if paths.binary_search(&artifact.path).is_ok() {
            self.seen.insert(artifact.path, artifact.fingerprint);
        }
    }

    /// One admission per hook, at most 128 per launch. An unchanged rejected
    /// file is not retried. A changed file may replace its earlier shelf item.
    pub(crate) fn next(&mut self) -> Option<PathBuf> {
        if self.attempts >= MAX_DELIVERIES {
            return None;
        }
        let paths = self.paths()?;
        self.prune(&paths);
        for path in paths {
            let Some(hash) = self.fingerprint(&path) else {
                continue;
            };
            if self.seen.get(&path) == Some(&hash) {
                continue;
            }
            self.seen.insert(path.clone(), hash);
            self.attempts += 1;
            return Some(path);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn primary_path_aliases_share_the_inbox_key_without_following_file_links() {
        let mut inbox = Inbox::new().unwrap();
        let aliases = tempfile::tempdir().unwrap();
        let alias = aliases.path().join("inbox");
        std::os::unix::fs::symlink(inbox.path(), &alias).unwrap();
        let path = inbox.path().join("diagram.svg");
        std::fs::write(&path, "<svg/>").unwrap();
        let artifact = inbox.capture(&alias.join("diagram.svg")).unwrap();
        assert_eq!(artifact.key(), key(&path));
        inbox.mark(artifact);
        assert!(inbox.next().is_none());
        let link = inbox.path().join("link.svg");
        std::os::unix::fs::symlink(path, &link).unwrap();
        assert!(inbox.capture(&link).is_none());
    }
    #[test]
    fn primary_deliveries_remain_deduplicated_after_inbox_rotation() {
        let mut inbox = Inbox::new().unwrap();
        for index in 0..MAX_ENTRIES {
            let path = inbox.path().join(format!("{index}.svg"));
            std::fs::write(&path, "<svg/>").unwrap();
            let artifact = inbox.capture(&path).unwrap();
            inbox.mark(artifact);
            std::fs::remove_file(path).unwrap();
        }
        let path = inbox.path().join("latest.svg");
        std::fs::write(&path, "<svg/>").unwrap();
        let artifact = inbox.capture(&path).unwrap();
        inbox.mark(artifact);
        assert!(
            inbox.next().is_none(),
            "primary display was lost after rotating 32 files"
        );
    }
    #[test]
    fn only_explicit_visual_artifacts_are_admitted_and_updates_are_deduplicated() {
        let mut inbox = Inbox::new().unwrap();
        std::fs::write(inbox.path().join("test.txt"), "not media").unwrap();
        std::fs::create_dir(inbox.path().join("nested")).unwrap();
        std::fs::write(inbox.path().join("nested/hidden.svg"), "hidden").unwrap();
        assert!(inbox.next().is_none());
        let path = inbox.path().join("diagram.svg");
        std::fs::write(&path, "<svg/>").unwrap();
        assert_eq!(inbox.next(), Some(path.clone()));
        assert!(inbox.next().is_none());
        std::fs::write(&path, "<svg />").unwrap();
        assert_eq!(inbox.next(), Some(path.clone()));
        std::fs::write(&path, "<svg new/>").unwrap();
        let artifact = inbox.capture(&path).unwrap();
        inbox.mark(artifact);
        assert!(inbox.next().is_none());
    }
    #[test]
    fn an_inflight_rewrite_is_not_marked_as_already_displayed() {
        let mut inbox = Inbox::new().unwrap();
        let path = inbox.path().join("diagram.svg");
        std::fs::write(&path, "<svg/>").unwrap();
        let artifact = inbox.capture(&path).unwrap();
        std::fs::write(&path, "<svg changed/>").unwrap();
        inbox.mark(artifact);
        assert_eq!(inbox.next(), Some(path));
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_and_fifos_are_never_read() {
        let mut inbox = Inbox::new().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), inbox.path().join("alias.svg")).unwrap();
        let fifo = std::ffi::CString::new(
            inbox
                .path()
                .join("blocked.png")
                .as_os_str()
                .as_encoded_bytes(),
        )
        .unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(inbox.next().is_none());
    }
    #[test]
    fn oversized_directories_and_media_are_refused() {
        let mut inbox = Inbox::new().unwrap();
        let large = std::fs::File::create(inbox.path().join("large.png")).unwrap();
        large.set_len(MAX_BYTES + 1).unwrap();
        assert!(inbox.next().is_none());
        for index in 0..MAX_ENTRIES {
            std::fs::write(inbox.path().join(format!("{index}.svg")), "<svg/>").unwrap();
        }
        assert!(inbox.next().is_none());
    }
}
