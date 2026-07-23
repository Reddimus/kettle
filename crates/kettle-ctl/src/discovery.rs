//! The server discovery registry.
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
#[cfg(windows)]
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, PSID};

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
///
/// On Unix, `sockaddr_un.sun_path` is 108 bytes on Linux (104 on BSD/macOS); a
/// direct `<dir>/ctl-<pid>.sock` path can exceed that under a long
/// `XDG_STATE_HOME`/`HOME` (an LDAP/AD-joined `/home/example.com/first.last`
/// style path, or a long macOS username). When the direct path would
/// overflow, fall back to a short, deterministic path under a private
/// uid-namespaced temp dir — the same class of fallback `activation.rs`'s
/// `activation_paths()` already applies to its own AF_UNIX endpoint, so both
/// sockets in this crate share one length-safe construction.
pub fn default_endpoint(dir: &std::path::Path, pid: u32) -> String {
    #[cfg(windows)]
    {
        let _ = dir;
        format!(r"\\.\pipe\kettle-ctl-{pid}")
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let direct = dir.join(format!("ctl-{pid}.sock"));
        // Leave headroom below the tightest known sun_path capacity (104 on
        // BSD/macOS) for the NUL terminator, matching activation.rs's margin.
        if direct.as_os_str().as_bytes().len() <= 100 {
            return direct.to_string_lossy().into_owned();
        }
        let hash = stable_hash(
            dir.as_os_str()
                .as_bytes()
                .iter()
                .copied()
                .chain(pid.to_le_bytes()),
        );
        private_temp_ctl_dir()
            .join(format!("ctl-{hash:016x}.sock"))
            .to_string_lossy()
            .into_owned()
    }
}

/// The private, uid-namespaced temp dir used for the length-fallback socket
/// path above. Sibling of `private_temp_state_dir` (which fronts a missing
/// `XDG_RUNTIME_DIR`/`XDG_STATE_HOME`/`HOME`), but this one is used
/// unconditionally once the direct path is too long, regardless of which
/// registry dir triggered the overflow.
#[cfg(unix)]
fn private_temp_ctl_dir() -> PathBuf {
    std::env::temp_dir().join(format!("kettle-{}", unsafe { libc::geteuid() }))
}

