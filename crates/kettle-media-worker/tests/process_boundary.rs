//! The worker binary end to end: its handshake and refusals, its watchdog,
//! and the setup it finishes before it reads anything.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::io::Read as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use kettle_media::wire::{Direction, Frame, encode, read_frame, write_frame};
use kettle_media::{
    BuildId, Canvas, Failure, FailureCode, FallbackFont, Hello, Job, JobKind, MediaKind,
    NativePath, PlayCommand, PlayOpen, PlayState, Ready, Source, Target, Theme, VideoFrame,
    Warning,
};

#[path = "../../kettle-media-render/tests/support/fonts.rs"]
mod fonts;
#[path = "../../kettle-media-render/tests/support/pdf.rs"]
mod pdf;

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

/// A PDF crosses the worker's boundary as the page asked for, with which
/// page it is of how many: an Auto job's reply says PDF and shows the first,
/// an explicit page comes back as asked, and a page past the last is refused
/// by its code. The worker renders it confined, as it must.
#[test]
fn a_pdf_page_crosses_the_boundary() {
    let document = pdf::pdf(
        100,
        50,
        &["1 0 0 rg 0 0 100 50 re f", "0 0 1 rg 0 0 100 50 re f"],
    );
    let Some(Frame::DetectedRendered { kind, rendered }) =
        only_reply(&job_of(JobKind::Auto, &document))
    else {
        panic!("no typed reply to an Auto PDF job");
    };
    assert_eq!(kind, MediaKind::Pdf);
    rendered.validate_as(MediaKind::Pdf).unwrap();
    assert_eq!(
        rendered.page,
        Some(kettle_media::PageOf { index: 0, count: 2 })
    );
    assert_eq!(rendered.rgba[..4], [255, 0, 0, 255]);
    let Some(Frame::Rendered(second)) =
        only_reply(&job_of(JobKind::PdfPage { index: 1 }, &document))
    else {
        panic!("no reply to an explicit PDF page");
    };
    assert_eq!(
        second.page,
        Some(kettle_media::PageOf { index: 1, count: 2 })
    );
    assert_eq!(second.rgba[..4], [0, 0, 255, 255]);
    assert_eq!(
        only_reply(&job_of(JobKind::PdfPage { index: 2 }, &document)),
        failure(FailureCode::IndexOutOfRange)
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
        // Bash itself: macOS's /bin/sh starts another shell, which the
        // worker's sandbox, running only the decoder's programs, does not.
        std::fs::write(&path, format!("#!/bin/bash\n{body}\n")).unwrap();
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

    let reply = reply_from(worker_with_decoder(&ffmpeg), &stills_of(&clip, sheet(4)));
    let Some(Frame::Rendered(rendered)) = reply else {
        panic!("no stills from the decoder: {reply:?}");
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
    assert_eq!(video.samples.len(), 4);
    for (sample, (requested, shown, keyframe)) in video.samples.iter().zip([
        (125, 100, 100), // No decode has been timed before the first sample.
        (375, 300, 0),
        (625, 600, 500),
        (875, 800, 500),
    ]) {
        assert_eq!(sample.requested_ms, requested);
        // Under load the deadline's pace can require the preceding keyframe.
        assert!(
            sample.actual_ms == shown || sample.actual_ms == keyframe,
            "unexpected sample {sample:?}"
        );
    }
    assert_eq!(
        u64::from(video.tolerance_ms),
        video
            .samples
            .iter()
            .map(|sample| sample.requested_ms.abs_diff(sample.actual_ms))
            .max()
            .unwrap()
    );
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

/// A video's cached thumbnail, attested as the parent found it, renders
/// across the boundary only while it names the video and its time; the
/// worker reads it through the sandbox like any other held file.
#[test]
fn a_cached_thumbnail_renders_only_for_its_video() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;
    let uri = "file:///home/user/Videos/clip.mp4";
    let cache = kettle_test_support::private_tempdir("kettle-worker-thumbnail-");
    let path = cache.path().join("0cc175b9c0f1b6a831c399e269772661.png");
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut encoder = png::Encoder::new(file, 2, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .add_text_chunk("Thumb::URI".into(), uri.into())
            .unwrap();
        encoder
            .add_text_chunk("Thumb::MTime".into(), "1696300000.5".into())
            .unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&[10, 20, 30, 255, 40, 50, 60, 255])
            .unwrap();
    }
    let metadata = std::fs::metadata(&path).unwrap();
    let thumbnail_of = |mtime_nanos| {
        let Frame::Job(mut job) = job_of(
            JobKind::CachedThumbnail(kettle_media::ThumbnailOf {
                uri_sha256: kettle_media::ThumbnailOf::uri_digest(uri),
                mtime_seconds: 1_696_300_000,
                mtime_nanos,
            }),
            b"",
        ) else {
            unreachable!()
        };
        job.source = Source::Path {
            path: NativePath::new(path.as_os_str().as_bytes().to_vec()).unwrap(),
            authorization: kettle_media::Authorization::ExternalAttested(
                kettle_media::ExternalAttested {
                    dev: metadata.dev(),
                    ino: metadata.ino(),
                },
            ),
        };
        job.target.width = 4;
        Frame::Job(job)
    };
    let Some(Frame::Rendered(rendered)) = reply_from(worker(), &thumbnail_of(500_000_000)) else {
        panic!("the thumbnail of the video did not render");
    };
    assert_eq!(
        (rendered.width, rendered.height, rendered.rgba),
        (2, 1, vec![10, 20, 30, 255, 40, 50, 60, 255])
    );
    assert_eq!(
        reply_from(worker(), &thumbnail_of(0)),
        failure(FailureCode::Changed)
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
    // Soft 5 s from the start; the hard limit is pinned once the worker
    // knows what it is for (a job's 5 s, or a session's).
    assert_eq!(
        limit("Max cpu time"),
        ("5".to_string(), "43200".to_string())
    );
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

/// `frame` with the `test-faults` worker's marker `marker` in its accent.
#[cfg(feature = "test-faults")]
fn marked(frame: Frame, marker: [u8; 2]) -> Frame {
    let Frame::Job(mut job) = frame else {
        unreachable!()
    };
    job.theme.accent = [marker[0], marker[1], 0, 0];
    Frame::Job(job)
}

/// A job runs confined: the file beside its source, the user's own and
/// readable, cannot be read from inside the job, whose video still renders
/// from the decoder (an unconfined worker reads it and fails the job).
#[cfg(feature = "test-faults")]
#[test]
fn a_job_cannot_read_beyond_what_it_holds() {
    let decoders = kettle_test_support::private_tempdir("kettle-worker-decoder-");
    let ffmpeg = stand_in_decoder(decoders.path());
    let clips = kettle_test_support::private_tempdir("kettle-worker-clip-");
    let clip = mp4_file(clips.path());
    std::fs::write(clips.path().join("secret"), b"not for the job").unwrap();
    let reply = reply_from(
        worker_with_decoder(&ffmpeg),
        &marked(stills_of(&clip, sheet(4)), *b"KS"),
    );
    assert!(matches!(reply, Some(Frame::Rendered(_))), "{reply:?}");
}

/// Where the worker cannot confine itself, a video is refused, never decoded
/// unconfined, while an image, which only Kettle's own code parses, still
/// renders.
#[cfg(feature = "test-faults")]
#[test]
fn without_a_sandbox_videos_are_refused_and_images_render() {
    let decoders = kettle_test_support::private_tempdir("kettle-worker-decoder-");
    let ffmpeg = stand_in_decoder(decoders.path());
    let clips = kettle_test_support::private_tempdir("kettle-worker-clip-");
    let clip = mp4_file(clips.path());
    assert_eq!(
        reply_from(
            worker_with_decoder(&ffmpeg),
            &marked(stills_of(&clip, sheet(4)), *b"KN"),
        ),
        failure(FailureCode::SandboxUnavailable)
    );
    let png = png([10, 20, 30, 255]);
    assert!(matches!(
        reply_from(worker(), &marked(job(&png), *b"KN")),
        Some(Frame::Rendered(_))
    ));
    // Nor is a PDF parsed unconfined, however it was asked for.
    let document = pdf::pdf(10, 10, &["1 0 0 rg 0 0 10 10 re f"]);
    for kind in [JobKind::Auto, JobKind::PdfPage { index: 0 }] {
        assert_eq!(
            reply_from(worker(), &marked(job_of(kind, &document), *b"KN")),
            failure(FailureCode::SandboxUnavailable),
            "{kind:?}"
        );
    }
}

/// A job counts as confined only if the watchdog, started earlier, confined
/// itself too: a worker whose watchdog could not refuses a video.
#[cfg(feature = "test-faults")]
#[test]
fn a_watchdog_left_unconfined_refuses_videos() {
    let decoders = kettle_test_support::private_tempdir("kettle-worker-decoder-");
    let ffmpeg = stand_in_decoder(decoders.path());
    let clips = kettle_test_support::private_tempdir("kettle-worker-clip-");
    let clip = mp4_file(clips.path());
    let worker = Command::new(WORKER)
        .env_clear()
        .env(kettle_media::video::DECODER_ENV, &ffmpeg)
        .env("KETTLE_MEDIA_TEST_UNCONFINED_WATCHDOG", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert_eq!(
        reply_from(worker, &stills_of(&clip, sheet(4))),
        failure(FailureCode::SandboxUnavailable)
    );
}

// ---- Playback sessions ----

/// A worker in a process group of its own, as Kettle starts one: a session
/// whose parent goes quiet kills its whole group, which must not be the
/// test's.
fn grouped_worker() -> Child {
    use std::os::unix::process::CommandExt as _;
    spawn(Command::new(WORKER).process_group(0))
}

/// A file the `test-faults` worker plays with its stand-in decoder: an MP4
/// box, then `frames fps_milli width height` and the stand-in's faults.
#[cfg(feature = "test-faults")]
fn fake_video(directory: &std::path::Path, spec: &str) -> std::path::PathBuf {
    let path = directory.join("video.mp4");
    let mut bytes = vec![0, 0, 0, 24];
    bytes.extend_from_slice(b"ftypisom\0\0\x02\0isomiso2");
    bytes.extend_from_slice(format!("kettle-media-worker-test-video {spec}\n").as_bytes());
    bytes.resize(bytes.len() + 64, 0);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn play_open(path: &std::path::Path, room: (u32, u32), start_ms: u64) -> Frame {
    Frame::PlayOpen(PlayOpen {
        source: Source::user_pull(
            NativePath::from_path(path).unwrap(),
            kettle_media::GuiActionWitness::from_explicit_gui_action(),
        ),
        max_width: room.0,
        max_height: room.1,
        start_ms,
        max_fps_milli: 30_000,
        audio: kettle_media::AudioRequest::On,
    })
}

/// [`play_open`] asking for no sound, as Kettle opens a video only an
/// external decoder plays.
fn silent_open(path: &std::path::Path, room: (u32, u32), start_ms: u64) -> Frame {
    let Frame::PlayOpen(open) = play_open(path, room, start_ms) else {
        unreachable!()
    };
    Frame::PlayOpen(PlayOpen {
        audio: kettle_media::AudioRequest::Off,
        ..open
    })
}

fn command(child: &mut Child, command: PlayCommand) {
    send(child, &Frame::PlayCommand(command));
}

/// Keep the lease until this fixture's last frame, then wait for clean exit.
/// Waiting makes a heartbeat written after completion fail deterministically.
fn heartbeat_before_last_frame(child: &mut Child, pts_ms: u64, last_pts_ms: u64) {
    if pts_ms == last_pts_ms {
        assert_eq!(exit_code(child, 5 * SECOND), 0);
    } else {
        command(child, PlayCommand::Heartbeat);
    }
}

/// The worker's frames as they come, read on a thread of their own so a
/// test can wait for one with a deadline.
struct Frames(std::sync::mpsc::Receiver<Option<Frame>>);

impl Frames {
    fn of(child: &mut Child) -> Self {
        let mut stdout = child.stdout.take().unwrap();
        let (sender, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let frame = read_frame(&mut stdout, Direction::WorkerToParent).unwrap();
                let end = frame.is_none();
                if sender.send(frame).is_err() || end {
                    return;
                }
            }
        });
        Self(received)
    }

    /// The next frame within `within`, or `None` when the worker closed
    /// its output; panics when nothing comes.
    fn next(&self, within: Duration) -> Option<Frame> {
        self.0
            .recv_timeout(within)
            .unwrap_or_else(|_| panic!("no frame within {within:?}"))
    }

    /// Whether nothing comes within `within`.
    #[cfg(feature = "test-faults")]
    fn quiet(&self, within: Duration) -> bool {
        self.0.recv_timeout(within).is_err()
    }
}

fn video_frame(frame: Option<Frame>) -> VideoFrame {
    match frame {
        Some(Frame::VideoFrame(frame)) => frame,
        other => panic!("expected a video frame, got {other:?}"),
    }
}

const SECOND: Duration = Duration::from_secs(1);

/// A session opens paused on its first frame, then plays every frame in
/// order at its time, each numbered and timed, and ends after the last.
#[cfg(feature = "test-faults")]
#[test]
fn a_session_opens_paused_then_plays_each_frame_at_its_time() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let video = fake_video(directory.path(), "10 20000 64 36");
    let mut child = grouped_worker();
    handshake(&mut child);
    send(&mut child, &play_open(&video, (32, 32), 0));
    let frames = Frames::of(&mut child);
    let Some(Frame::Opened(opened)) = frames.next(5 * SECOND) else {
        panic!("not opened");
    };
    assert_eq!(
        (
            opened.width,
            opened.height,
            opened.fps_milli,
            opened.start_ms
        ),
        (32, 18, 20_000, 0)
    );
    assert_eq!(opened.info.duration_ms, 500);
    let first = video_frame(frames.next(5 * SECOND));
    assert_eq!((first.seq, first.pts_ms, first.rgba[0]), (0, 0, 0));
    assert_eq!(first.rgba.len(), 32 * 18 * 4);
    assert_eq!(
        frames.next(SECOND),
        Some(Frame::PlayState(PlayState::Paused))
    );
    // Paused, nothing more comes.
    assert!(frames.quiet(Duration::from_millis(300)));
    let started = Instant::now();
    command(&mut child, PlayCommand::Play);
    assert_eq!(
        frames.next(SECOND),
        Some(Frame::PlayState(PlayState::Playing))
    );
    for index in 1..10u64 {
        let frame = video_frame(frames.next(SECOND));
        assert_eq!(
            (frame.seq, frame.pts_ms, u64::from(frame.rgba[0])),
            (index, index * 50, index)
        );
    }
    // Nine frames after the first, fifty ms apart: paced, not flushed.
    assert!(started.elapsed() >= Duration::from_millis(350));
    assert_eq!(
        frames.next(SECOND),
        Some(Frame::PlayState(PlayState::Ended))
    );
    assert_eq!(frames.next(SECOND), None);
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);
}

/// Pause holds the frames, Play takes them up where they stopped, and a
/// session opened later in the video starts there.
#[cfg(feature = "test-faults")]
#[test]
fn a_paused_session_holds_and_resumes_where_it_stopped() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let video = fake_video(directory.path(), "1000 20000 16 16");
    let mut child = grouped_worker();
    handshake(&mut child);
    send(&mut child, &play_open(&video, (16, 16), 1000));
    let frames = Frames::of(&mut child);
    assert!(
        matches!(frames.next(5 * SECOND), Some(Frame::Opened(opened)) if opened.start_ms == 1000)
    );
    let first = video_frame(frames.next(5 * SECOND));
    assert_eq!((first.seq, first.pts_ms), (0, 1000));
    assert_eq!(
        frames.next(SECOND),
        Some(Frame::PlayState(PlayState::Paused))
    );
    command(&mut child, PlayCommand::Play);
    assert_eq!(
        frames.next(SECOND),
        Some(Frame::PlayState(PlayState::Playing))
    );
    let mut last = 0;
    for _ in 0..3 {
        last = video_frame(frames.next(SECOND)).pts_ms;
    }
    command(&mut child, PlayCommand::Pause);
    // A frame already on its way may arrive before the state does.
    loop {
        match frames.next(SECOND) {
            Some(Frame::VideoFrame(frame)) => last = frame.pts_ms,
            Some(Frame::PlayState(PlayState::Paused)) => break,
            other => panic!("expected Paused, got {other:?}"),
        }
    }
    assert!(frames.quiet(Duration::from_millis(400)));
    command(&mut child, PlayCommand::Play);
    assert_eq!(
        frames.next(SECOND),
        Some(Frame::PlayState(PlayState::Playing))
    );
    assert_eq!(video_frame(frames.next(SECOND)).pts_ms, last + 50);
    command(&mut child, PlayCommand::Close);
    let rest: Vec<Frame> = std::iter::from_fn(|| frames.next(SECOND)).collect();
    assert!(
        rest.iter()
            .all(|frame| matches!(frame, Frame::VideoFrame(_)))
    );
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);
}

