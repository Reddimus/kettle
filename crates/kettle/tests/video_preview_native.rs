#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::io::Write as _;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const INPUT_MAGIC: &[u8; 8] = b"KTLVPIN2";
/// The shipped binary's build identity, as `main` records it.
fn build_identity() -> String {
    env!("KETTLE_SOURCE_ID").to_string()
}
const WORKER_SKEW_EXIT: i32 = 9;
const OUTPUT_MAGIC: &[u8; 8] = b"KTLVPOU1";
#[cfg(any(target_os = "macos", target_os = "windows"))]
const MAX_PREVIEW_WIDTH: u32 = 256;
#[cfg(any(target_os = "macos", target_os = "windows"))]
const MAX_PREVIEW_HEIGHT: u32 = 160;
const WORKER_TIMEOUT_EXIT: i32 = 4;
const VIDEO_FIXTURE: &[u8] = include_bytes!("../../kettle-ui/testdata/video-preview.mp4");

fn write_private_fixture(path: &std::path::Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn worker_input(path: &std::path::Path, identity: &str) -> Vec<u8> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(windows)]
    let bytes = {
        use std::os::windows::ffi::OsStrExt as _;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    };
    let mut input = Vec::with_capacity(13 + identity.len() + bytes.len());
    input.extend_from_slice(INPUT_MAGIC);
    input.push(identity.len() as u8);
    input.extend_from_slice(identity.as_bytes());
    input.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    input.extend_from_slice(&bytes);
    input
}

