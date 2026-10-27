//! The external decoder: the user's own ffmpeg and ffprobe, never bundled
//! and never linked. Each run is a child of the worker, contained so it
//! starts no process, with a fixed argument list into which only numbers
//! Kettle computed and the demuxer the content was sniffed as are put; its
//! input is a fresh descriptor of the held file (Linux reads it through
//! `/proc/self/fd/0`, macOS through `/dev/fd/0`), only the `file` protocol is
//! allowed, subtitles and data are dropped, and stderr is discarded.
//!
//! A stills job takes three steps. ffprobe describes the streams, and only
//! the fields asked for are read from its output, each bounded. ffprobe then
//! lists the video packets around each instant (no decoding), which gives
//! the frame actually showing at it: the last one starting at or before it.
//! Then ffmpeg decodes each such frame by its exact timestamp: an accurate
//! seek returns the first frame starting at or after the time asked, so
//! asking for that frame's own start returns it, not the next. A frame is
//! exactly the plan's width times height in RGBA, no byte short or over.
//! When the deadline nears, the remaining instants take the keyframe at or
//! before the frame instead, which decodes at once, and say so in their
//! times. A stream without timestamps is sought by time, each frame then
//! standing for its instant within one frame's length.
//!
//! Playback uses bounded raw-video and, when requested, PCM pipes. A
//! metadata-first framecrc tee preserves their common timestamps; concurrent
//! readers prevent per-slave interleaving from blocking large raw packets.
//! Audio is resampled before fixed 20 ms packets are formed. The decoder
//! never connects to an audio service.

mod packets;
mod playback;

use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

use kettle_media::video::{
    DECODER_ENV, DecodedFrame, DecodedStill, DecodedStills, FrameSource, PlayGate, PlayRequest,
    Playback, StillImage, StillInput, StillsPlan, VideoContainer, VideoDecoder, VideoInput,
    VideoPlayer, still_fit, still_size_drawn,
};
use kettle_media::{
    AudioStatus, FailureCode, MAX_VIDEO_FPS_MILLI, MAX_VIDEO_SIDE, Opened, StillSample, VideoCodec,
    VideoInfo, play_size,
};

use crate::reopen::reopen;
use crate::run::{Exit, Piece, Ran, RunError, Streaming, run, stream};
use crate::tools::{Refusal, Tool};

/// The held file, as each decoder run reads it: its stdin, reopened by path
/// inside the child.
#[cfg(target_os = "linux")]
const INPUT: &str = "file:/proc/self/fd/0";
#[cfg(target_os = "macos")]
const INPUT: &str = "file:/dev/fd/0";

/// Most bytes of a stream description read from ffprobe.
const MAX_PROBE_BYTES: usize = 64 * 1024;
/// Most streams a description may list.
const MAX_STREAMS: usize = 64;
/// Most bytes of a packet list read from ffprobe around the instants, and
/// for a whole stream when its duration is unknown.
const MAX_WINDOW_BYTES: usize = 2 << 20;
const MAX_SCAN_BYTES: usize = 6 << 20;
/// Most packets a list may hold.
const MAX_PACKETS: usize = 400_000;
/// How far past an instant its packets are read, to take in frames stored
/// out of order: a second, or 32 frames when that is longer, past the 16
/// frames H.264 and HEVC may reorder. ffprobe stops at the first packet past
/// the window, which a reordered stream stores ahead of earlier frames.
const WINDOW_US: i64 = 1_000_000;
const WINDOW_FRAMES: u64 = 32;
/// Most bytes of ffmpeg's codec list, read only to explain a failure.
const MAX_CODECS_BYTES: usize = 1 << 20;
/// Decoder threads per run. More multiplies memory under the worker's
/// address-space limit.
const THREADS: &str = "2";
/// Processor time a session's ffmpeg may use: half a minute, and four
/// seconds for each second of video it has left, room for its decoder and
/// filter threads at full speed the whole way. A run that spins without
/// writing goes when it is spent, whether or not anyone still reads it.
const PLAY_CPU_SECONDS: u64 = 30;
const PLAY_CPU_PER_SECOND: u64 = 4;
/// How long the explanation of a session that gave no frame may take.
const EXPLAIN_WITHIN: Duration = Duration::from_secs(2);

/// ffmpeg and ffprobe, trusted, from one directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ffmpeg {
    ffmpeg: Tool,
    ffprobe: Tool,
}

impl Ffmpeg {
    /// The decoder the parent named when it started this worker, trusted
    /// now; `None` when it named none.
    pub fn configured() -> Option<Result<Self, Refusal>> {
        let named = std::env::var_os(DECODER_ENV)?;
        Some(Self::trust(Path::new(&named)))
    }

    /// The ffmpeg at `path` and the ffprobe beside it, each trusted.
    pub fn trust(path: &Path) -> Result<Self, Refusal> {
        let ffmpeg = crate::tools::trust(path)?;
        let ffprobe = ffmpeg.sibling("ffprobe")?;
        Ok(Self { ffmpeg, ffprobe })
    }

    /// The first trusted ffmpeg with an ffprobe beside it in the fixed
    /// places, then `home`'s Nix profile.
    pub fn search(home: Option<&Path>) -> Option<Self> {
        crate::tools::SEARCH_DIRS
            .iter()
            .map(std::path::PathBuf::from)
            .chain(
                home.filter(|home| home.is_absolute())
                    .map(|home| home.join(".nix-profile/bin")),
            )
            .find_map(|directory| Self::trust(&directory.join("ffmpeg")).ok())
    }

    /// The ffmpeg that is run, every link resolved.
    pub fn path(&self) -> &Path {
        self.ffmpeg.path()
    }

    /// Both programs that are run, every link resolved: ffmpeg and ffprobe.
    pub fn programs(&self) -> [&Path; 2] {
        [self.ffmpeg.path(), self.ffprobe.path()]
    }

    /// The path ffmpeg was found by, whose directory holds the ffprobe beside
    /// it: what a parent names to the worker, so the worker finds the same
    /// pair.
    pub fn named(&self) -> &Path {
        self.ffmpeg.named()
    }
}

/// The demuxer a sniffed container is read with.
fn demuxer(container: VideoContainer) -> &'static str {
    match container {
        VideoContainer::IsoBmff | VideoContainer::QuickTime => "mov",
        VideoContainer::Matroska | VideoContainer::WebM => "matroska",
        VideoContainer::Avi => "avi",
        VideoContainer::FlashVideo => "flv",
        VideoContainer::MpegProgramStream => "mpeg",
        VideoContainer::MpegVideo => "mpegvideo",
        VideoContainer::MpegTransportStream => "mpegts",
        VideoContainer::Ogg => "ogg",
        VideoContainer::Asf => "asf",
    }
}

/// The codec ffprobe names, in the protocol's terms.
fn codec(name: &str) -> VideoCodec {
    match name {
        "h264" => VideoCodec::H264,
        "hevc" => VideoCodec::Hevc,
        "vp8" => VideoCodec::Vp8,
        "vp9" => VideoCodec::Vp9,
        "av1" => VideoCodec::Av1,
        "mpeg4" => VideoCodec::Mpeg4,
        "mpeg2video" => VideoCodec::Mpeg2,
        "prores" => VideoCodec::ProRes,
        "mjpeg" => VideoCodec::Mjpeg,
        "qtrle" => VideoCodec::QuickTimeAnimation,
        "theora" => VideoCodec::Theora,
        "gif" => VideoCodec::Gif,
        "apng" => VideoCodec::Apng,
        "webp" => VideoCodec::WebP,
        _ => VideoCodec::Unknown,
    }
}

/// A rational number from ffprobe, `a/b` or `a:b`, both parts positive and
/// within 32 bits, as ffmpeg's own rationals are; anything wider is no
/// rational ffmpeg wrote. The bound keeps every product below in range.
fn ratio(value: &str) -> Option<(u64, u64)> {
    let (numerator, denominator) = value.split_once(['/', ':'])?;
    let numerator: u64 = numerator.parse().ok()?;
    let denominator: u64 = denominator.parse().ok()?;
    let part = 1..=u64::from(i32::MAX.unsigned_abs());
    (part.contains(&numerator) && part.contains(&denominator)).then_some((numerator, denominator))
}

/// Seconds from ffprobe as whole microseconds, truncated past the sixth
/// decimal; `None` for `N/A` or anything else.
fn micros(value: &str) -> Option<i64> {
    let (negative, digits) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty()
        || whole.len() > 12
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let mut fraction: String = fraction.chars().take(6).collect();
    while fraction.len() < 6 {
        fraction.push('0');
    }
    let value = whole.parse::<i64>().ok()? * 1_000_000 + fraction.parse::<i64>().ok()?;
    Some(if negative { -value } else { value })
}

fn audio_start_frame(text: &str, origin_us: i64) -> Option<u64> {
    let at = text
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            if matches!(
                key,
                "packets.packet.0.pts_time" | "packets.packet.0.dts_time"
            ) {
                micros(value.trim_matches('"'))
            } else {
                None
            }
        })
        .min()?
        .checked_sub(origin_us)?;
    kettle_media::audio::frames_at(at.max(0), 1_000_000)
}

