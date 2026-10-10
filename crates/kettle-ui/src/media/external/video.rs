//! Opening a shown video in the permitted video player, from a private copy
//! of the file Kettle showed it from.
//!
//! The player never gets the file a program named. Kettle opens that file
//! itself, as the worker did, and checks it is still the one whose poster it
//! shows: the same identity, and the same first bytes making the same
//! container. It copies the file into its private video store
//! ([`crate::paste_image::VIDEOS`]), by a clone where the file system makes
//! one, and checks the copy is what was shown: its length, its first bytes,
//! and the source unchanged while it was copied. The copy is named with the
//! extension of the container its bytes are, never the name it came under,
//! and marked downloaded (macOS quarantine). The player is checked to be the
//! one this platform permits, and to play that container and codec, before
//! anything is copied, and the copy is checked to be still Kettle's just
//! before the player starts. Nothing here parses video: what the video is
//! comes from the worker, and its bytes are only compared.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use kettle_media::video::{VideoContainer, sniff_video_container};
use kettle_media::{Digest, PathIdentity, VideoCodec};

use super::{OpenFailure, Slot};
use crate::media::{ShelfItem, SourceInput};
use crate::paste_image::{PastedImages, VIDEOS};

/// The largest video Kettle copies to open outside it.
pub(crate) const MAX_VIDEO_COPY_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// How long a copy may take before it is given up: a byte copy of the
/// largest video at 40 MB/s, from a slow external drive.
const COPY_DEADLINE: Duration = Duration::from_secs(120);

/// QuickTime Player, on the sealed system volume.
const QUICKTIME_APP: &str = "/System/Applications/QuickTime Player.app";
/// mpv, where the system's packages put it.
const MPV: &str = "/usr/bin/mpv";
/// The descriptor mpv reads the copy from, as its stream URL names it.
#[cfg_attr(not(unix), allow(dead_code))]
const PINNED_FD: i32 = 3;
const PINNED_URL: &str = "fd://3";

/// The video player this platform permits. Each is found only on its own
/// platform, and never by the user's default association.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Player {
    /// macOS: QuickTime Player, under Apple's own signature.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    QuickTime,
    /// Linux: mpv, a root-owned program nobody else can change.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Mpv,
}

impl Player {
    /// The permitted player this platform has, by a quick look once per
    /// process; each open checks it fully before it copies anything.
    pub(crate) fn find() -> Option<Self> {
        static FOUND: OnceLock<Option<Player>> = OnceLock::new();
        *FOUND.get_or_init(Self::look)
    }

