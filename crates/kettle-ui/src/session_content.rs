//! Private, disposable text sidecar, bound to the exact session topology.

use crate::session::{MAX_RESTORE_PANES, MAX_RESTORE_WINDOWS, Session};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub(crate) const MAX_BYTES: usize = 8 * 1024 * 1024;
// JSON envelope, hashes and nested arrays; row budgets account for JSON escaping.
pub(crate) const TEXT_BUDGET: usize = MAX_BYTES - 64 * 1024;
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PaneContent {
    pub remote: bool,
    pub lines: Vec<kettle_core::restore::RestoreRow>,
}

pub(crate) type TabContent = Vec<PaneContent>;
pub(crate) type WindowContent = Vec<TabContent>;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContentSnapshot {
    version: u8,
    session_hash: [u8; 32],
    pub windows: Vec<WindowContent>,
}

fn session_hash(session: &Session) -> [u8; 32] {
    struct HashWriter(Sha256);
    impl io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    // Live argv can be large; hash the layout without allocating another copy.
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, session).expect("session serializes");
    writer.0.finalize().into()
}

pub(crate) fn path(session_path: &Path) -> PathBuf {
    session_path.with_extension("content")
}

impl ContentSnapshot {
    pub fn new(session: &Session, windows: Vec<WindowContent>) -> Self {
        Self {
            version: 1,
            session_hash: session_hash(session),
            windows,
        }
    }

    fn validate(&self, session: &Session) -> io::Result<()> {
        let windows = session.validated_restore_windows().map_err(invalid)?;
        if self.version != 1
            || self.session_hash != session_hash(session)
            || self.windows.len() != windows.len()
            || self.windows.len() > MAX_RESTORE_WINDOWS
        {
            return Err(invalid("content snapshot does not match session"));
        }
        let mut panes = 0;
        for (content, window) in self.windows.iter().zip(windows) {
            if content.len() != window.tabs.len() {
                return Err(invalid("content tab count"));
            }
            for (content, tab) in content.iter().zip(window.tabs) {
                if Some(content.len()) != tab.root.bounded_leaf_count(MAX_RESTORE_PANES) {
                    return Err(invalid("content pane count"));
                }
                panes += content.len();
                if panes > MAX_RESTORE_PANES {
                    return Err(invalid("content pane cap"));
                }
                for pane in content {
                    let lines = &pane.lines;
                    if lines.len() > kettle_core::restore::MAX_RESTORE_LINES
                        || !lines.iter().all(kettle_core::restore::valid_row)
                    {
                        return Err(invalid("invalid content row"));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn save(&self, session_path: &Path) -> io::Result<()> {
        let destination = path(session_path);
        let bytes = serde_json::to_vec(self).map_err(invalid)?;
        if bytes.len() > MAX_BYTES {
            return Err(invalid("content snapshot exceeds 8 MiB"));
        }
        if let Some(parent) = destination.parent() {
            kettle_state::create_private_dirs(parent)?;
        }
        kettle_state::atomic_replace(
            &destination,
            &bytes,
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
    }
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

pub(crate) fn load(session_path: &Path, session: &Session) -> Option<ContentSnapshot> {
    match load_result(session_path, session) {
        Ok(snapshot) => Some(snapshot),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            // Do not log parser errors: a serde error can contain private text.
            log::warn!(
                "ignoring invalid session content snapshot ({:?})",
                error.kind()
            );
            None
        }
    }
}

fn load_result(session_path: &Path, session: &Session) -> io::Result<ContentSnapshot> {
    let file = kettle_state::open_existing_private_file(&path(session_path))?;
    if file.metadata()?.len() > MAX_BYTES as u64 {
        return Err(invalid("content file too large"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("content file grew beyond cap"));
    }
    let snapshot: ContentSnapshot =
        serde_json::from_slice(&bytes).map_err(|_| invalid("corrupt content file"))?;
    snapshot.validate(session)?;
    Ok(snapshot)
}

pub(crate) fn remove(session_path: &Path) {
    let destination = path(session_path);
    let result = kettle_state::open_existing_private_file(&destination)
        .and_then(|file| kettle_state::remove_open_private_file(file, &destination));
    if let Err(error) = result
        && error.kind() != io::ErrorKind::NotFound
    {
        log::warn!(
            "could not remove session content snapshot ({:?})",
            error.kind()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SNode, STab};

    fn session() -> Session {
        Session {
            tabs: vec![STab {
                root: SNode::Leaf {
                    cwd: None,
                    cmd: vec![],
                    group: None,
                },
                focus: 0,
                title_override: None,
                zoomed: false,
            }],
            ..Session::default()
        }
    }

    #[test]
    fn private_content_round_trip_and_remove() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("session.json");
        let session = session();
        let snapshot = ContentSnapshot::new(
            &session,
            vec![vec![vec![PaneContent {
                remote: false,
                lines: vec![(
                    "\x1b[1;31m世界 e\u{301}\x1b[0m".into(),
                    false,
                    vec![(10, true), (13, true), (14, false), (17, true)],
                )],
            }]]],
        );
        snapshot.save(&path).unwrap();
        let loaded = load(&path, &session).unwrap();
        assert_eq!(
            loaded.windows[0][0][0].lines,
            snapshot.windows[0][0][0].lines
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(super::path(&path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        remove(&path);
        assert!(!super::path(&path).exists());
    }

    #[test]
    fn corrupt_oversized_hostile_and_mismatched_content_are_ignored() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("session.json");
        let session = session();
        for bytes in [b"{bad json".to_vec(), vec![b' '; MAX_BYTES + 1]] {
            kettle_state::atomic_replace(
                &super::path(&path),
                &bytes,
                kettle_state::AtomicWriteOptions::PRIVATE,
            )
            .unwrap();
            assert!(load(&path, &session).is_none());
        }
        for line in [
            "\x1b]52;c;evil\x07".into(),
            "x".repeat(kettle_core::restore::MAX_RESTORE_LINE_BYTES + 1),
        ] {
            ContentSnapshot::new(
                &session,
                vec![vec![vec![PaneContent {
                    remote: false,
                    lines: vec![(line, false, vec![])],
                }]]],
            )
            .save(&path)
            .unwrap();
            assert!(load(&path, &session).is_none());
        }
        ContentSnapshot::new(
            &session,
            vec![vec![vec![PaneContent {
                remote: false,
                lines: vec![("safe".into(), false, vec![])],
            }]]],
        )
        .save(&path)
        .unwrap();
        let mut different = session;
        different.active = 1;
        assert!(load(&path, &different).is_none());
    }

    #[test]
    fn total_serialized_byte_cap_is_enforced() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("session.json");
        let snapshot = ContentSnapshot::new(
            &session(),
            vec![vec![vec![PaneContent {
                remote: false,
                lines: vec![("x".repeat(MAX_BYTES), false, vec![])],
            }]]],
        );
        assert!(snapshot.save(&path).is_err());
        assert!(!super::path(&path).exists());
    }
}
