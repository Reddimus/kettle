//! Clipboard bitmap → temp PNG, so a screenshot can be pasted into a pane.
//!
//! Copying a *file* in the OS file manager populates `CF_HDROP` /
//! `text/uri-list`, which `paste_clipboard` already turns into a shell-quoted
//! path. Capturing a *screenshot* (Win+Shift+S, Snipping Tool, macOS
//! Cmd+Shift+4, GNOME Screenshot) does something different: it puts a raw
//! bitmap on the clipboard with no file and no text behind it. There was
//! nothing for the file-paste branch to path-ify, so the paste did nothing at
//! all.
//!
//! This module closes that gap by materializing the bitmap as a PNG and handing
//! back its path, which then flows through the exact same
//! [`crate::mux::format_paths_for_paste`] pipeline as a copied file — shell-aware
//! quoting and WSL `C:\` → `/mnt/c` translation included. Handing a CLI agent a
//! path also sidesteps the terminal-side clipboard-bitmap decoding that agents
//! do inconsistently across platforms.
//!
//! **Privacy.** A pasted screenshot is arbitrary user content — it can contain
//! anything that was on screen. Files use owner-only permissions inside an
//! owner-private per-process scratch directory. Kettle retains each file's
//! original handle, caps the live encoded-PNG aggregate at 256 MiB, and deletes
//! only that exact file identity on exit. Windows uses per-user Local App Data
//! because the process temp directory can grant sandbox principals delete-child
//! access; other platforms use the OS temp directory. A bounded startup sweep
//! reclaims an exact, old session only after its creator PID is definitively
//! dead, so a long-running sibling is never deleted merely because it is old.
//!
//! **Copies for an image viewer.** A second store of the same kind, under its
//! own prefix ([`OPENED`]), holds the PNG copies Kettle hands to the permitted
//! image viewer when the user opens a shelf item outside Kettle. A viewer has
//! read its copy once it opens, so that store drops its oldest copies to take
//! a new one instead of refusing it; it is marked downloaded on macOS before
//! any viewer sees it, deleted on exit, and swept with the pasted sessions.
//!
//! **Copies for a video player.** A third store ([`VIDEOS`]) holds private
//! copies of the files videos were shown from, for the permitted video
//! player. Each is a clone of the held source where the file system makes
//! one, and a bounded chunked copy where it cannot (another volume, or a
//! file system without clones) that leaves [`MIN_FREE_AFTER_COPY`] free; it
//! is made owner-only, named with the extension of the container its bytes
//! are, checked by the caller through a fresh handle before it is kept, and
//! then held, marked, deleted and swept as the image copies are.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What a store keeps, how much of it, and what it does when full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StoreKind {
    /// Directory-name prefix inside the platform scratch root. Also the sweep
    /// key, so it must stay stable across versions or old directories become
    /// unreclaimable.
    prefix: &'static str,
    /// The extensions its files are named with, lowercase. The first names
    /// the empty file that founds a session.
    extensions: &'static [&'static str],
    /// Per-session ceilings. The byte ceiling also bounds the source buffer:
    /// the caller already materialized it, but refusing a larger value keeps
    /// a malformed provider from driving an unbounded encode.
    max_files: usize,
    max_bytes: u64,
    /// The highest image number a session names. A store that refuses when
    /// full never passes its file count; one that drops its oldest keeps
    /// counting.
    max_sequence: usize,
    /// Whether a full store drops its oldest images for a new one rather than
    /// refusing it.
    evicts: bool,
    /// How long an image stays before it may be dropped for a newer one, so
    /// whoever was handed it has time to read it.
    min_age: Duration,
    /// The free space a byte copy into this store must leave on its volume.
    /// A clone takes none, so it is not asked.
    keep_free: u64,
}

/// Screenshots pasted as paths. A program may still read any of them, so a
/// full session refuses more. A paste is a deliberate user action, so the
/// ceilings are generous: they bound a stuck key or a hostile automation
/// loop, not normal use.
pub(crate) const PASTED: StoreKind = StoreKind {
    prefix: "kettle-paste-",
    extensions: &["png"],
    max_files: 64,
    max_bytes: 256 * 1024 * 1024,
    max_sequence: 64,
    evicts: false,
    min_age: Duration::ZERO,
    keep_free: 0,
};

/// Copies handed to the image viewer. The viewer reads its copy when it
/// opens it, so the oldest go first when the store is full, once they have
/// been there a minute; a store full of newer copies refuses another.
pub(crate) const OPENED: StoreKind = StoreKind {
    prefix: "kettle-open-",
    extensions: &["png"],
    max_files: 32,
    max_bytes: 128 * 1024 * 1024,
    max_sequence: 999_999,
    evicts: true,
    min_age: Duration::from_secs(60),
    keep_free: 0,
};

/// Private copies of videos handed to the video player, each named with
/// the extension of the container its bytes are. A player reads its copy
/// for as long as it plays it, and a copy dropped while open stays readable
/// to it, so the oldest go first when the store is full, once they have
/// been there five minutes. A clone shares its source's blocks, but each
/// copy counts as its full length, what a byte copy takes.
pub(crate) const VIDEOS: StoreKind = StoreKind {
    prefix: "kettle-video-",
    extensions: &[
        "mp4", "mov", "mkv", "webm", "avi", "flv", "mpg", "m2v", "ts", "ogv", "asf",
    ],
    max_files: 8,
    max_bytes: 8 * 1024 * 1024 * 1024,
    max_sequence: 999_999,
    evicts: true,
    min_age: Duration::from_secs(5 * 60),
    keep_free: MIN_FREE_AFTER_COPY,
};

/// The video store with no free space to keep, for tests that copy without
/// depending on how full the machine running them is.
#[cfg(all(test, unix))]
pub(crate) const VIDEOS_ANY_DISK: StoreKind = StoreKind {
    keep_free: 0,
    ..VIDEOS
};

/// Every store, for the crash sweep.
const KINDS: [StoreKind; 3] = [PASTED, OPENED, VIDEOS];

/// The free space a byte copy of a video must leave on its volume.
const MIN_FREE_AFTER_COPY: u64 = 2 * 1024 * 1024 * 1024;

/// How much of a video a byte copy reads and writes at a time, between
/// asking whether to stop.
#[cfg_attr(not(unix), allow(dead_code))]
const COPY_CHUNK: usize = 1024 * 1024;

/// Reject absurd dimensions before allocating. 16384² RGBA is ~1 GiB, already
/// far past any real screenshot; beyond this a malformed clipboard descriptor is
/// likelier than a genuine capture.
const MAX_DIMENSION: usize = 16_384;

/// The receipt is UI chrome, not a second copy of the screenshot. Keep its
/// decoded allocation small enough to remain cheap even on a large display.
const PREVIEW_MAX_WIDTH: u32 = 256;
const PREVIEW_MAX_HEIGHT: u32 = 160;

/// Directories from an earlier run are removed once they are older than this.
/// Only relevant when a previous process died without running its cleanup.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Startup cleanup is deliberately bounded even if the scratch root is hostile
/// or damaged. A valid session contains at most its kind's `max_files`
/// regular PNGs.
const MAX_SWEEP_ENTRIES: usize = 8_192;
const MAX_SWEEP_ATTEMPTS: usize = 64;
const MAX_SWEEP_SESSIONS: usize = 32;
const MAX_SWEEP_DURATION: Duration = Duration::from_millis(250);

struct LiveImage {
    path: PathBuf,
    name: OsString,
    file: File,
    /// Its length, counted against the session's bytes.
    bytes: u64,
    created: Instant,
    preview: Option<PastedImagePreview>,
}

/// The next file a store is ready to take: its name, path and number, and
/// the bytes left in the budget.
struct NextFile {
    name: OsString,
    path: PathBuf,
    sequence: usize,
    remaining: u64,
}

/// How a copy is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Copying {
    /// A clone where the file system makes one, else bytes.
    CloneFirst,
    /// Bytes, a chunk at a time.
    #[cfg_attr(not(all(test, unix)), allow(dead_code))]
    Bytes,
}

/// An image the store dropped but could not delete. It still counts against
/// the store's bounds, its bytes included, and cleanup tries it again.
struct StuckImage {
    name: OsString,
}

/// Bounded renderer projection retained only beside the private PNG whose path
/// Kettle pasted. A caller cannot construct one for an arbitrary terminal path.
#[derive(Clone, Debug)]
pub(crate) struct PastedImagePreview {
    pub(crate) image: kettle_core::ImageData,
    pub(crate) original_width: u32,
    pub(crate) original_height: u32,
}

struct SessionDirectory {
    path: PathBuf,
    file: File,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl SessionDirectory {
    /// Another handle on the same held directory.
    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            path: self.path.clone(),
            file: self.file.try_clone()?,
            #[cfg(unix)]
            device: self.device,
            #[cfg(unix)]
            inode: self.inode,
        })
    }
}

/// Room and a name a store set aside for one copy, which is made without
/// holding the store ([`Reservation::copy`]) and then kept
/// ([`PastedImages::keep_copy`]) or given back ([`PastedImages::release`]).
/// Until then its place and its bytes count against the store's bounds.
pub(crate) struct Reservation {
    /// Its own handle on the session directory the copy goes in.
    directory: SessionDirectory,
    next: NextFile,
    size: u64,
    /// The free space a byte copy must leave.
    keep_free: u64,
    /// The store's count of bytes its byte copies have yet to write.
    pending: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Reservation {
    /// Copy the first `size` bytes of `source`, an open regular file, into
    /// the reserved private file and return it. Where the file system can
    /// clone the source (APFS, Btrfs, XFS) the copy shares its blocks;
    /// elsewhere it is copied a chunk at a time, only while
    /// [`MIN_FREE_AFTER_COPY`] stays free. `stop` is asked before and after a
    /// clone and between chunks, and so is `deadline`; a copy either ends
    /// asks for is discarded. A read the file system never answers is not
    /// interrupted, but it holds no store.
    pub(crate) fn copy(
        &self,
        source: &File,
        stop: &dyn Fn() -> bool,
        deadline: Instant,
    ) -> io::Result<File> {
        self.copy_as(source, Copying::CloneFirst, stop, deadline)
    }

    fn copy_as(
        &self,
        source: &File,
        how: Copying,
        stop: &dyn Fn() -> bool,
        deadline: Instant,
    ) -> io::Result<File> {
        copy_into_session(
            &self.directory,
            self.next.name.as_os_str(),
            source,
            (self.size, how, self.keep_free),
            (stop, deadline),
            &self.pending,
        )
    }

    /// Discard `file`, the copy made for this reservation, which its maker
    /// found not to be what it should be. The reservation still has to be
    /// given back.
    pub(crate) fn discard(&self, file: File) {
        discard_private_file_in_session(&self.directory, file, self.next.name.as_os_str());
    }
}

/// Owner of this process's pasted-image scratch directory.
///
/// The directory is created lazily only after bitmap validation and budget
/// preflight, so a session that never attempts an image paste touches the
/// filesystem not at all.
pub(crate) struct PastedImages {
    kind: StoreKind,
    dir: PathBuf,
    seq: usize,
    directory: Option<SessionDirectory>,
    files: Vec<LiveImage>,
    stuck: Vec<StuckImage>,
    /// Copies being made: each one's name and the bytes set aside for it.
    reserved: Vec<(OsString, u64)>,
    /// The bytes this store's byte copies have yet to write, together, so
    /// copies made at once keep the free-space reserve between them.
    pending: std::sync::Arc<std::sync::atomic::AtomicU64>,
    bytes: u64,
    /// Closed for good: it takes no more images.
    closed: bool,
}

impl PastedImages {
    pub(crate) fn new(kind: StoreKind) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        Self {
            kind,
            dir: scratch_root().join(format!("{}{}-{nonce}", kind.prefix, std::process::id())),
            seq: 0,
            directory: None,
            files: Vec::new(),
            stuck: Vec::new(),
            reserved: Vec::new(),
            pending: Default::default(),
            bytes: 0,
            closed: false,
        }
    }

