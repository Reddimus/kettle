//! Apple's decoder on the shared fixtures: each frame is the one at the
//! time it reports, within the tolerance it reports; a single instant is
//! exact; a rotated track is shown upright, read as ffmpeg reads it; WebM is
//! left to the next decoder, and the chain hands it there. A session plays
//! each frame in order, timed as the frame it is, at its size and rate.
#![cfg(all(target_os = "macos", feature = "avfoundation"))]

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kettle_media::audio::{AudioBlock, AudioSource, CHANNELS, SAMPLE_RATE};
use kettle_media::video::{
    DecodedStills, PlayGate, PlayRequest, StillsPlan, VideoContainer, VideoDecoder, VideoInput,
    VideoPlayer, sniff_video_container,
};
use kettle_media::{AudioRequest, AudioStatus, FailureCode, PathIdentity, VideoCodec, VideoInfo};
use kettle_media_native::avfoundation::AvFoundation;

mod support;
use support::{frame_index, frame_index_mirrored};

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

/// A track whose display transform mirrors it shows mirrored, upright.
#[test]
fn a_mirrored_track_is_shown_mirrored() {
    let clip = Clip::fixture("mirrored.mp4");
    let (result, seen) = take(&AvFoundation, &clip, &[2000], (64, 36));
    let info = seen.unwrap();
    assert_eq!((info.width, info.height, info.rotation), (64, 36, 0));
    let frame = &result.unwrap().frames[0];
    assert_eq!(frame_index_mirrored(&frame.rgba, 64, 36), 20);
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

/// Every frame of a session opened on `name`, within `room`, from
/// `start_ms`, at most `max_fps_milli`: its size, and each frame's time and
/// index.
fn played(
    name: &str,
    room: (u32, u32),
    start_ms: u64,
    max_fps_milli: u32,
) -> ((u32, u32), Vec<(u64, u32)>) {
    let clip = Clip::fixture(name);
    let mut playback = AvFoundation
        .play(
            clip.input(),
            PlayRequest {
                max_width: room.0,
                max_height: room.1,
                start_ms,
                max_fps_milli,
                audio: AudioRequest::On,
            },
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap_or_else(|failure| panic!("{name}: {failure:?}"));
    let size = (playback.opened.width, playback.opened.height);
    assert_eq!(playback.opened.start_ms, start_ms, "{name}");
    let gate = PlayGate::default();
    gate.play();
    let mut frames = Vec::new();
    let mut rgba = vec![0; size.0 as usize * size.1 as usize * 4];
    while let Some(pts) = playback.frames.next_frame(&mut rgba, &gate).unwrap() {
        assert!(rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255));
        let index = match name {
            "rotated.mp4" => frame_index(&rgba, size.0, size.1, true),
            "mirrored.mp4" => frame_index_mirrored(&rgba, size.0, size.1),
            _ => frame_index(&rgba, size.0, size.1, false),
        };
        frames.push((pts, index));
        assert!(frames.len() <= 100, "a session that never ends");
    }
    (size, frames)
}

#[test]
fn a_session_rejects_display_transforms_it_cannot_copy() {
    for matrix in [
        [46341, 46341, 0, -46341, 46341, 0, 0, 0, 1 << 30],
        [1 << 16, 0, 0, 1 << 15, 1 << 16, 0, 0, 0, 1 << 30],
    ] {
        let mut bytes = std::fs::read(fixture("index.mp4")).unwrap();
        let track = bytes.windows(4).position(|word| word == b"tkhd").unwrap();
        assert_eq!(bytes[track + 4], 0);
        // A version 0 tkhd has 40 bytes of fields before its matrix.
        let matrix_at = track + 44;
        for (slot, value) in bytes[matrix_at..matrix_at + 36]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(matrix)
        {
            *slot = i32::to_be_bytes(value);
        }
        let clip = Clip::of(&bytes);
        let (poster, _) = take(&AvFoundation, &clip, &[2000], (32, 32));
        poster.unwrap();
        let played = AvFoundation.play(
            clip.input(),
            PlayRequest {
                max_width: 64,
                max_height: 64,
                start_ms: 0,
                max_fps_milli: 10_000,
                audio: AudioRequest::Off,
            },
            Instant::now() + Duration::from_secs(10),
        );
        assert_eq!(played.err(), Some(FailureCode::BackendUnavailable));
    }
}

/// A session plays every frame from where it starts, each timed as the
/// frame it shows, fitted to its room, upright, and at most the rate asked.
#[test]
fn a_session_plays_each_frame_in_order() {
    let all: Vec<(u64, u32)> = (0..40)
        .map(|index| (u64::from(index) * 100, index))
        .collect();
    assert_eq!(
        played("index.mp4", (64, 64), 0, 30_000),
        ((64, 36), all.clone())
    );
    assert_eq!(
        played("index.mp4", (32, 32), 2000, 30_000),
        ((32, 18), all[20..].to_vec())
    );
    assert_eq!(
        played("rotated.mp4", (64, 64), 0, 10_000),
        ((36, 64), all.clone())
    );
    // Mirrored left to right, as the stills show it too.
    assert_eq!(
        played("mirrored.mp4", (64, 64), 0, 10_000),
        ((64, 36), all.clone())
    );
    let (size, halved) = played("index.mp4", (64, 36), 0, 5_000);
    assert_eq!(size, (64, 36));
    assert_eq!(
        halved.iter().map(|(pts, _)| *pts).collect::<Vec<_>>(),
        (0..20).map(|index| index * 200).collect::<Vec<_>>()
    );
    assert!(halved.iter().all(
        |(pts, index)| u64::from(*index) * 100 <= *pts && *pts < u64::from(*index) * 100 + 200
    ));
}

/// A session asked to start past the last frame starts on it, the video's
/// last frame when the clip's sound runs six seconds longer, and at a rate
/// slower than the video's too.
#[test]
fn a_session_past_the_end_shows_the_last_frame() {
    // AVFoundation counts the sound's length a little short of ffprobe's.
    for (name, start_ms, at_least_ms, max_fps_milli) in [
        ("index.mp4", 9000, 4000, 30_000),
        ("long-audio.mp4", 9900, 9000, 30_000),
        ("index.mp4", 9000, 4000, 5_000),
        ("long-audio.mp4", 9900, 9000, 5_000),
    ] {
        let clip = Clip::fixture(name);
        let mut playback = AvFoundation
            .play(
                clip.input(),
                PlayRequest {
                    max_width: 64,
                    max_height: 36,
                    start_ms,
                    max_fps_milli,
                    audio: AudioRequest::On,
                },
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap();
        assert!(playback.opened.info.duration_ms >= at_least_ms, "{name}");
        assert_eq!(playback.opened.start_ms, 3900, "{name}");
        let gate = PlayGate::default();
        let mut rgba = vec![0; 64 * 36 * 4];
        assert_eq!(
            playback.frames.next_frame(&mut rgba, &gate),
            Ok(Some(3900)),
            "{name}"
        );
        assert_eq!(frame_index(&rgba, 64, 36, false), 39, "{name}");
        assert_eq!(
            playback.frames.next_frame(&mut rgba, &gate),
            Ok(None),
            "{name}"
        );
    }
}

/// A session on WebM is left to the next decoder.
#[test]
fn a_webm_session_is_left_to_the_next_decoder() {
    let clip = Clip::fixture("index.webm");
    let opened = AvFoundation.play(
        clip.input(),
        PlayRequest {
            max_width: 64,
            max_height: 36,
            start_ms: 0,
            max_fps_milli: 30_000,
            audio: AudioRequest::On,
        },
        Instant::now() + Duration::from_secs(10),
    );
    assert_eq!(opened.err(), Some(FailureCode::UnsupportedContainer));
}

/// Open `name` for a session from `start_ms` asking for `audio`.
fn session(
    name: &str,
    start_ms: u64,
    audio: AudioRequest,
) -> (Clip, kettle_media::video::Playback) {
    let clip = Clip::fixture(name);
    let playback = AvFoundation
        .play(
            clip.input(),
            PlayRequest {
                max_width: 64,
                max_height: 36,
                start_ms,
                max_fps_milli: 30_000,
                audio,
            },
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
    (clip, playback)
}

/// Every frame of a session's sound, from the first block's place: the
/// place, and the left channel.
fn sound_of(source: &mut dyn AudioSource) -> (u64, Vec<f32>) {
    let mut block = vec![0f32; 1024 * CHANNELS];
    let mut left = Vec::new();
    let mut first = None;
    let mut next = None;
    while let Some(AudioBlock { frames, at_frame }) = source.next_block(&mut block).unwrap() {
        // Never before the last block ended; any gap is silence.
        if let Some(next) = next {
            assert!(at_frame >= next, "{at_frame} before {next}");
            left.resize(left.len() + (at_frame - next) as usize, 0.0);
        }
        first.get_or_insert(at_frame);
        left.extend(block[..frames * CHANNELS].iter().step_by(CHANNELS));
        next = Some(at_frame + frames as u64);
        assert!(
            left.len() <= 10 * SAMPLE_RATE as usize,
            "a sound that never ends"
        );
    }
    (first.unwrap(), left)
}

/// Where each tick of `left` starts, in frames from `first`: the first
/// frame louder than a tenth after a stretch of quiet.
fn ticks(first: u64, left: &[f32]) -> Vec<u64> {
    let mut onsets = Vec::new();
    let mut quiet = usize::MAX / 2;
    for (at, sample) in left.iter().enumerate() {
        if sample.abs() > 0.1 {
            if quiet > 4_800 {
                onsets.push(first + at as u64);
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }
    onsets
}

/// The sound of `ticks.mp4`, whose 20 ms tick starts each whole second,
/// decodes to 48 kHz stereo floats placed on the video's timeline: four
/// seconds of it, each tick within 10 ms of its second and quiet between.
#[test]
fn a_session_decodes_its_sound_in_place() {
    let (_clip, mut playback) = session("ticks.mp4", 0, AudioRequest::On);
    assert_eq!(playback.opened.audio, AudioStatus::Ready);
    let (first, left) = sound_of(playback.audio.as_deref_mut().unwrap());
    assert!(first < 480, "starts at {first}");
    let length = first as usize + left.len();
    assert!(length.abs_diff(4 * 48_000) < 2_048, "{length} frames");
    let onsets = ticks(first, &left);
    assert_eq!(onsets.len(), 4, "{onsets:?}");
    for (second, onset) in onsets.iter().enumerate() {
        assert!(
            onset.abs_diff(second as u64 * 48_000) <= 480,
            "tick {second} at {onset}"
        );
    }
    // Quiet between ticks.
    let middle = &left[(24_000 - first as usize)..(40_000 - first as usize)];
    assert!(middle.iter().all(|sample| sample.abs() < 0.01));
}

/// From a second and a half in, the sound starts exactly there, and its
/// first tick is the one at two seconds.
#[test]
fn a_session_s_sound_starts_where_it_does() {
    let (_clip, mut playback) = session("ticks.mp4", 1_500, AudioRequest::Muted);
    assert_eq!(playback.opened.start_ms, 1_500);
    assert_eq!(playback.opened.audio, AudioStatus::Ready);
    let (first, left) = sound_of(playback.audio.as_deref_mut().unwrap());
    assert_eq!(first, 72_000);
    let onsets = ticks(first, &left);
    assert!(onsets[0].abs_diff(96_000) <= 480, "{onsets:?}");
}

/// No sound asked for, or none in the video: no source, and Opened says
/// why.
#[test]
fn a_session_without_sound_says_why() {
    let (_clip, playback) = session("ticks.mp4", 0, AudioRequest::Off);
    assert!(playback.audio.is_none());
    assert_eq!(playback.opened.audio, AudioStatus::Off);
    let (_clip, playback) = session("index.mp4", 0, AudioRequest::On);
    assert!(playback.audio.is_none());
    assert_eq!(playback.opened.audio, AudioStatus::NoTrack);
}