/// Matroska's DURATION tag is the track's final timestamp, not its span.
fn tagged_end(value: &str) -> Option<i64> {
    if value.len() > 24 {
        return None;
    }
    let mut parts = value.split(':');
    let hours = parts.next()?;
    let minutes = parts.next()?;
    let seconds = parts.next()?;
    if parts.next().is_some()
        || hours.is_empty()
        || minutes.len() != 2
        || !hours
            .bytes()
            .chain(minutes.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let hours: i64 = hours.parse().ok()?;
    let minutes: i64 = minutes.parse().ok()?;
    let seconds = micros(seconds)?;
    if minutes >= 60 || !(0..60_000_000).contains(&seconds) {
        return None;
    }
    let end = hours
        .checked_mul(3_600_000_000)?
        .checked_add(minutes * 60_000_000)?
        .checked_add(seconds)?;
    (end <= kettle_media::MAX_VIDEO_DURATION_MS as i64 * 1000).then_some(end)
}

/// Whole microseconds as seconds for an argument: `[-]S.UUUUUU`.
fn seconds(micros: i64) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let magnitude = micros.unsigned_abs();
    format!(
        "{sign}{}.{:06}",
        magnitude / 1_000_000,
        magnitude % 1_000_000
    )
}

/// One stream in ffprobe's description.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Stream {
    index: Option<u32>,
    video: bool,
    audio: bool,
    cover: bool,
    codec: String,
    width: u32,
    height: u32,
    aspect: Option<(u64, u64)>,
    average_rate: Option<(u64, u64)>,
    real_rate: Option<(u64, u64)>,
    time_base: Option<(u64, u64)>,
    start_us: Option<i64>,
    duration_us: Option<i64>,
    tagged_end_us: Option<i64>,
    /// Degrees counterclockwise, as the display matrix says.
    rotation: Option<i64>,
}

/// What ffprobe says about the video: the stream decoded, and the file's
/// start and length.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Probe {
    stream: Stream,
    has_audio: bool,
    start_us: i64,
    duration_us: Option<i64>,
}

/// Read ffprobe's `flat` description: `section.key=value` lines, string
/// values quoted. Only the keys asked for are read; others are ignored.
fn parse_probe(text: &str) -> Result<Probe, FailureCode> {
    let mut streams: Vec<Stream> = Vec::new();
    let mut start_us = None;
    let mut format_duration = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(value);
        if let Some(field) = key.strip_prefix("format.") {
            match field {
                "start_time" => start_us = micros(value),
                "duration" => format_duration = micros(value),
                _ => {}
            }
            continue;
        }
        let Some(rest) = key.strip_prefix("streams.stream.") else {
            continue;
        };
        let Some((number, field)) = rest.split_once('.') else {
            continue;
        };
        let number: usize = number.parse().map_err(|_| FailureCode::RenderParse)?;
        if number >= MAX_STREAMS {
            return Err(FailureCode::RenderResource);
        }
        if streams.len() <= number {
            streams.resize_with(number + 1, Stream::default);
        }
        let stream = &mut streams[number];
        match field {
            "index" => stream.index = value.parse().ok(),
            "codec_type" => {
                stream.video = value == "video";
                stream.audio = value == "audio";
            }
            "codec_name" => {
                stream.codec = value
                    .chars()
                    .take(32)
                    .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
                    .collect();
            }
            "width" => stream.width = value.parse().unwrap_or(0),
            "height" => stream.height = value.parse().unwrap_or(0),
            "sample_aspect_ratio" => stream.aspect = ratio(value),
            "avg_frame_rate" => stream.average_rate = ratio(value),
            "r_frame_rate" => stream.real_rate = ratio(value),
            "time_base" => stream.time_base = ratio(value),
            "start_time" => stream.start_us = micros(value),
            "duration" => stream.duration_us = micros(value),
            "tags.DURATION" => stream.tagged_end_us = tagged_end(value),
            "disposition.attached_pic" => stream.cover = value == "1",
            _ if field.starts_with("side_data_list.side_data.") && field.ends_with(".rotation") => {
                let degrees = micros(value).map(|micros| micros / 1_000_000);
                stream.rotation = stream.rotation.or(degrees);
            }
            _ => {}
        }
    }
    let has_audio = streams.iter().any(|stream| stream.audio);
    let stream = streams
        .into_iter()
        .find(|stream| stream.video && !stream.cover && stream.index.is_some())
        .ok_or(FailureCode::UnsupportedMedia)?;
    let duration_us = format_duration
        .or(stream.duration_us)
        .filter(|duration| *duration >= 0);
    Ok(Probe {
        stream,
        has_audio,
        start_us: start_us.unwrap_or(0),
        duration_us,
    })
}

impl Probe {
    /// Degrees clockwise the picture is turned to show it, to the nearest
    /// quarter turn.
    fn rotation(&self) -> u16 {
        let counterclockwise = self.stream.rotation.unwrap_or(0);
        let clockwise = (-counterclockwise).rem_euclid(360);
        (((clockwise + 45) / 90 % 4) * 90) as u16
    }

    /// The picture as shown: its pixel aspect and rotation applied.
    fn shown(&self) -> Result<(u32, u32), FailureCode> {
        let (width, height) = (u64::from(self.stream.width), u64::from(self.stream.height));
        if width == 0 || height == 0 {
            return Err(FailureCode::RenderParse);
        }
        let width = match self.stream.aspect {
            Some((numerator, denominator)) => (width * numerator + denominator / 2) / denominator,
            None => width,
        }
        .max(1);
        let (width, height) = if self.rotation() % 180 == 90 {
            (height, width)
        } else {
            (width, height)
        };
        let side = u64::from(MAX_VIDEO_SIDE);
        if width > side || height > side {
            return Err(FailureCode::RenderResource);
        }
        Ok((width as u32, height as u32))
    }

    /// Frames per thousand seconds, from the average rate, else the real one.
    fn fps_milli(&self) -> Option<u32> {
        let (numerator, denominator) = self.stream.average_rate.or(self.stream.real_rate)?;
        let fps = (numerator * 1000 + denominator / 2) / denominator;
        u32::try_from(fps)
            .ok()
            .filter(|fps| (1..=MAX_VIDEO_FPS_MILLI).contains(fps))
    }

    /// One frame's length in whole ms, rounded up; a second when unknown.
    fn frame_ms(&self) -> u64 {
        self.fps_milli()
            .map_or(1000, |fps| 1_000_000u64.div_ceil(u64::from(fps)))
    }

    /// Where the video stream ends, in ms from the file's start, when it
    /// says how long it is: the file's other streams can run on past it.
    fn video_end_ms(&self) -> Option<u64> {
        let end = if let Some(duration) = self.stream.duration_us.filter(|duration| *duration >= 0)
        {
            self.stream
                .start_us
                .unwrap_or(self.start_us)
                .saturating_add(duration)
        } else {
            self.stream.tagged_end_us?
        };
        u64::try_from(end.saturating_sub(self.start_us).max(0) / 1000).ok()
    }
}

/// A stream's timestamps: its time base, and the file's start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Clock {
    numerator: i128,
    denominator: i128,
    start_us: i64,
}

impl Clock {
    /// The last tick at or before `ms` from the start.
    fn tick_at(&self, ms: u64) -> i64 {
        let micros = i128::from(self.start_us) + i128::from(ms) * 1000;
        let ticks = (micros * self.denominator).div_euclid(self.numerator * 1_000_000);
        ticks.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    /// A tick in whole microseconds, rounded down, so seeking to it never
    /// passes the frame that starts there.
    fn micros(&self, tick: i64) -> i64 {
        let micros = (i128::from(tick) * self.numerator * 1_000_000).div_euclid(self.denominator);
        micros.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    /// A tick as ms from the start, to the nearest, within `0..=duration`.
    fn ms(&self, tick: i64, duration_ms: u64) -> u64 {
        let from_start = i128::from(self.micros(tick)) - i128::from(self.start_us);
        let ms = (from_start + 500).div_euclid(1000);
        ms.clamp(0, i128::from(duration_ms)) as u64
    }
}

/// The video packets ffprobe listed: each frame's start, and which are
/// keyframes, both sorted. `None` when a packet has no timestamp.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Timeline {
    starts: Vec<i64>,
    keys: Vec<i64>,
}

/// Read ffprobe's `pts,flags` lines. A discarded packet is no frame; a
/// packet with no timestamp makes the list useless (`None`).
fn parse_packets(text: &str) -> Result<Option<Timeline>, FailureCode> {
    let mut timeline = Timeline::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (pts, flags) = line.split_once(',').unwrap_or((line, ""));
        if flags.contains('D') {
            continue;
        }
        let Ok(pts) = pts.parse::<i64>() else {
            return Ok(None);
        };
        if timeline.starts.len() == MAX_PACKETS {
            return Err(FailureCode::RenderResource);
        }
        timeline.starts.push(pts);
        if flags.contains('K') {
            timeline.keys.push(pts);
        }
    }
    for list in [&mut timeline.starts, &mut timeline.keys] {
        list.sort_unstable();
        list.dedup();
    }
    Ok((!timeline.starts.is_empty()).then_some(timeline))
}

impl Timeline {
    /// The frame showing at `tick`: the last starting at or before it, or
    /// the first after it when none starts that early.
    fn shown(&self, tick: i64) -> i64 {
        let after = self.starts.partition_point(|&start| start <= tick);
        self.starts[after.saturating_sub(1)]
    }

    /// The earliest frame starting in the same whole microsecond as the one
    /// at `tick`, which a seek to that microsecond returns.
    fn first_in_microsecond(&self, tick: i64, clock: &Clock) -> i64 {
        let mut at = self.starts.partition_point(|&start| start < tick);
        let micro = clock.micros(tick);
        while at > 0 && clock.micros(self.starts[at - 1]) == micro {
            at -= 1;
        }
        self.starts.get(at).copied().unwrap_or(tick)
    }

