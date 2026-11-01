//! The external decoder end to end. Shell stand-ins for ffmpeg and ffprobe
//! pin what is run and how its output is taken, with no ffmpeg installed;
//! the fixtures pin real decoding when a trusted ffmpeg is (and say they
//! were skipped when it is not). The stand-ins use only shell built-ins: a
//! contained process cannot start another.
#![cfg(unix)]

use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kettle_media::video::{
    DecodedStills, PlayGate, PlayRequest, Playback, StillsPlan, VideoContainer, VideoDecoder,
    VideoInput, VideoPlayer, sniff_video_container,
};
use kettle_media::{AudioRequest, FailureCode, PathIdentity, VideoCodec, VideoInfo};
use kettle_media_native::ffmpeg::Ffmpeg;

mod support;
use support::{frame_index, frame_index_mirrored};

fn later() -> Instant {
    Instant::now() + Duration::from_secs(20)
}

/// A one-second, 40x30, 10 fps H.264 description with a millisecond clock.
const DESCRIPTION: &[&str] = &[
    "streams.stream.0.index=0",
    "streams.stream.0.codec_name=\"h264\"",
    "streams.stream.0.codec_type=\"video\"",
    "streams.stream.0.width=40",
    "streams.stream.0.height=30",
    "streams.stream.0.sample_aspect_ratio=\"1:1\"",
    "streams.stream.0.avg_frame_rate=\"10/1\"",
    "streams.stream.0.time_base=\"1/1000\"",
    "streams.stream.0.duration=\"1.000000\"",
    "streams.stream.0.disposition.attached_pic=0",
    "format.start_time=\"0.000000\"",
    "format.duration=\"1.000000\"",
];
/// Its ten frames, keyframes at 0 and 500 ms, listed out of order as B-frames are.
const PACKETS: &[&str] = &[
    "0,K__", "200,___", "100,___", "300,___", "400,___", "500,K__", "700,___", "600,___",
    "800,___", "900,___",
];

/// Single-quote each line for `printf '%s\n'`.
fn quoted(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|line| format!("'{line}'"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A directory holding stand-in `ffprobe` and `ffmpeg` scripts that log
/// their arguments beside themselves.
struct Fake {
    directory: kettle_test_support::PrivateTempDir,
}

/// How the stand-in ffmpeg answers.
#[derive(Clone, Copy)]
struct Answer {
    /// Frames a session's run writes, before any shortfall.
    frames: u32,
    /// Pixels short of the size asked for (negative: over).
    short: i64,
    /// A byte written past the frame.
    trailing: bool,
    status: u8,
    /// Killed by a signal once its frames are written.
    killed: bool,
    /// Whether its codec list says it decodes H.264.
    decodes: bool,
    /// Never answers.
    hangs: bool,
    /// Its codec list fails, empty.
    codecs_fail: bool,
    /// Its codec list never comes.
    codecs_hang: bool,
}

const GOOD: Answer = Answer {
    frames: 1,
    short: 0,
    trailing: false,
    status: 0,
    killed: false,
    decodes: true,
    hangs: false,
    codecs_fail: false,
    codecs_hang: false,
};

fn write_script(path: &Path, contents: &[u8]) {
    // Decoder forks must not inherit a writable script descriptor (ETXTBSY).
    // Publish a fresh inode only after its separate writer has exited.
    let staging = tempfile::Builder::new()
        .prefix(".kettle-script-")
        .tempdir_in(path.parent().unwrap())
        .unwrap();
    let script = staging.path().join("script");
    let mut writer = Command::new("/bin/sh")
        .args(["-c", "/bin/cat > \"$1\"", "fixture-writer"])
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let written = writer.stdin.take().unwrap().write_all(contents);
    let output = writer.wait_with_output().unwrap();
    written.unwrap();
    assert!(output.status.success(), "script writer failed: {output:?}");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(script, path).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn executable_replacement_is_not_held_by_an_old_writer() {
    let directory = kettle_test_support::private_tempdir("kettle-script-replace-");
    let path = directory.path().join("script");
    write_script(&path, b"#!/bin/sh\nexit 1\n");
    let old_writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    write_script(&path, b"#!/bin/sh\nexit 0\n");
    assert!(
        std::process::Command::new(&path)
            .status()
            .unwrap()
            .success()
    );
    drop(old_writer);
}

impl Fake {
    fn new(description: &[&str], packets: &[&str], answer: Answer) -> Self {
        let directory = kettle_test_support::private_tempdir("kettle-ffmpeg-fixture-");
        let log = |name: &str| format!("printf '%s\\n' \"$@\" '--' >> \"${{0%/*}}/{name}.log\"");
        let ffprobe = format!(
            r#"{}
case "$*" in
    *packet=pts_time,dts_time:*) printf '%s\n' 'packets.packet.0.pts_time="0.000000"' ;;
    *packet=*) printf '%s\n' {} ;;
    *) printf '%s\n' {} ;;
esac
"#,
            log("ffprobe"),
            quoted(packets),
            quoted(description),
        );
        let codecs = match (answer.codecs_fail, answer.decodes) {
            _ if answer.codecs_hang => "exec /bin/sleep 30",
            (true, _) => "exit 1",
            (false, true) => "printf '%s\\n' ' DEV.LS h264 H.264'; exit 0",
            (false, false) => "printf '%s\\n' ' ..V.L. h264 H.264'; exit 0",
        };
        let ffmpeg = if answer.hangs {
            format!("{}\nexec /bin/sleep 30", log("ffmpeg"))
        } else {
            format!(
                "{log}\nulimit -t >> \"${{0%/*}}/ffmpeg.cpu\"\n\
                 case \"$*\" in *-codecs*) {codecs} ;; esac\n\
                 w=0; h=0; f=1\n\
                 for arg in \"$@\"; do case \"$arg\" in fps=*) f={frames}; arg=${{arg#*,}} ;; esac; case \"$arg\" in scale=*) v=${{arg#scale=}}; w=${{v%%:*}}; v=${{v#*:}}; h=${{v%%:*}} ;; esac; done\n\
                 n=$((f * w * h - {short})); i=0\n\
                 while [ \"$i\" -lt \"$n\" ]; do printf '\\377\\000\\000\\377'; i=$((i + 1)); done\n\
                 {trailing}{killed}exit {status}",
                log = log("ffmpeg"),
                frames = answer.frames,
                short = answer.short,
                trailing = if answer.trailing { "printf x\n" } else { "" },
                killed = if answer.killed { "kill -KILL $$\n" } else { "" },
                status = answer.status,
            )
        };
        for (name, body) in [("ffprobe", ffprobe), ("ffmpeg", ffmpeg)] {
            let path = directory.path().join(name);
            write_script(&path, format!("#!/bin/sh\n{body}\n").as_bytes());
        }
        Self { directory }
    }

    fn decoder(&self) -> Ffmpeg {
        Ffmpeg::trust(&self.directory.path().join("ffmpeg")).unwrap()
    }

    fn audio_probe(&self, answer: &str) {
        let executable = self.directory.path().join("ffprobe");
        let script = std::fs::read_to_string(&executable).unwrap();
        let branch = script
            .lines()
            .find(|line| line.contains("*packet=pts_time,dts_time:*)"))
            .unwrap();
        let replacement = format!("    *packet=pts_time,dts_time:*) {answer} ;;");
        write_script(
            &executable,
            script.replacen(branch, &replacement, 1).as_bytes(),
        );
    }

    /// Each run's arguments, in order.
    fn calls(&self, name: &str) -> Vec<Vec<String>> {
        let log = std::fs::read_to_string(self.directory.path().join(format!("{name}.log")))
            .unwrap_or_default();
        log.split("--\n")
            .filter(|call| !call.is_empty())
            .map(|call| call.lines().map(str::to_string).collect())
            .collect()
    }
}