/// FNV-1a, matching `activation.rs`'s `stable_hash` exactly so both endpoint
/// fallbacks in this crate use the same construction. Kept as a separate copy
/// (rather than a shared helper) to respect this file's ownership boundary.
#[cfg(unix)]
fn stable_hash(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
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
    #[cfg(windows)]
    {
        // Verify actual ownership instead of trusting whatever ACL
        // %LOCALAPPDATA% happened to inherit (a redirected/shared profile, a
        // Terminal-Services-style multi-user box, or an injected env var
        // could all point this at a non-private directory otherwise).
        owned_by_current_user(dir)
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
    #[cfg(windows)]
    {
        // Mirror the Unix uid check above: don't trust that a `<pid>.json`
        // found in the registry dir is actually ours, only that its owning
        // SID matches this process's — closing the same spoofing gap the
        // Unix arm already closes via `uid()`.
        if !owned_by_current_user(path) {
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

/// Owns a `PSID` produced by one of two Win32 APIs, freed the way each API
/// documents: `LocalFree` for the security-descriptor buffer
/// `GetNamedSecurityInfoW` allocates, or nothing beyond the `Vec`'s own
/// deallocation for the `TOKEN_USER` buffer `GetTokenInformation` fills (the
/// SID it returns points inside that same buffer). Either way, `sid()` is
/// only valid for as long as this value is alive.
#[cfg(windows)]
enum OwnedSid {
    Descriptor(PSECURITY_DESCRIPTOR, PSID),
    // `u64`-backed (rather than `Vec<u8>`) so the buffer is pointer-aligned:
    // `TOKEN_USER` embeds a `PSID` field and Windows requires the buffer
    // passed to `GetTokenInformation` to be suitably aligned for it. The
    // `Vec` is never read directly — it is a lifetime anchor keeping the
    // buffer the sibling `PSID` points into alive until this value drops.
    TokenBuffer(#[allow(dead_code)] Vec<u64>, PSID),
}

#[cfg(windows)]
impl OwnedSid {
    fn sid(&self) -> PSID {
        match self {
            OwnedSid::Descriptor(_, sid) | OwnedSid::TokenBuffer(_, sid) => *sid,
        }
    }
}

#[cfg(windows)]
impl Drop for OwnedSid {
    fn drop(&mut self) {
        if let OwnedSid::Descriptor(descriptor, _) = self {
            // SAFETY: `descriptor` was allocated by `GetNamedSecurityInfoW`,
            // which documents `LocalFree` as its release function.
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(*descriptor);
            }
        }
    }
}

/// True if `path`'s Win32 security-descriptor owner SID matches the current
/// process token's user SID. This is the Windows equivalent of the Unix
/// `metadata.uid() == geteuid()` checks used throughout this file — without
/// it, `registry_dir_is_private`/`read_registry_entry` on Windows previously
/// trusted whatever ACL the directory or file happened to have, rather than
/// verifying ownership.
#[cfg(windows)]
fn owned_by_current_user(path: &std::path::Path) -> bool {
    let Some(owner) = path_owner_sid(path) else {
        return false;
    };
    // A path is "ours" if its owner SID is one this process's token would
    // stamp on objects it creates. For an ordinary user that is the token
    // USER SID; for an elevated/admin process Windows defaults new objects'
    // owner to the Administrators group — the token OWNER SID — so a directory
    // this very process just created is owned by Administrators, not the user
    // (this is exactly what breaks on an elevated CI runner). Accept either,
    // which is the same OW (owner) / BA (Administrators) trust the control
    // named pipe's DACL already grants.
    for ours in [current_user_sid(), current_owner_sid()]
        .into_iter()
        .flatten()
    {
        // SAFETY: both SIDs were produced by a successful query and their
        // owning buffers (held alive by `ours`/`owner`) are still in scope.
        if unsafe { windows_sys::Win32::Security::EqualSid(ours.sid(), owner.sid()) != 0 } {
            return true;
        }
    }
    false
}

/// The current process token's user SID (the identity behind the token).
#[cfg(windows)]
fn current_user_sid() -> Option<OwnedSid> {
    use windows_sys::Win32::Security::{TOKEN_USER, TokenUser};
    // SAFETY: for a `TokenUser` query `GetTokenInformation` writes a
    // `TOKEN_USER` at the start of the buffer; its `.User.Sid` points inside
    // that same buffer, kept alive by the returned `OwnedSid::TokenBuffer`.
    token_information_sid(TokenUser, |buffer| unsafe {
        (*buffer.cast::<TOKEN_USER>()).User.Sid
    })
}

/// The current process token's default OWNER SID — the SID Windows stamps as
/// owner on new objects the process creates. Equal to the user SID for an
/// ordinary token, but the Administrators group SID for an elevated token.
#[cfg(windows)]
fn current_owner_sid() -> Option<OwnedSid> {
    use windows_sys::Win32::Security::{TOKEN_OWNER, TokenOwner};
    // SAFETY: for a `TokenOwner` query `GetTokenInformation` writes a
    // `TOKEN_OWNER` at the start of the buffer; its `.Owner` points inside
    // that same buffer, kept alive by the returned `OwnedSid::TokenBuffer`.
    token_information_sid(TokenOwner, |buffer| unsafe {
        (*buffer.cast::<TOKEN_OWNER>()).Owner
    })
}

/// Query the current process token via `OpenProcessToken` +
/// `GetTokenInformation` (the standard two-call size-then-fill pattern: the
/// first call fails but reports the required buffer size), then extract the
/// SID from the filled buffer with `sid_of`. The buffer is retained in the
/// returned `OwnedSid` so the SID pointer it hands back stays valid.
#[cfg(windows)]
fn token_information_sid(
    info_class: windows_sys::Win32::Security::TOKEN_INFORMATION_CLASS,
    sid_of: impl Fn(*const u8) -> PSID,
) -> Option<OwnedSid> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
    // closing; `token` is a valid output pointer for this call.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let mut needed: u32 = 0;
    // SAFETY: a null, zero-length buffer is the documented way to query the
    // required size; `needed` is a valid output pointer. This call is
    // expected to fail (ERROR_INSUFFICIENT_BUFFER) — only `needed` matters.
    unsafe {
        GetTokenInformation(token, info_class, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        // SAFETY: `token` is the valid handle opened above.
        unsafe { CloseHandle(token) };
        return None;
    }
    // Round up to whole `u64` words so the buffer is at least `needed` bytes
    // and pointer-aligned (see `OwnedSid::TokenBuffer`'s doc comment).
    let mut buffer: Vec<u64> = vec![0u64; (needed as usize).div_ceil(std::mem::size_of::<u64>())];
    let mut written = 0u32;
    // SAFETY: `buffer` holds at least `needed` bytes, matching the size probe.
    let ok = unsafe {
        GetTokenInformation(
            token,
            info_class,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut written,
        )
    };
    // SAFETY: `token` remains the valid handle opened above until closed here.
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return None;
    }
    let sid = sid_of(buffer.as_ptr().cast());
    if sid.is_null() {
        return None;
    }
    Some(OwnedSid::TokenBuffer(buffer, sid))
}

/// `path`'s owning SID, via `GetNamedSecurityInfoW(OWNER_SECURITY_INFORMATION)`.
#[cfg(windows)]
fn path_owner_sid(path: &std::path::Path) -> Option<OwnedSid> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated and remains alive for the call; the
    // remaining arguments are valid output pointers. On success `descriptor`
    // owns `owner`'s backing storage and is freed via `LocalFree` in
    // `OwnedSid`'s `Drop`.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS || owner.is_null() {
        if !descriptor.is_null() {
            // SAFETY: `descriptor` was allocated by the call above.
            unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor) };
        }
        return None;
    }
    Some(OwnedSid::Descriptor(descriptor, owner))
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

    #[cfg(unix)]
    #[test]
    fn default_endpoint_falls_back_when_the_direct_path_would_overflow_sun_path() {
        // A short dir stays direct: exactly the historical `<dir>/ctl-<pid>.sock`.
        let short = std::path::PathBuf::from("/run/u/kettle/ctl");
        assert_eq!(
            default_endpoint(&short, 123),
            short.join("ctl-123.sock").to_string_lossy().into_owned()
        );

        // An LDAP/AD-style long HOME (mirrors the failure scenario: a long
        // XDG_STATE_HOME/HOME under which the direct path exceeds sun_path).
        let long = std::path::PathBuf::from(
            "/home/example.com/first.last/.local/state/kettle/ctl-with-a-very-long-directory-name-that-pushes-past-the-sun-path-limit",
        );
        let direct_len = long
            .join("ctl-123456.sock")
            .to_string_lossy()
            .into_owned()
            .len();
        assert!(direct_len > 100, "test fixture must exceed the threshold");
        let endpoint = default_endpoint(&long, 123456);
        assert!(
            endpoint.len() <= 100,
            "fallback endpoint must fit sun_path: {endpoint:?} ({} bytes)",
            endpoint.len()
        );
        assert!(
            !endpoint.starts_with(long.to_string_lossy().as_ref()),
            "fallback must not reuse the overlong dir: {endpoint:?}"
        );
        assert!(
            endpoint.starts_with(private_temp_ctl_dir().to_string_lossy().as_ref()),
            "fallback must live under the private uid-namespaced temp dir: {endpoint:?}"
        );

        // Deterministic: same (dir, pid) always resolves to the same fallback
        // path, so a server's `bind` and a client's registry validity check
        // (`entry.endpoint == default_endpoint(dir, entry.pid)`) agree.
        assert_eq!(endpoint, default_endpoint(&long, 123456));
        // Distinct dirs must not collide on the same fallback socket path.
        let other_long = long.join("nested-but-still-way-too-long-for-a-unix-socket-path");
        assert_ne!(endpoint, default_endpoint(&other_long, 123456));
    }

    #[cfg(windows)]
    #[test]
    fn current_process_owns_directories_and_files_it_creates() {
        let dir = std::env::temp_dir().join(format!("kettle-ctl-owner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            owned_by_current_user(&dir),
            "a directory this process just created must be recognized as self-owned"
        );

        let file = dir.join("owned.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(
            owned_by_current_user(&file),
            "a file this process just created must be recognized as self-owned"
        );

        assert!(
            !owned_by_current_user(&dir.join("does-not-exist.txt")),
            "a nonexistent path must not be treated as owned"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