/// A parent that goes quiet, its pipes still open, loses its session: the
/// worker kills its own group once the lease lapses, from playing or paused.
#[cfg(feature = "test-faults")]
#[test]
fn a_session_ends_when_its_parent_goes_quiet() {
    use std::os::unix::process::ExitStatusExt as _;
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let video = fake_video(directory.path(), "100000 10000 16 16");
    for play in [false, true] {
        let mut child = grouped_worker();
        handshake(&mut child);
        send(&mut child, &play_open(&video, (16, 16), 0));
        let frames = Frames::of(&mut child);
        let started = Instant::now();
        if play {
            command(&mut child, PlayCommand::Play);
        }
        // Heartbeats keep it for two seconds, then stop.
        for _ in 0..2 {
            std::thread::sleep(SECOND);
            command(&mut child, PlayCommand::Heartbeat);
        }
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                started.elapsed() < 10 * SECOND,
                "the session outlived its lease"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let ended = started.elapsed();
        // What it said besides frames, should it end otherwise.
        std::thread::sleep(Duration::from_millis(100));
        let said: Vec<Frame> = std::iter::from_fn(|| frames.0.try_recv().ok().flatten())
            .filter(|frame| !matches!(frame, Frame::VideoFrame(_)))
            .collect();
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "playing: {play}, {status:?} after {ended:?}, said {said:?}"
        );
        assert!(ended >= 6 * SECOND, "ended before its lease lapsed");
    }
}