    /// Encode `width`×`height` RGBA8 `rgba` as a PNG and return its path.
    ///
    /// Split from the clipboard call so the encode/bounds policy is testable
    /// without a live clipboard or a display server.
    pub(crate) fn save_rgba(
        &mut self,
        width: usize,
        height: usize,
        rgba: &[u8],
        with_preview: bool,
    ) -> io::Result<PathBuf> {
        if width == 0 || height == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "clipboard image has a zero dimension",
            ));
        }
        if width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("clipboard image {width}x{height} exceeds the {MAX_DIMENSION}px limit"),
            ));
        }
        // A short read here would make the encoder index out of bounds, so the
        // length is checked against the declared geometry rather than trusted.
        let expected = width
            .checked_mul(height)
            .and_then(|px| px.checked_mul(4))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "clipboard image size overflows")
            })?;
        if u64::try_from(expected).unwrap_or(u64::MAX) > self.kind.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "clipboard RGBA buffer exceeds the {} byte limit",
                    self.kind.max_bytes
                ),
            ));
        }
        if rgba.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "clipboard image is {} bytes, expected {expected} for {width}x{height} RGBA",
                    rgba.len()
                ),
            ));
        }
        // Make room oldest first, for as much as the PNG can take: its
        // filtered rows, deflated no larger than stored, plus framing.
        let bound = u64::try_from(expected + height)
            .unwrap_or(u64::MAX)
            .saturating_mul(9)
            / 8
            + 4096;
        let next = self.begin(bound, "png")?;
        let directory = self
            .directory
            .as_ref()
            .expect("begin established the session directory");
        let name = next.name.as_os_str();
        let file = create_private_file_in_session(directory, name)?;
        let mut writer = BudgetWriter::new(io::BufWriter::new(file), next.remaining);
        // `image` is already a kettle-ui dependency (the window icon decodes
        // through it), and the `png` feature it is built with covers encoding.
        use image::ImageEncoder as _;
        let encoded = image::codecs::png::PngEncoder::new(&mut writer)
            .write_image(
                rgba,
                width as u32,
                height as u32,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|error| {
                if writer.exceeded() {
                    budget_error(self.files.len(), self.bytes)
                } else {
                    io::Error::other(format!("PNG encode failed: {error}"))
                }
            })
            .and_then(|()| writer.flush());
        let written = writer.written();
        let buffered = writer.into_inner();
        let (file, _pending) = buffered.into_parts();
        if let Err(error) = encoded {
            discard_private_file_in_session(directory, file, name);
            return Err(error);
        }
        // Build UI chrome only after the private PNG has been published. A
        // session already at its file or byte limit should fail before doing
        // even the bounded thumbnail resize on every rejected paste.
        self.finish(
            next,
            file,
            written,
            |_| Ok(()),
            || {
                if with_preview {
                    make_preview(width, height, rgba)
                } else {
                    None
                }
            },
        )
    }

    /// Set aside room and a name for a `size` byte copy named with
    /// `extension`, one of this store's, to be made without holding the
    /// store.
    pub(crate) fn reserve_copy(&mut self, size: u64, extension: &str) -> io::Result<Reservation> {
        if !self.kind.extensions.contains(&extension) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("this store names no file .{extension}"),
            ));
        }
        if size == 0 || size > self.kind.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                format!("a {size} byte copy is past what this store holds"),
            ));
        }
        let next = self.begin(size, extension)?;
        if size > next.remaining {
            return Err(budget_error(self.files.len(), self.bytes));
        }
        let directory = self
            .directory
            .as_ref()
            .expect("begin established the session directory")
            .try_clone()?;
        // The name is taken now, so a copy reserved meanwhile gets the next.
        self.seq = next.sequence;
        self.reserved.push((next.name.clone(), size));
        Ok(Reservation {
            directory,
            next,
            size,
            keep_free: self.kind.keep_free,
            pending: std::sync::Arc::clone(&self.pending),
        })
    }

    /// Keep `file`, the copy made for `reservation`, once it is as long as
    /// reserved, a fresh handle through the held directory is the same file,
    /// `check` accepts it, and the store is open with the session it
    /// reserved in. Anything else discards it.
    pub(crate) fn keep_copy(
        &mut self,
        reservation: Reservation,
        file: File,
        check: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<PathBuf> {
        let Reservation {
            directory,
            next,
            size,
            ..
        } = reservation;
        self.unreserve(&next.name);
        let same = self.directory.as_ref().is_some_and(|held| {
            same_open_file_identity(&held.file, &directory.file).unwrap_or(false)
        });
        if self.closed || !same {
            discard_private_file_in_session(&directory, file, next.name.as_os_str());
            // The store closed while the copy was made, and its cleanup left
            // the directory for it: the last one out takes it away.
            if self.directory.is_none() {
                let _ = remove_session_directory(directory);
            }
            return Err(io::Error::other("the store closed while the copy was made"));
        }
        self.finish(next, file, size, check, || None)
    }

    /// Give back the room `reservation` set aside, its copy not made.
    pub(crate) fn release(&mut self, reservation: Reservation) {
        self.unreserve(&reservation.next.name);
        if self.directory.is_none() {
            let _ = remove_session_directory(reservation.directory);
        }
    }

    fn unreserve(&mut self, name: &OsStr) {
        if let Some(index) = self.reserved.iter().position(|(held, _)| held == name) {
            self.reserved.swap_remove(index);
        }
    }

    /// Delete the copy at `path`, through the held directory, and stop
    /// counting it: one that was kept but never handed over.
    pub(crate) fn remove(&mut self, path: &Path) -> io::Result<()> {
        let index = self
            .files
            .iter()
            .position(|image| image.path == path)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let directory = self.directory.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "the store holds no directory")
        })?;
        let image = self.files.remove(index);
        match remove_open_private_file_in_session(directory, image.file, &image.name) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                // Still on disk: still counted, its bytes included, for
                // cleanup to try again.
                self.stuck.push(StuckImage { name: image.name });
                return Err(error);
            }
        }
        self.bytes = self.bytes.saturating_sub(image.bytes);
        Ok(())
    }

    /// Another handle on the kept copy at `path`, pinned to the file Kettle
    /// checked whatever later happens to its name.
    pub(crate) fn kept_file(&self, path: &Path) -> io::Result<File> {
        self.files
            .iter()
            .find(|image| image.path == path)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?
            .file
            .try_clone()
    }

    /// Reserve, copy and keep in one go, for tests.
    #[cfg(all(test, unix))]
    fn save_copy(
        &mut self,
        source: &File,
        size: u64,
        extension: &str,
        stop: &dyn Fn() -> bool,
        deadline: Instant,
        check: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<PathBuf> {
        self.save_copy_as(
            source,
            (size, extension),
            Copying::CloneFirst,
            (stop, deadline),
            check,
        )
    }

    #[cfg(all(test, unix))]
    fn save_copy_as(
        &mut self,
        source: &File,
        (size, extension): (u64, &str),
        how: Copying,
        (stop, deadline): (&dyn Fn() -> bool, Instant),
        check: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<PathBuf> {
        let reservation = self.reserve_copy(size, extension)?;
        match reservation.copy_as(source, how, stop, deadline) {
            Ok(file) => self.keep_copy(reservation, file, check),
            Err(error) => {
                self.release(reservation);
                Err(error)
            }
        }
    }

    /// Get ready for one more file named with `extension`: make room for
    /// `bound` bytes when the store drops its oldest, refuse when it is full
    /// or closed, hold its session directory, and name the file.
    fn begin(&mut self, bound: u64, extension: &str) -> io::Result<NextFile> {
        if self.closed {
            return Err(io::Error::other("the image store is closed"));
        }
        if self.kind.evicts {
            while self.held() >= self.kind.max_files
                || self.kind.max_bytes.saturating_sub(self.counted_bytes()) < bound
            {
                if !self.drop_oldest() {
                    break;
                }
            }
        }
        let remaining = self.kind.max_bytes.saturating_sub(self.counted_bytes());
        if self.held() >= self.kind.max_files || remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                format!(
                    "pasted-image budget reached ({} files, {} bytes)",
                    self.files.len(),
                    self.bytes
                ),
            ));
        }
        if self.directory.is_none() {
            self.directory = Some(establish_session_directory(&self.dir, self.kind)?);
        }
        let directory = self
            .directory
            .as_ref()
            .expect("the session directory was established above");
        verify_session_directory_path(directory)?;

        let sequence = self
            .seq
            .checked_add(1)
            .filter(|sequence| *sequence <= self.kind.max_sequence)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::QuotaExceeded,
                    "pasted-image sequence overflowed",
                )
            })?;
        let leaf = format!("{sequence:04}.{extension}");
        Ok(NextFile {
            path: self.dir.join(&leaf),
            name: OsString::from(leaf),
            sequence,
            remaining,
        })
    }

    /// Keep `file`, the new file `next` names, once it is `written` bytes
    /// long within the budget, a fresh handle through the held directory is
    /// the same file, `check` accepts it, and the session is still the one
    /// held. Anything else discards it. `preview` is built only once it is
    /// kept.
    fn finish(
        &mut self,
        next: NextFile,
        file: File,
        written: u64,
        check: impl FnOnce(&File) -> io::Result<()>,
        preview: impl FnOnce() -> Option<PastedImagePreview>,
    ) -> io::Result<PathBuf> {
        let directory = self
            .directory
            .as_ref()
            .expect("begin established the session directory");
        let name = next.name.as_os_str();
        let actual = match file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                discard_private_file_in_session(directory, file, name);
                return Err(error);
            }
        };
        if actual != written || actual > next.remaining {
            discard_private_file_in_session(directory, file, name);
            return Err(if actual > next.remaining {
                budget_error(self.files.len(), self.bytes)
            } else {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "the file's length changed while writing (counted {written}, found {actual})"
                    ),
                )
            });
        }
        // Reopen through the held session-directory capability, then compare
        // kernel identities before releasing the creator. Windows also has the
        // creator's no-delete share pin; Unix does not rely on path stability.
        let retained = match open_existing_private_file_in_session(directory, name) {
            Ok(retained) => retained,
            Err(error) => {
                discard_private_file_in_session(directory, file, name);
                return Err(error);
            }
        };
        let same_file = match same_open_file_identity(&file, &retained) {
            Ok(same) => same,
            Err(error) => {
                drop(retained);
                discard_private_file_in_session(directory, file, name);
                return Err(error);
            }
        };
        if !same_file {
            drop(retained);
            discard_private_file_in_session(directory, file, name);
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "pasted-image path no longer identifies the created PNG",
            ));
        }
        if let Err(error) = check(&retained) {
            drop(retained);
            discard_private_file_in_session(directory, file, name);
            return Err(error);
        }
        match session_directory_matches_path(directory) {
            Ok(true) => {}
            Ok(false) => {
                drop(retained);
                discard_private_file_in_session(directory, file, name);
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "pasted-image session changed while publishing the PNG path",
                ));
            }
            Err(error) => {
                drop(retained);
                discard_private_file_in_session(directory, file, name);
                return Err(error);
            }
        }
        drop(file);
        let Some(total) = self.bytes.checked_add(actual) else {
            let _ = remove_open_private_file_in_session(directory, retained, name);
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                "pasted-image byte accounting overflowed",
            ));
        };
        self.bytes = total;
        // A copy reserved after this one may already have taken a later name.
        self.seq = self.seq.max(next.sequence);
        self.files.push(LiveImage {
            path: next.path.clone(),
            name: next.name,
            file: retained,
            bytes: actual,
            created: Instant::now(),
            preview: preview(),
        });
        Ok(next.path)
    }

    /// The images this store counts: those it holds, those it could not
    /// delete, and copies being made.
    fn held(&self) -> usize {
        self.files.len() + self.stuck.len() + self.reserved.len()
    }

    /// The bytes this store counts: what it holds and what copies being made
    /// set aside.
    fn counted_bytes(&self) -> u64 {
        self.reserved
            .iter()
            .fold(self.bytes, |total, (_, size)| total.saturating_add(*size))
    }

    /// Delete the oldest image, through the held directory, and stop counting
    /// it. False when there is none old enough to drop. One that cannot be
    /// deleted is still counted, for cleanup to try again, and the next
    /// oldest goes instead.
    fn drop_oldest(&mut self) -> bool {
        let Some(directory) = self.directory.as_ref() else {
            return false;
        };
        while self
            .files
            .first()
            .is_some_and(|image| image.created.elapsed() >= self.kind.min_age)
        {
            let image = self.files.remove(0);
            match remove_open_private_file_in_session(directory, image.file, &image.name) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    log::debug!(
                        "image store left {} unchanged: {error}",
                        image.path.display()
                    );
                    self.stuck.push(StuckImage { name: image.name });
                    continue;
                }
            }
            self.bytes = self.bytes.saturating_sub(image.bytes);
            return true;
        }
        false
    }

    /// Clean up and take no more images, for good: a worker still holding
    /// the store after exit cleanup cannot leave a new copy behind.
    pub(crate) fn close(&mut self) {
        self.closed = true;
        self.cleanup();
    }

    /// Mark the image at `path`, through the handle this store holds, as
    /// downloaded, so macOS treats it as from outside. Elsewhere it does
    /// nothing.
    pub(crate) fn mark_downloaded(&self, path: &Path) -> io::Result<()> {
        let image = self
            .files
            .iter()
            .find(|image| image.path == path)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd as _;
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs());
            let value = format!("0081;{seconds:x};Kettle;");
            // SAFETY: the name is NUL-terminated and static, `value` is valid
            // for its length, and the descriptor is the store's own.
            let status = unsafe {
                libc::fsetxattr(
                    image.file.as_raw_fd(),
                    c"com.apple.quarantine".as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    0,
                )
            };
            if status != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = image;
        Ok(())
    }

    /// Move out the preview attached to this exact retained PNG. File-list
    /// paste, drag-and-drop, and text that merely resembles a path never reach
    /// this. The receipt becomes the only owner, so its expiry releases the
    /// bounded GPU/CPU reservation instead of retaining it until process exit.
    pub(crate) fn take_preview_for_path(&mut self, path: &Path) -> Option<PastedImagePreview> {
        self.files
            .iter_mut()
            .find(|image| image.path == path)
            .and_then(|image| image.preview.take())
    }

    /// Release previews for a paste that will not be delivered. The PNGs stay
    /// retained for their documented process lifetime; only the extra bounded
    /// renderer copy is discarded.
    pub(crate) fn discard_previews_for_paths(&mut self, paths: &[PathBuf]) {
        for path in paths {
            let _ = self.take_preview_for_path(path);
        }
    }

    /// Whether `path` still names the exact private PNG retained by this
    /// process. Opening through the held directory closes the leaf-symlink and
    /// replacement races before UI chrome hands the path to another process.
    pub(crate) fn path_still_matches(&self, path: &Path) -> bool {
        let Some(directory) = self.directory.as_ref() else {
            return false;
        };
        let Some(image) = self.files.iter().find(|image| image.path == path) else {
            return false;
        };
        if verify_session_directory_path(directory).is_err() {
            return false;
        }
        let Ok(reopened) = open_existing_private_file_in_session(directory, &image.name) else {
            return false;
        };
        same_open_file_identity(&image.file, &reopened).unwrap_or(false)
    }

    /// Remove only the exact PNG objects this process created, then the verified
    /// empty session directory. Best effort: a failure here must never take
    /// down a shutdown path, so it is logged and swallowed.
    pub(crate) fn cleanup(&mut self) {
        if self.files.is_empty() && self.directory.is_none() {
            return;
        }
        for image in self.files.drain(..) {
            let removal = self.directory.as_ref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "pasted image has no held session directory",
                )
            });
            if let Err(error) = removal.and_then(|directory| {
                remove_open_private_file_in_session(directory, image.file, &image.name)
            }) && error.kind() != io::ErrorKind::NotFound
            {
                log::debug!(
                    "pasted-image cleanup left {} unchanged: {error}",
                    image.path.display()
                );
            }
        }
        // Images an eviction could not delete get another try, through a
        // fresh handle on the same held directory.
        for image in self.stuck.drain(..) {
            let Some(directory) = self.directory.as_ref() else {
                break;
            };
            if let Err(error) = open_existing_private_file_in_session(directory, &image.name)
                .and_then(|file| remove_open_private_file_in_session(directory, file, &image.name))
                && error.kind() != io::ErrorKind::NotFound
            {
                log::debug!(
                    "image store cleanup left {} unchanged: {error}",
                    directory.path.join(&image.name).display()
                );
            }
        }
        if let Some(directory) = self.directory.take()
            && let Err(error) = remove_session_directory(directory)
            && error.kind() != io::ErrorKind::NotFound
        {
            log::debug!(
                "pasted-image directory cleanup left {} unchanged: {error}",
                self.dir.display()
            );
        }
        self.bytes = 0;
        self.seq = 0;
    }

    #[cfg(test)]
    pub(crate) fn with_dir(dir: PathBuf) -> Self {
        Self::of_kind_in(PASTED, dir)
    }

    #[cfg(test)]
    pub(crate) fn of_kind_in(kind: StoreKind, dir: PathBuf) -> Self {
        Self {
            kind,
            dir,
            seq: 0,
            directory: None,
            files: Vec::new(),
            stuck: Vec::new(),
            reserved: Vec::new(),
            pending: Default::default(),
            bytes: 0,
            closed: false,
        }
    }
}

