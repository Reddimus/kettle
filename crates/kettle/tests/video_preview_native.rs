#[cfg(target_os = "windows")]
use std::io::Write as _;
#[cfg(target_os = "windows")]
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const INPUT_MAGIC: &[u8; 8] = b"KTLVPIN2";
/// The shipped binary's build identity, as `main` records it.
fn build_identity() -> String {
    env!("KETTLE_SOURCE_ID").to_string()
}
const WORKER_SKEW_EXIT: i32 = 9;
const OUTPUT_MAGIC: &[u8; 8] = b"KTLVPOU2";
#[cfg(target_os = "windows")]
const MAX_PREVIEW_WIDTH: u32 = 256;
#[cfg(target_os = "windows")]
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
    let reply = Reply::parse(&output.stdout);
    assert_eq!(reply.size, VIDEO_FIXTURE.len() as u64);
    // The same file checked again has the same fingerprint, which a
    // poster's second check compares.
    let again = Reply::parse(&run_worker_with(&input).stdout);
    assert_eq!(again.fingerprint, reply.fingerprint);
    assert_ne!(reply.fingerprint, [0; 32]);
}

/// A check's reply: the video's size, identity and sampled fingerprint, the
/// cached thumbnails it opened, and the poster it made, if any.
struct Reply {
    size: u64,
    #[cfg_attr(not(unix), allow(dead_code))]
    identity: (u64, u64, i64, u32),
    fingerprint: [u8; 32],
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    cached: Vec<(u64, u64, Vec<u8>)>,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Reply {
    fn parse(bytes: &[u8]) -> Self {
        let mut at = 0;
        let mut take = |len: usize| {
            let field = &bytes[at..at + len];
            at += len;
            field
        };
        assert_eq!(take(8), OUTPUT_MAGIC);
        let u64_at = |field: &[u8]| u64::from_le_bytes(field.try_into().unwrap());
        let u32_at = |field: &[u8]| u32::from_le_bytes(field.try_into().unwrap());
        let size = u64_at(take(8));
        let identity = (
            u64_at(take(8)),
            u64_at(take(8)),
            i64::from_le_bytes(take(8).try_into().unwrap()),
            u32_at(take(4)),
        );
        let fingerprint = take(32).try_into().unwrap();
        let count = take(1)[0];
        let cached = (0..count)
            .map(|_| {
                let dev = u64_at(take(8));
                let ino = u64_at(take(8));
                let len = u32_at(take(4)) as usize;
                (dev, ino, take(len).to_vec())
            })
            .collect();
        let width = u32_at(take(4));
        let height = u32_at(take(4));
        let len = u32_at(take(4)) as usize;
        let rgba = take(len).to_vec();
        assert_eq!(at, bytes.len(), "trailing bytes in the reply");
        Self {
            size,
            identity,
            fingerprint,
            cached,
            width,
            height,
            rgba,
        }
    }
}

/// On macOS and Linux the check decodes nothing: it answers with the file's
/// own identity, which the media worker is then held to, and no pixels.
#[cfg(unix)]
#[test]
fn shipped_check_answers_with_the_file_s_identity_and_no_pixels() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = kettle_test_support::private_tempdir("kettle-video-check-");
    let path = dir.path().join("clip.mp4");
    write_private_fixture(&path, VIDEO_FIXTURE);
    let input = worker_input(&path, &build_identity());
    let output = run_worker_with(&input);
    let output = if output.status.code() == Some(WORKER_TIMEOUT_EXIT) {
        run_worker_with(&input)
    } else {
        output
    };
    assert!(output.status.success(), "{output:?}");
    let reply = Reply::parse(&output.stdout);
    let metadata = std::fs::metadata(&path).unwrap();
    assert_eq!(reply.size, metadata.len());
    assert_eq!(
        reply.identity,
        (
            metadata.dev(),
            metadata.ino(),
            metadata.mtime(),
            u32::try_from(metadata.mtime_nsec()).unwrap()
        )
    );
    assert_eq!((reply.width, reply.height), (0, 0));
    assert!(reply.rgba.is_empty());
}

/// On Linux the check also opens the video's cached thumbnails in the
/// user's cache, and names each with the file it opened.
#[cfg(target_os = "linux")]
#[test]
fn shipped_check_names_the_video_s_cached_thumbnails() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    let dir = kettle_test_support::private_tempdir("kettle-video-check-");
    let path = dir.path().join("clip.mp4");
    write_private_fixture(&path, VIDEO_FIXTURE);
    // The MD5 of the video's file URI, which has no characters to escape.
    let uri = format!("file://{}", path.display());
    let digest = md5_hex(uri.as_bytes());
    let cache = dir.path().join("cache");
    let directory = cache.join("thumbnails/large");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let thumbnail = directory.join(format!("{digest}.png"));
    write_private_fixture(&thumbnail, b"never parsed by the check");
    let _worker = one_worker_at_a_time();
    let output = Command::new(env!("CARGO_BIN_EXE_kettle"))
        .arg("__media-preview-worker")
        .env("XDG_CACHE_HOME", &cache)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write as _;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(&worker_input(&path, &build_identity()))?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let reply = Reply::parse(&output.stdout);
    let metadata = std::fs::metadata(&thumbnail).unwrap();
    assert_eq!(
        reply.cached,
        vec![(
            metadata.dev(),
            metadata.ino(),
            thumbnail.as_os_str().as_bytes().to_vec()
        )]
    );
}

/// The lowercase hex MD5 of `bytes` (RFC 1321), enough for a cache file name.
#[cfg(target_os = "linux")]
fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest as _, Md5};
    Md5::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(target_os = "windows")]
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

/// On Windows, where no media worker runs, the check asks the Shell for the
/// poster itself.
#[cfg(target_os = "windows")]
#[test]
fn shipped_worker_extracts_a_bounded_native_video_poster() {
    let dir = kettle_test_support::private_tempdir("kettle-video-native-");
    let video = dir.path().join("poster.mp4");
    write_private_fixture(&video, VIDEO_FIXTURE);

    let output = run_native_worker(&video);
    let output = if output.status.code() == Some(WORKER_TIMEOUT_EXIT) {
        // Windows mirrors production by retrying only a deadline.
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
    let reply = Reply::parse(&output.stdout);
    assert_eq!(reply.size, VIDEO_FIXTURE.len() as u64);
    if reply.rgba.is_empty() {
        assert_eq!((reply.width, reply.height), (0, 0));
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
    assert!(reply.width > 0 && reply.width <= MAX_PREVIEW_WIDTH);
    assert!(reply.height > 0 && reply.height <= MAX_PREVIEW_HEIGHT);
    assert_eq!(
        reply.rgba.len(),
        reply.width as usize * reply.height as usize * 4
    );
    assert!(
        reply
            .rgba
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