/// Open a session on `video` with room for frames of 512 a side, and take
/// Opened, its first frame and Paused.
#[cfg(feature = "test-faults")]
fn opened_large(video: &std::path::Path) -> Child {
    let mut child = grouped_worker();
    handshake(&mut child);
    send(&mut child, &play_open(video, (512, 512), 0));
    assert!(matches!(receive(&mut child), Some(Frame::Opened(_))));
    assert!(matches!(receive(&mut child), Some(Frame::VideoFrame(_))));
    assert_eq!(
        receive(&mut child),
        Some(Frame::PlayState(PlayState::Paused))
    );
    child
}

/// A first frame whose write the parent holds past the opening deadline,
/// heartbeats coming all the while, still fails the session as timed out:
/// it opened late, whatever was read.
#[cfg(feature = "test-faults")]
#[test]
fn a_first_frame_written_past_the_deadline_is_a_timeout() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    // A frame of a megabyte, far more than a pipe holds.
    let video = fake_video(directory.path(), "100000 30000 512 512");
    let mut child = grouped_worker();
    handshake(&mut child);
    send(&mut child, &play_open(&video, (512, 512), 0));
    let opened = Instant::now();
    assert!(matches!(receive(&mut child), Some(Frame::Opened(_))));
    // Heartbeats keep the lease until well past the deadline; the last at
    // 4.5 s, so none can meet a worker its watchdog has ended.
    while opened.elapsed() < Duration::from_millis(4_500) {
        std::thread::sleep(Duration::from_millis(500));
        command(&mut child, PlayCommand::Heartbeat);
    }
    // Read just past the 5 s deadline. This clock started after the
    // worker's, so its deadline has passed, with most of the second before
    // its watchdog left for a slow runner.
    let resume = opened + Duration::from_millis(5_150);
    if let Some(wait) = resume.checked_duration_since(Instant::now()) {
        std::thread::sleep(wait);
    }
    assert!(matches!(receive(&mut child), Some(Frame::VideoFrame(_))));
    assert_eq!(
        receive(&mut child),
        Some(Frame::Failure(Failure {
            code: FailureCode::RenderTimeout
        }))
    );
    assert_eq!(exit_code(&mut child, 3 * SECOND), 0);
}