    /// The keyframe at or before `tick`, or the frame itself when none is.
    fn keyframe(&self, tick: i64) -> i64 {
        let after = self.keys.partition_point(|&key| key <= tick);
        after.checked_sub(1).map_or(tick, |index| self.keys[index])
    }
}

/// Where a frame is sought.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seek {
    /// The frame starting at this tick.
    Frame(i64),
    /// The first frame at or after this many ms from the start, for a
    /// stream without timestamps.
    Time(u64),
}

/// One decode: its fixed arguments around the numbers Kettle computed.
fn frame_args(
    container: VideoContainer,
    index: u32,
    seek: &str,
    exact: bool,
    width: u32,
    height: u32,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-protocol_whitelist",
        "file",
        "-threads",
        THREADS,
        "-f",
        demuxer(container),
    ]
    .iter()
    .map(OsString::from)
    .collect();
    if exact {
        // The seek names a timestamp, not a time from the file's start.
        args.extend(["-seek_timestamp", "1"].map(OsString::from));
    }
    args.extend(
        [
            "-ss".to_string(),
            seek.to_string(),
            "-i".to_string(),
            INPUT.to_string(),
            "-map".to_string(),
            format!("0:{index}"),
            "-an".to_string(),
            "-sn".to_string(),
            "-dn".to_string(),
            "-frames:v".to_string(),
            "1".to_string(),
            "-vf".to_string(),
            format!("scale={width}:{height}:flags=area,setsar=1"),
            "-pix_fmt".to_string(),
            "rgba".to_string(),
            "-f".to_string(),
            "rawvideo".to_string(),
            "pipe:1".to_string(),
        ]
        .map(OsString::from),
    );
    args
}

/// One session's decode: from `seek` seconds into the video until its end,
/// `fps_milli` frames per thousand seconds, each `width` by `height` RGBA.
fn play_args(
    container: VideoContainer,
    index: u32,
    seek: &str,
    width: u32,
    height: u32,
    fps_milli: u32,
) -> Vec<OsString> {
    [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-protocol_whitelist",
        "file",
        "-threads",
        THREADS,
        "-filter_threads",
        "1",
        "-f",
        demuxer(container),
        "-ss",
        seek,
        "-i",
        INPUT,
        "-map",
        &format!("0:{index}"),
        "-an",
        "-sn",
        "-dn",
        "-vf",
        // The rate first, so only the frames kept are scaled. At its end the
        // rate passes on the frame left rather than rounding it away: a
        // start on a video's last frame, at a rate slower than its own,
        // still shows that frame.
        &format!(
            "fps={fps_milli}/1000:eof_action=pass,scale={width}:{height}:flags=bilinear,setsar=1"
        ),
        "-pix_fmt",
        "rgba",
        // Raw output needs no frame-thread queue. Automatic encoder threading
        // can exhaust the worker's address space after a few large frames.
        "-threads:v",
        "1",
        "-f",
        "rawvideo",
        "pipe:1",
    ]
    .iter()
    .map(OsString::from)
    .collect()
}

/// The metadata slave comes first and flushes before either raw pipe.
fn sounding_args(
    mut args: Vec<OsString>,
    pipes: [i32; 2],
    initial_gap: bool,
    start_ms: u64,
    end_ms: u64,
) -> Vec<OsString> {
    let seek_index = args
        .iter()
        .position(|arg| arg == "-ss")
        .expect("fixed seek argument");
    let seek = args[seek_index + 1].to_string_lossy().into_owned();
    args.splice(
        0..0,
        [OsString::from("-copyts"), OsString::from("-start_at_zero")],
    );
    let seek_index = args
        .iter()
        .position(|arg| arg == "-ss")
        .expect("fixed seek argument");
    if seek == "0.000000" || initial_gap {
        // Seeking to a demuxer's first video keyframe can lose earlier sound.
        args.drain(seek_index..seek_index + 2);
    }
    // Shift PCM in integer output samples. Floating-point seconds can put
    // adjacent packets one sample apart in the wrong direction.
    let start_frame = start_ms * 48;
    let end_frame = end_ms * 48;
    if seek != "0.000000" {
        let vf = args
            .iter()
            .position(|arg| arg == "-vf")
            .expect("fixed video filter")
            + 1;
        // Both streams lose the same requested origin, never STARTPTS.
        args[vf] = format!(
            "trim=start={seek},setpts=PTS-{seek}/TB,{}",
            args[vf].to_string_lossy()
        )
        .into();
    }
    let an = args
        .iter()
        .position(|arg| arg == "-an")
        .expect("fixed play arguments");
    args.splice(an..=an, [OsString::from("-map"), OsString::from("0:a:0")]);
    args.truncate(args.len() - 3);
    args.extend([
        "-c:v".into(), "rawvideo".into(), "-c:a".into(), "pcm_f32le".into(),
        "-ar".into(), "48000".into(), "-ac".into(), "2".into(),
        "-af".into(), format!("aresample=48000,aformat=sample_fmts=flt:channel_layouts=stereo,atrim=start_pts={start_frame}:end_pts={end_frame},asetpts=PTS-{start_frame},asetnsamples=n=960:p=1").into(), "-f".into(), "tee".into(),
        format!("[f=framecrc:flush_packets=1:max_interleave_delta=1]pipe:1|[select=v:f=rawvideo:flush_packets=1]pipe:{}|[select=a:f=f32le:flush_packets=1]pipe:{}", pipes[0], pipes[1]).into(),
    ]);
    args
}

/// The description ffprobe is asked for.
fn probe_args(container: VideoContainer) -> Vec<String> {
    [
        "-v",
        "error",
        "-hide_banner",
        "-protocol_whitelist",
        "file",
        "-f",
        demuxer(container),
        "-show_entries",
        "stream=index,codec_type,codec_name,width,height,sample_aspect_ratio,\
         avg_frame_rate,r_frame_rate,time_base,start_time,duration:stream_disposition=attached_pic:\
         stream_side_data=rotation:stream_tags=DURATION:format=start_time,duration",
        "-of",
        "flat",
        INPUT,
    ]
    .map(str::to_string)
    .to_vec()
}

/// The packet list ffprobe is asked for: around each instant, or the whole
/// stream when `intervals` is empty.
fn packet_args(container: VideoContainer, index: u32, intervals: &[String]) -> Vec<String> {
    let mut args: Vec<String> = [
        "-v",
        "error",
        "-hide_banner",
        "-protocol_whitelist",
        "file",
        "-f",
        demuxer(container),
        "-select_streams",
    ]
    .map(str::to_string)
    .to_vec();
    args.push(index.to_string());
    if !intervals.is_empty() {
        args.push("-read_intervals".to_string());
        args.push(intervals.join(","));
    }
    args.extend(["-show_entries", "packet=pts,flags", "-of", "csv=p=0", INPUT].map(str::to_string));
    args
}

impl Ffmpeg {
    /// Run `tool` on a fresh descriptor of the input.
    fn run_on<A: AsRef<std::ffi::OsStr>>(
        tool: &Tool,
        input: &VideoInput<'_>,
        args: &[A],
        cap: usize,
        deadline: Instant,
    ) -> Result<Ran, FailureCode> {
        let file = reopen(input.file, input.identity)?;
        run(tool, args, Some(file), cap, deadline).map_err(failure)
    }

    fn probe(&self, input: &VideoInput<'_>, deadline: Instant) -> Result<Probe, FailureCode> {
        let ran = Self::run_on(
            &self.ffprobe,
            input,
            &probe_args(input.container),
            MAX_PROBE_BYTES,
            deadline,
        )?;
        if ran.overran {
            return Err(FailureCode::RenderResource);
        }
        if !ran.success {
            return Err(FailureCode::RenderParse);
        }
        let text = String::from_utf8(ran.stdout).map_err(|_| FailureCode::RenderParse)?;
        parse_probe(&text)
    }

    /// Stream start_time can be zero even when its first audio packet is late.
    fn first_audio_frame(
        &self,
        input: &VideoInput<'_>,
        origin_us: i64,
        deadline: Instant,
    ) -> Result<u64, FailureCode> {
        let args = [
            "-v",
            "error",
            "-hide_banner",
            "-protocol_whitelist",
            "file",
            "-f",
            demuxer(input.container),
            "-select_streams",
            "a:0",
            "-read_intervals",
            "%+#1",
            "-show_entries",
            "packet=pts_time,dts_time:packet_side_data=",
            "-of",
            "flat",
            INPUT,
        ];
        let ran = Self::run_on(&self.ffprobe, input, &args, 1024, deadline)?;
        if !ran.success || ran.overran {
            return Err(FailureCode::RenderParse);
        }
        let text = std::str::from_utf8(&ran.stdout).map_err(|_| FailureCode::RenderParse)?;
        audio_start_frame(text, origin_us).ok_or(FailureCode::RenderParse)
    }

    fn packets(
        &self,
        input: &VideoInput<'_>,
        index: u32,
        intervals: &[String],
        deadline: Instant,
    ) -> Result<Option<Timeline>, FailureCode> {
        let cap = if intervals.is_empty() {
            MAX_SCAN_BYTES
        } else {
            MAX_WINDOW_BYTES
        };
        let ran = Self::run_on(
            &self.ffprobe,
            input,
            &packet_args(input.container, index, intervals),
            cap,
            deadline,
        )?;
        if ran.overran {
            return Err(FailureCode::RenderResource);
        }
        if !ran.success {
            return Err(FailureCode::RenderParse);
        }
        let text = String::from_utf8(ran.stdout).map_err(|_| FailureCode::RenderParse)?;
        parse_packets(&text)
    }