/// One worker process at a time in this binary. Tests otherwise run in
/// parallel, and on a loaded Windows runner two workers asking the Shell
/// thumbnail provider at once can each overrun the production two-second
/// deadline, twice, although each passes alone. A failed test must not block
/// the rest, so a poisoned lock is still taken.
fn one_worker_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static WORKERS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    WORKERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Send `input` to the shipped worker and wait for it.
fn run_worker_with(input: &[u8]) -> std::process::Output {
    use std::io::Write as _;
    let _worker = one_worker_at_a_time();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kettle"))
        .arg("__media-preview-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch media preview worker");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

/// The shipped worker refuses a request from another build, by identity or
/// by frame version, with its skew exit code, before touching the path.
#[test]
fn shipped_worker_refuses_another_build_s_request() {
    let path = std::path::Path::new("/nonexistent/kettle-skew.mp4");
    let other = run_worker_with(&worker_input(path, "0.0.0 (000000000000)"));
    assert_eq!(other.status.code(), Some(WORKER_SKEW_EXIT), "{other:?}");
    assert!(other.stdout.is_empty());

    let mut old = b"KTLVPIN1".to_vec();
    old.extend_from_slice(&5u32.to_le_bytes());
    old.extend_from_slice(b"/a.mp");
    let old = run_worker_with(&old);
    assert_eq!(old.status.code(), Some(WORKER_SKEW_EXIT), "{old:?}");

    // The same build reads the request: the path is missing, not skew.
    let same = run_worker_with(&worker_input(path, &build_identity()));
    assert_ne!(same.status.code(), Some(WORKER_SKEW_EXIT), "{same:?}");
}

#[test]
fn shipped_worker_ignores_non_media_with_a_video_suffix() {
    let dir = kettle_test_support::private_tempdir("kettle-video-content-");
    for (name, bytes) in [
        ("notes.mp4", b"ordinary text, not a movie".as_slice()),
        ("partial.webm", b"\x1a\x45\xdf\xa3\x90".as_slice()),
        ("still.mp4", b"\0\0\0\x14ftypavif\0\0\0\0isom".as_slice()),
    ] {
        let path = dir.path().join(name);
        write_private_fixture(&path, bytes);
        let output = run_worker_with(&worker_input(&path, &build_identity()));
        assert_eq!(output.status.code(), Some(3), "{name}: {output:?}");
        assert!(output.stdout.is_empty(), "{name}: {output:?}");
    }
}

#[test]
fn shipped_worker_accepts_movie_bytes_with_another_video_suffix() {
    let dir = kettle_test_support::private_tempdir("kettle-video-content-");
    let path = dir.path().join("movie.avi");
    write_private_fixture(&path, VIDEO_FIXTURE);
    let input = worker_input(&path, &build_identity());
    let output = run_worker_with(&input);
    let output = if output.status.code() == Some(WORKER_TIMEOUT_EXIT) {
        run_worker_with(&input)
    } else {
        output
    };
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.len() >= 28, "{output:?}");
    assert_eq!(&output.stdout[..8], OUTPUT_MAGIC);
    assert_eq!(
        u64::from_le_bytes(output.stdout[8..16].try_into().unwrap()),
        VIDEO_FIXTURE.len() as u64
    );
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn run_native_worker(video: &Path) -> std::process::Output {
    let _worker = one_worker_at_a_time();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kettle"))
        .arg("__media-preview-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch media preview worker");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&worker_input(video, &build_identity()))
        .unwrap();
    child.wait_with_output().unwrap()
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn worker_timed_out(output: &std::process::Output) -> bool {
    output.status.code() == Some(WORKER_TIMEOUT_EXIT)
}

#[cfg(target_os = "macos")]
fn worker_returned_empty_poster(output: &std::process::Output) -> bool {
    output.status.success()
        && output.stdout.len() == 28
        && output.stdout.get(..8) == Some(OUTPUT_MAGIC.as_slice())
        && u64::from_le_bytes(output.stdout[8..16].try_into().unwrap())
            == VIDEO_FIXTURE.len() as u64
        && u32::from_le_bytes(output.stdout[16..20].try_into().unwrap()) == 0
        && u32::from_le_bytes(output.stdout[20..24].try_into().unwrap()) == 0
        && u32::from_le_bytes(output.stdout[24..28].try_into().unwrap()) == 0
}

#[cfg(target_os = "macos")]
#[test]
fn macos_warm_retry_accepts_only_an_exact_empty_poster() {
    use std::os::unix::process::ExitStatusExt as _;

    let mut stdout = Vec::with_capacity(28);
    stdout.extend_from_slice(OUTPUT_MAGIC);
    stdout.extend_from_slice(&(VIDEO_FIXTURE.len() as u64).to_le_bytes());
    stdout.extend_from_slice(&0_u32.to_le_bytes());
    stdout.extend_from_slice(&0_u32.to_le_bytes());
    stdout.extend_from_slice(&0_u32.to_le_bytes());
    let mut output = std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout,
        stderr: Vec::new(),
    };

    assert!(worker_returned_empty_poster(&output));
    output.stdout.push(0);
    assert!(!worker_returned_empty_poster(&output));
    output.stdout.pop();
    output.stdout[0] ^= 1;
    assert!(!worker_returned_empty_poster(&output));
    output.stdout[0] ^= 1;
    output.stdout[8] ^= 1;
    assert!(!worker_returned_empty_poster(&output));
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[test]
fn shipped_worker_extracts_a_bounded_native_video_poster() {
    let dir = kettle_test_support::private_tempdir("kettle-video-native-");
    let video = dir.path().join("poster.mp4");
    write_private_fixture(&video, VIDEO_FIXTURE);

    let output = run_native_worker(&video);
    let retry = worker_timed_out(&output);
    #[cfg(target_os = "macos")]
    let retry = retry || worker_returned_empty_poster(&output);
    let output = if retry {
        // Windows mirrors production by retrying only a deadline. Quick Look
        // can instead cold-return a valid empty poster before that deadline;
        // this provider-capability test gets one warm attempt in that case.
        // Production correctly keeps the empty result as a generic receipt.
        run_native_worker(&video)
    } else {
        output
    };
    assert!(
        output.status.success(),
        "native worker failed: status={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() >= 28);
    assert_eq!(&output.stdout[..8], OUTPUT_MAGIC);
    let size = u64::from_le_bytes(output.stdout[8..16].try_into().unwrap());
    let width = u32::from_le_bytes(output.stdout[16..20].try_into().unwrap());
    let height = u32::from_le_bytes(output.stdout[20..24].try_into().unwrap());
    let len = u32::from_le_bytes(output.stdout[24..28].try_into().unwrap()) as usize;
    assert_eq!(size, VIDEO_FIXTURE.len() as u64);
    if len == 0 {
        assert_eq!((width, height), (0, 0));
        #[cfg(target_os = "macos")]
        panic!("Quick Look returned no poster for the checked-in H.264 fixture");
        #[cfg(target_os = "windows")]
        {
            assert!(
                std::env::var_os("KETTLE_REQUIRE_NATIVE_VIDEO_POSTER").as_deref()
                    != Some(std::ffi::OsStr::new("1")),
                "Windows returned no poster although native poster support was required"
            );
            eprintln!(
                "skipping native video poster: set KETTLE_REQUIRE_NATIVE_VIDEO_POSTER=1 on a capable Windows host"
            );
            return;
        }
    }
    assert!(width > 0 && width <= MAX_PREVIEW_WIDTH);
    assert!(height > 0 && height <= MAX_PREVIEW_HEIGHT);
    assert_eq!(len, width as usize * height as usize * 4);
    assert_eq!(output.stdout.len(), 28 + len);
    assert!(
        output.stdout[28..]
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255),
        "native poster must be opaque for the straight-alpha renderer"
    );
}

#[test]
fn shipped_worker_exits_when_its_parent_never_finishes_input() {
    let _worker = one_worker_at_a_time();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kettle"))
        .arg("__media-preview-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch media preview worker");
    let _held_stdin = child.stdin.take().expect("worker stdin");
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait().expect("poll media preview worker") {
            Some(status) => break status,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            None => {
                let _ = child.kill();
                let output = child.wait_with_output().expect("reap stuck worker");
                panic!(
                    "worker outlived its self-deadline: stderr={}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    };
    assert_eq!(
        status.code(),
        Some(WORKER_TIMEOUT_EXIT),
        "self-deadline exit status"
    );
}