/// Close, or a closed stdin, ends a session at once and with 0 even while
/// the parent reads none of its frames, its loop held in a write, and more
/// heartbeats came first than any queue holds.
#[cfg(feature = "test-faults")]
#[test]
fn a_session_closes_while_its_frames_go_unread() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    // Frames of a megabyte, far more than a pipe holds.
    let video = fake_video(directory.path(), "100000 30000 512 512");
    for by_close in [true, false] {
        let mut child = opened_large(&video);
        command(&mut child, PlayCommand::Play);
        std::thread::sleep(Duration::from_millis(500));
        for _ in 0..20 {
            command(&mut child, PlayCommand::Heartbeat);
        }
        let closed = Instant::now();
        if by_close {
            command(&mut child, PlayCommand::Close);
        } else {
            drop(child.stdin.take());
        }
        assert_eq!(exit_code(&mut child, 3 * SECOND), 0, "close: {by_close}");
        assert!(closed.elapsed() < 2 * SECOND, "close: {by_close}");
    }
}

/// Close, or a closed stdin, ends a session at once and with 0 while its
/// decoder is still opening the video.
#[cfg(feature = "test-faults")]
#[test]
fn a_session_closes_while_its_decoder_opens() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let video = fake_video(directory.path(), "10 10000 16 16 open 3000");
    for by_close in [true, false] {
        let mut child = grouped_worker();
        handshake(&mut child);
        send(&mut child, &play_open(&video, (16, 16), 0));
        std::thread::sleep(Duration::from_millis(200));
        let closed = Instant::now();
        if by_close {
            command(&mut child, PlayCommand::Close);
        } else {
            drop(child.stdin.take());
        }
        assert_eq!(exit_code(&mut child, 3 * SECOND), 0, "close: {by_close}");
        assert!(
            closed.elapsed() < Duration::from_millis(1_500),
            "close: {by_close}, {:?}",
            closed.elapsed()
        );
    }
}

/// Close that comes just before the watchdog would end a session held in a
/// write still ends it with 0.
#[cfg(feature = "test-faults")]
#[test]
fn close_just_before_the_watchdog_still_ends_with_0() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let video = fake_video(directory.path(), "100000 30000 512 512");
    let mut child = opened_large(&video);
    command(&mut child, PlayCommand::Play);
    // Its loop is held from about here; the watchdog would end it 5 s on.
    std::thread::sleep(Duration::from_millis(4_600));
    command(&mut child, PlayCommand::Close);
    assert_eq!(exit_code(&mut child, 3 * SECOND), 0);
}

/// The parent leaving just before its lease would lapse, the session held
/// in a write, still ends it with 0: the lease ends with the parent, and
/// leaving renews nothing.
#[cfg(feature = "test-faults")]
#[test]
fn leaving_just_before_the_lease_lapses_still_ends_with_0() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let video = fake_video(directory.path(), "100000 30000 512 512");
    let mut child = opened_large(&video);
    command(&mut child, PlayCommand::Play);
    // Its loop is held from about here, its lease running to 5 s.
    std::thread::sleep(Duration::from_millis(4_700));
    drop(child.stdin.take());
    assert_eq!(exit_code(&mut child, 3 * SECOND), 0);
}

/// A heartbeat that comes while the session is held in a write keeps it,
/// even with decoded frames queued before it: the session does not end on
/// the lease of the command before.
#[cfg(feature = "test-faults")]
#[test]
fn a_heartbeat_during_a_held_write_keeps_the_session() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let video = fake_video(directory.path(), "100000 30000 512 512");
    let mut child = opened_large(&video);
    let started = Instant::now();
    command(&mut child, PlayCommand::Play);
    // Held in a write from about here, its lease running to 5 s.
    std::thread::sleep(Duration::from_millis(4_000));
    command(&mut child, PlayCommand::Heartbeat);
    std::thread::sleep(Duration::from_millis(500));
    // Read again just past the old lease.
    let frames = Frames::of(&mut child);
    while started.elapsed() < Duration::from_millis(7_000) {
        assert!(
            child.try_wait().unwrap().is_none(),
            "ended at {:?}",
            started.elapsed()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    command(&mut child, PlayCommand::Close);
    assert_eq!(exit_code(&mut child, 3 * SECOND), 0);
    drop(frames);
}

/// The lease runs out on time while the session is held in a write: four
/// seconds of reading after the last command, then none, and the worker's
/// group is killed five seconds after that command, not at the watchdog.
#[cfg(feature = "test-faults")]
#[test]
fn the_lease_runs_out_during_a_held_write() {
    use std::os::unix::process::ExitStatusExt as _;
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let video = fake_video(directory.path(), "100000 30000 512 512");
    let mut child = opened_large(&video);
    command(&mut child, PlayCommand::Play);
    let last = Instant::now();
    while last.elapsed() < Duration::from_millis(4_000) {
        assert!(receive(&mut child).is_some());
    }
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            last.elapsed() < 10 * SECOND,
            "the session outlived its lease"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    let ended = last.elapsed();
    assert!(
        ended >= Duration::from_millis(4_800) && ended <= Duration::from_millis(6_500),
        "ended {ended:?} after the last command"
    );
}

/// A heartbeat that waits behind a write the parent is slow to read renews
/// the lease from when it came: the session ends five seconds after it, not
/// five after the session took it.
#[cfg(feature = "test-faults")]
#[test]
fn a_heartbeat_held_behind_a_write_counts_from_when_it_came() {
    use std::os::unix::process::ExitStatusExt as _;
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let video = fake_video(directory.path(), "100000 30000 512 512");
    let mut child = opened_large(&video);
    command(&mut child, PlayCommand::Play);
    std::thread::sleep(Duration::from_millis(500));
    // The last command; then three seconds of frames unread.
    command(&mut child, PlayCommand::Heartbeat);
    let last = Instant::now();
    std::thread::sleep(3 * SECOND);
    let frames = Frames::of(&mut child);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            last.elapsed() < 10 * SECOND,
            "the session outlived its lease"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    let ended = last.elapsed();
    assert!(
        ended >= Duration::from_millis(4_500) && ended <= Duration::from_millis(6_500),
        "ended {ended:?} after the last heartbeat"
    );
    drop(frames);
}

/// A session whose frames the parent stops reading ends with the process,
/// heartbeats or not: a blocked write cannot hold its decoder.
#[cfg(feature = "test-faults")]
#[test]
fn a_session_whose_frames_go_unread_ends() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    // Each frame is larger than a pipe holds.
    let video = fake_video(directory.path(), "100000 30000 640 360");
    let mut child = grouped_worker();
    handshake(&mut child);
    send(&mut child, &play_open(&video, (640, 360), 0));
    let started = Instant::now();
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code().unwrap_or(-1);
        }
        assert!(
            started.elapsed() < 15 * SECOND,
            "the unread session never ended"
        );
        // Ignored once the worker has gone.
        let _ = write_frame(
            child.stdin.as_mut().unwrap(),
            &Frame::PlayCommand(PlayCommand::Heartbeat),
            Direction::ParentToWorker,
        );
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(code, EXIT_TIMEOUT);
}

