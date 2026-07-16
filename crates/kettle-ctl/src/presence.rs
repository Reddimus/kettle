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

use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Presence records contain only fixed-size metadata. Bound leaf reads so a
/// corrupt same-user file cannot force an unbounded allocation during window
/// creation.
const MAX_PRESENCE_ENTRY_BYTES: usize = 4 * 1024;

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

impl PresenceEntry {
    fn is_valid(&self) -> bool {
        self.v == 1
            && self.pid != 0
            && self.rgb.len() == 7
            && self.rgb.starts_with('#')
            && self.rgb[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
    }
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
        let mut replacement = self.entry.clone();
        replacement.rgb = rgb.to_string();
        if !replacement.is_valid() {
            return;
        }
        if let Ok(json) = serde_json::to_vec(&replacement)
            && json.len() <= MAX_PRESENCE_ENTRY_BYTES
            && kettle_state::atomic_replace(
                &self.path,
                &json,
                kettle_state::AtomicWriteOptions::PRIVATE,
            )
            .is_ok()
        {
            self.entry = replacement;
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
    if !entry.is_valid() {
        return None;
    }
    ensure_private_dir(dir).ok()?;
    let path = entry_path(dir, entry.pid, entry.win);
    let json = serde_json::to_vec(&entry).ok()?;
    if json.len() > MAX_PRESENCE_ENTRY_BYTES {
        return None;
    }
    kettle_state::atomic_replace(&path, &json, kettle_state::AtomicWriteOptions::PRIVATE).ok()?;
    Some(PresenceGuard { path, entry })
}

/// Every live claim, stale entries pruned as a side effect. Unreadable or
/// unparseable files are skipped (and removed — they can only be leftovers).
pub fn live_entries(dir: &Path) -> Vec<PresenceEntry> {
    let mut out = Vec::new();
    if !private_dir_is_valid(dir) {
        return out;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let parsed = read_entry(&path);
        match parsed {
            Some(e)
                if entry_path(dir, e.pid, e.win) == path && e.is_valid() && pid_alive(e.pid) =>
            {
                out.push(e);
            }
            _ => {
                // Dead pid or garbage — prune so the dir can't grow forever.
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    out
}

fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};

        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let metadata = std::fs::symlink_metadata(dir)?;
        if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "presence directory is not owned by the current user",
            ));
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    if !private_dir_is_valid(dir) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "presence directory is not private",
        ));
    }
    Ok(())
}

fn private_dir_is_valid(dir: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    if !metadata.file_type().is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn read_entry(path: &Path) -> Option<PresenceEntry> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        if std::fs::symlink_metadata(path)
            .ok()
            .is_none_or(|metadata| metadata.file_type().is_symlink())
        {
            return None;
        }
    }
    let mut file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_PRESENCE_ENTRY_BYTES as u64 {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return None;
        }
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_PRESENCE_ENTRY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_PRESENCE_ENTRY_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(entry_path(&dir, me, 1))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
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
        ensure_private_dir(&dir).unwrap();
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
    fn invalid_claims_and_updates_are_rejected() {
        let dir = tmp("invalid-claims");
        let me = std::process::id();
        assert!(claim(&dir, entry(me, 1, "red")).is_none());
        assert!(claim(&dir, entry(0, 1, "#cba6f7")).is_none());

        let mut guard = claim(&dir, entry(me, 2, "#cba6f7")).expect("valid claim");
        guard.set_rgb("not-a-color");
        assert_eq!(guard.rgb(), "#cba6f7");
        assert_eq!(live_entries(&dir)[0].rgb, "#cba6f7");
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hostile_or_oversized_records_are_pruned_without_loading_them() {
        let dir = tmp("bounded-records");
        ensure_private_dir(&dir).unwrap();
        let me = std::process::id();

        let mismatch = entry(me, 1, "#cba6f7");
        kettle_state::atomic_replace(
            &dir.join("wrong-name.json"),
            &serde_json::to_vec(&mismatch).unwrap(),
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
        .unwrap();
        let mut invalid = entry(me, 2, "#123456");
        invalid.v = 99;
        kettle_state::atomic_replace(
            &entry_path(&dir, me, 2),
            &serde_json::to_vec(&invalid).unwrap(),
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
        .unwrap();
        kettle_state::atomic_replace(
            &dir.join("oversized.json"),
            &vec![b'x'; MAX_PRESENCE_ENTRY_BYTES + 1],
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
        .unwrap();

        assert!(live_entries(&dir).is_empty());
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_record_is_pruned_without_following_its_target() {
        use std::os::unix::fs::symlink;

        let dir = tmp("record-symlink");
        ensure_private_dir(&dir).unwrap();
        let target = dir.with_extension("target");
        kettle_state::atomic_replace(
            &target,
            &serde_json::to_vec(&entry(std::process::id(), 9, "#cba6f7")).unwrap(),
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
        .unwrap();
        symlink(&target, entry_path(&dir, std::process::id(), 9)).unwrap();

        assert!(live_entries(&dir).is_empty());
        assert!(target.is_file(), "the external target must not be removed");
        let _ = std::fs::remove_file(target);
        let _ = std::fs::remove_dir_all(dir);
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
