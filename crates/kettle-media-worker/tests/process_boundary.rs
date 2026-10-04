//! The worker binary end to end: its handshake and refusals, its watchdog,
//! and the setup it finishes before it reads anything.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::io::Read as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use kettle_media::wire::{Direction, Frame, encode, read_frame, write_frame};
use kettle_media::{
    BuildId, Canvas, Failure, FailureCode, FallbackFont, Hello, Job, JobKind, NativePath, Ready,
    Source, Target, Theme, Warning,
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
