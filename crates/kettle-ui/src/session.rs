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
        let text = std::fs::read_to_string(p).ok()?;
        serde_json::from_str(&text).ok()
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
