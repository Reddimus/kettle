//! The external decoder: the user's own ffmpeg and ffprobe, never bundled
//! and never linked. Each run is a child of the worker, contained so it
//! starts no process, with a fixed argument list into which only numbers
//! Kettle computed and the demuxer the content was sniffed as are put; its
//! input is a fresh descriptor of the held file (Linux reads it through
//! `/proc/self/fd/0`, macOS through `/dev/fd/0`), only the `file` protocol is
//! allowed, audio, subtitles and data are dropped, and stderr is discarded.
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

use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

use kettle_media::video::{
    DECODER_ENV, DecodedFrame, DecodedStills, StillsPlan, VideoContainer, VideoDecoder, VideoInput,
};
use kettle_media::{
    FailureCode, MAX_VIDEO_FPS_MILLI, MAX_VIDEO_SIDE, StillSample, VideoCodec, VideoInfo,
};

use crate::reopen::reopen;
use crate::run::{Ran, RunError, run};
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
    duration_us: Option<i64>,
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
            "duration" => stream.duration_us = micros(value),
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
         avg_frame_rate,r_frame_rate,time_base,duration:stream_disposition=attached_pic:\
         stream_side_data=rotation:format=start_time,duration",
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

impl VideoDecoder for Ffmpeg {
    fn stills(
        &self,
        input: VideoInput<'_>,
        plan: &mut dyn FnMut(&VideoInfo) -> Result<StillsPlan, FailureCode>,
        deadline: Instant,
    ) -> Result<DecodedStills, FailureCode> {
        let probe = self.probe(&input, deadline)?;
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
            (None, Some(_)) => self.packets(&input, index, &[], deadline)?,
            _ => None,
        };
        let duration_us = match (probe.duration_us, &scanned, clock) {
            (Some(duration), _, _) => duration,
            (None, Some(timeline), Some(clock)) => {
                let last = *timeline.starts.last().unwrap_or(&0);
                (clock.micros(last) - probe.start_us).max(0)
                    + i64::try_from(probe.frame_ms() * 1000).unwrap_or(0)
            }
            _ => return Err(FailureCode::RenderParse),
        };
        let duration_ms = u64::try_from(duration_us / 1000).unwrap_or(0);
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

#[cfg(test)]
mod tests {
    use super::*;

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
