#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::io::Write as _;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(any(target_os = "macos", target_os = "windows"))]
const INPUT_MAGIC: &[u8; 8] = b"KTLVPIN1";
#[cfg(any(target_os = "macos", target_os = "windows"))]
const OUTPUT_MAGIC: &[u8; 8] = b"KTLVPOU1";
#[cfg(any(target_os = "macos", target_os = "windows"))]
const MAX_PREVIEW_WIDTH: u32 = 256;
#[cfg(any(target_os = "macos", target_os = "windows"))]
const MAX_PREVIEW_HEIGHT: u32 = 160;
#[cfg(any(target_os = "macos", target_os = "windows"))]
const VIDEO_FIXTURE: &[u8] = include_bytes!("../../kettle-ui/testdata/video-preview.mp4");

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn worker_input(path: &Path) -> Vec<u8> {
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
    let mut input = Vec::with_capacity(12 + bytes.len());
    input.extend_from_slice(INPUT_MAGIC);
    input.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    input.extend_from_slice(&bytes);
    input
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[test]
fn shipped_worker_extracts_a_bounded_native_video_poster() {
    let dir = kettle_test_support::private_tempdir("kettle-video-native-");
    let video = dir.path().join("poster.mp4");
    std::fs::write(&video, VIDEO_FIXTURE).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&video, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

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
        .write_all(&worker_input(&video))
        .unwrap();
    let output = child.wait_with_output().unwrap();
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
        eprintln!("skipping native video poster: this runner has no platform thumbnail provider");
        return;
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
    assert_eq!(status.code(), Some(4), "self-deadline exit status");
}
