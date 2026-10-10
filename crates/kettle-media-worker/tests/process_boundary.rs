//! The worker binary end to end: its handshake and refusals, its watchdog,
//! and the setup it finishes before it reads anything.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::io::Read as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use kettle_media::wire::{Direction, Frame, encode, read_frame, write_frame};
use kettle_media::{
    BuildId, Canvas, Failure, FailureCode, FallbackFont, Hello, Job, JobKind, MediaKind,
    NativePath, Ready, Source, Target, Theme, Warning,
};

#[path = "../../kettle-media-render/tests/support/fonts.rs"]
mod fonts;

const WORKER: &str = env!("CARGO_BIN_EXE_kettle-media-worker");
const EXIT_PROTOCOL: i32 = 2;
const EXIT_TIMEOUT: i32 = 4;
const EXIT_SKEW: i32 = 9;

fn own_build() -> BuildId {
    BuildId::from_embedded(env!("CARGO_PKG_VERSION"), env!("KETTLE_SOURCE_HASH")).unwrap()
}

fn hello(build_id: BuildId) -> Frame {
    Frame::Hello(Hello { build_id })
}

fn job(bytes: &[u8]) -> Frame {
    job_of(JobKind::Raster, bytes)
}

fn job_of(kind: JobKind, bytes: &[u8]) -> Frame {
    Frame::Job(Job {
        kind,
        source: Source::Bytes(bytes.to_vec()),
        theme: Theme {
            background: [0; 4],
            foreground: [255; 4],
            palette: [[0; 4]; 16],
            accent: [0; 4],
            is_dark: true,
        },
        canvas: Canvas::Theme,
        target: Target {
            width: 1,
            height: 1,
            scale: 1.0,
            crop: None,
        },
        fallback_fonts: vec![],
    })
}

fn spawn(command: &mut Command) -> Child {
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn worker() -> Child {
    spawn(&mut Command::new(WORKER))
}

fn send(child: &mut Child, frame: &Frame) {
    write_frame(
        child.stdin.as_mut().unwrap(),
        frame,
        Direction::ParentToWorker,
    )
    .unwrap();
}

fn send_bytes(child: &mut Child, bytes: &[u8]) {
    use std::io::Write as _;
    child.stdin.as_mut().unwrap().write_all(bytes).unwrap();
}

fn receive(child: &mut Child) -> Option<Frame> {
    read_frame(child.stdout.as_mut().unwrap(), Direction::WorkerToParent).unwrap()
}

/// Wait for the exit code, killing the worker if it is not done `within`.
fn exit_code(child: &mut Child, within: Duration) -> i32 {
    let deadline = Instant::now() + within;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code().unwrap_or(-1);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the worker did not exit within {within:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Everything else the worker wrote: stdout must be done, stderr returned.
fn finish(child: &mut Child) -> String {
    let mut rest = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty(), "stdout carried more than frames: {rest:?}");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    stderr
}

fn failure(code: FailureCode) -> Option<Frame> {
    Some(Frame::Failure(Failure { code }))
}

/// Hello, then Ready with this build's identity.
fn handshake(child: &mut Child) {
    send(child, &hello(own_build()));
    assert_eq!(
        receive(child),
        Some(Frame::Ready(Ready {
            build_id: own_build()
        }))
    );
}

/// A 1x1 PNG of `color`, encoded by the `image` crate.
fn png(color: [u8; 4]) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba(color)))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

#[test]
fn ready_carries_this_build_and_a_raster_job_is_rendered() {
    let mut child = worker();
    handshake(&mut child);
    send(&mut child, &job(&png([10, 20, 30, 255])));
    let Some(Frame::Rendered(rendered)) = receive(&mut child) else {
        panic!("no rendered reply");
    };
    assert_eq!(
        (rendered.width, rendered.height, rendered.rgba),
        (1, 1, vec![10, 20, 30, 255])
    );
    drop(child.stdin.take());
    assert_eq!(receive(&mut child), None);
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
    assert_eq!(finish(&mut child), "");
}

#[test]
fn an_svg_job_is_rendered() {
    let mut child = worker();
    handshake(&mut child);
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4" fill="rgb(10,20,30)"/></svg>"#;
    send(&mut child, &job_of(JobKind::Svg, svg));
    let Some(Frame::Rendered(rendered)) = receive(&mut child) else {
        panic!("no rendered reply");
    };
    assert_eq!(
        (rendered.width, rendered.height, rendered.rgba),
        (1, 1, vec![10, 20, 30, 255])
    );
    assert_eq!(receive(&mut child), None);
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
    assert_eq!(finish(&mut child), "");
}

