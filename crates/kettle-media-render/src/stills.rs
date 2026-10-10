//! Video stills: which instants a job asks for, how large each frame is
//! drawn, and the poster or sheet they make. Providers (an animation's
//! decoder here, a video decoder in the worker) hand over each frame at the
//! size [`tile_size`] gives, with the time of the frame shown; this lays them
//! out, labels them with those times, and says what was shown.

use std::time::{Duration, Instant};

use kettle_media::video::{
    MAX_VIDEO_PREFIX_BYTES, StillsPlan, VideoContainer, VideoDecoder, VideoInput,
    sniff_video_container,
};
use kettle_media::{
    Crop, Digest, FailureCode, Job, MAX_RASTER_BYTES, MediaKind, RenderLayout, Rendered,
    StillSample, StillsLayout, VideoInfo, VideoStills, VideoStillsResult,
};

use crate::source::{self, Held};

/// How long a stills job may take from its start, inside the worker's own
/// deadline for video.
const DEADLINE: Duration = Duration::from_millis(2500);

/// What a stills job's source is, by its first bytes, never its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StillsSource {
    /// A GIF, PNG or WebP: frames the image decoders give.
    Animation,
    /// A video container: frames a video decoder gives.
    Video(VideoContainer),
    Unsupported,
}

/// Classify a held source by its first bytes.
pub fn classify(held: &Held<'_>) -> StillsSource {
    let prefix = held.prefix();
    match image::guess_format(prefix) {
        Ok(image::ImageFormat::Gif | image::ImageFormat::Png | image::ImageFormat::WebP) => {
            StillsSource::Animation
        }
        _ => sniff_video_container(prefix, held.size())
            .map_or(StillsSource::Unsupported, StillsSource::Video),
    }
}

/// Render a stills job here: an animation is decoded and laid out; a video
/// needs a decoder this renderer does not have, which the worker supplies,
/// given the held file to read and the frames to take.
pub(crate) fn render(
    job: &Job,
    stills: &VideoStills,
    on_kind: &mut impl FnMut(MediaKind),
    decoder: Option<&dyn VideoDecoder>,
) -> Result<Rendered, FailureCode> {
    let deadline = Instant::now() + DEADLINE;
    let held = source::hold(&job.source, MAX_VIDEO_PREFIX_BYTES)?;
    match classify(&held) {
        StillsSource::Animation => {
            on_kind(MediaKind::Video);
            let snapshot = held.snapshot(MAX_RASTER_BYTES)?;
            let (info, frames, tolerance) =
                crate::animation::frames(&snapshot.bytes, stills, deadline)?;
            let digest = kettle_media::content_digest(&snapshot.bytes, snapshot.identity)
                .map_err(|_| FailureCode::BadParams)?;
            compose(stills, info, frames, tolerance, digest)
        }
        StillsSource::Video(container) => {
            // A decoder reads a file; inline bytes would have to be written
            // somewhere first, and the worker writes nothing.
            let Held::File {
                file,
                identity,
                prefix,
            } = &held
            else {
                return Err(FailureCode::UnsupportedMedia);
            };
            let decoder = decoder.ok_or(FailureCode::BackendUnavailable)?;
            on_kind(MediaKind::Video);
            let input = VideoInput {
                file,
                identity: *identity,
                container,
            };
            let mut planned = None;
            let decoded = decoder.stills(
                input,
                &mut |info| {
                    let times_ms = sample_times(info.duration_ms, stills);
                    let (width, height) = tile_size(info.width, info.height, stills)
                        .ok_or(FailureCode::RenderResource)?;
                    let plan = StillsPlan {
                        times_ms,
                        width,
                        height,
                    };
                    planned = Some(plan.clone());
                    Ok(plan)
                },
                deadline,
            )?;
            held.unchanged()?;
            let plan = planned.ok_or(FailureCode::RenderParse)?;
            if decoded.frames.len() != plan.times_ms.len()
                || decoded
                    .frames
                    .iter()
                    .zip(&plan.times_ms)
                    .any(|(frame, &asked)| frame.sample.requested_ms != asked)
            {
                return Err(FailureCode::RenderParse);
            }
            let frames = decoded
                .frames
                .into_iter()
                .map(|frame| Still {
                    sample: frame.sample,
                    width: plan.width,
                    height: plan.height,
                    rgba: frame.rgba,
                })
                .collect();
            // A video is too large to hash whole: its first bytes and the
            // held file's identity stand for it, and the file is checked
            // unchanged after decoding.
            let digest = kettle_media::content_digest(prefix, Some(*identity))
                .map_err(|_| FailureCode::BadParams)?;
            compose(stills, decoded.info, frames, decoded.tolerance_ms, digest)
        }
        StillsSource::Unsupported => Err(FailureCode::UnsupportedMedia),
    }
}