fn make_preview(width: usize, height: usize, rgba: &[u8]) -> Option<PastedImagePreview> {
    let width_u32 = u32::try_from(width).ok()?;
    let height_u32 = u32::try_from(height).ok()?;
    let source =
        image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(width_u32, height_u32, rgba)?;
    let (preview_width, preview_height) =
        if width_u32 <= PREVIEW_MAX_WIDTH && height_u32 <= PREVIEW_MAX_HEIGHT {
            (width_u32, height_u32)
        } else if u64::from(width_u32) * u64::from(PREVIEW_MAX_HEIGHT)
            > u64::from(height_u32) * u64::from(PREVIEW_MAX_WIDTH)
        {
            (
                PREVIEW_MAX_WIDTH,
                (u64::from(height_u32) * u64::from(PREVIEW_MAX_WIDTH) / u64::from(width_u32)).max(1)
                    as u32,
            )
        } else {
            (
                (u64::from(width_u32) * u64::from(PREVIEW_MAX_HEIGHT) / u64::from(height_u32))
                    .max(1) as u32,
                PREVIEW_MAX_HEIGHT,
            )
        };
    // `thumbnail` uses a cheap box downsample instead of applying a wide
    // Triangle kernel across the full-resolution clipboard image on the UI
    // thread. Small images are copied at their native size, never enlarged.
    let thumbnail = if width_u32 <= PREVIEW_MAX_WIDTH && height_u32 <= PREVIEW_MAX_HEIGHT {
        image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(
            width_u32,
            height_u32,
            rgba.to_vec(),
        )?
    } else {
        image::imageops::thumbnail(&source, preview_width, preview_height)
    };
    let image = kettle_core::ImageData::new(preview_width, preview_height, thumbnail.into_raw())?;
    Some(PastedImagePreview {
        image,
        original_width: width_u32,
        original_height: height_u32,
    })
}

impl Drop for PastedImages {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Remove pasted-image and viewer-copy directories left by a process that
/// died before its own cleanup ran.
///
/// Age alone is never authority to delete. The directory name must have the
/// exact creator/session grammar, its creator PID must be definitively dead,
/// and every child must open as an owner-private, non-reparse, single-link
/// regular PNG before identity-safe deletion starts.
pub(crate) fn sweep_stale() {
    if let Err(error) = std::thread::Builder::new()
        .name("kettle-paste-sweep".into())
        .spawn(|| {
            for kind in KINDS {
                sweep_stale_in(
                    &scratch_root(),
                    SystemTime::now(),
                    kind,
                    process_is_definitely_dead,
                );
            }
        })
    {
        log::debug!("could not start pasted-image crash cleanup: {error}");
    }
}

fn sweep_stale_in(
    root: &Path,
    now: SystemTime,
    kind: StoreKind,
    mut process_is_dead: impl FnMut(u32) -> bool,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let started = Instant::now();
    let mut attempted = 0_usize;
    let mut reaped = 0_usize;
    for entry in entries.take(MAX_SWEEP_ENTRIES).flatten() {
        if attempted >= MAX_SWEEP_ATTEMPTS
            || reaped >= MAX_SWEEP_SESSIONS
            || started.elapsed() >= MAX_SWEEP_DURATION
        {
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(SessionName {
            pid,
            nonce: _session_nonce,
        }) = parse_session_name(name, kind.prefix)
        else {
            continue;
        };
        let apparently_stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > STALE_AFTER);
        if !apparently_stale {
            continue;
        }
        attempted += 1;
        if !process_is_dead(pid) {
            continue;
        }
        if reap_stale_session(&entry.path(), now, kind).is_ok() {
            reaped += 1;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SessionName {
    pid: u32,
    nonce: u128,
}

fn canonical_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn parse_session_name(name: &str, prefix: &str) -> Option<SessionName> {
    let suffix = name.strip_prefix(prefix)?;
    let (pid, nonce) = suffix.split_once('-')?;
    if nonce.contains('-') || !canonical_decimal(pid) || !canonical_decimal(nonce) {
        return None;
    }
    let pid = pid.parse::<u32>().ok().filter(|pid| *pid != 0)?;
    let nonce = nonce.parse::<u128>().ok()?;
    Some(SessionName { pid, nonce })
}

fn parse_image_name(name: &str, kind: StoreKind) -> Option<usize> {
    let (sequence, extension) = name.split_once('.')?;
    if !kind.extensions.contains(&extension)
        || sequence.len() < 4
        || !sequence.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let value = sequence
        .parse::<usize>()
        .ok()
        .filter(|value| (1..=kind.max_sequence).contains(value))?;
    (format!("{value:04}.{extension}") == name).then_some(value)
}

fn reap_stale_session(path: &Path, now: SystemTime, kind: StoreKind) -> io::Result<()> {
    let directory = open_session_directory(path)?;
    verify_session_directory_path(&directory)?;
    let modified = directory.file.metadata()?.modified()?;
    if now
        .duration_since(modified)
        .ok()
        .is_none_or(|age| age <= STALE_AFTER)
    {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "pasted-image session is not stale",
        ));
    }

    let entries = session_directory_entry_names(&directory, kind.max_files + 1)?;
    if entries.len() > kind.max_files {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pasted-image session exceeds its file-count bound",
        ));
    }
    let mut files = Vec::with_capacity(entries.len());
    for name in entries {
        let Some(name) = name.to_str() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pasted-image session contains a non-UTF-8 name",
            ));
        };
        if parse_image_name(name, kind).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("pasted-image session contains an unknown entry: {name}"),
            ));
        }
        verify_session_directory_path(&directory)?;
        let file = open_existing_private_file_in_session(&directory, OsStr::new(name))?;
        verify_session_directory_path(&directory)?;
        files.push((file, OsString::from(name)));
    }
    if files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pasted-image session contains no verified artifacts",
        ));
    }

    for (file, name) in files {
        verify_session_directory_path(&directory)?;
        remove_open_private_file_in_session(&directory, file, &name)?;
    }
    remove_session_directory(directory)
}

fn scratch_root() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("kettle").join("paste");
        }
    }
    std::env::temp_dir()
}

fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    kettle_state::create_private_file_new(path)
}

/// Establish the session directory without ever writing clipboard content
/// through its pathname.
///
/// The private-file helper securely creates the parent tree on every supported
/// platform. A short-lived, empty bootstrap file gives us a directory to open
/// and identify; only after that exact directory is held do real PNG creations
/// use its capability. If a same-user process races the create/open boundary,
/// identity comparison fails before any screenshot bytes are encoded.
fn establish_session_directory(path: &Path, kind: StoreKind) -> io::Result<SessionDirectory> {
    let bootstrap = format!("0001.{}", kind.extensions[0]);
    let bootstrap_name = OsStr::new(&bootstrap);
    let bootstrap_path = path.join(&bootstrap);
    let creator = create_private_file(&bootstrap_path)?;
    let directory = match open_session_directory(path) {
        Ok(directory) => directory,
        Err(error) => {
            discard_private_file(creator, &bootstrap_path);
            return Err(error);
        }
    };
    let reopened = match open_existing_private_file_in_session(&directory, bootstrap_name) {
        Ok(reopened) => reopened,
        Err(error) => {
            discard_private_file_in_session(&directory, creator, bootstrap_name);
            return Err(error);
        }
    };
    let same_file = match same_open_file_identity(&creator, &reopened) {
        Ok(same_file) => same_file,
        Err(error) => {
            drop(reopened);
            discard_private_file_in_session(&directory, creator, bootstrap_name);
            return Err(error);
        }
    };
    if !same_file {
        drop(reopened);
        discard_private_file_in_session(&directory, creator, bootstrap_name);
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image bootstrap path no longer identifies the created file",
        ));
    }

    // The reopened handle remains an exact identity anchor after the creator's
    // restrictive Windows share mode is released.
    drop(creator);
    remove_open_private_file_in_session(&directory, reopened, bootstrap_name)?;
    verify_session_directory_path(&directory)?;
    Ok(directory)
}

#[cfg(unix)]
fn create_private_file_in_session(directory: &SessionDirectory, name: &OsStr) -> io::Result<File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let c_name = session_c_name(name)?;
    // SAFETY: `name` is NUL-terminated, the held descriptor identifies an
    // owner-private directory, and a successful call transfers a new fd.
    let descriptor = unsafe {
        libc::openat(
            directory.file.as_raw_fd(),
            c_name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            (0o600 as libc::mode_t) as libc::c_uint,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new owned descriptor.
    let file = unsafe { File::from_raw_fd(descriptor) };
    if let Err(error) = restrict_private_session_file(&file) {
        discard_private_file_in_session(directory, file, name);
        return Err(error);
    }
    Ok(file)
}

#[cfg(not(unix))]
fn create_private_file_in_session(directory: &SessionDirectory, name: &OsStr) -> io::Result<File> {
    create_private_file(&directory.path.join(name))
}

#[cfg(unix)]
fn session_c_name(name: &OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Component;

    let mut components = Path::new(name).components();
    if !matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(component)), None) if component == name
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pasted-image file name is not a single normal path component",
        ));
    }
    std::ffi::CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "pasted-image file name contains a NUL byte",
        )
    })
}