/// A decoder that stops giving frames while the session plays fails it;
/// one that fails says why; a parent that closes its end or sends anything
/// but a command ends it.
#[cfg(feature = "test-faults")]
#[test]
fn a_session_ends_on_a_stall_a_failure_or_a_broken_parent() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let open = |spec: &str| {
        let video = fake_video(directory.path(), spec);
        let mut child = grouped_worker();
        handshake(&mut child);
        send(&mut child, &play_open(&video, (16, 16), 0));
        let frames = Frames::of(&mut child);
        assert!(matches!(frames.next(5 * SECOND), Some(Frame::Opened(_))));
        video_frame(frames.next(5 * SECOND));
        assert_eq!(
            frames.next(SECOND),
            Some(Frame::PlayState(PlayState::Paused))
        );
        (child, frames)
    };
    let heartbeat_until = |child: &mut Child, frames: &Frames, end: fn(&Frame) -> bool| {
        let started = Instant::now();
        let mut input_open = true;
        loop {
            assert!(started.elapsed() < 15 * SECOND, "the session never ended");
            if input_open {
                match write_frame(
                    child.stdin.as_mut().unwrap(),
                    &Frame::PlayCommand(PlayCommand::Heartbeat),
                    Direction::ParentToWorker,
                ) {
                    Ok(()) => {}
                    // A closed input still requires the expected terminal reply.
                    Err(kettle_media::wire::WireError::Io) => input_open = false,
                    Err(error) => panic!("heartbeat failed: {error:?}"),
                }
            }
            match frames.0.recv_timeout(Duration::from_millis(500)) {
                Ok(Some(frame)) if end(&frame) => return frame,
                Ok(Some(Frame::VideoFrame(_) | Frame::PlayState(_))) | Err(_) => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    };

    let (mut child, frames) = open("100 20000 16 16 stall 3");
    command(&mut child, PlayCommand::Play);
    let stalled = Instant::now();
    let frame = heartbeat_until(&mut child, &frames, |frame| {
        matches!(frame, Frame::Failure(_))
    });
    assert_eq!(
        frame,
        Frame::Failure(Failure {
            code: FailureCode::RenderTimeout
        })
    );
    assert!(stalled.elapsed() >= 5 * SECOND);
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);

    let (mut child, frames) = open("100 20000 16 16 fail 2");
    command(&mut child, PlayCommand::Play);
    // The terminal reply can be queued after the worker has closed stdin.
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);
    let frame = heartbeat_until(&mut child, &frames, |frame| {
        matches!(frame, Frame::Failure(_))
    });
    assert_eq!(
        frame,
        Frame::Failure(Failure {
            code: FailureCode::RenderParse
        })
    );
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);

    let (mut child, frames) = open("100 20000 16 16");
    drop(child.stdin.take());
    assert_eq!(frames.next(SECOND), None);
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);

    // Anything larger than a command is refused before it is read; a
    // frame of a command's size that is no command, as it is.
    let (mut child, frames) = open("100 20000 16 16");
    send(&mut child, &hello(own_build()));
    assert_eq!(frames.next(SECOND), failure(FailureCode::TooLarge));
    assert_eq!(exit_code(&mut child, 5 * SECOND), EXIT_PROTOCOL);
    let (mut child, frames) = open("100 20000 16 16");
    send_bytes(
        &mut child,
        &encode(
            &Frame::Failure(Failure {
                code: FailureCode::BadParams,
            }),
            Direction::WorkerToParent,
        )
        .unwrap(),
    );
    assert_eq!(frames.next(SECOND), failure(FailureCode::BadParams));
    assert_eq!(exit_code(&mut child, 5 * SECOND), EXIT_PROTOCOL);
}

/// A file changed while its session plays fails it: what plays is the file
/// as it was opened, or nothing.
#[cfg(feature = "test-faults")]
#[test]
fn a_session_whose_file_changes_fails() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let video = fake_video(directory.path(), "100000 10000 16 16");
    let mut child = grouped_worker();
    handshake(&mut child);
    send(&mut child, &play_open(&video, (16, 16), 0));
    let frames = Frames::of(&mut child);
    assert!(matches!(frames.next(5 * SECOND), Some(Frame::Opened(_))));
    command(&mut child, PlayCommand::Play);
    std::thread::sleep(Duration::from_millis(300));
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&video)
            .unwrap();
        file.write_all(b"more").unwrap();
    }
    let deadline = Instant::now() + 5 * SECOND;
    loop {
        match frames.next(5 * SECOND) {
            Some(Frame::Failure(failure)) => {
                assert_eq!(failure.code, FailureCode::Changed);
                break;
            }
            Some(_) => assert!(Instant::now() < deadline, "the change went unnoticed"),
            None => panic!("ended without a failure"),
        }
    }
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);
}

/// A file changed while its decoder opens it is Changed, whether the
/// decoder then opens it or fails.
#[cfg(feature = "test-faults")]
#[test]
fn a_file_changed_while_its_decoder_opens_is_changed() {
    for spec in ["10 10000 16 16 open 800", "10 10000 16 16 open 800 refuse"] {
        let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
        let video = fake_video(directory.path(), spec);
        let mut child = grouped_worker();
        handshake(&mut child);
        send(&mut child, &play_open(&video, (16, 16), 0));
        let frames = Frames::of(&mut child);
        std::thread::sleep(Duration::from_millis(300));
        {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&video)
                .unwrap();
            file.write_all(b"more").unwrap();
        }
        assert_eq!(
            frames.next(5 * SECOND),
            failure(FailureCode::Changed),
            "{spec}"
        );
        assert_eq!(exit_code(&mut child, 5 * SECOND), 0, "{spec}");
    }
}

/// A session's processor time is what its video needs: half a minute and a
/// second for each second left, set once it is open; a job's stays 5 s.
#[cfg(all(feature = "test-faults", target_os = "linux"))]
#[test]
fn a_session_s_processor_time_follows_its_video() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    // 2000 frames at 10 fps: 200 s, opened 50 s in.
    let video = fake_video(directory.path(), "2000 10000 16 16");
    let mut child = grouped_worker();
    handshake(&mut child);
    send(&mut child, &play_open(&video, (16, 16), 50_000));
    let frames = Frames::of(&mut child);
    assert!(matches!(frames.next(5 * SECOND), Some(Frame::Opened(_))));
    let limits = std::fs::read_to_string(format!("/proc/{}/limits", child.id())).unwrap();
    let line = limits
        .lines()
        .find(|line| line.starts_with("Max cpu time"))
        .unwrap();
    let fields: Vec<&str> = line["Max cpu time".len()..].split_whitespace().collect();
    assert_eq!((fields[0], fields[1]), ("180", "180"));
    command(&mut child, PlayCommand::Close);
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);
}

