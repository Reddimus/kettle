//! Serializable snapshot of the tab/split tree + per-pane working directory,
//! persisted to `$CONFIG/kettle/session.json` and restored on launch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub enum SNode {
    Leaf {
        cwd: Option<String>,
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
