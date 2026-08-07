//! kettle's agent control plane — protocol, transport, discovery, and
//! client.
//!
//! This crate is UI-free and engine-free: it defines the versioned NDJSON wire
//! protocol ([`protocol`]), the local-IPC transport ([`transport`], a Unix
//! socket / Windows named pipe), the server discovery registry ([`discovery`]),
//! and a blocking [`client::Client`]. The GUI hosts the *server* side
//! (kettle-ui) over this transport; `kettle ctl` and `kettle mcp` (the bin)
//! host the client side. Keeping the protocol + transport here is the
//! forward-compat seam for the future `kettle-muxd` daemon
//! (docs/MUX-SERVER-DESIGN.md): the daemon can re-host the same server side and
//! no client changes.

pub mod activation;
pub mod client;
pub mod discovery;
// Cross-process window-presence registry (Peacock accent dedupe). No
// endpoint, always on, best-effort — see the module docs.
pub mod presence;
pub mod protocol;
pub mod transport;

pub use client::{Client, CtlError};
pub use discovery::{RegistryEntry, registry_dir};
pub use protocol::{Event, Request, Response, RpcError, error_codes};

#[cfg(unix)]
pub(crate) const MAX_UNIX_SOCKET_PATH_BYTES: usize = 100;

/// Validate both the native pathname bytes and the UTF-8 string handed to the
/// transport. Invalid UTF-8 expands during `to_string_lossy`, so checking only
/// the native length can still overflow `sockaddr_un.sun_path` after conversion.
#[cfg(unix)]
pub(crate) fn unix_socket_path_fits(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;

    path.as_os_str().as_bytes().len() <= MAX_UNIX_SOCKET_PATH_BYTES
        && path.to_string_lossy().len() <= MAX_UNIX_SOCKET_PATH_BYTES
}

#[cfg(unix)]
pub(crate) fn private_temp_socket_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("kettle-{}", unsafe { libc::geteuid() }))
}

#[cfg(unix)]
pub(crate) fn length_safe_unix_socket_path(
    file: &str,
    private_temp_dir: &std::path::Path,
) -> std::path::PathBuf {
    let candidate = private_temp_dir.join(file);
    if unix_socket_path_fits(&candidate) {
        return candidate;
    }

    // TMPDIR is user-controlled and can itself be too long. Both activation
    // and discovery use this fixed, uid-private second fallback.
    let short = std::path::PathBuf::from("/tmp")
        .join(format!("kettle-{}", unsafe { libc::geteuid() }))
        .join(file);
    assert!(
        unix_socket_path_fits(&short),
        "built-in Unix socket fallback exceeds sun_path"
    );
    short
}

