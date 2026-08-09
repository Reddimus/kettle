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

/// Whether `path` is a directory kettle named for itself.
///
/// Everything kettle puts under an XDG base or a temp root lives inside one
/// directory it chose the name of: `<base>/kettle`, or `<tmp>/kettle-<uid>` for
/// the length-safe socket fallback. Those are ours to set the mode on. The
/// conventional roots above them — `$XDG_RUNTIME_DIR`, `~/.local/state`, `/tmp`
/// — belong to the system or the user and are never touched.
#[cfg(unix)]
pub(crate) fn is_kettle_owned_dir_name(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "kettle" || name.starts_with("kettle-"))
}

/// Create `dir` and kettle's own directory above it with an explicit `0700`,
/// leaving only the conventional roots above that to the ambient umask.
///
/// This used to be `create_dir_all(parent)` followed by a mode-carrying builder
/// for the leaf alone, which made kettle create the very directory its own
/// checks then reject. On a `002` umask — Debian/Ubuntu's per-user-group
/// default — `$XDG_RUNTIME_DIR/kettle` landed at `0775`, and because
/// `kettle-state`'s private-path verifier walks *ancestors*, every private path
/// beneath it was refused. Observed on Ubuntu 24.04: single-instance
/// activation, the remote-command watcher and the update-check throttle all
/// silently disabled themselves, each reporting only a warning in a log.
///
/// `DirBuilder::mode` applies to every directory a recursive create makes, not
/// just the last one, so naming the mode is all that is required. An existing
/// kettle-owned directory left group-writable by an earlier run is repaired,
/// since otherwise the fix would only help installations that never ran.
#[cfg(unix)]
pub(crate) fn create_private_dir_chain(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};

    let private = |path: &std::path::Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    };

    if let Some(parent) = dir.parent() {
        if is_kettle_owned_dir_name(parent) {
            if let Some(root) = parent.parent() {
                std::fs::create_dir_all(root)?;
            }
            private(parent)?;
            let metadata = std::fs::symlink_metadata(parent)?;
            if metadata.file_type().is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 != 0
            {
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        } else {
            std::fs::create_dir_all(parent)?;
        }
    }
    private(dir)
}

pub(crate) fn ensure_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        create_private_dir_chain(dir)?;
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

/// The `002` umask regression, reproduced rather than reasoned about.
///
/// A permissive umask is process-wide, so these run in a re-executed child (the
/// same shape `kettle-state`'s restrictive-umask test uses) instead of racing
/// every other test in this binary.
#[cfg(all(test, unix))]
mod private_dir_chain_umask_tests {
    use super::{create_private_dir_chain, ensure_private_dir, test_scratch_root};
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    const CHILD_ENV: &str = "KETTLE_CTL_PERMISSIVE_UMASK_CHILD";

    fn mode_of(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap_or_else(|error| panic!("stat {}: {error}", path.display()))
            .permissions()
            .mode()
            & 0o7777
    }

    fn scratch(tag: &str) -> PathBuf {
        let root = test_scratch_root().join(format!(
            "kettle-ctl-umask-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        // The conventional root stands in for `$XDG_RUNTIME_DIR`: created
        // before the umask changes, and expected to survive untouched.
        std::fs::create_dir_all(&root).expect("scratch root");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).expect("root mode");
        root
    }

    fn in_child(name: &str, body: impl FnOnce()) {
        if std::env::var_os(CHILD_ENV).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--nocapture"])
                .env(CHILD_ENV, "1")
                .status()
                .expect("re-exec the test binary");
            assert!(status.success(), "permissive-umask child failed: {status}");
            return;
        }
        // SAFETY: only the isolated child reaches here, before it starts any
        // application threads.
        unsafe { libc::umask(0o002) };
        body();
    }

    #[test]
    fn a_permissive_umask_cannot_leave_kettles_own_directory_group_writable() {
        in_child(
            "private_dir_chain_umask_tests::\
             a_permissive_umask_cannot_leave_kettles_own_directory_group_writable",
            || {
                let root = scratch("create");
                let owned = root.join("kettle");
                let leaf = owned.join("ctl");

                create_private_dir_chain(&leaf).expect("create the private chain");

                // The regression: this was 0775, and because kettle-state's
                // verifier walks ancestors, every private path under it failed.
                assert_eq!(mode_of(&owned), 0o700, "kettle's own directory");
                assert_eq!(mode_of(&leaf), 0o700, "the leaf");
                assert_eq!(
                    mode_of(&root),
                    0o755,
                    "the conventional root above kettle's directory is not ours to change"
                );
                let _ = std::fs::remove_dir_all(&root);
            },
        );
    }

    #[test]
    fn an_existing_group_writable_kettle_directory_is_repaired() {
        in_child(
            "private_dir_chain_umask_tests::\
             an_existing_group_writable_kettle_directory_is_repaired",
            || {
                let root = scratch("repair");
                let owned = root.join("kettle");
                // Exactly what an earlier kettle left behind on a 002 umask.
                std::fs::create_dir_all(&owned).expect("pre-existing directory");
                std::fs::set_permissions(&owned, std::fs::Permissions::from_mode(0o775))
                    .expect("pre-existing mode");
                assert_eq!(mode_of(&owned), 0o775);

                ensure_private_dir(&owned.join("instances")).expect("ensure_private_dir");

                assert_eq!(
                    mode_of(&owned),
                    0o700,
                    "an installation that already ran must be repaired, not left broken"
                );
                let _ = std::fs::remove_dir_all(&root);
            },
        );
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
