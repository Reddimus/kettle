//! The external decoder's binaries. They are looked for only in fixed places,
//! or where Kettle's configuration names them, never in `PATH`, and trusted
//! only when no one but the user or root could have put them there: the file
//! and every directory and link on the way to it belong to the user or root,
//! and none can be written by anyone else. A group- or world-writable
//! directory passes only with its sticky bit, which keeps others from
//! replacing what is in it. On macOS the admin group counts as root: its
//! members can change anything as root through sudo, and Homebrew leaves
//! its directories writable by that group. A trusted binary is checked
//! again before each run, and refused if it changed.

use std::ffi::OsString;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

/// Where a decoder is looked for, in order, before the user's Nix profile.
pub const SEARCH_DIRS: [&str; 4] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/run/current-system/sw/bin",
];
/// Most symbolic links followed resolving one binary.
const MAX_LINKS: usize = 32;
/// The group that may write what is trusted, as root may: macOS's admin
/// group (gid 80), whose members are root through sudo. Linux has none.
#[cfg(target_os = "macos")]
const ROOT_EQUIVALENT_GROUP: Option<u32> = Some(80);
#[cfg(not(target_os = "macos"))]
const ROOT_EQUIVALENT_GROUP: Option<u32> = None;

/// Why a binary is not trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The path is not absolute.
    Relative,
    /// Nothing is there, or a directory on the way is not one.
    Missing,
    /// It is not a regular executable file, or it has a set-id bit.
    NotExecutable,
    /// It, or a directory or link on the way to it, belongs to someone other
    /// than the user or root.
    Owner,
    /// It, or a directory on the way to it, can be written by someone else.
    Writable,
    /// More links than are followed, or a path that cannot be read.
    Unreadable,
    /// It is no longer what was trusted.
    Changed,
}

/// A binary trusted to run: the path it was named by, where that leads after
/// every link, and what the file was when trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tool {
    named: PathBuf,
    path: PathBuf,
    identity: Identity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            size: metadata.size(),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

impl Tool {
    /// Where the binary is, every link resolved: what is run.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path it was named by.
    pub fn named(&self) -> &Path {
        &self.named
    }

    /// Whether it is still what was trusted, resolved again from its name.
    pub fn check(&self) -> Result<(), Refusal> {
        let now = trust(&self.named)?;
        if now.path == self.path && now.identity == self.identity {
            Ok(())
        } else {
            Err(Refusal::Changed)
        }
    }

    /// The binary `name` beside this one, by the directory this one was
    /// named in (ffprobe beside ffmpeg), trusted on its own.
    pub fn sibling(&self, name: &str) -> Result<Tool, Refusal> {
        let directory = self.named.parent().ok_or(Refusal::Missing)?;
        trust(&directory.join(name))
    }
}

/// The first trusted `name` in [`SEARCH_DIRS`], then in `home`'s Nix
/// profile. An untrusted file in one place does not hide a trusted one in a
/// later place, and is never run.
pub fn search(name: &str, home: Option<&Path>) -> Option<Tool> {
    let nix = home
        .filter(|home| home.is_absolute())
        .map(|home| home.join(".nix-profile/bin"));
    SEARCH_DIRS
        .iter()
        .map(PathBuf::from)
        .chain(nix)
        .find_map(|directory| trust(&directory.join(name)).ok())
}

/// Trust the binary at `path`, resolving each link by hand so that every
/// directory and link actually passed through is checked, not only where
/// the path ends.
pub fn trust(path: &Path) -> Result<Tool, Refusal> {
    if !path.is_absolute() {
        return Err(Refusal::Relative);
    }
    // SAFETY: geteuid has no preconditions.
    let user = unsafe { libc::geteuid() };
    let mut resolved = PathBuf::from("/");
    check_directory(&resolved, &metadata(&resolved)?, user)?;
    let mut pending: Vec<OsString> = Vec::new();
    push_components(&mut pending, path);
    let mut links = 0;
    while let Some(part) = pending.pop() {
        if part == ".." {
            // `resolved` holds no links, so its parent is the real one.
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&part);
        let found = metadata(&candidate)?;
        if found.file_type().is_symlink() {
            links += 1;
            if links > MAX_LINKS {
                return Err(Refusal::Unreadable);
            }
            if !owned(found.uid(), user) {
                return Err(Refusal::Owner);
            }
            let target = std::fs::read_link(&candidate).map_err(|_| Refusal::Unreadable)?;
            if target.is_absolute() {
                resolved = PathBuf::from("/");
            }
            push_components(&mut pending, &target);
            continue;
        }
        if pending.is_empty() {
            check_leaf(&candidate, &found, user)?;
            return Ok(Tool {
                named: path.to_path_buf(),
                path: candidate,
                identity: Identity::of(&found),
            });
        }
        if !found.is_dir() {
            return Err(Refusal::Missing);
        }
        check_directory(&candidate, &found, user)?;
        resolved = candidate;
    }
    Err(Refusal::Missing)
}