#[cfg(unix)]
fn require_owned_session_file(file: &File) -> io::Result<std::fs::Metadata> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata()?;
    if metadata.file_type().is_file()
        && metadata.uid() == unsafe { libc::geteuid() }
        && metadata.nlink() == 1
    {
        Ok(metadata)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted image is not a user-owned, single-link regular file",
        ))
    }
}

#[cfg(unix)]
fn restrict_private_session_file(file: &File) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    require_owned_session_file(file)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    if file.metadata()?.mode() & 0o777 == 0o600 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image mode verification failed after fchmod",
        ))
    }
}

#[cfg(unix)]
fn session_entry_matches(
    directory: &SessionDirectory,
    file: &File,
    name: &OsStr,
) -> io::Result<bool> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::MetadataExt as _;

    let opened = file.metadata()?;
    let name = session_c_name(name)?;
    // SAFETY: the held directory descriptor, NUL-terminated name, and output
    // buffer are valid. AT_SYMLINK_NOFOLLOW compares the directory entry itself.
    let mut current = unsafe { std::mem::zeroed::<libc::stat>() };
    if unsafe {
        libc::fstatat(
            directory.file.as_raw_fd(),
            name.as_ptr(),
            std::ptr::addr_of_mut!(current),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err(error)
        };
    }
    #[allow(clippy::unnecessary_cast)]
    Ok(current.st_mode & libc::S_IFMT == libc::S_IFREG
        && opened.dev() == current.st_dev as u64
        && opened.ino() == current.st_ino as u64)
}

#[cfg(unix)]
fn open_existing_private_file_in_session(
    directory: &SessionDirectory,
    name: &OsStr,
) -> io::Result<File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let c_name = session_c_name(name)?;
    // SAFETY: the name and held directory descriptor are valid. O_NOFOLLOW
    // rejects a leaf symlink, and a successful open transfers a new descriptor.
    let descriptor = unsafe {
        libc::openat(
            directory.file.as_raw_fd(),
            c_name.as_ptr(),
            libc::O_RDWR | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0 as libc::c_uint,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new owned descriptor.
    let file = unsafe { File::from_raw_fd(descriptor) };
    restrict_private_session_file(&file)?;
    if !session_entry_matches(directory, &file, name)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image entry changed while it was opened",
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_existing_private_file_in_session(
    directory: &SessionDirectory,
    name: &OsStr,
) -> io::Result<File> {
    kettle_state::open_existing_private_file(&directory.path.join(name))
}

#[cfg(unix)]
fn remove_open_private_file_in_session(
    directory: &SessionDirectory,
    file: File,
    name: &OsStr,
) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;

    require_owned_session_file(&file)?;
    if !session_entry_matches(directory, &file, name)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image entry no longer identifies the open file",
        ));
    }
    let name = session_c_name(name)?;
    // SAFETY: the held directory descriptor and NUL-terminated child name are
    // valid. The identity check above prevents deleting a known replacement.
    if unsafe { libc::unlinkat(directory.file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    drop(file);
    Ok(())
}

#[cfg(not(unix))]
fn remove_open_private_file_in_session(
    directory: &SessionDirectory,
    file: File,
    name: &OsStr,
) -> io::Result<()> {
    kettle_state::remove_open_private_file(file, &directory.path.join(name))
}

#[cfg(unix)]
fn discard_private_file_in_session(directory: &SessionDirectory, file: File, name: &OsStr) {
    if let Err(error) = remove_open_private_file_in_session(directory, file, name) {
        log::debug!(
            "failed to discard partial pasted image {}: {error}",
            directory.path.join(name).display()
        );
    }
}

#[cfg(not(unix))]
fn discard_private_file_in_session(directory: &SessionDirectory, file: File, name: &OsStr) {
    discard_private_file(file, &directory.path.join(name));
}

#[cfg(unix)]
fn session_directory_entry_names(
    directory: &SessionDirectory,
    limit: usize,
) -> io::Result<Vec<OsString>> {
    use errno::{Errno, errno, set_errno};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;

    let current = std::ffi::CString::new(".").expect("static path has no NUL");
    // Open "." relative to the retained capability to get a readable
    // descriptor with an independent directory offset on every Unix.
    let descriptor = unsafe {
        libc::openat(
            directory.file.as_raw_fd(),
            current.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0 as libc::c_uint,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new owned descriptor.
    let descriptor = unsafe { File::from_raw_fd(descriptor) }.into_raw_fd();
    // SAFETY: fdopendir takes ownership of `descriptor` on success.
    let stream = unsafe { libc::fdopendir(descriptor) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopendir did not consume the descriptor on failure.
        unsafe {
            libc::close(descriptor);
        }
        return Err(error);
    }
    struct DirectoryStream(*mut libc::DIR);
    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            // SAFETY: the stream is owned and closed exactly once.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = DirectoryStream(stream);
    let mut names = Vec::with_capacity(limit);
    while names.len() < limit {
        set_errno(Errno(0));
        // SAFETY: the stream remains live, and the returned entry is borrowed
        // only until the next readdir call.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = errno();
            if error.0 == 0 {
                break;
            }
            return Err(io::Error::from_raw_os_error(error.0));
        }
        // SAFETY: POSIX dirent names are NUL terminated.
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        names.push(OsStr::from_bytes(name).to_os_string());
    }
    Ok(names)
}

#[cfg(not(unix))]
fn session_directory_entry_names(
    directory: &SessionDirectory,
    limit: usize,
) -> io::Result<Vec<OsString>> {
    std::fs::read_dir(&directory.path)?
        .take(limit)
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect()
}

#[cfg(not(windows))]
fn discard_private_file(file: File, path: &Path) {
    // Never fall back to a pathname delete: if the entry changed after
    // creation, preserving the replacement is safer than guessing.
    if let Err(error) = kettle_state::remove_open_private_file(file, path) {
        log::debug!(
            "failed to discard partial pasted image {}: {error}",
            path.display()
        );
    }
}

#[cfg(windows)]
fn discard_private_file(file: File, path: &Path) {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };

    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // The private creator requested DELETE on this exact handle. Marking that
    // kernel object is both race-free and compatible with its intentionally
    // restrictive share mode.
    let result = unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            std::ptr::addr_of!(disposition).cast(),
            u32::try_from(std::mem::size_of::<FILE_DISPOSITION_INFO>())
                .expect("FILE_DISPOSITION_INFO size fits u32"),
        )
    };
    if let Err(error) = result {
        log::debug!(
            "failed to discard partial pasted image {}: {error}",
            path.display()
        );
    }
}

/// Copy `size` bytes of `source` into a new private file `name` in the held
/// session `directory`, as `how` says, and return the file. A clone the file
/// system cannot make falls back to bytes.
#[cfg(unix)]
fn copy_into_session(
    directory: &SessionDirectory,
    name: &OsStr,
    source: &File,
    (size, how, keep_free): (u64, Copying, u64),
    (stop, deadline): (&dyn Fn() -> bool, Instant),
    pending: &std::sync::atomic::AtomicU64,
) -> io::Result<File> {
    let asked_to_end = || -> io::Result<()> {
        if stop() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "the copy was stopped",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the copy ran past its deadline",
            ));
        }
        Ok(())
    };
    // A finished copy that an end was asked for meanwhile goes too.
    let keep_unless_ended = |file: File| match asked_to_end() {
        Ok(()) => Ok(file),
        Err(error) => {
            discard_private_file_in_session(directory, file, name);
            Err(error)
        }
    };
    asked_to_end()?;
    #[cfg(target_os = "macos")]
    if how == Copying::CloneFirst {
        match clone_into_session(directory, name, source) {
            Err(error) if clone_unsupported(&error) => {}
            Ok(file) => return keep_unless_ended(file),
            Err(error) => return Err(error),
        }
    }
    let file = create_private_file_in_session(directory, name)?;
    #[cfg(target_os = "linux")]
    if how == Copying::CloneFirst {
        use std::os::fd::AsRawFd as _;
        // SAFETY: both descriptors are open, and FICLONE only reads the
        // source's and writes the new file's blocks.
        if unsafe { libc::ioctl(file.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) } == 0 {
            return keep_unless_ended(file);
        }
        let error = io::Error::last_os_error();
        if !clone_unsupported(&error) {
            discard_private_file_in_session(directory, file, name);
            return Err(error);
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = how;
    // What every byte copy of the store has left to write must fit before
    // each chunk, keeping the reserve: two copies never count it twice.
    let share = Pending::new(pending, size);
    let room = |left: u64| {
        share.left(left);
        require_free(
            directory,
            pending.load(std::sync::atomic::Ordering::Acquire),
            keep_free,
        )
    };
    match copy_chunks(source, &file, size, &asked_to_end, &room) {
        Ok(()) => keep_unless_ended(file),
        Err(error) => {
            discard_private_file_in_session(directory, file, name);
            Err(error)
        }
    }
}

#[cfg(not(unix))]
fn copy_into_session(
    _directory: &SessionDirectory,
    _name: &OsStr,
    _source: &File,
    _size: (u64, Copying, u64),
    _stop: (&dyn Fn() -> bool, Instant),
    _pending: &std::sync::atomic::AtomicU64,
) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "videos are copied only on Unix",
    ))
}

/// Clone `source` as `name` in the held session `directory`, owner-only,
/// and open it. The clone takes the source's mode, so it is made
/// owner-only before it is opened, inside a directory only the owner may
/// enter.
#[cfg(target_os = "macos")]
fn clone_into_session(
    directory: &SessionDirectory,
    name: &OsStr,
    source: &File,
) -> io::Result<File> {
    use std::os::fd::AsRawFd as _;
    /// `sys/clonefile.h`: do not follow a link at the destination, and take
    /// no ownership from the source.
    const CLONE_NOFOLLOW: u32 = 0x0001;
    const CLONE_NOOWNERCOPY: u32 = 0x0002;

    let c_name = session_c_name(name)?;
    // SAFETY: both descriptors are open and the name is NUL-terminated; the
    // call creates the clone and fails if the name exists.
    if unsafe {
        libc::fclonefileat(
            source.as_raw_fd(),
            directory.file.as_raw_fd(),
            c_name.as_ptr(),
            CLONE_NOFOLLOW | CLONE_NOOWNERCOPY,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above; AT_SYMLINK_NOFOLLOW changes the entry itself.
    let made_private = unsafe {
        libc::fchmodat(
            directory.file.as_raw_fd(),
            c_name.as_ptr(),
            0o600,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0;
    let opened = if made_private {
        open_existing_private_file_in_session(directory, name)
    } else {
        Err(io::Error::last_os_error())
    };
    if opened.is_err() {
        // SAFETY: as above. The name was free until the clone took it, in a
        // directory only this user may change.
        unsafe { libc::unlinkat(directory.file.as_raw_fd(), c_name.as_ptr(), 0) };
    }
    opened
}

/// Whether a clone failed only because this file system, or this pair of
/// volumes, cannot make one, so bytes will do.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn clone_unsupported(error: &io::Error) -> bool {
    // ENOTSUP and EOPNOTSUPP are one number on Linux and two on macOS.
    error.raw_os_error().is_some_and(|code| {
        [
            libc::EXDEV,
            libc::ENOTSUP,
            libc::EOPNOTSUPP,
            libc::EINVAL,
            libc::ENOTTY,
            libc::ENOSYS,
        ]
        .contains(&code)
    })
}

/// One byte copy's share of its store's pending bytes: what it has left to
/// write, updated as it writes and given back when it ends.
#[cfg(unix)]
struct Pending<'a> {
    total: &'a std::sync::atomic::AtomicU64,
    left: std::cell::Cell<u64>,
}

#[cfg(unix)]
impl<'a> Pending<'a> {
    fn new(total: &'a std::sync::atomic::AtomicU64, size: u64) -> Self {
        total.fetch_add(size, std::sync::atomic::Ordering::AcqRel);
        Self {
            total,
            left: std::cell::Cell::new(size),
        }
    }

    /// This copy now has `left` bytes to write, never more than before.
    fn left(&self, left: u64) {
        let written = self.left.get().saturating_sub(left);
        self.left.set(self.left.get() - written);
        self.total
            .fetch_sub(written, std::sync::atomic::Ordering::AcqRel);
    }
}

#[cfg(unix)]
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.total
            .fetch_sub(self.left.get(), std::sync::atomic::Ordering::AcqRel);
    }
}

