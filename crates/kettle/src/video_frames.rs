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
    Canvas, ExternalRequest, ExternalSource, FailureCode, Job, JobKind, MAX_VIDEO_STILLS,
    MediaKind, NativePath, StillsLayout, Target, Theme, VideoCodec, VideoStills, VideoStillsResult,
    still_label,
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
        let stills = if let Some(at) = self.at_s {
            if self.start_s.is_some() || self.end_s.is_some() || self.count.is_some_and(|n| n != 1)
            {
                return Err(FailureCode::BadParams);
            }
            VideoStills {
                count: 1,
                max_edge,
                start_s: 0.0,
                end_s: None,
                at_s: Some(at),
                layout: StillsLayout::Poster,
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
                layout: if count == 1 {
                    StillsLayout::Poster
                } else {
                    StillsLayout::Sheet {
                        cols: columns(count),
                        labels: true,
                    }
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
fn source(path: &Path) -> Result<ExternalSource, FailureCode> {
    let path = std::path::absolute(path).map_err(|_| FailureCode::BadParams)?;
    let attestation = crate::show_cli::attest(&path)?;
    let path = NativePath::from_path(&path).map_err(|_| FailureCode::BadParams)?;
    Ok(ExternalSource::Path { path, attestation })
}

/// The job for `stills` of the file at `path`.
fn job(path: &Path, stills: VideoStills) -> Result<Job, FailureCode> {
    let request = ExternalRequest {
        kind: JobKind::VideoStills(stills),
        source: source(path)?,
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
    Ok(request.into())
}

/// Take the stills `request` asks for, in a worker `client` starts.
pub(crate) fn render(request: &FramesRequest, client: &WorkerClient) -> Result<Sheet, FailureCode> {
    let stills = request.stills()?;
    let job = job(&request.path, stills)?;
    let rendered = client.render(&job)?;
    rendered
        .validate_as(MediaKind::Video)
        .map_err(|_| FailureCode::RenderParse)?;
    let video = rendered.video.clone().ok_or(FailureCode::RenderParse)?;
    let image = RgbaImage::from_raw(rendered.width, rendered.height, rendered.rgba)
        .ok_or(FailureCode::RenderParse)?;
    Ok(Sheet {
        image,
        video,
        layout: stills.layout,
    })
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
        let (width, height) = (sheet.width() * 3 / 4, sheet.height() * 3 / 4);
        if width.max(height) < MIN_FIT_EDGE || width == 0 || height == 0 {
            return Err(FailureCode::TooLarge);
        }
        sheet =
            image::imageops::resize(&sheet, width, height, image::imageops::FilterType::Triangle);
    }
}

/// A codec as people name it.
fn codec_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Unknown => "an unknown codec",
        VideoCodec::H264 => "H.264",
        VideoCodec::Hevc => "HEVC",
        VideoCodec::Vp8 => "VP8",
        VideoCodec::Vp9 => "VP9",
        VideoCodec::Av1 => "AV1",
        VideoCodec::Mpeg4 => "MPEG-4",
        VideoCodec::Mpeg2 => "MPEG-2",
        VideoCodec::ProRes => "ProRes",
        VideoCodec::Mjpeg => "Motion JPEG",
        VideoCodec::QuickTimeAnimation => "QuickTime Animation",
        VideoCodec::Theora => "Theora",
        VideoCodec::Gif => "GIF",
        VideoCodec::Apng => "APNG",
        VideoCodec::WebP => "WebP",
    }
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

/// A span as people read it: `22 ms` under a second, else seconds to the
/// tenth, `1.4 s`.
fn span(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{}.{} s", ms / 1000, ms / 100 % 10)
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
    match layout {
        StillsLayout::Poster => {
            let sample = video.samples[0];
            let _ = writeln!(
                text,
                "One frame, at {}.",
                still_label(sample.actual_ms, duration)
            );
        }
        StillsLayout::Sheet { cols, .. } => {
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

/// Write `bytes` to `out` whole or not at all: a private file beside it,
/// renamed over it. The video itself is never the output.
fn write_out(out: &Path, input: &Path, bytes: &[u8]) -> Result<(), FailureCode> {
    if let (Ok(out), Ok(input)) = (std::fs::canonicalize(out), std::fs::canonicalize(input))
        && out == input
    {
        return Err(FailureCode::BadParams);
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
        .ok_or(FailureCode::BadParams)?;
    let name = out.file_name().ok_or(FailureCode::BadParams)?;
    let mut temporary = name.to_os_string();
    temporary.push(format!(".kettle-{}.tmp", std::process::id()));
    let temporary = directory.join(temporary);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options
        .open(&temporary)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&temporary, out));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return Err(FailureCode::FilePermission);
    }
    Ok(())
}

/// Run `kettle video-frames …`; returns the exit code (0 written, 1 not).
pub(crate) fn run(request: FramesRequest, out: &Path) -> i32 {
    let result = crate::media_client()
        .ok_or(FailureCode::WorkerUnavailable)
        .and_then(|client| render(&request, &client))
        .and_then(|sheet| {
            write_out(out, &request.path, &sheet.jpeg(&|_| true)?)?;
            Ok(sheet)
        });
    match result {
        Ok(sheet) => {
            println!("{}", sheet.index());
            0
        }
        Err(failure) => {
            eprintln!("kettle video-frames: {}", failure.model_message());
            if let Some(hint) = decoder_hint(failure) {
                eprintln!("kettle video-frames: {hint}");
            }
            1
        }
    }
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
            (1, Some(3.5), StillsLayout::Poster)
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
             One frame, at 00:03.0.\n\
             This is what you have seen of the video, not all of it."
        );
        assert_eq!(span(22), "22 ms");
        assert_eq!(span(1_450), "1.4 s");
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
    /// never over the video itself.
    #[test]
    fn output_is_written_whole_and_never_over_the_input() {
        let directory = tempfile::tempdir().unwrap();
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
            FailureCode::BadParams
        );
        assert_eq!(std::fs::read(&input).unwrap(), b"video");
        assert_eq!(
            write_out(
                &directory.path().join("missing/frames.jpg"),
                &input,
                b"jpeg"
            )
            .unwrap_err(),
            FailureCode::FilePermission
        );
        let leftovers: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
