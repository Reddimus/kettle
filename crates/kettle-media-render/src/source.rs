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
//!
//! A video can be far larger than anything read into memory, so a stills job
//! holds its file instead ([`hold`]): opened and checked the same way, its
//! first bytes read for classification, the rest read only by what decodes
//! it, through the same descriptor, its identity checked again afterwards.

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

impl Snapshot<'_> {
    /// Refuse this snapshot over `cap` as [`load`] would have: a file's is
    /// `FileTooLarge`, inline bytes' `TooLarge`.
    pub fn within(&self, cap: usize) -> Result<(), FailureCode> {
        match (self.bytes.len() <= cap, self.identity) {
            (true, _) => Ok(()),
            (false, Some(_)) => Err(FailureCode::FileTooLarge),
            (false, None) => Err(FailureCode::TooLarge),
        }
    }
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
    let (file, opened) = open_regular(path, attested)?;
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

/// Each admitted path, by the bytes the job names it by, and what opening it
/// gave.
type Admission = Vec<(Vec<u8>, Result<File, FailureCode>)>;

thread_local! {
    /// The files a worker admitted for its job before confining itself, by
    /// the path the job names each by, and what opening each gave. Once set,
    /// rendering on this thread opens nothing else: an admitted file is read
    /// through a fresh description of itself, a file that failed keeps its
    /// failure, and any other path is refused. Unset outside a worker, where
    /// paths open by name. Rendering never leaves the calling thread.
    static ADMITTED: std::cell::RefCell<Option<Admission>> =
        const { std::cell::RefCell::new(None) };
}

/// Admit the files `job` reads, opened and checked as rendering checks them
/// (its source, when it names a file, then its fallback fonts), so the
/// worker's sandbox can grant exactly these before a byte of any is read,
/// and rendering on this thread then reads these same files, whatever later
/// takes their names. A file that cannot be opened keeps its failure.
/// Returns another handle on each opened file, for the sandbox.
pub fn hold_inputs(job: &kettle_media::Job) -> Vec<File> {
    let source = match &job.source {
        Source::Path {
            path,
            authorization,
        } => {
            let attested = match authorization {
                Authorization::ExternalAttested(attested) => Some((attested.dev, attested.ino)),
                Authorization::UserPull(_) => None,
            };
            Some((path.as_bytes(), attested))
        }
        Source::Bytes(_) => None,
    };
    let fonts = job
        .fallback_fonts
        .iter()
        .map(|font| (font.path.as_bytes(), None));
    let mut admitted = Vec::new();
    let mut files = Vec::new();
    for (path, attested) in source.into_iter().chain(fonts) {
        let opened = open_by_name(Path::new(std::ffi::OsStr::from_bytes(path)), attested)
            .map(|(file, _)| file);
        if let Ok(copy) = opened.as_ref().map(File::try_clone) {
            match copy {
                Ok(copy) => files.push(copy),
                Err(_) => continue,
            }
        }
        admitted.push((path.to_vec(), opened));
    }
    ADMITTED.with(|cell| *cell.borrow_mut() = Some(admitted));
    files
}

/// What this thread's admission says about `path`: `None` outside a worker;
/// otherwise another handle on the admitted file (read from its start, as
/// every read here is), its recorded failure, or a refusal for a path the
/// job never named.
fn admitted(path: &Path) -> Option<Result<File, FailureCode>> {
    ADMITTED.with(|cell| {
        let admitted = cell.borrow();
        let admitted = admitted.as_ref()?;
        let entry = admitted
            .iter()
            .find(|(name, _)| name.as_slice() == path.as_os_str().as_bytes());
        Some(match entry {
            Some((_, Ok(file))) => file.try_clone().map_err(|_| FailureCode::Changed),
            Some((_, Err(failure))) => Err(*failure),
            None => Err(FailureCode::FileNotFound),
        })
    })
}

/// A reader over a file from its start that keeps its own position, so it
/// never moves, or depends on, the offset other handles on the same open file
/// share: an admitted file's handles are duplicates of one another.
struct FromStart<'a> {
    file: &'a File,
    at: u64,
}

impl<'a> FromStart<'a> {
    fn new(file: &'a File) -> Self {
        Self { file, at: 0 }
    }
}

impl std::io::Read for FromStart<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        use std::os::unix::fs::FileExt as _;
        let read = self.file.read_at(buffer, self.at)?;
        self.at += read as u64;
        Ok(read)
    }
}

/// `path` opened read-only, without waiting on a FIFO or becoming a
/// controlling terminal, and what the open descriptor says it is: a regular
/// file, the attested one when an attestation is given.
fn open_regular(
    path: &Path,
    attested: Option<(u64, u64)>,
) -> Result<(File, Metadata), FailureCode> {
    match admitted(path) {
        Some(file) => checked(file?, attested),
        None => open_by_name(path, attested),
    }
}