/// Refuse a `size` byte copy into the held session `directory` that would
/// leave less than `keep` bytes free on its volume.
#[cfg(unix)]
fn require_free(directory: &SessionDirectory, size: u64, keep: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: the descriptor is open and the buffer is the call's own type.
    let mut stats = unsafe { std::mem::zeroed::<libc::statvfs>() };
    if unsafe { libc::fstatvfs(directory.file.as_raw_fd(), &mut stats) } != 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(clippy::unnecessary_cast)]
    let free = (stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64);
    if free < size.saturating_add(keep) {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            format!("a {size} byte copy would leave less than {keep} bytes free"),
        ));
    }
    Ok(())
}

/// Copy the first `size` bytes of `source` to `dest`, a chunk at a time.
/// Before each chunk `ended` says whether to stop and `room` whether what is
/// left still fits. A source that ends early is refused.
#[cfg(unix)]
fn copy_chunks(
    source: &File,
    dest: &File,
    size: u64,
    ended: &dyn Fn() -> io::Result<()>,
    room: &dyn Fn(u64) -> io::Result<()>,
) -> io::Result<()> {
    use std::os::unix::fs::FileExt as _;
    let mut buffer = vec![0_u8; COPY_CHUNK];
    let mut offset = 0_u64;
    while offset < size {
        ended()?;
        room(size - offset)?;
        let want = usize::try_from(size - offset)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = match source.read_at(&mut buffer[..want], offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the source ended before its size",
                ));
            }
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        dest.write_all_at(&buffer[..read], offset)?;
        offset += read as u64;
    }
    Ok(())
}

fn budget_error(files: usize, bytes: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::QuotaExceeded,
        format!("pasted-image budget reached ({files} files, {bytes} bytes)"),
    )
}

struct BudgetWriter<W> {
    inner: W,
    remaining: u64,
    written: u64,
    exceeded: bool,
}

impl<W> BudgetWriter<W> {
    fn new(inner: W, remaining: u64) -> Self {
        Self {
            inner,
            remaining,
            written: 0,
            exceeded: false,
        }
    }

    fn exceeded(&self) -> bool {
        self.exceeded
    }

    fn written(&self) -> u64 {
        self.written
    }

    fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: io::Write> io::Write for BudgetWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let requested = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        if requested > self.remaining {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                "encoded PNG exceeds the remaining pasted-image budget",
            ));
        }
        let written = self.inner.write(buffer)?;
        let written = u64::try_from(written).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "encoded PNG writer returned an invalid byte count",
            )
        })?;
        self.remaining = self.remaining.checked_sub(written).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "encoded PNG writer exceeded its declared byte count",
            )
        })?;
        self.written = self.written.checked_add(written).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::QuotaExceeded,
                "encoded PNG byte count overflowed",
            )
        })?;
        Ok(usize::try_from(written).expect("written originated as usize"))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(unix)]
fn same_open_file_identity(left: &File, right: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> io::Result<(u64, [u8; 16])> {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };

    let mut information = FILE_ID_INFO::default();
    // SAFETY: `file` is live and `information` is a correctly sized, writable
    // output buffer.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileIdInfo,
            std::ptr::addr_of_mut!(information).cast(),
            u32::try_from(std::mem::size_of::<FILE_ID_INFO>()).expect("FILE_ID_INFO size fits u32"),
        )
    }
    .map_err(io::Error::other)?;
    Ok((
        information.VolumeSerialNumber,
        information.FileId.Identifier,
    ))
}

#[cfg(windows)]
fn same_open_file_identity(left: &File, right: &File) -> io::Result<bool> {
    Ok(windows_file_identity(left)? == windows_file_identity(right)?)
}

#[cfg(not(any(unix, windows)))]
fn same_open_file_identity(_left: &File, _right: &File) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "open-file identity is unavailable on this platform",
    ))
}

#[cfg(unix)]
fn session_directory_matches_path(directory: &SessionDirectory) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let current = std::fs::symlink_metadata(&directory.path)?;
    Ok(current.file_type().is_dir()
        && current.uid() == unsafe { libc::geteuid() }
        && current.mode() & 0o777 == 0o700
        && current.dev() == directory.device
        && current.ino() == directory.inode)
}

#[cfg(windows)]
fn session_directory_matches_path(directory: &SessionDirectory) -> io::Result<bool> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut options = OpenOptions::new();
    options
        .access_mode((FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES).0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0);
    let current = options.open(&directory.path)?;
    let metadata = current.metadata()?;
    Ok(metadata.file_type().is_dir()
        && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
        && same_open_file_identity(&directory.file, &current)?)
}

#[cfg(not(any(unix, windows)))]
fn session_directory_matches_path(directory: &SessionDirectory) -> io::Result<bool> {
    Ok(std::fs::symlink_metadata(&directory.path)?
        .file_type()
        .is_dir())
}

fn verify_session_directory_path(directory: &SessionDirectory) -> io::Result<()> {
    if session_directory_matches_path(directory)? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image session path no longer identifies the held directory",
        ))
    }
}

#[cfg(unix)]
fn open_session_directory(path: &Path) -> io::Result<SessionDirectory> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "pasted-image session is not an owner-private ordinary directory: {}",
                path.display()
            ),
        ));
    }
    Ok(SessionDirectory {
        path: path.to_path_buf(),
        file,
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn open_session_directory(path: &Path) -> io::Result<SessionDirectory> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut options = OpenOptions::new();
    options
        .access_mode((FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES).0)
        // Denying delete-sharing pins this directory name/identity while files
        // are enumerated, created, reopened, or removed beneath it.
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_dir()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "pasted-image session is not a non-reparse directory: {}",
                path.display()
            ),
        ));
    }
    Ok(SessionDirectory {
        path: path.to_path_buf(),
        file,
    })
}

#[cfg(not(any(unix, windows)))]
fn open_session_directory(path: &Path) -> io::Result<SessionDirectory> {
    let file = File::open(path)?;
    if !file.metadata()?.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "pasted-image session is not an ordinary directory: {}",
                path.display()
            ),
        ));
    }
    Ok(SessionDirectory {
        path: path.to_path_buf(),
        file,
    })
}

#[cfg(unix)]
fn remove_session_directory(directory: SessionDirectory) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let current = std::fs::symlink_metadata(&directory.path)?;
    if !current.file_type().is_dir()
        || current.uid() != unsafe { libc::geteuid() }
        || current.mode() & 0o777 != 0o700
        || current.dev() != directory.device
        || current.ino() != directory.inode
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image session path no longer identifies the held directory",
        ));
    }
    std::fs::remove_dir(&directory.path)
}

#[cfg(windows)]
fn remove_session_directory(directory: SessionDirectory) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FileDispositionInfo, SetFileInformationByHandle,
    };

    let original_identity = windows_file_identity(&directory.file)?;
    let mut transition_options = OpenOptions::new();
    transition_options
        .access_mode((FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES).0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0);
    // This no-DELETE transition handle can coexist with the name-pinning
    // lifetime handle. It preserves the exact identity across releasing that
    // pin and acquiring the final DELETE-capable handle.
    let transition = transition_options.open(&directory.path)?;
    if windows_file_identity(&transition)? != original_identity {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image session path no longer identifies the held directory",
        ));
    }
    drop(directory.file);
    let mut deletion_options = OpenOptions::new();
    deletion_options
        .access_mode((FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | DELETE).0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0);
    let deletion = deletion_options.open(&directory.path)?;
    if windows_file_identity(&deletion)? != original_identity
        || !same_open_file_identity(&transition, &deletion)?
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pasted-image session changed while acquiring delete access",
        ));
    }
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `deletion` has DELETE access and the buffer exactly matches the
    // requested information class.
    unsafe {
        SetFileInformationByHandle(
            HANDLE(deletion.as_raw_handle()),
            FileDispositionInfo,
            std::ptr::addr_of!(disposition).cast(),
            u32::try_from(std::mem::size_of::<FILE_DISPOSITION_INFO>())
                .expect("FILE_DISPOSITION_INFO size fits u32"),
        )
    }
    .map_err(io::Error::other)
}

#[cfg(not(any(unix, windows)))]
fn remove_session_directory(directory: SessionDirectory) -> io::Result<()> {
    let _held = directory.file;
    std::fs::remove_dir(directory.path)
}

#[cfg(unix)]
fn process_is_definitely_dead(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 performs only an existence/permission probe.
    let result = unsafe { libc::kill(pid, 0) };
    result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(windows)]
fn process_is_definitely_dead(pid: u32) -> bool {
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, STILL_ACTIVE, WIN32_ERROR,
    };
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: the requested access is query-only and every successful handle
    // is closed below.
    let process = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(process) => process,
        Err(error) => {
            // Access denial and transient failures are not evidence of death.
            return WIN32_ERROR::from_error(&error) == Some(ERROR_INVALID_PARAMETER);
        }
    };
    let mut exit_code = 0_u32;
    // SAFETY: `process` is live and `exit_code` is writable for the call.
    let queried = unsafe { GetExitCodeProcess(process, &mut exit_code) }.is_ok();
    // SAFETY: OpenProcess transferred this handle to us.
    let _ = unsafe { CloseHandle(process) };
    queried && exit_code != STILL_ACTIVE.0 as u32
}