    fn look() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            Path::new(QUICKTIME_APP).is_dir().then_some(Self::QuickTime)
        }
        #[cfg(target_os = "linux")]
        {
            super::trusted_program(Path::new(MPV)).then_some(Self::Mpv)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }

    /// The menu row that opens a video in this player.
    pub(crate) fn label(self, tr: &kettle_i18n::Translator) -> &'static str {
        tr.text(match self {
            Self::QuickTime => kettle_i18n::Text::MenuOpenInQuicktime,
            Self::Mpv => kettle_i18n::Text::MenuOpenInMpv,
        })
    }

    /// Whether the player is still the one permitted: QuickTime Player
    /// signed by Apple as itself, or mpv owned by root and writable by no one
    /// else.
    fn verified(self) -> bool {
        match self {
            #[cfg(target_os = "macos")]
            Self::QuickTime => kettle_ctl::signing::verify_static(
                Path::new(QUICKTIME_APP),
                kettle_ctl::signing::Requirement::QUICKTIME,
            )
            .is_ok(),
            #[cfg(target_os = "linux")]
            Self::Mpv => super::trusted_program(Path::new(MPV)),
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }

    /// Whether this player plays `codec` in `container`, by what it is made
    /// to read. QuickTime Player reads MP4 and QuickTime movies through
    /// AVFoundation, with the codecs every Mac decodes; AV1 and VP9 need
    /// hardware or a registration it does not make, so they are left out.
    /// mpv reads every container Kettle recognizes, through FFmpeg; a
    /// distribution that builds FFmpeg without a codec leaves mpv to say so.
    /// A codec Kettle does not know is played by neither.
    pub(crate) fn plays(self, container: VideoContainer, codec: VideoCodec) -> bool {
        use VideoCodec as C;
        match self {
            Self::QuickTime => {
                matches!(
                    container,
                    VideoContainer::IsoBmff | VideoContainer::QuickTime
                ) && matches!(codec, C::H264 | C::Hevc | C::Mpeg4 | C::ProRes | C::Mjpeg)
            }
            Self::Mpv => matches!(
                codec,
                C::H264
                    | C::Hevc
                    | C::Vp8
                    | C::Vp9
                    | C::Av1
                    | C::Mpeg4
                    | C::Mpeg2
                    | C::ProRes
                    | C::Mjpeg
                    | C::QuickTimeAnimation
                    | C::Theora
            ),
        }
    }

    /// The command that opens the copy at `file`, an absolute path, in this
    /// player: a fixed program and arguments, no shell and no search path.
    /// QuickTime Player opens the path. mpv opens its own window, paused, as
    /// QuickTime Player does, titled `title`, on descriptor 3 (`fd://3`),
    /// which [`Player::start`] makes the copy itself, after `--`, so nothing
    /// reads as an option.
    fn command(self, file: &Path, title: &str) -> std::process::Command {
        debug_assert!(file.is_absolute(), "a leading `/` is never an option");
        let mut command = match self {
            Self::QuickTime => {
                let mut command = std::process::Command::new("/usr/bin/open");
                command.args(["-a", QUICKTIME_APP]).arg(file);
                command
            }
            Self::Mpv => {
                let mut command = std::process::Command::new(MPV);
                command
                    .args(["--player-operation-mode=pseudo-gui", "--pause"])
                    .arg(format!("--force-media-title={title}"))
                    .args(["--", PINNED_URL]);
                command
            }
        };
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
    }

    /// Start this player on the kept copy at `path` in `store`. mpv gets the
    /// copy's own file as descriptor 3, read-only, so it plays the file
    /// Kettle checked whatever later happens to its name; QuickTime Player
    /// takes only a path.
    /// Nothing starts once `closing` says exit has begun, asked last, just
    /// before the spawn; a spawn already under way when exit begins still
    /// happens, and its copy goes with the store.
    fn start(
        self,
        store: &PastedImages,
        path: &Path,
        title: &str,
        closing: &dyn Fn() -> bool,
    ) -> Result<std::process::Child, OpenFailure> {
        let mut command = self.command(path, title);
        if self == Self::Mpv {
            let pinned = store
                .kept_file(path)
                .and_then(|file| read_only(&file))
                .map_err(|_| OpenFailure::Copy)?;
            pin(&mut command, pinned);
        }
        if closing() {
            return Err(OpenFailure::Copy);
        }
        command.spawn().map_err(|_| OpenFailure::Viewer)
    }
}

/// Give `command`'s process `file` as descriptor [`PINNED_FD`], and nothing
/// else of Kettle's: the descriptor is owned by the command, so the parent's
/// copy closes when the command goes.
#[cfg(unix)]
fn pin(command: &mut std::process::Command, file: File) {
    use std::os::fd::{AsRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    let owned = OwnedFd::from(file);
    // SAFETY: the hook runs in the child between fork and exec and calls
    // only `dup2` and `fcntl`, which are async-signal-safe. `dup2` onto the
    // same number would leave close-on-exec set, so that case clears it.
    unsafe {
        command.pre_exec(move || {
            let fd = owned.as_raw_fd();
            let status = if fd == PINNED_FD {
                libc::fcntl(fd, libc::F_SETFD, 0)
            } else {
                libc::dup2(fd, PINNED_FD)
            };
            if status == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn pin(_command: &mut std::process::Command, _file: File) {}

/// A read-only handle on the same file as `file`: on Linux a fresh
/// description of it, checked to be the same file; elsewhere another handle
/// on it.
fn read_only(file: &File) -> io::Result<File> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::MetadataExt as _;
        let reopened = File::open(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
        let (was, is) = (file.metadata()?, reopened.metadata()?);
        if (was.dev(), was.ino()) != (is.dev(), is.ino()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the copy is not the file it was",
            ));
        }
        Ok(reopened)
    }
    #[cfg(not(target_os = "linux"))]
    {
        file.try_clone()
    }
}

/// The private copies handed to the video player, made off the window
/// thread.
pub(crate) struct VideoCopies {
    store: Mutex<PastedImages>,
    /// Set at exit before the store closes, so a copy on its way stops at
    /// its next chunk rather than holding exit up.
    closing: AtomicBool,
}

impl VideoCopies {
    pub(crate) fn new() -> Self {
        Self::of(PastedImages::new(VIDEOS))
    }

    fn of(store: PastedImages) -> Self {
        Self {
            store: Mutex::new(store),
            closing: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PastedImages> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stop a copy on its way, delete every copy, and take no more. A copy
    /// being made holds no store, so this waits on none.
    pub(crate) fn close(&self) {
        self.closing.store(true, Ordering::Release);
        self.store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .close();
    }
}

/// What opening a shown video needs: the file it was shown from, what the
/// worker found it to be, and the digest its poster carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VideoSource {
    pub path: PathBuf,
    pub digest: Digest,
    pub container: VideoContainer,
    pub codec: VideoCodec,
    /// The item's title, sanitized, for the player's window.
    pub title: String,
}

impl VideoSource {
    /// What opening `item` needs, when it is a video shown from its file.
    pub(crate) fn of(item: &ShelfItem) -> Option<Self> {
        let video = item.video?;
        let SourceInput::Path { path, .. } = &item.source.spec.input else {
            return None;
        };
        Some(Self {
            path: native_path(path)?,
            digest: item.source.digest.clone(),
            container: video.container?,
            codec: video.codec,
            title: item.title.clone(),
        })
    }
}

/// The path `path` names, on Unix, where videos open outside Kettle.
#[cfg(unix)]
fn native_path(path: &kettle_media::NativePath) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(path.as_bytes())))
}

