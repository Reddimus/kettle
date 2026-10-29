//! Descriptor-relative removal of a directory tree the caller already holds.
//!
//! `std::fs::remove_dir_all` resolves its first component by pathname. In a
//! directory other users can write, such as `/Applications`, someone can swap a
//! different tree in under that name between an ownership check and the delete,
//! and the delete then runs against their tree with the caller's rights. Here
//! the caller passes the descriptor it took when it established ownership, and
//! every name below it is resolved relative to an open descriptor, so a rename
//! of any ancestor cannot redirect the walk.

use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;

/// Bounds for [`remove_directory_tree_at`].
///
/// A tree that exceeds either bound is left partly removed and reported as an
/// error, rather than walked without end. Callers size these for the trees they
/// create, with headroom.
#[derive(Clone, Copy, Debug)]
pub struct TreeRemovalLimits {
    /// Directory levels below the held directory that may be entered.
    pub max_depth: usize,
    /// Directory entries, at every level combined, that may be removed.
    pub max_entries: usize,
}

/// Remove `name` in `parent` and everything under it, where `held` is an open
/// descriptor for that same directory.
///
/// The contents are removed through `held`, so they are the contents of the
/// directory the caller opened even if the name has since been moved. Children
/// are opened with `O_NOFOLLOW`, checked to be the entry that was listed, and
/// never followed onto another device. The final `rmdir` of `name` happens only
/// if `name` still refers to `held`; otherwise the emptied directory is left
/// where it is and an error is returned. The one remaining window is between
/// that last check and the `rmdir`, and `rmdir` can only remove an empty
/// directory.
///
/// # Errors
///
/// Any failure to list, open or remove an entry stops the walk. A limit in
/// `limits` being reached returns [`io::ErrorKind::InvalidData`]; an entry
/// that changed identity during the walk, or a `name` that no longer refers to
/// `held`, returns [`io::ErrorKind::Other`].
pub fn remove_directory_tree_at(
    parent: &File,
    name: &OsStr,
    held: &File,
    limits: TreeRemovalLimits,
) -> io::Result<()> {
    let root = held.metadata()?;
    if !root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the held descriptor is not a directory",
        ));
    }
    let mut remaining = limits.max_entries;
    remove_contents(held, root.dev(), 0, limits.max_depth, &mut remaining)?;

    let name = c_string(name)?;
    if identity(&stat_at(parent, &name)?) != (root.dev(), root.ino()) {
        return Err(io::Error::other(
            "the directory name no longer refers to the held directory",
        ));
    }
    unlink_at(parent, &name, libc::AT_REMOVEDIR)
}

/// Whether an open directory has no entries besides `.` and `..`.
///
/// Reads through the descriptor, so the answer is about the directory the
/// caller holds, not whatever its pathname now names.
pub fn directory_is_empty(directory: &File) -> io::Result<bool> {
    let mut stream = DirectoryStream::open(directory)?;
    Ok(stream.next_name()?.is_none())
}

fn remove_contents(
    directory: &File,
    device: u64,
    depth: usize,
    max_depth: usize,
    remaining: &mut usize,
) -> io::Result<()> {
    // Collect first, then remove. Unlinking while a stream is open leaves
    // whether later entries appear unspecified, and the list is bounded by the
    // same entry budget the removal spends.
    let mut names = Vec::new();
    {
        let mut stream = DirectoryStream::open(directory)?;
        while let Some(name) = stream.next_name()? {
            if names.len() >= *remaining {
                return Err(limit_error("too many entries"));
            }
            names.push(name);
        }
    }
    for name in names {
        // Children spend the same budget, so it can run out partway through
        // a listing that fitted when it was read.
        *remaining = remaining
            .checked_sub(1)
            .ok_or_else(|| limit_error("too many entries"))?;
        let listed = stat_at(directory, &name)?;
        if listed.st_mode & libc::S_IFMT != libc::S_IFDIR {
            unlink_at(directory, &name, 0)?;
            continue;
        }
        if depth >= max_depth {
            return Err(limit_error("the tree is too deep"));
        }
        let listed = identity(&listed);
        if listed.0 != device {
            return Err(io::Error::other("refusing to descend onto another device"));
        }
        let child = open_directory_at(directory, &name)?;
        let opened = child.metadata()?;
        if (opened.dev(), opened.ino()) != listed {
            return Err(io::Error::other(
                "a directory changed while it was being removed",
            ));
        }
        remove_contents(&child, device, depth + 1, max_depth, remaining)?;
        drop(child);
        unlink_at(directory, &name, libc::AT_REMOVEDIR)?;
    }
    Ok(())
}

