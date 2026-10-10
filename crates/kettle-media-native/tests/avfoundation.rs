//! Apple's decoder on the shared fixtures: each frame is the one at the
//! time it reports, within the tolerance it reports; a single instant is
//! exact; a rotated track is shown upright, read as ffmpeg reads it; WebM is
//! left to the next decoder, and the chain hands it there.
#![cfg(all(target_os = "macos", feature = "avfoundation"))]

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kettle_media::video::{
    DecodedStills, StillsPlan, VideoContainer, VideoDecoder, VideoInput, sniff_video_container,
};
use kettle_media::{FailureCode, PathIdentity, VideoCodec, VideoInfo};
use kettle_media_native::avfoundation::AvFoundation;

mod support;
use support::frame_index;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/video")
        .join(name)
}

/// A held copy of a fixture, or of other bytes.
struct Clip {
    _directory: tempfile::TempDir,
    file: File,
    identity: PathIdentity,
    container: VideoContainer,
}

impl Clip {
    fn of(bytes: &[u8]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clip");
        std::fs::write(&path, bytes).unwrap();
        let file = File::open(&path).unwrap();
        let identity = PathIdentity::of(&file.metadata().unwrap()).unwrap();
        let container = sniff_video_container(bytes, bytes.len() as u64).unwrap();
        Self {
            _directory: directory,
            file,
            identity,
            container,
        }
    }

    fn fixture(name: &str) -> Self {
        Self::of(&std::fs::read(fixture(name)).unwrap())
    }

    fn input(&self) -> VideoInput<'_> {
        VideoInput {
            file: &self.file,
            identity: self.identity,
            container: self.container,
        }
    }
}

fn take(
    decoder: &dyn VideoDecoder,
    clip: &Clip,
    times: &[u64],
    size: (u32, u32),
) -> (Result<DecodedStills, FailureCode>, Option<VideoInfo>) {
    let mut seen = None;
    let result = decoder.stills(
        clip.input(),
        &mut |info| {
            seen = Some(*info);
            Ok(StillsPlan {
                times_ms: times.to_vec(),
                width: size.0,
                height: size.1,
            })
        },
        Instant::now() + Duration::from_secs(20),
    );
    (result, seen)
}

/// Each frame shows the fixture frame that starts at the time reported,
/// each time within the tolerance reported of the instant asked, and the
/// video is described as it is.
#[test]
fn each_frame_is_the_one_at_the_time_it_reports() {
    let clip = Clip::fixture("index.mp4");
    let (result, seen) = take(&AvFoundation, &clip, &[666, 2000, 3333, 3999], (32, 18));
    let stills = result.unwrap();
    let info = seen.unwrap();
    assert_eq!(
        (info.duration_ms, info.width, info.height, info.rotation),
        (4000, 64, 36, 0)
    );
    assert_eq!(
        (info.codec, info.fps_milli, info.has_audio, info.container),
        (
            VideoCodec::H264,
            Some(10_000),
            false,
            Some(VideoContainer::IsoBmff)
        )
    );
    for frame in &stills.frames {
        let sample = frame.sample;
        assert_eq!(
            frame_index(&frame.rgba, 32, 18, false),
            (sample.actual_ms / 100) as u32,
            "{sample:?}"
        );
        assert_eq!(sample.actual_ms % 100, 0, "a frame's own start: {sample:?}");
        assert!(
            sample.requested_ms.abs_diff(sample.actual_ms) <= u64::from(stills.tolerance_ms),
            "{sample:?} within {}",
            stills.tolerance_ms
        );
        assert_eq!(frame.rgba.len(), 32 * 18 * 4);
        assert!(
            frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 255)
        );
    }
    assert!(
        stills.tolerance_ms <= 166,
        "a quarter of the closest gap at most"
    );
}

/// Instants whose shares of the window meet at keyframes each take a frame
/// inside their own share: `segments.mp4` has keyframes only at even
/// seconds, so the middle of an odd one-second segment has a keyframe half a
/// segment after it, starting the next segment, and none before; each
/// middle still takes its own segment.
#[test]
fn each_frame_stays_inside_its_share() {
    let clip = Clip::fixture("segments.mp4");
    let times: Vec<u64> = (0..9).map(|segment| segment * 1000 + 500).collect();
    let (result, _) = take(&AvFoundation, &clip, &times, (16, 9));
    let stills = result.unwrap();
    for (segment, frame) in stills.frames.iter().enumerate() {
        let share = segment as u64 * 1000..(segment as u64 + 1) * 1000;
        assert!(
            share.contains(&frame.sample.actual_ms),
            "{:?} outside {share:?}",
            frame.sample
        );
        assert_eq!(frame_index(&frame.rgba, 16, 9, false), segment as u32);
    }
}

/// A single instant is taken exactly: the frame showing then.
#[test]
fn a_single_instant_is_exact() {
    let clip = Clip::fixture("index.mp4");
    for (asked, shown) in [(666, 6), (2050, 20), (3999, 39), (0, 0)] {
        let (result, _) = take(&AvFoundation, &clip, &[asked], (16, 9));
        let stills = result.unwrap();
        let frame = &stills.frames[0];
        assert_eq!(frame_index(&frame.rgba, 16, 9, false), shown, "at {asked}");
        assert_eq!(frame.sample.actual_ms, u64::from(shown) * 100, "at {asked}");
    }
}

/// A rotated track is shown upright, its rotation and size read as ffmpeg
/// reads them.
#[test]
fn a_rotated_track_is_shown_upright() {
    let clip = Clip::fixture("rotated.mp4");
    let (result, seen) = take(&AvFoundation, &clip, &[2000], (18, 32));
    let info = seen.unwrap();
    assert_eq!((info.width, info.height, info.rotation), (36, 64, 270));
    let frame = &result.unwrap().frames[0];
    assert_eq!(frame_index(&frame.rgba, 18, 32, true), 20);
}

/// WebM is not AVFoundation's: it is left to the next decoder, which the
/// chain hands it to; with none, the chain says it has no decoder for it.
#[test]
fn webm_is_left_to_the_next_decoder() {
    let clip = Clip::fixture("index.webm");
    let (result, seen) = take(&AvFoundation, &clip, &[2000], (16, 9));
    assert_eq!(result.unwrap_err(), FailureCode::UnsupportedContainer);
    assert_eq!(seen, None);
    let chain = kettle_media_native::Decoders::external(None);
    let (result, _) = take(&chain, &clip, &[2000], (16, 9));
    assert_eq!(result.unwrap_err(), FailureCode::BackendUnavailable);
}

/// A clip cut short is the stream's fault, not a crash or a hang.
#[test]
fn a_truncated_clip_is_refused() {
    let bytes = std::fs::read(fixture("index.mp4")).unwrap();
    let clip = Clip::of(&bytes[..bytes.len() / 2]);
    let (result, _) = take(&AvFoundation, &clip, &[3500], (16, 9));
    assert!(
        matches!(
            result,
            Err(FailureCode::RenderParse
                | FailureCode::UnsupportedContainer
                | FailureCode::UnsupportedMedia
                | FailureCode::CodecUnavailable)
        ),
        "{result:?}"
    );
}