const SVG_1X1: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><rect width="1" height="1" fill="rgb(10,20,30)"/></svg>"#;

/// One job's whole exchange: its reply, then a clean exit with nothing else
/// said.
fn only_reply(frame: &Frame) -> Option<Frame> {
    let mut child = worker();
    handshake(&mut child);
    send(&mut child, frame);
    let reply = receive(&mut child);
    drop(child.stdin.take());
    assert_eq!(receive(&mut child), None);
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
    assert_eq!(finish(&mut child), "");
    reply
}

#[test]
fn an_auto_job_replies_with_the_kind_it_turned_out_to_be() {
    for (bytes, expected) in [
        (png([10, 20, 30, 255]), MediaKind::Raster),
        (SVG_1X1.to_vec(), MediaKind::Svg),
    ] {
        let Some(Frame::DetectedRendered { kind, rendered }) =
            only_reply(&job_of(JobKind::Auto, &bytes))
        else {
            panic!("no typed reply to an Auto job");
        };
        assert_eq!(kind, expected);
        assert_eq!(
            (rendered.width, rendered.height, rendered.rgba),
            (1, 1, vec![10, 20, 30, 255])
        );
    }
    assert_eq!(
        only_reply(&job_of(JobKind::Auto, b"ordinary prose")),
        failure(FailureCode::UnsupportedMedia)
    );
}

/// A Markdown gallery crosses the worker's boundary whole: an Auto job's
/// reply says Markdown and carries every page, the document as read and
/// the page rendered; an explicit page comes back as asked, and a page past
/// the last is refused by its code.
#[test]
fn a_markdown_gallery_crosses_the_boundary_whole() {
    let document = "# Two\n\n```mermaid\nflowchart LR\n  A --> B\n```\n\n\
        ```mermaid\nflowchart TD\n  C --> D\n```\n";
    let Some(Frame::DetectedRendered { kind, rendered }) =
        only_reply(&job_of(JobKind::Auto, document.as_bytes()))
    else {
        panic!("no typed reply to an Auto Markdown job");
    };
    assert_eq!(kind, MediaKind::Markdown);
    rendered.validate_as(MediaKind::Markdown).unwrap();
    assert_eq!(
        rendered.fence_sources,
        ["flowchart LR\n  A --> B\n", "flowchart TD\n  C --> D\n"]
    );
    assert_eq!((rendered.fence_count, rendered.fence_index), (2, Some(0)));
    assert_eq!(rendered.exact_source.as_deref(), Some(document));
    let Some(Frame::Rendered(second)) = only_reply(&job_of(
        JobKind::MarkdownDiagrams { index: 1 },
        document.as_bytes(),
    )) else {
        panic!("no reply to an explicit page");
    };
    assert_eq!(second.fence_index, Some(1));
    assert_eq!(second.digest, rendered.digest);
    assert_eq!(
        only_reply(&job_of(
            JobKind::MarkdownDiagrams { index: 2 },
            document.as_bytes()
        )),
        failure(FailureCode::IndexOutOfRange)
    );
}

/// An animation's stills cross the worker's boundary: the sheet, with what
/// the animation is and each frame's times; a video container, with no
/// decoder in this worker yet, is refused by its code.
#[test]
fn an_animations_stills_cross_the_boundary() {
    let stills = kettle_media::VideoStills {
        count: 3,
        max_edge: 256,
        start_s: 0.0,
        end_s: None,
        at_s: None,
        layout: kettle_media::StillsLayout::Sheet {
            cols: 3,
            labels: true,
        },
    };
    let webp = include_bytes!("../../kettle-media-render/tests/fixtures/stills/anim.webp");
    let Some(Frame::Rendered(rendered)) = only_reply(&job_of(JobKind::VideoStills(stills), webp))
    else {
        panic!("no reply to a stills job");
    };
    rendered.validate_as(MediaKind::Video).unwrap();
    let video = rendered.video.unwrap();
    assert_eq!(
        (video.info.duration_ms, video.info.codec),
        (1000, kettle_media::VideoCodec::WebP)
    );
    assert_eq!(
        video
            .samples
            .iter()
            .map(|sample| sample.actual_ms)
            .collect::<Vec<_>>(),
        [0, 300, 600]
    );
    // Inline video bytes: no decoder reads them, decoders reading files.
    let mut mp4 = vec![0, 0, 0, 24];
    mp4.extend_from_slice(b"ftypisom\0\0\x02\0isomiso2");
    assert_eq!(
        only_reply(&job_of(JobKind::VideoStills(stills), &mp4)),
        failure(FailureCode::UnsupportedMedia)
    );
}