    /// One frame, exactly `width` by `height` RGBA.
    #[allow(clippy::too_many_arguments)]
    fn frame(
        &self,
        input: &VideoInput<'_>,
        index: u32,
        clock: Option<Clock>,
        seek: Seek,
        width: u32,
        height: u32,
        deadline: Instant,
    ) -> Result<Vec<u8>, Option<FailureCode>> {
        let (at, exact) = match (seek, clock) {
            (Seek::Frame(tick), Some(clock)) => (seconds(clock.micros(tick)), true),
            (Seek::Time(ms), _) => (seconds(i64::try_from(ms * 1000).unwrap_or(i64::MAX)), false),
            (Seek::Frame(_), None) => return Err(Some(FailureCode::RenderParse)),
        };
        let size = width as usize * height as usize * 4;
        let args = frame_args(input.container, index, &at, exact, width, height);
        let ran = Self::run_on(&self.ffmpeg, input, &args, size, deadline).map_err(Some)?;
        if ran.success && !ran.overran && ran.stdout.len() == size {
            Ok(ran.stdout)
        } else {
            // The decoder failed or framed its output wrong: why is asked
            // once, by the caller.
            Err(None)
        }
    }

    /// Why a decode failed: its codec missing from this ffmpeg, or the
    /// stream itself.
    fn explain(&self, codec: &str, deadline: Instant) -> FailureCode {
        let args = ["-hide_banner", "-loglevel", "error", "-codecs"];
        match run(&self.ffmpeg, &args, None, MAX_CODECS_BYTES, deadline) {
            Err(error) => failure(error),
            // A list that did not come back whole says nothing either way.
            Ok(ran) if !ran.success || ran.overran => FailureCode::RenderParse,
            Ok(ran) if decodes(&String::from_utf8_lossy(&ran.stdout), codec) => {
                FailureCode::RenderParse
            }
            Ok(_) => FailureCode::CodecUnavailable,
        }
    }
}

/// Whether ffmpeg's `-codecs` list says it decodes `codec`: a line whose
/// first field's first flag is `D` and whose second field is the name.
fn decodes(list: &str, codec: &str) -> bool {
    list.lines().any(|line| {
        let mut fields = line.split_whitespace();
        matches!(
            (fields.next(), fields.next()),
            (Some(flags), Some(name)) if flags.len() == 6 && flags.starts_with('D') && name == codec
        )
    })
}

/// Whether the frames left, each taking as long as the slowest so far,
/// would run past the time remaining: then a frame's keyframe is decoded
/// instead. Nothing is hurried before one frame has been timed.
fn hurried(slowest: Option<Duration>, left: u32, remaining: Duration) -> bool {
    slowest.is_some_and(|slowest| slowest.saturating_mul(left) > remaining)
}

fn failure(error: RunError) -> FailureCode {
    match error {
        RunError::Unavailable => FailureCode::BackendUnavailable,
        RunError::Deadline => FailureCode::RenderTimeout,
    }
}

/// What ffprobe says a video is: the stream decoded, its clock and, when the
/// file gave no duration, its packets, which gave it; and the video as a
/// result describes it.
struct Described {
    probe: Probe,
    index: u32,
    clock: Option<Clock>,
    scanned: Option<Timeline>,
    info: VideoInfo,
}

impl Ffmpeg {
    fn describe(
        &self,
        input: &VideoInput<'_>,
        deadline: Instant,
    ) -> Result<Described, FailureCode> {
        let probe = self.probe(input, deadline)?;
        let index = probe.stream.index.ok_or(FailureCode::UnsupportedMedia)?;
        let clock = probe
            .stream
            .time_base
            .map(|(numerator, denominator)| Clock {
                numerator: i128::from(numerator),
                denominator: i128::from(denominator),
                start_us: probe.start_us,
            });
        // Without a duration the whole stream's packets are listed, which
        // also gives its length.
        let scanned = match (probe.duration_us, clock) {
            (None, Some(_)) => self.packets(input, index, &[], deadline)?,
            _ => None,
        };
        let duration_us = match (probe.duration_us, &scanned, clock) {
            (Some(duration), _, _) => i128::from(duration),
            (None, Some(timeline), Some(clock)) => {
                let last = *timeline.starts.last().unwrap_or(&0);
                (i128::from(clock.micros(last)) - i128::from(probe.start_us)).max(0)
                    + i128::from(probe.frame_ms()) * 1000
            }
            _ => return Err(FailureCode::RenderParse),
        };
        if duration_us > i128::from(kettle_media::MAX_VIDEO_DURATION_MS) * 1000 {
            return Err(FailureCode::RenderParse);
        }
        let duration_ms =
            u64::try_from(duration_us / 1000).map_err(|_| FailureCode::RenderParse)?;
        let (width, height) = probe.shown()?;
        let info = VideoInfo {
            duration_ms,
            width,
            height,
            rotation: probe.rotation(),
            codec: codec(&probe.stream.codec),
            fps_milli: probe.fps_milli(),
            has_audio: probe.has_audio,
            container: Some(input.container),
        };
        info.validate().map_err(|_| FailureCode::RenderParse)?;
        Ok(Described {
            probe,
            index,
            clock,
            scanned,
            info,
        })
    }
}

impl VideoPlayer for Ffmpeg {
    /// One ffmpeg run from the start asked for to the end, its frames
    /// timed from that start at the rate it was asked for: rawvideo carries
    /// no timestamps of its own. A start past the video's last frame shows
    /// that frame, though the file's sound runs on.
    fn play(
        &self,
        input: VideoInput<'_>,
        request: PlayRequest,
        deadline: Instant,
    ) -> Result<Playback, FailureCode> {
        let Described {
            probe,
            index,
            info,
            clock,
            mut scanned,
        } = self.describe(&input, deadline)?;
        // Unusual files omit both track-end fields. Reuse the bounded packet
        // scan and this opening's deadline, rather than treating audio as video.
        if probe.video_end_ms().is_none() && info.has_audio && scanned.is_none() && clock.is_some()
        {
            scanned = self.packets(&input, index, &[], deadline)?;
        }
        let (width, height) = play_size(
            info.width,
            info.height,
            request.max_width,
            request.max_height,
        )
        .ok_or(FailureCode::RenderResource)?;
        let fps_milli = info
            .fps_milli
            .map_or(request.max_fps_milli, |fps| fps.min(request.max_fps_milli));
        let video_end_ms = probe
            .video_end_ms()
            .or_else(|| {
                let last = *scanned.as_ref()?.starts.last()?;
                Some(
                    clock?
                        .ms(last, info.duration_ms)
                        .saturating_add(probe.frame_ms()),
                )
            })
            .unwrap_or(info.duration_ms)
            .min(info.duration_ms);
        let start_ms = request
            .start_ms
            .min(video_end_ms.saturating_sub(probe.frame_ms()));
        let mut opened = Opened {
            info,
            width,
            height,
            fps_milli,
            start_ms,
            audio: AudioStatus::silent(request.audio, info.has_audio),
        };
        opened.validate().map_err(|_| FailureCode::RenderParse)?;
        let seek = seconds(i64::try_from(start_ms * 1000).unwrap_or(i64::MAX));
        let args = play_args(input.container, index, &seek, width, height, fps_milli);
        let left = (info.duration_ms - start_ms).div_ceil(1000);
        let cpu = PLAY_CPU_SECONDS.saturating_add(left.saturating_mul(PLAY_CPU_PER_SECOND));
        let initial_gap = probe
            .stream
            .start_us
            .is_some_and(|video| video > probe.start_us.saturating_add((start_ms * 1000) as i64));
        if request.audio != kettle_media::AudioRequest::Off && info.has_audio {
            let sounding = (|| {
                let first_audio = self.first_audio_frame(&input, probe.start_us, deadline)?;
                if first_audio > info.duration_ms * 48 {
                    return Err(FailureCode::RenderParse);
                }
                let pipes = crate::run::triple(
                    &self.ffmpeg,
                    |pipes| {
                        sounding_args(args.clone(), pipes, initial_gap, start_ms, info.duration_ms)
                    },
                    reopen(input.file, input.identity)?,
                    cpu,
                )
                .map_err(failure)?;
                playback::split(
                    pipes,
                    width as usize * height as usize * 4,
                    start_ms,
                    info.duration_ms,
                    first_audio,
                    initial_gap,
                    deadline,
                )
            })();
            match sounding {
                Ok((frames, audio)) => {
                    return Ok(Playback {
                        video_end_ms: Some(video_end_ms),
                        opened: Opened {
                            audio: AudioStatus::Ready,
                            ..opened
                        },
                        frames: Box::new(frames),
                        audio: Some(Box::new(audio)),
                    });
                }
                Err(FailureCode::RenderParse) => opened.audio = AudioStatus::Unsupported,
                Err(error) => return Err(error),
            }
        }
        let file = reopen(input.file, input.identity)?;
        let streaming = stream(&self.ffmpeg, &args, file, cpu).map_err(failure)?;
        Ok(Playback {
            video_end_ms: Some(video_end_ms),
            opened,
            frames: Box::new(Frames {
                streaming,
                start_ms,
                duration_ms: info.duration_ms,
                fps_milli,
                taken: 0,
                size: width as usize * height as usize * 4,
                decoder: self.clone(),
                codec: probe.stream.codec,
            }),
            // No decoded audio was requested, or the file has no track.
            audio: None,
        })
    }
}

