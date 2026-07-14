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

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Registry records are tiny (normally a few hundred bytes). Bound reads so a
/// corrupt same-user file cannot turn discovery into an unbounded allocation.
const MAX_REGISTRY_ENTRY_BYTES: usize = 16 * 1024;
const MAX_VERSION_BYTES: usize = 256;

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

#[cfg(unix)]
fn private_temp_state_dir() -> PathBuf {
    std::env::temp_dir()
        .join(format!("kettle-{}", unsafe { libc::geteuid() }))
        .join("state")
}

#[cfg(windows)]
fn private_temp_state_dir() -> PathBuf {
    std::env::temp_dir()
}

/// Resolve the registry directory, with the environment injected for tests.
pub fn registry_dir_from(get: impl Fn(&str) -> Option<String>) -> PathBuf {
    let env = |k: &str| get(k).filter(|s| !s.is_empty());
    let env_path = |k: &str| env(k).map(PathBuf::from).filter(|path| path.is_absolute());
    // Fall back to an absolute temp location (uid-namespaced on Unix) — NOT the
    // relative CWD ".", which would put the registry under whatever
    // directory the process happened to start in, so a server and a client
    // launched from different CWDs would never find each other (and writing
    // there pollutes arbitrary dirs). temp_dir keeps both sides in agreement.
    let base: PathBuf = if cfg!(windows) {
        env_path("LOCALAPPDATA").unwrap_or_else(std::env::temp_dir)
    } else {
        env_path("XDG_RUNTIME_DIR")
            .or_else(|| env_path("XDG_STATE_HOME"))
            .or_else(|| env_path("HOME").map(|home| home.join(".local/state")))
            .unwrap_or_else(private_temp_state_dir)
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

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

fn registry_entry_is_valid(dir: &std::path::Path, file_pid: u32, entry: &RegistryEntry) -> bool {
    entry.v == 1
        && entry.pid == file_pid
        && matches!(entry.kind.as_str(), "gui" | "muxd")
        && entry.endpoint == default_endpoint(dir, entry.pid)
        && !entry.version.is_empty()
        && entry.version.len() <= MAX_VERSION_BYTES
}

fn registry_dir_is_private(dir: &std::path::Path) -> bool {
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

fn read_registry_entry(path: &std::path::Path) -> Option<String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        // Reject a reparse-point leaf before opening it. The registry directory
        // is private, so only the owning user can race this check.
        if std::fs::symlink_metadata(path)
            .ok()
            .is_none_or(|metadata| metadata.file_type().is_symlink())
        {
            return None;
        }
    }
    let mut file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_REGISTRY_ENTRY_BYTES as u64 {
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
    std::io::Read::by_ref(&mut file)
        .take(MAX_REGISTRY_ENTRY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_REGISTRY_ENTRY_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Write (or replace) this server's registry entry. Creates the dir `0700` on
/// Unix. Returns the entry-file path so the server can unlink it on shutdown.
pub fn register(dir: &std::path::Path, entry: &RegistryEntry) -> std::io::Result<PathBuf> {
    if !registry_entry_is_valid(dir, entry.pid, entry) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid control registry entry",
        ));
    }
    #[cfg(unix)]
    {
        // Create the leaf dir 0700 from the start (DirBuilder applies the mode
        // at creation) so there is no create-then-chmod window where it is
        // briefly world-readable. Parents are created first without the mode
        // (they are conventional XDG dirs); then re-assert 0700 on the leaf in
        // case it already existed with looser perms.
        use std::os::unix::fs::{DirBuilderExt, MetadataExt as _, PermissionsExt};
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
                "control registry directory is not a current-user directory",
            ));
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    if !registry_dir_is_private(dir) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "control registry directory is not private",
        ));
    }
    let path = entry_path(dir, entry.pid);
    let json = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    let suffix = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(
        ".{}.json.tmp-{}-{suffix}",
        entry.pid,
        std::process::id()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let write_result = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        #[cfg(unix)]
        std::fs::rename(&tmp, &path)?;
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt as _;
            use windows_sys::Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            };

            let from: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            // SAFETY: both path buffers are NUL-terminated and remain alive for
            // the call. The files share a directory, so replacement stays on
            // one volume and is atomic from readers' perspective.
            if unsafe {
                MoveFileExW(
                    from.as_ptr(),
                    to.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
        #[cfg(unix)]
        if let Ok(directory) = std::fs::File::open(dir) {
            let _ = directory.sync_all();
        }
        Ok::<(), std::io::Error>(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write_result?;
    Ok(path)
}

/// Remove this server's entry (best-effort).
pub fn unregister(dir: &std::path::Path, pid: u32) {
    let _ = std::fs::remove_file(entry_path(dir, pid));
}

/// List all registry entries, skipping unparseable / unreadable files.
pub fn list(dir: &std::path::Path) -> Vec<RegistryEntry> {
    let mut out = Vec::new();
    if !registry_dir_is_private(dir) {
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
        let Some(file_pid) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<u32>().ok())
        else {
            continue;
        };
        if let Some(s) = read_registry_entry(&path)
            && let Ok(e) = serde_json::from_str::<RegistryEntry>(&s)
            && registry_entry_is_valid(dir, file_pid, &e)
        {
            out.push(e);
        }
    }
    // Newest first.
    out.sort_by_key(|e| std::cmp::Reverse(e.started_unix));
    out
}

/// Like [`list`], but filters out entries whose owning process is no longer
/// alive, best-effort `prune`ing each dead one as a side effect (mirroring
/// `presence::live_entries`). Used by the client's `discover` so dead entries
/// from a crashed/killed server don't accumulate and aren't probed.
///
/// `list` is kept pure (raw enumeration) for callers that want every entry
/// regardless of liveness (e.g. diagnostics); this is the liveness-aware view.
pub fn list_live(dir: &std::path::Path) -> Vec<RegistryEntry> {
    let mut out = list(dir);
    out.retain(|e| {
        if crate::presence::pid_alive(e.pid) {
            true
        } else {
            // Dead owner — its server can never come back under this pid; drop
            // the entry so the dir can't grow forever and we don't waste a
            // connect attempt on it.
            prune(dir, e.pid);
            false
        }
    });
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
            let d = registry_dir_from(|_| None);
            assert!(d.starts_with(private_temp_state_dir()));
            let d = registry_dir_from(|k| {
                (k == "XDG_RUNTIME_DIR").then(|| "relative/runtime".to_string())
            });
            assert!(d.is_absolute(), "relative XDG paths must be ignored");
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
            endpoint: default_endpoint(&dir, 111),
            version: "x".into(),
            started_unix: 100,
        };
        let e2 = RegistryEntry {
            pid: 222,
            endpoint: default_endpoint(&dir, 222),
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
    fn list_live_excludes_dead_pids_and_prunes_them() {
        let dir = std::env::temp_dir().join(format!("kettle-ctl-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A live entry (our own pid) and a dead one (u32::MAX-1 is far past any
        // real pid table on Windows and Linux alike — same convention as the
        // presence tests).
        let live = RegistryEntry {
            v: 1,
            kind: "gui".into(),
            pid: std::process::id(),
            endpoint: default_endpoint(&dir, std::process::id()),
            version: "x".into(),
            started_unix: 100,
        };
        let dead = RegistryEntry {
            pid: u32::MAX - 1,
            endpoint: default_endpoint(&dir, u32::MAX - 1),
            started_unix: 200,
            ..live.clone()
        };
        register(&dir, &live).unwrap();
        register(&dir, &dead).unwrap();
        // Raw `list` sees both; `list_live` keeps only the live one.
        assert_eq!(list(&dir).len(), 2, "raw list enumerates both");
        let alive = list_live(&dir);
        assert_eq!(alive.len(), 1, "only the live entry survives");
        assert_eq!(alive[0].pid, std::process::id());
        // The dead entry's file is pruned from disk as a side effect.
        assert!(
            !entry_path(&dir, dead.pid).exists(),
            "dead entry pruned from disk"
        );
        assert!(
            entry_path(&dir, live.pid).exists(),
            "live entry left on disk"
        );
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

    #[cfg(unix)]
    #[test]
    fn registry_entry_is_private_regular_and_exact_version() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!("kettle-ctl-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let entry = RegistryEntry {
            v: 1,
            kind: "gui".into(),
            pid: 123,
            endpoint: default_endpoint(&dir, 123),
            version: "x".into(),
            started_unix: 1,
        };
        let path = register(&dir, &entry).unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(metadata.file_type().is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);

        let mut wrong = entry.clone();
        wrong.v = 0;
        std::fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(
            list(&dir).is_empty(),
            "non-v1 discovery records are ignored"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn registry_rejects_symlink_leaf_and_untrusted_records() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let root =
            std::env::temp_dir().join(format!("kettle-ctl-registry-guards-{}", std::process::id()));
        let dir = root.join("ctl");
        let redirected = root.join("redirected");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&redirected).unwrap();
        symlink(&redirected, &dir).unwrap();
        let entry = RegistryEntry {
            v: 1,
            kind: "gui".into(),
            pid: 321,
            endpoint: default_endpoint(&dir, 321),
            version: "x".into(),
            started_unix: 1,
        };
        assert!(register(&dir, &entry).is_err());

        std::fs::remove_file(&dir).unwrap();
        let path = register(&dir, &entry).unwrap();
        let mut redirected_entry = entry.clone();
        redirected_entry.endpoint = root.join("attacker.sock").to_string_lossy().into_owned();
        std::fs::write(&path, serde_json::to_vec(&redirected_entry).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(list(&dir).is_empty(), "redirected endpoints are ignored");

        std::fs::write(&path, vec![b'x'; MAX_REGISTRY_ENTRY_BYTES + 1]).unwrap();
        assert!(list(&dir).is_empty(), "oversize records are ignored");

        let target = root.join("record-target.json");
        std::fs::write(&target, serde_json::to_vec(&entry).unwrap()).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::remove_file(&path).unwrap();
        symlink(&target, &path).unwrap();
        assert!(list(&dir).is_empty(), "symlink records are ignored");
        let _ = std::fs::remove_dir_all(&root);
    }
}