#[cfg(not(unix))]
fn native_path(_path: &kettle_media::NativePath) -> Option<PathBuf> {
    None
}

/// Open a private copy of `video` in `player`, on a short thread. `failed`
/// hears why, if it does not open; it runs on that thread.
pub(crate) fn open(
    copies: Arc<VideoCopies>,
    video: VideoSource,
    player: Player,
    failed: impl FnOnce(OpenFailure) + Send + 'static,
) {
    let Some(slot) = Slot::take() else {
        failed(OpenFailure::Busy);
        return;
    };
    // A thread that cannot start drops the slot with its closure.
    let _ = std::thread::Builder::new()
        .name("kettle-ui-open-video".into())
        .spawn(move || match hand_over(&copies, &video, player) {
            Ok(child) => {
                // The slot bounds the work of opening, not how long the
                // player stays up.
                drop(slot);
                super::watch(child, failed);
            }
            Err(why) => {
                drop(slot);
                failed(why);
            }
        });
}

/// Check the player and what it can play, copy the video, and start the
/// player on the copy. A copy that does not reach the player is deleted.
fn hand_over(
    copies: &VideoCopies,
    video: &VideoSource,
    player: Player,
) -> Result<std::process::Child, OpenFailure> {
    if !player.verified() {
        return Err(OpenFailure::Viewer);
    }
    if !player.plays(video.container, video.codec) {
        return Err(OpenFailure::Unplayable);
    }
    let path = copy(copies, video)?;
    let mut store = copies.lock();
    // Exit may have begun while the copy was made, and a copy must still be
    // Kettle's. Started while the store is held, so the copy cannot be
    // dropped for a newer one before the player is on its way to it.
    let started = if store.path_still_matches(&path) {
        player.start(&store, &path, &video.title, &|| {
            copies.closing.load(Ordering::Acquire)
        })
    } else {
        Err(OpenFailure::Copy)
    };
    if started.is_err() {
        let _ = store.remove(&path);
    }
    started
}

/// Hold the file `video` was shown from and check it is still the one
/// shown, copy it without holding the store, keep the copy once it is what
/// was shown, and mark it downloaded. The store is held only to reserve the
/// copy and to keep it, so neither exit nor another open waits on the file
/// system.
fn copy(copies: &VideoCopies, video: &VideoSource) -> Result<PathBuf, OpenFailure> {
    let held = Held::open(video)?;
    if held.identity.size > MAX_VIDEO_COPY_BYTES {
        return Err(OpenFailure::Room);
    }
    let reservation = copies
        .lock()
        .reserve_copy(held.identity.size, video.container.extension())
        .map_err(copy_failure)?;
    let stop = || copies.closing.load(Ordering::Acquire);
    // Made and checked without the store: checking reads the source's
    // metadata, which a network file system may take its time over.
    let made = reservation
        .copy(&held.file, &stop, Instant::now() + COPY_DEADLINE)
        .and_then(|file| match held.check_copy(&file, video.container) {
            Ok(()) => Ok(file),
            Err(error) => {
                reservation.discard(file);
                Err(error)
            }
        });
    let mut store = copies.lock();
    let file = match made {
        Ok(file) => file,
        Err(error) => {
            store.release(reservation);
            return Err(copy_failure(error));
        }
    };
    let path = store
        .keep_copy(reservation, file, |_| Ok(()))
        .map_err(copy_failure)?;
    if store.mark_downloaded(&path).is_err() {
        let _ = store.remove(&path);
        return Err(OpenFailure::Copy);
    }
    Ok(path)
}

