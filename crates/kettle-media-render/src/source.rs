//! A job's source, read once into memory through one held descriptor.
//!
//! A path is opened read-only and non-blocking (so a FIFO cannot hang the
//! worker), then everything is decided from that descriptor, never from the
//! path again: it must be a regular file, an external request's attested
//! device and inode must match it, it is read to at most the cap plus one
//! byte, and its identity is read again afterwards. A file that grew, shrank,
//! was rewritten or was replaced in place while it was read is `Changed`;
//! renaming another file over the path cannot redirect the descriptor. A
//! symbolic link at the leaf is followed, as opening a file the user named
//! would. This does not make the file a snapshot: a same-size rewrite that
//! restores its modification time between the two checks is not seen.

use std::borrow::Cow;
use std::fs::{File, Metadata, OpenOptions};
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

use kettle_media::{Authorization, FailureCode, PathIdentity, Source};

/// A source in memory (inline bytes are borrowed, not copied), with the
/// identity of the file it was read from.
#[derive(Debug)]
pub struct Snapshot<'a> {
    pub bytes: Cow<'a, [u8]>,
    pub identity: Option<PathIdentity>,
}

/// Read `source`, refusing more than `cap` bytes.
pub fn load(source: &Source, cap: usize) -> Result<Snapshot<'_>, FailureCode> {
    match source {
        Source::Bytes(bytes) => {
            if bytes.len() > cap {
                return Err(FailureCode::TooLarge);
            }
            Ok(Snapshot {
                bytes: Cow::Borrowed(bytes),
                identity: None,
            })
        }
        Source::Path {
            path,
            authorization,
        } => load_path(
            Path::new(std::ffi::OsStr::from_bytes(path.as_bytes())),
            authorization,
            cap,
            || {},
        ),
    }
}

/// [`load`] for a path, with `after_open` run between the open and the read
/// (for tests that change the file there).
pub(crate) fn load_path(
    path: &Path,
    authorization: &Authorization,
    cap: usize,
    after_open: impl FnOnce(),
) -> Result<Snapshot<'static>, FailureCode> {
    let attested = match authorization {
        Authorization::ExternalAttested(attested) => Some((attested.dev, attested.ino)),
        Authorization::UserPull(_) => None,
    };
    load_regular_path(path, attested, cap, after_open)
}

/// A named regular file, optionally requiring the source's external
/// attestation. Font entries have no attestation field; they use the same
/// held-descriptor, non-blocking read and before/after identity checks.
pub(crate) fn load_regular_path(
    path: &Path,
    attested: Option<(u64, u64)>,
    cap: usize,
    after_open: impl FnOnce(),
) -> Result<Snapshot<'static>, FailureCode> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .map_err(open_failure)?;
    let opened = file.metadata().map_err(|_| FailureCode::FileNotFound)?;
    if !opened.file_type().is_file() {
        return Err(FailureCode::FileNotRegular);
    }
    if let Some(attested) = attested
        && attested != (opened.dev(), opened.ino())
    {
        return Err(FailureCode::Changed);
    }
    let cap_bytes = u64::try_from(cap).map_err(|_| FailureCode::TooLarge)?;
    if opened.len() > cap_bytes {
        return Err(FailureCode::FileTooLarge);
    }
    after_open();
    let bytes = read_capped(&file, cap_bytes)?;
    if bytes.len() > cap {
        return Err(FailureCode::FileTooLarge);
    }
    let reread = file.metadata().map_err(|_| FailureCode::Changed)?;
    let identity = identity(&reread)?;
    let read = u64::try_from(bytes.len()).map_err(|_| FailureCode::TooLarge)?;
    if identity != self::identity(&opened)? || read != reread.len() {
        return Err(FailureCode::Changed);
    }
    Ok(Snapshot {
        bytes: Cow::Owned(bytes),
        identity: Some(identity),
    })
}

/// Up to `cap + 1` bytes: one more than allowed shows the file is too large
/// without reading the rest.
fn read_capped(file: &File, cap: u64) -> Result<Vec<u8>, FailureCode> {
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| FailureCode::Changed)?;
    Ok(bytes)
}

fn identity(metadata: &Metadata) -> Result<PathIdentity, FailureCode> {
    Ok(PathIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        size: metadata.len(),
        mtime_seconds: metadata.mtime(),
        mtime_nanos: u32::try_from(metadata.mtime_nsec()).map_err(|_| FailureCode::Changed)?,
    })
}

fn open_failure(error: std::io::Error) -> FailureCode {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => FailureCode::FilePermission,
        _ => FailureCode::FileNotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_media::ExternalAttested;

    fn user_pull() -> Authorization {
        // The source's own constructor for a GUI action; tests build the
        // authorization the private route carries.
        match Source::user_pull(
            kettle_media::NativePath::new(b"/x".to_vec()).unwrap(),
            kettle_media::GuiActionWitness::from_explicit_gui_action(),
        ) {
            Source::Path { authorization, .. } => authorization,
            Source::Bytes(_) => unreachable!("a user pull is a path"),
        }
    }

    #[test]
    fn path_replacement_never_switches_held_handle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.png");
        let other = directory.path().join("other.png");
        std::fs::write(&path, b"first").unwrap();
        std::fs::write(&other, b"second, replacing it").unwrap();
        let snapshot = load_path(&path, &user_pull(), 1024, || {
            std::fs::rename(&other, &path).unwrap();
        })
        .unwrap();
        assert_eq!(&*snapshot.bytes, b"first");
    }

    #[test]
    fn in_place_mutation_is_changed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.png");
        std::fs::write(&path, b"first").unwrap();
        // Grown between the open and the read.
        assert_eq!(
            load_path(&path, &user_pull(), 1024, || {
                use std::io::Write as _;
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap();
                file.write_all(b" and more").unwrap();
            })
            .unwrap_err(),
            FailureCode::Changed
        );
    }

    #[test]
    fn size_at_open_over_cap_is_refused_before_reading() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.png");
        std::fs::write(&path, vec![0; 64]).unwrap();
        // Shrunk after the open: the size the open handle reported decides,
        // and nothing is read.
        let mut ran = false;
        assert_eq!(
            load_path(&path, &user_pull(), 32, || {
                ran = true;
                std::fs::write(&path, b"small").unwrap();
            })
            .unwrap_err(),
            FailureCode::FileTooLarge
        );
        assert!(!ran);
    }

    #[test]
    fn external_attestation_must_match_open_handle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.png");
        std::fs::write(&path, b"content").unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let matching = Authorization::ExternalAttested(ExternalAttested {
            dev: metadata.dev(),
            ino: metadata.ino(),
        });
        assert!(load_path(&path, &matching, 1024, || {}).is_ok());
        let other = Authorization::ExternalAttested(ExternalAttested {
            dev: metadata.dev(),
            ino: metadata.ino() + 1,
        });
        assert_eq!(
            load_path(&path, &other, 1024, || {}).unwrap_err(),
            FailureCode::Changed
        );
    }
}