/// A held file whose first bytes say MP4, as the renderer holds one.
struct Clip {
    _directory: kettle_test_support::PrivateTempDir,
    path: PathBuf,
    file: File,
    identity: PathIdentity,
    container: VideoContainer,
}

impl Clip {
    fn mp4() -> Self {
        let mut bytes = vec![0, 0, 0, 24];
        bytes.extend_from_slice(b"ftypisom\0\0\x02\0isomiso2");
        bytes.extend_from_slice(&[0; 64]);
        Self::of(&bytes)
    }

    fn of(bytes: &[u8]) -> Self {
        let directory = kettle_test_support::private_tempdir("kettle-ffmpeg-fixture-");
        let path = directory.path().join("clip");
        std::fs::write(&path, bytes).unwrap();
        Self::open(directory, path)
    }

    fn open(directory: kettle_test_support::PrivateTempDir, path: PathBuf) -> Self {
        let file = File::open(&path).unwrap();
        let identity = PathIdentity::of(&file.metadata().unwrap()).unwrap();
        let prefix = std::fs::read(&path).unwrap();
        let container = sniff_video_container(&prefix, prefix.len() as u64).unwrap();
        Self {
            _directory: directory,
            path,
            file,
            identity,
            container,
        }
    }

    fn input(&self) -> VideoInput<'_> {
        VideoInput {
            file: &self.file,
            identity: self.identity,
            container: self.container,
        }
    }
}

/// Take `times` at `width` by `height`, keeping what the decoder said the
/// video is.
fn take(
    decoder: &Ffmpeg,
    clip: &Clip,
    times: &[u64],
    size: (u32, u32),
    deadline: Instant,
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
        deadline,
    );
    (result, seen)
}

fn arg_after<'a>(call: &'a [String], flag: &str) -> Option<&'a str> {
    call.iter()
        .position(|arg| arg == flag)
        .and_then(|at| call.get(at + 1))
        .map(String::as_str)
}

/// Each instant takes the frame showing at it, decoded by that frame's own
/// timestamp, so its time is the frame's start; the packets are read only
/// around the instants; the video is described as ffprobe says.
#[test]
fn stills_take_the_frame_showing_by_its_own_timestamp() {
    let fake = Fake::new(DESCRIPTION, PACKETS, GOOD);
    let clip = Clip::mp4();
    let (result, seen) = take(
        &fake.decoder(),
        &clip,
        &[125, 375, 625, 875],
        (8, 6),
        later(),
    );
    let stills = result.unwrap();
    let info = seen.unwrap();
    assert_eq!(
        info,
        VideoInfo {
            duration_ms: 1000,
            width: 40,
            height: 30,
            rotation: 0,
            codec: VideoCodec::H264,
            fps_milli: Some(10_000),
            has_audio: false,
            container: Some(VideoContainer::IsoBmff),
        }
    );
    assert_eq!(stills.info, info);
    assert_eq!(
        stills
            .frames
            .iter()
            .map(|frame| (frame.sample.requested_ms, frame.sample.actual_ms))
            .collect::<Vec<_>>(),
        [(125, 100), (375, 300), (625, 600), (875, 800)]
    );
    assert_eq!(stills.tolerance_ms, 75);
    assert!(
        stills
            .frames
            .iter()
            .all(|frame| frame.rgba == [255, 0, 0, 255].repeat(48))
    );

    let probes = fake.calls("ffprobe");
    assert_eq!(probes.len(), 2, "a description, then the packets");
    assert_eq!(
        arg_after(&probes[1], "-read_intervals"),
        Some("0.125000%3.325000,0.375000%3.575000,0.625000%3.825000,0.875000%4.075000"),
        "32 frames past each instant"
    );
    let decodes = fake.calls("ffmpeg");
    assert_eq!(
        decodes
            .iter()
            .map(|call| arg_after(call, "-ss").unwrap())
            .collect::<Vec<_>>(),
        ["0.100000", "0.300000", "0.600000", "0.800000"]
    );
    for call in &decodes {
        assert_eq!(arg_after(call, "-seek_timestamp"), Some("1"));
        assert_eq!(
            arg_after(call, "-vf"),
            Some("scale=8:6:flags=area,setsar=1")
        );
        assert_eq!(arg_after(call, "-f"), Some("mov"));
        assert_eq!(arg_after(call, "-protocol_whitelist"), Some("file"));
        let input = arg_after(call, "-i").unwrap();
        assert!(input == "file:/proc/self/fd/0" || input == "file:/dev/fd/0");
    }
}

/// Instants showing the same frame decode it once.
#[test]
fn one_frame_is_decoded_once() {
    let fake = Fake::new(DESCRIPTION, PACKETS, GOOD);
    let clip = Clip::mp4();
    let (result, _) = take(&fake.decoder(), &clip, &[10, 50, 99, 150], (4, 3), later());
    let stills = result.unwrap();
    assert_eq!(
        stills
            .frames
            .iter()
            .map(|frame| frame.sample.actual_ms)
            .collect::<Vec<_>>(),
        [0, 0, 0, 100]
    );
    assert_eq!(fake.calls("ffmpeg").len(), 2);
}

/// Output a byte short or over, or a failing exit, is no frame: the stream
/// is at fault when ffmpeg decodes its codec, and the codec is missing when
/// it does not.
#[test]
fn misframed_output_is_refused() {
    let clip = Clip::mp4();
    for (answer, failure) in [
        (Answer { short: 1, ..GOOD }, FailureCode::RenderParse),
        (
            Answer {
                trailing: true,
                ..GOOD
            },
            FailureCode::RenderParse,
        ),
        (Answer { short: -1, ..GOOD }, FailureCode::RenderParse),
        (Answer { status: 1, ..GOOD }, FailureCode::RenderParse),
        (
            Answer {
                short: 1,
                decodes: false,
                ..GOOD
            },
            FailureCode::CodecUnavailable,
        ),
        // A codec list that fails says nothing about the codec.
        (
            Answer {
                short: 1,
                decodes: false,
                codecs_fail: true,
                ..GOOD
            },
            FailureCode::RenderParse,
        ),
    ] {
        let fake = Fake::new(DESCRIPTION, PACKETS, answer);
        let (result, _) = take(&fake.decoder(), &clip, &[125], (8, 6), later());
        assert_eq!(result.unwrap_err(), failure);
    }
}

/// At one frame a second the packets are read 32 seconds past an instant,
/// so frames a reordered stream stores late are still seen.
#[test]
fn slow_streams_read_further_past_each_instant() {
    let slow: Vec<String> = DESCRIPTION
        .iter()
        .map(|line| line.replace("\"10/1\"", "\"1/1\""))
        .collect();
    let slow: Vec<&str> = slow.iter().map(String::as_str).collect();
    let fake = Fake::new(&slow, PACKETS, GOOD);
    let clip = Clip::mp4();
    let (result, _) = take(&fake.decoder(), &clip, &[500], (8, 6), later());
    result.unwrap();
    assert_eq!(
        arg_after(&fake.calls("ffprobe")[1], "-read_intervals"),
        Some("0.500000%32.500000")
    );
}

