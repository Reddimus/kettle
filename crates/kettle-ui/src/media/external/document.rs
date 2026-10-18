//! Opening a shown PDF in the permitted document viewer, from a private copy
//! of the bytes Kettle showed.
//!
//! The viewer never gets the file a program named. A PDF shown from a file
//! is opened again, as the worker opened it, and checked to be the one
//! shown by its identity; it is copied into the private document store
//! ([`crate::paste_image::DOCUMENTS`]), by a clone where the file system
//! makes one, and the copy is checked to be what was shown: its digest,
//! taken over every byte, is the one its pages were rendered under, and the
//! source did not change while it was copied. A PDF shown from bytes a
//! request carried is written from the bytes Kettle still holds and checked
//! the same way. The copy is named `.pdf` and marked downloaded (macOS
//! quarantine). The viewer is checked to be the one this platform permits
//! before anything is copied, and the copy is checked to be still Kettle's
//! just before the viewer gets it. Nothing here parses PDF: its bytes are
//! only compared.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use kettle_media::{Digest, MAX_PDF_BYTES, PathIdentity, content_digest, content_digest_read};

use super::video::{identity, native_path, open_read_only};
use super::{OpenFailure, PrivateCopies, Slot, Viewer, copy_failure, private_copy};
use crate::media::source::ChargedBytes;
use crate::media::{ShelfItem, SourceInput};

/// How long a copy may take before it is given up: a byte copy of the
/// largest PDF, and reading it back to check it, from a slow drive.
const COPY_DEADLINE: Duration = Duration::from_secs(30);

/// Papers, GNOME's document viewer from GNOME 47 (Ubuntu 25.10 and later).
const PAPERS: &str = "/usr/bin/papers";
/// Evince, GNOME's document viewer before Papers (Ubuntu 24.04 and
/// earlier).
const EVINCE: &str = "/usr/bin/evince";

/// The document viewer this platform permits. Each is found only on its own
/// platform, and never by the user's default association.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reader {
    /// macOS: Preview, under Apple's own signature.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Preview,
    /// Linux: Papers, a root-owned program nobody else can change.
    /// Ubuntu 25.10 and later have it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Papers,
    /// Linux: Evince, where Papers is missing, trusted the same way.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Evince,
}

impl Reader {
    /// The permitted viewer this platform has, by a quick look once per
    /// process; each open checks it fully before it copies anything.
    pub(crate) fn find() -> Option<Self> {
        static FOUND: OnceLock<Option<Reader>> = OnceLock::new();
        *FOUND.get_or_init(Self::look)
    }

    fn look() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            Path::new(super::PREVIEW_APP)
                .is_dir()
                .then_some(Self::Preview)
        }
        #[cfg(target_os = "linux")]
        {
            linux_reader(
                super::trusted_program(Path::new(PAPERS)),
                super::trusted_program(Path::new(EVINCE)),
            )
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }

    /// The menu row that opens a PDF in this viewer.
    pub(crate) fn label(self, tr: &kettle_i18n::Translator) -> &'static str {
        tr.text(match self {
            Self::Preview => kettle_i18n::Text::MenuOpenInPreview,
            Self::Papers | Self::Evince => kettle_i18n::Text::MenuOpenInDocumentViewer,
        })
    }

    /// Whether the viewer is still the one permitted: Preview signed by
    /// Apple as itself, or a root-owned system viewer on Linux.
    fn verified(self) -> bool {
        match self {
            Self::Preview => Viewer::Preview.verified(),
            #[cfg(target_os = "linux")]
            Self::Papers => super::trusted_program(Path::new(PAPERS)),
            #[cfg(target_os = "linux")]
            Self::Evince => super::trusted_program(Path::new(EVINCE)),
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }

    /// The command that opens the copy at `file`, an absolute path: a fixed
    /// program and arguments, no shell and no search path. Papers and Evince
    /// are non-unique applications (`G_APPLICATION_NON_UNIQUE`), so a launch
    /// never forwards its file to one already running: the program Kettle
    /// checked is the one that opens it.
    fn command(self, file: &Path) -> std::process::Command {
        debug_assert!(file.is_absolute(), "a leading `/` is never an option");
        let program = match self {
            Self::Preview => return Viewer::Preview.command(file),
            Self::Papers => PAPERS,
            Self::Evince => EVINCE,
        };
        let mut command = std::process::Command::new(program);
        command
            .arg(file)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
    }

    /// Start this viewer on the kept copy at `path`.
    fn start(self, path: &Path) -> Result<Child, OpenFailure> {
        self.command(path).spawn().map_err(|_| OpenFailure::Viewer)
    }
}