/// A session's frames from one ffmpeg run.
struct Frames {
    streaming: Streaming,
    start_ms: u64,
    duration_ms: u64,
    fps_milli: u32,
    /// Frames taken so far.
    taken: u64,
    /// Each frame's bytes: a buffer of any other length is refused, so a
    /// frame's edge is always the stream's.
    size: usize,
    /// Asked why, when the first frame does not come.
    decoder: Ffmpeg,
    codec: String,
}

impl FrameSource for Frames {
    /// The next frame, timed `taken` frame lengths after the start. Its
    /// output ending at a frame's edge, ffmpeg then exiting 0, ends the
    /// video. Ending before the first frame is a failure, explained; ffmpeg
    /// failing after a frame, or ending inside one, fails the stream, and
    /// one killed by a signal (its processor-time limit) is out of
    /// resources.
    fn next_frame(
        &mut self,
        rgba: &mut [u8],
        _gate: &PlayGate,
    ) -> Result<Option<u64>, FailureCode> {
        if rgba.len() != self.size {
            return Err(FailureCode::BadParams);
        }
        match self.streaming.next_piece(rgba) {
            Ok(Piece::Filled) => {
                let after = self.taken * 1_000_000 / u64::from(self.fps_milli);
                self.taken += 1;
                Ok(Some((self.start_ms + after).min(self.duration_ms)))
            }
            Ok(Piece::Ended(Exit::Clean)) if self.taken > 0 => Ok(None),
            Ok(Piece::Ended(Exit::Killed)) => Err(FailureCode::RenderResource),
            Ok(Piece::Ended(_)) if self.taken == 0 => Err(self
                .decoder
                .explain(&self.codec, Instant::now() + EXPLAIN_WITHIN)),
            Ok(Piece::Ended(_)) | Err(()) => Err(FailureCode::RenderParse),
        }
    }
}

/// The most ffprobe says of a still: a grid lists each of its tiles.
const MAX_STILL_PROBE_BYTES: usize = 256 * 1024;
/// The most `ffmpeg -version` says.
const MAX_VERSION_BYTES: usize = 64 * 1024;

/// What ffprobe says of a HEIF or AVIF still: its coding; the size it is
/// shown at, a grid of tiles' own size, turned as it is shown; how far it is
/// turned to be shown, clockwise, in quarter turns' degrees; and whether it
/// is a grid.
#[derive(Debug, PartialEq, Eq)]
struct StillProbe {
    codec: String,
    width: u32,
    height: u32,
    turn: u32,
    grid: bool,
}

/// What ffprobe is asked of a still: its first stream, its turn, and the
/// grid of tiles it is made of, if it is.
fn still_probe_args() -> Vec<String> {
    [
        "-v",
        "error",
        "-hide_banner",
        "-protocol_whitelist",
        "file",
        "-f",
        "mov",
        "-show_entries",
        "stream=codec_type,codec_name,width,height:stream_side_data=rotation:\
         stream_group=type:stream_group_components",
        "-of",
        "flat",
        INPUT,
    ]
    .map(str::to_string)
    .to_vec()
}

/// Read ffprobe's answer for a still. A turn is ffmpeg's own reading of the
/// display matrix, rounded to a quarter: the matrix says how far it turns
/// counterclockwise, which is shown turned the other way.
fn parse_still_probe(text: &str) -> Result<StillProbe, FailureCode> {
    let (mut codec, mut width, mut height, mut rotation) = (None, None, None, None);
    let (mut grid, mut grid_codec, mut grid_width, mut grid_height) = (false, None, None, None);
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim_matches('"');
        match key {
            "streams.stream.0.codec_name" => codec = Some(value.to_string()),
            "streams.stream.0.width" => width = value.parse::<u32>().ok(),
            "streams.stream.0.height" => height = value.parse::<u32>().ok(),
            "stream_groups.stream_group.0.type" => grid = value == "Tile Grid",
            "stream_groups.stream_group.0.streams.stream.0.codec_name" => {
                grid_codec = Some(value.to_string());
            }
            "stream_groups.stream_group.0.components.component.0.width" => {
                grid_width = value.parse::<u32>().ok();
            }
            "stream_groups.stream_group.0.components.component.0.height" => {
                grid_height = value.parse::<u32>().ok();
            }
            _ if key.ends_with(".rotation") && rotation.is_none() => {
                rotation = value.parse::<f64>().ok();
            }
            _ => {}
        }
    }
    let (codec, width, height) = if grid {
        (grid_codec, grid_width, grid_height)
    } else {
        (codec, width, height)
    };
    let (Some(codec), Some(width), Some(height)) = (codec, width, height) else {
        return Err(FailureCode::RenderParse);
    };
    let turn = match rotation {
        None => 0,
        Some(degrees) if degrees.is_finite() => {
            ((-degrees / 90.0).round() as i64).rem_euclid(4) as u32 * 90
        }
        Some(_) => return Err(FailureCode::RenderParse),
    };
    let (width, height) = if turn % 180 == 90 {
        (height, width)
    } else {
        (width, height)
    };
    Ok(StillProbe {
        codec,
        width,
        height,
        turn,
        grid,
    })
}

/// The decode of a still, turned as it is shown and drawn at exactly
/// `width` by `height`: a grid of tiles as the one picture they make. ffmpeg
/// turns nothing itself, so the size is the one asked for.
fn still_args(grid: bool, turn: u32, width: u32, height: u32) -> Vec<String> {
    let turned = match turn {
        90 => "transpose=clock,",
        180 => "hflip,vflip,",
        270 => "transpose=cclock,",
        _ => "",
    };
    let filters = format!("{turned}scale={width}:{height}:flags=area,setsar=1");
    let mut args: Vec<String> = [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-protocol_whitelist",
        "file",
        "-threads",
        THREADS,
        "-noautorotate",
        "-f",
        "mov",
        "-i",
        INPUT,
    ]
    .map(str::to_string)
    .to_vec();
    if grid {
        args.extend([
            "-filter_complex".to_string(),
            format!("[0:g:0]{filters}[still]"),
            "-map".to_string(),
            "[still]".to_string(),
        ]);
    } else {
        args.extend([
            "-map".to_string(),
            "0:v:0".to_string(),
            "-vf".to_string(),
            filters,
        ]);
    }
    args.extend(
        [
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "pipe:1",
        ]
        .map(str::to_string),
    );
    args
}

/// Whether ffmpeg's first `-version` line names a release that reads HEIF
/// and AVIF stills whole, a grid of tiles as one picture: 7.1 or later. A
/// build named by its commit (`N-…`) is a recent one.
fn reads_stills(version_line: &str) -> bool {
    let Some(rest) = version_line.strip_prefix("ffmpeg version ") else {
        return false;
    };
    let version = rest.split_whitespace().next().unwrap_or_default();
    if version.starts_with('N') {
        return true;
    }
    let version = version.strip_prefix('n').unwrap_or(version);
    let mut numbers = version
        .split(|c: char| !c.is_ascii_digit())
        .map(|part| part.parse::<u32>().ok());
    match (numbers.next().flatten(), numbers.next().flatten()) {
        (Some(major), minor) => (major, minor.unwrap_or(0)) >= (7, 1),
        (None, _) => false,
    }
}

impl Ffmpeg {
    /// Whether this ffmpeg reads HEIF and AVIF stills, by its version.
    fn reads_stills(&self, deadline: Instant) -> Result<bool, FailureCode> {
        let ran = run(
            &self.ffmpeg,
            &["-hide_banner", "-version"],
            None,
            MAX_VERSION_BYTES,
            deadline,
        )
        .map_err(failure)?;
        let text = String::from_utf8_lossy(&ran.stdout);
        Ok(ran.success && reads_stills(text.lines().next().unwrap_or_default()))
    }
}

impl VideoDecoder for Ffmpeg {
    /// A HEIF or AVIF still, from the file it was read from, opened again: a
    /// release too old to read one, or bytes with no file, is
    /// `CodecUnavailable`; a still past the caps is `FileTooLarge`.
    fn still_image(
        &self,
        input: StillInput<'_>,
        _still: StillImage,
        max_edge: u32,
        deadline: Instant,
    ) -> Result<DecodedStill, FailureCode> {
        let Some((file, identity)) = input.file else {
            return Err(FailureCode::CodecUnavailable);
        };
        if !self.reads_stills(deadline)? {
            return Err(FailureCode::CodecUnavailable);
        }
        let ran = run(
            &self.ffprobe,
            &still_probe_args(),
            Some(reopen(file, identity)?),
            MAX_STILL_PROBE_BYTES,
            deadline,
        )
        .map_err(failure)?;
        if ran.overran {
            return Err(FailureCode::RenderResource);
        }
        if !ran.success {
            return Err(FailureCode::RenderParse);
        }
        let probe = parse_still_probe(
            &String::from_utf8(ran.stdout).map_err(|_| FailureCode::RenderParse)?,
        )?;
        if !still_size_drawn(probe.width, probe.height) {
            return Err(FailureCode::FileTooLarge);
        }
        let (width, height) = still_fit(probe.width, probe.height, max_edge);
        let size = width as usize * height as usize * 4;
        let ran = run(
            &self.ffmpeg,
            &still_args(probe.grid, probe.turn, width, height),
            Some(reopen(file, identity)?),
            size,
            deadline,
        )
        .map_err(failure)?;
        if ran.success && !ran.overran && ran.stdout.len() == size {
            return Ok(DecodedStill {
                width,
                height,
                rgba: ran.stdout,
                source_width: probe.width,
                source_height: probe.height,
            });
        }
        // The decoder failed or framed its output wrong: its codec missing
        // from this ffmpeg, or the still itself.
        Err(self.explain(&probe.codec, deadline))
    }