/// A codec list still coming at the deadline makes the failure a timeout,
/// not a guess about the codec.
#[test]
fn an_explanation_past_the_deadline_is_a_timeout() {
    let fake = Fake::new(
        DESCRIPTION,
        PACKETS,
        Answer {
            short: 1,
            codecs_hang: true,
            ..GOOD
        },
    );
    let clip = Clip::mp4();
    let started = Instant::now();
    let (result, _) = take(
        &fake.decoder(),
        &clip,
        &[125],
        (8, 6),
        started + Duration::from_millis(1500),
    );
    assert_eq!(result.unwrap_err(), FailureCode::RenderTimeout);
    assert!(started.elapsed() < Duration::from_secs(6));
}

/// A decoder still running at the deadline is stopped there.
#[test]
fn a_decoder_past_its_deadline_is_stopped() {
    let fake = Fake::new(
        DESCRIPTION,
        PACKETS,
        Answer {
            hangs: true,
            ..GOOD
        },
    );
    let clip = Clip::mp4();
    let started = Instant::now();
    let (result, _) = take(
        &fake.decoder(),
        &clip,
        &[125],
        (8, 6),
        started + Duration::from_millis(700),
    );
    assert_eq!(result.unwrap_err(), FailureCode::RenderTimeout);
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// With no duration, the whole stream's packets are listed and give its
/// length; with no video it is unsupported; a failing ffprobe is the
/// stream's fault.
#[test]
fn descriptions_without_a_duration_or_video() {
    let undated: Vec<&str> = DESCRIPTION
        .iter()
        .copied()
        .filter(|line| !line.contains("duration"))
        .collect();
    let fake = Fake::new(&undated, PACKETS, GOOD);
    let clip = Clip::mp4();
    let (result, seen) = take(&fake.decoder(), &clip, &[500], (8, 6), later());
    assert_eq!(result.unwrap().frames[0].sample.actual_ms, 500);
    assert_eq!(
        seen.unwrap().duration_ms,
        1000,
        "the last frame and its length"
    );
    let probes = fake.calls("ffprobe");
    assert_eq!(probes.len(), 2);
    assert_eq!(arg_after(&probes[1], "-read_intervals"), None);

    let audio: Vec<String> = DESCRIPTION
        .iter()
        .map(|line| line.replace("\"video\"", "\"audio\""))
        .collect();
    let audio: Vec<&str> = audio.iter().map(String::as_str).collect();
    let fake = Fake::new(&audio, PACKETS, GOOD);
    let (result, _) = take(&fake.decoder(), &clip, &[500], (8, 6), later());
    assert_eq!(result.unwrap_err(), FailureCode::UnsupportedMedia);

    let fake = Fake::new(DESCRIPTION, PACKETS, GOOD);
    write_script(
        &fake.directory.path().join("ffprobe"),
        b"#!/bin/sh\nexit 1\n",
    );
    let decoder = Ffmpeg::trust(&fake.directory.path().join("ffmpeg"));
    let (result, _) = take(&decoder.unwrap(), &clip, &[500], (8, 6), later());
    assert_eq!(result.unwrap_err(), FailureCode::RenderParse);
}

#[test]
fn inferred_durations_reject_extreme_packet_timestamps() {
    let undated: Vec<&str> = DESCRIPTION
        .iter()
        .copied()
        .filter(|line| !line.contains("duration"))
        .collect();
    let clip = Clip::mp4();
    for last in ["9223372036854775,K__", "9223372036854775807,K__"] {
        let fake = Fake::new(&undated, &[last], GOOD);
        let (result, seen) = take(&fake.decoder(), &clip, &[0], (8, 6), later());
        assert_eq!(result.unwrap_err(), FailureCode::RenderParse);
        assert_eq!(seen, None);
        assert!(fake.calls("ffmpeg").is_empty());
    }
}

/// Packets without timestamps leave seeking by time: each frame stands for
/// its instant within one frame's length.
#[test]
fn untimed_streams_seek_by_time() {
    let fake = Fake::new(DESCRIPTION, &["N/A,K__", "N/A,___"], GOOD);
    let clip = Clip::mp4();
    let (result, _) = take(&fake.decoder(), &clip, &[125, 625], (8, 6), later());
    let stills = result.unwrap();
    assert_eq!(
        stills
            .frames
            .iter()
            .map(|frame| frame.sample.actual_ms)
            .collect::<Vec<_>>(),
        [125, 625]
    );
    assert_eq!(stills.tolerance_ms, 100);
    let decodes = fake.calls("ffmpeg");
    assert_eq!(arg_after(&decodes[0], "-ss"), Some("0.125000"));
    assert!(
        decodes
            .iter()
            .all(|call| !call.contains(&"-seek_timestamp".to_string()))
    );
}

/// A decoder others could have replaced is refused, as is one without an
/// ffprobe beside it, and a binary changed after it was trusted is not run;
/// a held file changed in place is refused before any decoder reads it.
#[test]
fn untrusted_decoders_and_changed_files_are_refused() {
    let fake = Fake::new(DESCRIPTION, PACKETS, GOOD);
    let ffmpeg = fake.directory.path().join("ffmpeg");
    std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o775)).unwrap();
    assert!(Ffmpeg::trust(&ffmpeg).is_err());
    std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
    let decoder = Ffmpeg::trust(&ffmpeg).unwrap();
    std::fs::remove_file(fake.directory.path().join("ffprobe")).unwrap();
    assert!(Ffmpeg::trust(&ffmpeg).is_err());
    let clip = Clip::mp4();
    let (result, _) = take(&decoder, &clip, &[125], (8, 6), later());
    assert_eq!(result.unwrap_err(), FailureCode::BackendUnavailable);

    let fake = Fake::new(DESCRIPTION, PACKETS, GOOD);
    let clip = Clip::mp4();
    std::thread::sleep(Duration::from_millis(5));
    std::fs::write(&clip.path, b"rewritten in place").unwrap();
    let (result, _) = take(&fake.decoder(), &clip, &[125], (8, 6), later());
    assert_eq!(result.unwrap_err(), FailureCode::Changed);
    assert!(fake.calls("ffprobe").is_empty());
}

fn fixture(name: &str) -> PathBuf {
    std::env::var_os("KETTLE_TEST_VIDEO_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video"))
        .join(name)
}

fn installed() -> Option<Ffmpeg> {
    let found = Ffmpeg::search(std::env::var_os("HOME").as_deref().map(Path::new));
    if found.is_none() {
        eprintln!("skipped: no trusted ffmpeg is installed");
    }
    found
}

/// A real ffmpeg returns the frame showing at each instant, not the one
/// after it, in MP4, WebM, a rotated MP4 (shown upright) and a WebM with no
/// duration (measured from its packets).
#[test]
fn real_ffmpeg_takes_the_frames_showing() {
    let Some(decoder) = installed() else {
        return;
    };
    for (name, shown, size) in [
        ("index.mp4", (64, 36), (32, 18)),
        ("index.webm", (64, 36), (32, 18)),
        ("rotated.mp4", (36, 64), (18, 32)),
        ("unsized.webm", (64, 36), (32, 18)),
    ] {
        let directory = kettle_test_support::private_tempdir("kettle-ffmpeg-fixture-");
        let path = directory.path().join(name);
        std::fs::copy(fixture(name), &path).unwrap();
        let clip = Clip::open(directory, path);
        let (result, seen) = take(&decoder, &clip, &[666, 2000, 3333, 3999], size, later());
        let stills = result.unwrap_or_else(|failure| panic!("{name}: {failure:?}"));
        let info = seen.unwrap();
        assert_eq!((info.width, info.height), shown, "{name}");
        assert_eq!(info.duration_ms, 4000, "{name}");
        assert_eq!(info.fps_milli, Some(10_000), "{name}");
        assert_eq!(
            info.rotation,
            if name == "rotated.mp4" { 270 } else { 0 },
            "{name}"
        );
        assert_eq!(
            stills
                .frames
                .iter()
                .map(|frame| frame.sample.actual_ms)
                .collect::<Vec<_>>(),
            [600, 2000, 3300, 3900],
            "{name}"
        );
        assert_eq!(
            stills
                .frames
                .iter()
                .map(|frame| frame_index(&frame.rgba, size.0, size.1, name == "rotated.mp4"))
                .collect::<Vec<_>>(),
            [6, 20, 33, 39],
            "{name}"
        );
        assert!(
            stills
                .frames
                .iter()
                .all(|frame| frame.rgba.len() == (size.0 * size.1 * 4) as usize)
        );
    }
}