/// A worker started with an external decoder named, as Kettle starts one.
fn worker_with_decoder(ffmpeg: &std::path::Path) -> Child {
    Command::new(WORKER)
        .env_clear()
        .env(kettle_media::video::DECODER_ENV, ffmpeg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// The one reply to `frame` from `child`, which then exits quietly.
fn reply_from(mut child: Child, frame: &Frame) -> Option<Frame> {
    handshake(&mut child);
    send(&mut child, frame);
    let reply = receive(&mut child);
    drop(child.stdin.take());
    assert_eq!(receive(&mut child), None);
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
    assert_eq!(finish(&mut child), "");
    reply
}

/// A stills job on a file, as a GUI action names one.
fn stills_of(path: &std::path::Path, stills: kettle_media::VideoStills) -> Frame {
    use std::os::unix::ffi::OsStrExt as _;
    let Frame::Job(mut job) = job_of(JobKind::VideoStills(stills), b"") else {
        unreachable!()
    };
    job.source = Source::user_pull(
        NativePath::new(path.as_os_str().as_bytes().to_vec()).unwrap(),
        kettle_media::GuiActionWitness::from_explicit_gui_action(),
    );
    Frame::Job(job)
}

fn sheet(count: u8) -> kettle_media::VideoStills {
    kettle_media::VideoStills {
        count,
        max_edge: 64,
        start_s: 0.0,
        end_s: None,
        at_s: None,
        layout: kettle_media::StillsLayout::Sheet {
            cols: 2,
            labels: true,
        },
    }
}

/// Stand-in ffmpeg and ffprobe for a one-second, 40x30, 10 fps H.264 video,
/// built from shell built-ins only (a contained decoder cannot start a
/// process, and the worker's file-size limit forbids writing a log).
fn stand_in_decoder(directory: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let description = [
        "streams.stream.0.index=0",
        "streams.stream.0.codec_name=\"h264\"",
        "streams.stream.0.codec_type=\"video\"",
        "streams.stream.0.width=40",
        "streams.stream.0.height=30",
        "streams.stream.0.avg_frame_rate=\"10/1\"",
        "streams.stream.0.time_base=\"1/1000\"",
        "format.start_time=\"0.000000\"",
        "format.duration=\"1.000000\"",
    ];
    let packets = [
        "0,K__", "100,___", "200,___", "300,___", "400,___", "500,K__", "600,___", "700,___",
        "800,___", "900,___",
    ];
    let quoted = |lines: &[&str]| {
        lines
            .iter()
            .map(|line| format!("'{line}'"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let ffprobe = format!(
        "case \"$*\" in\n*packet=*) printf '%s\\n' {} ;;\n*) printf '%s\\n' {} ;;\nesac",
        quoted(&packets),
        quoted(&description)
    );
    let ffmpeg = "w=0; h=0\n\
        for arg in \"$@\"; do case \"$arg\" in scale=*) v=${arg#scale=}; w=${v%%:*}; v=${v#*:}; h=${v%%:*} ;; esac; done\n\
        n=$((w * h)); i=0\n\
        while [ \"$i\" -lt \"$n\" ]; do printf '\\000\\200\\377\\377'; i=$((i + 1)); done";
    for (name, body) in [("ffprobe", ffprobe.as_str()), ("ffmpeg", ffmpeg)] {
        let path = directory.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    directory.join("ffmpeg")
}

/// A file whose first bytes say MP4.
fn mp4_file(directory: &std::path::Path) -> std::path::PathBuf {
    let mut bytes = vec![0, 0, 0, 24];
    bytes.extend_from_slice(b"ftypisom\0\0\x02\0isomiso2");
    bytes.extend_from_slice(&[0; 64]);
    let path = directory.join("clip.mp4");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// A video's stills come from the decoder the parent named, under the
/// worker's own limits: the sheet, with what the video is and the time of
/// each frame shown. Without a decoder, or with one others could have
/// replaced, they are `BackendUnavailable`; inline video bytes are
/// unsupported, a decoder reading only files.
#[test]
fn a_videos_stills_come_from_the_decoder_the_parent_named() {
    use std::os::unix::fs::PermissionsExt as _;
    let decoders = kettle_test_support::private_tempdir("kettle-worker-decoder-");
    let ffmpeg = stand_in_decoder(decoders.path());
    let clips = kettle_test_support::private_tempdir("kettle-worker-clip-");
    let clip = mp4_file(clips.path());

    let Some(Frame::Rendered(rendered)) =
        reply_from(worker_with_decoder(&ffmpeg), &stills_of(&clip, sheet(4)))
    else {
        panic!("no stills from the decoder");
    };
    rendered.validate_as(MediaKind::Video).unwrap();
    let video = rendered.video.unwrap();
    assert_eq!(
        (
            video.info.duration_ms,
            video.info.width,
            video.info.height,
            video.info.codec
        ),
        (1000, 40, 30, kettle_media::VideoCodec::H264)
    );
    assert_eq!(
        video
            .samples
            .iter()
            .map(|sample| (sample.requested_ms, sample.actual_ms))
            .collect::<Vec<_>>(),
        [(125, 100), (375, 300), (625, 600), (875, 800)]
    );
    assert_eq!(video.tolerance_ms, 75);
    assert_eq!(
        rendered.digest.path_identity.map(|identity| identity.size),
        Some(88),
        "the held file's identity stands for the video"
    );

    // With no external decoder: Linux has none; macOS tries Apple's, which
    // cannot read this stand-in file.
    let without = failure(if cfg!(target_os = "macos") {
        FailureCode::UnsupportedContainer
    } else {
        FailureCode::BackendUnavailable
    });
    assert_eq!(reply_from(worker(), &stills_of(&clip, sheet(4))), without);
    let Frame::Job(mut inline) = stills_of(&clip, sheet(4)) else {
        unreachable!()
    };
    inline.source = Source::Bytes(std::fs::read(&clip).unwrap());
    assert_eq!(
        reply_from(worker_with_decoder(&ffmpeg), &Frame::Job(inline)),
        failure(FailureCode::UnsupportedMedia)
    );
    std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        reply_from(worker_with_decoder(&ffmpeg), &stills_of(&clip, sheet(4))),
        without,
        "an untrusted decoder is no decoder"
    );
}

/// On macOS, Apple's decoder reads an MP4 inside the worker, under its
/// limits, with no external decoder: each frame's time is a frame's own
/// start, within the tolerance reported.
#[cfg(target_os = "macos")]
#[test]
fn apples_decoder_reads_mp4_in_the_worker() {
    let clips = kettle_test_support::private_tempdir("kettle-worker-clip-");
    let clip = clips.path().join("index.mp4");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kettle-media-native/tests/fixtures/video/index.mp4"
        ),
        &clip,
    )
    .unwrap();
    let Some(Frame::Rendered(rendered)) = reply_from(worker(), &stills_of(&clip, sheet(3))) else {
        panic!("no stills from AVFoundation");
    };
    rendered.validate_as(MediaKind::Video).unwrap();
    let video = rendered.video.unwrap();
    assert_eq!(
        (video.info.codec, video.info.duration_ms, video.info.width),
        (kettle_media::VideoCodec::H264, 4000, 64)
    );
    for sample in &video.samples {
        assert_eq!(sample.actual_ms % 100, 0, "{sample:?}");
        assert!(sample.requested_ms.abs_diff(sample.actual_ms) <= u64::from(video.tolerance_ms));
    }
}

/// A video shown as it is (an Auto job) comes back as a video: its poster
/// fitted to the job's box and its metadata, from the decoder the parent
/// named, whatever the file is called.
#[test]
fn an_auto_job_on_a_video_replies_with_its_poster() {
    use std::os::unix::ffi::OsStrExt as _;
    let decoders = kettle_test_support::private_tempdir("kettle-worker-decoder-");
    let ffmpeg = stand_in_decoder(decoders.path());
    let clips = kettle_test_support::private_tempdir("kettle-worker-clip-");
    let clip = mp4_file(clips.path());
    let named = clips.path().join("clip.png");
    std::fs::rename(&clip, &named).unwrap();
    let Frame::Job(mut job) = job_of(JobKind::Auto, b"") else {
        unreachable!()
    };
    job.source = Source::user_pull(
        NativePath::new(named.as_os_str().as_bytes().to_vec()).unwrap(),
        kettle_media::GuiActionWitness::from_explicit_gui_action(),
    );
    job.target.width = 20;
    job.target.height = 20;
    let Some(Frame::DetectedRendered { kind, rendered }) =
        reply_from(worker_with_decoder(&ffmpeg), &Frame::Job(job))
    else {
        panic!("no poster for a video");
    };
    assert_eq!(kind, MediaKind::Video);
    assert_eq!(
        (rendered.width, rendered.height),
        (20, 15),
        "40x30 fitted into 20x20"
    );
    let video = rendered.video.unwrap();
    assert_eq!(
        (video.info.duration_ms, video.samples[0].requested_ms),
        (1000, 500),
        "the middle frame"
    );
}

/// A real ffmpeg, when one is installed, decodes through the worker under
/// its limits and returns the frames showing at each instant.
#[test]
fn a_real_decoder_crosses_the_boundary() {
    let home = std::env::var_os("HOME");
    let Some(found) =
        kettle_media_native::ffmpeg::Ffmpeg::search(home.as_deref().map(std::path::Path::new))
    else {
        eprintln!("skipped: no trusted ffmpeg is installed");
        return;
    };
    let clips = kettle_test_support::private_tempdir("kettle-worker-clip-");
    let clip = clips.path().join("index.webm");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kettle-media-native/tests/fixtures/video/index.webm"
        ),
        &clip,
    )
    .unwrap();
    let Some(Frame::Rendered(rendered)) = reply_from(
        worker_with_decoder(found.path()),
        &stills_of(&clip, sheet(3)),
    ) else {
        panic!("no stills from ffmpeg");
    };
    let video = rendered.video.unwrap();
    assert_eq!(video.info.codec, kettle_media::VideoCodec::Vp8);
    assert_eq!(
        video
            .samples
            .iter()
            .map(|sample| sample.actual_ms)
            .collect::<Vec<_>>(),
        [600, 2000, 3300]
    );
}

#[test]
fn an_explicit_kind_gets_the_plain_reply() {
    for frame in [job(&png([10, 20, 30, 255])), job_of(JobKind::Svg, SVG_1X1)] {
        assert!(matches!(only_reply(&frame), Some(Frame::Rendered(_))));
    }
}

#[test]
fn installed_worker_renders_svg_and_raster() {
    let install = kettle_test_support::private_tempdir("kettle-worker-install-");
    let bin = install.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let installed = bin.join("kettle-media-worker");
    kettle_test_support::copy_executable_fixture(std::path::Path::new(WORKER), &installed).unwrap();
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><rect width="1" height="1" fill="rgb(10,20,30)"/></svg>"#;
    for frame in [job(&png([10, 20, 30, 255])), job_of(JobKind::Svg, svg)] {
        let mut child = spawn(&mut Command::new(&installed));
        handshake(&mut child);
        send(&mut child, &frame);
        let Some(Frame::Rendered(rendered)) = receive(&mut child) else {
            panic!("installed worker did not render");
        };
        assert_eq!(rendered.rgba, [10, 20, 30, 255]);
        assert_eq!((rendered.width, rendered.height), (1, 1));
        assert_eq!(receive(&mut child), None);
        assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
        assert_eq!(finish(&mut child), "");
    }
}

#[test]
fn explicit_collection_faces_and_font_failures_cross_the_worker_protocol() {
    use std::os::unix::ffi::OsStrExt as _;
    let directory = kettle_test_support::private_tempdir("kettle-worker-font-");
    let path = directory.path().join("faces.ttc");
    std::fs::write(&path, fonts::collection().0).unwrap();
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"160\" height=\"64\"><text y=\"42\" font-size=\"32\" font-family=\"{}\">\u{4e2d}</text></svg>",
        fonts::FAMILY
    );
    for (index, expected) in [(0, None), (1, None), (2, Some(FailureCode::RenderParse))] {
        let Frame::Job(mut job) = job_of(JobKind::Svg, svg.as_bytes()) else {
            unreachable!()
        };
        job.target.width = 160;
        job.target.height = 64;
        job.fallback_fonts.push(FallbackFont {
            path: NativePath::new(path.as_os_str().as_bytes().to_vec()).unwrap(),
            face_index: index,
        });
        let mut child = worker();
        handshake(&mut child);
        send(&mut child, &Frame::Job(job));
        let reply = receive(&mut child);
        if let Some(code) = expected {
            assert_eq!(reply, failure(code));
        } else {
            let Some(Frame::Rendered(rendered)) = reply else {
                panic!("no font result")
            };
            assert_eq!((rendered.width, rendered.height), (160, 64));
            assert_eq!(
                rendered.warnings.contains(&Warning::MissingGlyphs),
                index == 0
            );
            assert_eq!(
                rendered.warnings.contains(&Warning::FontFallback),
                index == 1
            );
            assert_eq!(
                rendered.uncovered_scripts,
                if index == 0 { vec!["Han"] } else { Vec::new() }
            );
        }
        assert_eq!(receive(&mut child), None);
        assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
        assert_eq!(finish(&mut child), "");
    }
}

#[test]
fn svg_admission_failures_are_framed_and_exit_cleanly() {
    let names = format!(
        "<feFlood result=\"{}\"/><feMerge>{}</feMerge>",
        "a".repeat(200_000),
        "<feMergeNode/>".repeat(500),
    );
    let layers = format!(
        "<feMerge>{}</feMerge>",
        "<feMergeNode in=\"SourceGraphic\"/>".repeat(20),
    );
    let dual = "<feBlend in=\"SourceGraphic\" in2=\"SourceGraphic\"/>".repeat(8);
    for primitives in [names, layers, dual] {
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"512\" height=\"512\"><defs><filter id=\"f\">{primitives}</filter></defs><rect width=\"512\" height=\"512\" filter=\"url(#f)\"/></svg>"
        );
        let Frame::Job(mut job) = job_of(JobKind::Svg, svg.as_bytes()) else {
            unreachable!();
        };
        job.target.width = 512;
        job.target.height = 512;
        let mut child = worker();
        handshake(&mut child);
        send(&mut child, &Frame::Job(job));
        assert_eq!(receive(&mut child), failure(FailureCode::RenderResource));
        assert_eq!(receive(&mut child), None);
        assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
        assert_eq!(finish(&mut child), "");
    }
}

