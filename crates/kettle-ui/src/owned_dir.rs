//! A private, read-only directory of files Kettle writes and offers to the
//! programs a pane starts: Claude Code's plugin, and the startup files that
//! define `codex` in a pane's shell.
//!
//! Each set of files is written once, into a directory named by its
//! contents, so a directory a running program read is never rewritten, and is
//! checked again before every use without following links: owned by this user
//! or root, no writable edge, read-only modes, no entry Kettle did not write,
//! and every file exactly what Kettle wrote. Kettles that share a root install
//! one at a time.
//!
//! The check needs Unix ownership and modes; elsewhere nothing is offered.
#![cfg_attr(not(unix), allow(dead_code))]

#[cfg(unix)]
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Largest file the check reads; Kettle's own are well under it.
pub(crate) const MAX_FILE_BYTES: u64 = 16 * 1024;

/// Reads the executable and version an older directory under a root names,
/// for deciding what is stale.
pub(crate) type IdentityReader = fn(&Path) -> Option<(String, Option<String>)>;

/// One set of files for one Kettle executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnedFiles {
    /// What its directory's name starts with, before the hash.
    prefix: &'static str,
    /// The executable the files name.
    pub(crate) command: String,
    /// The Kettle version that wrote them.
    pub(crate) version: String,
    /// Each file, by its `/`-separated path inside the directory.
    pub(crate) files: Vec<(&'static str, String)>,
}