/// A real ffmpeg from 7.1 draws a HEIF and an AVIF still from the file it
/// was read from, at no more than the size asked, in its own colour
/// (`fixtures/still/README.md`), and never enlarged; without the file it
/// reads no still.
#[test]
fn real_ffmpeg_draws_heif_and_avif_stills() {
    use kettle_media::video::{StillImage, StillInput};
    let Some(decoder) = installed() else {
        return;
    };
    for (name, still, source) in [
        ("solid.heic", StillImage::Heif, (640, 480)),
        ("solid.avif", StillImage::Avif, (640, 480)),
        ("grid.heic", StillImage::Heif, (1024, 768)),
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/still")
            .join(name);
        let bytes = std::fs::read(&path).unwrap();
        let file = File::open(&path).unwrap();
        let identity = PathIdentity::of(&file.metadata().unwrap()).unwrap();
        let input = StillInput {
            bytes: &bytes,
            file: Some((&file, identity)),
        };
        let drawn = match decoder.still_image(input, still, 64, later()) {
            Ok(drawn) => drawn,
            Err(FailureCode::CodecUnavailable) => {
                eprintln!("skipped {name}: this ffmpeg reads no stills");
                continue;
            }
            Err(failure) => panic!("{name}: {failure:?}"),
        };
        assert_eq!((drawn.width, drawn.height), (64, 48), "{name}");
        assert_eq!((drawn.source_width, drawn.source_height), source, "{name}");
        let centre = &drawn.rgba[(24 * 64 + 32) * 4..][..4];
        assert!(
            centre[0].abs_diff(230) <= 4
                && centre[1].abs_diff(40) <= 4
                && centre[2].abs_diff(20) <= 4,
            "{name}: {centre:?}"
        );
        let whole = decoder
            .still_image(input, still, 4096, later())
            .unwrap_or_else(|failure| panic!("{name}: {failure:?}"));
        assert_eq!((whole.width, whole.height), source, "never enlarged");
        let bare = StillInput {
            bytes: &bytes,
            file: None,
        };
        assert_eq!(
            decoder.still_image(bare, still, 64, later()).unwrap_err(),
            FailureCode::CodecUnavailable
        );
    }
}

/// A real ffmpeg on a clip cut short fails as the stream's fault.
#[test]
fn real_ffmpeg_refuses_a_truncated_clip() {
    let Some(decoder) = installed() else {
        return;
    };
    let bytes = std::fs::read(fixture("index.mp4")).unwrap();
    let clip = Clip::of(&bytes[..bytes.len() / 2]);
    let (result, _) = take(&decoder, &clip, &[3500], (32, 18), later());
    assert!(
        matches!(
            result,
            Err(FailureCode::RenderParse) | Err(FailureCode::UnsupportedMedia)
        ),
        "{result:?}"
    );
}

/// A decoder's sandbox needs both of its programs and each one's own library
/// tree: ffprobe may come from another package than ffmpeg.
#[test]
fn each_program_brings_its_own_library_tree() {
    let root = kettle_test_support::private_tempdir("kettle-ffmpeg-trees-");
    let root = std::fs::canonicalize(root.path()).unwrap();
    for (package, program) in [("a", "ffmpeg"), ("b", "ffprobe")] {
        let bin = root.join(package).join("bin");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(&bin)
            .unwrap();
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(root.join(package).join("lib"))
            .unwrap();
        let path = bin.join(program);
        write_script(&path, b"#!/bin/sh\nexit 0\n");
    }
    let named = root.join("named");
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(&named)
        .unwrap();
    std::os::unix::fs::symlink(root.join("a/bin/ffmpeg"), named.join("ffmpeg")).unwrap();
    std::os::unix::fs::symlink(root.join("b/bin/ffprobe"), named.join("ffprobe")).unwrap();
    let ffmpeg = Ffmpeg::trust(&named.join("ffmpeg")).expect("trusted");
    let (programs, trees) = kettle_media_native::Decoders::external(Some(ffmpeg)).sandbox_needs();
    assert_eq!(
        programs,
        [root.join("a/bin/ffmpeg"), root.join("b/bin/ffprobe")]
    );
    assert_eq!(trees, [root.join("a/lib"), root.join("b/lib")]);
}

/// Open `clip` for a session within `room`, from `start_ms`, at most
/// `max_fps_milli`.
fn play(
    decoder: &dyn VideoPlayer,
    clip: &Clip,
    room: (u32, u32),
    start_ms: u64,
    max_fps_milli: u32,
) -> Result<Playback, FailureCode> {
    decoder.play(
        clip.input(),
        PlayRequest {
            max_width: room.0,
            max_height: room.1,
            start_ms,
            max_fps_milli,
            audio: AudioRequest::On,
        },
        later(),
    )
}

/// Every frame of an opened session to its end: each one's time and pixels.
fn frames_of(playback: &mut Playback) -> Result<Vec<(u64, Vec<u8>)>, FailureCode> {
    let audio = playback.audio.take().map(|mut source| {
        std::thread::spawn(move || {
            let mut samples = vec![0.0; 1920];
            while source.next_block(&mut samples).unwrap().is_some() {}
        })
    });
    let size = playback.opened.width as usize * playback.opened.height as usize * 4;
    let gate = PlayGate::default();
    gate.play();
    let mut frames = Vec::new();
    loop {
        let mut rgba = vec![0; size];
        match playback.frames.next_frame(&mut rgba, &gate)? {
            Some(pts) => frames.push((pts, rgba)),
            None => {
                if let Some(audio) = audio {
                    audio.join().unwrap();
                }
                return Ok(frames);
            }
        }
        assert!(frames.len() <= 1000, "a session that never ends");
    }
}