    fn stills(
        &self,
        input: VideoInput<'_>,
        plan: &mut dyn FnMut(&VideoInfo) -> Result<StillsPlan, FailureCode>,
        deadline: Instant,
    ) -> Result<DecodedStills, FailureCode> {
        let Described {
            probe,
            index,
            clock,
            scanned,
            info,
        } = self.describe(&input, deadline)?;
        let duration_ms = info.duration_ms;
        let wanted = plan(&info)?;
        if wanted.times_ms.is_empty() || wanted.width == 0 || wanted.height == 0 {
            return Err(FailureCode::BadParams);
        }
        let timeline = match (scanned, clock) {
            (Some(timeline), _) => Some(timeline),
            (None, Some(clock)) => {
                let window_us = i64::try_from(probe.frame_ms() * WINDOW_FRAMES * 1000)
                    .unwrap_or(i64::MAX)
                    .max(WINDOW_US);
                let mut instants: Vec<u64> = wanted.times_ms.clone();
                instants.sort_unstable();
                instants.dedup();
                let intervals: Vec<String> = instants
                    .iter()
                    .map(|&ms| {
                        let at = clock.micros(clock.tick_at(ms));
                        format!("{}%{}", seconds(at), seconds(at.saturating_add(window_us)))
                    })
                    .collect();
                self.packets(&input, index, &intervals, deadline)?
            }
            (None, None) => None,
        };
        let timed = timeline.as_ref().zip(clock);

        let mut frames: Vec<DecodedFrame> = Vec::with_capacity(wanted.times_ms.len());
        let mut decoded: Vec<(Seek, usize)> = Vec::new();
        let mut slowest: Option<Duration> = None;
        let mut tolerance_ms = 0u64;
        for (at, &requested_ms) in wanted.times_ms.iter().enumerate() {
            let (seek, actual_ms) = match timed {
                Some((timeline, clock)) => {
                    let shown = timeline.shown(clock.tick_at(requested_ms));
                    // Past the deadline's pace, the keyframe decodes at once.
                    let left = (wanted.times_ms.len() - at) as u32;
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let tick = if hurried(slowest, left, remaining) {
                        timeline.keyframe(shown)
                    } else {
                        shown
                    };
                    // A seek in whole microseconds cannot pass over a frame
                    // starting earlier in the same microsecond.
                    let tick = timeline.first_in_microsecond(tick, &clock);
                    (Seek::Frame(tick), clock.ms(tick, duration_ms))
                }
                None => {
                    tolerance_ms = tolerance_ms.max(probe.frame_ms());
                    (Seek::Time(requested_ms), requested_ms)
                }
            };
            tolerance_ms = tolerance_ms.max(requested_ms.abs_diff(actual_ms));
            let reused = decoded
                .iter()
                .find(|(done, _)| *done == seek)
                .map(|&(_, earlier)| frames[earlier].rgba.clone());
            let rgba = match reused {
                Some(rgba) => rgba,
                None => {
                    let started = Instant::now();
                    let rgba = self
                        .frame(
                            &input,
                            index,
                            clock,
                            seek,
                            wanted.width,
                            wanted.height,
                            deadline,
                        )
                        .map_err(|why| {
                            why.unwrap_or_else(|| self.explain(&probe.stream.codec, deadline))
                        })?;
                    let took = started.elapsed();
                    slowest = Some(slowest.map_or(took, |slowest| slowest.max(took)));
                    decoded.push((seek, frames.len()));
                    rgba
                }
            };
            frames.push(DecodedFrame {
                sample: StillSample {
                    requested_ms,
                    actual_ms,
                },
                rgba,
            });
        }
        Ok(DecodedStills {
            info,
            frames,
            // Within the duration: every time is, and an untimed stream's
            // frame length may not be.
            tolerance_ms: u32::try_from(tolerance_ms.min(duration_ms)).unwrap_or(u32::MAX),
        })
    }
}