/// A session with no decoder to open it is refused, and so is one whose
/// file is not a video.
#[test]
fn a_session_needs_a_decoder_and_a_video() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let clip = mp4_file(directory.path());
    let mut child = grouped_worker();
    handshake(&mut child);
    send(&mut child, &play_open(&clip, (64, 64), 0));
    // macOS tries Apple's decoder, which cannot read the stand-in file.
    let without = failure(if cfg!(target_os = "macos") {
        FailureCode::UnsupportedContainer
    } else {
        FailureCode::BackendUnavailable
    });
    assert_eq!(receive(&mut child), without);
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);

    let text = directory.path().join("notes.txt");
    std::fs::write(&text, b"not a video at all").unwrap();
    let mut child = grouped_worker();
    handshake(&mut child);
    send(&mut child, &play_open(&text, (64, 64), 0));
    assert_eq!(receive(&mut child), failure(FailureCode::UnsupportedMedia));
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);
}

/// The trusted ffmpeg installed here, if any.
fn installed_ffmpeg() -> Option<std::path::PathBuf> {
    let found = kettle_media_native::ffmpeg::Ffmpeg::search(
        std::env::var_os("HOME")
            .as_deref()
            .map(std::path::Path::new),
    );
    if found.is_none() {
        eprintln!("skipped: no trusted ffmpeg is installed");
    }
    found.map(|ffmpeg| ffmpeg.named().to_path_buf())
}

/// A fixture clip whose frames carry their index, copied somewhere private:
/// the MP4, which Apple's decoder reads on macOS.
#[cfg(target_os = "macos")]
fn index_clip(directory: &std::path::Path) -> std::path::PathBuf {
    fixture_clip(directory, "index.mp4")
}

/// A fixture clip copied somewhere private: the WebM, which only ffmpeg
/// reads, on macOS too.
fn fixture_clip(directory: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = directory.join(name);
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../kettle-media-native/tests/fixtures/video")
            .join(name),
        &path,
    )
    .unwrap();
    path
}

fn grouped_worker_with_decoder(ffmpeg: &std::path::Path) -> Child {
    use std::os::unix::process::CommandExt as _;
    Command::new(WORKER)
        .env_clear()
        .env(kettle_media::video::DECODER_ENV, ffmpeg)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// A session asking for sound runs no external decoder: on macOS it is
/// confined for sound with Apple's decoder alone, which turns WebM down, so
/// the WebM clip is refused even with a trusted ffmpeg at hand. Asked
/// without sound, ffmpeg plays it (`real_ffmpeg_plays_a_session_to_its_end`).
#[cfg(target_os = "macos")]
#[test]
fn a_session_asking_for_sound_runs_no_external_decoder() {
    let Some(ffmpeg) = installed_ffmpeg() else {
        return;
    };
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let clip = fixture_clip(directory.path(), "index.webm");
    let mut child = grouped_worker_with_decoder(&ffmpeg);
    handshake(&mut child);
    send(&mut child, &play_open(&clip, (64, 36), 0));
    assert_eq!(
        receive(&mut child),
        Some(Frame::Failure(Failure {
            code: FailureCode::UnsupportedContainer
        }))
    );
    assert_eq!(exit_code(&mut child, 3 * SECOND), 0);
}

/// With a real ffmpeg, a session asking for no sound plays every frame of
/// the WebM clip in order, confined to the file and its decoder, and ends;
/// on macOS it gets there after Apple's decoder turns WebM down.
#[test]
fn real_ffmpeg_plays_a_session_to_its_end() {
    let Some(ffmpeg) = installed_ffmpeg() else {
        return;
    };
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let clip = fixture_clip(directory.path(), "index.webm");
    let mut child = grouped_worker_with_decoder(&ffmpeg);
    handshake(&mut child);
    send(&mut child, &silent_open(&clip, (64, 36), 2000));
    let frames = Frames::of(&mut child);
    let opened = match frames.next(5 * SECOND) {
        Some(Frame::Opened(opened)) => opened,
        other => panic!("not opened: {other:?}"),
    };
    assert_eq!(
        (opened.width, opened.height, opened.start_ms),
        (64, 36, 2000)
    );
    let first = video_frame(frames.next(5 * SECOND));
    assert_eq!(first.pts_ms, 2000);
    assert_eq!(
        frames.next(SECOND),
        Some(Frame::PlayState(PlayState::Paused))
    );
    command(&mut child, PlayCommand::Play);
    assert_eq!(
        frames.next(SECOND),
        Some(Frame::PlayState(PlayState::Playing))
    );
    let mut times = vec![first.pts_ms];
    loop {
        match frames.next(5 * SECOND) {
            Some(Frame::VideoFrame(frame)) => {
                heartbeat_before_last_frame(&mut child, frame.pts_ms, 3_900);
                times.push(frame.pts_ms);
            }
            Some(Frame::PlayState(PlayState::Ended)) => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    // Paced at ten a second, none dropped on an idle machine is not
    // promised: every one sent is in order and on the clip's frames.
    assert!(times.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(
        times
            .iter()
            .all(|time| time % 100 == 0 && (2000..4000).contains(time))
    );
    assert_eq!(times.last(), Some(&3900));
    assert_eq!(frames.next(SECOND), None);
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);
}

/// A paused session whose parent goes quiet takes its ffmpeg with it: once
/// the lease lapses no process of the group is left.
#[test]
fn a_quiet_parent_s_session_leaves_no_decoder_behind() {
    let Some(ffmpeg) = installed_ffmpeg() else {
        return;
    };
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let clip = fixture_clip(directory.path(), "index.webm");
    let mut child = grouped_worker_with_decoder(&ffmpeg);
    let group = child.id() as libc::pid_t;
    handshake(&mut child);
    send(&mut child, &silent_open(&clip, (64, 36), 0));
    let frames = Frames::of(&mut child);
    assert!(matches!(frames.next(5 * SECOND), Some(Frame::Opened(_))));
    video_frame(frames.next(5 * SECOND));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < 10 * SECOND,
            "the session outlived its lease"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    // The group is empty once its last member, killed with it, is reaped
    // by whoever inherited it.
    let emptied = Instant::now();
    // SAFETY: kill with signal 0 only asks whether the group has a member.
    while unsafe { libc::kill(-group, 0) } == 0 {
        assert!(
            emptied.elapsed() < 2 * SECOND,
            "a decoder outlived its session"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(frames);
}

/// A job's processor time is pinned to 5 s, soft and hard, as it arrives;
/// before that the hard limit leaves a session room.
#[cfg(all(feature = "test-faults", target_os = "linux"))]
#[test]
fn a_job_pins_its_processor_time_as_it_arrives() {
    let mut child = worker();
    handshake(&mut child);
    assert_eq!(cpu_limit(&child), ("5".to_string(), "43200".to_string()));
    // Paused a second before it is classified.
    let Frame::Job(mut paused) = job(&png([0, 0, 0, 255])) else {
        unreachable!()
    };
    paused.theme.accent = [b'K', b'T', 10, 0];
    send(&mut child, &Frame::Job(paused));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(cpu_limit(&child), ("5".to_string(), "5".to_string()));
    assert!(matches!(receive(&mut child), Some(Frame::Rendered(_))));
    drop(child.stdin.take());
    assert_eq!(exit_code(&mut child, Duration::from_secs(10)), 0);
}

/// The worker's processor-time limit, soft and hard, as /proc says.
#[cfg(all(feature = "test-faults", target_os = "linux"))]
fn cpu_limit(child: &Child) -> (String, String) {
    let limits = std::fs::read_to_string(format!("/proc/{}/limits", child.id())).unwrap();
    let line = limits
        .lines()
        .find(|line| line.starts_with("Max cpu time"))
        .unwrap();
    let fields: Vec<&str> = line["Max cpu time".len()..].split_whitespace().collect();
    (fields[0].to_string(), fields[1].to_string())
}

/// A parent that stops reading for a while gets the frames that follow at
/// their pace, every one in order, never in a burst to catch up: the session
/// runs behind instead.
#[cfg(feature = "test-faults")]
#[test]
fn a_late_reader_gets_frames_at_their_pace_not_in_a_burst() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    // Each frame a quarter of a pipe: a few fill it.
    let video = fake_video(directory.path(), "1000 20000 64 64");
    let mut child = grouped_worker();
    handshake(&mut child);
    send(&mut child, &play_open(&video, (64, 64), 0));
    let mut times = Vec::new();
    while times.is_empty() {
        match receive(&mut child) {
            Some(Frame::VideoFrame(frame)) => times.push(frame.pts_ms),
            Some(_) => {}
            None => panic!("the session ended before its first frame"),
        }
    }
    command(&mut child, PlayCommand::Play);
    // Not read for a second and a half, at twenty frames a second.
    std::thread::sleep(Duration::from_millis(1500));
    command(&mut child, PlayCommand::Heartbeat);
    // Then read on a thread of its own, each frame timed as it is read, so
    // the test's own scheduling neither stalls nor compresses the reading,
    // and a session that ends or stalls fails rather than hangs.
    let mut stdout = child.stdout.take().unwrap();
    let (sender, read) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let frame = read_frame(&mut stdout, Direction::WorkerToParent).unwrap();
            let end = frame.is_none();
            if sender.send((Instant::now(), frame)).is_err() || end {
                return;
            }
        }
    });
    let mut arrived = Vec::new();
    // A parent that reads late still keeps the lease, a heartbeat a second.
    let mut beat = Instant::now();
    let give_up = Instant::now() + 30 * SECOND;
    while times.len() < 60 {
        if beat.elapsed() >= SECOND {
            command(&mut child, PlayCommand::Heartbeat);
            beat = Instant::now();
        }
        match read.recv_timeout(Duration::from_millis(200)) {
            Ok((at, Some(Frame::VideoFrame(frame)))) => {
                times.push(frame.pts_ms);
                arrived.push(at);
            }
            Ok((_, Some(_))) => {}
            Ok((_, None)) => panic!("the session ended after {} frames", times.len()),
            Err(_) => assert!(
                Instant::now() < give_up,
                "no frames for 30 s, after {}",
                times.len()
            ),
        }
    }
    assert_eq!(
        times,
        (0..60).map(|index| index * 50).collect::<Vec<_>>(),
        "every frame, in order"
    );
    // What the pipe held comes at once; the forty after it at their pace.
    let paced = arrived[arrived.len() - 1] - arrived[arrived.len() - 41];
    assert!(paced >= Duration::from_millis(1700), "a burst: {paced:?}");
    command(&mut child, PlayCommand::Close);
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);
}