/// `path` opened by its name, as [`open_regular`] describes.
fn open_by_name(
    path: &Path,
    attested: Option<(u64, u64)>,
) -> Result<(File, Metadata), FailureCode> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .map_err(open_failure)?;
    checked(file, attested)
}

/// `file` and what it says it is, once it is a regular file, the attested
/// one when an attestation is given.
fn checked(file: File, attested: Option<(u64, u64)>) -> Result<(File, Metadata), FailureCode> {
    let opened = file.metadata().map_err(|_| FailureCode::FileNotFound)?;
    if !opened.file_type().is_file() {
        return Err(FailureCode::FileNotRegular);
    }
    if let Some(attested) = attested
        && attested != (opened.dev(), opened.ino())
    {
        return Err(FailureCode::Changed);
    }
    Ok((file, opened))
}

/// A stills job's source held for decoding: inline bytes, or an open
/// regular file with the identity it had when opened and its first bytes.
#[derive(Debug)]
pub enum Held<'a> {
    Bytes(&'a [u8]),
    File {
        file: File,
        identity: PathIdentity,
        prefix: Vec<u8>,
    },
}

/// Hold `source` for a stills job: a file is opened and checked as [`load`]
/// opens one, and its first `prefix_cap` bytes are read, nothing more.
pub fn hold(source: &Source, prefix_cap: usize) -> Result<Held<'_>, FailureCode> {
    match source {
        Source::Bytes(bytes) => Ok(Held::Bytes(bytes)),
        Source::Path {
            path,
            authorization,
        } => {
            let attested = match authorization {
                Authorization::ExternalAttested(attested) => Some((attested.dev, attested.ino)),
                Authorization::UserPull(_) => None,
            };
            let path = Path::new(std::ffi::OsStr::from_bytes(path.as_bytes()));
            let (file, opened) = open_regular(path, attested)?;
            let identity = identity(&opened)?;
            let cap = u64::try_from(prefix_cap).map_err(|_| FailureCode::TooLarge)?;
            let mut prefix = Vec::new();
            FromStart::new(&file)
                .take(cap)
                .read_to_end(&mut prefix)
                .map_err(|_| FailureCode::Changed)?;
            Ok(Held::File {
                file,
                identity,
                prefix,
            })
        }
    }
}

impl Held<'_> {
    /// The first bytes, for classification: inline bytes are all there.
    pub fn prefix(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::File { prefix, .. } => prefix,
        }
    }

    /// The source's size in bytes: inline bytes', or the file's when opened.
    pub fn size(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::File { identity, .. } => identity.size,
        }
    }

    /// The identity the file had when opened, `None` for inline bytes.
    pub fn identity(&self) -> Option<PathIdentity> {
        match self {
            Self::Bytes(_) => None,
            Self::File { identity, .. } => Some(*identity),
        }
    }

    /// Whether the file is still what was opened; inline bytes always are.
    /// A decoder that read it through the descriptor asks afterwards.
    pub fn unchanged(&self) -> Result<(), FailureCode> {
        match self {
            Self::Bytes(_) => Ok(()),
            Self::File { file, identity, .. } => {
                let now = file.metadata().map_err(|_| FailureCode::Changed)?;
                if self::identity(&now)? == *identity {
                    Ok(())
                } else {
                    Err(FailureCode::Changed)
                }
            }
        }
    }

    /// The whole source in memory, refused over `cap` as [`load`] refuses
    /// it, its file read from the start through the held descriptor and
    /// checked unchanged afterwards.
    pub fn snapshot(&self, cap: usize) -> Result<Snapshot<'_>, FailureCode> {
        match self {
            Self::Bytes(bytes) => {
                if bytes.len() > cap {
                    return Err(FailureCode::TooLarge);
                }
                Ok(Snapshot {
                    bytes: Cow::Borrowed(bytes),
                    identity: None,
                })
            }
            Self::File { file, identity, .. } => {
                let cap_bytes = u64::try_from(cap).map_err(|_| FailureCode::TooLarge)?;
                if identity.size > cap_bytes {
                    return Err(FailureCode::FileTooLarge);
                }
                let mut bytes = Vec::new();
                FromStart::new(file)
                    .take(cap_bytes.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|_| FailureCode::Changed)?;
                self.unchanged()?;
                if bytes.len() as u64 != identity.size {
                    return Err(FailureCode::Changed);
                }
                Ok(Snapshot {
                    bytes: Cow::Owned(bytes),
                    identity: Some(*identity),
                })
            }
        }
    }
}

/// Up to `cap + 1` bytes: one more than allowed shows the file is too large
/// without reading the rest.
fn read_capped(file: &File, cap: u64) -> Result<Vec<u8>, FailureCode> {
    let mut bytes = Vec::new();
    FromStart::new(file)
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| FailureCode::Changed)?;
    Ok(bytes)
}