impl OwnedFiles {
    pub(crate) fn new(
        prefix: &'static str,
        command: String,
        version: String,
        files: Vec<(&'static str, String)>,
    ) -> Self {
        Self {
            prefix,
            command,
            version,
            files,
        }
    }

    /// The directory name, from the contents: changed files get a new
    /// directory.
    pub(crate) fn directory_name(&self) -> String {
        // FNV-1a: stable across builds, which a std hasher is not.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for (path, contents) in &self.files {
            for byte in path.bytes().chain([0]).chain(contents.bytes()).chain([0]) {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        format!("{}-{hash:016x}", self.prefix)
    }

    /// Every directory the files sit in, inside the top one, each before
    /// those it holds.
    fn directories(&self) -> Vec<String> {
        let mut directories: Vec<String> = self
            .files
            .iter()
            .flat_map(|(path, _)| {
                path.match_indices('/')
                    .map(|(at, _)| path[..at].to_owned())
                    .collect::<Vec<_>>()
            })
            .collect();
        directories.sort();
        directories.dedup();
        directories
    }

    /// The names directly inside `directory` (`""` for the top one), sorted.
    fn children(&self, directory: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .files
            .iter()
            .filter_map(|(path, _)| {
                let rest = if directory.is_empty() {
                    *path
                } else {
                    path.strip_prefix(directory)?.strip_prefix('/')?
                };
                Some(rest.split('/').next().unwrap_or(rest).to_owned())
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// Write `files` under `root` unless they are already there intact, and
/// remove what is stale. Returns their directory. Kettles that share `root`
/// install one at a time, so nothing changes between a check and what follows
/// it, and any staging found was left by an install that ended. `identity`
/// reads the executable and version an older directory under `root` names.
#[cfg(unix)]
///
/// While `keep_older` is set, typically because another build's primary is
/// still running after an update, directories of the same command at older
/// versions stay: that build's panes still read them (#575).
pub(crate) fn install(
    root: &Path,
    files: &OwnedFiles,
    identity: IdentityReader,
    keep_older: bool,
) -> Result<PathBuf, ()> {
    kettle_state::create_private_dirs(root).map_err(|_| ())?;
    // Never waits: this runs on the UI thread, and another Kettle can hold
    // the lock for as long as it is suspended. A busy lock asks for a retry.
    let Some(_installing) =
        kettle_state::ExclusiveFileLock::try_acquire(&root.join(INSTALL_LOCK)).map_err(|_| ())?
    else {
        if let Ok(mut retry) = RETRY_AT.lock() {
            retry.get_or_insert_with(|| std::time::Instant::now() + INSTALL_RETRY);
        }
        return Err(());
    };
    let directory = root.join(files.directory_name());
    if verify(&directory, files).is_err() {
        write(root, &directory, files);
        verify(&directory, files)?;
    }
    remove_stale(root, &directory, files, identity, keep_older);
    Ok(directory)
}

#[cfg(not(unix))]
pub(crate) fn install(_: &Path, _: &OwnedFiles, _: IdentityReader, _: bool) -> Result<PathBuf, ()> {
    Err(())
}

/// The lock Kettles sharing a root install under.
#[cfg(unix)]
pub(crate) const INSTALL_LOCK: &str = ".install.lock";
/// How soon an install that found the lock busy is tried again.
#[cfg(unix)]
const INSTALL_RETRY: std::time::Duration = std::time::Duration::from_millis(250);

/// When an install that found the lock busy wants another try, if one did.
static RETRY_AT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// The next install retry, for the event loop's wake deadline.
pub(crate) fn retry_deadline() -> Option<std::time::Instant> {
    *RETRY_AT.lock().ok()?
}

/// Whether an install retry is due at `now`, clearing it when it is.
pub(crate) fn take_retry(now: std::time::Instant) -> bool {
    let Ok(mut retry) = RETRY_AT.lock() else {
        return false;
    };
    if retry.is_some_and(|deadline| deadline <= now) {
        *retry = None;
        true
    } else {
        false
    }
}

/// Replace whatever is at `directory` with `files`, built read-only in
/// staging under `root` and renamed into place. A failure leaves no staging
/// and is for the check that follows to find.
#[cfg(unix)]
pub(crate) fn write(root: &Path, directory: &Path, files: &OwnedFiles) {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    if directory.symlink_metadata().is_ok() && remove_tree(directory).is_err() {
        return;
    }
    let mut suffix = [0u8; 8];
    if getrandom::fill(&mut suffix).is_err() {
        return;
    }
    let staging = root.join(format!(
        ".staging-{}",
        suffix
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ));
    let directories = files.directories();
    let build = || -> std::io::Result<()> {
        let private = |path: &Path| std::fs::DirBuilder::new().mode(0o700).create(path);
        private(&staging)?;
        for subdirectory in &directories {
            private(&staging.join(subdirectory))?;
        }
        for (path, contents) in &files.files {
            let path = staging.join(path);
            let mut file = kettle_state::create_private_file_new(&path)?;
            std::io::Write::write_all(&mut file, contents.as_bytes())?;
            file.sync_all()?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))?;
        }
        // Innermost first, so each is still writable while its own are
        // made read-only.
        for subdirectory in directories.iter().rev() {
            std::fs::set_permissions(
                staging.join(subdirectory),
                std::fs::Permissions::from_mode(0o500),
            )?;
        }
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o500))?;
        std::fs::rename(&staging, directory)
    };
    if build().is_err() {
        let _ = remove_tree(&staging);
    }
}

/// Whether `directory` holds exactly `files`, with nothing that another user
/// could change: no link anywhere, read-only modes, owned by this user or
/// root, and no entry Kettle did not write. An access list that lets this
/// user write adds nothing the owner could not do through the mode, and any
/// change it allows fails the next check.
#[cfg(unix)]
pub(crate) fn verify(directory: &Path, files: &OwnedFiles) -> Result<(), ()> {
    use std::os::unix::fs::PermissionsExt as _;
    let read_only_directory = |path: &Path| -> Result<(), ()> {
        let metadata = path.symlink_metadata().map_err(|_| ())?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o500 {
            return Err(());
        }
        kettle_state::validate_trusted_directory(path).map_err(|_| ())
    };
    // No entry beyond the files' own.
    let entries = |path: &Path| -> Result<Vec<String>, ()> {
        let mut names = std::fs::read_dir(path)
            .map_err(|_| ())?
            .map(|entry| {
                entry
                    .map_err(|_| ())
                    .and_then(|entry| entry.file_name().into_string().map_err(|_| ()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        names.sort();
        Ok(names)
    };
    read_only_directory(directory)?;
    if entries(directory)? != files.children("") {
        return Err(());
    }
    for subdirectory in files.directories() {
        let path = directory.join(&subdirectory);
        read_only_directory(&path)?;
        if entries(&path)? != files.children(&subdirectory) {
            return Err(());
        }
    }
    for (path, contents) in &files.files {
        let file = kettle_state::open_trusted_file_read(&directory.join(path)).map_err(|_| ())?;
        let metadata = file.metadata().map_err(|_| ())?;
        if metadata.permissions().mode() & 0o777 != 0o400 || metadata.len() > MAX_FILE_BYTES {
            return Err(());
        }
        let mut read = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut read)
            .map_err(|_| ())?;
        if read != contents.as_bytes() {
            return Err(());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn verify(_: &Path, _: &OwnedFiles) -> Result<(), ()> {
    Err(())
}

/// Remove `path` and what it holds without following a link, making Kettle's
/// read-only directories writable first. A directory whose mode this user
/// cannot change is still removed when it is empty.
#[cfg(unix)]
pub(crate) fn remove_tree(path: &Path) -> std::io::Result<()> {
    remove_tree_with(path, || {})
}

#[cfg(unix)]
fn remove_tree_with(path: &Path, opened: impl FnOnce()) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let parent = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path.parent().unwrap_or_else(|| Path::new(".")))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let name_c = std::ffi::CString::new(name.as_bytes())?;
    let held = match kettle_state::open_directory_for_tree_removal_at(&parent, name) {
        Ok(held) => held,
        Err(error) => {
            let flags = match error.raw_os_error() {
                Some(libc::ELOOP | libc::ENOTDIR) => 0,
                Some(libc::EACCES) => libc::AT_REMOVEDIR,
                _ => return Err(error),
            };
            // SAFETY: unlinkat resolves only this entry in the held parent; it never follows links.
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name_c.as_ptr(), flags) } == 0 {
                return Ok(());
            }
            return Err(std::io::Error::last_os_error());
        }
    };
    opened();
    kettle_state::remove_owned_readonly_tree_at(
        &parent,
        name,
        &held,
        kettle_state::TreeRemovalLimits {
            max_depth: 64,
            max_entries: 8192,
        },
    )
}

/// Remove what is left under `root` that no pane will get again: this
/// executable's directories from an older version, directories
/// whose executable is gone or that `identity` cannot read, and staging,
/// which under the install lock is always left over. Another installed
/// Kettle's directory stays, since its panes still get it, as does a newer
/// version's for this executable, which a Kettle upgraded in place offers,
/// and another of this version's, which a Kettle of this version with other
/// settings may still offer. A running program that read a removed directory
/// keeps what it read.
#[cfg(unix)]
fn remove_stale(
    root: &Path,
    keep: &Path,
    files: &OwnedFiles,
    identity: IdentityReader,
    keep_older: bool,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let ours = version_key(&files.version);
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let stale = if name.starts_with(".staging-") {
            true
        } else if name.starts_with(&format!("{}-", files.prefix)) {
            match identity(&path) {
                None => true,
                Some((command, version)) if command == files.command => {
                    // Kept when it is this version's or a newer one's, or any
                    // version's while another build may still be using it.
                    !(keep_older
                        || ours.is_some() && version.as_deref().and_then(version_key) >= ours)
                }
                Some((command, _)) => !Path::new(&command).exists(),
            }
        } else {
            false
        };
        if stale {
            let _ = remove_tree(&path);
        }
    }
}

/// A Kettle version's numbered parts, for comparing; `None` for one that is
/// not all numbers.
pub(crate) fn version_key(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|part| part.parse().ok()).collect()
}

/// The file at `path` inside `directory`, read as a trusted file of at most
/// [`MAX_FILE_BYTES`], for an identity reader.
#[cfg(unix)]
pub(crate) fn read_small(directory: &Path, path: &str) -> Option<Vec<u8>> {
    let file = kettle_state::open_trusted_file_read(&directory.join(path)).ok()?;
    let mut text = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut text).ok()?;
    (text.len() as u64 <= MAX_FILE_BYTES).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nested() -> OwnedFiles {
        OwnedFiles::new(
            "kettle-test",
            "/opt/kettle".into(),
            "5.0.0".into(),
            vec![
                ("kettle.json", "{}\n".into()),
                ("zsh/.zshenv", "zsh\n".into()),
                ("fish/fish/vendor_conf.d/kettle.fish", "fish\n".into()),
            ],
        )
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_does_not_follow_a_swapped_directory() {
        use std::os::unix::fs::symlink;
        let base = kettle_test_support::private_tempdir("kettle-cleanup-swap-");
        let staging = base.path().join(".staging-test");
        let moved = base.path().join("moved");
        let outside = base.path().join("outside");
        std::fs::create_dir(&staging).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let sentinel = outside.join("keep");
        std::fs::write(&sentinel, b"outside").unwrap();
        let _ = remove_tree_with(&staging, || {
            std::fs::rename(&staging, &moved).unwrap();
            symlink(&outside, &staging).unwrap();
        });
        assert_eq!(
            std::fs::read(&sentinel).ok().as_deref(),
            Some(&b"outside"[..]),
            "cleanup must never traverse a replacement symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_replaces_an_unreadable_owned_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = kettle_test_support::private_tempdir("kettle-owned-unreadable-");
        let files = nested();
        let directory = install(base.path(), &files, |_| None, false).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o300)).unwrap();
        let result = install(base.path(), &files, |_| None, false);
        // Restore access even when the regression leaves the old tree behind.
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(result, Ok(directory.clone()));
        assert_eq!(verify(&directory, &files), Ok(()));
        remove_tree(&directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn install_replaces_an_unreadable_owned_subdirectory() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = kettle_test_support::private_tempdir("kettle-owned-unreadable-child-");
        let files = nested();
        let directory = install(base.path(), &files, |_| None, false).unwrap();
        let child = directory.join("fish/fish");
        std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o300)).unwrap();
        let result = install(base.path(), &files, |_| None, false);
        if result.is_err() {
            std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        assert_eq!(result, Ok(directory.clone()));
        assert_eq!(verify(&directory, &files), Ok(()));
        remove_tree(&directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_repairs_only_the_held_unreadable_directory_after_a_swap() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let base = kettle_test_support::private_tempdir("kettle-unreadable-swap-");
        let staging = base.path().join("staging");
        let moved = base.path().join("moved");
        let outside = base.path().join("outside");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("old"), b"old").unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"outside").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o500)).unwrap();
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o300)).unwrap();
        let result = remove_tree_with(&staging, || {
            std::fs::rename(&staging, &moved).unwrap();
            symlink(&outside, &staging).unwrap();
        });
        assert!(result.is_err(), "a swapped name must be refused");
        assert!(
            moved.exists(),
            "the unreadable directory must have been opened"
        );
        assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 0);
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"outside");
        assert_eq!(
            std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o500
        );
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// Every directory a file sits in is known, parents first, and each
    /// directory's children are its files and directories only.
    #[test]
    fn a_nested_layout_lists_every_directory_and_its_children() {
        let files = nested();
        assert_eq!(
            files.directories(),
            ["fish", "fish/fish", "fish/fish/vendor_conf.d", "zsh"]
        );
        assert_eq!(files.children(""), ["fish", "kettle.json", "zsh"]);
        assert_eq!(files.children("fish"), ["fish"]);
        assert_eq!(files.children("fish/fish/vendor_conf.d"), ["kettle.fish"]);
        assert_eq!(files.children("zsh"), [".zshenv"]);
    }

    /// A nested set installs read-only at every level and verifies; an extra
    /// entry in a nested directory, or a changed file there, fails the check.
    #[cfg(unix)]
    #[test]
    fn a_nested_set_installs_read_only_and_verifies() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = kettle_test_support::private_tempdir("kettle-owned-dir-");
        let root = base.path().join("kettle/owned");
        let files = nested();
        let directory = install(&root, &files, |_| None, false).unwrap();
        assert_eq!(directory, root.join(files.directory_name()));
        for inner in ["", "fish", "fish/fish", "fish/fish/vendor_conf.d", "zsh"] {
            let mode = std::fs::metadata(directory.join(inner))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o500, "{inner}");
        }
        assert_eq!(verify(&directory, &files), Ok(()));
        let inner = directory.join("fish/fish/vendor_conf.d");
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(inner.join("extra.fish"), "x").unwrap();
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(verify(&directory, &files), Err(()));
        // The next install puts it right.
        assert_eq!(
            install(&root, &files, |_| None, false),
            Ok(directory.clone())
        );
        assert_eq!(verify(&directory, &files), Ok(()));
        remove_tree(&base.path().join("kettle")).unwrap();
    }

    /// The version an older test directory names, from its one file.
    #[cfg(unix)]
    fn test_identity(directory: &Path) -> Option<(String, Option<String>)> {
        let version = std::fs::read_to_string(directory.join("version")).ok()?;
        Some(("/opt/kettle/bin/kettle".into(), Some(version.trim().into())))
    }

    /// #575: after an update the old build's panes still read their files.
    /// While its primary runs, the new build's install keeps them; once it
    /// has exited, they go as stale.
    #[cfg(unix)]
    #[test]
    fn installing_keeps_older_same_command_files_while_another_build_runs() {
        let base = kettle_test_support::private_tempdir("kettle-owned-dir-builds-");
        let root = base.path().join("kettle/owned");
        let files = |version: &str| {
            OwnedFiles::new(
                "test",
                "/opt/kettle/bin/kettle".into(),
                version.into(),
                vec![("version", format!("{version}\n"))],
            )
        };
        let old = install(&root, &files("5.1.0"), test_identity, false).unwrap();
        install(&root, &files("5.2.0"), test_identity, true).unwrap();
        assert!(old.exists(), "removed while the old build still runs");
        install(&root, &files("5.2.0"), test_identity, false).unwrap();
        assert!(!old.exists(), "kept after the old build exited");
        remove_tree(&base.path().join("kettle")).unwrap();
    }
}