pub(crate) fn stable_hash(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

/// Create `dir` (and its parents) as a directory only this user can enter, and
/// verify that is what it actually is.
///
/// Shared by the activation endpoint and the control-socket listener: both put
/// a local-IPC endpoint inside a path that is predictable from the uid, so
/// another local user can pre-create it. `create_dir_all` succeeds against an
/// existing directory and says nothing about who owns it, so ownership and mode
/// are checked after creation -- via `symlink_metadata`, so a symlink is
/// rejected rather than silently followed to its target.
/// Whether a directory with these attributes is safe to hold a local-IPC
/// endpoint.
///
/// Two arrangements are safe, and the second is easy to miss:
///
/// - owned by us, so nobody else can touch what we put inside; or
/// - owned by root with the sticky bit set. That is the standard shared
///   temporary directory: world-writable, but the sticky bit stops one user
///   unlinking another's entries, which is exactly the property that matters.
///
/// Anything else -- notably a directory some other unprivileged user created at
/// kettle's predictable, uid-derived endpoint path -- is rejected.
///
/// Rejecting the sticky root instead breaks every Linux system, where
/// `std::env::temp_dir()` IS `/tmp`. That asymmetry is why the first version of
/// this check passed on macOS (whose per-user `$TMPDIR` we own) and failed
/// every kettle-ctl test on Linux.
#[cfg(unix)]
pub(crate) fn unix_dir_is_safe_for_endpoint(is_dir: bool, uid: u32, mode: u32) -> bool {
    if !is_dir {
        return false;
    }
    if uid == unsafe { libc::geteuid() } {
        return true;
    }
    const S_ISVTX: u32 = 0o1000;
    uid == 0 && mode & S_ISVTX != 0
}

/// Verify `dir` is a directory safe to hold a control endpoint, creating it if
/// needed.
///
/// Weaker than [`ensure_private_dir`] on purpose. The control socket's parent
/// is whatever endpoint directory the caller resolved, which may be a shared
/// temp root this process legitimately cannot `chmod` -- macOS's per-user
/// `$TMPDIR` returns `EPERM` -- and which is not, and should not be, owned by
/// us. The attack this must stop is another local user pre-creating the
/// predictable, uid-derived endpoint directory and then removing or replacing
/// the socket inside it. See [`unix_dir_is_safe_for_endpoint`].
///
/// Unix-only: the Windows transport uses a named pipe, which has no directory
/// to secure, so an unconditional definition is dead code there and
/// `clippy -D warnings` rejects it.
#[cfg(unix)]
pub(crate) fn ensure_owned_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        // `symlink_metadata`, so a symlink planted at this path is rejected
        // rather than followed to a directory its owner does control.
        let metadata = std::fs::symlink_metadata(dir)?;
        if !unix_dir_is_safe_for_endpoint(
            metadata.file_type().is_dir(),
            metadata.uid(),
            metadata.mode(),
        ) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "control socket directory is neither owned by this user nor a sticky shared root: {}",
                    dir.display()
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn ensure_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
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
                format!(
                    "control directory is not owned by the current user: {}",
                    dir.display()
                ),
            ));
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let metadata = std::fs::symlink_metadata(dir)?;
        if metadata.mode() & 0o077 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "control directory is group- or world-accessible: {}",
                    dir.display()
                ),
            ));
        }
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_scratch_root() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .expect("Windows tests require LOCALAPPDATA or USERPROFILE")
    }
    #[cfg(not(windows))]
    {
        std::env::temp_dir()
    }
}

#[cfg(all(test, unix))]
mod endpoint_dir_safety_tests {
    use super::unix_dir_is_safe_for_endpoint;

    const STICKY_WORLD_WRITABLE: u32 = 0o41777;
    const PLAIN_WORLD_WRITABLE: u32 = 0o40777;

    #[test]
    fn a_directory_we_own_is_safe() {
        let us = unsafe { libc::geteuid() };
        assert!(unix_dir_is_safe_for_endpoint(true, us, 0o40700));
        assert!(unix_dir_is_safe_for_endpoint(
            true,
            us,
            PLAIN_WORLD_WRITABLE
        ));
    }

    // `std::env::temp_dir()` is `/tmp` on Linux: root-owned and world-writable,
    // but sticky, so no other user can unlink our socket. Rejecting this shape
    // broke every kettle-ctl test on Linux while passing on macOS, whose
    // per-user `$TMPDIR` we do own.
    #[test]
    fn the_sticky_shared_temp_root_is_safe() {
        assert!(unix_dir_is_safe_for_endpoint(
            true,
            0,
            STICKY_WORLD_WRITABLE
        ));
    }

    // The actual attack: another unprivileged user pre-creates kettle's
    // predictable, uid-derived endpoint directory.
    #[test]
    fn a_directory_another_user_owns_is_rejected() {
        let squatter = unsafe { libc::geteuid() }.wrapping_add(1);
        assert!(!unix_dir_is_safe_for_endpoint(true, squatter, 0o40700));
        assert!(!unix_dir_is_safe_for_endpoint(
            true,
            squatter,
            STICKY_WORLD_WRITABLE
        ));
    }

    // Root-owned but NOT sticky means anyone may unlink our socket.
    #[test]
    fn a_root_owned_directory_without_the_sticky_bit_is_rejected() {
        assert!(!unix_dir_is_safe_for_endpoint(
            true,
            0,
            PLAIN_WORLD_WRITABLE
        ));
    }

    #[test]
    fn a_symlink_or_file_is_rejected() {
        let us = unsafe { libc::geteuid() };
        assert!(!unix_dir_is_safe_for_endpoint(false, us, 0o40700));
        assert!(!unix_dir_is_safe_for_endpoint(
            false,
            0,
            STICKY_WORLD_WRITABLE
        ));
    }
}