fn identity(metadata: &Metadata) -> Result<PathIdentity, FailureCode> {
    PathIdentity::of(metadata).ok_or(FailureCode::Changed)
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

    /// What a worker admitted for its job is what rendering reads, from its
    /// start and with its own offset each time, even once another file has
    /// taken its name. A file that could not be opened keeps that failure,
    /// even once its name leads to an admitted file, and a path the job never
    /// named is refused.
    #[test]
    fn rendering_reads_only_what_was_admitted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("held.svg");
        let font = directory.path().join("font.ttf");
        let missing = directory.path().join("missing.ttf");
        let other = directory.path().join("other.svg");
        std::fs::write(&path, b"held bytes").unwrap();
        std::fs::write(&font, b"font bytes").unwrap();
        std::fs::write(&other, b"bytes that took its name").unwrap();
        let native = |path: &Path| kettle_media::NativePath::from_path(path).unwrap();
        let fallback = |path: &Path| kettle_media::FallbackFont {
            path: native(path),
            face_index: 0,
        };
        let job = kettle_media::Job {
            kind: kettle_media::JobKind::Svg,
            source: Source::Path {
                path: native(&path),
                authorization: user_pull(),
            },
            theme: kettle_media::Theme {
                background: [0; 4],
                foreground: [255; 4],
                palette: [[0; 4]; 16],
                accent: [0; 4],
                is_dark: true,
            },
            canvas: kettle_media::Canvas::Theme,
            target: kettle_media::Target {
                width: 1,
                height: 1,
                scale: 1.0,
                crop: None,
            },
            fallback_fonts: vec![fallback(&font), fallback(&missing)],
        };
        let files = hold_inputs(&job);
        assert_eq!(files.len(), 2, "the source and the font that opened");
        std::fs::rename(&other, &path).unwrap();
        for _ in 0..2 {
            let snapshot = load_path(&path, &user_pull(), 1024, || {}).unwrap();
            assert_eq!(&*snapshot.bytes, b"held bytes");
        }
        std::os::unix::fs::symlink(&font, &missing).unwrap();
        assert_eq!(
            load_path(&missing, &user_pull(), 1024, || {}).unwrap_err(),
            FailureCode::FileNotFound,
            "its failure, not the admitted font its name now leads to"
        );
        let fresh = directory.path().join("fresh.svg");
        std::fs::write(&fresh, b"never named").unwrap();
        assert_eq!(
            load_path(&fresh, &user_pull(), 1024, || {}).unwrap_err(),
            FailureCode::FileNotFound
        );
        ADMITTED.with(|cell| cell.borrow_mut().take());
        let snapshot = load_path(&fresh, &user_pull(), 1024, || {}).unwrap();
        assert_eq!(
            &*snapshot.bytes, b"never named",
            "outside a worker, by name"
        );
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

    /// A held file reads its first bytes only, then whole through the same
    /// descriptor, whatever is renamed over its path, and is changed once it
    /// is written in place; inline bytes are their own prefix and never
    /// change. Either over the cap is refused as `load` refuses it.
    #[test]
    fn a_held_file_reads_through_its_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clip.mp4");
        std::fs::write(&path, b"0123456789").unwrap();
        let source = Source::Path {
            path: kettle_media::NativePath::from_path(&path).unwrap(),
            authorization: user_pull(),
        };
        let held = hold(&source, 4).unwrap();
        assert_eq!((held.prefix(), held.size()), (&b"0123"[..], 10));
        let other = directory.path().join("other.mp4");
        std::fs::write(&other, b"replacement").unwrap();
        std::fs::rename(&other, &path).unwrap();
        assert_eq!(&*held.snapshot(64).unwrap().bytes, b"0123456789");
        assert_eq!(held.snapshot(9).unwrap_err(), FailureCode::FileTooLarge);
        held.unchanged().unwrap();
        let held = hold(&source, 64).unwrap();
        {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(b"!").unwrap();
        }
        assert_eq!(held.unchanged().unwrap_err(), FailureCode::Changed);
        assert_eq!(held.snapshot(64).unwrap_err(), FailureCode::Changed);
        // Rewritten in place at the same size: the identity tells.
        let held = hold(&source, 64).unwrap();
        let size = held.size() as usize;
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&path, vec![b'x'; size]).unwrap();
        assert_eq!(held.snapshot(64).unwrap_err(), FailureCode::Changed);
        let bytes = Source::Bytes(b"GIF89a".to_vec());
        let inline = hold(&bytes, 2).unwrap();
        assert_eq!((inline.prefix(), inline.identity()), (&b"GIF89a"[..], None));
        assert_eq!(inline.snapshot(5).unwrap_err(), FailureCode::TooLarge);
        assert_eq!(
            hold(
                &Source::Path {
                    path: kettle_media::NativePath::from_path(directory.path()).unwrap(),
                    authorization: user_pull(),
                },
                4
            )
            .unwrap_err(),
            FailureCode::FileNotRegular
        );
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