/// Push `path`'s parts so the first comes off the stack first. `.` parts
/// and the root are dropped; `..` is kept to resolve against what is real.
fn push_components(pending: &mut Vec<OsString>, path: &Path) {
    let parts: Vec<OsString> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_os_string()),
            Component::ParentDir => Some(OsString::from("..")),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
        })
        .collect();
    pending.extend(parts.into_iter().rev());
}

fn metadata(path: &Path) -> Result<Metadata, Refusal> {
    std::fs::symlink_metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => Refusal::Missing,
        _ => Refusal::Unreadable,
    })
}

fn owned(uid: u32, user: u32) -> bool {
    uid == 0 || uid == user
}

/// Whether the mode lets someone other than the owner, root or the
/// root-equivalent group write.
fn writable_by_others(found: &Metadata) -> bool {
    let mode = found.mode();
    mode & 0o002 != 0 || (mode & 0o020 != 0 && Some(found.gid()) != ROOT_EQUIVALENT_GROUP)
}

fn check_directory(path: &Path, found: &Metadata, user: u32) -> Result<(), Refusal> {
    if !owned(found.uid(), user) {
        return Err(Refusal::Owner);
    }
    let sticky = found.mode() & 0o1000 != 0;
    if writable_by_others(found) && !sticky {
        return Err(Refusal::Writable);
    }
    #[cfg(target_os = "macos")]
    if !sticky && crate::acl::grants_write(path) {
        return Err(Refusal::Writable);
    }
    let _ = path;
    Ok(())
}