/// Papers where the system has it, else Evince.
#[cfg(any(target_os = "linux", all(test, unix)))]
fn linux_reader(papers: bool, evince: bool) -> Option<Reader> {
    if papers {
        Some(Reader::Papers)
    } else {
        evince.then_some(Reader::Evince)
    }
}

/// What opening a shown PDF needs: where its bytes are, and the digest its
/// pages were rendered under.
#[derive(Clone, Debug)]
pub(crate) enum DocumentSource {
    /// The file it was shown from.
    File { path: PathBuf, digest: Digest },
    /// The bytes a request carried, still held.
    Bytes {
        bytes: Arc<ChargedBytes>,
        digest: Digest,
    },
}

impl DocumentSource {
    /// What opening `item` needs, when it is a PDF whose file Kettle can
    /// name or whose bytes it still holds.
    pub(crate) fn of(item: &ShelfItem) -> Option<Self> {
        if item.kind != crate::media::ItemKind::Media(kettle_media::MediaKind::Pdf) {
            return None;
        }
        let digest = item.source.digest.clone();
        match &item.source.spec.input {
            SourceInput::Path { path, .. } => Some(Self::File {
                path: native_path(path)?,
                digest,
            }),
            SourceInput::Bytes(bytes) => Some(Self::Bytes {
                bytes: Arc::clone(bytes),
                digest,
            }),
            SourceInput::Released => None,
        }
    }
}

/// Open a private copy of `document` in `reader`, on a short thread.
/// `failed` hears why, if it does not open; it runs on that thread.
pub(crate) fn open(
    copies: Arc<PrivateCopies>,
    document: DocumentSource,
    reader: Reader,
    failed: impl FnOnce(OpenFailure) + Send + 'static,
) {
    let Some(slot) = Slot::take() else {
        failed(OpenFailure::Busy);
        return;
    };
    // A thread that cannot start drops the slot with its closure.
    let _ = std::thread::Builder::new()
        .name("kettle-ui-open-document".into())
        .spawn(move || match hand_over(&copies, &document, reader) {
            Ok(child) => {
                // The slot bounds the work of opening, not how long the
                // viewer stays up: Papers runs until the user closes it.
                drop(slot);
                super::watch(child, failed);
            }
            Err(why) => {
                drop(slot);
                failed(why);
            }
        });
}

/// Check the viewer, copy the PDF, and hand the viewer the copy. A copy
/// that does not reach the viewer is deleted.
fn hand_over(
    copies: &PrivateCopies,
    document: &DocumentSource,
    reader: Reader,
) -> Result<Child, OpenFailure> {
    if !reader.verified() {
        return Err(OpenFailure::Viewer);
    }
    let path = copy(copies, document)?;
    let mut store = copies.lock();
    // Exit may have begun while the copy was made, and a copy must still be
    // Kettle's. Started while the store is held, so the copy cannot be
    // dropped for a newer one before the viewer is on its way to it.
    let started = if store.path_still_matches(&path) && !copies.closing() {
        reader.start(&path)
    } else {
        Err(OpenFailure::Copy)
    };
    if started.is_err() {
        let _ = store.remove(&path);
    }
    started
}

