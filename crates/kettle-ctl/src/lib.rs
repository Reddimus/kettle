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

/// Create `dir` (and its parents) as a directory only this user can enter, and
/// verify that is what it actually is.
///
/// Shared by the activation endpoint and the control-socket listener: both put
/// a local-IPC endpoint inside a path that is predictable from the uid, so
/// another local user can pre-create it. `create_dir_all` succeeds against an
/// existing directory and says nothing about who owns it, so ownership and mode
/// are checked after creation -- via `symlink_metadata`, so a symlink is
/// rejected rather than silently followed to its target.
/// Verify `dir` is a real directory owned by this user, creating it if needed.
///
/// Weaker than [`ensure_private_dir`] on purpose. The control socket's parent
/// is whatever endpoint directory the caller resolved, which may be a shared
/// temp root that this process legitimately cannot `chmod` -- macOS's
/// per-user `$TMPDIR` returns `EPERM`. The attack this must stop is another
/// local user pre-creating the predictable, uid-derived endpoint directory and
/// then removing or replacing the socket inside it, and that is an OWNERSHIP
/// question. Tighten the mode where we can, but only the owner check is fatal.
pub(crate) fn ensure_owned_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        // `symlink_metadata`, so a symlink planted at this path is rejected
        // rather than followed to a directory its owner does control.
        let metadata = std::fs::symlink_metadata(dir)?;
        if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "control socket directory is not a directory owned by this user: {}",
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
