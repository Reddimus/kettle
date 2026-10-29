//! Video stills for a model to read: `kettle video-frames` and, through the
//! same code, the `kettle_video_frames` MCP tool. The command opens the file
//! itself, inside the caller's sandbox, and renders it in a media worker it
//! starts as its own child, with no Kettle window involved: a contact sheet
//! of evenly spaced frames, or one frame, each labeled with its time, as one
//! JPEG, and a text index of what each frame is and how far its time may be
//! from the instant it stands for. Showing media to the user is
//! `kettle show`; this is reading it, a separate act.

use std::fmt::Write as _;
use std::io::Write as _;
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
pub(crate) fn render(request: &FramesRequest, client: &WorkerClient) -> Result<Sheet, FailureCode> {
    render_attested(request, client).map(|(sheet, _)| sheet)
}

fn render_attested(
    request: &FramesRequest,
    client: &WorkerClient,
) -> Result<(Sheet, ExternalAttested), FailureCode> {
    let stills = request.stills()?;
    let (job, attestation) = job(&request.path, stills)?;
    let rendered = client.render(&job)?;
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
        encode(self.image.clone(), fits).map(|(jpeg, _, _)| jpeg)
    }

    /// What it shows, in words.
    pub(crate) fn index(&self) -> String {
        index(&self.video, self.layout)
    }
}

/// The sheet as the largest JPEG `fits` accepts, with its size.
fn encode(
    mut sheet: RgbaImage,
    fits: &dyn Fn(&[u8]) -> bool,
) -> Result<(Vec<u8>, u32, u32), FailureCode> {
    loop {
        // The sheet is opaque: compose flattens every frame onto it.
        let rgb = DynamicImage::ImageRgba8(sheet.clone()).to_rgb8();
        for quality in QUALITIES {
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality)
                .encode_image(&rgb)
                .map_err(|_| FailureCode::RenderResource)?;
            if fits(&jpeg) {
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
}

impl OutputRefusal {
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::IsInput => "The output would replace the video itself; choose another path.",
            Self::Shared => {
                "Other users can change the output's folder; write the sheet somewhere only you can change."
            }
            Self::Unwritable => "The sheet could not be written there.",
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

/// Windows has no media worker, so no sheet is ever written there.
#[cfg(not(unix))]
fn shared(_: &Path) -> Result<bool, OutputRefusal> {
    Ok(false)
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

/// A new private file in `directory` under a hidden, unpredictable name
/// made by this call and by nothing else. Created as a
/// user-selected export, so on macOS it never inherits an ACL from the
/// folder that would let another user read the frames.
#[cfg(not(unix))]
fn create_private(directory: &Path) -> Result<(PathBuf, std::fs::File), OutputRefusal> {
    for _ in 0..16 {
        let candidate = format!(".kettle-{:016x}.tmp", unpredictable());
        let path = directory.join(candidate);
        match kettle_state::create_user_selected_file_new(&path) {
            Ok(file) => return Ok((path, file.persist())),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(OutputRefusal::Unwritable),
        }
    }
    Err(OutputRefusal::Unwritable)
}

#[cfg(unix)]
fn create_private(
    directory: &std::fs::File,
) -> Result<(std::ffi::CString, std::fs::File), OutputRefusal> {
    for _ in 0..16 {
        let candidate = format!(".kettle-{:016x}.tmp", unpredictable());
        match kettle_state::create_user_selected_file_new_at(directory, candidate.as_ref()) {
            Ok(file) => return Ok((std::ffi::CString::new(candidate).unwrap(), file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(OutputRefusal::Unwritable),
        }
    }
    Err(OutputRefusal::Unwritable)
}

/// Write `bytes` to `out` whole or not at all: a private file of this
/// call's own beside it, renamed over it, in a folder no one else can
/// change, and never over the video. Only a file this call made is ever
/// removed.
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

// Windows retains path-based publication; it cannot pin parent replacements.
#[cfg(not(unix))]
fn write_out_attested(
    out: &Path,
    input: &Path,
    _attestation: ExternalAttested,
    bytes: &[u8],
    after_check: impl FnOnce(),
) -> Result<(), OutputRefusal> {
    if let (Ok(out), Ok(input)) = (std::fs::canonicalize(out), std::fs::canonicalize(input))
        && out == input
    {
        return Err(OutputRefusal::IsInput);
    }
    let directory = out
        .parent()
        .map(|parent| {
            if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            }
        })
        .ok_or(OutputRefusal::Unwritable)?;
    out.file_name().ok_or(OutputRefusal::Unwritable)?;
    after_check();
    if shared(directory)? {
        return Err(OutputRefusal::Shared);
    }
    let (temporary, mut file) = create_private(directory)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    // Closed first: Windows refuses to rename a file while a handle without
    // delete sharing is open on it, and the private create grants none.
    drop(file);
    let written = written.and_then(|()| std::fs::rename(&temporary, out));
    if written.is_err() {
        // By name, which is safe here: no one else can change an entry in a
        // folder that is not shared.
        let _ = std::fs::remove_file(&temporary);
        return Err(OutputRefusal::Unwritable);
    }
    Ok(())
}

#[cfg(unix)]
fn write_out_attested(
    out: &Path,
    input_path: &Path,
    input: ExternalAttested,
    bytes: &[u8],
    after_check: impl FnOnce(),
) -> Result<(), OutputRefusal> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let directory = out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = out.file_name().ok_or(OutputRefusal::Unwritable)?;
    let name = CString::new(name.as_bytes()).map_err(|_| OutputRefusal::Unwritable)?;
    // Linux can pin a writable/searchable directory without read permission.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let access = libc::O_PATH;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let access = libc::O_RDONLY;
    let parent = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(access | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(directory)
        .map_err(|_| OutputRefusal::Unwritable)?;
    // SAFETY: the held parent, NUL-terminated leaf and stat output are valid.
    let mut found: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut found,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        // The stat field widths vary across Unix targets.
        #[allow(clippy::unnecessary_cast)]
        let is_input = found.st_dev as u64 == input.dev && found.st_ino as u64 == input.ino;
        if is_input {
            return Err(OutputRefusal::IsInput);
        }
        if found.st_mode & libc::S_IFMT == libc::S_IFLNK {
            return Err(OutputRefusal::Unwritable);
        }
    } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
        return Err(OutputRefusal::Unwritable);
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
    if shared_directory(&parent)? {
        return Err(OutputRefusal::Shared);
    }
    let (temporary, mut file) = create_private(&parent)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    let written = written.and_then(|()| {
        // SAFETY: both leaf names resolve within the same held directory.
        if unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                temporary.as_ptr(),
                parent.as_raw_fd(),
                name.as_ptr(),
            )
        } == 0
        {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    });
    if written.is_err() {
        // SAFETY: only this call created the unpredictable temporary entry.
        let _ = unsafe { libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0) };
        return Err(OutputRefusal::Unwritable);
    }
    Ok(())
}

/// Run `kettle video-frames …`; returns the exit code (0 written, 1 not).
pub(crate) fn run(request: FramesRequest, out: &Path) -> i32 {
    let sheet = crate::media_client()
        .ok_or(FailureCode::WorkerUnavailable)
        .and_then(|client| render_attested(&request, &client))
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
}
