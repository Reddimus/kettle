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
//!
//! An entry names its owner by pid *and* by that process's start-time token
//! (`presence::process_start_token`), because a pid alone is recycled: without
//! the token a stranger inheriting the number keeps a dead server's entry
//! advertised forever and every client wastes a connect attempt on it.

use std::io::Read as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
#[cfg(windows)]
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, PSID};

#[cfg(unix)]
use crate::{
    length_safe_unix_socket_path, private_temp_socket_dir, stable_hash, unix_socket_path_fits,
};

/// Registry records are tiny (normally a few hundred bytes). Bound reads so a
/// corrupt same-user file cannot turn discovery into an unbounded allocation.
const MAX_REGISTRY_ENTRY_BYTES: usize = 16 * 1024;
const MAX_VERSION_BYTES: usize = 256;
/// A normal desktop has only a handful of live control servers. Bound the
/// directory walk so a corrupt or hostile same-user registry cannot make
/// every discovery attempt enumerate an arbitrarily large directory.
const MAX_REGISTRY_DIR_ENTRIES: usize = 1024;

/// A registry entry describing one running control server.
///
/// `#[non_exhaustive]`: an entry is only trustworthy while it names the process
/// instance that wrote it, so outside this crate one is built by
/// [`RegistryEntry::registering`] (which resolves the token) or by
/// deserializing one that already exists — never by a struct literal that can
/// leave the binding out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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
    /// The server process's [`crate::presence::process_start_token`] at
    /// registration, so the entry is bound to one process *instance* rather
    /// than to a pid the OS may hand to somebody else. `None` for an entry
    /// written by a build that predates the token, or on a platform that
    /// cannot report one; readers then fall back to bare pid liveness.
    #[serde(default)]
    pub start_token: Option<u64>,
}

impl RegistryEntry {
    /// An entry describing a control server hosted by the *current* process.
    ///
    /// The instance token is resolved here rather than passed in: it must
    /// describe the process that is registering, and a call site cannot forget
    /// what it never supplies.
    pub fn registering(
        kind: &str,
        pid: u32,
        endpoint: String,
        version: &str,
        started_unix: u64,
    ) -> Self {
        Self {
            v: 1,
            kind: kind.to_string(),
            pid,
            endpoint,
            version: version.to_string(),
            started_unix,
            start_token: crate::presence::process_start_token(pid),
        }
    }
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
        let direct = dir.join(format!("ctl-{pid}.sock"));
        // Leave headroom below the tightest known sun_path capacity (104 on
        // BSD/macOS) for the NUL terminator, matching activation.rs's margin.
        if unix_socket_path_fits(&direct) {
            return direct.to_string_lossy().into_owned();
        }
        fallback_ctl_endpoint(dir, pid, &private_temp_socket_dir())
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(unix)]
fn fallback_ctl_endpoint(
    dir: &std::path::Path,
    pid: u32,
    private_temp_dir: &std::path::Path,
) -> PathBuf {
    use std::os::unix::ffi::OsStrExt as _;

    let hash = stable_hash(
        dir.as_os_str()
            .as_bytes()
            .iter()
            .copied()
            .chain(pid.to_le_bytes()),
    );
    let file = format!("ctl-{hash:016x}.sock");
    length_safe_unix_socket_path(&file, private_temp_dir)
}