#[cfg(not(any(unix, windows)))]
fn process_is_definitely_dead(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn scratch(name: &str) -> PathBuf {
        scratch_root().join(format!(
            "kettle-paste-test-{name}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos()),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn stale_session(root: &Path, pid: u32) -> (PathBuf, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
            .saturating_add(u128::from(TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)));
        let directory = root.join(format!("{}{pid}-{nonce}", PASTED.prefix));
        let image = directory.join("0001.png");
        let mut file = create_private_file(&image).expect("create stale image");
        file.write_all(b"private image bytes")
            .expect("write stale image");
        drop(file);
        (directory, image)
    }

    #[test]
    fn saves_rgba_as_a_real_png() {
        let dir = scratch("save");
        let mut images = PastedImages::with_dir(dir.clone());
        // 2x2 opaque red.
        let rgba = [255u8, 0, 0, 255].repeat(4);
        let path = images.save_rgba(2, 2, &rgba, true).expect("save");
        assert!(path.exists(), "the PNG must actually be written");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            &bytes[..8],
            b"\x89PNG\r\n\x1a\n",
            "must be a real PNG, not raw bytes"
        );
        // Round-trip through the decoder to prove the geometry survived.
        let decoded = image::load_from_memory(&bytes).expect("decode");
        assert_eq!((decoded.width(), decoded.height()), (2, 2));
        // A second paste gets its own file rather than clobbering the first.
        let second = images.save_rgba(2, 2, &rgba, true).expect("save 2");
        assert_ne!(path, second);
        images.cleanup();
        assert!(!dir.exists(), "cleanup removes the whole directory");
    }

    #[test]
    fn preview_is_bounded_aspect_preserving_and_path_pinned() {
        let dir = scratch("preview");
        let mut images = PastedImages::with_dir(dir.clone());
        let rgba = [30u8, 60, 90, 255].repeat(640 * 360);
        let path = images.save_rgba(640, 360, &rgba, true).expect("save");
        let preview = images
            .take_preview_for_path(&path)
            .expect("managed preview");
        assert_eq!(
            (preview.original_width, preview.original_height),
            (640, 360)
        );
        assert_eq!((preview.image.width, preview.image.height), (256, 144));
        assert!(preview.image.byte_len() <= (PREVIEW_MAX_WIDTH * PREVIEW_MAX_HEIGHT * 4) as usize);
        assert!(
            images
                .take_preview_for_path(&dir.join("not-created-by-kettle.png"))
                .is_none(),
            "arbitrary paths must never acquire a thumbnail"
        );
        assert!(
            images.take_preview_for_path(&path).is_none(),
            "the bounded pixel reservation moves into the receipt instead of \
             living for the rest of the process"
        );
        assert!(
            images.path_still_matches(&path),
            "moving the preview must not release its file identity anchor"
        );
        images.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn receipt_open_rejects_a_replaced_image_path() {
        let dir = scratch("receipt-identity");
        let mut images = PastedImages::with_dir(dir.clone());
        let path = images
            .save_rgba(1, 1, &[10, 20, 30, 255], true)
            .expect("save managed image");
        assert!(images.path_still_matches(&path));

        let replacement = dir.join("replacement.png");
        std::fs::write(&replacement, b"not the retained image").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(
            !images.path_still_matches(&path),
            "a replacement must not be handed to the desktop opener"
        );
        std::fs::remove_file(&path).unwrap();
        images.cleanup();
    }

    #[test]
    fn disabled_receipts_do_not_build_or_retain_a_preview() {
        let dir = scratch("preview-disabled");
        let mut images = PastedImages::with_dir(dir.clone());
        let rgba = [30u8, 60, 90, 255].repeat(640 * 360);
        let path = images
            .save_rgba(640, 360, &rgba, false)
            .expect("the PNG still materializes");
        assert!(path.exists());
        assert!(
            images.take_preview_for_path(&path).is_none(),
            "a disabled receipt must pay no thumbnail allocation"
        );
        images.cleanup();
    }

    #[test]
    fn abandoned_paste_discards_only_its_managed_previews() {
        let dir = scratch("preview-abandoned");
        let mut images = PastedImages::with_dir(dir.clone());
        let rgba = [30u8, 60, 90, 255].repeat(4);
        let abandoned = images.save_rgba(2, 2, &rgba, true).expect("save first");
        let retained = images.save_rgba(2, 2, &rgba, true).expect("save second");

        images.discard_previews_for_paths(std::slice::from_ref(&abandoned));
        assert!(
            images.take_preview_for_path(&abandoned).is_none(),
            "an abandoned confirmation must release its renderer copy"
        );
        assert!(
            images.take_preview_for_path(&retained).is_some(),
            "discarding one paste must not release another receipt candidate"
        );
        assert!(
            abandoned.exists() && retained.exists(),
            "PNG lifetime is unchanged"
        );
        images.cleanup();
    }

    #[cfg(windows)]
    #[test]
    fn default_windows_scratch_directory_uses_local_app_data() {
        let images = PastedImages::new(PASTED);
        assert_eq!(
            images.dir.parent(),
            Some(scratch_root().as_path()),
            "Windows scratch images must not use a temp directory shared with sandbox principals"
        );
    }

    #[test]
    fn rejects_malformed_geometry_instead_of_panicking() {
        let dir = scratch("malformed");
        let mut images = PastedImages::with_dir(dir.clone());
        // Byte count disagrees with the declared size — the encoder would index
        // out of bounds if this were trusted.
        assert!(images.save_rgba(4, 4, &[0u8; 8], true).is_err());
        assert!(images.save_rgba(0, 4, &[], true).is_err(), "zero dimension");
        assert!(images.save_rgba(4, 0, &[], true).is_err(), "zero dimension");
        assert!(
            images
                .save_rgba(MAX_DIMENSION + 1, 1, &[0u8; 4], true)
                .is_err(),
            "absurd dimensions are refused before allocating"
        );
        assert!(
            images
                .save_rgba(MAX_DIMENSION, 4_097, &[], true)
                .unwrap_err()
                .to_string()
                .contains("RGBA buffer exceeds"),
            "declared source buffers above 256 MiB are refused before byte access"
        );
        // A rejected paste must not create anything on disk.
        assert!(!dir.exists());
        images.cleanup();
    }

    #[test]
    fn enforces_the_per_session_file_budget() {
        let dir = scratch("budget");
        let mut images = PastedImages::with_dir(dir.clone());
        let rgba = vec![0u8, 0, 0, 255];
        for i in 0..PASTED.max_files {
            images.save_rgba(1, 1, &rgba, true).unwrap_or_else(|e| {
                panic!("save {i} within budget failed: {e}");
            });
        }
        assert!(
            images.save_rgba(1, 1, &rgba, true).is_err(),
            "a paste loop must not be able to fill the disk"
        );
        images.cleanup();
        assert!(!dir.exists());
    }

    #[test]
    fn final_encoded_png_cannot_cross_the_aggregate_byte_budget() {
        let dir = scratch("encoded-budget");
        let mut images = PastedImages::with_dir(dir.clone());
        images.bytes = PASTED.max_bytes - 1;
        let error = images
            .save_rgba(1, 1, &[0, 0, 0, 255], true)
            .expect_err("a PNG cannot fit in one remaining byte");
        assert_eq!(error.kind(), io::ErrorKind::QuotaExceeded);
        assert_eq!(images.bytes, PASTED.max_bytes - 1);
        assert_eq!(images.seq, 0, "failed encodes do not consume a sequence");
        assert!(
            images.files.is_empty(),
            "failed PNGs are never live artifacts"
        );
        assert!(
            std::fs::read_dir(&dir)
                .expect("failed encode leaves a verified empty session")
                .next()
                .is_none(),
            "the partial PNG must be removed by its original handle"
        );
        images.bytes = 0;
        let retry = images
            .save_rgba(1, 1, &[0, 0, 0, 255], true)
            .expect("the failed sequence is reusable");
        assert_eq!(retry.file_name(), Some(std::ffi::OsStr::new("0001.png")));
        images.cleanup();
        assert!(!dir.exists());
    }

    #[test]
    fn bounded_writer_counts_only_bytes_it_accepts() {
        let mut writer = BudgetWriter::new(Vec::new(), 3);
        writer.write_all(&[1, 2, 3]).expect("exact budget");
        let error = writer.write_all(&[4]).expect_err("over budget");
        assert_eq!(error.kind(), io::ErrorKind::QuotaExceeded);
        assert!(writer.exceeded());
        assert_eq!(writer.written(), 3);
        assert_eq!(writer.into_inner(), vec![1, 2, 3]);
    }

    #[test]
    fn retained_handle_must_match_the_creator_identity() {
        let dir = scratch("identity-compare");
        let first_path = dir.join("first.png");
        let first = create_private_file(&first_path).expect("first creator");
        let first_reopened =
            kettle_state::open_existing_private_file(&first_path).expect("reopen first");
        assert!(
            same_open_file_identity(&first, &first_reopened).expect("compare same file"),
            "the cooperative handle must identify the creator's exact object"
        );

        let second_path = dir.join("second.png");
        let second = create_private_file(&second_path).expect("second creator");
        assert!(
            !same_open_file_identity(&first, &second).expect("compare different files"),
            "two private files must not alias one identity"
        );

        drop(first_reopened);
        discard_private_file(first, &first_path);
        discard_private_file(second, &second_path);
        std::fs::remove_dir(dir).expect("remove identity test directory");
    }

    #[test]
    fn session_and_image_names_require_the_exact_creator_grammar() {
        assert_eq!(
            parse_session_name("kettle-paste-42-0", PASTED.prefix),
            Some(SessionName { pid: 42, nonce: 0 })
        );
        for near_miss in [
            "kettle-paste-0-1",
            "kettle-paste-042-1",
            "kettle-paste-4294967296-1",
            "kettle-paste-42-01",
            "kettle-paste-42",
            "kettle-paste-42-1-extra",
            "kettle-paste-42-340282366920938463463374607431768211456",
            "other-42-1",
        ] {
            assert_eq!(
                parse_session_name(near_miss, PASTED.prefix),
                None,
                "{near_miss}"
            );
        }
        let pasted = |name| parse_image_name(name, PASTED);
        assert_eq!(pasted("0001.png"), Some(1));
        assert_eq!(pasted("0064.png"), Some(64));
        for near_miss in [
            "1.png",
            "0000.png",
            "00001.png",
            "0065.png",
            "9999.png",
            "10000.png",
            "0001.PNG",
            "0001.png.extra",
            "18446744073709551616.png",
            "note.txt",
        ] {
            assert_eq!(pasted(near_miss), None, "{near_miss}");
        }
        // A viewer-copy session keeps counting past its file count, in the
        // same grammar, under its own prefix.
        assert_eq!(
            parse_session_name("kettle-open-42-0", OPENED.prefix),
            Some(SessionName { pid: 42, nonce: 0 })
        );
        assert_eq!(parse_session_name("kettle-open-42-0", PASTED.prefix), None);
        let opened = |name| parse_image_name(name, OPENED);
        assert_eq!(opened("0065.png"), Some(65));
        assert_eq!(opened("999999.png"), Some(999_999));
        for near_miss in ["0000.png", "00065.png", "1000000.png", "0001.mp4"] {
            assert_eq!(opened(near_miss), None, "{near_miss}");
        }
        // A video-copy session names each copy with one of its containers'
        // extensions, and nothing else.
        assert_eq!(
            parse_session_name("kettle-video-42-0", VIDEOS.prefix),
            Some(SessionName { pid: 42, nonce: 0 })
        );
        assert_eq!(parse_session_name("kettle-video-42-0", OPENED.prefix), None);
        let videos = |name| parse_image_name(name, VIDEOS);
        assert_eq!(videos("0001.mp4"), Some(1));
        assert_eq!(videos("0007.webm"), Some(7));
        assert_eq!(videos("0012.ts"), Some(12));
        for near_miss in [
            "0001.png",
            "0001.MP4",
            "0001.mp4.extra",
            "0001.",
            "0001",
            ".mp4",
            "0000.mp4",
            "1000000.mov",
        ] {
            assert_eq!(videos(near_miss), None, "{near_miss}");
        }
    }

    #[test]
    fn old_live_session_is_preserved_but_dead_session_is_reaped() {
        let root = scratch("stale-root");
        let pid = std::process::id();
        let (directory, image) = stale_session(&root, pid);
        let future = SystemTime::now() + STALE_AFTER + Duration::from_secs(1);

        sweep_stale_in(&root, future, PASTED, |candidate| {
            assert_eq!(candidate, pid);
            false
        });
        assert!(image.exists(), "age is never sufficient deletion authority");

        sweep_stale_in(&root, future, PASTED, |candidate| {
            assert_eq!(candidate, pid);
            true
        });
        assert!(!directory.exists(), "a verified dead session is reclaimed");
        let _ = std::fs::remove_dir(&root);
    }

    #[test]
    fn stale_sweep_fails_closed_for_unknown_and_hard_linked_entries() {
        let root = scratch("stale-adversarial");
        let pid = std::process::id();
        let (unknown_dir, unknown_image) = stale_session(&root, pid);
        let unknown = unknown_dir.join("notes.txt");
        let unknown_file = create_private_file(&unknown).expect("unknown sibling");
        drop(unknown_file);

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
            .saturating_add(10_000);
        let linked_dir = root.join(format!("{}{pid}-{nonce}", PASTED.prefix));
        let linked_image = linked_dir.join("0001.png");
        let linked = create_private_file(&linked_image).expect("linked image");
        drop(linked);
        let outside_link = root.join(format!("outside-hardlink-{nonce}.png"));
        std::fs::hard_link(&linked_image, &outside_link).expect("create hard-link sentinel");

        let future = SystemTime::now() + STALE_AFTER + Duration::from_secs(1);
        sweep_stale_in(&root, future, PASTED, |_| true);
        assert!(unknown_image.exists() && unknown.exists());
        assert!(linked_image.exists() && outside_link.exists());

        std::fs::remove_file(unknown_image).expect("remove image");
        std::fs::remove_file(unknown).expect("remove unknown");
        std::fs::remove_dir(unknown_dir).expect("remove unknown dir");
        std::fs::remove_file(linked_image).expect("remove linked image");
        std::fs::remove_file(outside_link).expect("remove outside link");
        std::fs::remove_dir(linked_dir).expect("remove linked dir");
        std::fs::remove_dir(root).expect("remove test root");
    }

    #[test]
    fn current_process_is_never_reported_definitively_dead() {
        assert!(!process_is_definitely_dead(std::process::id()));
    }

    #[cfg(windows)]
    #[test]
    fn invalid_windows_pid_is_definitively_dead() {
        assert!(process_is_definitely_dead(u32::MAX));
    }

    #[cfg(unix)]
    #[test]
    fn held_directory_creation_never_writes_into_a_path_replacement() {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

        let path = scratch("held-create");
        let displaced = path.with_extension("displaced");
        let directory = establish_session_directory(&path, PASTED).expect("establish session");
        std::fs::rename(&path, &displaced).expect("displace held session");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("create path replacement");

        let name = OsStr::new("0001.png");
        let mut file = create_private_file_in_session(&directory, name).expect("create through fd");
        file.write_all(b"screenshot bytes")
            .expect("write through held directory");
        let reopened =
            open_existing_private_file_in_session(&directory, name).expect("reopen through fd");
        assert!(
            same_open_file_identity(&file, &reopened).expect("compare open files"),
            "reopen must retain the created file identity"
        );
        assert_eq!(
            session_directory_entry_names(&directory, 2).expect("enumerate through fd"),
            [OsString::from("0001.png")],
            "enumeration must remain anchored to the displaced held directory"
        );
        assert!(
            !path.join("0001.png").exists(),
            "clipboard bytes must never enter the pathname replacement"
        );
        assert_eq!(
            std::fs::read(displaced.join("0001.png")).expect("read held-directory file"),
            b"screenshot bytes"
        );

        drop(file);
        reopened
            .set_permissions(std::fs::Permissions::from_mode(0o400))
            .expect("narrow retained file permissions");
        remove_open_private_file_in_session(&directory, reopened, name).expect("remove exact file");
        assert!(
            !displaced.join(name).exists(),
            "fd-relative removal must unlink from the held directory"
        );
        drop(directory);
        std::fs::remove_dir(displaced).expect("remove displaced session");
        std::fs::remove_dir(path).expect("remove replacement");
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_preserves_a_path_replacement() {
        let dir = scratch("identity-replacement");
        let mut images = PastedImages::with_dir(dir.clone());
        let path = images
            .save_rgba(1, 1, &[1, 2, 3, 4], true)
            .expect("save original");
        std::fs::remove_file(&path).expect("unlink original while its handle is retained");
        let mut replacement = create_private_file(&path).expect("create replacement");
        replacement
            .write_all(b"replacement")
            .expect("write replacement");
        drop(replacement);

        images.cleanup();
        assert_eq!(
            std::fs::read(&path).expect("replacement survives"),
            b"replacement"
        );
        std::fs::remove_file(path).expect("remove replacement");
        std::fs::remove_dir(dir).expect("remove test directory");
    }

    /// A full viewer-copy store drops its oldest copies, by count and by
    /// bytes, and keeps counting; its sessions are swept under their own
    /// prefix, past the pasted store's numbering, and the pasted sweep
    /// leaves them alone.
    #[test]
    fn a_full_viewer_copy_store_drops_its_oldest() {
        // Copies old enough to drop, for the eviction itself.
        let aged = StoreKind {
            min_age: Duration::ZERO,
            ..OPENED
        };
        let dir = scratch("opened");
        let mut images = PastedImages::of_kind_in(aged, dir.clone());
        let pixel = [0u8, 0, 0, 255];
        let mut paths = Vec::new();
        for _ in 0..OPENED.max_files + 2 {
            paths.push(images.save_rgba(1, 1, &pixel, false).expect("save"));
        }
        assert_eq!(images.files.len(), OPENED.max_files);
        assert!(
            !paths[0].exists() && !paths[1].exists(),
            "oldest went first"
        );
        assert!(paths[2].exists() && paths.last().unwrap().exists());
        assert_eq!(images.seq, OPENED.max_files + 2, "numbers are not reused");
        let counted: u64 = images.files.iter().map(|image| image.bytes).sum();
        assert_eq!(images.bytes, counted);
        images.cleanup();
        assert!(!dir.exists());

        // By bytes: with one copy in it, the store has less room left than
        // a second copy's bound, however well the first compressed.
        let small = StoreKind {
            max_bytes: 8 * 1024,
            ..aged
        };
        let dir = scratch("opened-bytes");
        let mut images = PastedImages::of_kind_in(small, dir.clone());
        let mut state = 1u32;
        let noise: Vec<u8> = (0..32 * 32 * 4)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (state >> 16) as u8
            })
            .collect();
        let first = images.save_rgba(32, 32, &noise, false).expect("first");
        let second = images.save_rgba(32, 32, &noise, false).expect("second");
        assert!(!first.exists(), "the first made room for the second");
        assert!(second.exists() && images.files.len() == 1);
        assert!(images.bytes <= small.max_bytes);
        images.cleanup();

        // The crash sweep covers every store.
        assert_eq!(KINDS, [PASTED, OPENED, VIDEOS]);
        let root = scratch("opened-sweep");
        let pid = std::process::id();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let directory = root.join(format!("{}{pid}-{nonce}", OPENED.prefix));
        let image = directory.join("0100.png");
        drop(create_private_file(&image).expect("stale copy"));
        let future = SystemTime::now() + STALE_AFTER + Duration::from_secs(1);
        sweep_stale_in(&root, future, PASTED, |_| true);
        assert!(image.exists(), "not the pasted sweep's to take");
        sweep_stale_in(&root, future, OPENED, |_| true);
        assert!(!directory.exists(), "a dead viewer-copy session goes");
        let _ = std::fs::remove_dir(&root);
    }

    /// A copy is never dropped before its viewer has had a minute to read
    /// it: a store full of newer copies refuses another, as over quota, and
    /// keeps every one.
    #[test]
    fn a_viewer_copy_stays_its_minute() {
        let dir = scratch("opened-young");
        let mut images = PastedImages::of_kind_in(OPENED, dir.clone());
        let pixel = [0u8, 0, 0, 255];
        let paths: Vec<_> = (0..OPENED.max_files)
            .map(|_| images.save_rgba(1, 1, &pixel, false).expect("save"))
            .collect();
        let refused = images
            .save_rgba(1, 1, &pixel, false)
            .expect_err("every copy is too new to drop");
        assert_eq!(refused.kind(), io::ErrorKind::QuotaExceeded);
        assert!(paths.iter().all(|path| path.exists()));
        images.cleanup();
        assert!(!dir.exists());
    }

    /// A copy the store cannot delete still counts against its bounds, so
    /// the store never holds more than it may, and cleanup deletes it once
    /// it can. A closed store takes nothing more.
    #[cfg(unix)]
    #[test]
    fn a_copy_that_will_not_go_still_counts_until_cleanup() {
        use std::os::unix::fs::PermissionsExt as _;
        // SAFETY: `geteuid` has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("root deletes whatever a directory's mode says; skipped");
            return;
        }
        let aged = StoreKind {
            max_files: 3,
            min_age: Duration::ZERO,
            ..OPENED
        };
        let dir = scratch("opened-stuck");
        let mut images = PastedImages::of_kind_in(aged, dir.clone());
        let pixel = [0u8, 0, 0, 255];
        for _ in 0..aged.max_files {
            images.save_rgba(1, 1, &pixel, false).expect("save");
        }
        let held = images.bytes;
        // Nothing in the directory can be deleted, or created.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(images.save_rgba(1, 1, &pixel, false).is_err());
        assert_eq!(images.files.len() + images.stuck.len(), aged.max_files);
        assert_eq!(images.bytes, held, "still counted");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), aged.max_files);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        images.close();
        assert!(!dir.exists(), "cleanup deleted the stuck copies too");
        assert!(images.save_rgba(1, 1, &pixel, false).is_err(), "closed");
        assert!(!dir.exists());
    }

    /// A copy is marked downloaded on macOS through the store's own handle;
    /// a path the store does not hold is refused.
    #[test]
    fn a_viewer_copy_is_marked_downloaded() {
        let dir = scratch("quarantine");
        let mut images = PastedImages::of_kind_in(OPENED, dir.clone());
        let path = images
            .save_rgba(1, 1, &[0, 0, 0, 255], false)
            .expect("save");
        images.mark_downloaded(&path).expect("mark");
        assert_eq!(
            images
                .mark_downloaded(&dir.join("0099.png"))
                .map_err(|error| error.kind()),
            Err(io::ErrorKind::NotFound)
        );
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
        images.cleanup();
    }

    /// A source file of `len` patterned bytes, with `mode`, in its own
    /// private directory.
    #[cfg(unix)]
    fn video_source(len: usize, mode: u32) -> (kettle_test_support::PrivateTempDir, File) {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = kettle_test_support::private_tempdir("kettle-video-source-");
        let path = dir.path().join("clip.bin");
        let bytes: Vec<u8> = (0..len).map(|index| (index % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let file = File::open(&path).unwrap();
        (dir, file)
    }

    #[cfg(unix)]
    fn never() -> bool {
        false
    }

    #[cfg(unix)]
    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    /// A video copy is the source's bytes in a new owner-only file named
    /// with the extension asked for, which the store holds, marks and
    /// deletes like any other. A read-only source copies too, as its clone
    /// takes its mode before being made private.
    #[cfg(unix)]
    #[test]
    fn a_video_copy_is_a_private_copy_of_its_source() {
        use std::os::unix::fs::PermissionsExt as _;
        for (how, mode) in [
            (Copying::CloneFirst, 0o644),
            (Copying::CloneFirst, 0o444),
            (Copying::Bytes, 0o444),
        ] {
            let dir = scratch("video-copy");
            let mut copies = PastedImages::of_kind_in(VIDEOS_ANY_DISK, dir.clone());
            // Past one chunk, and not a whole number of them.
            let len = COPY_CHUNK * 2 + 4321;
            let (_source_dir, source) = video_source(len, mode);
            let path = copies
                .save_copy_as(&source, (len as u64, "webm"), how, (&never, soon()), |_| {
                    Ok(())
                })
                .unwrap_or_else(|error| panic!("{how:?} {mode:o}: {error}"));
            assert_eq!(path.extension(), Some(OsStr::new("webm")));
            let meta = std::fs::symlink_metadata(&path).unwrap();
            assert!(meta.is_file());
            assert_eq!(
                meta.permissions().mode() & 0o7777,
                0o600,
                "{how:?} {mode:o}"
            );
            let expected: Vec<u8> = (0..len).map(|index| (index % 251) as u8).collect();
            assert!(
                std::fs::read(&path).unwrap() == expected,
                "{how:?} {mode:o}"
            );
            assert!(copies.path_still_matches(&path));
            copies.mark_downloaded(&path).expect("mark");
            assert_eq!(copies.bytes, len as u64);
            copies.cleanup();
            assert!(!dir.exists(), "{how:?} {mode:o}");
        }
    }

    /// A byte copy stops between chunks when asked, or once its deadline
    /// passes, and a source shorter than it said is refused; none of them
    /// leaves a file behind or counts against the store.
    #[cfg(unix)]
    #[test]
    fn an_unfinished_copy_leaves_nothing() {
        let len = COPY_CHUNK + 1;
        let (_source_dir, source) = video_source(len, 0o600);
        let stopped = || true;
        let cases: [(&dyn Fn() -> bool, Instant, u64, io::ErrorKind); 3] = [
            (&stopped, soon(), len as u64, io::ErrorKind::Interrupted),
            (&never, Instant::now(), len as u64, io::ErrorKind::TimedOut),
            (&never, soon(), len as u64 + 1, io::ErrorKind::UnexpectedEof),
        ];
        for (stop, deadline, size, kind) in cases {
            let dir = scratch("video-unfinished");
            let mut copies = PastedImages::of_kind_in(VIDEOS_ANY_DISK, dir.clone());
            let error = copies
                .save_copy_as(
                    &source,
                    (size, "mp4"),
                    Copying::Bytes,
                    (stop, deadline),
                    |_| Ok(()),
                )
                .expect_err("unfinished");
            assert_eq!(error.kind(), kind);
            assert_eq!(
                (copies.files.len(), copies.stuck.len(), copies.bytes),
                (0, 0, 0)
            );
            let directory = copies.directory.as_ref().expect("session held");
            assert!(
                session_directory_entry_names(directory, 8)
                    .unwrap()
                    .is_empty(),
                "{kind:?}"
            );
            copies.cleanup();
            assert!(!dir.exists());
        }
    }

    /// A clone of a source that is not the size it was said to be, or a
    /// copy its check refuses, is discarded.
    #[cfg(unix)]
    #[test]
    fn a_copy_that_is_not_what_was_asked_is_discarded() {
        let len = 4096;
        let (_source_dir, source) = video_source(len, 0o600);
        for (size, refuse) in [(len as u64 - 1, false), (len as u64, true)] {
            let dir = scratch("video-discard");
            let mut copies = PastedImages::of_kind_in(VIDEOS_ANY_DISK, dir.clone());
            let checked = std::cell::Cell::new(false);
            let error = copies
                .save_copy(&source, size, "mov", &never, soon(), |copy| {
                    checked.set(true);
                    // As the caller's check does: the copy is the source's
                    // length, and what it holds is the video.
                    if refuse || copy.metadata()?.len() != len as u64 {
                        Err(io::Error::new(io::ErrorKind::InvalidData, "not the video"))
                    } else {
                        Ok(())
                    }
                })
                .expect_err("discarded");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            // A clone is the whole source, so a short one fails its length
            // before its check is asked. APFS clones; a byte copy, where a
            // file system cannot, copies only what it was asked.
            #[cfg(target_os = "macos")]
            assert_eq!(checked.get(), refuse);
            #[cfg(not(target_os = "macos"))]
            let _ = checked;
            assert_eq!((copies.files.len(), copies.bytes), (0, 0));
            let directory = copies.directory.as_ref().expect("session held");
            assert!(
                session_directory_entry_names(directory, 8)
                    .unwrap()
                    .is_empty()
            );
            copies.cleanup();
            assert!(!dir.exists());
        }
    }

    /// A copy is named only with one of its store's extensions, is never
    /// empty or past the store's bytes, and the store's extensions are
    /// exactly its containers'.
    #[cfg(unix)]
    #[test]
    fn a_copy_is_named_and_sized_within_its_store() {
        let (_source_dir, source) = video_source(16, 0o600);
        let dir = scratch("video-names");
        let mut copies = PastedImages::of_kind_in(VIDEOS_ANY_DISK, dir.clone());
        for (size, extension, kind) in [
            (16, "png", io::ErrorKind::InvalidInput),
            (16, "exe", io::ErrorKind::InvalidInput),
            (16, "MP4", io::ErrorKind::InvalidInput),
            (16, "../mp4", io::ErrorKind::InvalidInput),
            (0, "mp4", io::ErrorKind::QuotaExceeded),
            (VIDEOS.max_bytes + 1, "mp4", io::ErrorKind::QuotaExceeded),
        ] {
            let error = copies
                .save_copy(&source, size, extension, &never, soon(), |_| Ok(()))
                .expect_err(extension);
            assert_eq!(error.kind(), kind, "{size} {extension}");
        }
        assert!(!dir.exists(), "a refused copy writes nothing");
        let containers: Vec<_> = kettle_media::video::VideoContainer::ALL
            .iter()
            .map(|container| container.extension())
            .collect();
        assert_eq!(VIDEOS.extensions, containers.as_slice());
    }

    /// A store full of copies too new to drop refuses another, and a byte
    /// copy that would leave the volume too full is refused before it
    /// starts.
    #[cfg(unix)]
    #[test]
    fn a_full_store_or_volume_refuses_a_copy() {
        let (_source_dir, source) = video_source(16, 0o600);
        let dir = scratch("video-full");
        let mut copies = PastedImages::of_kind_in(VIDEOS_ANY_DISK, dir.clone());
        for _ in 0..VIDEOS.max_files {
            copies
                .save_copy(&source, 16, "mp4", &never, soon(), |_| Ok(()))
                .expect("room");
        }
        let error = copies
            .save_copy(&source, 16, "mp4", &never, soon(), |_| Ok(()))
            .expect_err("full of new copies");
        assert_eq!(error.kind(), io::ErrorKind::QuotaExceeded);
        let directory = copies.directory.as_ref().expect("session held");
        assert!(require_free(directory, 0, 0).is_ok());
        assert_eq!(
            require_free(directory, u64::MAX / 2, 0).map_err(|error| error.kind()),
            Err(io::ErrorKind::StorageFull)
        );
        assert_eq!(
            require_free(directory, 0, u64::MAX).map_err(|error| error.kind()),
            Err(io::ErrorKind::StorageFull)
        );
        copies.cleanup();
        assert!(!dir.exists());
        // A byte copy keeps the free space its store asks for: the video
        // store's 2 GiB, here more than any disk has.
        assert_eq!(VIDEOS.keep_free, 2 * 1024 * 1024 * 1024);
        let dir = scratch("video-keep-free");
        let greedy = StoreKind {
            keep_free: u64::MAX / 4,
            ..VIDEOS
        };
        let mut copies = PastedImages::of_kind_in(greedy, dir.clone());
        let error = copies
            .save_copy_as(
                &source,
                (16, "mp4"),
                Copying::Bytes,
                (&never, soon()),
                |_| Ok(()),
            )
            .expect_err("no disk keeps that much free");
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        copies.cleanup();
        assert!(!dir.exists());
    }

    /// A copy kept but never handed over goes when asked, and stops
    /// counting; one the store does not hold is not found.
    #[cfg(unix)]
    #[test]
    fn a_kept_copy_can_be_removed() {
        let (_source_dir, source) = video_source(64, 0o600);
        let dir = scratch("video-remove");
        let mut copies = PastedImages::of_kind_in(VIDEOS_ANY_DISK, dir.clone());
        let path = copies
            .save_copy(&source, 64, "mp4", &never, soon(), |_| Ok(()))
            .unwrap();
        assert_eq!((copies.held(), copies.bytes), (1, 64));
        copies.remove(&path).unwrap();
        assert!(!path.exists());
        assert_eq!((copies.held(), copies.bytes), (0, 0));
        assert_eq!(
            copies.remove(&path).map_err(|error| error.kind()),
            Err(io::ErrorKind::NotFound)
        );
        copies.cleanup();
        assert!(!dir.exists());
    }

    /// A kept copy that cannot be deleted stays counted, its bytes
    /// included, for cleanup to try again.
    #[cfg(unix)]
    #[test]
    fn a_copy_that_will_not_go_stays_charged() {
        let (_source_dir, source) = video_source(64, 0o600);
        let dir = scratch("video-stuck");
        let mut copies = PastedImages::of_kind_in(VIDEOS_ANY_DISK, dir.clone());
        let path = copies
            .save_copy(&source, 64, "mp4", &never, soon(), |_| Ok(()))
            .unwrap();
        // Another file takes its name, so the held one can no longer be
        // deleted by it.
        let other = dir.join("other");
        std::fs::write(&other, b"x").unwrap();
        std::fs::rename(&other, &path).unwrap();
        assert!(copies.remove(&path).is_err());
        assert_eq!((copies.held(), copies.counted_bytes()), (1, 64));
        std::fs::remove_file(&path).unwrap();
        copies.cleanup();
        assert!(!dir.exists());
    }

    /// Byte copies made at once keep the reserve between them: what another
    /// copy has yet to write counts against this one's room, and a copy's
    /// share is given back however it ends.
    #[cfg(unix)]
    #[test]
    fn copies_made_at_once_share_the_reserve() {
        let (_source_dir, source) = video_source(64, 0o600);
        let dir = scratch("video-pending");
        let mut copies = PastedImages::of_kind_in(VIDEOS_ANY_DISK, dir.clone());
        let counter = std::sync::Arc::clone(&copies.pending);
        let other = Pending::new(&counter, u64::MAX / 4);
        let error = copies
            .save_copy_as(
                &source,
                (64, "mp4"),
                Copying::Bytes,
                (&never, soon()),
                |_| Ok(()),
            )
            .expect_err("the other copy's bytes do not fit beside it");
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        drop(other);
        assert_eq!(copies.pending.load(std::sync::atomic::Ordering::Acquire), 0);
        copies
            .save_copy_as(
                &source,
                (64, "mp4"),
                Copying::Bytes,
                (&never, soon()),
                |_| Ok(()),
            )
            .expect("room once it is done");
        assert_eq!(copies.pending.load(std::sync::atomic::Ordering::Acquire), 0);
        copies.cleanup();
        assert!(!dir.exists());
    }

    /// A reserved copy holds its name, its place and its bytes until it is
    /// kept or given back, so copies made at once never pass the bounds or
    /// share a name; one finished after the store closed is discarded with
    /// the directory cleanup left for it.
    #[cfg(unix)]
    #[test]
    fn a_reserved_copy_holds_its_place() {
        let (_source_dir, source) = video_source(64, 0o600);
        let dir = scratch("video-reserve");
        let mut copies = PastedImages::of_kind_in(VIDEOS_ANY_DISK, dir.clone());
        let first = copies.reserve_copy(64, "mp4").unwrap();
        let second = copies.reserve_copy(64, "webm").unwrap();
        assert_ne!(first.next.name, second.next.name);
        let kept = first.next.sequence.max(second.next.sequence);
        assert_eq!((copies.held(), copies.counted_bytes()), (2, 128));
        let mut rest: Vec<_> = (2..VIDEOS.max_files)
            .map(|_| copies.reserve_copy(64, "mp4").unwrap())
            .collect();
        assert_eq!(
            copies
                .reserve_copy(64, "mp4")
                .map(|_| ())
                .map_err(|error| error.kind()),
            Err(io::ErrorKind::QuotaExceeded),
            "every place is reserved"
        );
        copies.release(rest.pop().unwrap());
        let again = copies.reserve_copy(64, "mp4").unwrap();
        for reservation in rest.drain(..).chain([again]) {
            copies.release(reservation);
        }
        // The second finishes first; the first then keeps its own name.
        let made = second.copy(&source, &never, soon()).unwrap();
        let second_path = copies.keep_copy(second, made, |_| Ok(())).unwrap();
        let made = first.copy(&source, &never, soon()).unwrap();
        let first_path = copies.keep_copy(first, made, |_| Ok(())).unwrap();
        assert_ne!(first_path, second_path);
        assert_eq!((copies.held(), copies.bytes), (2, 128));
        // Numbering goes on past both, whichever was kept last.
        let late = copies.reserve_copy(64, "webm").unwrap();
        assert!(late.next.sequence > kept, "{}", late.next.sequence);
        let made = late.copy(&source, &never, soon()).unwrap();
        copies.close();
        assert!(
            dir.exists(),
            "cleanup leaves the directory to the copy being made"
        );
        assert!(copies.keep_copy(late, made, |_| Ok(())).is_err());
        assert!(!dir.exists(), "the last one out takes the directory away");
    }

    /// A copy that an end is asked for while it is made is discarded, even
    /// once the clone or the last chunk is done.
    #[cfg(unix)]
    #[test]
    fn a_copy_ended_meanwhile_is_discarded() {
        let len = COPY_CHUNK;
        let (_source_dir, source) = video_source(len, 0o600);
        for (how, asks_before_ending) in [(Copying::CloneFirst, 1), (Copying::Bytes, 2)] {
            let dir = scratch("video-ended");
            let mut copies = PastedImages::of_kind_in(VIDEOS_ANY_DISK, dir.clone());
            let asked = std::cell::Cell::new(0);
            let stop = || {
                asked.set(asked.get() + 1);
                asked.get() > asks_before_ending
            };
            let reservation = copies.reserve_copy(len as u64, "mp4").unwrap();
            let error = reservation
                .copy_as(&source, how, &stop, soon())
                .expect_err("ended");
            assert_eq!(error.kind(), io::ErrorKind::Interrupted, "{how:?}");
            copies.release(reservation);
            let directory = copies.directory.as_ref().unwrap();
            assert!(
                session_directory_entry_names(directory, 8)
                    .unwrap()
                    .is_empty(),
                "{how:?}"
            );
            copies.cleanup();
            assert!(!dir.exists());
        }
    }

    /// A byte copy asks, before every chunk, whether to end and whether what
    /// is left still fits, and stops when either says so.
    #[cfg(unix)]
    #[test]
    fn the_reserve_is_asked_before_every_chunk() {
        let len = COPY_CHUNK * 2 + 10;
        let (_source_dir, source) = video_source(len, 0o600);
        let dest = tempfile::tempfile().unwrap();
        let asked = std::cell::RefCell::new(Vec::new());
        let room = |left: u64| {
            asked.borrow_mut().push(left);
            Ok(())
        };
        copy_chunks(&source, &dest, len as u64, &|| Ok(()), &room).unwrap();
        let chunk = COPY_CHUNK as u64;
        assert_eq!(
            *asked.borrow(),
            [len as u64, len as u64 - chunk, len as u64 - 2 * chunk]
        );
        // An end asked for before the second chunk stops it there.
        asked.borrow_mut().clear();
        let ends = std::cell::Cell::new(0);
        let ended = || {
            ends.set(ends.get() + 1);
            if ends.get() > 1 {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(())
            }
        };
        assert_eq!(
            copy_chunks(&source, &dest, len as u64, &ended, &room).map_err(|error| error.kind()),
            Err(io::ErrorKind::Interrupted)
        );
        assert_eq!(*asked.borrow(), [len as u64], "one chunk, then the end");
        let full = |left: u64| {
            if left < len as u64 {
                Err(io::Error::from(io::ErrorKind::StorageFull))
            } else {
                Ok(())
            }
        };
        assert_eq!(
            copy_chunks(&source, &dest, len as u64, &|| Ok(()), &full)
                .map_err(|error| error.kind()),
            Err(io::ErrorKind::StorageFull)
        );
    }

    /// A video-copy session a dead process left is reclaimed, its copies
    /// named with their containers' extensions.
    #[test]
    fn a_dead_video_session_is_reaped() {
        let root = scratch("video-stale-root");
        let directory = root.join(format!("{}{}-7", VIDEOS.prefix, std::process::id()));
        for name in ["0001.mp4", "0002.webm", "0003.ts"] {
            let mut file = create_private_file(&directory.join(name)).expect("create");
            file.write_all(b"private video bytes").expect("write");
        }
        let future = SystemTime::now() + STALE_AFTER + Duration::from_secs(1);
        sweep_stale_in(&root, future, VIDEOS, |_| true);
        assert!(
            !directory.exists(),
            "a verified dead video session is reclaimed"
        );
        let _ = std::fs::remove_dir(&root);
    }

    #[test]
    fn sweep_leaves_this_process_directory_alone() {
        // The production liveness probe independently protects this process.
        let mut images = PastedImages::new(PASTED);
        let own = images.dir.clone();
        images.save_rgba(1, 1, &[1u8, 2, 3, 4], true).expect("save");
        sweep_stale();
        assert!(
            own.exists(),
            "sweep must skip the running process's own dir"
        );
        images.cleanup();
    }
}