/// Why a copy failed, for the notice: a store full of copies too new to
/// drop asks for a while, a full volume says so, and a copy that is not
/// what was shown, or a source that ended early, says the video changed.
fn copy_failure(error: io::Error) -> OpenFailure {
    log::debug!("open: could not copy the video: {error}");
    match error.kind() {
        io::ErrorKind::QuotaExceeded => OpenFailure::Busy,
        io::ErrorKind::StorageFull => OpenFailure::Room,
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => OpenFailure::Changed,
        _ => OpenFailure::Copy,
    }
}

/// The file a video was shown from, held open, with the identity and first
/// bytes it had when shown.
struct Held {
    file: File,
    identity: PathIdentity,
    prefix: Vec<u8>,
}

impl Held {
    /// Open the file `video` was shown from, as the worker opened it, and
    /// check it is still the one shown: a regular file with the same
    /// identity, whose first bytes give the poster's digest and make the
    /// same container.
    fn open(video: &VideoSource) -> Result<Self, OpenFailure> {
        let shown = video.digest.path_identity.ok_or(OpenFailure::Changed)?;
        let file = open_read_only(&video.path).map_err(|_| OpenFailure::Changed)?;
        let opened = file.metadata().map_err(|_| OpenFailure::Changed)?;
        if !opened.is_file() || identity(&opened) != Some(shown) {
            return Err(OpenFailure::Changed);
        }
        let prefix = read_prefix(&file, shown.size).map_err(|_| OpenFailure::Changed)?;
        if kettle_media::content_digest(&prefix, Some(shown)).ok() != Some(video.digest.clone())
            || sniff_video_container(&prefix, shown.size) != Some(video.container)
        {
            return Err(OpenFailure::Changed);
        }
        Ok(Self {
            file,
            identity: shown,
            prefix,
        })
    }

    /// Whether `copy` is what was shown: as long as the source, which did
    /// not change while it was copied, with the same first bytes making the
    /// same `container`. Anything else is `InvalidData`.
    fn check_copy(&self, copy: &File, container: VideoContainer) -> io::Result<()> {
        let changed = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
        if identity(&self.file.metadata()?) != Some(self.identity) {
            return Err(changed("the video changed while it was copied"));
        }
        let len = copy.metadata()?.len();
        if len != self.identity.size {
            return Err(changed("the copy is not the video's length"));
        }
        let prefix = read_prefix(copy, len)?;
        if prefix != self.prefix || sniff_video_container(&prefix, len) != Some(container) {
            return Err(changed("the copy does not begin as the video does"));
        }
        Ok(())
    }
}

/// `path` opened read-only, without waiting on a FIFO or becoming a
/// controlling terminal, as the worker opens a source.
#[cfg(unix)]
fn open_read_only(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
}

#[cfg(not(unix))]
fn open_read_only(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn identity(metadata: &std::fs::Metadata) -> Option<PathIdentity> {
    PathIdentity::of(metadata)
}

#[cfg(not(unix))]
fn identity(_metadata: &std::fs::Metadata) -> Option<PathIdentity> {
    None
}

/// The first bytes of `file`, `size` long, that the worker read to know it:
/// as many as it inspects, or all of a shorter file.
#[cfg(unix)]
fn read_prefix(file: &File, size: u64) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt as _;
    let len = usize::try_from(size)
        .unwrap_or(usize::MAX)
        .min(kettle_media::video::MAX_VIDEO_PREFIX_BYTES);
    let mut prefix = vec![0; len];
    file.read_exact_at(&mut prefix, 0)?;
    Ok(prefix)
}