/// What the fuzz target and the sweep below check of the text ffprobe and
/// ffmpeg write, split at NUL bytes into a description, a packet list and a
/// codec list: nothing panics; first-audio timestamps stay within the duration
/// bound; a description that parses shows a picture
/// within bounds or fails, turns it by a quarter turn and has a frame
/// length of at most a thousand seconds; a packet list that parses is sorted and distinct, its keyframes
/// among its frames, and every frame it finds for a time is one of them.
#[cfg(any(test, feature = "fuzzing"))]
pub(crate) fn check_text(data: &[u8]) {
    let mut parts = data
        .splitn(3, |byte| *byte == 0)
        .map(|part| std::str::from_utf8(part).ok());
    let (description, packets, codecs) = (
        parts.next().flatten(),
        parts.next().flatten(),
        parts.next().flatten(),
    );
    let probe = description.and_then(|text| parse_probe(text).ok());
    if let Some(text) = description {
        for origin in [
            i64::MIN,
            0,
            i64::MAX,
            probe.as_ref().map_or(0, |p| p.start_us),
        ] {
            if let Some(frame) = audio_start_frame(text, origin) {
                assert!(frame <= kettle_media::MAX_VIDEO_DURATION_MS * 48);
            }
        }
    }
    if let Some(probe) = &probe {
        if let Ok((width, height)) = probe.shown() {
            let side = 1..=MAX_VIDEO_SIDE;
            assert!(side.contains(&width) && side.contains(&height));
        }
        assert!(matches!(probe.rotation(), 0 | 90 | 180 | 270));
        // A rate of one frame per thousand seconds is the slowest kept.
        assert!((1..=1_000_000).contains(&probe.frame_ms()));
        if let Some(fps) = probe.fps_milli() {
            assert!((1..=MAX_VIDEO_FPS_MILLI).contains(&fps));
        }
        let _ = (probe.video_end_ms(), codec(&probe.stream.codec));
    }
    if let Some(Ok(Some(timeline))) = packets.map(parse_packets) {
        let rising = |list: &[i64]| list.windows(2).all(|pair| pair[0] < pair[1]);
        assert!(rising(&timeline.starts) && rising(&timeline.keys));
        let among = |tick: i64| timeline.starts.binary_search(&tick).is_ok();
        assert!(timeline.keys.iter().all(|&key| among(key)));
        let (first, last) = (
            timeline.starts[0],
            timeline.starts[timeline.starts.len() - 1],
        );
        for tick in [i64::MIN, -1, 0, 1, first, last, i64::MAX] {
            assert!(among(timeline.shown(tick)));
            let key = timeline.keyframe(tick);
            assert!(key == tick || (key <= tick && among(key)));
        }
        let clock = probe.as_ref().and_then(|probe| {
            let (numerator, denominator) = probe.stream.time_base?;
            Some(Clock {
                numerator: i128::from(numerator),
                denominator: i128::from(denominator),
                start_us: probe.start_us,
            })
        });
        if let Some(clock) = clock {
            for &tick in timeline.starts.iter().take(64) {
                let earliest = timeline.first_in_microsecond(tick, &clock);
                assert!(earliest <= tick && among(earliest));
                assert!(clock.ms(tick, 10_000) <= 10_000);
            }
        }
    }
    if let Some(list) = codecs {
        let _ = decodes(list, "h264");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_audio_packet_timestamps_share_the_format_origin() {
        for (text, origin, expected) in [
            ("packets.packet.0.pts_time=\"10.000000\"", 0, Some(480_000)),
            (
                "packets.packet.0.pts_time=\"13.000000\"",
                3_000_000,
                Some(480_000),
            ),
            (
                "packets.packet.0.pts_time=\"0.000000\"\npackets.packet.0.dts_time=\"-0.020000\"",
                0,
                Some(0),
            ),
            (
                "packets.packet.0.pts_time=\"13.000000\"\npackets.packet.0.dts_time=\"12.980000\"",
                3_000_000,
                Some(479_040),
            ),
            (
                "packets.packet.0.pts_time=\"N/A\"\npackets.packet.0.dts_time=\"0.500000\"",
                0,
                Some(24_000),
            ),
            ("packets.packet.1.pts_time=\"10.000000\"", 0, None),
            ("packets.packet.0.pts_time=\"999999999999.000000\"", 0, None),
            ("packets.packet.0.pts_time=\"10.000000\"", i64::MIN, None),
            ("", 0, None),
        ] {
            assert_eq!(audio_start_frame(text, origin), expected, "{text:?}");
        }
    }

    /// A still is read from its first stream, or from the grid of tiles it
    /// is made of; a turn of a quarter swaps the size it is shown at; and an
    /// answer without a coding or a size is a broken still.
    #[test]
    fn still_probes_say_what_is_shown() {
        let plain = "streams.stream.0.codec_name=\"av1\"\nstreams.stream.0.codec_type=\"video\"\n\
                     streams.stream.0.width=640\nstreams.stream.0.height=480\n";
        assert_eq!(
            parse_still_probe(plain),
            Ok(StillProbe {
                codec: "av1".into(),
                width: 640,
                height: 480,
                turn: 0,
                grid: false,
            })
        );
        let turned = format!("{plain}streams.stream.0.side_data_list.side_data.0.rotation=-90\n");
        let probe = parse_still_probe(&turned).unwrap();
        assert_eq!((probe.width, probe.height, probe.turn), (480, 640, 90));
        let back = format!("{plain}streams.stream.0.side_data_list.side_data.0.rotation=90\n");
        assert_eq!(parse_still_probe(&back).unwrap().turn, 270);
        let half = format!("{plain}streams.stream.0.side_data_list.side_data.0.rotation=180\n");
        let probe = parse_still_probe(&half).unwrap();
        assert_eq!((probe.width, probe.height, probe.turn), (640, 480, 180));
        let grid = "stream_groups.stream_group.0.type=\"Tile Grid\"\n\
                    stream_groups.stream_group.0.components.component.0.nb_tiles=54\n\
                    stream_groups.stream_group.0.components.component.0.coded_width=8064\n\
                    stream_groups.stream_group.0.components.component.0.coded_height=6144\n\
                    stream_groups.stream_group.0.components.component.0.width=8064\n\
                    stream_groups.stream_group.0.components.component.0.height=6048\n\
                    stream_groups.stream_group.0.streams.stream.0.codec_name=\"hevc\"\n\
                    stream_groups.stream_group.0.streams.stream.0.width=896\n\
                    stream_groups.stream_group.0.streams.stream.0.height=1024\n\
                    streams.stream.0.codec_name=\"hevc\"\nstreams.stream.0.width=896\n\
                    streams.stream.0.height=1024\n";
        assert_eq!(
            parse_still_probe(grid),
            Ok(StillProbe {
                codec: "hevc".into(),
                width: 8064,
                height: 6048,
                turn: 0,
                grid: true,
            })
        );
        assert_eq!(
            parse_still_probe("streams.stream.0.width=640\n"),
            Err(FailureCode::RenderParse)
        );
        let nan = format!("{plain}streams.stream.0.side_data_list.side_data.0.rotation=nan\n");
        assert_eq!(parse_still_probe(&nan), Err(FailureCode::RenderParse));
    }

    /// A still decodes its first stream, or its grid through a graph of its
    /// own, turned by Kettle and never by ffmpeg, at exactly the size asked.
    #[test]
    fn stills_decode_at_exactly_the_size_asked() {
        let plain = still_args(false, 0, 64, 48);
        let at = |args: &[String], flag: &str| {
            args.iter()
                .position(|arg| arg == flag)
                .map(|index| args[index + 1].clone())
        };
        assert!(plain.iter().any(|arg| arg == "-noautorotate"));
        assert!(
            plain.iter().position(|arg| arg == "-noautorotate")
                < plain.iter().position(|arg| arg == "-i")
        );
        assert_eq!(at(&plain, "-map").as_deref(), Some("0:v:0"));
        assert_eq!(
            at(&plain, "-vf").as_deref(),
            Some("scale=64:48:flags=area,setsar=1")
        );
        assert_eq!(at(&plain, "-pix_fmt").as_deref(), Some("rgba"));
        assert_eq!(plain.last().map(String::as_str), Some("pipe:1"));
        let turned = still_args(false, 90, 48, 64);
        assert_eq!(
            at(&turned, "-vf").as_deref(),
            Some("transpose=clock,scale=48:64:flags=area,setsar=1")
        );
        let grid = still_args(true, 270, 64, 48);
        assert_eq!(
            at(&grid, "-filter_complex").as_deref(),
            Some("[0:g:0]transpose=cclock,scale=64:48:flags=area,setsar=1[still]")
        );
        assert_eq!(at(&grid, "-map").as_deref(), Some("[still]"));
        assert!(!grid.iter().any(|arg| arg == "-vf"));
    }

    /// Only a release that reads stills whole, 7.1 or later, or a build
    /// named by its commit, is asked for one.
    #[test]
    fn only_a_recent_ffmpeg_reads_stills() {
        for (line, reads) in [
            (
                "ffmpeg version 8.0.1-3ubuntu2 Copyright (c) 2000-2025 the FFmpeg developers",
                true,
            ),
            (
                "ffmpeg version 9.0.2 Copyright (c) 2000-2026 the FFmpeg developers",
                true,
            ),
            ("ffmpeg version 7.1 Copyright", true),
            ("ffmpeg version n7.1.1 Copyright", true),
            ("ffmpeg version 7.0.2 Copyright", false),
            ("ffmpeg version 6.1.1-3ubuntu5 Copyright", false),
            ("ffmpeg version 4.4.2-0ubuntu0.22.04.1 Copyright", false),
            ("ffmpeg version N-118215-g1234abcd Copyright", true),
            ("ffmpeg version git-2026-10-01 Copyright", false),
            ("ffprobe version 8.0.1", false),
            ("", false),
        ] {
            assert_eq!(reads_stills(line), reads, "{line}");
        }
    }

    #[test]
    fn numbers_from_ffprobe_are_read_exactly_or_not_at_all() {
        assert_eq!(micros("10.000000"), Some(10_000_000));
        assert_eq!(micros("0.0333667"), Some(33_366));
        assert_eq!(micros("-1.5"), Some(-1_500_000));
        assert_eq!(micros("7"), Some(7_000_000));
        for bad in ["N/A", "", ".5", "1e3", "1.2.3", "-", "9999999999999"] {
            assert_eq!(micros(bad), None, "{bad}");
        }
        assert_eq!(seconds(33_366), "0.033366");
        assert_eq!(seconds(-1_500_000), "-1.500000");
        assert_eq!(ratio("30000/1001"), Some((30000, 1001)));
        assert_eq!(ratio("1:1"), Some((1, 1)));
        assert_eq!(ratio("2147483647/1"), Some((2_147_483_647, 1)));
        for bad in [
            "0/0",
            "30/0",
            "0:1",
            "N/A",
            "30",
            "2147483648/1",
            "18446744073709551615/18446744073709551615",
        ] {
            assert_eq!(ratio(bad), None, "{bad}");
        }
    }

    const ROTATED: &str = r#"streams.stream.0.index=0
streams.stream.0.codec_name="h264"
streams.stream.0.codec_type="video"
streams.stream.0.width=1280
streams.stream.0.height=720
streams.stream.0.sample_aspect_ratio="1:1"
streams.stream.0.r_frame_rate="30/1"
streams.stream.0.avg_frame_rate="30000/1001"
streams.stream.0.time_base="1/15360"
streams.stream.0.duration="10.000000"
streams.stream.0.disposition.attached_pic=0
streams.stream.0.side_data_list.side_data.0.rotation=90
streams.stream.1.index=1
streams.stream.1.codec_name="aac"
streams.stream.1.codec_type="audio"
streams.stream.1.r_frame_rate="0/0"
format.start_time="0.000000"
format.duration="10.000000"
"#;

    /// A description gives the video stream's codec, size as shown (a
    /// display matrix of 90 degrees counterclockwise turns a landscape
    /// picture upright), rate, clock and audio.
    #[test]
    fn matroska_track_end_tags_do_not_shift_a_delayed_video_twice() {
        let description = ROTATED.replace(
            "streams.stream.0.duration=\"10.000000\"",
            "streams.stream.0.duration=\"N/A\"",
        );
        let tagged = format!(
            "{description}\nstreams.stream.0.start_time=\"1.000000\"\nstreams.stream.0.tags.DURATION=\"00:00:04.000000000\"\n"
        );
        assert_eq!(parse_probe(&tagged).unwrap().video_end_ms(), Some(4000));
        for value in [
            "00:60:00",
            "00:00:60",
            "-1:00:00",
            "999999999999:00:00",
            "00:00:NaN",
        ] {
            let invalid = format!("{description}\nstreams.stream.0.tags.DURATION=\"{value}\"\n");
            assert_eq!(
                parse_probe(&invalid).unwrap().video_end_ms(),
                None,
                "{value}"
            );
        }
    }

    #[test]
    fn a_description_reads_as_shown() {
        let probe = parse_probe(ROTATED).unwrap();
        assert_eq!(probe.stream.index, Some(0));
        assert_eq!(codec(&probe.stream.codec), VideoCodec::H264);
        assert_eq!(probe.rotation(), 270);
        assert_eq!(probe.shown(), Ok((720, 1280)));
        assert_eq!(probe.fps_milli(), Some(29_970));
        assert_eq!(probe.frame_ms(), 34);
        assert!(probe.has_audio);
        assert_eq!((probe.start_us, probe.duration_us), (0, Some(10_000_000)));
        assert_eq!(probe.stream.time_base, Some((1, 15360)));
    }

    /// The video's own end, when its stream gives a length: from where the
    /// stream starts in the file, which may be after the file's start.
    #[test]
    fn the_video_ends_where_its_stream_does() {
        let probe = |lines: &str| {
            parse_probe(&format!(
                "streams.stream.0.index=0\nstreams.stream.0.codec_type=\"video\"\n{lines}"
            ))
            .unwrap()
        };
        let shorter = probe(
            "streams.stream.0.start_time=\"0.000000\"\nstreams.stream.0.duration=\"4.000000\"\n\
             format.start_time=\"0.000000\"\nformat.duration=\"10.000000\"\n",
        );
        assert_eq!(
            (shorter.duration_us, shorter.video_end_ms()),
            (Some(10_000_000), Some(4000))
        );
        let later = probe(
            "streams.stream.0.start_time=\"1.500000\"\nstreams.stream.0.duration=\"4.000000\"\n\
             format.start_time=\"1.000000\"\nformat.duration=\"10.000000\"\n",
        );
        assert_eq!(later.video_end_ms(), Some(4500));
        let earlier = probe(
            "streams.stream.0.start_time=\"-1.000000\"\nstreams.stream.0.duration=\"2.000000\"\n\
             format.start_time=\"0.000000\"\nformat.duration=\"3.000000\"\n",
        );
        assert_eq!(earlier.video_end_ms(), Some(1000));
        let before = probe(
            "streams.stream.0.start_time=\"-3.000000\"\nstreams.stream.0.duration=\"2.000000\"\n\
             format.start_time=\"0.000000\"\nformat.duration=\"3.000000\"\n",
        );
        assert_eq!(before.video_end_ms(), Some(0));
        let untold = probe("format.duration=\"10.000000\"\n");
        assert_eq!(untold.video_end_ms(), None);
        // ffprobe is asked for each stream's start and length.
        let args = probe_args(VideoContainer::IsoBmff);
        let streams = args
            .iter()
            .find_map(|arg| arg.strip_prefix("stream="))
            .and_then(|entries| entries.split(':').next())
            .unwrap();
        let fields: Vec<&str> = streams.split(',').collect();
        assert!(fields.contains(&"start_time") && fields.contains(&"duration"));
    }

    /// Cover art is skipped for the real video; a stream's pixel aspect
    /// widens it; with no format duration the stream's is used, and with
    /// neither there is none; a description with no video, or too many
    /// streams, is refused.
    #[test]
    fn descriptions_choose_the_real_video() {
        let cover = "streams.stream.0.index=0\nstreams.stream.0.codec_type=\"video\"\n\
            streams.stream.0.codec_name=\"mjpeg\"\nstreams.stream.0.width=600\n\
            streams.stream.0.height=600\nstreams.stream.0.disposition.attached_pic=1\n\
            streams.stream.1.index=1\nstreams.stream.1.codec_type=\"video\"\n\
            streams.stream.1.codec_name=\"mpeg2video\"\nstreams.stream.1.width=720\n\
            streams.stream.1.height=576\nstreams.stream.1.sample_aspect_ratio=\"16:15\"\n\
            streams.stream.1.duration=\"4.000000\"\nstreams.stream.1.side_data_list.side_data.0.rotation=-90\n\
            format.duration=\"N/A\"\n";
        let probe = parse_probe(cover).unwrap();
        assert_eq!(probe.stream.index, Some(1));
        assert_eq!(probe.rotation(), 90);
        assert_eq!(probe.shown(), Ok((576, 768)));
        assert_eq!(probe.duration_us, Some(4_000_000));
        assert!(!probe.has_audio);
        assert_eq!(probe.fps_milli(), None);
        assert_eq!(probe.frame_ms(), 1000);

        let unknown = "streams.stream.0.index=0\nstreams.stream.0.codec_type=\"video\"\n\
            streams.stream.0.width=16\nstreams.stream.0.height=16\nformat.duration=\"N/A\"\n";
        assert_eq!(parse_probe(unknown).unwrap().duration_us, None);
        assert_eq!(
            parse_probe("streams.stream.0.index=0\nstreams.stream.0.codec_type=\"audio\"\n")
                .unwrap_err(),
            FailureCode::UnsupportedMedia
        );
        assert_eq!(
            parse_probe("streams.stream.64.index=64\n").unwrap_err(),
            FailureCode::RenderResource
        );
        let huge = ROTATED.replace("width=1280", "width=20000");
        assert_eq!(
            parse_probe(&huge).unwrap().shown(),
            Err(FailureCode::RenderResource)
        );
        let zero = ROTATED.replace("height=720", "height=0");
        assert_eq!(
            parse_probe(&zero).unwrap().shown(),
            Err(FailureCode::RenderParse)
        );
    }

    /// Ticks convert exactly: an instant's tick is the last at or before
    /// it, a tick's microseconds round down, and its ms round to nearest
    /// within the duration, all from the file's start.
    #[test]
    fn the_clock_converts_without_drift() {
        let ntsc = Clock {
            numerator: 1001,
            denominator: 30000,
            start_us: 0,
        };
        assert_eq!(ntsc.tick_at(1000), 29);
        assert_eq!(ntsc.micros(1), 33_366);
        assert_eq!(ntsc.ms(1, 10_000), 33);
        assert_eq!(ntsc.ms(-5, 10_000), 0);
        assert_eq!(ntsc.ms(1_000_000, 10_000), 10_000);
        let late = Clock {
            numerator: 1,
            denominator: 90000,
            start_us: 1_400_000,
        };
        assert_eq!(late.tick_at(0), 126_000);
        assert_eq!(late.tick_at(500), 171_000);
        assert_eq!(late.ms(171_000, 10_000), 500);
    }

    /// The frame showing at an instant is the last starting at or before
    /// it (the first, before any starts); the keyframe is the last at or
    /// before that; discarded packets are no frames, and a packet without a
    /// timestamp makes the list unusable.
    #[test]
    fn packets_give_the_frame_showing() {
        let text = "0,K__\n3000,___\n1000,___\n2000,___\n5000,K__\n4000,_D_\n6000,___\n";
        let timeline = parse_packets(text).unwrap().unwrap();
        assert_eq!(timeline.starts, [0, 1000, 2000, 3000, 5000, 6000]);
        assert_eq!(timeline.keys, [0, 5000]);
        assert_eq!(timeline.shown(2999), 2000);
        assert_eq!(timeline.shown(3000), 3000);
        assert_eq!(
            timeline.shown(4500),
            3000,
            "the discarded packet is skipped"
        );
        assert_eq!(timeline.shown(-10), 0);
        assert_eq!(timeline.shown(99_999), 6000);
        assert_eq!(timeline.keyframe(3000), 0);
        assert_eq!(timeline.keyframe(6000), 5000);
        assert_eq!(parse_packets("N/A,K__\n0,___\n").unwrap(), None);
        // Frames under a microsecond apart: a seek cannot tell them apart,
        // so the earlier is the one asked for, and said.
        let fine = Timeline {
            starts: vec![0, 999_000, 999_900],
            keys: vec![0],
        };
        let nanos = Clock {
            numerator: 1,
            denominator: 1_000_000_000,
            start_us: 0,
        };
        assert_eq!(fine.shown(nanos.tick_at(1)), 999_900);
        assert_eq!(fine.first_in_microsecond(999_900, &nanos), 999_000);
        assert_eq!(fine.first_in_microsecond(0, &nanos), 0);
        assert_eq!(parse_packets("\n").unwrap(), None);
    }

    /// A decode's arguments are fixed but for the stream, the seek and the
    /// size, and its input is the descriptor: no path, no other protocol.
    #[test]
    fn decode_arguments_are_fixed() {
        let args: Vec<String> = frame_args(VideoContainer::WebM, 2, "1.500000", true, 160, 90)
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        assert_eq!(
            args.join(" "),
            format!(
                "-nostdin -hide_banner -loglevel error -protocol_whitelist file -threads 2 \
                 -f matroska -seek_timestamp 1 -ss 1.500000 -i {INPUT} -map 0:2 -an -sn -dn \
                 -frames:v 1 -vf scale=160:90:flags=area,setsar=1 -pix_fmt rgba -f rawvideo pipe:1"
            )
        );
        let by_time = frame_args(VideoContainer::Avi, 0, "2.000000", false, 4, 4);
        assert!(!by_time.iter().any(|arg| arg == "-seek_timestamp"));
        assert_eq!(
            packet_args(
                VideoContainer::IsoBmff,
                0,
                &["1.000000%2.000000".to_string()]
            )
            .join(" "),
            format!(
                "-v error -hide_banner -protocol_whitelist file -f mov -select_streams 0 \
                 -read_intervals 1.000000%2.000000 -show_entries packet=pts,flags -of csv=p=0 {INPUT}"
            )
        );
        assert!(
            !packet_args(VideoContainer::Ogg, 0, &[])
                .iter()
                .any(|arg| arg == "-read_intervals")
        );
    }

    /// A session's decode runs from its start to the end, video only, at
    /// the rate asked, each frame scaled after the rate drops the others.
    #[test]
    fn play_arguments_are_fixed() {
        let args: Vec<String> = play_args(VideoContainer::IsoBmff, 1, "2.500000", 640, 360, 29_970)
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        assert_eq!(
            args.join(" "),
            format!(
                "-nostdin -hide_banner -loglevel error -protocol_whitelist file -threads 2 \
                 -filter_threads 1 -f mov -ss 2.500000 -i {INPUT} -map 0:1 -an -sn -dn \
                 -vf fps=29970/1000:eof_action=pass,scale=640:360:flags=bilinear,setsar=1 -pix_fmt rgba \
                 -threads:v 1 -f rawvideo pipe:1"
            )
        );
    }

    #[test]
    fn frames_are_hurried_only_once_the_pace_would_miss_the_deadline() {
        let ms = Duration::from_millis;
        assert!(!hurried(None, 16, ms(1)), "nothing timed yet");
        assert!(!hurried(Some(ms(100)), 5, ms(500)));
        assert!(hurried(Some(ms(100)), 6, ms(500)));
        assert!(hurried(Some(ms(1)), 1, ms(0)));
        assert!(!hurried(Some(ms(0)), u32::MAX, ms(0)));
    }

    #[test]
    fn the_codec_list_says_what_decodes() {
        let list = " DEV.LS h264    H.264 / AVC\n ..V.L. hevc    H.265\n D.A.L. aac     AAC\n";
        assert!(decodes(list, "h264"));
        assert!(!decodes(list, "hevc"), "encode only");
        assert!(!decodes(list, "vp9"), "absent");
    }
}
