//! Cycle 926 (agent-first A2): the server discovery registry.
//!
//! Each running kettle that has its control server enabled writes a
//! `<pid>.json` entry into a registry directory; a client lists the directory,
//! picks the newest live entry (or one named by `--pid`), and connects to its
//! endpoint. A registry directory (not a `latest` symlink) is used because
//! Windows symlink creation needs a privilege; per-pid files also make a
//! two-window setup explicit.
//!
//! Registry dir:
//! - Unix: `$XDG_RUNTIME_DIR/kettle/ctl` (else `$XDG_STATE_HOME/kettle/ctl`,
//!   else `$HOME/.local/state/kettle/ctl`), the dir created `0700`.
//! - Windows: `%LOCALAPPDATA%\kettle\ctl`.
//!
//! The `kind` field is reserved so the future `kettle-muxd` daemon can register
//! as `"muxd"` alongside `"gui"` without a format change.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A registry entry describing one running control server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Registry format version.
    pub v: u32,
    /// `"gui"` today; `"muxd"` reserved for the future daemon.
    pub kind: String,
    /// The server process's pid.
    pub pid: u32,
    /// Transport endpoint: a socket path (Unix) or `\\.\pipe\…` name (Windows).
    pub endpoint: String,
    /// kettle version string, for diagnostics.
    pub version: String,
    /// Unix seconds when the server started (newest wins on ambiguity).
    pub started_unix: u64,
}

/// Resolve the registry directory, with the environment injected for tests.
pub fn registry_dir_from(get: impl Fn(&str) -> Option<String>) -> PathBuf {
    let env = |k: &str| get(k).filter(|s| !s.is_empty());
    let base: PathBuf = if cfg!(windows) {
        env("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        env("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| env("XDG_STATE_HOME").map(PathBuf::from))
            .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .unwrap_or_else(|| PathBuf::from("."))
    };
    base.join("kettle").join("ctl")
}

/// The default registry directory from the real environment.
pub fn registry_dir() -> PathBuf {
    registry_dir_from(|k| std::env::var(k).ok())
}

/// The default transport endpoint for `pid` (matches the server's `bind`).
pub fn default_endpoint(dir: &std::path::Path, pid: u32) -> String {
    #[cfg(windows)]
    {
        let _ = dir;
        format!(r"\\.\pipe\kettle-ctl-{pid}")
    }
    #[cfg(not(windows))]
    {
        dir.join(format!("ctl-{pid}.sock"))
            .to_string_lossy()
            .into_owned()
    }
}

/// Path of the `<pid>.json` entry file.
fn entry_path(dir: &std::path::Path, pid: u32) -> PathBuf {
    dir.join(format!("{pid}.json"))
}

/// Write (or replace) this server's registry entry. Creates the dir `0700` on
/// Unix. Returns the entry-file path so the server can unlink it on shutdown.
pub fn register(dir: &std::path::Path, entry: &RegistryEntry) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let path = entry_path(dir, entry.pid);
    let json = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// Remove this server's entry (best-effort).
pub fn unregister(dir: &std::path::Path, pid: u32) {
    let _ = std::fs::remove_file(entry_path(dir, pid));
}

/// List all registry entries, skipping unparseable / unreadable files.
pub fn list(dir: &std::path::Path) -> Vec<RegistryEntry> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(&path)
            && let Ok(e) = serde_json::from_str::<RegistryEntry>(&s)
        {
            out.push(e);
        }
    }
    // Newest first.
    out.sort_by_key(|e| std::cmp::Reverse(e.started_unix));
    out
}

/// Remove a stale entry file (e.g. when a connect proves the server is dead).
pub fn prune(dir: &std::path::Path, pid: u32) {
    unregister(dir, pid);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_dir_prefers_xdg_runtime_on_unix() {
        if cfg!(windows) {
            let d = registry_dir_from(|k| (k == "LOCALAPPDATA").then(|| "C:/la".to_string()));
            assert!(d.ends_with("kettle/ctl") || d.ends_with("kettle\\ctl"));
        } else {
            let d = registry_dir_from(|k| (k == "XDG_RUNTIME_DIR").then(|| "/run/u".to_string()));
            assert_eq!(d, PathBuf::from("/run/u/kettle/ctl"));
            // Falls back to HOME/.local/state when XDG unset.
            let d = registry_dir_from(|k| (k == "HOME").then(|| "/home/x".to_string()));
            assert_eq!(d, PathBuf::from("/home/x/.local/state/kettle/ctl"));
        }
    }

    #[test]
    fn register_list_unregister_round_trip() {
        let dir = std::env::temp_dir().join(format!("kettle-ctl-reg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let e1 = RegistryEntry {
            v: 1,
            kind: "gui".into(),
            pid: 111,
            endpoint: "ep1".into(),
            version: "x".into(),
            started_unix: 100,
        };
        let e2 = RegistryEntry {
            pid: 222,
            started_unix: 200,
            ..e1.clone()
        };
        register(&dir, &e1).unwrap();
        register(&dir, &e2).unwrap();
        let listed = list(&dir);
        assert_eq!(listed.len(), 2);
        // Newest (higher started_unix) first.
        assert_eq!(listed[0].pid, 222);
        unregister(&dir, 222);
        assert_eq!(list(&dir).len(), 1);
        assert_eq!(list(&dir)[0].pid, 111);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_skips_garbage_files() {
        let dir = std::env::temp_dir().join(format!("kettle-ctl-garbage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bad.json"), "not json").unwrap();
        std::fs::write(dir.join("note.txt"), "ignored").unwrap();
        assert!(list(&dir).is_empty(), "garbage entries are skipped");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