#[cfg(not(unix))]
fn read_prefix(_file: &File, _size: u64) -> io::Result<Vec<u8>> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write as _;

    use kettle_media::video::MAX_VIDEO_PREFIX_BYTES;

    use kettle_test_support::{PrivateTempDir, private_tempdir};

    const MP4: &[u8] = include_bytes!("../../../testdata/video-preview.mp4");

    /// What the worker would have shown for the file at `path`: its identity
    /// and first bytes, as `container` holding H.264.
    fn shown_as(path: &Path, container: VideoContainer) -> VideoSource {
        use std::io::Read as _;
        let identity = PathIdentity::of(&std::fs::metadata(path).unwrap()).unwrap();
        let mut prefix = Vec::new();
        File::open(path)
            .unwrap()
            .take(MAX_VIDEO_PREFIX_BYTES as u64)
            .read_to_end(&mut prefix)
            .unwrap();
        VideoSource {
            path: path.to_owned(),
            digest: kettle_media::content_digest(&prefix, Some(identity)).unwrap(),
            container,
            codec: VideoCodec::H264,
            title: "clip".into(),
        }
    }

    /// The fixture MP4 as `name` in a private directory, shown.
    fn shown(name: &str) -> (PrivateTempDir, VideoSource) {
        let dir = private_tempdir("kettle-open-video-");
        let path = dir.path().join(name);
        std::fs::write(&path, MP4).unwrap();
        let video = shown_as(&path, VideoContainer::IsoBmff);
        (dir, video)
    }

    /// Empty video copies whose session would go in `dir`.
    fn copies_in(dir: &PrivateTempDir) -> VideoCopies {
        VideoCopies::of(PastedImages::of_kind_in(
            crate::paste_image::VIDEOS_ANY_DISK,
            dir.path().join("copies"),
        ))
    }

    /// The copy is the shown file's bytes, owner-only, named by the
    /// container its bytes are and not the name it came under, marked
    /// downloaded on macOS, and still Kettle's.
    #[test]
    fn a_shown_video_is_copied_as_its_bytes_say() {
        use std::os::unix::fs::PermissionsExt as _;
        let (dir, video) = shown("clip.webm");
        let copies = copies_in(&dir);
        let path = copy(&copies, &video).expect("copied");
        let mut store = copies.lock();
        assert_eq!(path.extension(), Some(std::ffi::OsStr::new("mp4")));
        assert_eq!(std::fs::read(&path).unwrap(), MP4);
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

    /// A file that is not the one shown is never copied: one changed,
    /// replaced, gone, or a FIFO in its place, one whose first bytes changed
    /// though its identity did not, a digest without an identity, or bytes
    /// that are not the container shown.
    #[test]
    fn a_video_that_is_not_the_one_shown_is_not_copied() {
        let refused = |video: &VideoSource, dir: &PrivateTempDir, case: &str| {
            assert_eq!(
                copy(&copies_in(dir), video),
                Err(OpenFailure::Changed),
                "{case}"
            );
            assert!(
                !dir.path().join("copies").exists(),
                "{case}: nothing written"
            );
        };

        let (dir, video) = shown("grown.mp4");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&video.path)
            .unwrap()
            .write_all(b"x")
            .unwrap();
        refused(&video, &dir, "grown");

        let (dir, video) = shown("replaced.mp4");
        let other = dir.path().join("other.mp4");
        std::fs::write(&other, MP4).unwrap();
        std::fs::rename(&other, &video.path).unwrap();
        refused(&video, &dir, "replaced");

        let (dir, video) = shown("gone.mp4");
        std::fs::remove_file(&video.path).unwrap();
        refused(&video, &dir, "gone");

        let (dir, video) = shown("fifo.mp4");
        std::fs::remove_file(&video.path).unwrap();
        let name = std::ffi::CString::new(video.path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: the name is NUL-terminated.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        refused(&video, &dir, "fifo");

        // The same identity, as a program that restores the time can give
        // it, but other first bytes.
        let (dir, video) = shown("rewritten.mp4");
        let modified = std::fs::metadata(&video.path).unwrap().modified().unwrap();
        let mut bytes = MP4.to_vec();
        bytes[MP4.len() / 2] ^= 0xff;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&video.path)
            .unwrap();
        std::os::unix::fs::FileExt::write_all_at(&file, &bytes, 0).unwrap();
        file.set_modified(modified).unwrap();
        drop(file);
        assert_eq!(
            PathIdentity::of(&std::fs::metadata(&video.path).unwrap()),
            video.digest.path_identity,
            "the identity is the one shown"
        );
        refused(&video, &dir, "rewritten");

        let (dir, mut video) = shown("unplaced.mp4");
        video.digest.path_identity = None;
        refused(&video, &dir, "no identity");

        let (dir, mut video) = shown("webm.mp4");
        video.container = VideoContainer::WebM;
        refused(&video, &dir, "another container");
    }

    /// A video past what Kettle copies is refused before anything is
    /// written: a sparse file, so the test takes no disk.
    #[test]
    fn a_video_past_the_copy_limit_is_refused() {
        let (dir, _) = shown("huge.mp4");
        let path = dir.path().join("huge.mp4");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_VIDEO_COPY_BYTES + 1)
            .unwrap();
        let video = shown_as(&path, VideoContainer::IsoBmff);
        assert_eq!(copy(&copies_in(&dir), &video), Err(OpenFailure::Room));
        assert!(!dir.path().join("copies").exists());
    }

    /// A copy whose source changed while it was made, or that does not
    /// begin as the video does, is refused as changed.
    #[test]
    fn a_copy_that_is_not_what_was_shown_is_refused() {
        let (dir, video) = shown("clip.mp4");
        let held = Held::open(&video).expect("held");
        let copy_path = dir.path().join("copy.mp4");
        std::fs::write(&copy_path, MP4).unwrap();
        let copied = File::open(&copy_path).unwrap();
        held.check_copy(&copied, VideoContainer::IsoBmff)
            .expect("a true copy passes");
        let kind = |result: io::Result<()>| result.map_err(|error| error.kind());
        assert_eq!(
            kind(held.check_copy(&copied, VideoContainer::QuickTime)),
            Err(io::ErrorKind::InvalidData),
            "another container"
        );
        let mut torn = MP4.to_vec();
        torn[40] ^= 0xff;
        std::fs::write(&copy_path, &torn).unwrap();
        let copied = File::open(&copy_path).unwrap();
        assert_eq!(
            kind(held.check_copy(&copied, VideoContainer::IsoBmff)),
            Err(io::ErrorKind::InvalidData),
            "other first bytes"
        );
        // Past the first bytes compared, only the length tells a short copy.
        let long = dir.path().join("long.mp4");
        let mut padded = MP4.to_vec();
        padded.resize(MAX_VIDEO_PREFIX_BYTES * 2, 0);
        std::fs::write(&long, &padded).unwrap();
        let long_held = Held::open(&shown_as(&long, VideoContainer::IsoBmff)).expect("held");
        std::fs::write(&copy_path, &padded[..padded.len() - 1]).unwrap();
        let copied = File::open(&copy_path).unwrap();
        assert_eq!(
            kind(long_held.check_copy(&copied, VideoContainer::IsoBmff)),
            Err(io::ErrorKind::InvalidData),
            "shorter"
        );
        std::fs::write(&copy_path, MP4).unwrap();
        let copied = File::open(&copy_path).unwrap();
        held.file
            .set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            kind(held.check_copy(&copied, VideoContainer::IsoBmff)),
            Err(io::ErrorKind::InvalidData),
            "the source changed while copied"
        );
        assert_eq!(
            copy_failure(io::ErrorKind::InvalidData.into()),
            OpenFailure::Changed
        );
    }

    /// A copy that cannot be made says why: a store full of new copies is
    /// busy, a full disk is out of room, and anything else is the copy.
    #[test]
    fn copy_failures_say_why() {
        for (kind, why) in [
            (io::ErrorKind::QuotaExceeded, OpenFailure::Busy),
            (io::ErrorKind::StorageFull, OpenFailure::Room),
            (io::ErrorKind::InvalidData, OpenFailure::Changed),
            (io::ErrorKind::UnexpectedEof, OpenFailure::Changed),
            (io::ErrorKind::TimedOut, OpenFailure::Copy),
            (io::ErrorKind::Interrupted, OpenFailure::Copy),
            (io::ErrorKind::PermissionDenied, OpenFailure::Copy),
        ] {
            assert_eq!(copy_failure(kind.into()), why, "{kind:?}");
        }
    }

    /// Once closed, a store copies nothing more, and a copy asks it to stop.
    #[test]
    fn a_closed_store_copies_nothing() {
        let (dir, video) = shown("late.mp4");
        let copies = copies_in(&dir);
        assert!(!copies.closing.load(Ordering::Acquire));
        copies.close();
        assert!(copies.closing.load(Ordering::Acquire));
        assert_eq!(copy(&copies, &video), Err(OpenFailure::Copy));
        assert!(!dir.path().join("copies").exists());
    }

    /// A pinned player reads the copy it was given on descriptor 3 (on Linux
    /// read-only), even once something else takes the copy's name before it
    /// starts.
    #[test]
    fn a_pinned_player_reads_the_copy_it_was_given() {
        use std::io::Read as _;
        let (dir, video) = shown("clip.mp4");
        let copies = copies_in(&dir);
        let path = copy(&copies, &video).expect("copied");
        let store = copies.lock();
        let pinned = read_only(&store.kept_file(&path).unwrap()).unwrap();
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd as _;
            // SAFETY: F_GETFL only reads the open descriptor's flags.
            let flags = unsafe { libc::fcntl(pinned.as_raw_fd(), libc::F_GETFL) };
            assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
        }
        // What a process sharing the user's rights could do after the check.
        let other = dir.path().join("other.mp4");
        std::fs::write(&other, b"not the video").unwrap();
        std::fs::rename(&other, &path).unwrap();
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", "cat <&3"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        pin(&mut command, pinned);
        let mut child = command.spawn().unwrap();
        let mut out = Vec::new();
        child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
        assert!(child.wait().unwrap().success());
        assert!(out == MP4, "the checked copy, not what took its name");
        drop(store);
        copies.close();
    }

    /// Once exit has begun, no player starts: the check comes after mpv's
    /// descriptor is ready, just before the spawn. (QuickTime Player is left
    /// out so that a fault here never opens a window.)
    #[test]
    fn nothing_starts_once_exit_begins() {
        let (dir, video) = shown("clip.mp4");
        let copies = copies_in(&dir);
        let path = copy(&copies, &video).expect("copied");
        let store = copies.lock();
        assert_eq!(
            Player::Mpv
                .start(&store, &path, "clip", &|| true)
                .map(|_| ()),
            Err(OpenFailure::Copy)
        );
        drop(store);
        copies.close();
    }

    /// Each player plays only what it is made to read, and a codec Kettle
    /// does not know is played by neither.
    #[test]
    fn each_player_plays_only_what_it_reads() {
        use VideoCodec as C;
        use VideoContainer as V;
        let quicktime = |container, codec| Player::QuickTime.plays(container, codec);
        assert!(quicktime(V::IsoBmff, C::H264));
        assert!(quicktime(V::QuickTime, C::ProRes));
        assert!(quicktime(V::IsoBmff, C::Hevc));
        assert!(!quicktime(V::WebM, C::Vp8), "WebM");
        assert!(!quicktime(V::Matroska, C::H264), "Matroska");
        assert!(!quicktime(V::IsoBmff, C::Av1), "AV1");
        assert!(!quicktime(V::IsoBmff, C::Vp9), "VP9");
        let mpv = |container, codec| Player::Mpv.plays(container, codec);
        assert!(mpv(V::WebM, C::Vp9));
        assert!(mpv(V::Avi, C::Mpeg4));
        assert!(mpv(V::Ogg, C::Theora));
        assert!(mpv(V::MpegTransportStream, C::Mpeg2));
        for container in V::ALL {
            for codec in [C::Unknown, C::Gif, C::Apng, C::WebP] {
                assert!(!quicktime(container, codec), "{container:?} {codec:?}");
                assert!(!mpv(container, codec), "{container:?} {codec:?}");
            }
        }
    }

    /// The player command is a fixed program: QuickTime Player takes the
    /// absolute path last, and mpv the pinned descriptor after `--`, its
    /// title one argument whatever it says.
    #[test]
    fn the_player_is_fixed() {
        let file = Path::new("/tmp/kettle-video-1-0/0001.mp4");
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
            shape(Player::QuickTime.command(file, "clip")),
            (
                "/usr/bin/open".into(),
                vec!["-a".into(), QUICKTIME_APP.into(), file.as_os_str().into()]
            )
        );
        assert_eq!(
            shape(Player::Mpv.command(file, "clip --vo=x")),
            (
                MPV.into(),
                vec![
                    "--player-operation-mode=pseudo-gui".into(),
                    "--pause".into(),
                    "--force-media-title=clip --vo=x".into(),
                    "--".into(),
                    "fd://3".into()
                ]
            )
        );
    }

    /// A video item names its file, digest, container and codec, and opens
    /// in the video player; an animation, which has no container, opens as
    /// an image.
    #[test]
    fn a_video_item_opens_in_the_player() {
        let (_dir, video) = shown("clip.mp4");
        let mut item = ShelfItem::new(
            1,
            None,
            "clip.mp4".into(),
            crate::media::Provenance::User,
            kettle_media::MediaKind::Video,
            Vec::new(),
            kettle_core::ImageData::new(1, 1, vec![0; 4]).unwrap(),
            crate::media::ItemSource::sample_file(&video.path, video.digest.clone()),
        );
        let info = kettle_media::VideoInfo {
            duration_ms: 1000,
            width: 160,
            height: 90,
            rotation: 0,
            codec: VideoCodec::H264,
            fps_milli: None,
            has_audio: false,
            container: Some(VideoContainer::IsoBmff),
        };
        item.video = Some(info);
        assert_eq!(
            VideoSource::of(&item),
            Some(VideoSource {
                title: "clip.mp4".into(),
                ..video
            })
        );
        assert_eq!(
            super::super::Outside::of(&item),
            Player::find().map(super::super::Outside::Video)
        );
        item.video = Some(kettle_media::VideoInfo {
            container: None,
            ..info
        });
        assert_eq!(VideoSource::of(&item), None);
        assert_eq!(
            super::super::Outside::of(&item),
            super::super::Viewer::find().map(super::super::Outside::Image)
        );
    }

    /// The player gets the copy only after it is checked and can play the
    /// video, and the copy is made from the held file, checked, marked and
    /// checked still Kettle's, all in that order and under the store's lock.
    #[test]
    fn a_video_reaches_the_player_only_after_every_check() {
        let source = include_str!("video.rs")
            .split("#[cfg(all(test, unix))]")
            .next()
            .expect("production part")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let body = |name: &str| {
            source
                .split(&format!("fn {name}("))
                .nth(1)
                .unwrap_or_else(|| panic!("{name}"))
                .split(" fn ")
                .next()
                .expect("body")
                .to_owned()
        };
        let in_order = |body: &str, steps: &[&str]| {
            let at: Vec<_> = steps
                .iter()
                .map(|step| body.find(step).unwrap_or_else(|| panic!("{step}")))
                .collect();
            assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "{at:?}");
        };
        in_order(
            &body("hand_over"),
            &[
                "if !player.verified() { return Err(OpenFailure::Viewer); }",
                "if !player.plays(video.container, video.codec) { return Err(OpenFailure::Unplayable); }",
                "let path = copy(copies, video)?;",
                "let mut store = copies.lock();",
                "if store.path_still_matches(&path) { player.start(&store, &path, &video.title, &|| { copies.closing.load(Ordering::Acquire) })",
                "if started.is_err() { let _ = store.remove(&path); }",
            ],
        );
        in_order(
            &body("copy"),
            &[
                "let held = Held::open(video)?;",
                "if held.identity.size > MAX_VIDEO_COPY_BYTES { return Err(OpenFailure::Room); }",
                ".reserve_copy(held.identity.size, video.container.extension())",
                "let made = reservation .copy(&held.file, &stop, Instant::now() + COPY_DEADLINE)",
                "held.check_copy(&file, video.container)",
                "reservation.discard(file);",
                "let mut store = copies.lock();",
                "store.release(reservation);",
                ".keep_copy(reservation, file, |_| Ok(()))",
                "if store.mark_downloaded(&path).is_err() { let _ = store.remove(&path); return Err(OpenFailure::Copy); }",
            ],
        );
    }

    /// Live, by hand on a Mac with a desktop session: QuickTime Player
    /// passes its check and opens a marked copy, and the copy goes with the
    /// store. It opens a QuickTime Player window, so it is ignored by
    /// default.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "opens a QuickTime Player window"]
    fn quicktime_opens_a_marked_copy_live() {
        let (dir, video) = shown("clip.webm");
        let copies = copies_in(&dir);
        let mut child = hand_over(&copies, &video, Player::QuickTime).expect("handed over");
        assert!(child.wait().expect("open").success());
        // QuickTime Player reads the file once it is up.
        std::thread::sleep(Duration::from_secs(3));
        copies.close();
        assert!(!dir.path().join("copies").exists());
    }
}