/// A worker that does not lead its process group, as one started by hand
/// does not, signals no group when its lease lapses: it exits, and only its
/// own decoder can go with it.
#[cfg(feature = "test-faults")]
#[test]
fn a_session_outside_its_own_group_just_exits() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let video = fake_video(directory.path(), "100000 10000 16 16");
    let mut child = worker();
    handshake(&mut child);
    send(&mut child, &play_open(&video, (16, 16), 0));
    let frames = Frames::of(&mut child);
    assert!(matches!(frames.next(5 * SECOND), Some(Frame::Opened(_))));
    assert_eq!(exit_code(&mut child, 10 * SECOND), EXIT_TIMEOUT);
    drop(frames);
}

/// A session whose first frame never comes fails as timed out, within the
/// open's 5 s.
#[cfg(feature = "test-faults")]
#[test]
fn a_session_whose_first_frame_never_comes_times_out() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let video = fake_video(directory.path(), "100 20000 16 16 stall 0");
    let mut child = grouped_worker();
    handshake(&mut child);
    let started = Instant::now();
    send(&mut child, &play_open(&video, (16, 16), 0));
    let frames = Frames::of(&mut child);
    assert!(matches!(frames.next(5 * SECOND), Some(Frame::Opened(_))));
    loop {
        command(&mut child, PlayCommand::Heartbeat);
        match frames.0.recv_timeout(Duration::from_millis(500)) {
            Ok(Some(Frame::Failure(failure))) => {
                assert_eq!(failure.code, FailureCode::RenderTimeout);
                break;
            }
            Err(_) => assert!(started.elapsed() < 8 * SECOND, "no answer"),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(started.elapsed() >= 5 * SECOND - Duration::from_millis(100));
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);
}

/// On macOS a session plays an MP4's sound with its picture, confined:
/// Opened says Ready, every frame comes in order, and the session ends once
/// the sound has, about four seconds after Play. Muted, so a run makes no
/// sound. A machine with no audio output says Unavailable and plays on
/// silent; with `KETTLE_REQUIRE_AUDIO` set that is a failure.
#[cfg(target_os = "macos")]
#[test]
fn apples_decoder_plays_a_session_with_its_sound_in_the_worker() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let clip = fixture_clip(directory.path(), "ticks.mp4");
    let mut child = grouped_worker();
    handshake(&mut child);
    let Frame::PlayOpen(mut open) = play_open(&clip, (32, 32), 0) else {
        unreachable!()
    };
    open.audio = kettle_media::AudioRequest::Muted;
    send(&mut child, &Frame::PlayOpen(open));
    let frames = Frames::of(&mut child);
    let opened = match frames.next(5 * SECOND) {
        Some(Frame::Opened(opened)) => opened,
        other => panic!("not opened: {other:?}"),
    };
    match opened.audio {
        kettle_media::AudioStatus::Ready => {}
        kettle_media::AudioStatus::Unavailable
            if std::env::var_os("KETTLE_REQUIRE_AUDIO").is_none() =>
        {
            eprintln!("skipped: no audio output on this machine");
            return;
        }
        other => panic!("sound {other:?}"),
    }
    assert_eq!(video_frame(frames.next(5 * SECOND)).pts_ms, 0);
    assert_eq!(
        frames.next(SECOND),
        Some(Frame::PlayState(PlayState::Paused))
    );
    let played = Instant::now();
    command(&mut child, PlayCommand::Play);
    let mut shown = vec![0];
    loop {
        match frames.next(5 * SECOND) {
            Some(Frame::VideoFrame(frame)) => {
                if frame.pts_ms == 3_900 {
                    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);
                }
                shown.push(frame.pts_ms);
            }
            Some(Frame::PlayState(PlayState::Playing)) => {}
            Some(Frame::PlayState(PlayState::Ended)) => break,
            other => panic!("unexpected {other:?}"),
        }
        // Completion can be buffered before the test reads the last frame.
        let _ = write_frame(
            child.stdin.as_mut().unwrap(),
            &Frame::PlayCommand(PlayCommand::Heartbeat),
            Direction::ParentToWorker,
        );
    }
    let elapsed = played.elapsed();
    assert!(
        elapsed >= Duration::from_millis(3_500) && elapsed < Duration::from_secs(6),
        "ended {elapsed:?} after Play"
    );
    // In order, and the last frame shown.
    // In order, each on the clip's own frames, the last among them; one
    // the sound passed may be dropped on a busy machine, so most, not all.
    assert!(shown.windows(2).all(|pair| pair[0] < pair[1]), "{shown:?}");
    assert!(shown.iter().all(|pts| pts % 100 == 0), "{shown:?}");
    assert!(shown.len() >= 30, "{} of 40 frames: {shown:?}", shown.len());
    assert_eq!(shown.last(), Some(&3_900));
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);
}

