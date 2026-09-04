// `Output` is only named by the Unix-only `wait_bounded` below; importing it at
// module scope makes it an unused import on Windows, which `-D warnings` turns
// into a build failure.
use std::process::{Command, Stdio};

/// Create the implicit config with the same owner-only file contract the
/// production writer uses. Tower's `002` umask otherwise leaves a fixture at
/// `0664`, so the trust gate correctly rejects the fixture before these tests
/// reach the FIFO, size, or decoding behavior they are meant to exercise.
fn create_config_file(path: &std::path::Path) -> std::fs::File {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path).expect("config")
}

fn check_config_command(config_home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kettle"));
    command
        .arg("--check-config")
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("KETTLE_CONFIG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[cfg(unix)]
fn wait_bounded(mut child: std::process::Child) -> std::process::Output {
    use std::io::Read as _;
    use std::process::Output;
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll kettle --check-config") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("kettle --check-config blocked on a FIFO");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .stdout
        .take()
        .expect("captured stdout")
        .read_to_end(&mut stdout)
        .expect("read stdout");
    child
        .stderr
        .take()
        .expect("captured stderr")
        .read_to_end(&mut stderr)
        .expect("read stderr");
    Output {
        status,
        stdout,
        stderr,
    }
}

#[cfg(unix)]
#[test]
fn check_config_rejects_fifo_at_resolved_default_path_without_blocking() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let root = kettle_test_support::private_tempdir("kettle-check-config-");
    let config_dir = root.path().join("kettle");
    std::fs::create_dir(&config_dir).expect("config dir");
    let path = config_dir.join("config");
    let path_c = CString::new(path.as_os_str().as_bytes()).expect("path has no NUL");
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);

    let output = wait_bounded(
        check_config_command(root.path())
            .spawn()
            .expect("spawn kettle --check-config"),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "FIFO must be a config error");
    assert!(stdout.contains("i/o error:"), "stdout was {stdout:?}");
    assert!(
        stdout.contains("not a regular file"),
        "stdout was {stdout:?}"
    );
}

#[test]
fn check_config_rejects_oversize_file_at_resolved_default_path() {
    let root = kettle_test_support::private_tempdir("kettle-check-config-");
    let config_dir = root.path().join("kettle");
    std::fs::create_dir(&config_dir).expect("config dir");
    create_config_file(&config_dir.join("config"))
        .set_len(1024 * 1024 + 1)
        .expect("oversize config");

    let output = check_config_command(root.path())
        .output()
        .expect("run kettle --check-config");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "oversize config must be an error");
    assert!(stdout.contains("i/o error:"), "stdout was {stdout:?}");
    assert!(stdout.contains("1048576"), "stdout was {stdout:?}");
}

#[test]
fn check_config_decodes_utf16_bom_at_resolved_default_path() {
    let text = "scrollback = 4321\n";
    for (label, bom, encode) in [
        (
            "UTF-16LE",
            [0xff, 0xfe],
            u16::to_le_bytes as fn(u16) -> [u8; 2],
        ),
        (
            "UTF-16BE",
            [0xfe, 0xff],
            u16::to_be_bytes as fn(u16) -> [u8; 2],
        ),
    ] {
        let root = kettle_test_support::private_tempdir("kettle-check-config-");
        let config_dir = root.path().join("kettle");
        std::fs::create_dir(&config_dir).expect("config dir");
        let mut bytes = bom.to_vec();
        bytes.extend(text.encode_utf16().flat_map(encode));
        let mut file = create_config_file(&config_dir.join("config"));
        std::io::Write::write_all(&mut file, &bytes).expect("write encoded config");

        let output = check_config_command(root.path())
            .output()
            .expect("run kettle --check-config");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{label} failed: {stdout}");
        assert!(stdout.contains("scrollback: 4321"), "{label}: {stdout}");
        assert!(stdout.contains("status:  OK"), "{label}: {stdout}");
    }
}