#[test]
fn a_job_it_cannot_render_is_refused() {
    let mut child = worker();
    handshake(&mut child);
    send(&mut child, &job(&[1, 2, 3]));
    assert_eq!(receive(&mut child), failure(FailureCode::UnsupportedMedia));
    assert_eq!(receive(&mut child), None);
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
    assert_eq!(finish(&mut child), "");
}

#[test]
fn another_build_s_hello_is_restart_required() {
    for other in [
        BuildId::from_embedded(env!("CARGO_PKG_VERSION"), "0").unwrap(),
        BuildId::from_embedded("0.0.1", env!("KETTLE_SOURCE_HASH")).unwrap(),
    ] {
        let mut child = worker();
        send(&mut child, &hello(other));
        assert_eq!(receive(&mut child), failure(FailureCode::RestartRequired));
        assert_eq!(receive(&mut child), None);
        assert_eq!(exit_code(&mut child, Duration::from_secs(10)), EXIT_SKEW);
        assert_eq!(finish(&mut child), "");
    }
}

#[test]
fn another_frame_version_is_restart_required() {
    let mut bytes = encode(&hello(own_build()), Direction::ParentToWorker).unwrap();
    // The header's protocol version follows the four-byte magic.
    bytes[4] = bytes[4].wrapping_add(1);
    let mut child = worker();
    send_bytes(&mut child, &bytes);
    assert_eq!(receive(&mut child), failure(FailureCode::RestartRequired));
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), EXIT_SKEW);
    assert_eq!(finish(&mut child), "");
}