/// Make the private copy of `document` ([`private_copy`]): from the file it
/// was shown from, held open and checked to be that file, or from the bytes
/// still held, checked to be those shown; kept once its every byte is what
/// was shown.
fn copy(copies: &PrivateCopies, document: &DocumentSource) -> Result<PathBuf, OpenFailure> {
    let deadline = Instant::now() + COPY_DEADLINE;
    match document {
        DocumentSource::File { path, digest } => {
            let shown = digest.path_identity.ok_or(OpenFailure::Changed)?;
            let file = open_read_only(path).map_err(|_| OpenFailure::Changed)?;
            let opened = file.metadata().map_err(|_| OpenFailure::Changed)?;
            // A file past what Kettle shows was never the one shown.
            if !opened.is_file()
                || identity(&opened) != Some(shown)
                || shown.size > MAX_PDF_BYTES as u64
            {
                return Err(OpenFailure::Changed);
            }
            private_copy(
                copies,
                (shown.size, "pdf"),
                |reservation, stop| reservation.copy(&file, stop, deadline),
                |copy| {
                    if identity(&file.metadata()?) != Some(shown) {
                        return Err(changed("the PDF changed while it was copied"));
                    }
                    check_copy(copy, shown.size, Some(shown), digest)
                },
            )
            .map_err(copy_failure)
        }
        DocumentSource::Bytes { bytes, digest } => {
            let bytes = bytes.as_slice();
            if content_digest(bytes, None).ok().as_ref() != Some(digest) {
                return Err(OpenFailure::Changed);
            }
            let len = bytes.len() as u64;
            private_copy(
                copies,
                (len, "pdf"),
                |reservation, stop| reservation.write(bytes, stop, deadline),
                |copy| check_copy(copy, len, None, digest),
            )
            .map_err(copy_failure)
        }
    }
}

/// Whether `copy`, read from its start, is `len` bytes whose digest, with
/// `identity`, is `digest`: what was shown. Anything else is `InvalidData`.
fn check_copy(
    copy: &File,
    len: u64,
    identity: Option<PathIdentity>,
    digest: &Digest,
) -> io::Result<()> {
    if copy.metadata()?.len() != len {
        return Err(changed("the copy is not the PDF's length"));
    }
    if content_digest_read(At { file: copy, at: 0 }, len, identity)? != *digest {
        return Err(changed("the copy is not the PDF shown"));
    }
    Ok(())
}

fn changed(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

/// A file read from an offset of its own, leaving the file's own offset as
/// it was.
struct At<'a> {
    file: &'a File,
    at: u64,
}

#[cfg(unix)]
impl io::Read for At<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        use std::os::unix::fs::FileExt as _;
        let read = self.file.read_at(buffer, self.at)?;
        self.at += read as u64;
        Ok(read)
    }
}