fn check_leaf(path: &Path, found: &Metadata, user: u32) -> Result<(), Refusal> {
    if !found.is_file() || found.mode() & 0o111 == 0 || found.mode() & 0o6000 != 0 {
        return Err(Refusal::NotExecutable);
    }
    if !owned(found.uid(), user) {
        return Err(Refusal::Owner);
    }
    if writable_by_others(found) {
        return Err(Refusal::Writable);
    }
    #[cfg(target_os = "macos")]
    if crate::acl::grants_write(path) {
        return Err(Refusal::Writable);
    }
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    fn executable(path: &Path) {
        std::fs::write(path, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn chmod(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A private temporary directory. Its parents are the system's, which
    /// must be trusted for these tests to mean anything: every directory on
    /// the way passes, and the directory itself is no executable.
    fn private_dir() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        chmod(directory.path(), 0o700);
        assert_eq!(
            trust(directory.path()).unwrap_err(),
            Refusal::NotExecutable,
            "the temporary directory's parents are not trusted"
        );
        directory
    }

    /// A binary in a private directory is trusted through links, relative
    /// and absolute, with the path it leads to and the name it was given;
    /// the trust is checked again and holds until the file changes.
    #[test]
    fn a_private_binary_is_trusted_through_its_links() {
        let directory = private_dir();
        let cellar = directory.path().join("Cellar/ffmpeg/9.0/bin");
        std::fs::create_dir_all(&cellar).unwrap();
        for part in [
            "Cellar",
            "Cellar/ffmpeg",
            "Cellar/ffmpeg/9.0",
            "Cellar/ffmpeg/9.0/bin",
        ] {
            chmod(&directory.path().join(part), 0o755);
        }
        executable(&cellar.join("ffmpeg"));
        executable(&cellar.join("ffprobe"));
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        chmod(&bin, 0o755);
        symlink("../Cellar/ffmpeg/9.0/bin/ffmpeg", bin.join("ffmpeg")).unwrap();
        symlink(cellar.join("ffprobe"), bin.join("ffprobe")).unwrap();
        let real = std::fs::canonicalize(cellar.join("ffmpeg")).unwrap();

        let tool = trust(&bin.join("ffmpeg")).unwrap();
        assert_eq!(std::fs::canonicalize(tool.path()).unwrap(), real);
        assert_eq!(tool.named(), bin.join("ffmpeg"));
        tool.check().unwrap();
        let probe = tool.sibling("ffprobe").unwrap();
        assert_eq!(probe.named(), bin.join("ffprobe"));
        assert_eq!(
            tool.sibling("ffplay").unwrap_err(),
            Refusal::Missing,
            "nothing beside it"
        );

        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(cellar.join("ffmpeg"), b"#!/bin/sh\n# replaced\n").unwrap();
        assert_eq!(tool.check().unwrap_err(), Refusal::Changed);
    }

    /// What is refused: a relative path, nothing there, a directory, a file
    /// that is not executable or has a set-id bit, a file or directory others
    /// can write, a writable directory anywhere on the way (the link's own
    /// directory or the target's), a link loop; a sticky shared directory
    /// passes.
    #[test]
    fn anything_others_could_change_is_refused() {
        let directory = private_dir();
        let root = directory.path();
        assert_eq!(
            trust(Path::new("bin/ffmpeg")).unwrap_err(),
            Refusal::Relative
        );
        assert_eq!(trust(&root.join("absent")).unwrap_err(), Refusal::Missing);
        assert_eq!(
            trust(&root.join("absent/ffmpeg")).unwrap_err(),
            Refusal::Missing
        );

        let bin = root.join("bin");
        std::fs::create_dir(&bin).unwrap();
        chmod(&bin, 0o755);
        let tool = bin.join("ffmpeg");
        executable(&tool);
        trust(&tool).unwrap();
        for (mode, refusal) in [
            (0o644, Refusal::NotExecutable),
            (0o4755, Refusal::NotExecutable),
            (0o2755, Refusal::NotExecutable),
            (0o775, Refusal::Writable),
            (0o757, Refusal::Writable),
        ] {
            chmod(&tool, mode);
            assert_eq!(trust(&tool).unwrap_err(), refusal, "{mode:o}");
        }
        chmod(&tool, 0o755);
        assert_eq!(
            trust(&tool.join("more")).unwrap_err(),
            Refusal::Missing,
            "a file on the way"
        );

        chmod(&bin, 0o775);
        assert_eq!(trust(&tool).unwrap_err(), Refusal::Writable);
        chmod(&bin, 0o1777);
        trust(&tool).unwrap();
        chmod(&bin, 0o755);

        // macOS's admin group writes as root does: Homebrew's layout.
        #[cfg(target_os = "macos")]
        if std::os::unix::fs::chown(&bin, None, Some(80)).is_ok() {
            std::os::unix::fs::chown(&tool, None, Some(80)).unwrap();
            chmod(&bin, 0o775);
            chmod(&tool, 0o775);
            trust(&tool).unwrap();
            chmod(&bin, 0o777);
            assert_eq!(
                trust(&tool).unwrap_err(),
                Refusal::Writable,
                "not by everyone"
            );
            chmod(&bin, 0o755);
            chmod(&tool, 0o755);
        }

        // A link in a directory others can write, to a good binary.
        let shared = root.join("shared");
        std::fs::create_dir(&shared).unwrap();
        chmod(&shared, 0o777);
        symlink(&tool, shared.join("ffmpeg")).unwrap();
        assert_eq!(
            trust(&shared.join("ffmpeg")).unwrap_err(),
            Refusal::Writable
        );
        // A good link to a binary in a directory others can write.
        executable(&shared.join("real"));
        symlink(shared.join("real"), bin.join("through")).unwrap();
        assert_eq!(trust(&bin.join("through")).unwrap_err(), Refusal::Writable);

        symlink(bin.join("loop-b"), bin.join("loop-a")).unwrap();
        symlink(bin.join("loop-a"), bin.join("loop-b")).unwrap();
        assert_eq!(trust(&bin.join("loop-a")).unwrap_err(), Refusal::Unreadable);
        assert_eq!(
            trust(&bin).unwrap_err(),
            Refusal::NotExecutable,
            "a directory"
        );
    }

    /// The search looks only in its fixed places and the Nix profile under
    /// the home it is given, never `PATH`.
    #[test]
    fn the_search_finds_a_nix_profile_and_never_path() {
        let home = private_dir();
        let name = "kettle-test-decoder-that-is-nowhere-else";
        assert_eq!(search(name, Some(home.path())), None);
        let profile = home.path().join(".nix-profile/bin");
        std::fs::create_dir_all(&profile).unwrap();
        chmod(&home.path().join(".nix-profile"), 0o755);
        chmod(&profile, 0o755);
        executable(&profile.join(name));
        let found = search(name, Some(home.path())).unwrap();
        assert_eq!(found.named(), profile.join(name));
        assert_eq!(search(name, Some(Path::new("relative"))), None);
        assert_eq!(search(name, None), None);
    }
}