#[test]
fn garbage_and_frames_out_of_order_are_refused() {
    // Not a frame at all.
    let mut child = worker();
    send_bytes(&mut child, b"this is not a media frame");
    assert_eq!(receive(&mut child), failure(FailureCode::BadParams));
    assert_eq!(
        exit_code(&mut child, Duration::from_secs(10)),
        EXIT_PROTOCOL
    );
    assert_eq!(finish(&mut child), "");

    // A job before Hello.
    let mut child = worker();
    send(&mut child, &job(&[1]));
    assert_eq!(receive(&mut child), failure(FailureCode::BadParams));
    assert_eq!(
        exit_code(&mut child, Duration::from_secs(10)),
        EXIT_PROTOCOL
    );
    assert_eq!(finish(&mut child), "");

    // A second Hello where the job belongs.
    let mut child = worker();
    handshake(&mut child);
    send(&mut child, &hello(own_build()));
    assert_eq!(receive(&mut child), failure(FailureCode::BadParams));
    assert_eq!(
        exit_code(&mut child, Duration::from_secs(10)),
        EXIT_PROTOCOL
    );
    assert_eq!(finish(&mut child), "");
}

#[test]
fn a_frame_cut_short_is_still_answered() {
    let whole = encode(&hello(own_build()), Direction::ParentToWorker).unwrap();
    // Part of the header, then part of the payload, then the parent stops
    // writing but keeps reading.
    for cut in [5, whole.len() - 3] {
        let mut child = worker();
        send_bytes(&mut child, &whole[..cut]);
        drop(child.stdin.take());
        assert_eq!(
            receive(&mut child),
            failure(FailureCode::BadParams),
            "cut at {cut}"
        );
        assert_eq!(
            exit_code(&mut child, Duration::from_secs(10)),
            EXIT_PROTOCOL
        );
        assert_eq!(finish(&mut child), "");
    }
}