/// On macOS a session plays an MP4 through Apple's decoder inside the
/// confined worker, with no external decoder: every frame in order, each on
/// the clip's own time, and its index where it should be.
#[cfg(target_os = "macos")]
#[test]
fn apples_decoder_plays_a_session_in_the_worker() {
    let directory = kettle_test_support::private_tempdir("kettle-worker-session-");
    let clip = index_clip(directory.path());
    let mut child = grouped_worker();
    handshake(&mut child);
    send(&mut child, &play_open(&clip, (32, 32), 1000));
    let frames = Frames::of(&mut child);
    let opened = match frames.next(5 * SECOND) {
        Some(Frame::Opened(opened)) => opened,
        other => panic!("not opened: {other:?}"),
    };
    assert_eq!(
        (opened.width, opened.height, opened.start_ms),
        (32, 18, 1000)
    );
    let first = video_frame(frames.next(5 * SECOND));
    assert_eq!(first.pts_ms, 1000);
    assert_eq!(
        frames.next(SECOND),
        Some(Frame::PlayState(PlayState::Paused))
    );
    command(&mut child, PlayCommand::Play);
    assert_eq!(
        frames.next(SECOND),
        Some(Frame::PlayState(PlayState::Playing))
    );
    let mut shown = vec![first];
    loop {
        match frames.next(5 * SECOND) {
            Some(Frame::VideoFrame(frame)) => {
                heartbeat_before_last_frame(&mut child, frame.pts_ms, 3_900);
                shown.push(frame);
            }
            Some(Frame::PlayState(PlayState::Ended)) => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(
        shown.iter().map(|frame| frame.pts_ms).collect::<Vec<_>>(),
        (10..40).map(|index| index * 100).collect::<Vec<_>>()
    );
    for frame in &shown {
        // Bit 0 of the index: the top-left block, white for odd.
        let (x, y) = (32 / 6, 18 / 4);
        let red = frame.rgba[((y * 32 + x) * 4) as usize];
        assert_eq!(
            red > 128,
            (frame.pts_ms / 100) % 2 == 1,
            "frame at {}",
            frame.pts_ms
        );
    }
    assert_eq!(exit_code(&mut child, 5 * SECOND), 0);
}

/// Opt-in real local audio-service test; an unavailable service is a failure.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs a local PulseAudio-compatible service and matching worker"]
fn linux_pcm_only_broker_plays_pauses_and_closes() {
    use kettle_media::pcm::{BYTES, Block, FRAMES, Feedback, Message, Open};
    struct Owned(Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let before = self.0.try_wait();
            eprintln!("PCM broker exit before cleanup: {before:?}");

            let _ = self.0.kill();
            let _ = self.0.wait();
            if let Some(mut stderr) = self.0.stderr.take() {
                let mut text = String::new();
                let _ = stderr.read_to_string(&mut text);
                eprintln!("PCM broker stderr: {text}");
            }
        }
    }
    let path = std::env::var_os("KETTLE_TEST_PCM_WORKER").unwrap_or_else(|| WORKER.into());
    let mut owned = Owned(spawn(&mut Command::new(path)));
    println!("owned PCM broker pid={}", owned.0.id());
    let child = &mut owned.0;
    send(child, &hello(own_build()));
    assert!(matches!(receive(child), Some(Frame::Ready(_))));
    send(
        child,
        &Frame::Pcm(Message::Open(Open {
            generation: 77,
            start_frame: 48_000,
            end_frame: 96_000,
            muted: true,
        })),
    );
    let stdout = child.stdout.take().unwrap();
    let (sent, heard) = std::sync::mpsc::sync_channel(16);
    let reader = std::thread::spawn(move || {
        let mut stdout = stdout;
        while let Ok(Some(frame)) = read_frame(&mut stdout, Direction::WorkerToParent) {
            if sent.send(frame).is_err() {
                break;
            }
        }
    });
    let feedback = |condition: &dyn Fn(Feedback) -> bool| -> Feedback {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            let frame = heard
                .recv_timeout(until.saturating_duration_since(Instant::now()))
                .expect("PCM feedback deadline");
            let Frame::Pcm(Message::Feedback(feedback)) = frame else {
                panic!("unexpected PCM frame");
            };
            assert!(!feedback.failed, "PCM service/confinement failed");
            if condition(feedback) {
                return feedback;
            }
        }
    };
    feedback(&|_| true);
    send(child, &Frame::PlayCommand(PlayCommand::Play));
    // The real client can request Play before its first decoded packet arrives.
    std::thread::sleep(Duration::from_millis(150));
    for sequence in 0..4 {
        send(
            child,
            &Frame::Pcm(Message::Block(Block {
                generation: 77,
                sequence,
                at_frame: 48_000 + sequence * FRAMES as u64,
                samples: 0.125f32.to_le_bytes().repeat(BYTES / 4),
            })),
        );
    }
    let playing = feedback(&|f| f.played_frame.is_some_and(|frame| frame > 48_000));
    feedback(&|f| f.consumed == 4);
    send(child, &Frame::PlayCommand(PlayCommand::Pause));
    feedback(&|f| f.played_frame.is_none());
    std::thread::sleep(Duration::from_millis(100));
    for sequence in 4..8 {
        send(
            child,
            &Frame::Pcm(Message::Block(Block {
                generation: 77,
                sequence,
                at_frame: 48_000 + sequence * FRAMES as u64,
                samples: 0.125f32.to_le_bytes().repeat(BYTES / 4),
            })),
        );
    }
    send(child, &Frame::PlayCommand(PlayCommand::Play));
    feedback(&|f| {
        f.played_frame
            .is_some_and(|frame| frame > playing.played_frame.unwrap() + 480)
    });
    send(child, &Frame::PlayCommand(PlayCommand::Close));
    assert_eq!(exit_code(child, Duration::from_secs(3)), 0);
    drop(heard);
    reader.join().unwrap();
}
