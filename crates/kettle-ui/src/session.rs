//! Serializable snapshot of the tab/split tree + per-pane working directory,
//! persisted to `$CONFIG/kettle/session.json` and restored on launch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub enum SNode {
    Leaf {
        cwd: Option<String>,
        /// argv the pane ran (empty = default shell); persists SSH panes.
        #[serde(default)]
        cmd: Vec<String>,
    },
    Split {
        /// `true` = stacked (horizontal divider); `false` = side-by-side.
        vertical: bool,
        ratio: f32,
        a: Box<SNode>,
        b: Box<SNode>,
    },
}

#[derive(Serialize, Deserialize, Clone)]
pub struct STab {
    pub root: SNode,
    /// Focused pane's DFS-order index within `root`'s leaves (0 = first
    /// leaf). `#[serde(default)]` means old session files without this
    /// field still load — they restore to the first leaf, the pre-cycle
    /// behavior. Saved by `Mux::snapshot`, consumed by `Mux::restore`.
    #[serde(default)]
    pub focus: usize,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Session {
    pub tabs: Vec<STab>,
    pub active: usize,
    /// Theme chosen at runtime (`next_theme`/`prev_theme`); restored on
    /// launch so a picked theme sticks. `default` so old files still load.
    #[serde(default)]
    pub theme: Option<String>,
}

/// Cycle 291 / 789 (audit D1): sanitize a user-supplied layout name (from
/// `kettle --layout <NAME>`) to `[A-Za-z0-9._-]`, replacing every other byte —
/// crucially path separators `/` and `\`, plus the Windows drive `:` — with
/// `_`. This is the only guard stopping `--layout ../../etc/passwd` from
/// reading an arbitrary file: after sanitizing it becomes the in-`layouts/`
/// filename `.._.._etc_passwd.json`, with no separator to traverse on. Chars
/// are *replaced*, never dropped, so a non-empty name always yields a
/// non-empty, separator-free result; only an empty name returns `None`. The
/// `.json` suffix appended by the caller also defuses a bare `..` (→ `...json`,
/// a regular filename, not a parent-dir reference).
fn sanitize_layout_name(name: &str) -> Option<String> {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() { None } else { Some(safe) }
}

impl Session {
    pub fn path() -> Option<PathBuf> {
        kettle_config::Config::default_path()
            .and_then(|p| p.parent().map(|d| d.join("session.json")))
    }

    /// Cycle 291 named-layout path: returns
    /// `<config-dir>/layouts/<sanitized>.json`. The name is sanitized
    /// to `[A-Za-z0-9._-]` so a user can't traverse out of the
    /// layouts directory via `--layout ../../etc/passwd`. Returns
    /// `None` if the config dir isn't resolvable.
    pub fn path_for_layout(name: &str) -> Option<PathBuf> {
        let safe = sanitize_layout_name(name)?;
        kettle_config::Config::default_path().and_then(|p| {
            p.parent()
                .map(|d| d.join("layouts").join(format!("{safe}.json")))
        })
    }

    pub fn load() -> Option<Session> {
        let p = Self::path()?;
        load_from_path(&p)
    }

    /// Cycle 291: load from the named-layout path instead of the default
    /// `session.json`. Used when the user launched with `kettle --layout
    /// <NAME>`. Returns `None` if the layout file doesn't exist yet —
    /// kettle just starts with a default first tab, exactly the same
    /// shape as a fresh install would.
    pub fn load_layout(name: &str) -> Option<Session> {
        let p = Self::path_for_layout(name)?;
        load_from_path(&p)
    }

    /// Cycle 404 (Terminator parity, detachable-tabs Bucket-D
    /// sub-cycle 8 file-fallback): load a one-shot tab-handoff
    /// JSON file written by another kettle process (cycle 384's
    /// Action::MoveTabToNewWindow). Reads the path + deletes it
    /// after read (one-shot handoff — avoids accidental re-use
    /// across launches).
    pub fn load_tab_handoff(path: &std::path::Path) -> Option<Session> {
        let session = load_from_path(path)?;
        // One-shot: delete after read so a subsequent kettle
        // launch with the same args doesn't accidentally
        // re-restore stale handoff state.
        let _ = std::fs::remove_file(path);
        Some(session)
    }

    pub fn save(&self) {
        let Some(p) = Self::path() else {
            return;
        };
        if let Err(e) = save_to_path(self, &p) {
            log::warn!("could not save session to {}: {e}", p.display());
        }
    }

