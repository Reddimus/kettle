//! Multi-window cycle (Peacock accents): the cross-process window-presence
//! registry.
//!
//! Every live kettle WINDOW (across every kettle process) writes a
//! `<pid>-w<seq>.json` entry recording the accent color it claimed, so a new
//! window — in this process or another — can pick a hue no live window is
//! using (VS Code Peacock vibes, but deduped LIVE). Unlike the `discovery`
//! registry this has no endpoint and is always on: it costs one tiny file
//! per window and a directory listing per window-open.
//!
//! Failure policy: every operation is best-effort. A claim that can't be
//! written, a directory that can't be created, an unreadable entry — all
//! degrade to "no dedupe information", never to a startup failure.
//!
//! Staleness: entries from dead pids are pruned on every read (pid-liveness
//! via `kill(pid, 0)` on Unix / `OpenProcess` + `GetExitCodeProcess` on
//! Windows). Two processes claiming simultaneously can race to the same hue
//! (no locking by design); the worst case is two same-colored windows, which
//! the next window-open won't repeat.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One live window's accent claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceEntry {
    /// Format version.
    pub v: u32,
    /// Owning process.
    pub pid: u32,
    /// The window's per-process sequence number.
    pub win: u64,
    /// Claimed accent as `#rrggbb`.
    pub rgb: String,
    /// True when the claim came from the Peacock auto pool (a pinned
    /// `accent-color = <hex>` window still registers, so auto windows can
    /// avoid colliding with it).
    pub auto: bool,
    /// Unix seconds at claim time (diagnostics only).
    pub started_unix: u64,
}

/// Resolve the presence directory, environment injected for tests. Sibling
/// of the ctl discovery registry: `<runtime base>/kettle/instances`.
pub fn presence_dir_from(get: impl Fn(&str) -> Option<String>) -> PathBuf {
    // Discovery owns runtime-base validation and the private fallback policy.
    // Derive the sibling instead of duplicating that security boundary here.
    let mut dir = crate::discovery::registry_dir_from(get);
    dir.set_file_name("instances");
    dir
}

/// The default presence directory from the real environment.
pub fn presence_dir() -> PathBuf {
    presence_dir_from(|k| std::env::var(k).ok())
}

fn entry_path(dir: &Path, pid: u32, win: u64) -> PathBuf {
    dir.join(format!("{pid}-w{win}.json"))
}

/// Is `pid` a live process?
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0): no signal sent, just the permission/existence check.
        // ESRCH = gone; EPERM = alive but not ours (still alive).
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            // A reused pid whose process exited reports the real exit code;
            // STILL_ACTIVE (259) means it is genuinely running. (A process
            // that exits WITH code 259 is a documented Windows footgun we
            // accept — the entry just lives until the next reboot cleanup.)
            ok && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// RAII claim: the entry file lives as long as the guard. Dropped on window
/// close (or process exit via OS cleanup + the next reader's pruning).
#[derive(Debug)]
pub struct PresenceGuard {
    path: PathBuf,
    entry: PresenceEntry,
}

impl PresenceGuard {
    /// The currently-claimed color.
    pub fn rgb(&self) -> &str {
        &self.entry.rgb
    }

    /// Re-claim with a new color (theme switch re-resolves the pool slot).
    /// Best-effort, like everything here.
    pub fn set_rgb(&mut self, rgb: &str) {
        if self.entry.rgb == rgb {
            return;
        }
        self.entry.rgb = rgb.to_string();
        if let Ok(json) = serde_json::to_string(&self.entry) {
            let _ = std::fs::write(&self.path, json);
        }
    }
}

impl Drop for PresenceGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Write this window's claim. Returns `None` (silently — see the module
/// failure policy) when the directory or file can't be created.
pub fn claim(dir: &Path, entry: PresenceEntry) -> Option<PresenceGuard> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .ok()?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir).ok()?;
    let path = entry_path(dir, entry.pid, entry.win);
    let json = serde_json::to_string(&entry).ok()?;
    std::fs::write(&path, json).ok()?;
    Some(PresenceGuard { path, entry })
}

/// Every live claim, stale entries pruned as a side effect. Unreadable or
/// unparseable files are skipped (and removed — they can only be leftovers).
pub fn live_entries(dir: &Path) -> Vec<PresenceEntry> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<PresenceEntry>(&s).ok());
        match parsed {
            Some(e) if pid_alive(e.pid) => out.push(e),
            _ => {
                // Dead pid or garbage — prune so the dir can't grow forever.
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "kettle-presence-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn entry(pid: u32, win: u64, rgb: &str) -> PresenceEntry {
        PresenceEntry {
            v: 1,
            pid,
            win,
            rgb: rgb.into(),
            auto: true,
            started_unix: 0,
        }
    }

    #[test]
    fn claim_live_release_round_trip() {
        let dir = tmp("roundtrip");
        let me = std::process::id();
        let g1 = claim(&dir, entry(me, 1, "#cba6f7")).expect("claim 1");
        let _g2 = claim(&dir, entry(me, 2, "#89b4fa")).expect("claim 2");
        let mut rgbs: Vec<String> = live_entries(&dir).into_iter().map(|e| e.rgb).collect();
        rgbs.sort();
        assert_eq!(rgbs, vec!["#89b4fa".to_string(), "#cba6f7".to_string()]);
        // Dropping a guard releases its claim.
        drop(g1);
        let rgbs: Vec<String> = live_entries(&dir).into_iter().map(|e| e.rgb).collect();
        assert_eq!(rgbs, vec!["#89b4fa".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dead_pid_entries_are_pruned_on_read() {
        let dir = tmp("stale");
        std::fs::create_dir_all(&dir).unwrap();
        // A pid that can't be alive: u32::MAX is far past any real pid table
        // on Windows and Linux alike.
        let stale = entry(u32::MAX - 1, 1, "#ff0000");
        std::fs::write(
            entry_path(&dir, stale.pid, stale.win),
            serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("garbage.json"), "not json").unwrap();
        assert!(
            live_entries(&dir).is_empty(),
            "dead-pid + garbage entries are dropped"
        );
        assert!(
            std::fs::read_dir(&dir).unwrap().flatten().next().is_none(),
            "pruned from disk too"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_rgb_updates_the_claim_in_place() {
        let dir = tmp("update");
        let me = std::process::id();
        let mut g = claim(&dir, entry(me, 7, "#cba6f7")).expect("claim");
        g.set_rgb("#a6e3a1");
        let live = live_entries(&dir);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].rgb, "#a6e3a1");
        assert_eq!(live[0].win, 7);
        drop(g);
        assert!(live_entries(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn own_pid_is_alive() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(u32::MAX - 1));
    }

    #[test]
    fn presence_dir_is_sibling_of_ctl_registry() {
        let fake = |k: &str| {
            (k == if cfg!(windows) {
                "LOCALAPPDATA"
            } else {
                "XDG_RUNTIME_DIR"
            })
            .then(|| "/base".to_string())
        };
        let p = presence_dir_from(fake);
        let d = crate::discovery::registry_dir_from(fake);
        assert_eq!(p.parent(), d.parent(), "shared <base>/kettle parent");
        assert_eq!(
            p.file_name().and_then(|name| name.to_str()),
            Some("instances")
        );
        assert_eq!(d.file_name().and_then(|name| name.to_str()), Some("ctl"));
    }
}