#[cfg(not(unix))]
impl io::Read for At<'_> {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        let _ = (self.file, self.at);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    use kettle_test_support::{PrivateTempDir, private_tempdir};

    use crate::paste_image::PastedImages;

    /// A small document's bytes. Nothing here parses them.
    const PDF: &[u8] =
        b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n";

    /// How many copies the store whose session is in `dir` keeps on disk.
    fn kept(dir: &PrivateTempDir) -> usize {
        std::fs::read_dir(dir.path().join("copies")).map_or(0, |files| files.count())
    }

    /// `bytes` written as `name` in a private directory, as a worker would
    /// have shown them: the file's path and the digest of its every byte
    /// with its identity.
    fn shown(name: &str, bytes: &[u8]) -> (PrivateTempDir, PathBuf, DocumentSource) {
        let dir = private_tempdir("kettle-open-document-");
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let digest = shown_digest(&path);
        let source = DocumentSource::File {
            path: path.clone(),
            digest,
        };
        (dir, path, source)
    }

    fn shown_digest(path: &Path) -> Digest {
        let bytes = std::fs::read(path).unwrap();
        let identity = PathIdentity::of(&std::fs::metadata(path).unwrap());
        content_digest(&bytes, identity).unwrap()
    }

    /// Empty document copies whose session would go in `dir`.
    fn copies_in(dir: &PrivateTempDir) -> PrivateCopies {
        PrivateCopies::of(PastedImages::of_kind_in(
            crate::paste_image::DOCUMENTS_ANY_DISK,
            dir.path().join("copies"),
        ))
    }

    fn charged(bytes: &[u8]) -> Arc<ChargedBytes> {
        Arc::new(ChargedBytes::new(bytes.to_vec()).expect("room"))
    }

    /// The copy is the shown file's bytes, owner-only, named `.pdf` whatever
    /// the file was named, marked downloaded on macOS, and still Kettle's.
    #[test]
    fn a_shown_file_is_copied_as_it_was_shown() {
        use std::os::unix::fs::PermissionsExt as _;
        let (dir, _path, document) = shown("report.txt", PDF);
        let copies = copies_in(&dir);
        let path = copy(&copies, &document).expect("copied");
        let mut store = copies.lock();
        assert_eq!(path.extension(), Some(std::ffi::OsStr::new("pdf")));
        assert_eq!(std::fs::read(&path).unwrap(), PDF);
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o7777, 0o600);
        assert!(store.path_still_matches(&path));
        #[cfg(target_os = "macos")]
        {
            let out = std::process::Command::new("/usr/bin/xattr")
                .arg("-p")
                .arg("com.apple.quarantine")
                .arg(&path)
                .output()
                .expect("xattr");
            assert!(String::from_utf8_lossy(&out.stdout).contains(";Kettle;"));
        }
        store.cleanup();
        assert!(!path.exists());
    }

    /// Bytes a request carried are written as the copy, checked against the
    /// digest shown; bytes that are not what was shown are refused before
    /// anything is written.
    #[test]
    fn held_bytes_are_copied_as_they_were_shown() {
        let dir = private_tempdir("kettle-open-document-");
        let copies = copies_in(&dir);
        let document = DocumentSource::Bytes {
            bytes: charged(PDF),
            digest: content_digest(PDF, None).unwrap(),
        };
        let path = copy(&copies, &document).expect("copied");
        assert_eq!(std::fs::read(&path).unwrap(), PDF);
        let other = DocumentSource::Bytes {
            bytes: charged(b"%PDF-1.7 something else"),
            digest: content_digest(PDF, None).unwrap(),
        };
        assert_eq!(copy(&copies, &other), Err(OpenFailure::Changed));
        assert_eq!(kept(&dir), 1, "only the first copy");
        copies.close();
        assert_eq!(kept(&dir), 0);
    }

    /// A file that is not the one shown is never copied: another file at
    /// the path, the same file rewritten to the same length, or a file
    /// grown past what Kettle shows.
    #[test]
    fn a_changed_file_is_not_copied() {
        let (dir, path, document) = shown("report.pdf", PDF);
        let copies = copies_in(&dir);
        // Rewritten in place to the same length, later: another identity.
        let mut rewritten = PDF.to_vec();
        rewritten[1] = b'Q';
        std::fs::write(&path, &rewritten).unwrap();
        let file = File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(9))
            .unwrap();
        assert_eq!(copy(&copies, &document), Err(OpenFailure::Changed));
        // Another file put in its place, made before the original goes so
        // it cannot take its inode.
        let next = dir.path().join("next.pdf");
        std::fs::write(&next, PDF).unwrap();
        std::fs::rename(&next, &path).unwrap();
        assert_eq!(copy(&copies, &document), Err(OpenFailure::Changed));
        // Gone.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(copy(&copies, &document), Err(OpenFailure::Changed));
        assert_eq!(kept(&dir), 0);
        copies.close();
        assert!(!dir.path().join("copies").exists());
    }

    /// A copy whose bytes differ from what was shown, though the file keeps
    /// its identity, is caught by its digest and deleted.
    #[test]
    fn a_copy_is_checked_byte_for_byte() {
        let (dir, path, _) = shown("report.pdf", PDF);
        let copies = copies_in(&dir);
        let identity = PathIdentity::of(&std::fs::metadata(&path).unwrap());
        // The digest of other bytes under this file's identity: what a file
        // rewritten without its identity changing would give.
        let mut other = PDF.to_vec();
        other[1] = b'Q';
        let document = DocumentSource::File {
            path,
            digest: content_digest(&other, identity).unwrap(),
        };
        assert_eq!(copy(&copies, &document), Err(OpenFailure::Changed));
        assert_eq!(kept(&dir), 0, "the copy was deleted");
        copies.close();
    }

    /// The check reads every byte of the copy from its start, and a copy
    /// shorter or longer than shown is not what was shown.
    #[test]
    fn the_check_reads_the_whole_copy() {
        let dir = private_tempdir("kettle-open-document-");
        let at = dir.path().join("copy.pdf");
        let long: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
        std::fs::write(&at, &long).unwrap();
        let digest = content_digest(&long, None).unwrap();
        let file = File::open(&at).unwrap();
        assert!(check_copy(&file, long.len() as u64, None, &digest).is_ok());
        assert!(
            check_copy(&file, long.len() as u64, None, &digest).is_ok(),
            "again"
        );
        let kind = |result: io::Result<()>| result.map_err(|error| error.kind());
        assert_eq!(
            kind(check_copy(&file, long.len() as u64 - 1, None, &digest)),
            Err(io::ErrorKind::InvalidData)
        );
        std::fs::write(&at, &long[..long.len() - 1]).unwrap();
        let file = File::open(&at).unwrap();
        assert_eq!(
            kind(check_copy(&file, long.len() as u64, None, &digest)),
            Err(io::ErrorKind::InvalidData)
        );
    }

    /// Once closed, a store copies nothing more.
    #[test]
    fn a_closed_store_copies_nothing() {
        let (dir, _path, document) = shown("late.pdf", PDF);
        let copies = copies_in(&dir);
        copies.close();
        assert_eq!(copy(&copies, &document), Err(OpenFailure::Copy));
        assert!(!dir.path().join("copies").exists());
    }

    /// A PDF item names its file or its held bytes and opens in the
    /// document viewer, never the image viewer; any other item, or a PDF
    /// whose bytes were let go, names none.
    #[test]
    fn a_pdf_item_opens_in_the_document_viewer() {
        let (_dir, path, _) = shown("report.pdf", PDF);
        let digest = shown_digest(&path);
        let page = Some(kettle_media::PageOf { index: 1, count: 3 });
        let item = |kind, source: crate::media::ItemSource| {
            ShelfItem::new(
                1,
                None,
                "report.pdf".into(),
                crate::media::Provenance::User,
                kind,
                Vec::new(),
                kettle_core::ImageData::new(1, 1, vec![0; 4]).unwrap(),
                source,
            )
        };
        let pdf = kettle_media::MediaKind::Pdf;
        let file = item(
            pdf,
            crate::media::ItemSource::sample_file(&path, digest.clone()).with_pages(page),
        );
        assert!(matches!(
            DocumentSource::of(&file),
            Some(DocumentSource::File { path: named, digest: same }) if named == path && same == digest
        ));
        assert_eq!(
            super::super::Outside::of(&file),
            Reader::find().map(super::super::Outside::Document)
        );
        let mut held = item(pdf, crate::media::ItemSource::sample(PDF).with_pages(page));
        assert!(matches!(
            DocumentSource::of(&held),
            Some(DocumentSource::Bytes { bytes, .. }) if bytes.as_slice() == PDF
        ));
        held.source.release();
        assert!(DocumentSource::of(&held).is_none(), "its bytes were let go");
        let picture = item(
            kettle_media::MediaKind::Raster,
            crate::media::ItemSource::sample_file(&path, digest),
        );
        assert!(DocumentSource::of(&picture).is_none());
        assert_eq!(
            super::super::Outside::of(&picture),
            Viewer::find().map(super::super::Outside::Image)
        );
    }

    /// Papers is preferred where the system has it, Evince otherwise.
    #[test]
    fn papers_is_preferred_with_evince_as_the_fallback() {
        assert_eq!(linux_reader(true, false), Some(Reader::Papers));
        assert_eq!(linux_reader(true, true), Some(Reader::Papers));
        assert_eq!(linux_reader(false, true), Some(Reader::Evince));
        assert_eq!(linux_reader(false, false), None);
    }

    /// Each viewer's command is a fixed program with the absolute copy last.
    #[test]
    fn each_command_is_fixed() {
        let file = Path::new("/tmp/kettle-document-1-0/0001.pdf");
        let shape = |command: std::process::Command| {
            (
                command.get_program().to_os_string(),
                command
                    .get_args()
                    .map(|arg| arg.to_os_string())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            shape(Reader::Preview.command(file)),
            (
                "/usr/bin/open".into(),
                vec![
                    "-a".into(),
                    super::super::PREVIEW_APP.into(),
                    file.as_os_str().into()
                ]
            )
        );
        assert_eq!(
            shape(Reader::Evince.command(file)),
            (EVINCE.into(), vec![file.as_os_str().into()])
        );
        assert_eq!(
            shape(Reader::Papers.command(file)),
            (PAPERS.into(), vec![file.as_os_str().into()])
        );
    }

    /// The viewer gets the copy only after it is checked and the copy is
    /// made and checked, and while the copy is still Kettle's, under the
    /// store's lock; a copy the viewer did not get is deleted.
    #[test]
    fn a_pdf_reaches_the_viewer_only_after_every_check() {
        let source = include_str!("document.rs")
            .split("#[cfg(all(test, unix))]")
            .next()
            .expect("production part")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let body = source
            .split("fn hand_over(")
            .nth(1)
            .expect("hand_over")
            .split(" fn ")
            .next()
            .expect("body");
        let order = [
            "if !reader.verified() { return Err(OpenFailure::Viewer); }",
            "let path = copy(copies, document)?;",
            "let mut store = copies.lock();",
            "if store.path_still_matches(&path) && !copies.closing() { reader.start(&path) }",
            "if started.is_err() { let _ = store.remove(&path); }",
        ]
        .map(|step| body.find(step).unwrap_or_else(|| panic!("{step}")));
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    }

    /// A one-page PDF, its cross-reference at the offsets written, for the
    /// live tests.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn one_page() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R >>",
            "<< /Length 25 >>\nstream\n1 0 0 rg 0 0 100 100 re f\nendstream",
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (number, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend(format!("{} 0 obj\n{object}\nendobj\n", number + 1).into_bytes());
        }
        let xref = bytes.len();
        bytes.extend(b"xref\n0 5\n0000000000 65535 f \n");
        for offset in offsets {
            bytes.extend(format!("{offset:010} 00000 n \n").into_bytes());
        }
        bytes.extend(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").into_bytes(),
        );
        bytes
    }

    /// Live, by hand on a Mac with a desktop session: Preview passes its
    /// check and opens a marked copy of a real PDF, and the copy goes with
    /// the store. It opens a Preview window, so it is ignored by default.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "opens a Preview window"]
    fn preview_opens_a_marked_copy_live() {
        let (dir, _path, document) = shown("report.pdf", &one_page());
        let copies = copies_in(&dir);
        let mut child = hand_over(&copies, &document, Reader::Preview).expect("handed over");
        assert!(child.wait().expect("open").success());
        // Preview reads the file once it is up.
        std::thread::sleep(Duration::from_secs(3));
        copies.close();
        assert!(!dir.path().join("copies").exists());
    }

    /// Live, by hand in a GNOME session with Papers: Papers passes its check
    /// and opens a copy of a real PDF, which stays open once the store goes.
    /// It opens a Papers window, so it is ignored by default.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "opens a Papers window"]
    // Papers stays up, for whoever runs this to look at and close.
    #[allow(clippy::zombie_processes)]
    fn papers_opens_a_copy_live() {
        let (dir, _path, document) = shown("report.pdf", &one_page());
        let copies = copies_in(&dir);
        let child = hand_over(&copies, &document, Reader::Papers).expect("handed over");
        // For whoever runs this to close.
        eprintln!("papers started: {}", child.id());
        // Papers reads the file once it is up.
        std::thread::sleep(Duration::from_secs(3));
        copies.close();
        assert!(!dir.path().join("copies").exists());
    }
}