/// A session's run is described first, then decodes from its start at the
/// rate asked, fitted to the room and never enlarged; its frames are timed
/// from that start, its output ending at a frame's edge ends the video, and
/// it runs within a processor-time limit for the video it has left.
#[test]
fn a_session_decodes_from_its_start_at_its_rate() {
    let fake = Fake::new(DESCRIPTION, PACKETS, Answer { frames: 4, ..GOOD });
    let clip = Clip::mp4();
    let mut playback = play(&fake.decoder(), &clip, (20, 20), 300, 5_000).unwrap();
    let opened = playback.opened;
    assert_eq!(
        (
            opened.width,
            opened.height,
            opened.fps_milli,
            opened.start_ms
        ),
        (20, 15, 5_000, 300)
    );
    assert_eq!(opened.info.duration_ms, 1000);
    let frames = frames_of(&mut playback).unwrap();
    assert_eq!(
        frames.iter().map(|(pts, _)| *pts).collect::<Vec<_>>(),
        // 300 ms on, a fifth of a second apart, held to the video's length.
        [300, 500, 700, 900]
    );
    assert!(frames.iter().all(|(_, rgba)| rgba.len() == 20 * 15 * 4));
    // Read once that run has ended, before another starts.
    let call = fake.calls("ffmpeg").pop().unwrap();
    assert_eq!(arg_after(&call, "-ss"), Some("0.300000"));
    assert_eq!(
        arg_after(&call, "-vf"),
        Some("fps=5000/1000:eof_action=pass,scale=20:15:flags=bilinear,setsar=1")
    );
    // Half a minute and four seconds for the one second left.
    let cpu = std::fs::read_to_string(fake.directory.path().join("ffmpeg.cpu")).unwrap();
    assert_eq!(cpu.trim(), "34");
    // A buffer of another size is refused, never read as a frame's worth.
    let mut playback = play(&fake.decoder(), &clip, (20, 20), 300, 5_000).unwrap();
    let gate = PlayGate::default();
    for wrong in [0, 20 * 15 * 2, 20 * 15 * 4 + 1] {
        assert_eq!(
            playback.frames.next_frame(&mut vec![0; wrong], &gate),
            Err(FailureCode::BadParams)
        );
    }

    // A video's own rate is kept when it is slower, its own size when it
    // is smaller, and a start past the last frame shows the last frame.
    let mut playback = play(&fake.decoder(), &clip, (1920, 1080), 5_000, 30_000).unwrap();
    let opened = playback.opened;
    assert_eq!(
        (
            opened.width,
            opened.height,
            opened.fps_milli,
            opened.start_ms
        ),
        (40, 30, 10_000, 900)
    );
    // Frames past the end, which a real ffmpeg does not write, are held to
    // the video's length.
    assert_eq!(
        frames_of(&mut playback)
            .unwrap()
            .iter()
            .map(|(pts, _)| *pts)
            .collect::<Vec<_>>(),
        [900, 1000, 1000, 1000]
    );

    // Sound that runs two seconds past the video: a start past the video's
    // last frame still shows that frame.
    let longer: Vec<&str> = DESCRIPTION
        .iter()
        .map(|line| {
            if line.starts_with("format.duration=") {
                "format.duration=\"3.000000\""
            } else {
                line
            }
        })
        .collect();
    let fake = Fake::new(&longer, PACKETS, GOOD);
    let mut playback = play(&fake.decoder(), &clip, (40, 30), 2_500, 10_000).unwrap();
    assert_eq!(
        (playback.opened.info.duration_ms, playback.opened.start_ms),
        (3000, 900)
    );
    assert_eq!(frames_of(&mut playback).unwrap().len(), 1);
    let call = fake.calls("ffmpeg").pop().unwrap();
    assert_eq!(arg_after(&call, "-ss"), Some("0.900000"));
}

#[test]
fn a_session_keeps_video_when_first_audio_timestamps_are_unusable() {
    let mut description = DESCRIPTION.to_vec();
    description.push("streams.stream.1.codec_type=\"audio\"");
    for answer in [
        "exit 1",
        "printf '%s\\n' 'packets.packet.0.pts_time=\"0.000000\"'; printf '%1025s' ''",
        "printf '%s\\n' 'packets.packet.0.pts_time=\"N/A\"'",
        "printf '%s\\n' 'packets.packet.0.pts_time=\"2.000000\"'",
    ] {
        let fake = Fake::new(&description, PACKETS, Answer { frames: 4, ..GOOD });
        fake.audio_probe(answer);
        let clip = Clip::mp4();
        let mut playback = play(&fake.decoder(), &clip, (20, 20), 300, 10_000).unwrap();
        assert_eq!(
            playback.opened.audio,
            kettle_media::AudioStatus::Unsupported
        );
        assert!(playback.audio.is_none());
        let frames = frames_of(&mut playback).unwrap();
        assert_eq!(
            frames.iter().map(|(pts, _)| *pts).collect::<Vec<_>>(),
            [300, 400, 500, 600]
        );
        let calls = fake.calls("ffmpeg");
        assert_eq!(calls.len(), 1, "{answer}");
        assert!(calls[0].iter().any(|arg| arg == "-an"));
    }
}

#[test]
fn a_slow_audio_probe_leaves_time_for_video_fallback() {
    let mut description = DESCRIPTION.to_vec();
    description.push("streams.stream.1.codec_type=\"audio\"");
    for within in [Duration::from_secs(2), Duration::from_secs(8)] {
        let fake = Fake::new(&description, PACKETS, Answer { frames: 4, ..GOOD });
        fake.audio_probe("exec /bin/sleep 30");
        let clip = Clip::mp4();
        let mut playback = fake
            .decoder()
            .play(
                clip.input(),
                PlayRequest {
                    max_width: 20,
                    max_height: 20,
                    start_ms: 300,
                    max_fps_milli: 10_000,
                    audio: AudioRequest::On,
                },
                Instant::now() + within,
            )
            .expect("an optional audio scan must not prevent seeking the video");
        assert_eq!(
            playback.opened.audio,
            kettle_media::AudioStatus::Unsupported
        );
        assert!(playback.audio.is_none());
        assert_eq!(
            frames_of(&mut playback)
                .unwrap()
                .iter()
                .map(|(pts, _)| *pts)
                .collect::<Vec<_>>(),
            [300, 400, 500, 600]
        );
        let calls = fake.calls("ffmpeg");
        assert_eq!(calls.len(), 1);
        assert_eq!(arg_after(&calls[0], "-ss"), Some("0.300000"));
        assert!(calls[0].iter().any(|arg| arg == "-an"));
    }
}

