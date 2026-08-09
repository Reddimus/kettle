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
    // Make kettle's own ancestors private on the way in. This looked exempt —
    // the leaf is chmod'd just below, and the parents are either a shared temp
    // root we must not touch or a directory some earlier path already fixed —
    // but the ordering refutes it: `CtlServer::start` binds the socket through
    // here BEFORE `discovery::register` repairs anything, so an agent-enabled
    // launch on a fresh install would bind under a 0775 `<base>/kettle` and
    // leave a window in which a group peer can rename the endpoint out of it.
    kettle_state::create_private_dirs(dir)?;
    {
        use std::os::unix::fs::MetadataExt as _;

        // No path-based chmod here. `create_private_dirs` above already set the
        // mode through a descriptor, so this was redundant — and in the
        // `<tmp>/kettle-<uid>` fallback it was a primitive: creating a NEW name
        // in a sticky /tmp is allowed, so a peer can plant that path as a
        // symlink to a directory they want narrowed. The helper correctly skips
        // it (ELOOP), and then this line followed the link and chmodded the
        // target before the check below could reject the bind.
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
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        kettle_state::create_private_dirs(dir)?;
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
    use super::{ensure_private_dir, test_scratch_root};
    use kettle_state::create_private_dirs;
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
        // Deliberately NOT named `kettle-…`: the scratch root stands in for a
        // conventional root, and a name the helper claims would make the test
        // assert against its own fixture.
        let root = test_scratch_root().join(format!(
            "ctl-umask-scratch-{tag}-{}-{:?}",
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

                create_private_dirs(&leaf).expect("create the private chain");

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

    #[test]
    fn a_pre_existing_nested_chain_is_repaired_at_every_level() {
        in_child(
            "private_dir_chain_umask_tests::\
             a_pre_existing_nested_chain_is_repaired_at_every_level",
            || {
                let root = scratch("nested-repair");
                let outer = root.join("kettle-1000");
                let middle = outer.join("state");
                let inner = middle.join("kettle");
                let leaf = inner.join("ctl");
                // Exactly what the OLD code left on a no-HOME machine.
                std::fs::create_dir_all(&leaf).expect("pre-existing chain");
                for path in [&outer, &middle, &inner, &leaf] {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o775))
                        .expect("pre-existing mode");
                }

                create_private_dirs(&leaf).expect("repair the nested chain");

                for path in [&outer, &middle, &inner, &leaf] {
                    assert_eq!(
                        mode_of(path),
                        0o700,
                        "{} was left group-writable by the repair",
                        path.display()
                    );
                }
                let _ = std::fs::remove_dir_all(&root);
            },
        );
    }

    #[test]
    fn a_kettle_named_target_repairs_itself_not_just_its_ancestors() {
        in_child(
            "private_dir_chain_umask_tests::\
             a_kettle_named_target_repairs_itself_not_just_its_ancestors",
            || {
                let root = scratch("self-repair");
                // `~/.config/kettle` is passed as the TARGET by the config
                // write-back and the update-check cache, not as a parent.
                let owned = root.join("kettle");
                std::fs::create_dir_all(&owned).expect("pre-existing directory");
                std::fs::set_permissions(&owned, std::fs::Permissions::from_mode(0o775))
                    .expect("pre-existing mode");

                create_private_dirs(&owned).expect("repair the target itself");

                assert_eq!(
                    mode_of(&owned),
                    0o700,
                    "a kettle-named target must repair itself, not only ancestors"
                );
                let _ = std::fs::remove_dir_all(&root);
            },
        );
    }

    /// A dotfile manager can make `~/.config/kettle` a symlink into its own
    /// repository. `O_NOFOLLOW` protects the FINAL component: a link there is
    /// skipped and left to the ownership checks at the call site, rather than
    /// having its target chmodded the way path-based `set_permissions` would.
    ///
    /// An ANCESTOR link is different, and the test says so rather than leaving
    /// the stronger-sounding claim standing: it resolves the way it does for
    /// every other path, so the real directory behind it is repaired. For a
    /// dotfile-managed tree that is the wanted outcome — the directory kettle
    /// actually uses gets secured — but it is not "never through a symlink".
    #[test]
    fn symlink_repair_skips_the_final_component_and_resolves_ancestors() {
        in_child(
            "private_dir_chain_umask_tests::\
             symlink_repair_skips_the_final_component_and_resolves_ancestors",
            || {
                let root = scratch("symlink");
                let real = root.join("elsewhere");
                std::fs::create_dir_all(&real).expect("link target");
                std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o775))
                    .expect("target mode");
                let link = root.join("kettle");
                std::os::unix::fs::symlink(&real, &link).expect("symlink");

                // Not `let _`: a test that ignores the result also passes when
                // the helper simply fails, which proves nothing about symlinks.
                create_private_dirs(&link).expect("a symlinked target is skipped, not an error");

                assert_eq!(
                    mode_of(&real),
                    0o775,
                    "the repair followed a symlink and chmodded its target"
                );
                assert!(
                    std::fs::symlink_metadata(&link)
                        .expect("the link survives")
                        .file_type()
                        .is_symlink(),
                    "the repair replaced the symlink instead of leaving it alone"
                );

                // The same, one level up: `<root>/kettle` a real directory but
                // its parent a link. The walk repairs from the outermost
                // kettle-named component down, so an intermediate link is on
                // that path too.
                let inner_real = root.join("inner-real");
                std::fs::create_dir_all(inner_real.join("kettle")).expect("inner tree");
                std::fs::set_permissions(
                    inner_real.join("kettle"),
                    std::fs::Permissions::from_mode(0o775),
                )
                .expect("inner mode");
                let inner_link = root.join("inner-link");
                std::os::unix::fs::symlink(&inner_real, &inner_link).expect("inner symlink");
                create_private_dirs(&inner_link.join("kettle").join("ctl"))
                    .expect("an intermediate link resolves like any path");
                // O_NOFOLLOW protects the FINAL component only. An ancestor
                // link resolves the way it does for every other path, so the
                // real directory behind it IS repaired — which is the right
                // outcome for a dotfile-managed tree, but not what "never
                // through a symlink" would suggest, so assert it rather than
                // leave the weaker claim standing.
                assert_eq!(
                    mode_of(&inner_real.join("kettle")),
                    0o700,
                    "the real directory behind an ancestor link should be repaired"
                );
                assert!(
                    std::fs::symlink_metadata(&inner_link)
                        .expect("the inner link survives")
                        .file_type()
                        .is_symlink(),
                    "an intermediate symlink was replaced"
                );
                let _ = std::fs::remove_file(&link);
                let _ = std::fs::remove_dir_all(&root);
            },
        );
    }

    /// The no-`HOME` fallback nests two kettle-named directories:
    /// `discovery::registry_dir_from` ends at `<tmp>/kettle-<uid>/state/kettle/ctl`
    /// when `XDG_RUNTIME_DIR`, `XDG_STATE_HOME` and `HOME` are all unset. Fixing
    /// only the immediate parent leaves the OUTER one at the umask's mercy, and
    /// the verifier walks every ancestor — so that path would still be refused,
    /// with a message pointing at a directory the fix had already handled.
    #[test]
    fn every_kettle_named_ancestor_is_private_not_just_the_innermost() {
        in_child(
            "private_dir_chain_umask_tests::\
             every_kettle_named_ancestor_is_private_not_just_the_innermost",
            || {
                let root = scratch("nested");
                let outer = root.join("kettle-1000");
                let middle = outer.join("state");
                let inner = middle.join("kettle");
                let leaf = inner.join("ctl");

                create_private_dirs(&leaf).expect("create the nested private chain");

                for path in [&outer, &middle, &inner, &leaf] {
                    assert_eq!(
                        mode_of(path),
                        0o700,
                        "{} must not be left group-writable",
                        path.display()
                    );
                }
                assert_eq!(mode_of(&root), 0o755, "the conventional root is untouched");
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
