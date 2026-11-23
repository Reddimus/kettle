//! Video stills for a model to read: `kettle video-frames` and, through the
//! same code, the `kettle_video_frames` MCP tool. The command opens the file
//! itself, inside the caller's sandbox, and renders it in a media worker it
//! starts as its own child, with no Kettle window involved: a contact sheet
//! of evenly spaced frames, or one frame, each labeled with its time, as one
//! JPEG, and a text index of what each frame is and how far its time may be
//! from the instant it stands for. Showing media to the user is
//! `kettle show`; this is reading it, a separate act.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use image::{DynamicImage, RgbaImage};
use kettle_media::client::WorkerClient;
use kettle_media::{
    Canvas, ExternalAttested, ExternalRequest, ExternalSource, FailureCode, Job, JobKind,
    MAX_VIDEO_STILLS, MediaKind, NativePath, StillsLayout, Target, Theme, VideoCodec, VideoStills,
    VideoStillsResult, still_label,
};

/// Frames on a sheet when none are asked for, and the most there may be.
pub(crate) const DEFAULT_COUNT: u8 = 9;
/// A sheet's longer edge when none is asked for, and the longest a model may
/// ask for, to read text in a frame.
pub(crate) const DEFAULT_EDGE: u32 = 1568;
pub(crate) const MAX_EDGE: u32 = 2560;
/// The shortest edge a sheet is shrunk to while fitting a size limit.
const MIN_FIT_EDGE: u32 = 320;
/// JPEG qualities tried in turn at each size.
const QUALITIES: [u8; 5] = [90, 80, 70, 60, 50];

/// What the caller asks for, before any check.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FramesRequest {
    pub(crate) path: PathBuf,
    pub(crate) start_s: Option<f64>,
    pub(crate) end_s: Option<f64>,
    pub(crate) at_s: Option<f64>,
    pub(crate) count: Option<u8>,
    pub(crate) max_edge: Option<u32>,
}

/// The fewest columns that hold `count` frames in a square or wider grid.
fn columns(count: u8) -> u8 {
    (1..=count)
        .find(|columns| u16::from(*columns) * u16::from(*columns) >= u16::from(count))
        .unwrap_or(1)
}

impl FramesRequest {
    /// The worker's stills options, or `BadParams`: times finite and not
    /// negative, a window that ends after it starts, `at_s` alone (one
    /// frame), one to sixteen frames, an edge up to 2560.
    pub(crate) fn stills(&self) -> Result<VideoStills, FailureCode> {
        let time = |value: Option<f64>| value.is_none_or(|value| value.is_finite() && value >= 0.0);
        if !time(self.start_s) || !time(self.end_s) || !time(self.at_s) {
            return Err(FailureCode::BadParams);
        }
        let max_edge = self.max_edge.unwrap_or(DEFAULT_EDGE);
        if !(1..=MAX_EDGE).contains(&max_edge) {
            return Err(FailureCode::BadParams);
        }
        if self.path.as_os_str().len() > kettle_media::MAX_PATH_BYTES {
            return Err(FailureCode::BadParams);
        }
        // Every frame a model reads carries its time label, a single one
        // too: a one-column sheet, never the unlabeled poster.
        let stills = if let Some(at) = self.at_s {
            if self.start_s.is_some() || self.end_s.is_some() || self.count.is_some() {
                return Err(FailureCode::BadParams);
            }
            VideoStills {
                count: 1,
                max_edge,
                start_s: 0.0,
                end_s: None,
                at_s: Some(at),
                layout: StillsLayout::Sheet {
                    cols: 1,
                    labels: true,
                },
            }
        } else {
            let count = self.count.unwrap_or(DEFAULT_COUNT);
            if !(1..=MAX_VIDEO_STILLS).contains(&count) {
                return Err(FailureCode::BadParams);
            }
            let start_s = self.start_s.unwrap_or(0.0);
            if self.end_s.is_some_and(|end| end <= start_s) {
                return Err(FailureCode::BadParams);
            }
            VideoStills {
                count,
                max_edge,
                start_s,
                end_s: self.end_s,
                at_s: None,
                layout: StillsLayout::Sheet {
                    cols: columns(count),
                    labels: true,
                },
            }
        };
        stills.validate().map_err(|_| FailureCode::BadParams)?;
        Ok(stills)
    }
}

/// A sheet taken, before it is encoded: its pixels, the video's
/// description and each frame's times, and how the frames are laid out.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Sheet {
    pub(crate) image: RgbaImage,
    pub(crate) video: VideoStillsResult,
    pub(crate) layout: StillsLayout,
}

/// Open `path` here, in this process and its sandbox, and attest what was
/// opened, as `kettle show` does. A relative path is taken from the working
/// directory.
fn source(path: &Path) -> Result<(ExternalSource, ExternalAttested), FailureCode> {
    let path = std::path::absolute(path).map_err(|_| FailureCode::BadParams)?;
    let attestation = crate::show_cli::attest(&path)?;
    let path = NativePath::from_path(&path).map_err(|_| FailureCode::BadParams)?;
    Ok((ExternalSource::Path { path, attestation }, attestation))
}

/// The job for `stills` of the file at `path`.
fn job(path: &Path, stills: VideoStills) -> Result<(Job, ExternalAttested), FailureCode> {
    let (source, attestation) = source(path)?;
    let request = ExternalRequest {
        kind: JobKind::VideoStills(stills),
        source,
        theme: Theme {
            background: [24, 24, 24, 255],
            foreground: [235, 235, 235, 255],
            palette: [[0; 4]; 16],
            accent: [0; 4],
            is_dark: true,
        },
        canvas: Canvas::Theme,
        target: Target {
            width: stills.max_edge,
            height: stills.max_edge,
            scale: 1.0,
            crop: None,
        },
        fallback_fonts: Vec::new(),
    };
    Ok((request.into(), attestation))
}

/// Take the stills `request` asks for, in a worker `client` starts.
pub(crate) fn render(
    request: &FramesRequest,
    client: &WorkerClient,
    cancelled: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<Sheet, FailureCode> {
    let control =
        cancelled.map(|flag| kettle_media::client::RenderControl::with_cancel_flag(flag.clone()));
    render_attested(request, client, control.as_ref()).map(|(sheet, _)| sheet)
}

fn render_attested(
    request: &FramesRequest,
    client: &WorkerClient,
    control: Option<&kettle_media::client::RenderControl>,
) -> Result<(Sheet, ExternalAttested), FailureCode> {
    if control.is_some_and(|control| control.is_cancelled()) {
        return Err(FailureCode::RenderTimeout);
    }
    let stills = request.stills()?;
    let (job, attestation) = job(&request.path, stills)?;
    let rendered = match control {
        Some(control) => {
            client
                .render_with_control(&job, control)
                .map_err(|error| match error {
                    kettle_media::client::RenderError::Cancelled => FailureCode::RenderTimeout,
                    kettle_media::client::RenderError::Failure(failure) => failure,
                })?
        }
        None => client.render(&job)?,
    };
    rendered
        .validate_as(MediaKind::Video)
        .map_err(|_| FailureCode::RenderParse)?;
    let video = rendered.video.clone().ok_or(FailureCode::RenderParse)?;
    let image = RgbaImage::from_raw(rendered.width, rendered.height, rendered.rgba)
        .ok_or(FailureCode::RenderParse)?;
    Ok((
        Sheet {
            image,
            video,
            layout: stills.layout,
        },
        attestation,
    ))
}

impl Sheet {
    /// The sheet as the largest JPEG `fits` accepts: each quality in turn,
    /// then shrunk by a quarter at a time down to 320 pixels on its longer
    /// edge. One that fits nowhere is `TooLarge`; nothing is cut.
    pub(crate) fn jpeg(&self, fits: &dyn Fn(&[u8]) -> bool) -> Result<Vec<u8>, FailureCode> {
        self.jpeg_cancellable(fits, None)
    }

    pub(crate) fn jpeg_cancellable(
        &self,
        fits: &dyn Fn(&[u8]) -> bool,
        cancelled: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Vec<u8>, FailureCode> {
        encode_cancellable(self.image.clone(), fits, cancelled).map(|(jpeg, _, _)| jpeg)
    }

    /// What it shows, in words.
    pub(crate) fn index(&self) -> String {
        index(&self.video, self.layout)
    }
}

/// The sheet as the largest JPEG `fits` accepts, with its size.
#[cfg(test)]
fn encode(
    sheet: RgbaImage,
    fits: &dyn Fn(&[u8]) -> bool,
) -> Result<(Vec<u8>, u32, u32), FailureCode> {
    encode_cancellable(sheet, fits, None)
}

fn encode_cancellable(
    mut sheet: RgbaImage,
    fits: &dyn Fn(&[u8]) -> bool,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(Vec<u8>, u32, u32), FailureCode> {
    let check = || {
        if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            Err(FailureCode::RenderTimeout)
        } else {
            Ok(())
        }
    };
    loop {
        check()?;
        // The sheet is opaque: compose flattens every frame onto it.
        let rgb = DynamicImage::ImageRgba8(sheet.clone()).to_rgb8();
        for quality in QUALITIES {
            check()?;
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality)
                .encode_image(&rgb)
                .map_err(|_| FailureCode::RenderResource)?;
            let accepted = fits(&jpeg);
            check()?;
            if accepted {
                return Ok((jpeg, rgb.width(), rgb.height()));
            }
        }
        let longer = sheet.width().max(sheet.height());
        if longer <= MIN_FIT_EDGE {
            return Err(FailureCode::TooLarge);
        }
        // A quarter smaller, or exactly the minimum when that is closer.
        let target = (longer * 3 / 4).max(MIN_FIT_EDGE);
        let scale = |side: u32| (u64::from(side) * u64::from(target) / u64::from(longer)) as u32;
        let (width, height) = (scale(sheet.width()).max(1), scale(sheet.height()).max(1));
        sheet =
            image::imageops::resize(&sheet, width, height, image::imageops::FilterType::Triangle);
    }
}

/// A codec as people name it.
fn codec_name(codec: VideoCodec) -> &'static str {
    codec.name().unwrap_or("an unknown codec")
}