/// `(device, inode)` as `std::fs::Metadata` reports them.
// dev_t is i32 on Darwin and u64 on Linux, so the cast is needed on one and
// redundant on the other.
#[allow(clippy::unnecessary_cast)]
fn identity(stat: &libc::stat) -> (u64, u64) {
    (stat.st_dev as u64, stat.st_ino as u64)
}

fn limit_error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

fn c_string(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn stat_at(directory: &File, name: &CStr) -> io::Result<libc::stat> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `directory` is an open descriptor, `name` is NUL-terminated, and
    // `stat` is valid writable storage for one `struct stat`.
    let status = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatat succeeded, so it initialized `stat`.
    Ok(unsafe { stat.assume_init() })
}

fn unlink_at(directory: &File, name: &CStr, flags: libc::c_int) -> io::Result<()> {
    // SAFETY: `directory` is an open descriptor and `name` is NUL-terminated.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn open_directory_at(directory: &File, name: &CStr) -> io::Result<File> {
    // SAFETY: `directory` is an open descriptor and `name` is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor that nothing else owns.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// A `readdir` stream over its own descriptor for an already-open directory.
struct DirectoryStream(*mut libc::DIR);

impl DirectoryStream {
    fn open(directory: &File) -> io::Result<Self> {
        // `fdopendir` takes ownership of its descriptor and shares the file
        // offset with any duplicate, so open "." afresh to get a readable
        // descriptor with its own offset. Linux also needs this for O_PATH
        // descriptors, which fdopendir cannot read.
        let fd = open_directory_at(directory, c".")?.into_raw_fd();
        // SAFETY: `fd` is an open directory descriptor we own; fdopendir takes
        // it over on success.
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: fdopendir did not take the descriptor on failure.
            unsafe { libc::close(fd) };
            return Err(error);
        }
        Ok(Self(stream))
    }

    /// The next name other than `.` and `..`, or `None` at the end.
    fn next_name(&mut self) -> io::Result<Option<CString>> {
        loop {
            // readdir reports errors only through errno, so clear it first.
            set_errno(0);
            // SAFETY: the stream is open until drop; the entry it returns is
            // read before the next call.
            let entry = unsafe { libc::readdir(self.0) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                return match error.raw_os_error() {
                    Some(0) | None => Ok(None),
                    Some(_) => Err(error),
                };
            }
            // SAFETY: d_name is NUL-terminated within the entry.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            return Ok(Some(name.to_owned()));
        }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: the stream is owned here and closed exactly once.
        unsafe { libc::closedir(self.0) };
    }
}

