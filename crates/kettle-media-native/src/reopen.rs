//! A new open file description for the file the renderer holds: the same
//! file, with an offset of its own, so a decoder reading it shares nothing
//! with another, and checked to be the file that was held. Linux reopens the
//! descriptor itself through `/proc/self/fd`; macOS asks the descriptor for
//! its current path (which follows a rename) and opens that, so a file
//! replaced at that path in between is refused, never read.

use std::fs::File;

use kettle_media::{FailureCode, PathIdentity};

/// `file` opened again, refused as `Changed` unless it is the regular file
/// `identity` describes.
pub(crate) fn reopen(file: &File, identity: PathIdentity) -> Result<File, FailureCode> {
    let reopened = open_again(file).ok_or(FailureCode::Changed)?;
    let now = reopened.metadata().map_err(|_| FailureCode::Changed)?;
    if !now.is_file() || PathIdentity::of(&now) != Some(identity) {
        return Err(FailureCode::Changed);
    }
    Ok(reopened)
}

#[cfg(target_os = "linux")]
fn open_again(file: &File) -> Option<File> {
    use std::os::fd::AsRawFd as _;
    File::open(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()
}

#[cfg(target_os = "macos")]
fn open_again(file: &File) -> Option<File> {
    use std::ffi::{CStr, OsStr};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut buffer = [0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes a NUL-terminated path of at most MAXPATHLEN
    // (PATH_MAX) bytes into `buffer`, which is that long and outlives the
    // call.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } == -1 {
        return None;
    }
    let path = CStr::from_bytes_until_nul(&buffer).ok()?;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(OsStr::from_bytes(path.to_bytes()))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Seek as _, SeekFrom};

    fn identity(file: &File) -> PathIdentity {
        PathIdentity::of(&file.metadata().unwrap()).unwrap()
    }

    /// A reopened file has its own offset and follows the held file through
    /// a rename; a file replaced or rewritten since it was held is refused.
    #[test]
    fn a_reopened_file_is_the_held_one_with_its_own_offset() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clip.mp4");
        std::fs::write(&path, b"0123456789").unwrap();
        let mut held = File::open(&path).unwrap();
        let was = identity(&held);
        held.seek(SeekFrom::Start(6)).unwrap();
        let mut again = reopen(&held, was).unwrap();
        let mut text = String::new();
        again.read_to_string(&mut text).unwrap();
        assert_eq!(text, "0123456789", "from its own start");
        assert_eq!(held.stream_position().unwrap(), 6, "the held offset kept");

        let moved = directory.path().join("moved.mp4");
        std::fs::rename(&path, &moved).unwrap();
        reopen(&held, was).unwrap();

        std::fs::write(directory.path().join("other.mp4"), b"9876543210").unwrap();
        std::fs::rename(directory.path().join("other.mp4"), &moved).unwrap();
        if cfg!(target_os = "macos") {
            // The held file is unlinked; its old path is someone else's.
            assert_eq!(reopen(&held, was).unwrap_err(), FailureCode::Changed);
        }

        let path = directory.path().join("edited.mp4");
        std::fs::write(&path, b"0123456789").unwrap();
        let held = File::open(&path).unwrap();
        let was = identity(&held);
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&path, b"abcdefghij").unwrap();
        assert_eq!(reopen(&held, was).unwrap_err(), FailureCode::Changed);
    }
}