/// A frame rate as people write it: `25 fps`, `29.97 fps`.
fn rate(fps_milli: u32) -> String {
    let whole = fps_milli / 1000;
    let fraction = fps_milli % 1000;
    if fraction == 0 {
        format!("{whole} fps")
    } else {
        let digits = format!("{fraction:03}");
        format!("{whole}.{} fps", digits.trim_end_matches('0'))
    }
}

/// A span as people read it, never less than it is: `22 ms` under a
/// second, else seconds to the tenth rounded up, `1.5 s` for 1450 ms.
fn span(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        let tenths = ms.div_ceil(100);
        format!("{}.{} s", tenths / 10, tenths % 10)
    }
}

/// What a sheet shows, in words: the video, then each frame's time in
/// layout order, then how far a time may be from its instant. The times are
/// the frames actually shown. Nothing depends on the JPEG's size, which
/// shrinks to fit while these words stay.
pub(crate) fn index(video: &VideoStillsResult, layout: StillsLayout) -> String {
    let info = video.info;
    let duration = info.duration_ms;
    let mut text = String::new();
    let _ = write!(
        text,
        "Video: {}, {}x{}, {}",
        still_label(duration, duration),
        info.width,
        info.height,
        codec_name(info.codec)
    );
    if let Some(fps) = info.fps_milli {
        let _ = write!(text, ", {}", rate(fps));
    }
    let _ = writeln!(
        text,
        ", {}.",
        if info.has_audio {
            "with audio"
        } else {
            "no audio"
        }
    );
    match (layout, video.samples.as_slice()) {
        (_, [sample]) => {
            let _ = writeln!(
                text,
                "One frame, labeled with its time: {}.",
                still_label(sample.actual_ms, duration)
            );
        }
        (StillsLayout::Poster, _) => {
            let _ = writeln!(text, "{} frames:", video.samples.len());
            for (at, sample) in video.samples.iter().enumerate() {
                let _ = writeln!(
                    text,
                    "{}. {}",
                    at + 1,
                    still_label(sample.actual_ms, duration)
                );
            }
        }
        (StillsLayout::Sheet { cols, .. }, _) => {
            let _ = writeln!(
                text,
                "A sheet of {} frames, {cols} across, read left to right and top to bottom, \
                 each labeled with its time:",
                video.samples.len()
            );
            for (at, sample) in video.samples.iter().enumerate() {
                let _ = writeln!(
                    text,
                    "{}. {}",
                    at + 1,
                    still_label(sample.actual_ms, duration)
                );
            }
        }
    }
    if video.tolerance_ms > 0 {
        let _ = writeln!(
            text,
            "Each frame's time is within {} of the instant it stands for.",
            span(u64::from(video.tolerance_ms))
        );
    }
    text.push_str("This is what you have seen of the video, not all of it.");
    text
}

/// Who may install a decoder, said once a decoder is what was missing.
pub(crate) fn decoder_hint(failure: FailureCode) -> Option<&'static str> {
    matches!(
        failure,
        FailureCode::UnsupportedContainer
            | FailureCode::CodecUnavailable
            | FailureCode::BackendUnavailable
    )
    .then_some(if cfg!(target_os = "macos") {
        "The user can install ffmpeg (brew install ffmpeg) to read this video; do not install software yourself."
    } else {
        "The user can install ffmpeg from the system's packages (for example, apt install ffmpeg) to read this video; do not install software yourself."
    })
}

/// Why the sheet was not written, in fixed words that name no path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputRefusal {
    /// The output names the video itself.
    IsInput,
    /// Someone else could replace a file in the output's folder: it can be
    /// written by others and has no sticky bit.
    Shared,
    /// The folder could not take the file.
    Unwritable,
    /// Something else took or left the output's name while the sheet was
    /// being written. Nothing was replaced.
    Changed,
    /// The folder's filesystem has no step that places a file without
    /// replacing whatever holds the name.
    NoSafePlacement,
    /// A file that was moved off the output's name could not be moved back,
    /// and is kept under a private name beside it.
    Stranded,
}

impl OutputRefusal {
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::IsInput => "The output would replace the video itself; choose another path.",
            Self::Shared => {
                "Other users can change the output's folder; write the sheet somewhere only you can change."
            }
            Self::Unwritable => "The sheet could not be written there.",
            Self::Changed => {
                "The output's name was changed by something else while the sheet was being written; nothing was replaced. Try again."
            }
            Self::NoSafePlacement => {
                "The output folder's filesystem cannot place the sheet without risking another file; write it to a folder on a local disk."
            }
            Self::Stranded => {
                "The output's name changed while the sheet was being written, and a file moved off it could not be moved back; it is kept in the same folder under a name beginning with .kettle-"
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
fn shared(directory: &Path) -> Result<bool, OutputRefusal> {
    shared_directory(&std::fs::File::open(directory).map_err(|_| OutputRefusal::Unwritable)?)
}

/// Whether someone other than the owner could replace an entry in
/// `directory`: it is group- or world-writable (or, on macOS, an ACL lets
/// another write) and has no sticky bit.
#[cfg(unix)]
fn shared_directory(directory: &std::fs::File) -> Result<bool, OutputRefusal> {
    use std::os::unix::fs::MetadataExt as _;
    let found = directory
        .metadata()
        .map_err(|_| OutputRefusal::Unwritable)?;
    // SAFETY: geteuid has no preconditions.
    if shared_mode(found.uid(), found.mode(), unsafe { libc::geteuid() }) {
        return Ok(true);
    }
    if found.mode() & 0o1000 != 0 {
        return Ok(false);
    }
    #[cfg(target_os = "macos")]
    if kettle_media_native::acl::grants_write_file(directory) {
        return Ok(true);
    }
    Ok(false)
}

#[cfg(any(unix, test))]
fn shared_mode(owner: u32, mode: u32, user: u32) -> bool {
    (owner != 0 && owner != user) || (mode & 0o1000 == 0 && mode & 0o022 != 0)
}

/// A name no one can guess ahead of time: this process's randomly keyed
/// hash of the clock.
fn unpredictable() -> u64 {
    use std::hash::{BuildHasher as _, Hasher as _};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(now.as_nanos());
    }
    hasher.finish()
}

/// A hidden name in the output's folder that nothing else could have chosen.
fn private_name(suffix: &str) -> OsString {
    format!(".kettle-{:016x}.{suffix}", unpredictable()).into()
}

/// Which file a name holds, whatever it is called. The file number is wide
/// enough for Windows' 128-bit identifiers, which ReFS needs to keep files
/// apart; Unix numbers fit in its low half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileId {
    volume: u64,
    file: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A regular file.
    File,
    /// A pipe, socket or device.
    #[cfg(unix)]
    Other,
    Folder,
    Link,
}

/// What a name in the output's folder holds, never following a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    id: FileId,
    kind: Kind,
}

/// The steps a filesystem may not have. Tests take them away to reach each
/// fallback on a filesystem that has them all, and read back which ran.
const EXCHANGE: u8 = 1;
const RENAME_NEW: u8 = 2;
const LINK_NEW: u8 = 4;