/// Space between frames and around them on a sheet, in pixels.
const GAP: u32 = 4;
/// A sheet's background: dark, so a frame's edge shows against it.
const BACKGROUND: [u8; 4] = [24, 24, 24, 255];

/// The instants `stills` asks for in a video `duration_ms` long, in the order
/// they are laid out: the one at `at_s`, or `count` evenly through the window
/// from `start_s` to `end_s` (the end without one), each the middle of its
/// share. Past the end is the end.
pub fn sample_times(duration_ms: u64, stills: &VideoStills) -> Vec<u64> {
    let ms = |seconds: f64| {
        let ms = (seconds * 1000.0).round();
        if ms >= duration_ms as f64 {
            duration_ms
        } else {
            ms.max(0.0) as u64
        }
    };
    if let Some(at) = stills.at_s {
        return vec![ms(at)];
    }
    let start = ms(stills.start_s);
    let end = stills.end_s.map_or(duration_ms, ms).max(start);
    let count = u64::from(stills.count.max(1));
    let span = end - start;
    (0..count)
        .map(|index| start + (span * (2 * index + 1)) / (2 * count))
        .collect()
}

/// The layout's grid, columns by rows.
pub fn grid(stills: &VideoStills) -> (u32, u32) {
    match stills.layout {
        StillsLayout::Poster => (1, 1),
        StillsLayout::Sheet { cols, .. } => {
            let cols = u32::from(cols.max(1));
            (cols, u32::from(stills.count).div_ceil(cols))
        }
    }
}

/// The size each frame of a picture `width` by `height` (as shown) is drawn
/// at: as large as the grid and its gaps allow within `max_edge` on a side,
/// never larger than the picture itself, its shape kept. `None` for an empty
/// picture.
pub fn tile_size(width: u32, height: u32, stills: &VideoStills) -> Option<(u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }
    let (cols, rows) = grid(stills);
    let gap = gap(stills);
    let room = |count: u32| {
        stills
            .max_edge
            .checked_sub(gap * (count + 1))
            .map(|left| left / count)
            .filter(|&room| room > 0)
    };
    let (room_w, room_h) = (room(cols)?, room(rows)?);
    let scale = (f64::from(room_w) / f64::from(width))
        .min(f64::from(room_h) / f64::from(height))
        .min(1.0);
    let tile = |side: u32| ((f64::from(side) * scale).floor() as u32).max(1);
    Some((tile(width), tile(height)))
}

fn gap(stills: &VideoStills) -> u32 {
    match stills.layout {
        StillsLayout::Poster => 0,
        StillsLayout::Sheet { .. } => GAP,
    }
}

/// One frame taken: when it was asked for and when the frame shown is, and
/// its straight RGBA at the tile size.
#[derive(Clone, Debug, PartialEq)]
pub struct Still {
    pub sample: StillSample,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A time as a label: `mm:ss`, `h:mm:ss` from an hour, with tenths for a
/// video shorter than a minute, where whole seconds would repeat.
pub fn label(ms: u64, duration_ms: u64) -> String {
    let seconds = ms / 1000;
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    let base = if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    };
    if duration_ms < 60_000 {
        format!("{base}.{}", ms / 100 % 10)
    } else {
        base
    }
}