fn set_errno(value: libc::c_int) {
    // SAFETY: the errno location is valid for the calling thread.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    unsafe {
        *libc::__error() = value;
    }
    // SAFETY: the errno location is valid for the calling thread.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe {
        *libc::__errno_location() = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::ffi::OsStringExt as _;
    use std::path::Path;

    /// Names in an open directory, for tests that must see what a walk left.
    fn names_in(directory: &File) -> io::Result<Vec<OsString>> {
        let mut stream = DirectoryStream::open(directory)?;
        let mut names = Vec::new();
        while let Some(name) = stream.next_name()? {
            names.push(OsString::from_vec(name.into_bytes()));
        }
        names.sort();
        Ok(names)
    }

    const LIMITS: TreeRemovalLimits = TreeRemovalLimits {
        max_depth: 8,
        max_entries: 64,
    };

    fn open_dir(path: &Path) -> File {
        use std::os::unix::fs::OpenOptionsExt as _;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
            .unwrap()
    }

    #[test]
    fn removes_a_nested_tree_without_following_links_out_of_it() {
        let root = crate::test_tempdir();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"keep").unwrap();

        let tree = root.path().join("tree");
        fs::create_dir_all(tree.join("a/b/c")).unwrap();
        fs::write(tree.join("a/b/c/file"), b"x").unwrap();
        fs::write(tree.join("top"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("a/link")).unwrap();

        let parent = open_dir(root.path());
        let held = open_dir(&tree);
        remove_directory_tree_at(&parent, OsStr::new("tree"), &held, LIMITS).unwrap();

        assert!(!tree.exists());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"keep");
    }

    /// The name was moved aside and an empty directory left in its place: the
    /// held tree is emptied, and the stranger's directory is not removed.
    #[test]
    fn leaves_a_directory_that_replaced_the_name() {
        let root = crate::test_tempdir();
        let tree = root.path().join("tree");
        fs::create_dir_all(tree.join("inner")).unwrap();
        fs::write(tree.join("inner/file"), b"x").unwrap();
        let held = open_dir(&tree);
        fs::rename(&tree, root.path().join("moved")).unwrap();
        fs::create_dir(&tree).unwrap();

        let parent = open_dir(root.path());
        let error = remove_directory_tree_at(&parent, OsStr::new("tree"), &held, LIMITS)
            .expect_err("a replaced name is refused");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(tree.is_dir(), "the replacement directory survives");
        assert!(
            names_in(&held).unwrap().is_empty(),
            "the held tree is emptied"
        );
    }

    #[test]
    fn stops_at_the_entry_limit() {
        let root = crate::test_tempdir();
        let tree = root.path().join("tree");
        fs::create_dir(&tree).unwrap();
        for index in 0..5 {
            fs::write(tree.join(format!("f{index}")), b"x").unwrap();
        }
        let parent = open_dir(root.path());
        let held = open_dir(&tree);
        let limits = TreeRemovalLimits {
            max_depth: 8,
            max_entries: 4,
        };
        let error = remove_directory_tree_at(&parent, OsStr::new("tree"), &held, limits)
            .expect_err("five entries exceed a limit of four");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(tree.is_dir());
    }

    /// Entries spent inside a subdirectory count against the same budget as
    /// the siblings still waiting in the parent's listing.
    #[test]
    fn stops_at_the_entry_limit_across_nested_directories() {
        let root = crate::test_tempdir();
        let tree = root.path().join("tree");
        for child in ["a", "b"] {
            fs::create_dir_all(tree.join(child)).unwrap();
            fs::write(tree.join(child).join("file"), b"x").unwrap();
        }
        let parent = open_dir(root.path());
        let held = open_dir(&tree);
        let limits = TreeRemovalLimits {
            max_depth: 8,
            max_entries: 2,
        };
        let error = remove_directory_tree_at(&parent, OsStr::new("tree"), &held, limits)
            .expect_err("four entries exceed a limit of two");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn stops_at_the_depth_limit() {
        let root = crate::test_tempdir();
        let tree = root.path().join("tree");
        fs::create_dir_all(tree.join("1/2/3")).unwrap();
        let parent = open_dir(root.path());
        let held = open_dir(&tree);
        let limits = TreeRemovalLimits {
            max_depth: 2,
            max_entries: 64,
        };
        let error = remove_directory_tree_at(&parent, OsStr::new("tree"), &held, limits)
            .expect_err("three levels exceed a depth of two");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(tree.join("1/2/3").is_dir());
    }

    #[test]
    fn reports_emptiness_of_the_held_directory() {
        let root = crate::test_tempdir();
        let tree = root.path().join("tree");
        fs::create_dir(&tree).unwrap();
        let held = open_dir(&tree);
        assert!(directory_is_empty(&held).unwrap());
        fs::write(tree.join("child"), b"x").unwrap();
        assert!(!directory_is_empty(&held).unwrap());
    }
}
