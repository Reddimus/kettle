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
pub(crate) fn install(
    root: &Path,
    files: &OwnedFiles,
    identity: IdentityReader,
) -> Result<PathBuf, ()> {
    kettle_state::create_private_dirs(root).map_err(|_| ())?;
    let _installing =
        kettle_state::ExclusiveFileLock::acquire_timeout(&root.join(INSTALL_LOCK), INSTALL_WAIT)
            .map_err(|_| ())?;
    let directory = root.join(files.directory_name());
    if verify(&directory, files).is_err() {
        write(root, &directory, files);
        verify(&directory, files)?;
    }
    remove_stale(root, &directory, files, identity);
    Ok(directory)
}

#[cfg(not(unix))]
pub(crate) fn install(_: &Path, _: &OwnedFiles, _: IdentityReader) -> Result<PathBuf, ()> {
    Err(())
}

/// The lock Kettles sharing a root install under, and the longest an install
/// waits for another to finish.
#[cfg(unix)]
pub(crate) const INSTALL_LOCK: &str = ".install.lock";
#[cfg(unix)]
const INSTALL_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

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
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = path.symlink_metadata()?;
    if !metadata.is_dir() {
        return std::fs::remove_file(path);
    }
    let writable = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    match std::fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries {
                remove_tree(&entry?.path())?;
            }
        }
        // Unlisted, it can still go if it is empty.
        Err(_) if writable.is_err() => {}
        Err(error) => return Err(error),
    }
    std::fs::remove_dir(path)
}

/// Remove what is left under `root` that no pane will get again: this
/// executable's directories from the same or an older version, directories
/// whose executable is gone or that `identity` cannot read, and staging,
/// which under the install lock is always left over. Another installed
/// Kettle's directory stays, since its panes still get it, as does a newer
/// version's for this executable, which a Kettle upgraded in place offers. A
/// running program that read a removed directory keeps what it read.
#[cfg(unix)]
fn remove_stale(root: &Path, keep: &Path, files: &OwnedFiles, identity: IdentityReader) {
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
                    // Kept only when it is a newer version's.
                    !(ours.is_some() && version.as_deref().and_then(version_key) > ours)
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
        let directory = install(&root, &files, |_| None).unwrap();
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
        assert_eq!(install(&root, &files, |_| None), Ok(directory.clone()));
        assert_eq!(verify(&directory, &files), Ok(()));
        remove_tree(&base.path().join("kettle")).unwrap();
    }
}