#[test]
fn a_parent_that_leaves_ends_the_worker_quietly() {
    // Before Hello, and after Ready.
    let mut child = worker();
    drop(child.stdin.take());
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
    assert_eq!(finish(&mut child), "");

    let mut child = worker();
    handshake(&mut child);
    drop(child.stdin.take());
    assert_eq!(receive(&mut child), None);
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
    assert_eq!(finish(&mut child), "");
}

#[test]
fn worker_watchdog_exits_without_parent_scheduler() {
    // A parent that holds stdin open and says nothing, before Hello and
    // after Ready: the worker's own watchdog ends it after five seconds.
    for after_ready in [false, true] {
        let mut child = worker();
        if after_ready {
            handshake(&mut child);
        }
        let started = Instant::now();
        assert_eq!(
            exit_code(&mut child, Duration::from_secs(20)),
            EXIT_TIMEOUT,
            "after Ready: {after_ready}"
        );
        assert!(started.elapsed() >= Duration::from_secs(4));
        assert_eq!(finish(&mut child), "");
    }
}

#[test]
fn each_phase_has_its_own_deadline() {
    // Slow but within each phase: Hello after three seconds, the job three
    // and a half seconds after Ready. Six and a half in all is past the
    // first phase's five, so this passes only if Ready starts a new one.
    let mut child = worker();
    std::thread::sleep(Duration::from_secs(3));
    handshake(&mut child);
    std::thread::sleep(Duration::from_millis(3500));
    send(&mut child, &job(&[1]));
    assert_eq!(receive(&mut child), failure(FailureCode::UnsupportedMedia));
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
}

