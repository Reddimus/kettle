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

impl Session {
    pub fn path() -> Option<PathBuf> {
        kettle_config::Config::default_path()
            .and_then(|p| p.parent().map(|d| d.join("session.json")))
    }

    pub fn load() -> Option<Session> {
        let p = Self::path()?;
        load_from_path(&p)
    }

    pub fn save(&self) {
        let Some(p) = Self::path() else {
            return;
        };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, text);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
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
pub(crate) fn load_from_path(p: &std::path::Path) -> Option<Session> {
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