/// Path of the `<pid>.json` entry file.
fn entry_path(dir: &std::path::Path, pid: u32) -> PathBuf {
    dir.join(format!("{pid}.json"))
}

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
    let json = crate::protocol::to_json_vec_bounded(entry, MAX_REGISTRY_ENTRY_BYTES)
        .map_err(std::io::Error::other)?;
    kettle_state::atomic_replace(&path, &json, kettle_state::AtomicWriteOptions::PRIVATE)?;
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
    for ent in rd.take(MAX_REGISTRY_DIR_ENTRIES).flatten() {
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
/// alive, best-effort [`prune_stale`]ing each dead one as a side effect
/// (mirroring `presence::live_entries`), so dead entries from a
/// crashed/killed server don't accumulate and aren't probed.
///
/// `list` is kept pure (raw enumeration) for callers that want every entry
/// regardless of liveness (e.g. diagnostics); this is the liveness-aware view.
/// The client's `discover` runs the same `list_live_by` core with the same
/// `owner_alive` predicate, differing only in that it can inject a stand-in —
/// so what is pinned here is what discovery does. (Both are crate-private, so
/// they are named rather than linked: rustdoc denies a public item linking to
/// something the reader cannot follow.)
pub fn list_live(dir: &std::path::Path) -> Vec<RegistryEntry> {
    list_live_by(dir, owner_alive)
}

/// Whether the process instance that wrote `entry` is still running.
///
/// This is the predicate the production discovery path uses, named rather than
/// inlined so it is exercised directly by tests instead of only through
/// stand-ins.
pub fn owner_alive(entry: &RegistryEntry) -> bool {
    crate::presence::owner_alive(entry.pid, entry.start_token)
}

/// The shared core of [`list_live`] and the client's discovery enumeration,
/// with the liveness predicate injected so both run the same pruning rule.
pub(crate) fn list_live_by(
    dir: &std::path::Path,
    owner_alive: impl Fn(&RegistryEntry) -> bool,
) -> Vec<RegistryEntry> {
    let mut out = list(dir);
    out.retain(|e| {
        if owner_alive(e) {
            true
        } else {
            // Dead owner — its server can never come back under this pid; drop
            // the entry so the dir can't grow forever and we don't waste a
            // connect attempt on it. "Dead" includes a pid the OS has since
            // recycled: that process is a stranger, not our server.
            prune_stale(dir, e);
            false
        }
    });
    out
}

/// Remove an entry that was judged stale — but only while the file still *is*
/// that entry.
///
/// An entry is named on disk by its pid, and a pid outlives the process that
/// held it: between the read that judged this entry and this delete, the OS can
/// hand that number to a new kettle which registers at the very same path.
/// Deleting by pid alone would unadvertise that healthy server for the rest of
/// its life, because a server `register`s exactly once at startup and never
/// heartbeats. Re-reading makes the delete a no-op unless the file is still the
/// record that was judged; if it cannot be read at all, nothing is deleted —
/// an entry that outlives its owner is pruned by the next reader, whereas a
/// wrongly deleted one never comes back.
pub fn prune_stale(dir: &std::path::Path, entry: &RegistryEntry) {
    let path = entry_path(dir, entry.pid);
    let still_the_same = read_registry_entry(&path)
        .and_then(|text| serde_json::from_str::<RegistryEntry>(&text).ok())
        .is_some_and(|current| current == *entry);
    if still_the_same {
        let _ = std::fs::remove_file(&path);
    }
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

    // The raw bytes fit sun_path, but `to_string_lossy` expands each invalid
    // byte to a three-byte replacement character, so the path that would
    // actually reach bind(2) is longer than the one that was measured.
    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_measured_after_lossy_conversion() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let mut raw = b"/tmp/".to_vec();
        raw.extend(std::iter::repeat_n(0xffu8, 90));
        let path = PathBuf::from(OsString::from_vec(raw));

        use std::os::unix::ffi::OsStrExt as _;
        assert!(
            path.as_os_str().as_bytes().len() <= crate::MAX_UNIX_SOCKET_PATH_BYTES,
            "fixture must fit the raw byte budget"
        );
        assert!(
            path.to_string_lossy().len() > crate::MAX_UNIX_SOCKET_PATH_BYTES,
            "fixture must overflow once converted"
        );
        assert!(
            !unix_socket_path_fits(&path),
            "a path that overflows after lossy conversion must be rejected"
        );
    }
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
        let dir = crate::test_scratch_root().join(format!("kettle-ctl-reg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let e1 = RegistryEntry::registering("gui", 111, default_endpoint(&dir, 111), "x", 100);
        let e2 = RegistryEntry::registering("gui", 222, default_endpoint(&dir, 222), "x", 200);
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
        let dir =
            crate::test_scratch_root().join(format!("kettle-ctl-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A live entry (our own pid) and a dead one (u32::MAX-1 is far past any
        // real pid table on Windows and Linux alike — same convention as the
        // presence tests).
        let me = std::process::id();
        let live = RegistryEntry::registering("gui", me, default_endpoint(&dir, me), "x", 100);
        let dead = RegistryEntry::registering(
            "gui",
            u32::MAX - 1,
            default_endpoint(&dir, u32::MAX - 1),
            "x",
            200,
        );
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

    /// A server that died and whose pid the OS handed to an unrelated process
    /// must not stay advertised. The stale entry names a pid that IS alive —
    /// our own — so bare pid liveness cannot distinguish it from a real
    /// server, and the entry would otherwise survive every future discovery.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn list_live_prunes_an_entry_whose_pid_was_recycled() {
        let dir =
            crate::test_scratch_root().join(format!("kettle-ctl-recycled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let me = std::process::id();
        let mut recycled =
            RegistryEntry::registering("gui", me, default_endpoint(&dir, me), "x", 100);
        let token = recycled
            .start_token
            .expect("a supported platform reports a start token");
        // Same pid, a different process instance behind it.
        recycled.start_token = Some(token.wrapping_add(1));
        register(&dir, &recycled).unwrap();
        assert_eq!(list(&dir).len(), 1, "raw list still enumerates the entry");

        assert!(
            list_live(&dir).is_empty(),
            "an entry whose owning instance is gone is not live"
        );
        assert!(
            !entry_path(&dir, me).exists(),
            "the recycled-pid entry is pruned from disk"
        );

        // The complementary half: the same pid with its real token stays.
        let mine = RegistryEntry::registering("gui", me, default_endpoint(&dir, me), "x", 100);
        register(&dir, &mine).unwrap();
        assert_eq!(list_live(&dir).len(), 1, "this instance's entry survives");
        assert!(entry_path(&dir, me).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pruning is aimed at a *record*, not at a pid. A reader judges an entry
    /// stale from a snapshot, and by the time it deletes, the pid in that
    /// entry's filename can belong to a new kettle that has already registered
    /// there. Deleting by pid alone would unadvertise that healthy server for
    /// the rest of its life — a server registers exactly once and never
    /// heartbeats — which is strictly worse than the stale entry it was
    /// chasing.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn pruning_a_stale_entry_spares_the_live_one_that_replaced_it() {
        let dir = crate::test_scratch_root()
            .join(format!("kettle-ctl-prune-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let me = std::process::id();
        let mut stale = RegistryEntry::registering("gui", me, default_endpoint(&dir, me), "x", 100);
        let token = stale
            .start_token
            .expect("a supported platform reports a start token");
        stale.start_token = Some(token.wrapping_add(1));
        register(&dir, &stale).unwrap();
        let judged = list(&dir);
        assert_eq!(judged.len(), 1);
        assert!(!owner_alive(&judged[0]), "the snapshot's entry is stale");

        // The recycle completes: a new server owns that pid now and registered
        // at the same path before the delete aimed at the old record landed.
        let live = RegistryEntry::registering("gui", me, default_endpoint(&dir, me), "x", 200);
        register(&dir, &live).unwrap();

        prune_stale(&dir, &judged[0]);
        assert_eq!(
            list(&dir),
            vec![live.clone()],
            "a delete aimed at the stale record must not take the live one with it"
        );
        assert_eq!(
            list_live(&dir),
            vec![live.clone()],
            "and the live server is still discoverable"
        );

        // The same call does delete the record it actually judged.
        prune_stale(&dir, &live);
        assert!(
            list(&dir).is_empty(),
            "an entry still unchanged on disk is pruned"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_skips_garbage_files() {
        let dir =
            crate::test_scratch_root().join(format!("kettle-ctl-garbage-{}", std::process::id()));
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

        let dir =
            crate::test_scratch_root().join(format!("kettle-ctl-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let entry = RegistryEntry::registering("gui", 123, default_endpoint(&dir, 123), "x", 1);
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

        let root = crate::test_scratch_root()
            .join(format!("kettle-ctl-registry-guards-{}", std::process::id()));
        let dir = root.join("ctl");
        let redirected = root.join("redirected");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&redirected).unwrap();
        symlink(&redirected, &dir).unwrap();
        let entry = RegistryEntry::registering("gui", 321, default_endpoint(&dir, 321), "x", 1);
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
            endpoint.starts_with(private_temp_socket_dir().to_string_lossy().as_ref()),
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

    #[cfg(unix)]
    #[test]
    fn fallback_endpoint_stays_within_sun_path_when_tmpdir_is_long() {
        let registry = std::path::PathBuf::from(
            "/home/example.com/first.last/.local/state/kettle/ctl-with-an-overlong-registry-path",
        );
        let injected_private_temp = std::path::PathBuf::from("/tmp")
            .join("an-unusually-long-tmpdir-component".repeat(4))
            .join("kettle-1234");
        let unchecked = injected_private_temp.join("ctl-0123456789abcdef.sock");
        assert!(
            !unix_socket_path_fits(&unchecked),
            "test TMPDIR fixture must overflow sun_path"
        );

        let endpoint = fallback_ctl_endpoint(&registry, 123456, &injected_private_temp);
        assert!(
            unix_socket_path_fits(&endpoint),
            "completed fallback must fit sun_path: {endpoint:?}"
        );
        assert!(
            endpoint.starts_with(
                std::path::Path::new("/tmp").join(format!("kettle-{}", unsafe { libc::geteuid() }))
            ),
            "an overlong TMPDIR must fall back to the fixed short Unix temp root"
        );
    }

    #[cfg(windows)]
    #[test]
    fn current_process_owns_directories_and_files_it_creates() {
        let dir =
            crate::test_scratch_root().join(format!("kettle-ctl-owner-{}", std::process::id()));
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