#[test]
fn inherited_high_fd_is_closed() {
    use std::os::unix::process::CommandExt as _;

    let mut ends = [0; 2];
    // SAFETY: `ends` has room for the two descriptors.
    assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
    let [read_end, write_end] = ends;
    let mut command = Command::new(WORKER);
    // SAFETY: dup2 is async-signal-safe and touches only descriptor numbers.
    // It leaves fd 40 without close-on-exec, as a careless parent would.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(write_end, 40) == 40 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = spawn(&mut command);
    // SAFETY: the test owns the write end; the worker now holds its only
    // other copy, at 40.
    unsafe { libc::close(write_end) };
    handshake(&mut child);
    // The worker is alive and waiting for its job. If it had kept fd 40, the
    // pipe would still have a writer and this read would wait.
    let mut poll = libc::pollfd {
        fd: read_end,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `poll` is one valid pollfd.
    let ready = unsafe { libc::poll(&mut poll, 1, 5_000) };
    let mut byte = 0u8;
    // SAFETY: reads at most one byte into `byte`.
    let read = unsafe { libc::read(read_end, (&raw mut byte).cast(), 1) };
    assert_eq!((ready, read), (1, 0), "the worker kept the inherited pipe");
    // SAFETY: the test owns the read end.
    unsafe { libc::close(read_end) };
    drop(child.stdin.take());
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_worker_is_not_dumpable() {
    use std::os::unix::fs::MetadataExt as _;

    let mut child = worker();
    handshake(&mut child);
    // A process that is not dumpable has its /proc entries owned by root.
    let owner = std::fs::metadata(format!("/proc/{}/environ", child.id()))
        .unwrap()
        .uid();
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("running as root: /proc ownership cannot show dumpability");
    } else {
        assert_eq!(owner, 0, "the worker is still dumpable");
    }
    drop(child.stdin.take());
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn worker_rlimits_are_installed_before_read() {
    let mut child = worker();
    handshake(&mut child);
    let limits = std::fs::read_to_string(format!("/proc/{}/limits", child.id())).unwrap();
    let limit = |name: &str| {
        let line = limits
            .lines()
            .find(|line| line.starts_with(name))
            .unwrap_or_else(|| panic!("{name} missing"));
        let fields: Vec<&str> = line[name.len()..].split_whitespace().collect();
        (fields[0].to_string(), fields[1].to_string())
    };
    let both = |value: &str| (value.to_string(), value.to_string());
    assert_eq!(limit("Max cpu time"), both("5"));
    assert_eq!(limit("Max file size"), both("0"));
    assert_eq!(limit("Max core file size"), both("0"));
    assert_eq!(limit("Max address space"), both("1073741824"));
    let (soft, hard) = limit("Max open files");
    assert_eq!(soft, hard);
    assert!(soft.parse::<u32>().unwrap() <= 32);
    drop(child.stdin.take());
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
}

#[cfg(feature = "test-faults")]
#[test]
fn worker_panic_does_not_echo_payload() {
    let mut child = worker();
    handshake(&mut child);
    send(
        &mut child,
        &job(b"kettle-media-worker-test-panic:secret path /Users/someone/a.png"),
    );
    // No reply follows a panic.
    assert_eq!(receive(&mut child), None);
    assert_ne!(exit_code(&mut child, Duration::from_secs(10)), 0);
    assert_eq!(finish(&mut child), "media worker panic\n");
}

/// `kind` over `bytes`, pausing `before` and `after` tenths of a second
/// either side of its classification (the `test-faults` worker's hook).
#[cfg(feature = "test-faults")]
fn paused(kind: JobKind, bytes: &[u8], before: u8, after: u8) -> Frame {
    let Frame::Job(mut paused) = job_of(kind, bytes) else {
        unreachable!()
    };
    paused.theme.accent = [b'K', b'T', before, after];
    Frame::Job(paused)
}

/// The job's reply, or `None` when the worker's watchdog ended it first.
#[cfg(feature = "test-faults")]
fn reply_or_timeout(frame: &Frame) -> Option<Frame> {
    let mut child = worker();
    handshake(&mut child);
    send(&mut child, frame);
    drop(child.stdin.take());
    let reply = receive(&mut child);
    let code = exit_code(&mut child, Duration::from_secs(10));
    assert_eq!(code, if reply.is_some() { 0 } else { EXIT_TIMEOUT });
    assert_eq!(finish(&mut child), "");
    reply
}

/// Raster gets two seconds and SVG three, whether the kind was asked for or
/// found by classification.
#[cfg(feature = "test-faults")]
#[test]
fn each_kind_keeps_its_own_render_deadline() {
    let png = png([10, 20, 30, 255]);
    assert_eq!(
        reply_or_timeout(&paused(JobKind::Raster, &png, 0, 22)),
        None
    );
    assert_eq!(reply_or_timeout(&paused(JobKind::Auto, &png, 0, 22)), None);
    assert!(matches!(
        reply_or_timeout(&paused(JobKind::Svg, SVG_1X1, 0, 22)),
        Some(Frame::Rendered(_))
    ));
    assert!(matches!(
        reply_or_timeout(&paused(JobKind::Auto, SVG_1X1, 0, 22)),
        Some(Frame::DetectedRendered {
            kind: MediaKind::Svg,
            ..
        })
    ));
}

/// Learning an Auto job's kind narrows its deadline from the job's arrival:
/// it does not start a new one. Each pause alone is within the raster
/// deadline; together they are not.
#[cfg(feature = "test-faults")]
#[test]
fn classification_does_not_restart_the_render_clock() {
    let png = png([10, 20, 30, 255]);
    assert!(matches!(
        reply_or_timeout(&paused(JobKind::Auto, &png, 0, 12)),
        Some(Frame::DetectedRendered {
            kind: MediaKind::Raster,
            ..
        })
    ));
    assert_eq!(reply_or_timeout(&paused(JobKind::Auto, &png, 12, 12)), None);
}