#[test]
fn a_session_retries_video_when_audio_conversion_is_unavailable() {
    let mut description = DESCRIPTION.to_vec();
    description.push("streams.stream.1.codec_type=\"audio\"");
    let fake = Fake::new(&description, PACKETS, Answer { frames: 4, ..GOOD });
    let executable = fake.directory.path().join("ffmpeg");
    let script = std::fs::read_to_string(&executable).unwrap();
    write_script(
        &executable,
        script
            .replacen(
                "#!/bin/sh\n",
                r#"#!/bin/sh
case "$*" in
    *0:a:0*)
        printf '%s\n' "$@" '--' >> "${0%/*}/audio-attempt.log"
        printf '%s\n' "$$" >> "${0%/*}/audio-attempt.pid"
        while [ ! -s "${0%/*}/video-attempt.pid" ]; do :; done
        exit 1
        ;;
    *tee*)
        printf '%s\n' "$@" '--' >> "${0%/*}/video-attempt.log"
        printf '%s\n' "$$" >> "${0%/*}/video-attempt.pid"
        printf '%s\n' '#tb 0: 1/10' '#media_type 0: video' '#codec_id 0: rawvideo' '0, 0, 0, 1, 1200, 0x00000000'
        while :; do :; done
        ;;
esac
"#,
                1,
            )
            .as_bytes(),
    );
    let clip = Clip::mp4();
    let mut playback = play(&fake.decoder(), &clip, (20, 20), 300, 10_000).unwrap();
    assert!(playback.opened.info.has_audio);
    assert_eq!(
        playback.opened.audio,
        kettle_media::AudioStatus::Unsupported
    );
    assert!(playback.audio.is_none());
    let frames = frames_of(&mut playback).unwrap();
    assert_eq!(
        frames.iter().map(|(pts, _)| *pts).collect::<Vec<_>>(),
        [300, 400, 500, 600]
    );
    assert!(frames.iter().all(|(_, rgba)| {
        rgba.len() == 20 * 15 * 4
            && rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [255, 0, 0, 255])
    }));
    let calls = fake.calls("ffmpeg");
    assert_eq!(calls.len(), 1);
    assert!(calls[0].iter().any(|arg| arg == "-an"));
    assert_eq!(arg_after(&calls[0], "-ss"), Some("0.300000"));
    let audio_call =
        std::fs::read_to_string(fake.directory.path().join("audio-attempt.log")).unwrap();
    let video_call =
        std::fs::read_to_string(fake.directory.path().join("video-attempt.log")).unwrap();
    assert!(audio_call.lines().any(|arg| arg == "0:a:0"));
    assert!(video_call.lines().any(|arg| arg == "-an"));
    assert!(audio_call.lines().any(|arg| arg == "tee"));
    assert!(video_call.lines().any(|arg| arg == "tee"));
    let pids = ["audio-attempt.pid", "video-attempt.pid"]
        .map(|name| std::fs::read_to_string(fake.directory.path().join(name)).unwrap());
    for pid in &pids {
        let pid: libc::pid_t = pid.trim().parse().unwrap();
        let mut status = 0;
        // SAFETY: status is valid for the call; WNOHANG never signals a process.
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        assert_eq!(waited, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}

/// A session's run that ends inside a frame is a misframed stream; one that
/// gives no frame at all is explained, by its codec missing from this
/// ffmpeg or by the stream; one that fails after whole frames fails the
/// stream, and one killed by a signal then is out of resources.
#[test]
fn a_session_without_whole_frames_fails() {
    let clip = Clip::mp4();
    for (answer, failure) in [
        (
            Answer {
                frames: 2,
                status: 1,
                ..GOOD
            },
            FailureCode::RenderParse,
        ),
        (
            Answer {
                frames: 2,
                killed: true,
                ..GOOD
            },
            FailureCode::RenderResource,
        ),
    ] {
        let fake = Fake::new(DESCRIPTION, PACKETS, answer);
        let mut playback = play(&fake.decoder(), &clip, (40, 30), 0, 10_000).unwrap();
        assert_eq!(frames_of(&mut playback), Err(failure));
    }
    let short = Fake::new(
        DESCRIPTION,
        PACKETS,
        Answer {
            frames: 2,
            short: 1,
            ..GOOD
        },
    );
    let mut playback = play(&short.decoder(), &clip, (40, 30), 0, 10_000).unwrap();
    assert_eq!(frames_of(&mut playback), Err(FailureCode::RenderParse));
    for (decodes, failure) in [
        (false, FailureCode::CodecUnavailable),
        (true, FailureCode::RenderParse),
    ] {
        let none = Fake::new(
            DESCRIPTION,
            PACKETS,
            Answer {
                frames: 0,
                status: 1,
                decodes,
                ..GOOD
            },
        );
        let mut playback = play(&none.decoder(), &clip, (40, 30), 0, 10_000).unwrap();
        assert_eq!(frames_of(&mut playback), Err(failure), "decodes: {decodes}");
    }
}

/// A real ffmpeg started past the video's end, in a file whose sound runs
/// six seconds longer, shows the video's last frame, at the video's rate or
/// one far slower.
#[test]
fn real_ffmpeg_shows_the_last_frame_of_a_video_its_sound_outlasts() {
    let Some(decoder) = installed() else {
        return;
    };
    for max_fps_milli in [10_000, 1_000] {
        let directory = kettle_test_support::private_tempdir("kettle-ffmpeg-fixture-");
        let path = directory.path().join("long-audio.mp4");
        std::fs::copy(fixture("long-audio.mp4"), &path).unwrap();
        let clip = Clip::open(directory, path);
        let mut playback = play(&decoder, &clip, (64, 36), 9_900, max_fps_milli).unwrap();
        assert_eq!(playback.opened.info.duration_ms, 10_000);
        assert_eq!(playback.opened.start_ms, 3900);
        let frames = frames_of(&mut playback)
            .unwrap_or_else(|failure| panic!("at {max_fps_milli}: {failure:?}"));
        let shown: Vec<(u64, u32)> = frames
            .iter()
            .map(|(pts, rgba)| (*pts, frame_index(rgba, 64, 36, false)))
            .collect();
        assert_eq!(shown, [(3900, 39)], "at {max_fps_milli}");
    }
}

/// A real ffmpeg plays every frame in order from where it starts, each
/// timed as the frame it is, at the rate asked: in MP4, WebM, a rotated MP4
/// (shown upright) and a WebM with no duration.
#[test]
fn real_ffmpeg_plays_each_frame_in_order() {
    let Some(decoder) = installed() else {
        return;
    };
    for (name, room, start_ms, max_fps_milli, size, first, step) in [
        ("index.mp4", (64, 64), 0, 30_000, (64, 36), 0, 1),
        ("index.mp4", (32, 32), 2000, 30_000, (32, 18), 20, 1),
        ("index.mp4", (64, 36), 0, 5_000, (64, 36), 0, 2),
        ("index.webm", (64, 36), 1000, 10_000, (64, 36), 10, 1),
        ("rotated.mp4", (64, 64), 0, 10_000, (36, 64), 0, 1),
        ("mirrored.mp4", (64, 64), 0, 10_000, (64, 36), 0, 1),
        ("unsized.webm", (64, 36), 0, 10_000, (64, 36), 0, 1),
    ] {
        let directory = kettle_test_support::private_tempdir("kettle-ffmpeg-fixture-");
        let path = directory.path().join(name);
        std::fs::copy(fixture(name), &path).unwrap();
        let clip = Clip::open(directory, path);
        let mut playback = play(&decoder, &clip, room, start_ms, max_fps_milli)
            .unwrap_or_else(|failure| panic!("{name}: {failure:?}"));
        let opened = playback.opened;
        assert_eq!((opened.width, opened.height), size, "{name}");
        assert_eq!(opened.info.duration_ms, 4000, "{name}");
        let frames =
            frames_of(&mut playback).unwrap_or_else(|failure| panic!("{name}: {failure:?}"));
        let indices: Vec<u32> = frames
            .iter()
            .map(|(_, rgba)| match name {
                "mirrored.mp4" => frame_index_mirrored(rgba, size.0, size.1),
                _ => frame_index(rgba, size.0, size.1, name == "rotated.mp4"),
            })
            .collect();
        let expected: Vec<u32> = (first..40).step_by(step).collect();
        assert_eq!(indices, expected, "{name} from {start_ms}");
        let times: Vec<u64> = frames.iter().map(|(pts, _)| *pts).collect();
        let expected: Vec<u64> = expected
            .iter()
            .map(|&index| u64::from(index) * 100)
            .collect();
        assert_eq!(times, expected, "{name} from {start_ms}");
    }
}

/// A paced consumer cannot drain future pictures to make its sound arrive.
#[test]
fn real_ffmpeg_paces_video_from_pcm_without_stalling() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    let Some(decoder) = installed() else { return };
    let directory = kettle_test_support::private_tempdir("kettle-paced-pcm-");
    let path = directory.path().join("ticks.mp4");
    std::fs::copy(fixture("ticks.mp4"), &path).unwrap();
    let clip = Clip::open(directory, path);
    let mut playback = play(&decoder, &clip, (64, 36), 0, 30_000).unwrap();
    let mut audio = playback.audio.take().expect("decoded PCM");
    let cancel = audio.cancellation().unwrap();
    let heard = Arc::new(AtomicU64::new(0));
    let stopped = Arc::new(AtomicBool::new(false));
    let (clock, stop) = (Arc::clone(&heard), Arc::clone(&stopped));
    let sound = std::thread::spawn(move || {
        let mut samples = [0.0; 1920];
        while !stop.load(Ordering::Acquire) {
            match audio.next_block(&mut samples) {
                Ok(Some(block)) => {
                    clock.store(block.at_frame, Ordering::Release);
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => return Ok(()),
                Err(_) if stop.load(Ordering::Acquire) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    });
    let gate = PlayGate::default();
    let mut rgba = vec![0; playback.opened.width as usize * playback.opened.height as usize * 4];
    let mut times = Vec::new();
    let result = (|| -> Result<(), String> {
        while let Some(pts) = playback
            .frames
            .next_frame(&mut rgba, &gate)
            .map_err(|error| format!("video: {error:?}"))?
        {
            let until = Instant::now() + Duration::from_secs(2);
            while heard.load(Ordering::Acquire) < pts * 48 {
                if Instant::now() >= until {
                    return Err(format!(
                        "picture {pts} waits for PCM at {}",
                        heard.load(Ordering::Acquire)
                    ));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            times.push(pts);
        }
        Ok(())
    })();
    stopped.store(true, Ordering::Release);
    cancel();
    drop(playback);
    let audio_result = sound.join().unwrap();
    result.unwrap();
    audio_result.unwrap();
    assert_eq!(times, (0..4000).step_by(100).collect::<Vec<_>>());
}

fn delayed_audio_clip() -> Clip {
    let directory = kettle_test_support::private_tempdir("kettle-audio-delay-");
    let path = directory.path().join("delay.mkv");
    std::fs::copy(fixture("delayed-audio.mkv"), &path).unwrap();
    Clip::open(directory, path)
}

#[test]
fn real_ffmpeg_primes_audio_before_a_delayed_track() {
    let Some(decoder) = installed() else { return };
    let clip = delayed_audio_clip();
    for start_ms in [0, 1001, 9501] {
        let mut playback = play(&decoder, &clip, (64, 48), start_ms, 10_000).unwrap();
        let mut audio = playback.audio.take().expect("decoded PCM");
        let cancel = audio.cancellation().unwrap();
        let (sent, received) = std::sync::mpsc::sync_channel(1);
        let reading = std::thread::spawn(move || {
            let mut samples = vec![0.0; kettle_media::pcm::FRAMES * 2];
            let first = audio.next_block(&mut samples);
            let _ = sent.send((first, samples));
        });
        let first = received.recv_timeout(Duration::from_secs(2));
        cancel();
        drop(playback);
        reading.join().unwrap();
        let (block, samples) =
            first.expect("first PCM stalled behind video before the audio starts");
        let block = block.unwrap().expect("leading silence");
        assert_eq!(block.at_frame, start_ms * 48);
        assert_eq!(block.frames, kettle_media::pcm::FRAMES);
        assert!(samples.iter().all(|sample| *sample == 0.0));
    }
}

#[test]
fn real_ffmpeg_delayed_audio_keeps_its_timestamp_and_samples() {
    let Some(decoder) = installed() else { return };
    let clip = delayed_audio_clip();
    for start_ms in [0, 1001, 9501, 10501] {
        let mut playback = play(&decoder, &clip, (64, 48), start_ms, 10_000).unwrap();
        let mut audio = playback.audio.take().expect("decoded PCM");
        let cancel = audio.cancellation().unwrap();
        let video = std::thread::spawn(move || frames_of(&mut playback));
        let mut samples = vec![0.0; kettle_media::pcm::FRAMES * 2];
        let mut end = start_ms * 48;
        let mut first_signal = None;
        let mut valid = true;
        let (mut energy, mut real, mut imaginary, mut measured) = (0.0, 0.0, 0.0, 0usize);
        let mut ended = false;
        for _ in 0..1000 {
            match audio.next_block(&mut samples) {
                Ok(Some(block)) => {
                    if block.frames != kettle_media::pcm::FRAMES
                        || block.at_frame < end
                        || (first_signal.is_some() && block.at_frame != end)
                    {
                        valid = false;
                        break;
                    }
                    valid &= samples.iter().all(|sample| sample.is_finite());
                    if first_signal.is_none() {
                        first_signal = samples
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .position(|frame| frame.iter().any(|sample| sample.abs() > 0.0001))
                            .map(|offset| block.at_frame + offset as u64);
                    }
                    for (offset, frame) in samples.as_chunks::<2>().0.iter().enumerate() {
                        let at = block.at_frame + offset as u64;
                        if (480_000.max(start_ms * 48)..576_000).contains(&at) {
                            let value = f64::from(frame[0]);
                            let phase = at as f64 * 440.0 * std::f64::consts::TAU / 48_000.0;
                            energy += value * value;
                            real += value * phase.cos();
                            imaginary += value * phase.sin();
                            measured += 1;
                        }
                    }
                    end = block.at_frame + block.frames as u64;
                }
                Ok(None) => {
                    ended = true;
                    break;
                }
                Err(_) => {
                    valid = false;
                    break;
                }
            }
        }
        cancel();
        drop(audio);
        let frames = video.join().unwrap().unwrap();
        assert!(valid && ended, "bad or unfinished PCM at seek {start_ms}");
        let expected_signal = if start_ms < 10_000 {
            480_001
        } else {
            start_ms * 48
        };
        assert_eq!(first_signal, Some(expected_signal), "seek {start_ms}");
        assert_eq!(measured, 576_000 - 480_000.max(start_ms * 48) as usize);
        let tone = 2.0 * (real * real + imaginary * imaginary) / (measured as f64 * energy);
        assert!(tone > 0.99, "delayed PCM broke tone continuity: {tone}");
        assert!(
            (576_000..=576_960).contains(&end),
            "PCM end {end} at seek {start_ms}"
        );
        assert!((start_ms..=start_ms + 100).contains(&frames.first().unwrap().0));
        assert!((11_900..12_000).contains(&frames.last().unwrap().0));
    }
}

/// Audio may precede the first picture. A paused session must still open
/// without buffering that whole lead or independently shifting either clock.
#[test]
fn real_ffmpeg_keeps_audio_before_video_without_opening_deadlock() {
    let Some(decoder) = installed() else {
        return;
    };
    let directory = kettle_test_support::private_tempdir("kettle-audio-lead-");
    let path = directory.path().join("lead.mkv");
    let generated = Command::new(decoder.path())
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-threads",
            "2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=8000:duration=4",
            "-itsoffset",
            "1",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x48:rate=10:duration=2",
            "-map",
            "1:v",
            "-map",
            "0:a",
            "-c:v",
            "ffv1",
            "-threads:v",
            "2",
            "-c:a",
            "pcm_s16le",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let clip = Clip::open(directory, path);
    for start_ms in [0, 500] {
        let mut playback = play(&decoder, &clip, (64, 48), start_ms, 10_000).unwrap();
        assert_eq!(playback.opened.audio, kettle_media::AudioStatus::Ready);
        let mut audio = playback.audio.take().expect("decoded PCM");
        let (sent, heard) = std::sync::mpsc::sync_channel(1);
        let opening = std::thread::spawn(move || {
            let mut first = vec![0; 64 * 48 * 4];
            let result = playback.frames.next_frame(&mut first, &PlayGate::default());
            let _ = sent.send((result, first, playback));
        });
        let (result, first, mut playback) = heard
            .recv_timeout(Duration::from_secs(2))
            .expect("paused first frame stalled behind leading sound");
        opening.join().unwrap();
        assert_eq!(result, Ok(Some(start_ms)));
        assert!(
            first
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [0, 0, 0, 255])
        );
        let frames = std::thread::spawn(move || frames_of(&mut playback).unwrap());
        let mut samples = [0.0; 1920];
        let mut next = start_ms * 48;
        let mut loudest = 0.0f32;
        let (mut energy, mut real, mut imaginary, mut measured) = (0.0, 0.0, 0.0, 0usize);
        while let Some(block) = audio.next_block(&mut samples).unwrap() {
            assert_eq!((block.at_frame, block.frames), (next, 960));
            assert!(samples.iter().all(|v| v.is_finite()));
            loudest = samples.iter().fold(loudest, |max, v| max.max(v.abs()));
            for (offset, frame) in samples.as_chunks::<2>().0.iter().enumerate() {
                let at = block.at_frame + offset as u64;
                if (48_000..96_000).contains(&at) {
                    let value = f64::from(frame[0]);
                    let phase = at as f64 * 440.0 * std::f64::consts::TAU / 48_000.0;
                    energy += value * value;
                    real += value * phase.cos();
                    imaginary += value * phase.sin();
                    measured += 1;
                }
            }
            next += 960;
        }
        assert!(next >= 4 * 48_000);
        assert!(loudest > 0.05, "PCM lost the tone");
        assert_eq!(measured, 48_000);
        let tone = 2.0 * (real * real + imaginary * imaginary) / (measured as f64 * energy);
        assert!(tone > 0.99, "decoded PCM broke tone continuity: {tone}");
        let frames = frames.join().unwrap();
        assert_eq!(
            frames.first().unwrap().0,
            1000,
            "video origin reset independently"
        );
        assert_eq!(frames.last().unwrap().0, 2900);
    }
}

#[test]
fn real_ffmpeg_clips_codec_padding_to_the_opened_duration() {
    let Some(decoder) = installed() else { return };
    for start in [0, 500, 3500, 9900] {
        let directory = kettle_test_support::private_tempdir("kettle-audio-padding-");
        let path = directory.path().join("long-audio.mp4");
        std::fs::copy(fixture("long-audio.mp4"), &path).unwrap();
        let clip = Clip::open(directory, path);
        let mut playback = play(&decoder, &clip, (64, 36), start, 10_000).unwrap();
        let opened = playback.opened;
        let mut audio = playback.audio.take().unwrap();
        let sound = std::thread::spawn(move || {
            let mut samples = vec![0.0; 1920];
            let mut end = None;
            while let Some(block) = audio.next_block(&mut samples).unwrap() {
                assert_eq!(block.frames, 960);
                end = Some(block.at_frame + block.frames as u64);
            }
            end.unwrap()
        });
        let frames = frames_of(&mut playback).unwrap();
        assert_eq!(frames.last().unwrap().0, 3900);
        let end = sound.join().unwrap();
        assert!(end >= opened.info.duration_ms * 48);
        assert!(
            end <= opened.info.duration_ms * 48 + 960,
            "codec padding escaped the duration: {end}"
        );
    }
}

#[test]
fn an_end_seek_keeps_a_long_final_frame() {
    let description: Vec<_> = DESCRIPTION
        .iter()
        .map(|&line| match line {
            "streams.stream.0.avg_frame_rate=\"10/1\"" => {
                "streams.stream.0.avg_frame_rate=\"11/4\""
            }
            "streams.stream.0.duration=\"1.000000\"" => "streams.stream.0.duration=\"4.000000\"",
            "format.duration=\"1.000000\"" => "format.duration=\"4.000000\"",
            _ => line,
        })
        .collect();
    let fake = Fake::new(
        &description,
        &[
            "0,K__", "100,___", "200,___", "300,___", "400,___", "500,K__", "600,___", "700,___",
            "800,___", "900,___", "1000,___",
        ],
        GOOD,
    );
    let clip = Clip::mp4();
    let mut playback = play(&fake.decoder(), &clip, (40, 30), 3999, 30_000).unwrap();
    assert_eq!(
        playback.opened.start_ms, 1000,
        "seek must not discard the final picture"
    );
    assert_eq!(frames_of(&mut playback).unwrap().len(), 1);
    assert_eq!(
        arg_after(&fake.calls("ffmpeg").pop().unwrap(), "-ss"),
        Some("1.000000")
    );
}

/// The next audio timestamp is needed to fill an interior gap while future
/// video remains queued. Requiring its pictures to drain first deadlocks.
#[test]
fn real_ffmpeg_audio_crosses_an_interior_gap_while_video_is_held() {
    let Some(decoder) = installed() else { return };
    let directory = kettle_test_support::private_tempdir("kettle-interior-audio-gap-");
    let path = directory.path().join("gap.mkv");
    std::fs::copy(fixture("interior-audio-gap.mkv"), &path).unwrap();
    let clip = Clip::open(directory, path);
    let mut playback = play(&decoder, &clip, (64, 48), 0, 10_000).unwrap();
    let mut audio = playback.audio.take().expect("decoded PCM");
    let cancel = audio.cancellation().unwrap();
    let (sent, received) = std::sync::mpsc::sync_channel(1);
    let reading = std::thread::spawn(move || {
        let mut samples = [0.0; 1920];
        for _ in 0..200 {
            match audio.next_block(&mut samples) {
                Ok(Some(block)) if block.at_frame >= 96_000 => {
                    let loudest = samples[..block.frames * 2]
                        .iter()
                        .fold(0.0f32, |max, sample| max.max(sample.abs()));
                    let _ = sent.send(Ok((block.at_frame, loudest)));
                    return;
                }
                Ok(Some(_)) => {}
                result => {
                    let _ = sent.send(Err(format!("audio ended before the gap: {result:?}")));
                    return;
                }
            }
        }
        let _ = sent.send(Err("no timestamp after the interior gap".into()));
    });
    let mut rgba = vec![0; 64 * 48 * 4];
    for expected in [0, 100, 200, 300, 400] {
        assert_eq!(
            playback.frames.next_frame(&mut rgba, &PlayGate::default()),
            Ok(Some(expected))
        );
    }
    let after_gap = received.recv_timeout(Duration::from_secs(2));
    cancel();
    drop(playback);
    reading.join().unwrap();
    let (at_frame, loudest) = after_gap
        .expect("future PCM stalled behind queued video")
        .unwrap();
    assert!(
        (96_000..100_800).contains(&at_frame),
        "unexpected gap end {at_frame}"
    );
    assert!(loudest > 0.01, "post-gap tone was lost");
}

#[test]
fn real_ffmpeg_cancelling_pcm_keeps_video_decoding() {
    let Some(decoder) = installed() else { return };
    let directory = kettle_test_support::private_tempdir("kettle-drop-pcm-");
    let path = directory.path().join("ticks.mp4");
    std::fs::copy(fixture("ticks.mp4"), &path).unwrap();
    let clip = Clip::open(directory, path);
    let mut playback = play(&decoder, &clip, (64, 36), 0, 10_000).unwrap();
    let audio = playback.audio.take().expect("decoded PCM");
    audio.cancellation().unwrap()();
    drop(audio);
    let frames = frames_of(&mut playback).unwrap();
    assert_eq!(
        frames.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
        (0..4000).step_by(100).collect::<Vec<_>>()
    );
}

#[test]
fn real_ffmpeg_seeking_beyond_audio_keeps_its_supported_status() {
    let Some(decoder) = installed() else { return };
    let directory = kettle_test_support::private_tempdir("kettle-ended-pcm-");
    let path = directory.path().join("short-audio.mkv");
    std::fs::copy(fixture("short-audio.mkv"), &path).unwrap();
    let clip = Clip::open(directory, path);
    let mut playback = play(&decoder, &clip, (64, 48), 1500, 10_000).unwrap();
    assert_eq!(playback.opened.audio, kettle_media::AudioStatus::Ready);
    let mut audio = playback.audio.take().expect("supported audio source");
    assert_eq!(audio.next_block(&mut [0.0; 1920]), Ok(None));
    let frames = frames_of(&mut playback).unwrap();
    assert_eq!(
        frames.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
        (1500..3000).step_by(100).collect::<Vec<_>>()
    );
}