/// The poster or sheet `stills` lays out from `frames`, in their order, with
/// what the video is and how far a frame may be from its time: a Rendered
/// whose digest is the source's. Frames must be the tile size; labels, when
/// the sheet has them, show each frame's own time.
pub fn compose(
    stills: &VideoStills,
    info: VideoInfo,
    frames: Vec<Still>,
    tolerance_ms: u32,
    digest: Digest,
) -> Result<Rendered, FailureCode> {
    let (tile_w, tile_h) =
        tile_size(info.width, info.height, stills).ok_or(FailureCode::RenderResource)?;
    if frames.is_empty()
        || frames.len() > usize::from(stills.count)
        || frames.iter().any(|frame| {
            (frame.width, frame.height) != (tile_w, tile_h)
                || frame.rgba.len() != tile_w as usize * tile_h as usize * 4
        })
    {
        return Err(FailureCode::RenderResource);
    }
    let (cols, rows) = grid(stills);
    let gap = gap(stills);
    let width = cols * tile_w + gap * (cols + 1);
    let height = rows * tile_h + gap * (rows + 1);
    let mut rgba = BACKGROUND.repeat(width as usize * height as usize);
    let labels = matches!(stills.layout, StillsLayout::Sheet { labels: true, .. });
    let label_size = (tile_h / 12).clamp(10, 28);
    for (index, frame) in frames.iter().enumerate() {
        let index = index as u32;
        let x = gap + (index % cols) * (tile_w + gap);
        let y = gap + (index / cols) * (tile_h + gap);
        // A frame's transparency shows the sheet's background, so the sheet
        // stays opaque.
        for row in 0..tile_h {
            for column in 0..tile_w {
                let src = ((row * tile_w + column) * 4) as usize;
                let dst = (((y + row) * width + x + column) * 4) as usize;
                let alpha = u16::from(frame.rgba[src + 3]);
                for channel in 0..3 {
                    rgba[dst + channel] = ((u16::from(frame.rgba[src + channel]) * alpha
                        + u16::from(BACKGROUND[channel]) * (255 - alpha)
                        + 127)
                        / 255) as u8;
                }
            }
        }
        if labels {
            let text = label(frame.sample.actual_ms, info.duration_ms);
            let (lw, lh, premultiplied) = crate::svg::render_label(&text, label_size)?;
            // Bottom left of the frame, inset; a label wider than the frame
            // is cut to it.
            let inset = (label_size / 3).max(2);
            let (lx, ly) = (x + inset, (y + tile_h).saturating_sub(lh + inset).max(y));
            for row in 0..lh.min(tile_h) {
                for column in 0..lw.min(tile_w.saturating_sub(inset)) {
                    let src = ((row * lw + column) * 4) as usize;
                    let dst = (((ly + row) * width + lx + column) * 4) as usize;
                    let alpha = u16::from(premultiplied[src + 3]);
                    for channel in 0..3 {
                        let under = u16::from(rgba[dst + channel]);
                        rgba[dst + channel] = (u16::from(premultiplied[src + channel])
                            + (under * (255 - alpha) + 127) / 255)
                            .min(255) as u8;
                    }
                }
            }
        }
    }
    let whole = Crop {
        x: 0,
        y: 0,
        width,
        height,
    };
    let rendered = Rendered {
        width,
        height,
        rgba,
        digest,
        layout: RenderLayout {
            source_width: f64::from(width),
            source_height: f64::from(height),
            image_in_target: whole,
            result_in_target: whole,
        },
        exact_source: None,
        source_text: Vec::new(),
        fence_sources: Vec::new(),
        fence_count: 0,
        fence_index: None,
        uncovered_scripts: Vec::new(),
        warnings: Vec::new(),
        video: Some(VideoStillsResult {
            info,
            samples: frames.iter().map(|frame| frame.sample).collect(),
            tolerance_ms,
        }),
    };
    rendered
        .validate_as(kettle_media::MediaKind::Video)
        .map_err(|_| FailureCode::RenderResource)?;
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stills(count: u8, layout: StillsLayout) -> VideoStills {
        VideoStills {
            count,
            max_edge: 1568,
            start_s: 0.0,
            end_s: None,
            at_s: None,
            layout,
        }
    }

    fn sheet(count: u8, cols: u8, labels: bool) -> VideoStills {
        stills(count, StillsLayout::Sheet { cols, labels })
    }

    /// Frames fall in the middle of their equal shares of the window, the
    /// window narrowing to `start_s`..`end_s`; `at_s` is one instant; past
    /// the end is the end, and a still image is all at 0.
    #[test]
    fn instants_are_the_middles_of_equal_shares() {
        assert_eq!(
            sample_times(1000, &sheet(4, 2, false)),
            [125, 375, 625, 875]
        );
        let window = VideoStills {
            start_s: 2.0,
            end_s: Some(4.0),
            ..sheet(2, 2, false)
        };
        assert_eq!(sample_times(10_000, &window), [2500, 3500]);
        let past = VideoStills {
            start_s: 9.0,
            end_s: Some(20.0),
            ..sheet(2, 2, false)
        };
        assert_eq!(sample_times(10_000, &past), [9250, 9750]);
        let at = VideoStills {
            at_s: Some(3.25),
            ..stills(1, StillsLayout::Poster)
        };
        assert_eq!(sample_times(10_000, &at), [3250]);
        let after = VideoStills {
            at_s: Some(30.0),
            ..stills(1, StillsLayout::Poster)
        };
        assert_eq!(sample_times(10_000, &after), [10_000]);
        assert_eq!(sample_times(0, &sheet(3, 3, false)), [0, 0, 0]);
    }

    /// A tile is as large as the grid and gaps allow within the edge, its
    /// shape kept, never larger than the picture; a poster has no gaps.
    #[test]
    fn tiles_fill_the_edge_without_growing_a_picture() {
        assert_eq!(grid(&sheet(9, 3, false)), (3, 3));
        assert_eq!(grid(&sheet(10, 4, false)), (4, 3));
        assert_eq!(grid(&stills(1, StillsLayout::Poster)), (1, 1));
        // 1568 less four 4 px gaps, over three: 517 wide at most.
        assert_eq!(tile_size(1920, 1080, &sheet(9, 3, false)), Some((517, 290)));
        assert_eq!(
            tile_size(1920, 1080, &stills(1, StillsLayout::Poster)),
            Some((1568, 882))
        );
        assert_eq!(tile_size(160, 90, &sheet(9, 3, false)), Some((160, 90)));
        assert_eq!(tile_size(0, 90, &sheet(9, 3, false)), None);
        let tiny = VideoStills {
            max_edge: 8,
            ..sheet(4, 2, false)
        };
        assert_eq!(tile_size(160, 90, &tiny), None, "no room past the gaps");
    }

    #[test]
    fn labels_say_the_time_shown() {
        assert_eq!(label(0, 2_000), "00:00.0");
        assert_eq!(label(1_550, 2_000), "00:01.5");
        assert_eq!(label(13_000, 120_000), "00:13");
        assert_eq!(label(3_725_000, 4_000_000), "1:02:05");
    }

    fn info(width: u32, height: u32) -> VideoInfo {
        VideoInfo {
            duration_ms: 1000,
            width,
            height,
            rotation: 0,
            codec: kettle_media::VideoCodec::Gif,
            fps_milli: None,
            has_audio: false,
            container: None,
        }
    }

    fn solid(color: [u8; 4], width: u32, height: u32, at: u64) -> Still {
        Still {
            sample: StillSample {
                requested_ms: at,
                actual_ms: at,
            },
            width,
            height,
            rgba: color.repeat((width * height) as usize),
        }
    }

    fn digest() -> Digest {
        kettle_media::content_digest(b"frames", None).unwrap()
    }

    /// Frames land in reading order with the gap around each, transparent
    /// pixels showing the background, the result an opaque video reply with
    /// each frame's times; labels mark each frame's lower left, and only
    /// when asked for. Frames off the tile size, none or too many are
    /// refused.
    #[test]
    fn a_sheet_lays_frames_out_with_their_times() {
        let layout = sheet(3, 2, false);
        let (tw, th) = tile_size(40, 30, &layout).unwrap();
        assert_eq!((tw, th), (40, 30));
        let frames = vec![
            solid([255, 0, 0, 255], tw, th, 100),
            solid([0, 255, 0, 255], tw, th, 400),
            solid([0, 0, 255, 0], tw, th, 700),
        ];
        let rendered = compose(&layout, info(40, 30), frames.clone(), 50, digest()).unwrap();
        assert_eq!(
            (rendered.width, rendered.height),
            (2 * 40 + 12, 2 * 30 + 12)
        );
        let at = |x: u32, y: u32| {
            let i = ((y * rendered.width + x) * 4) as usize;
            [
                rendered.rgba[i],
                rendered.rgba[i + 1],
                rendered.rgba[i + 2],
                rendered.rgba[i + 3],
            ]
        };
        assert_eq!(at(0, 0), BACKGROUND, "the gap");
        assert_eq!(at(GAP + 1, GAP + 1), [255, 0, 0, 255]);
        assert_eq!(at(2 * GAP + 40 + 1, GAP + 1), [0, 255, 0, 255]);
        assert_eq!(
            at(GAP + 1, 2 * GAP + 30 + 1),
            BACKGROUND,
            "transparent shows it"
        );
        assert_eq!(
            at(2 * GAP + 40 + 1, 2 * GAP + 30 + 1),
            BACKGROUND,
            "no fourth frame"
        );
        assert!(
            rendered
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 255)
        );
        // Where a label's box would be, inset from the lower left.
        assert_eq!(
            at(GAP + 5, GAP + th - 6),
            [255, 0, 0, 255],
            "no label unasked"
        );
        let video = rendered.video.as_ref().unwrap();
        assert_eq!(
            video
                .samples
                .iter()
                .map(|sample| sample.actual_ms)
                .collect::<Vec<_>>(),
            [100, 400, 700]
        );
        assert_eq!(video.tolerance_ms, 50);
        rendered.validate_as(MediaKind::Video).unwrap();

        let labeled = compose(
            &sheet(3, 2, true),
            info(40, 30),
            frames.clone(),
            50,
            digest(),
        )
        .unwrap();
        let red_tile_changed = (0..th)
            .flat_map(|y| (0..tw).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let i = (((GAP + y) * labeled.width + GAP + x) * 4) as usize;
                labeled.rgba[i..i + 4] != [255, 0, 0, 255]
            })
            .count();
        assert!(
            red_tile_changed > 20,
            "a label is drawn: {red_tile_changed}"
        );
        assert_eq!(
            labeled.rgba[((GAP * labeled.width + GAP + tw - 1) * 4) as usize..][..4],
            [255, 0, 0, 255],
            "the upper right stays the frame"
        );

        let off = vec![solid([1, 2, 3, 255], tw - 1, th, 0)];
        assert_eq!(
            compose(&layout, info(40, 30), off, 0, digest()).unwrap_err(),
            FailureCode::RenderResource
        );
        assert_eq!(
            compose(&layout, info(40, 30), Vec::new(), 0, digest()).unwrap_err(),
            FailureCode::RenderResource
        );
        let mut four = frames;
        four.push(solid([9, 9, 9, 255], tw, th, 900));
        assert_eq!(
            compose(&layout, info(40, 30), four, 0, digest()).unwrap_err(),
            FailureCode::RenderResource
        );
    }

    /// A stills job renders an animation through the crate's entry point as
    /// video; inline video bytes, which no decoder reads, and other bytes are
    /// unsupported.
    #[test]
    fn a_stills_job_renders_animations_and_leaves_video_to_the_worker() {
        let job = |bytes: Vec<u8>| Job {
            kind: kettle_media::JobKind::VideoStills(sheet(2, 2, true)),
            source: kettle_media::Source::Bytes(bytes),
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
            fallback_fonts: Vec::new(),
        };
        let webp = include_bytes!("../tests/fixtures/stills/anim.webp").to_vec();
        let mut kinds = Vec::new();
        let (kind, rendered) =
            crate::render_with_kind(&job(webp), |kind| kinds.push(kind)).unwrap();
        assert_eq!((kind, kinds), (MediaKind::Video, vec![MediaKind::Video]));
        rendered.validate_as(MediaKind::Video).unwrap();
        let mut mp4 = vec![0, 0, 0, 24];
        mp4.extend_from_slice(b"ftypisom\0\0\x02\0isomiso2");
        assert_eq!(
            crate::render(&job(mp4)).unwrap_err(),
            FailureCode::UnsupportedMedia
        );
        assert_eq!(
            crate::render(&job(b"plain text".to_vec())).unwrap_err(),
            FailureCode::UnsupportedMedia
        );
    }

    /// What a stand-in decoder does once it has the plan.
    #[derive(Clone, Copy, PartialEq)]
    enum Misstep {
        None,
        OneFrameShort,
        OtherTime,
        RewritesTheFile,
    }

    /// A decoder in process: it describes a 40x30, one-second video, takes
    /// the plan, and answers solid frames at each instant, unless told to
    /// misstep.
    struct StandIn {
        misstep: Misstep,
        path: std::path::PathBuf,
    }

    impl VideoDecoder for StandIn {
        fn stills(
            &self,
            input: VideoInput<'_>,
            plan: &mut dyn FnMut(&VideoInfo) -> Result<StillsPlan, FailureCode>,
            _deadline: Instant,
        ) -> Result<kettle_media::video::DecodedStills, FailureCode> {
            assert_eq!(input.container, VideoContainer::IsoBmff);
            let info = VideoInfo {
                container: Some(VideoContainer::IsoBmff),
                ..info(40, 30)
            };
            let plan = plan(&info)?;
            let mut frames: Vec<_> = plan
                .times_ms
                .iter()
                .map(|&at| kettle_media::video::DecodedFrame {
                    sample: StillSample {
                        requested_ms: at + u64::from(self.misstep == Misstep::OtherTime),
                        actual_ms: at,
                    },
                    rgba: [9, 99, 199, 255].repeat((plan.width * plan.height) as usize),
                })
                .collect();
            if self.misstep == Misstep::OneFrameShort {
                frames.pop();
            }
            if self.misstep == Misstep::RewritesTheFile {
                std::thread::sleep(Duration::from_millis(5));
                std::fs::write(&self.path, b"rewritten while decoding").unwrap();
            }
            Ok(kettle_media::video::DecodedStills {
                info,
                frames,
                tolerance_ms: 0,
            })
        }
    }

    /// A video's stills come from the decoder the worker passes in, laid out
    /// with the decoder's times and the held file's identity as the digest;
    /// a decoder answering the wrong frames, or a file changed while it was
    /// decoded, is refused, and inline video bytes are unsupported.
    #[test]
    fn a_video_is_decoded_by_the_decoder_given() {
        use std::os::unix::ffi::OsStrExt as _;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clip.mp4");
        let mut mp4 = vec![0, 0, 0, 24];
        mp4.extend_from_slice(b"ftypisom\0\0\x02\0isomiso2");
        let job = |source: kettle_media::Source| Job {
            kind: kettle_media::JobKind::VideoStills(sheet(4, 2, false)),
            source,
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
            fallback_fonts: Vec::new(),
        };
        let file_job = || {
            job(kettle_media::Source::user_pull(
                kettle_media::NativePath::new(path.as_os_str().as_bytes().to_vec()).unwrap(),
                kettle_media::GuiActionWitness::from_explicit_gui_action(),
            ))
        };
        let render = |misstep: Misstep| {
            std::fs::write(&path, &mp4).unwrap();
            let decoder = StandIn {
                misstep,
                path: path.clone(),
            };
            crate::render_with_decoder(&file_job(), |_| {}, Some(&decoder))
        };

        let (kind, rendered) = render(Misstep::None).unwrap();
        assert_eq!(kind, MediaKind::Video);
        let video = rendered.video.as_ref().unwrap();
        assert_eq!(
            video
                .samples
                .iter()
                .map(|sample| sample.actual_ms)
                .collect::<Vec<_>>(),
            [125, 375, 625, 875]
        );
        assert_eq!(
            rendered.digest,
            kettle_media::content_digest(
                &mp4,
                kettle_media::PathIdentity::of(&std::fs::metadata(&path).unwrap())
            )
            .unwrap()
        );
        let at = |x: u32, y: u32| {
            let i = ((y * rendered.width + x) * 4) as usize;
            rendered.rgba[i..i + 4].to_vec()
        };
        assert_eq!(at(GAP + 1, GAP + 1), [9, 99, 199, 255]);

        for misstep in [Misstep::OneFrameShort, Misstep::OtherTime] {
            assert_eq!(render(misstep).unwrap_err(), FailureCode::RenderParse);
        }
        assert_eq!(
            render(Misstep::RewritesTheFile).unwrap_err(),
            FailureCode::Changed
        );
        let decoder = StandIn {
            misstep: Misstep::None,
            path: path.clone(),
        };
        assert_eq!(
            crate::render_with_decoder(
                &job(kettle_media::Source::Bytes(mp4.clone())),
                |_| {},
                Some(&decoder)
            )
            .unwrap_err(),
            FailureCode::UnsupportedMedia
        );
        std::fs::write(&path, &mp4).unwrap();
        assert_eq!(
            crate::render(&file_job()).unwrap_err(),
            FailureCode::BackendUnavailable,
            "no decoder given"
        );
    }
}