#[cfg(test)]
thread_local! {
    static LACKING: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static RAN: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    /// Run once, right after the sheet and the output have traded names.
    static AFTER_EXCHANGE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    /// Run once, when the sheet is written and about to be published.
    static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run one publication step, unless a test says this filesystem lacks it.
fn step(which: u8, run: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    #[cfg(test)]
    if LACKING.get() & which != 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let done = run();
    #[cfg(test)]
    if done.is_ok() {
        RAN.set(RAN.get() | which);
    }
    #[cfg(not(test))]
    let _ = which;
    done
}

/// Whether a step failed because the filesystem or the system does not have
/// it, not because of what the names hold. Linux answers `EINVAL` for a
/// rename flag the filesystem does not take, macOS `ENOTSUP`.
fn unsupported(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    {
        // Some of these share a value on some systems, so not a pattern.
        let refusals = [
            libc::EINVAL,
            libc::ENOTSUP,
            libc::EOPNOTSUPP,
            libc::ENOSYS,
            libc::EXDEV,
        ];
        error
            .raw_os_error()
            .is_some_and(|code| refusals.contains(&code))
    }
    #[cfg(not(unix))]
    false
}

/// The output's folder, held open so that no later step can be led to
/// another folder by a change to the path.
#[cfg(unix)]
struct Folder {
    held: std::fs::File,
}

#[cfg(unix)]
fn leaf(name: &OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(name.as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

#[cfg(unix)]
fn file_id(file: &std::fs::File) -> io::Result<FileId> {
    use std::os::unix::fs::MetadataExt as _;
    let found = file.metadata()?;
    Ok(FileId {
        volume: found.dev(),
        file: u128::from(found.ino()),
    })
}

#[cfg(unix)]
fn kind(mode: libc::mode_t) -> Kind {
    match mode & libc::S_IFMT {
        libc::S_IFREG => Kind::File,
        libc::S_IFDIR => Kind::Folder,
        libc::S_IFLNK => Kind::Link,
        _ => Kind::Other,
    }
}

/// What an open file is, by its own descriptor.
#[cfg(unix)]
fn described(file: &std::fs::File) -> io::Result<Entry> {
    use std::os::unix::fs::MetadataExt as _;
    let found = file.metadata()?;
    // The width of mode_t varies across Unix targets.
    #[allow(clippy::unnecessary_cast)]
    let mode = found.mode() as libc::mode_t;
    Ok(Entry {
        id: FileId {
            volume: found.dev(),
            file: u128::from(found.ino()),
        },
        kind: kind(mode),
    })
}

/// The video's identity: the one attested when it was opened.
#[cfg(unix)]
fn input_id(_: &Path, attested: ExternalAttested) -> Option<FileId> {
    Some(FileId {
        volume: attested.dev,
        file: u128::from(attested.ino),
    })
}

#[cfg(unix)]
impl Folder {
    fn open(directory: &Path) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        // Linux can pin a writable/searchable directory without read permission.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let access = libc::O_PATH;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let access = libc::O_RDONLY;
        let held = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(access | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(directory)?;
        Ok(Self { held })
    }

    fn shared(&self) -> Result<bool, OutputRefusal> {
        shared_directory(&self.held)
    }

    /// A new private file under a hidden, unpredictable name made by this
    /// call and by nothing else. Created as a user-selected export, so on
    /// macOS it never inherits an ACL from the folder that would let another
    /// user read the frames.
    fn create(&self) -> Result<(OsString, std::fs::File), OutputRefusal> {
        for _ in 0..16 {
            let candidate = private_name("tmp");
            match kettle_state::create_user_selected_file_new_at(&self.held, &candidate) {
                Ok(file) => return Ok((candidate, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(OutputRefusal::Unwritable),
            }
        }
        Err(OutputRefusal::Unwritable)
    }

    fn entry(&self, name: &OsStr) -> io::Result<Option<Entry>> {
        use std::os::fd::AsRawFd as _;
        let name = leaf(name)?;
        // SAFETY: stat contains only integer fields, which admit all-zero bits.
        let mut found: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: the folder stays open and name is NUL-terminated; found is
        // an exclusive writable stat output buffer throughout fstatat.
        if unsafe {
            libc::fstatat(
                self.held.as_raw_fd(),
                name.as_ptr(),
                &mut found,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error)
            };
        }
        // The stat field widths vary across Unix targets.
        #[allow(clippy::unnecessary_cast)]
        let id = FileId {
            volume: found.st_dev as u64,
            file: u128::from(found.st_ino as u64),
        };
        Ok(Some(Entry {
            id,
            kind: kind(found.st_mode),
        }))
    }

    /// What `name` holds before anything is written, held open where the
    /// system allows. A filesystem may give a removed file's number to the
    /// next one made, which could then pass for the file examined here; a
    /// file held open keeps its number to itself.
    fn examine(&self, name: &OsStr) -> io::Result<(Option<Entry>, Option<std::fs::File>)> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        let leaf = leaf(name)?;
        // `O_PATH` holds an entry of any kind, a link itself included,
        // needs no permission on it and has no effect on it.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            // SAFETY: the folder stays open and leaf is NUL-terminated.
            let held = unsafe {
                libc::openat(
                    self.held.as_raw_fd(),
                    leaf.as_ptr(),
                    libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if held < 0 {
                let error = io::Error::last_os_error();
                return if error.kind() == io::ErrorKind::NotFound {
                    Ok((None, None))
                } else {
                    Err(error)
                };
            }
            // SAFETY: openat returned a new descriptor that nothing else owns.
            let held = unsafe { std::fs::File::from_raw_fd(held) };
            Ok((Some(described(&held)?), Some(held)))
        }
        // Elsewhere only a regular file this user may read can be held.
        // One that cannot is still told apart by its number and kind, on
        // filesystems (APFS among them) that do not give numbers out again.
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            let Some(entry) = self.entry(name)? else {
                return Ok((None, None));
            };
            if entry.kind != Kind::File {
                return Ok((Some(entry), None));
            }
            // SAFETY: the folder stays open and leaf is NUL-terminated.
            let held = unsafe {
                libc::openat(
                    self.held.as_raw_fd(),
                    leaf.as_ptr(),
                    libc::O_RDONLY
                        | libc::O_NONBLOCK
                        | libc::O_NOCTTY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                )
            };
            if held < 0 {
                return Ok((Some(entry), None));
            }
            // SAFETY: openat returned a new descriptor that nothing else owns.
            let held = unsafe { std::fs::File::from_raw_fd(held) };
            // What the name held when it was opened, which may not be what
            // it held a moment before: the file held is the one examined.
            Ok((Some(described(&held)?), Some(held)))
        }
    }

    /// Rename within the held folder: the two names trade what they hold
    /// (`exchange`), or `from` moves to `to` only while `to` is free. Both
    /// are one atomic step and neither can replace a file.
    fn rename_flagged(&self, from: &OsStr, to: &OsStr, exchange: bool) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;
        let (from, to) = (leaf(from)?, leaf(to)?);
        let folder = self.held.as_raw_fd();
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let flags = if exchange {
                libc::RENAME_EXCHANGE
            } else {
                libc::RENAME_NOREPLACE
            };
            // SAFETY: the folder stays open, both names are NUL-terminated,
            // and both flags are valid for renameat2.
            if unsafe { libc::renameat2(folder, from.as_ptr(), folder, to.as_ptr(), flags) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        #[cfg(target_os = "macos")]
        {
            // Not in libc: `renameatx_np` is a Darwin extension.
            unsafe extern "C" {
                fn renameatx_np(
                    from_dir: libc::c_int,
                    from: *const libc::c_char,
                    to_dir: libc::c_int,
                    to: *const libc::c_char,
                    flags: libc::c_uint,
                ) -> libc::c_int;
            }
            const RENAME_SWAP: libc::c_uint = 0x0000_0002;
            const RENAME_EXCL: libc::c_uint = 0x0000_0004;
            let flags = if exchange { RENAME_SWAP } else { RENAME_EXCL };
            // SAFETY: the folder stays open, both names are NUL-terminated,
            // and both flags are valid for renameatx_np.
            if unsafe { renameatx_np(folder, from.as_ptr(), folder, to.as_ptr(), flags) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        {
            let _ = (folder, from, to, exchange);
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    fn exchange(&self, one: &OsStr, other: &OsStr) -> io::Result<()> {
        self.rename_flagged(one, other, true)
    }

    fn rename_new(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        self.rename_flagged(from, to, false)
    }

    /// A second name for `from`, made only while `to` is free.
    fn link_new(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;
        let (from, to) = (leaf(from)?, leaf(to)?);
        let folder = self.held.as_raw_fd();
        // SAFETY: the folder stays open and both names are NUL-terminated.
        if unsafe { libc::linkat(folder, from.as_ptr(), folder, to.as_ptr(), 0) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// A plain rename, which replaces whatever holds `to`.
    fn rename(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;
        let (from, to) = (leaf(from)?, leaf(to)?);
        let folder = self.held.as_raw_fd();
        // SAFETY: the folder stays open and both names are NUL-terminated.
        if unsafe { libc::renameat(folder, from.as_ptr(), folder, to.as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn remove(&self, name: &OsStr) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;
        let name = leaf(name)?;
        // SAFETY: the folder stays open and name is NUL-terminated.
        if unsafe { libc::unlinkat(self.held.as_raw_fd(), name.as_ptr(), 0) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

/// Windows has no media worker, so no sheet is ever written there; the same
/// publication runs by path, since Windows cannot pin a replaced parent.
#[cfg(not(unix))]
struct Folder {
    path: PathBuf,
}

#[cfg(not(unix))]
fn information(file: &std::fs::File) -> io::Result<Entry> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    };
    let handle = file.as_raw_handle() as HANDLE;
    let mut found = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: file owns a valid handle and found is writable for the exact
    // structure GetFileInformationByHandle fills.
    if unsafe { GetFileInformationByHandle(handle, &raw mut found) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut wide = FILE_ID_INFO::default();
    // SAFETY: file owns a valid handle and wide is writable for the size
    // passed, the exact structure FileIdInfo fills.
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&raw mut wide).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let kind = if found.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        Kind::Link
    } else if found.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        Kind::Folder
    } else {
        Kind::File
    };
    // Not the 64-bit index, which ReFS does not keep unique, and no stand-in
    // for a volume without the 128-bit identifier: there nothing is published.
    let id = FileId {
        volume: wide.VolumeSerialNumber,
        file: u128::from_le_bytes(wide.FileId.Identifier),
    };
    Ok(Entry { id, kind })
}

/// Open `path` to ask what it is, with no access to its contents.
#[cfg(not(unix))]
fn inspect(path: &Path, follow: bool) -> io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    let reparse = if follow {
        0
    } else {
        FILE_FLAG_OPEN_REPARSE_POINT
    };
    std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | reparse)
        .open(path)
}

#[cfg(not(unix))]
fn file_id(file: &std::fs::File) -> io::Result<FileId> {
    information(file).map(|entry| entry.id)
}

/// The video's identity as its path gives it now: nothing attests one on
/// Windows.
#[cfg(not(unix))]
fn input_id(path: &Path, _: ExternalAttested) -> Option<FileId> {
    inspect(path, true).and_then(|file| file_id(&file)).ok()
}

#[cfg(not(unix))]
impl Folder {
    /// Resolved once, to the verbatim path every later step uses, so a
    /// name near the component limit stays within `MoveFileExW`'s reach.
    fn open(directory: &Path) -> io::Result<Self> {
        Ok(Self {
            path: std::fs::canonicalize(directory)?,
        })
    }

    fn shared(&self) -> Result<bool, OutputRefusal> {
        Ok(false)
    }

    fn create(&self) -> Result<(OsString, std::fs::File), OutputRefusal> {
        for _ in 0..16 {
            let candidate = private_name("tmp");
            match kettle_state::create_user_selected_file_new(&self.path.join(&candidate)) {
                Ok(file) => return Ok((candidate, file.persist())),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(OutputRefusal::Unwritable),
            }
        }
        Err(OutputRefusal::Unwritable)
    }

    fn entry(&self, name: &OsStr) -> io::Result<Option<Entry>> {
        match inspect(&self.path.join(name), false) {
            Ok(file) => information(&file).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// What `name` holds before anything is written. Nothing is held open:
    /// an NTFS file number carries a count that changes when the number is
    /// given out again, so a later file cannot pass for this one.
    fn examine(&self, name: &OsStr) -> io::Result<(Option<Entry>, Option<std::fs::File>)> {
        Ok((self.entry(name)?, None))
    }

    /// Windows has no exchange of two names; the sheet is published by
    /// moving the old output aside, which `MoveFileExW` does on any volume.
    fn exchange(&self, _: &OsStr, _: &OsStr) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// `from` moves to `to` only while `to` is free.
    fn rename_new(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        use std::os::windows::ffi::OsStrExt as _;
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        let wide = |name: &OsStr| -> Vec<u16> {
            self.path
                .join(name)
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        };
        let (from, to) = (wide(from), wide(to));
        // SAFETY: both paths are NUL-terminated and outlive the call; with
        // no flags MoveFileExW fails when the new name exists.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn link_new(&self, _: &OsStr, _: &OsStr) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn rename(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        std::fs::rename(self.path.join(from), self.path.join(to))
    }

    fn remove(&self, name: &OsStr) -> io::Result<()> {
        std::fs::remove_file(self.path.join(name))
    }
}

/// How a move to a name that must be free ended.
enum Placed {
    Done,
    /// Something holds the name; nothing moved.
    Taken,
}

impl Folder {
    /// Remove `name` only while it holds `file`.
    fn remove_only(&self, name: &OsStr, file: FileId) {
        if matches!(self.entry(name), Ok(Some(entry)) if entry.id == file) {
            let _ = self.remove(name);
        }
    }

    /// Move `from`, which holds `file`, to `to` only while `to` is free: an
    /// exclusive rename, or where the filesystem has none, a hard link
    /// followed by removing the old name.
    fn place_new(&self, from: &OsStr, file: FileId, to: &OsStr) -> Result<Placed, OutputRefusal> {
        let error = match step(RENAME_NEW, || self.rename_new(from, to)) {
            Ok(()) => return Ok(Placed::Done),
            Err(error) => error,
        };
        if error.kind() == io::ErrorKind::AlreadyExists {
            return Ok(Placed::Taken);
        }
        if !unsupported(&error) {
            return Err(OutputRefusal::Unwritable);
        }
        match step(LINK_NEW, || self.link_new(from, to)) {
            Ok(()) => {
                self.remove_only(from, file);
                Ok(Placed::Done)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(Placed::Taken),
            Err(error) if unsupported(&error) => Err(OutputRefusal::NoSafePlacement),
            Err(_) => Err(OutputRefusal::Unwritable),
        }
    }

    /// Put the finished sheet, `own` under the name `temporary`, at `name`,
    /// replacing only `checked`: what `name` held when it was examined.
    ///
    /// No step here can destroy a file. A free name is taken by a move that
    /// fails when the name is held. A held name is exchanged with the
    /// sheet's, so whatever held it at that instant survives under the
    /// private name; it is removed only when it is the file that was
    /// examined, and is exchanged back otherwise. A video moved onto the
    /// output's name after the check is therefore never lost.
    fn publish(
        &self,
        name: &OsStr,
        checked: Option<Entry>,
        temporary: &OsStr,
        own: FileId,
        input: Option<FileId>,
    ) -> Result<(), OutputRefusal> {
        let changed = |now: Option<FileId>| {
            if now.is_some() && now == input {
                OutputRefusal::IsInput
            } else {
                OutputRefusal::Changed
            }
        };
        let holds = |name: &OsStr| self.entry(name).ok().flatten().map(|entry| entry.id);
        #[cfg(test)]
        if let Some(hook) = BEFORE_PUBLISH.take() {
            hook();
        }
        // Something else under the private name is not published, and is
        // left where it is.
        if holds(temporary) != Some(own) {
            return Err(OutputRefusal::Changed);
        }
        let Some(checked) = checked else {
            return match self.place_new(temporary, own, name)? {
                Placed::Done => Ok(()),
                Placed::Taken => Err(changed(holds(name))),
            };
        };
        match step(EXCHANGE, || self.exchange(temporary, name)) {
            Ok(()) => {
                #[cfg(test)]
                if let Some(hook) = AFTER_EXCHANGE.take() {
                    hook();
                }
                let displaced = self.entry(temporary).ok().flatten();
                if displaced == Some(checked) && holds(name) == Some(own) {
                    let _ = self.remove(temporary);
                    return Ok(());
                }
                let refusal = changed(displaced.map(|entry| entry.id));
                let back = step(EXCHANGE, || self.exchange(temporary, name));
                match self.entry(temporary) {
                    // The sheet is under its private name again, where the
                    // caller removes it, or something else already has.
                    Ok(Some(entry)) if entry.id == own => Err(refusal),
                    Ok(None) if back.is_ok() => Err(refusal),
                    // Nothing was exchanged back: what the sheet displaced
                    // had already left the private name. The sheet is
                    // published only if the output's name still holds it.
                    Ok(None) if holds(name) == Some(own) => Ok(()),
                    Ok(None) => Err(OutputRefusal::Changed),
                    // The private name holds a file that is not this
                    // call's: what was displaced and could not go back, or
                    // what took the output's name between the two
                    // exchanges. It is left there.
                    _ => Err(OutputRefusal::Stranded),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Err(OutputRefusal::Changed),
            Err(error) if unsupported(&error) => {
                self.publish_aside(name, checked, temporary, own, &changed)
            }
            Err(_) => Err(OutputRefusal::Unwritable),
        }
    }

    /// Publication where the filesystem cannot exchange two names: the old
    /// output moves aside to a private name, the sheet takes the free name
    /// by a move that cannot replace, and the old output is then removed.
    /// What moved aside is put back when it is not the file that was
    /// examined, or when the sheet could not take the name. The name is
    /// briefly free, and a crash can leave the old output under its private
    /// name, but no file is destroyed unexamined. A filesystem with neither
    /// an exclusive rename nor hard links is refused before anything moves.
    fn publish_aside(
        &self,
        name: &OsStr,
        checked: Entry,
        temporary: &OsStr,
        own: FileId,
        changed: &dyn Fn(Option<FileId>) -> OutputRefusal,
    ) -> Result<(), OutputRefusal> {
        let aside = private_name("old");
        match step(RENAME_NEW, || self.rename_new(name, &aside)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(OutputRefusal::Changed);
            }
            Err(error) if unsupported(&error) => {
                // The sheet will need a hard link to take the name without
                // replacing; find out that links work while nothing has moved.
                let probe = private_name("tmp");
                match step(LINK_NEW, || self.link_new(temporary, &probe)) {
                    Ok(()) => {}
                    Err(error) if unsupported(&error) => {
                        return Err(OutputRefusal::NoSafePlacement);
                    }
                    Err(_) => return Err(OutputRefusal::Unwritable),
                }
                self.remove_only(&probe, own);
                // A plain rename replaces only what already holds the new
                // name, and nothing holds a name no one could have guessed.
                match self.rename(name, &aside) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        return Err(OutputRefusal::Changed);
                    }
                    Err(_) => return Err(OutputRefusal::Unwritable),
                }
            }
            Err(_) => return Err(OutputRefusal::Unwritable),
        }
        let displaced = match self.entry(&aside) {
            Ok(Some(displaced)) => displaced,
            // Moved on again by whoever is changing the folder; there is
            // nothing here to put back.
            Ok(None) => return Err(OutputRefusal::Changed),
            // Unreadable, so not known to be the file examined: left there.
            Err(_) => return Err(OutputRefusal::Stranded),
        };
        let refusal = if displaced == checked {
            match self.place_new(temporary, own, name) {
                Ok(Placed::Done) => {
                    self.remove_only(&aside, checked.id);
                    return Ok(());
                }
                Ok(Placed::Taken) => OutputRefusal::Changed,
                Err(refusal) => refusal,
            }
        } else {
            changed(Some(displaced.id))
        };
        match self.place_new(&aside, displaced.id, name) {
            Ok(Placed::Done) => Err(refusal),
            _ => Err(OutputRefusal::Stranded),
        }
    }
}

/// Write `bytes` to `out` whole or not at all: a private file of this
/// call's own beside it, published over it without a step that could
/// destroy any other file (see [`Folder::publish`]), in a folder no one else
/// can change, and never over the video. The only files removed are this
/// call's own and the output that was examined and replaced.
#[cfg(test)]
fn write_out(out: &Path, input: &Path, bytes: &[u8]) -> Result<(), OutputRefusal> {
    write_out_with(out, input, bytes, || {})
}

#[cfg(test)]
fn write_out_with(
    out: &Path,
    input: &Path,
    bytes: &[u8],
    after_check: impl FnOnce(),
) -> Result<(), OutputRefusal> {
    let attestation = crate::show_cli::attest(input).map_err(|_| OutputRefusal::Unwritable)?;
    write_out_attested(out, input, attestation, bytes, after_check)
}

fn write_out_attested(
    out: &Path,
    input_path: &Path,
    input: ExternalAttested,
    bytes: &[u8],
    after_check: impl FnOnce(),
) -> Result<(), OutputRefusal> {
    let directory = out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = out.file_name().ok_or(OutputRefusal::Unwritable)?;
    let input = input_id(input_path, input);
    let folder = Folder::open(directory).map_err(|_| OutputRefusal::Unwritable)?;
    // Held until the end, so that what publication finds can be compared
    // with what was examined here.
    let (checked, _held) = folder
        .examine(name)
        .map_err(|_| OutputRefusal::Unwritable)?;
    if let Some(found) = checked {
        if Some(found.id) == input {
            return Err(OutputRefusal::IsInput);
        }
        if matches!(found.kind, Kind::Folder | Kind::Link) {
            return Err(OutputRefusal::Unwritable);
        }
    }
    // The input path may now name a replacement for the attested video.
    if let (Ok(out), Ok(input)) = (
        std::fs::canonicalize(out),
        std::fs::canonicalize(input_path),
    ) && out == input
    {
        return Err(OutputRefusal::IsInput);
    }
    after_check();
    if folder.shared()? {
        return Err(OutputRefusal::Shared);
    }
    let (temporary, mut file) = folder.create()?;
    // Unidentified, it could not be told from a file put in its place, so
    // it is left as it is: empty, under a hidden name.
    let own = file_id(&file).map_err(|_| OutputRefusal::Unwritable)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    // Windows refuses to rename a file while a handle without delete sharing
    // is open on it, and the private create grants none. Elsewhere the open
    // file keeps its identity from being reused while names are compared.
    #[cfg(not(unix))]
    drop(file);
    let published = match written {
        Ok(()) => folder.publish(name, checked, &temporary, own, input),
        Err(_) => Err(OutputRefusal::Unwritable),
    };
    // After an exchange the private name may hold someone's file, so it is
    // removed only while it still holds this call's own.
    folder.remove_only(&temporary, own);
    published
}

/// Run `kettle video-frames …`; returns the exit code (0 written, 1 not).
pub(crate) fn run(request: FramesRequest, out: &Path) -> i32 {
    let sheet = crate::media_client()
        .ok_or(FailureCode::WorkerUnavailable)
        .and_then(|client| render_attested(&request, &client, None))
        .and_then(|(sheet, input)| sheet.jpeg(&|_| true).map(|jpeg| (sheet, jpeg, input)));
    let (sheet, jpeg, input) = match sheet {
        Ok(taken) => taken,
        Err(failure) => {
            eprintln!("kettle video-frames: {}", failure.model_message());
            if let Some(hint) = decoder_hint(failure) {
                eprintln!("kettle video-frames: {hint}");
            }
            return 1;
        }
    };
    if let Err(refusal) = write_out_attested(out, &request.path, input, &jpeg, || {}) {
        eprintln!("kettle video-frames: {}", refusal.text());
        return 1;
    }
    println!("{}", sheet.index());
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> FramesRequest {
        FramesRequest {
            path: PathBuf::from("/x/clip.webm"),
            ..FramesRequest::default()
        }
    }

    /// Defaults are nine frames, three across, on a 1568 edge; `at_s` is one
    /// frame; a window narrows the sheet.
    #[test]
    fn requests_become_bounded_stills() {
        let stills = request().stills().unwrap();
        assert_eq!(
            (stills.count, stills.max_edge, stills.layout),
            (
                9,
                1568,
                StillsLayout::Sheet {
                    cols: 3,
                    labels: true
                }
            )
        );
        let one = FramesRequest {
            at_s: Some(3.5),
            ..request()
        }
        .stills()
        .unwrap();
        assert_eq!(
            (one.count, one.at_s, one.layout),
            (
                1,
                Some(3.5),
                StillsLayout::Sheet {
                    cols: 1,
                    labels: true
                }
            ),
            "a single frame is labeled too"
        );
        let window = FramesRequest {
            start_s: Some(2.0),
            end_s: Some(4.0),
            count: Some(16),
            max_edge: Some(2560),
            ..request()
        }
        .stills()
        .unwrap();
        assert_eq!(
            (window.start_s, window.end_s, window.count, window.layout),
            (
                2.0,
                Some(4.0),
                16,
                StillsLayout::Sheet {
                    cols: 4,
                    labels: true
                }
            )
        );
        assert_eq!(columns(2), 2);
        assert_eq!(columns(5), 3);
        assert_eq!(columns(1), 1);
    }

    /// Times that are not finite or are negative, a window ending at or
    /// before its start, `at_s` beside a window or several frames, no frames
    /// or seventeen, and an edge of 0 or past 2560 are refused.
    #[test]
    fn bad_requests_are_refused() {
        for bad in [
            FramesRequest {
                start_s: Some(-1.0),
                ..request()
            },
            FramesRequest {
                end_s: Some(f64::NAN),
                ..request()
            },
            FramesRequest {
                at_s: Some(f64::INFINITY),
                ..request()
            },
            FramesRequest {
                start_s: Some(4.0),
                end_s: Some(4.0),
                ..request()
            },
            FramesRequest {
                at_s: Some(1.0),
                end_s: Some(2.0),
                ..request()
            },
            FramesRequest {
                at_s: Some(1.0),
                count: Some(9),
                ..request()
            },
            FramesRequest {
                at_s: Some(1.0),
                count: Some(1),
                ..request()
            },
            FramesRequest {
                path: PathBuf::from(format!("/{}", "a/".repeat(2100))),
                ..request()
            },
            FramesRequest {
                count: Some(0),
                ..request()
            },
            FramesRequest {
                count: Some(17),
                ..request()
            },
            FramesRequest {
                max_edge: Some(0),
                ..request()
            },
            FramesRequest {
                max_edge: Some(2561),
                ..request()
            },
        ] {
            assert_eq!(bad.stills(), Err(FailureCode::BadParams), "{bad:?}");
        }
    }

    fn sheet(width: u32, height: u32, noisy: bool) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            if noisy {
                let seed = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503);
                let byte = |shift: u32| (seed.rotate_left(shift) & 0xff) as u8;
                image::Rgba([byte(0), byte(8), byte(16), 255])
            } else {
                image::Rgba([40, 80, 120, 255])
            }
        })
    }

    /// A sheet is encoded at the best quality that fits, then shrunk a
    /// quarter at a time; one that fits nowhere is too large, never cut.
    #[test]
    fn sheets_shrink_until_they_fit() {
        let (flat, width, _) = encode(sheet(800, 450, false), &|_| true).unwrap();
        assert_eq!(width, 800);
        assert_eq!(&flat[..2], &[0xff, 0xd8], "a JPEG");
        let full = encode(sheet(800, 450, true), &|_| true).unwrap().0.len();
        let limit = full / 3;
        let (fitted, width, height) =
            encode(sheet(800, 450, true), &|jpeg| jpeg.len() <= limit).unwrap();
        assert!(fitted.len() <= limit);
        assert!(width < 800 && height < 450, "{width}x{height}");
        assert_eq!(
            encode(sheet(800, 450, true), &|_| false).unwrap_err(),
            FailureCode::TooLarge
        );
        // The minimum itself is tried: 800, 600, 450, 337, then 320.
        let (_, width, _) = encode(sheet(800, 450, true), &|jpeg| {
            image::load_from_memory(jpeg).is_ok_and(|image| image.width() <= 320)
        })
        .unwrap();
        assert_eq!(width, 320);
    }

    fn video(tolerance_ms: u32) -> VideoStillsResult {
        VideoStillsResult {
            info: kettle_media::VideoInfo {
                duration_ms: 12_400,
                width: 1280,
                height: 720,
                rotation: 0,
                codec: VideoCodec::Vp8,
                fps_milli: Some(29_970),
                has_audio: false,
                container: Some(kettle_media::video::VideoContainer::WebM),
            },
            samples: vec![
                kettle_media::StillSample {
                    requested_ms: 3_100,
                    actual_ms: 3_000,
                },
                kettle_media::StillSample {
                    requested_ms: 9_300,
                    actual_ms: 9_300,
                },
            ],
            tolerance_ms,
        }
    }

    /// The index names the video, each frame's actual time in layout order,
    /// how far a time may be from its instant, and that this is not the whole
    /// video.
    #[test]
    fn the_index_says_what_each_frame_is() {
        let text = index(
            &video(100),
            StillsLayout::Sheet {
                cols: 2,
                labels: true,
            },
        );
        assert_eq!(
            text,
            "Video: 00:12.4, 1280x720, VP8, 29.97 fps, no audio.\n\
             A sheet of 2 frames, 2 across, read left to right and top to bottom, \
             each labeled with its time:\n\
             1. 00:03.0\n\
             2. 00:09.3\n\
             Each frame's time is within 100 ms of the instant it stands for.\n\
             This is what you have seen of the video, not all of it."
        );
        let mut one = video(0);
        one.samples.truncate(1);
        one.info.fps_milli = None;
        one.info.has_audio = true;
        assert_eq!(
            index(&one, StillsLayout::Poster),
            "Video: 00:12.4, 1280x720, VP8, with audio.\n\
             One frame, labeled with its time: 00:03.0.\n\
             This is what you have seen of the video, not all of it."
        );
        assert_eq!(span(22), "22 ms");
        assert_eq!(span(1_450), "1.5 s", "never less than it is");
        assert_eq!(span(1_400), "1.4 s");
        assert_eq!(rate(25_000), "25 fps");
        assert_eq!(rate(23_976), "23.976 fps");
    }

    #[test]
    fn only_a_missing_decoder_brings_the_install_hint() {
        assert!(decoder_hint(FailureCode::UnsupportedContainer).is_some());
        assert!(decoder_hint(FailureCode::CodecUnavailable).is_some());
        assert!(decoder_hint(FailureCode::BackendUnavailable).is_some());
        assert!(decoder_hint(FailureCode::RenderParse).is_none());
        assert!(
            decoder_hint(FailureCode::BackendUnavailable)
                .unwrap()
                .contains("do not install software yourself")
        );
    }

    /// The output is written whole beside where it goes, privately, and
    /// never over the video itself; a folder others can change is refused
    /// unless it is sticky; a failure removes nothing this call did not make.
    #[test]
    fn output_is_written_whole_and_never_over_the_input() {
        let directory = kettle_test_support::private_tempdir("kettle-video-output-");
        let input = directory.path().join("clip.webm");
        std::fs::write(&input, b"video").unwrap();
        let out = directory.path().join("frames.jpg");
        write_out(&out, &input, b"jpeg").unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"jpeg");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            write_out(&input, &input, b"jpeg").unwrap_err(),
            OutputRefusal::IsInput
        );
        assert_eq!(std::fs::read(&input).unwrap(), b"video");
        assert_eq!(
            write_out(
                &directory.path().join("missing/frames.jpg"),
                &input,
                b"jpeg"
            )
            .unwrap_err(),
            OutputRefusal::Unwritable
        );
        let leftovers: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn output_in_write_search_only_directory() {
        use std::os::unix::fs::PermissionsExt as _;

        // Root bypasses the permission check this regression exercises.
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipping write/search-only directory check as root");
            return;
        }
        let root = kettle_test_support::private_tempdir("kettle-video-write-search-");
        let input = root.path().join("clip.webm");
        std::fs::write(&input, b"video").unwrap();
        let exports = root.path().join("exports");
        std::fs::create_dir(&exports).unwrap();
        let out = exports.join("frames.jpg");
        std::fs::set_permissions(&exports, std::fs::Permissions::from_mode(0o300)).unwrap();
        let unreadable = std::fs::read_dir(&exports).is_err();
        let result = write_out(&out, &input, b"jpeg");
        // Restore enumeration permission before asserting, so failures clean up.
        std::fs::set_permissions(&exports, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(unreadable);
        assert_eq!(result, Ok(()));
        assert_eq!(std::fs::read(&out).unwrap(), b"jpeg");
        assert_eq!(
            std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read(&input).unwrap(), b"video");
        assert_eq!(std::fs::read_dir(&exports).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn output_refuses_input_identity_and_symlink_leaves() {
        use std::os::unix::fs::symlink;
        let root = kettle_test_support::private_tempdir("kettle-video-input-identity-");
        let input = root.path().join("clip.webm");
        std::fs::write(&input, b"video").unwrap();
        let alias = root.path().join("alias.webm");
        std::fs::hard_link(&input, &alias).unwrap();
        assert_eq!(
            write_out(&alias, &input, b"jpeg"),
            Err(OutputRefusal::IsInput)
        );
        let output = root.path().join("frames.jpg");
        symlink(&input, &output).unwrap();
        assert_eq!(
            write_out(&output, &input, b"jpeg"),
            Err(OutputRefusal::Unwritable)
        );
        assert_eq!(std::fs::read(&input).unwrap(), b"video");
        assert!(
            std::fs::symlink_metadata(&output)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        std::fs::remove_file(&output).unwrap();
        std::fs::create_dir(&output).unwrap();
        assert_eq!(
            write_out(&output, &input, b"jpeg"),
            Err(OutputRefusal::Unwritable)
        );
        assert!(std::fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn output_refuses_replaced_input_path() {
        let root = kettle_test_support::private_tempdir("kettle-video-replaced-input-");
        let input = root.path().join("clip.mp4");
        let original = root.path().join("original.mp4");
        std::fs::write(&input, b"original video").unwrap();
        let attestation = crate::show_cli::attest(&input).unwrap();
        std::fs::rename(&input, &original).unwrap();
        std::fs::write(&input, b"replacement video").unwrap();

        let result = write_out_attested(&input, &input, attestation, b"jpeg", || {});
        assert_eq!(std::fs::read(&input).unwrap(), b"replacement video");
        assert_eq!(result, Err(OutputRefusal::IsInput));
        assert_eq!(std::fs::read(&original).unwrap(), b"original video");
    }

    #[cfg(unix)]
    #[test]
    fn output_parent_swap_cannot_replace_the_video() {
        use std::os::unix::fs::symlink;
        let root = kettle_test_support::private_tempdir("kettle-video-parent-swap-");
        let videos = root.path().join("videos");
        let exports = root.path().join("exports");
        let checked = root.path().join("checked");
        std::fs::create_dir(&videos).unwrap();
        std::fs::create_dir(&exports).unwrap();
        let input = videos.join("clip.webm");
        let out = exports.join("clip.webm");
        std::fs::write(&input, b"video").unwrap();
        let result = write_out_with(&out, &input, b"jpeg", || {
            std::fs::rename(&exports, &checked).unwrap();
            symlink(&videos, &exports).unwrap();
        });
        assert_eq!(
            std::fs::read(&input).unwrap(),
            b"video",
            "a swapped output parent must never overwrite the input"
        );
        if result.is_ok() {
            assert_eq!(std::fs::read(checked.join("clip.webm")).unwrap(), b"jpeg");
        }
        assert!(std::fs::read_dir(&checked).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn exported_sheet_does_not_inherit_read_acl() {
        let directory = kettle_test_support::private_tempdir("kettle-video-output-acl-");
        let folder = directory.path().join("out");
        std::fs::create_dir(&folder).unwrap();
        let status = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow read,file_inherit"])
            .arg(&folder)
            .status()
            .unwrap();
        assert!(status.success());
        let listing = |path: &Path| {
            let output = std::process::Command::new("/bin/ls")
                .arg("-le")
                .arg(path)
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap()
        };
        let inherited = folder.join("inherited");
        std::fs::write(&inherited, b"fixture").unwrap();
        assert!(listing(&inherited).contains("allow read"));
        assert!(!shared(&folder).unwrap());

        let input = directory.path().join("clip.webm");
        std::fs::write(&input, b"video").unwrap();
        let out = folder.join("frames.jpg");
        write_out(&out, &input, b"jpeg").unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"jpeg");
        assert!(!listing(&out).contains("allow read"));
    }

    #[cfg(unix)]
    #[test]
    fn output_folders_others_can_change_are_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let chmod = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("clip.webm");
        std::fs::write(&input, b"video").unwrap();
        let folder = directory.path().join("out");
        std::fs::create_dir(&folder).unwrap();
        let out = folder.join("frames.jpg");
        for (mode, refused) in [
            (0o777, true),
            (0o775, true),
            (0o1777, false),
            (0o755, false),
        ] {
            chmod(&folder, mode);
            let written = write_out(&out, &input, b"jpeg");
            if refused {
                assert_eq!(written.unwrap_err(), OutputRefusal::Shared, "{mode:o}");
            } else {
                written.unwrap_or_else(|refusal| panic!("{mode:o}: {refusal:?}"));
            }
        }
        // A folder that takes no file: nothing there is touched, including a
        // file that looks like one of this command's own. Root writes
        // anywhere, so this half cannot run as root.
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let lookalike = folder.join(".frames.jpg.0000000000000000.tmp");
        std::fs::write(&lookalike, b"someone's").unwrap();
        chmod(&folder, 0o555);
        assert_eq!(
            write_out(&out, &input, b"new").unwrap_err(),
            OutputRefusal::Unwritable
        );
        chmod(&folder, 0o755);
        assert_eq!(std::fs::read(&lookalike).unwrap(), b"someone's");
        assert_eq!(std::fs::read(&out).unwrap(), b"jpeg");
    }
    #[test]
    fn sticky_output_directory_requires_a_trusted_owner() {
        assert!(
            shared_mode(43, 0o1777, 42),
            "a foreign directory owner can replace staging entries despite sticky mode"
        );
        assert!(!shared_mode(0, 0o1777, 42));
        assert!(!shared_mode(42, 0o1777, 42));
        assert!(shared_mode(43, 0o755, 42));
        assert!(shared_mode(42, 0o777, 42));
    }

    #[test]
    fn output_filename_near_component_limit_is_supported() {
        let root = kettle_test_support::private_tempdir("kettle-video-long-output-");
        let input = root.path().join("clip.webm");
        std::fs::write(&input, b"video").unwrap();
        let output = root.path().join(format!("{}.jpg", "x".repeat(236)));
        assert_eq!(
            write_out(&output, &input, b"jpeg"),
            Ok(()),
            "valid output filename must not be lengthened for staging"
        );
        assert_eq!(std::fs::read(output).unwrap(), b"jpeg");
        assert_eq!(std::fs::read(input).unwrap(), b"video");
    }

    /// Run `body` on a filesystem that lacks `steps`; also gives the steps
    /// that ran.
    fn lacking<T>(steps: u8, body: impl FnOnce() -> T) -> (T, u8) {
        LACKING.set(steps);
        RAN.set(0);
        let result = body();
        LACKING.set(0);
        (result, RAN.get())
    }

    /// Each way to publish, as the steps taken away: the system's own best,
    /// no exchange, no exclusive rename either, and nothing at all.
    const TIERS: [u8; 4] = [
        0,
        EXCHANGE,
        EXCHANGE | RENAME_NEW,
        EXCHANGE | RENAME_NEW | LINK_NEW,
    ];

    /// Whether a filesystem lacking `steps` can still take a file without
    /// replacing one. Windows never falls back to hard links.
    fn places(steps: u8) -> bool {
        steps & RENAME_NEW == 0 || (cfg!(unix) && steps & LINK_NEW == 0)
    }

    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The race this publication exists for: the video is moved onto the
    /// output's name after the name was examined. Whether the name was free
    /// or held the last sheet, and whichever steps the filesystem has, the
    /// video is still there afterwards and nothing is left behind.
    #[test]
    fn a_video_moved_onto_the_output_name_is_never_replaced() {
        for steps in TIERS {
            for existing in [true, false] {
                let case = format!("steps {steps:03b}, existing {existing}");
                let root = kettle_test_support::private_tempdir("kettle-video-moved-input-");
                let input = root.path().join("clip.webm");
                let out = root.path().join("frames.jpg");
                std::fs::write(&input, b"video").unwrap();
                if existing {
                    std::fs::write(&out, b"old sheet").unwrap();
                }
                let (result, _) = lacking(steps, || {
                    write_out_with(&out, &input, b"jpeg", || {
                        std::fs::rename(&input, &out).unwrap();
                    })
                });
                assert_eq!(std::fs::read(&out).unwrap(), b"video", "{case}");
                assert_eq!(names(root.path()), ["frames.jpg"], "{case}");
                let refusal = if places(steps) {
                    OutputRefusal::IsInput
                } else {
                    OutputRefusal::NoSafePlacement
                };
                assert_eq!(result, Err(refusal), "{case}");
            }
        }
    }

    /// The ordinary case: the last sheet is replaced, a free name is taken,
    /// and the sheet stays private, by whichever steps the filesystem has.
    /// One with no step that cannot replace is refused, untouched. Run on
    /// each system, this also proves its own exchange is what ran.
    #[test]
    fn the_last_sheet_is_replaced_by_steps_that_cannot_destroy_a_file() {
        for steps in TIERS {
            let case = format!("steps {steps:03b}");
            let root = kettle_test_support::private_tempdir("kettle-video-replace-");
            let input = root.path().join("clip.webm");
            let out = root.path().join("frames.jpg");
            std::fs::write(&input, b"video").unwrap();
            std::fs::write(&out, b"old sheet").unwrap();
            let (replaced, replacing) = lacking(steps, || write_out(&out, &input, b"jpeg"));
            if !places(steps) {
                assert_eq!(replaced, Err(OutputRefusal::NoSafePlacement), "{case}");
                assert_eq!(std::fs::read(&out).unwrap(), b"old sheet", "{case}");
                assert_eq!(names(root.path()), ["clip.webm", "frames.jpg"], "{case}");
                std::fs::remove_file(&out).unwrap();
                let (taken, _) = lacking(steps, || write_out(&out, &input, b"jpeg"));
                assert_eq!(taken, Err(OutputRefusal::NoSafePlacement), "{case}");
                assert_eq!(names(root.path()), ["clip.webm"], "{case}");
                continue;
            }
            assert_eq!(replaced, Ok(()), "{case}");
            assert_eq!(std::fs::read(&out).unwrap(), b"jpeg", "{case}");
            assert_eq!(names(root.path()), ["clip.webm", "frames.jpg"], "{case}");
            std::fs::remove_file(&out).unwrap();
            let (taken, taking) = lacking(steps, || write_out(&out, &input, b"new"));
            assert_eq!(taken, Ok(()), "{case}");
            assert_eq!(std::fs::read(&out).unwrap(), b"new", "{case}");
            assert_eq!(names(root.path()), ["clip.webm", "frames.jpg"], "{case}");
            assert_eq!(std::fs::read(&input).unwrap(), b"video", "{case}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                assert_eq!(
                    std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
                    0o600,
                    "{case}"
                );
            }
            if steps == 0 {
                // Linux `RENAME_EXCHANGE` and macOS `RENAME_SWAP`; Windows
                // moves the last sheet aside with `MoveFileExW`.
                let native = if cfg!(any(target_os = "linux", target_os = "macos")) {
                    EXCHANGE
                } else {
                    RENAME_NEW
                };
                assert_eq!(replacing, native, "the system's own step replaces");
                assert_eq!(taking, RENAME_NEW, "an exclusive rename takes a free name");
            }
        }
    }

    /// Anything else that takes or leaves the output's name after it was
    /// examined is refused and kept: another file, a folder, a link, or
    /// nothing at all.
    #[test]
    fn a_name_that_changes_while_the_sheet_is_written_is_refused() {
        for steps in TIERS {
            let case = format!("steps {steps:03b}");
            let changed = if places(steps) {
                OutputRefusal::Changed
            } else {
                OutputRefusal::NoSafePlacement
            };
            let fixture = || {
                let root = kettle_test_support::private_tempdir("kettle-video-changed-name-");
                let input = root.path().join("clip.webm");
                let out = root.path().join("frames.jpg");
                std::fs::write(&input, b"video").unwrap();
                std::fs::write(&out, b"old sheet").unwrap();
                (root, input, out)
            };

            // Another file moved onto the last sheet.
            let (root, input, out) = fixture();
            let other = root.path().join("other.bin");
            std::fs::write(&other, b"other").unwrap();
            let (result, _) = lacking(steps, || {
                write_out_with(&out, &input, b"jpeg", || {
                    std::fs::rename(&other, &out).unwrap();
                })
            });
            assert_eq!(std::fs::read(&out).unwrap(), b"other", "{case}");
            assert_eq!(result, Err(changed), "{case}");
            assert_eq!(names(root.path()), ["clip.webm", "frames.jpg"], "{case}");

            // Another file written to a name that was free.
            let (root, input, out) = fixture();
            std::fs::remove_file(&out).unwrap();
            let (result, _) = lacking(steps, || {
                write_out_with(&out, &input, b"jpeg", || {
                    std::fs::write(&out, b"other").unwrap();
                })
            });
            assert_eq!(std::fs::read(&out).unwrap(), b"other", "{case}");
            assert_eq!(result, Err(changed), "{case}");
            assert_eq!(names(root.path()), ["clip.webm", "frames.jpg"], "{case}");

            // The last sheet removed and another file made in its place,
            // which ext4 and tmpfs give the removed file's number.
            let (root, input, out) = fixture();
            let (result, _) = lacking(steps, || {
                write_out_with(&out, &input, b"jpeg", || {
                    std::fs::remove_file(&out).unwrap();
                    std::fs::write(&out, b"other").unwrap();
                })
            });
            assert_eq!(std::fs::read(&out).unwrap(), b"other", "{case}");
            assert_eq!(result, Err(changed), "{case}");
            assert_eq!(names(root.path()), ["clip.webm", "frames.jpg"], "{case}");

            // The last sheet removed.
            let (root, input, out) = fixture();
            let (result, _) = lacking(steps, || {
                write_out_with(&out, &input, b"jpeg", || {
                    std::fs::remove_file(&out).unwrap();
                })
            });
            assert_eq!(result, Err(changed), "{case}");
            assert_eq!(names(root.path()), ["clip.webm"], "{case}");

            // A folder in its place. An exchange and an exclusive rename
            // move a folder as readily as a file; a hard link cannot, so
            // there the folder stays under the private name it was moved to.
            let (root, input, out) = fixture();
            let (result, _) = lacking(steps, || {
                write_out_with(&out, &input, b"jpeg", || {
                    std::fs::remove_file(&out).unwrap();
                    std::fs::create_dir(&out).unwrap();
                    std::fs::write(out.join("kept"), b"kept").unwrap();
                })
            });
            if cfg!(unix) && steps == EXCHANGE | RENAME_NEW {
                assert_eq!(result, Err(OutputRefusal::Stranded), "{case}");
                let left = names(root.path());
                assert_eq!(left.len(), 2, "{left:?}");
                assert!(left[0].starts_with(".kettle-"), "{left:?}");
                assert_eq!(
                    std::fs::read(root.path().join(&left[0]).join("kept")).unwrap(),
                    b"kept"
                );
            } else {
                assert_eq!(result, Err(changed), "{case}");
                assert_eq!(std::fs::read(out.join("kept")).unwrap(), b"kept", "{case}");
                assert_eq!(names(root.path()), ["clip.webm", "frames.jpg"], "{case}");
            }

            // A link to the video in its place: never followed, and kept.
            // Unix only: making a link on Windows needs a privilege.
            #[cfg(unix)]
            {
                let (root, input, out) = fixture();
                let (result, _) = lacking(steps, || {
                    write_out_with(&out, &input, b"jpeg", || {
                        std::fs::remove_file(&out).unwrap();
                        std::os::unix::fs::symlink(&input, &out).unwrap();
                    })
                });
                assert_eq!(result, Err(changed), "{case}");
                assert!(std::fs::symlink_metadata(&out).unwrap().is_symlink());
                assert_eq!(std::fs::read(&input).unwrap(), b"video", "{case}");
                assert_eq!(names(root.path()), ["clip.webm", "frames.jpg"], "{case}");
            }
        }
    }

    /// The name changes again after the sheet has traded names with it,
    /// where only the exchange itself can be raced. Whatever ends up
    /// displaced is kept, and the command never reports a sheet that is not
    /// at the output's name.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_name_that_changes_between_the_exchanges_loses_nothing() {
        let fixture = || {
            let root = kettle_test_support::private_tempdir("kettle-video-mid-exchange-");
            let input = root.path().join("clip.webm");
            let out = root.path().join("frames.jpg");
            let other = root.path().join("other.bin");
            std::fs::write(&input, b"video").unwrap();
            std::fs::write(&out, b"old sheet").unwrap();
            std::fs::write(&other, b"other").unwrap();
            (root, input, out, other)
        };
        let private = |root: &Path| -> Vec<PathBuf> {
            names(root)
                .into_iter()
                .filter(|name| name.starts_with(".kettle-"))
                .map(|name| root.join(name))
                .collect()
        };

        // The video takes the name before the exchange and a later file
        // takes it from the sheet after: the video goes back, and the later
        // file is kept under the private name and reported.
        let (root, input, out, other) = fixture();
        let later = root.path().join("later.bin");
        std::fs::write(&later, b"later").unwrap();
        AFTER_EXCHANGE.set(Some(Box::new({
            let (later, out) = (later.clone(), out.clone());
            move || std::fs::rename(&later, &out).unwrap()
        })));
        let result = write_out_with(&out, &input, b"jpeg", || {
            std::fs::rename(&input, &out).unwrap();
        });
        assert_eq!(result, Err(OutputRefusal::Stranded));
        assert_eq!(std::fs::read(&out).unwrap(), b"video");
        let kept = private(root.path());
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(std::fs::read(&kept[0]).unwrap(), b"later");
        assert_eq!(std::fs::read(&other).unwrap(), b"other");

        // Another file takes the name before the exchange, and after it
        // both that file and the sheet are removed by something else: there
        // is no sheet, so the command does not say there is.
        let (root, input, out, other) = fixture();
        AFTER_EXCHANGE.set(Some(Box::new({
            let (root, out) = (root.path().to_path_buf(), out.clone());
            move || {
                for moved in private(&root) {
                    std::fs::remove_file(moved).unwrap();
                }
                std::fs::remove_file(&out).unwrap();
            }
        })));
        let result = write_out_with(&out, &input, b"jpeg", || {
            std::fs::rename(&other, &out).unwrap();
        });
        assert_eq!(result, Err(OutputRefusal::Changed));
        assert_eq!(names(root.path()), ["clip.webm"]);

        // Another file takes the name before the exchange and leaves the
        // private name after it: the sheet is at the output's name, whole.
        let (root, input, out, other) = fixture();
        let moved_on = root.path().join("moved-on.bin");
        AFTER_EXCHANGE.set(Some(Box::new({
            let (root, moved_on) = (root.path().to_path_buf(), moved_on.clone());
            move || {
                for moved in private(&root) {
                    std::fs::rename(moved, &moved_on).unwrap();
                }
            }
        })));
        let result = write_out_with(&out, &input, b"jpeg", || {
            std::fs::rename(&other, &out).unwrap();
        });
        assert_eq!(result, Ok(()));
        assert_eq!(std::fs::read(&out).unwrap(), b"jpeg");
        assert_eq!(std::fs::read(&moved_on).unwrap(), b"other");
        assert_eq!(
            names(root.path()),
            ["clip.webm", "frames.jpg", "moved-on.bin"]
        );
    }

    /// Another file put under the sheet's private name while the sheet is
    /// written is not published in its place: the last sheet stays, or the
    /// name stays free, and that file is left where it was put.
    #[test]
    fn a_file_put_in_the_sheets_place_is_not_published() {
        for steps in TIERS {
            for existing in [true, false] {
                let case = format!("steps {steps:03b}, existing {existing}");
                let root = kettle_test_support::private_tempdir("kettle-video-swapped-sheet-");
                let input = root.path().join("clip.webm");
                let out = root.path().join("frames.jpg");
                std::fs::write(&input, b"video").unwrap();
                if existing {
                    std::fs::write(&out, b"old sheet").unwrap();
                }
                BEFORE_PUBLISH.set(Some(Box::new({
                    let root = root.path().to_path_buf();
                    move || {
                        let other = root.join("other.bin");
                        std::fs::write(&other, b"other").unwrap();
                        let private: Vec<_> = names(&root)
                            .into_iter()
                            .filter(|name| name.starts_with(".kettle-"))
                            .collect();
                        assert_eq!(private.len(), 1, "{private:?}");
                        std::fs::rename(&other, root.join(&private[0])).unwrap();
                    }
                })));
                let (result, _) = lacking(steps, || write_out(&out, &input, b"jpeg"));
                assert_eq!(result, Err(OutputRefusal::Changed), "{case}");
                if existing {
                    assert_eq!(std::fs::read(&out).unwrap(), b"old sheet", "{case}");
                } else {
                    assert!(!out.exists(), "{case}");
                }
                let left = names(root.path());
                let private: Vec<_> = left
                    .iter()
                    .filter(|name| name.starts_with(".kettle-"))
                    .collect();
                assert_eq!(private.len(), 1, "{case}: {left:?}");
                assert_eq!(
                    std::fs::read(root.path().join(private[0])).unwrap(),
                    b"other",
                    "{case}"
                );
                assert_eq!(std::fs::read(&input).unwrap(), b"video", "{case}");
            }
        }
    }

    /// The refusals that mean a filesystem lacks a step, by the numbers the
    /// systems give, apart from those that say what the names hold.
    #[cfg(unix)]
    #[test]
    fn a_missing_step_is_told_from_a_held_or_missing_name() {
        for code in [
            libc::EINVAL,
            libc::ENOTSUP,
            libc::EOPNOTSUPP,
            libc::ENOSYS,
            libc::EXDEV,
        ] {
            assert!(unsupported(&io::Error::from_raw_os_error(code)), "{code}");
        }
        for code in [libc::EEXIST, libc::ENOENT, libc::EACCES, libc::ENOSPC] {
            assert!(!unsupported(&io::Error::from_raw_os_error(code)), "{code}");
        }
    }
}