    /// Cycle 708 (Terminator parity, `terminatorlib/layoutlauncher.py`):
    /// list saved layouts by name (alphabetical). Walks
    /// `<config-dir>/layouts/*.json`, strips the extension. Returns
    /// an empty `Vec` when the layouts dir doesn't exist (a fresh
    /// install has none) — that's not an error, just "nothing to
    /// pick from yet". Closes the layout-launcher Bucket-D gap by
    /// giving `Action::OpenLayoutPicker` (cycle 708) a source of
    /// names to filter against.
    pub fn list_layouts() -> Vec<String> {
        let Some(default) = kettle_config::Config::default_path() else {
            return Vec::new();
        };
        let Some(dir) = default.parent().map(|d| d.join("layouts")) else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) != Some("json") {
                    return None;
                }
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .collect();
        names.sort();
        names
    }

    /// Cycle 291: save to the named-layout path. Creates the parent
    /// directory if it doesn't exist (a first-time
    /// `kettle --layout dev` from a fresh install needs to create
    /// `<config-dir>/layouts/` itself).
    pub fn save_layout(&self, name: &str) {
        let Some(p) = Self::path_for_layout(name) else {
            return;
        };
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = save_to_path(self, &p) {
            log::warn!("could not save layout {name:?} to {}: {e}", p.display());
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
}

/// Atomic save: serialize the session, write to a `.tmp` sibling, then
/// `rename` over the destination. If kettle is killed mid-write the
/// destination either survives intact (rename hasn't run yet) or holds
/// the new contents (rename succeeded) — never a half-written file.
/// That eliminates the upstream cause of cycle 108's corrupted-load
/// symptom: a non-atomic `fs::write(p, text)` left the user's session
/// in a corrupted state any time kettle hit an unclean shutdown
/// mid-write (signal, panic, crash, power loss). Returns the first I/O
/// error so the public `save` can surface it via `log::warn!` instead
/// of silently dropping every failure.
pub(crate) fn save_to_path(s: &Session, p: &std::path::Path) -> std::io::Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(s)
        .map_err(|e| std::io::Error::other(format!("serialize session: {e}")))?;
    // PID + nanos so two kettle processes that happen to save the same
    // session path within a clock tick don't collide on the temp file.
    // (Worst case: two windows of the same user; harmless but easy to
    // avoid.)
    let tmp = p.with_extension(format!(
        "json.tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp, text)?;
    // `rename` is atomic on every supported filesystem (POSIX rename(2),
    // Windows MoveFileEx with MOVEFILE_REPLACE_EXISTING — which is what
    // Rust's std uses internally). If the rename fails we still leave
    // the tmp file behind so the user has a forensic artifact; the
    // caller's `log::warn!` will name the destination.
    std::fs::rename(&tmp, p)?;
    Ok(())
}

/// Read and parse a session file at `path`. A read error (no file, HOME
/// changed) is the expected first-launch case and returns `None` silently;
/// a JSON parse error is a real signal — kettle was killed mid-write, the
/// disk filled up, the file got hand-edited badly — so we log a warning
/// AND rename the broken file to `<path>.broken.<unix-seconds>` so the
/// next launch starts fresh while the user keeps a forensic artifact.
///
/// Pure (well, takes a path; no other state) so the rename-on-corruption
/// contract is testable without standing up the full app.
///
/// Cycle 585: bound the read at 16 MiB. A realistic kettle session is
/// at most a few KB (a handful of panes × short cwd / title / cmd
/// strings). The session file is auto-generated by kettle, but a
/// swap-attack with filesystem access could replace it with a multi-GB
/// payload that `std::fs::read_to_string` would happily load into RAM.
/// 16 MiB is a 1000× margin over real sessions while still detecting
/// the bomb early via the metadata check (cheap stat call) before any
/// allocation.
pub(crate) fn load_from_path(p: &std::path::Path) -> Option<Session> {
    const MAX_SESSION_BYTES: u64 = 16 * 1024 * 1024;
    let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    if size > MAX_SESSION_BYTES {
        log::warn!(
            "session file {} is {size} bytes (cap {MAX_SESSION_BYTES}); \
             refusing to load and renaming to .toobig",
            p.display()
        );
        // Stash for forensics like the parse-error branch below; future
        // launches start fresh rather than blocking on a doom-loop.
        let dst = p.with_extension(format!(
            "json.toobig.{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        ));
        let _ = std::fs::rename(p, &dst);
        return None;
    }
    let text = std::fs::read_to_string(p).ok()?;
    match serde_json::from_str(&text) {
        Ok(s) => Some(s),
        Err(e) => {
            let dst = p.with_extension(format!(
                "json.broken.{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            ));
            let renamed = std::fs::rename(p, &dst).is_ok();
            log::warn!(
                "session file {} is corrupted ({e}); {}",
                p.display(),
                if renamed {
                    format!("backed up to {}", dst.display())
                } else {
                    "could not rename — kettle will overwrite on the next save".into()
                }
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cycle 789 drift guard (audit D1, security). `sanitize_layout_name` is
    /// the sole barrier between an untrusted `--layout <NAME>` CLI argument and
    /// the filesystem; a regression that stopped replacing path separators
    /// would reopen `--layout ../../etc/passwd` as an arbitrary-file read.
    #[test]
    fn sanitize_layout_name_blocks_path_traversal() {
        // Benign names round-trip byte-for-byte.
        assert_eq!(
            sanitize_layout_name("my-layout").as_deref(),
            Some("my-layout")
        );
        assert_eq!(
            sanitize_layout_name("test.json").as_deref(),
            Some("test.json")
        );
        assert_eq!(sanitize_layout_name("a_b.1-2").as_deref(), Some("a_b.1-2"));
        // No separator (forward, back, or Windows drive colon) and no NUL /
        // control byte survives — the whole point of the guard.
        for hostile in [
            "../../etc/passwd",
            "..\\..\\windows\\system32",
            "a/../b",
            "/abs/path",
            "C:\\evil",
            "a\0b\nc\td",
        ] {
            let s = sanitize_layout_name(hostile).expect("non-empty input stays Some");
            assert!(!s.contains('/'), "`{hostile}` left a forward slash: {s}");
            assert!(!s.contains('\\'), "`{hostile}` left a backslash: {s}");
            assert!(!s.contains(':'), "`{hostile}` left a drive colon: {s}");
            assert!(!s.contains('\0'), "`{hostile}` left a NUL: {s}");
        }
        // Exact shape of the canonical traversal attempt: dots survive,
        // separators collapse to `_`, so it lands inside `layouts/`.
        assert_eq!(
            sanitize_layout_name("../../etc/passwd").as_deref(),
            Some(".._.._etc_passwd")
        );
        // Chars are *replaced*, never dropped: only an empty name → None
        // (so the "all-special → None" intuition is wrong, and that matters —
        // a separator-only name must still produce a safe in-dir filename).
        assert_eq!(sanitize_layout_name(""), None);
        assert_eq!(sanitize_layout_name("/\\:").as_deref(), Some("___"));
    }

    fn tmp_dir(label: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "kettle-session-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn load_from_path_returns_none_silently_when_file_missing() {
        // First-launch case — no file at the path. Must NOT panic, must
        // NOT log (would be noisy on every fresh install). load_from_path
        // returning None covers both — log::warn! is only called from the
        // parse-error branch.
        let dir = tmp_dir("missing");
        let path = dir.join("session.json");
        assert!(load_from_path(&path).is_none());
        let _ = std::fs::remove_dir(&dir);
    }

    /// Cycle 585: a session.json swapped out for a multi-MB blob
    /// (filesystem-tampering scenario; out of strict scope but
    /// defense-in-depth) must not be read into RAM. The 16 MiB
    /// pre-read size cap returns None and renames the bomb to
    /// `.json.toobig.<unix-seconds>` for forensics, same pattern
    /// as the cycle-108 corrupted-file recovery path above.
    #[test]
    fn load_from_path_rejects_oversize_file_without_reading_into_memory() {
        let dir = tmp_dir("oversize");
        let path = dir.join("session.json");
        // Write a 17 MiB file (1 MiB over the 16 MiB cap). All
        // zero bytes — `read_to_string` would still allocate the
        // whole buffer if the cap weren't enforced, so just one
        // extra byte past the cap exercises the size branch.
        let oversize = vec![b'A'; 17 * 1024 * 1024];
        std::fs::write(&path, &oversize).unwrap();
        assert!(
            load_from_path(&path).is_none(),
            "oversize session must not deserialize"
        );
        assert!(!path.exists(), "original should be renamed to .toobig");
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".toobig."))
            .collect();
        assert_eq!(
            backups.len(),
            1,
            "expected one .toobig stash, got {backups:?}"
        );
        for n in &backups {
            let _ = std::fs::remove_file(dir.join(n));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn load_from_path_backs_up_corrupted_file_and_returns_none() {
        // Cycle-108 contract: a corrupted file (kettle killed mid-write,
        // disk full, hand-edit) used to silently drop the user's tabs/
        // splits state on the next launch. Now: return None *and* rename
        // the file out of the way so the user can inspect / restore.
        let dir = tmp_dir("corrupted");
        let path = dir.join("session.json");
        std::fs::write(&path, "{ this is not valid json at all").unwrap();
        assert!(
            load_from_path(&path).is_none(),
            "corrupted file should not deserialize"
        );
        assert!(!path.exists(), "original should be renamed out of the way");
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".broken."))
            .collect();
        assert_eq!(backups.len(), 1, "expected one backup, got {backups:?}");
        for n in &backups {
            let _ = std::fs::remove_file(dir.join(n));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn save_to_path_is_atomic_and_round_trips() {
        // Cycle-109 contract: save writes through a `.tmp` sibling then
        // renames into place. Asserts the rename happened (dest exists,
        // tmp doesn't), and that the saved content round-trips through
        // load_from_path back to an equivalent Session.
        let dir = tmp_dir("save");
        let path = dir.join("session.json");
        let s = Session {
            tabs: vec![STab {
                root: SNode::Leaf {
                    cwd: Some("/tmp".into()),
                    cmd: vec!["bash".into()],
                },
                focus: 0,
            }],
            active: 0,
            theme: Some("Dracula".into()),
        };
        save_to_path(&s, &path).expect("save");
        assert!(path.exists(), "destination written");
        // No leftover .tmp.* sibling (proves the rename succeeded, not
        // a stray crash that left the tmp file behind).
        let tmps: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(
            tmps.is_empty(),
            "tmp file should have been renamed away: {tmps:?}"
        );
        // Round-trip back through the load path.
        let loaded = load_from_path(&path).expect("load");
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.theme.as_deref(), Some("Dracula"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn save_to_path_overwrites_atomically() {
        // The rename-into-place semantics must also replace an existing
        // file, not error or leave the old contents. Save twice with
        // different state; second one wins.
        let dir = tmp_dir("save-overwrite");
        let path = dir.join("session.json");
        let s1 = Session::default();
        save_to_path(&s1, &path).expect("first save");
        let s2 = Session {
            tabs: vec![STab {
                root: SNode::Leaf {
                    cwd: None,
                    cmd: vec![],
                },
                focus: 0,
            }],
            active: 0,
            theme: None,
        };
        save_to_path(&s2, &path).expect("second save");
        let loaded = load_from_path(&path).expect("load");
        assert_eq!(loaded.tabs.len(), 1, "second save should win");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn load_from_path_round_trips_valid_session() {
        // Healthy session round-trips through serde_json. The empty
        // default is enough to confirm the rename-on-error logic
        // doesn't fire on the happy path.
        let dir = tmp_dir("ok");
        let path = dir.join("session.json");
        let s = Session::default();
        std::fs::write(&path, serde_json::to_string_pretty(&s).unwrap()).unwrap();
        let loaded = load_from_path(&path).expect("valid session loads");
        assert!(loaded.is_empty());
        assert!(path.exists());
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".broken."))
            .collect();
        assert!(
            backups.is_empty(),
            "no backup expected on happy path: {backups:?}"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn session_json_round_trips_ssh_panes() {
        let s = Session {
            tabs: vec![STab {
                root: SNode::Split {
                    vertical: false,
                    ratio: 0.5,
                    a: Box::new(SNode::Leaf {
                        cwd: Some("/tmp".into()),
                        cmd: vec![],
                    }),
                    b: Box::new(SNode::Leaf {
                        cwd: None,
                        cmd: vec!["ssh".into(), "-t".into(), "me@host".into()],
                    }),
                },
                focus: 1, // second leaf focused — round-trip should keep it
            }],
            active: 0,
            theme: Some("Dracula".into()),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tabs.len(), 1);
        assert_eq!(back.theme.as_deref(), Some("Dracula"), "theme persists");
        match &back.tabs[0].root {
            SNode::Split { a, b, .. } => {
                assert!(matches!(**a, SNode::Leaf { ref cwd, .. } if cwd.as_deref()==Some("/tmp")));
                assert!(matches!(**b, SNode::Leaf { ref cmd, .. } if cmd.len()==3));
            }
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn old_session_without_cmd_field_still_loads() {
        // `cmd` has #[serde(default)] so pre-SSH sessions remain loadable.
        let json = r#"{"tabs":[{"root":{"Leaf":{"cwd":null}}}],"active":0}"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert_eq!(s.tabs.len(), 1);
        // `theme` is also #[serde(default)] → None on pre-theme files.
        assert_eq!(s.theme, None);
        // `focus` likewise — pre-cycle sessions restore to first leaf.
        assert_eq!(s.tabs[0].focus, 0);
    }

    #[test]
    fn session_round_trips_focused_pane_index() {
        // Cycle 82: the session now records which leaf was focused so
        // restore brings the user back to the same pane within each tab.
        // Confirm the round-trip and that the default lands on first leaf
        // (preserving the pre-cycle behavior for older sessions).
        let s = Session {
            tabs: vec![STab {
                root: SNode::Split {
                    vertical: true,
                    ratio: 0.6,
                    a: Box::new(SNode::Leaf {
                        cwd: None,
                        cmd: vec![],
                    }),
                    b: Box::new(SNode::Leaf {
                        cwd: None,
                        cmd: vec![],
                    }),
                },
                focus: 1,
            }],
            active: 0,
            theme: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tabs[0].focus, 1, "focus index round-trips");
        // Old session without focus field defaults to 0.
        let legacy = r#"{"tabs":[{"root":{"Leaf":{"cwd":null}}}],"active":0}"#;
        let l: Session = serde_json::from_str(legacy).unwrap();
        assert_eq!(l.tabs[0].focus, 0);
    }
}
