//! Agent-first (A1) e2e: drive the real `kettle` binary's `exec`
//! subcommand under a real PTY and assert the agent-facing contract — piped
//! stdout, exit-code propagation, timeout, stdin forwarding, strip-ansi, and
//! `--json` events. Spawned via `std::process::Command` with piped stdio +
//! `wait`, which is exactly how an MCP client / agent launches it (and the path
//! that works regardless of the Windows GUI subsystem — the OS process handle
//! carries the true exit code, the pipe delivers output on EOF).
//!
//! Soft-skips when no PTY is available in the sandbox (mirrors kettle-core's
//! teardown tests) so CI without a console doesn't red the suite.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn kettle() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kettle"))
}

/// Run `kettle exec <extra…> -- <argv…>`, feeding `stdin_data` if any. Returns
/// (exit_code, stdout, stderr). Kills + fails the test if it runs too long.
fn run_exec(extra: &[&str], argv: &[&str], stdin_data: Option<&[u8]>) -> (i32, String, String) {
    run_exec_with_env(extra, argv, stdin_data, &[])
}

fn run_exec_with_env(
    extra: &[&str],
    argv: &[&str],
    stdin_data: Option<&[u8]>,
    env: &[(&str, &str)],
) -> (i32, String, String) {
    let mut cmd = kettle();
    cmd.arg("exec");
    cmd.args(extra);
    cmd.arg("--");
    cmd.args(argv);
    cmd.envs(env.iter().copied());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(if stdin_data.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = cmd.spawn().expect("spawn kettle exec");
    if let Some(data) = stdin_data {
        child.stdin.take().unwrap().write_all(data).unwrap();
        // stdin dropped here → EOF to the pump.
    }
    let mut out = String::new();
    let mut err = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    let status = child.wait().expect("wait");
    (status.code().unwrap_or(-1), out, err)
}

/// True if the run looks like a PTY-less sandbox failure we should soft-skip.
fn no_pty(code: i32, err: &str) -> bool {
    code == 125 && err.contains("cannot start PTY")
}

#[cfg(windows)]
struct WindowsHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for WindowsHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: WindowsHandle exclusively owns this non-null handle.
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
fn windows_wide_null(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn create_named_event(name: &std::ffi::OsStr) -> std::io::Result<WindowsHandle> {
    use windows_sys::Win32::System::Threading::CreateEventW;

    let name = windows_wide_null(name);
    // SAFETY: the generated name is NUL-terminated and remains live for the
    // call; null security attributes request the documented defaults.
    let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) };
    if handle.is_null() {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(WindowsHandle(handle))
    }
}

#[cfg(windows)]
fn open_named_event(name: &std::ffi::OsStr) -> std::io::Result<WindowsHandle> {
    use windows_sys::Win32::System::Threading::{OpenEventW, SYNCHRONIZATION_SYNCHRONIZE};

    let name = windows_wide_null(name);
    // SAFETY: the inherited environment value is converted to a
    // NUL-terminated buffer that remains live for the call.
    let handle = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    if handle.is_null() {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(WindowsHandle(handle))
    }
}

#[cfg(windows)]
fn set_pipe_nowait(handle: &impl std::os::windows::io::AsRawHandle) -> std::io::Result<()> {
    use windows_sys::Win32::System::Pipes::{PIPE_NOWAIT, SetNamedPipeHandleState};

    let mode = PIPE_NOWAIT;
    // SAFETY: `handle` owns a valid pipe handle, `mode` is readable for the
    // call, and the optional collection pointers are intentionally null.
    let configured = unsafe {
        SetNamedPipeHandleState(
            handle.as_raw_handle(),
            &mode,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if configured == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn saturate_nonblocking_pipe(writer: &mut impl Write, deadline: Instant) -> Result<usize, String> {
    const MIN_ACCEPTED: usize = 64 * 1024;
    const MAX_ACCEPTED: usize = 16 * 1024 * 1024;
    const STABLY_FULL_FOR: Duration = Duration::from_millis(200);

    let chunk = [b'x'; 4096];
    let mut accepted = 0usize;
    let mut full_since = None;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(format!(
                "stdin pipe did not remain full before its deadline \
                 ({accepted} bytes accepted)"
            ));
        }
        if accepted >= MAX_ACCEPTED {
            return Err(format!(
                "stdin accepted the {MAX_ACCEPTED}-byte fixture cap without \
                 stable downstream backpressure"
            ));
        }

        // Once a full write is observed, probe one byte at a time. Sustained
        // zero progress at that quantum proves the pending DSR reply cannot
        // slip through spare capacity.
        let bytes = if full_since.is_some() {
            &chunk[..1]
        } else {
            &chunk[..]
        };
        match writer.write(bytes) {
            Ok(0) => {
                let full_since = *full_since.get_or_insert(now);
                if accepted >= MIN_ACCEPTED && now.duration_since(full_since) >= STABLY_FULL_FOR {
                    return Ok(accepted);
                }
                std::thread::yield_now();
            }
            Ok(written) => {
                accepted += written;
                full_since = None;
            }
            Err(error) => {
                return Err(format!(
                    "stdin pipe failed before stable saturation after \
                     {accepted} bytes: {error}"
                ));
            }
        }
    }
}

#[cfg(windows)]
const CONPTY_READY_MARKER: &[u8] = b"CONPTY_BACKPRESSURE_READY";
#[cfg(windows)]
const CONPTY_QUERY_MARKER: &[u8] = b"CONPTY_QUERY_SENT";

#[cfg(windows)]
fn scan_conpty_markers(window: &mut Vec<u8>, chunk: &[u8]) -> (bool, bool) {
    const CARRY: usize = CONPTY_READY_MARKER.len() - 1;

    window.extend_from_slice(chunk);
    let ready = window
        .windows(CONPTY_READY_MARKER.len())
        .any(|candidate| candidate == CONPTY_READY_MARKER);
    let query = window
        .windows(CONPTY_QUERY_MARKER.len())
        .any(|candidate| candidate == CONPTY_QUERY_MARKER);
    if window.len() > CARRY {
        let keep_from = window.len() - CARRY;
        window.copy_within(keep_from.., 0);
        window.truncate(CARRY);
    }
    (ready, query)
}

#[cfg(windows)]
struct BoundedDiagnostic {
    head: Vec<u8>,
    tail: Vec<u8>,
    total: usize,
}

#[cfg(windows)]
impl BoundedDiagnostic {
    const HEAD_CAP: usize = 32 * 1024;
    const TAIL_CAP: usize = 32 * 1024;

    fn new() -> Self {
        Self {
            head: Vec::with_capacity(Self::HEAD_CAP),
            tail: Vec::with_capacity(Self::TAIL_CAP),
            total: 0,
        }
    }

    fn push(&mut self, mut bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len());
        if self.head.len() < Self::HEAD_CAP {
            let take = bytes.len().min(Self::HEAD_CAP - self.head.len());
            self.head.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }
        if bytes.is_empty() {
            return;
        }
        if bytes.len() >= Self::TAIL_CAP {
            self.tail.clear();
            self.tail
                .extend_from_slice(&bytes[bytes.len() - Self::TAIL_CAP..]);
            return;
        }
        let overflow = self
            .tail
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(Self::TAIL_CAP);
        if overflow != 0 {
            self.tail.drain(..overflow);
        }
        self.tail.extend_from_slice(bytes);
    }

    fn finish(mut self) -> (Vec<u8>, usize) {
        let omitted = self
            .total
            .saturating_sub(self.head.len().saturating_add(self.tail.len()));
        self.head.extend_from_slice(&self.tail);
        (self.head, omitted)
    }
}

fn private_test_scratch_root() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("Windows tests require LOCALAPPDATA or USERPROFILE")
    }
    #[cfg(not(windows))]
    {
        std::env::temp_dir()
    }
}

#[cfg(windows)]
const ECHO: [&str; 3] = ["cmd", "/c", "echo"];
#[cfg(unix)]
const ECHO: [&str; 1] = ["echo"];

#[test]
fn exec_streams_stdout_and_exits_zero() {
    let mut argv: Vec<&str> = ECHO.to_vec();
    argv.push("agent-marker-7f3");
    // strip-ansi so we assert on clean text (raw includes ConPTY handshake).
    let (code, out, err) = run_exec(&["--strip-ansi"], &argv, None);
    if no_pty(code, &err) {
        eprintln!("skipping exec_streams_stdout_and_exits_zero: no PTY");
        return;
    }
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("agent-marker-7f3"), "stdout was: {out:?}");
}

#[test]
fn exec_propagates_child_exit_code() {
    #[cfg(windows)]
    let argv = ["cmd", "/c", "exit", "3"];
    #[cfg(unix)]
    let argv = ["sh", "-c", "exit 3"];
    let (code, _out, err) = run_exec(&[], &argv, None);
    if no_pty(code, &err) {
        eprintln!("skipping exec_propagates_child_exit_code: no PTY");
        return;
    }
    assert_eq!(code, 3, "exit code must propagate; stderr: {err}");
}

#[test]
fn exec_timeout_returns_124() {
    // A command that would run far longer than the timeout.
    #[cfg(windows)]
    let argv = ["cmd", "/c", "pause"];
    #[cfg(unix)]
    let argv = ["sh", "-c", "sleep 30"];
    let start = Instant::now();
    let (code, _out, err) = run_exec(&["--timeout", "1.0"], &argv, None);
    if no_pty(code, &err) {
        eprintln!("skipping exec_timeout_returns_124: no PTY");
        return;
    }
    assert_eq!(code, 124, "timeout must exit 124; stderr: {err}");
    assert!(
        start.elapsed() < Duration::from_secs(15),
        "timeout took too long: {:?}",
        start.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn exec_forwards_piped_stdin() {
    let argv = [
        "sh",
        "-c",
        "IFS= read -r line; printf 'stdin:%s\\n' \"$line\"",
    ];

    let (code, out, err) = run_exec(
        &["--timeout", "5.0", "--strip-ansi"],
        &argv,
        Some(b"agent-stdin-42\n"),
    );
    if no_pty(code, &err) {
        eprintln!("skipping exec_forwards_piped_stdin: no PTY");
        return;
    }
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("stdin:agent-stdin-42"), "stdout was: {out:?}");
}

#[cfg(windows)]
#[test]
fn exec_forwards_delimited_piped_stdin_through_conpty() {
    let helper = std::env::current_exe().expect("resolve integration-test helper");
    let helper = helper.to_str().expect("integration-test path is UTF-8");
    let argv = [
        helper,
        "--exact",
        "windows_piped_stdin_helper",
        "--nocapture",
        "--test-threads=1",
    ];
    let payload = b"agent-windows-stdin-42\n";
    let (code, out, err) = run_exec_with_env(
        &["--timeout", "5.0", "--strip-ansi"],
        &argv,
        Some(payload),
        &[("KETTLE_EXEC_WINDOWS_STDIN_HELPER", "1")],
    );
    if no_pty(code, &err) {
        eprintln!("skipping exec_forwards_delimited_piped_stdin_through_conpty: no PTY");
        return;
    }
    assert_eq!(code, 0, "stderr: {err}; stdout: {out:?}");
    assert!(
        out.contains("WINDOWS_PIPE_DELIMITED_OK"),
        "native ConPTY child did not receive exact delimited input: {out:?}"
    );
}

#[cfg(windows)]
#[test]
fn windows_piped_stdin_helper() {
    if std::env::var_os("KETTLE_EXEC_WINDOWS_STDIN_HELPER").is_none() {
        return;
    }
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .expect("read native ConPTY input through newline");
    assert_eq!(input, "agent-windows-stdin-42\r\n");
    println!("WINDOWS_PIPE_DELIMITED_OK");
}

#[cfg(windows)]
#[test]
fn exec_timeout_closes_a_saturated_conpty_after_a_query() {
    use std::ffi::OsStr;
    use windows_sys::Win32::System::Threading::SetEvent;

    let helper = std::env::current_exe().expect("resolve integration-test helper");
    let helper = helper.to_str().expect("integration-test path is UTF-8");
    let event_name = format!(
        "Local\\kettle-exec-backpressure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos()
    );
    let release_event =
        create_named_event(OsStr::new(&event_name)).expect("create helper release event");
    let mut cmd = kettle();
    cmd.args([
        "exec",
        "--timeout",
        "8.0",
        "--strip-ansi",
        "--",
        helper,
        "--exact",
        "windows_conpty_backpressure_helper",
        "--nocapture",
        "--test-threads=1",
    ]);
    cmd.env("KETTLE_EXEC_WINDOWS_BACKPRESSURE_EVENT", &event_name);
    cmd.env("RUST_LOG", "kettle::exec=debug");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let started = Instant::now();
    let mut child = cmd.spawn().expect("spawn backpressured ConPTY exec");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let (query_tx, query_rx) = std::sync::mpsc::sync_channel(1);
    let stdout_reader = std::thread::spawn(move || {
        let mut diagnostics = BoundedDiagnostic::new();
        let mut marker_window = Vec::with_capacity(4096 + CONPTY_READY_MARKER.len());
        let mut buf = [0u8; 4096];
        let mut ready_sent = false;
        let mut query_sent = false;
        loop {
            let read = stdout.read(&mut buf).unwrap();
            if read == 0 {
                break;
            }
            let bytes = &buf[..read];
            diagnostics.push(bytes);
            let (saw_ready, saw_query) = scan_conpty_markers(&mut marker_window, bytes);
            if !ready_sent && saw_ready {
                let _ = ready_tx.send(());
                ready_sent = true;
            }
            if !query_sent && saw_query {
                let _ = query_tx.send(());
                query_sent = true;
            }
        }
        diagnostics.finish()
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });

    if let Err(ready_error) = ready_rx.recv_timeout(Duration::from_secs(4)) {
        drop(stdin);
        let _ = child.kill();
        let status = child.wait().expect("wait for unready ConPTY exec");
        let (out, omitted) = stdout_reader.join().expect("join stdout reader");
        let out = String::from_utf8_lossy(&out).into_owned();
        let err = String::from_utf8_lossy(&stderr_reader.join().expect("join stderr reader"))
            .into_owned();
        if no_pty(status.code().unwrap_or(-1), &err) {
            eprintln!("skipping exec_timeout_closes_a_saturated_conpty_after_a_query: no PTY");
            return;
        }
        panic!(
            "native ConPTY helper never became ready ({ready_error}); stderr: {err}; \
             stdout ({omitted} bytes omitted): {out:?}"
        );
    }

    set_pipe_nowait(&stdin).expect("make kettle stdin pipe nonblocking");
    let saturation = saturate_nonblocking_pipe(&mut stdin, Instant::now() + Duration::from_secs(3));
    // SAFETY: release_event exclusively owns a valid event handle for the
    // duration of this call.
    let event_signaled = unsafe { SetEvent(release_event.0) };
    let query_observed = query_rx.recv_timeout(Duration::from_secs(1));

    let (status, watchdog_killed) = loop {
        if let Some(status) = child.try_wait().expect("poll backpressured exec") {
            break (status, false);
        }
        if started.elapsed() >= Duration::from_secs(12) {
            child.kill().expect("kill stalled kettle exec");
            break (
                child.wait().expect("wait for watchdog-killed kettle exec"),
                true,
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(stdin);
    let (out, omitted) = stdout_reader.join().expect("join stdout reader");
    let out = String::from_utf8_lossy(&out).into_owned();
    let err =
        String::from_utf8_lossy(&stderr_reader.join().expect("join stderr reader")).into_owned();

    if no_pty(status.code().unwrap_or(-1), &err) {
        eprintln!("skipping exec_timeout_closes_a_saturated_conpty_after_a_query: no PTY");
        return;
    }
    assert!(
        !watchdog_killed,
        "saturated ConPTY defeated kettle exec timeout/close; \
         saturation={saturation:?}; query={query_observed:?}; \
         stderr: {err}; stdout ({omitted} bytes omitted): {out:?}"
    );
    let saturated_bytes = saturation.unwrap_or_else(|error| {
        panic!(
            "native stdin did not reach stable ConPTY backpressure: {error}; \
             stderr: {err}; stdout ({omitted} bytes omitted): {out:?}"
        )
    });
    assert!(
        saturated_bytes >= 64 * 1024,
        "fixture accepted too little input to prove downstream saturation: {saturated_bytes}"
    );
    assert_ne!(
        event_signaled,
        0,
        "release saturated-query helper: {}",
        std::io::Error::last_os_error()
    );
    assert!(
        query_observed.is_ok(),
        "native child did not emit its query after saturation \
         ({query_observed:?}); stderr: {err}; \
         stdout ({omitted} bytes omitted): {out:?}"
    );
    assert_eq!(
        status.code(),
        Some(124),
        "timeout lost under native ConPTY backpressure; stderr: {err}; \
         stdout ({omitted} bytes omitted): {out:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "saturated ConPTY close took {:?}",
        started.elapsed()
    );
}

#[cfg(windows)]
#[test]
fn windows_pipe_nowait_never_blocks_at_capacity() {
    use std::fs::File;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::System::Pipes::CreatePipe;

    let mut read_handle = std::ptr::null_mut();
    let mut write_handle = std::ptr::null_mut();
    // SAFETY: both output pointers are valid, and a null security descriptor
    // requests the documented default anonymous-pipe attributes.
    let created = unsafe { CreatePipe(&mut read_handle, &mut write_handle, std::ptr::null(), 0) };
    assert_ne!(
        created,
        0,
        "create anonymous byte pipe: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: CreatePipe succeeded and transferred two unique owned handles.
    // Each is moved into exactly one File and is therefore closed exactly once.
    let mut reader = unsafe { File::from_raw_handle(read_handle) };
    let mut writer = unsafe { File::from_raw_handle(write_handle) };

    set_pipe_nowait(&writer).expect("enable PIPE_NOWAIT");

    let fill_started = Instant::now();
    let initial = [b'x'; 1024];
    assert_eq!(
        writer.write(&initial).expect("initial nonblocking write"),
        initial.len(),
        "the 1 KiB write quantum must fit in an empty anonymous pipe"
    );
    let mut filled = initial.len();
    loop {
        let call_started = Instant::now();
        let written = writer.write(b"x").expect("nonblocking pipe fill");
        assert!(
            call_started.elapsed() < Duration::from_millis(250),
            "PIPE_NOWAIT write blocked for {:?}",
            call_started.elapsed()
        );
        if written == 0 {
            break;
        }
        filled += written;
        assert!(
            filled < 1024 * 1024,
            "unread pipe accepted an implausibly large payload"
        );
    }
    assert!(
        fill_started.elapsed() < Duration::from_secs(2),
        "pipe saturation took {:?}",
        fill_started.elapsed()
    );

    let (byte_tx, byte_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let reader_thread = std::thread::spawn(move || {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).expect("read one pipe byte");
        byte_tx.send(byte).expect("publish read byte");
        release_rx.recv().expect("hold read handle");
    });
    let resumed_at = Instant::now();
    let written = loop {
        let written = writer.write(b"y").expect("nonblocking pipe write");
        if written != 0 {
            break written;
        }
        assert!(
            resumed_at.elapsed() < Duration::from_secs(2),
            "writer never observed the pending pipe read"
        );
        std::thread::yield_now();
    };
    assert_eq!(written, 1);
    assert_eq!(byte_rx.recv().expect("receive read byte"), [b'x']);
    release_tx.send(()).expect("release read handle");
    reader_thread.join().expect("join pipe reader");
}

#[cfg(windows)]
#[test]
fn windows_conpty_backpressure_helper() {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    let Some(event_name) = std::env::var_os("KETTLE_EXEC_WINDOWS_BACKPRESSURE_EVENT") else {
        return;
    };
    let release_event = open_named_event(&event_name).expect("open helper release event");
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "CONPTY_BACKPRESSURE_READY").unwrap();
    stdout.flush().unwrap();

    // SAFETY: release_event owns a valid event handle and remains live for the
    // complete bounded wait.
    let wait_result = unsafe { WaitForSingleObject(release_event.0, 5_000) };
    assert_eq!(
        wait_result, WAIT_OBJECT_0,
        "parent never confirmed stable ConPTY input saturation"
    );
    stdout.write_all(b"\x1b[5n").unwrap();
    stdout.flush().unwrap();
    writeln!(stdout, "CONPTY_QUERY_SENT").unwrap();
    stdout.flush().unwrap();
    std::thread::sleep(Duration::from_secs(30));
}

#[cfg(unix)]
#[test]
fn exec_preserves_terminal_replies_after_piped_stdin_eof() {
    assert_canonical_eof_case(b"agent-eof-payload");
}

#[cfg(unix)]
#[test]
fn exec_empty_piped_stdin_reaches_eof_and_preserves_terminal_replies() {
    assert_canonical_eof_case(b"");
}

#[cfg(unix)]
#[test]
fn exec_line_terminated_piped_stdin_reaches_eof_without_extra_input() {
    assert_canonical_eof_case(b"agent-line-payload\n");
}

#[cfg(unix)]
fn assert_canonical_eof_case(input: &[u8]) {
    let helper = std::env::current_exe().expect("resolve integration-test helper");
    let helper = helper.to_str().expect("integration-test path is UTF-8");
    let argv = [
        helper,
        "--exact",
        "pty_stdin_eof_then_query_helper",
        "--nocapture",
        "--test-threads=1",
    ];
    let (code, out, err) = run_exec_with_env(
        &["--timeout", "5.0", "--strip-ansi"],
        &argv,
        Some(input),
        &[
            ("KETTLE_EXEC_EOF_QUERY_HELPER", "1"),
            (
                "KETTLE_EXEC_EOF_EXPECTED",
                std::str::from_utf8(input).expect("fixture input is UTF-8"),
            ),
        ],
    );
    if no_pty(code, &err) {
        eprintln!("skipping exec_preserves_terminal_replies_after_piped_stdin_eof: no PTY");
        return;
    }
    assert_eq!(code, 0, "stderr: {err}; stdout: {out:?}");
    assert!(
        out.contains("PTY_EOF_QUERY_OK"),
        "post-EOF terminal query did not complete; stdout: {out:?}"
    );
}

#[cfg(unix)]
#[test]
fn pty_stdin_eof_then_query_helper() {
    if std::env::var_os("KETTLE_EXEC_EOF_QUERY_HELPER").is_none() {
        return;
    }

    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .expect("read forwarded PTY input through EOF");
    let expected = std::env::var("KETTLE_EXEC_EOF_EXPECTED").unwrap();
    assert_eq!(input, expected.as_bytes());

    // Query only after read_to_end observed EOF. This catches closing the
    // shared Unix PTY master writer: that old behavior made the reply vanish.
    assert_post_eof_terminal_queries("default canonical mode");

    println!("PTY_EOF_QUERY_OK");
}

#[cfg(unix)]
fn make_stdin_raw() -> libc::termios {
    let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
    assert_eq!(unsafe { libc::tcgetattr(0, &mut termios) }, 0);
    let original = termios;
    unsafe { libc::cfmakeraw(&mut termios) };
    assert_eq!(unsafe { libc::tcsetattr(0, libc::TCSANOW, &termios) }, 0);
    original
}

#[cfg(unix)]
fn query_pty(query: &[u8], terminator: &[u8]) -> Vec<u8> {
    std::io::stdout().write_all(query).unwrap();
    std::io::stdout().flush().unwrap();
    let mut response = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !response.ends_with(terminator) && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut poll_fd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) } <= 0 {
            break;
        }
        let mut buf = [0u8; 64];
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
        response.extend_from_slice(&buf[..n as usize]);
    }
    response
}

#[cfg(unix)]
fn assert_post_eof_terminal_queries(context: &str) {
    let original = make_stdin_raw();
    let dsr = query_pty(b"\x1b[5n", b"\x1b[0n");
    assert_eq!(
        dsr, b"\x1b[0n",
        "{context}: invalid DSR response after forwarded stdin EOF"
    );
    let da = query_pty(b"\x1b[c", b"c");
    assert!(
        da.starts_with(b"\x1b[?") && da.ends_with(b"c"),
        "{context}: invalid DA1 response after forwarded stdin EOF: {da:02x?}"
    );
    let da_codes = std::str::from_utf8(&da[3..da.len() - 1])
        .unwrap()
        .split(';')
        .collect::<Vec<_>>();
    assert!(
        !da_codes.contains(&"52"),
        "{context}: headless exec advertised unavailable OSC 52 writes: {da:02x?}"
    );
    let kitty = query_pty(b"\x1b[?u", b"\x1b[?0u");
    assert_eq!(
        kitty, b"\x1b[?0u",
        "{context}: invalid Kitty keyboard response after forwarded stdin EOF"
    );
    let _ = unsafe { libc::tcsetattr(0, libc::TCSANOW, &original) };
}

#[cfg(unix)]
#[test]
fn exec_raw_mode_eof_is_explicit_and_does_not_destroy_terminal_replies() {
    use std::io::BufRead;

    let helper = std::env::current_exe().expect("resolve integration-test helper");
    let helper = helper.to_str().expect("integration-test path is UTF-8");
    let mut cmd = kettle();
    cmd.args([
        "exec",
        "--timeout",
        "5.0",
        "--strip-ansi",
        "--",
        helper,
        "--exact",
        "pty_raw_eof_then_query_helper",
        "--nocapture",
        "--test-threads=1",
    ]);
    cmd.env("KETTLE_EXEC_RAW_EOF_QUERY_HELPER", "1");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn kettle exec");
    let stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut out = String::new();
    loop {
        let mut line = String::new();
        if stdout.read_line(&mut line).unwrap() == 0 {
            break;
        }
        out.push_str(&line);
        if line.contains("RAW_MODE_READY") {
            break;
        }
    }
    assert!(
        out.contains("RAW_MODE_READY"),
        "raw-mode helper never became ready: {out:?}"
    );
    drop(stdin);
    stdout.read_to_string(&mut out).unwrap();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    let status = child.wait().expect("wait");
    let code = status.code().unwrap_or(-1);
    if no_pty(code, &err) {
        eprintln!("skipping exec_raw_mode_eof_is_explicit: no PTY");
        return;
    }
    assert_eq!(code, 0, "stderr: {err}; stdout: {out:?}");
    assert!(out.contains("PTY_RAW_EOF_QUERY_OK"), "stdout: {out:?}");
    assert!(
        err.contains("stdin reached EOF but this PTY has no safe EOF signal"),
        "missing explicit raw-mode EOF diagnostic: {err:?}"
    );
}

#[cfg(unix)]
#[test]
fn pty_raw_eof_then_query_helper() {
    if std::env::var_os("KETTLE_EXEC_RAW_EOF_QUERY_HELPER").is_none() {
        return;
    }

    let original = make_stdin_raw();
    println!("RAW_MODE_READY");
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_millis(250));
    let dsr = query_pty(b"\x1b[5n", b"\x1b[0n");
    let da = query_pty(b"\x1b[c", b"c");
    let kitty = query_pty(b"\x1b[?u", b"\x1b[?0u");
    let _ = unsafe { libc::tcsetattr(0, libc::TCSANOW, &original) };
    assert_eq!(
        dsr, b"\x1b[0n",
        "raw-mode EOF injected input or lost the DSR reply"
    );
    assert!(
        da.starts_with(b"\x1b[?") && da.ends_with(b"c"),
        "raw-mode EOF lost the DA1 reply: {da:02x?}"
    );
    let da_codes = std::str::from_utf8(&da[3..da.len() - 1])
        .unwrap()
        .split(';')
        .collect::<Vec<_>>();
    assert!(
        !da_codes.contains(&"52"),
        "raw-mode headless exec advertised unavailable OSC 52 writes: {da:02x?}"
    );
    assert_eq!(
        kitty, b"\x1b[?0u",
        "raw-mode EOF lost the Kitty keyboard reply"
    );
    println!("PTY_RAW_EOF_QUERY_OK");
}

#[cfg(unix)]
#[test]
fn exec_canonical_eof_follows_live_termios_boundaries() {
    let cases: [(&str, &[u8]); 10] = [
        ("inlcr", b"x\n"),
        ("igncr", b"x\r\r"),
        ("icrnl", b"x\r"),
        ("veol", b"x;"),
        ("veol2", b"x:"),
        ("vlnext", b"abc\x16\n"),
        ("no-iexten-vlnext", b"x\x16\n"),
        ("verase", b"x\x7f"),
        ("vkill", b"abc\x15"),
        ("istrip", &[b'x', 0x84]),
    ];
    for (mode, input) in cases {
        let (code, out, err) = run_synchronized_eof_helper(mode, input);
        if no_pty(code, &err) {
            eprintln!("skipping exec_canonical_eof_follows_live_termios_boundaries: no PTY");
            return;
        }
        assert_eq!(
            code, 0,
            "termios mode {mode}; stderr: {err}; stdout: {out:?}"
        );
        assert!(
            out.contains("PTY_TERMIOS_EOF_QUERY_OK"),
            "termios mode {mode} did not complete cleanly: {out:?}"
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn exec_canonical_eof_follows_live_linux_iuclc() {
    let (code, out, err) = run_synchronized_eof_helper("iuclc", b"xQ");
    if no_pty(code, &err) {
        eprintln!("skipping exec_canonical_eof_follows_live_linux_iuclc: no PTY");
        return;
    }
    assert_eq!(code, 0, "stderr: {err}; stdout: {out:?}");
    assert!(
        out.contains("PTY_TERMIOS_EOF_QUERY_OK"),
        "IUCLC fixture did not complete cleanly: {out:?}"
    );
}

#[cfg(unix)]
fn large_line_delimited_payload() -> Vec<u8> {
    let mut payload = Vec::with_capacity(96 * 1024);
    for index in 0..6_000 {
        payload.extend_from_slice(format!("kettle-line-{index:05}\n").as_bytes());
    }
    assert!(payload.len() > 64 * 1024);
    payload
}

#[cfg(unix)]
#[test]
fn exec_large_canonical_streams_reach_eof_without_unbounded_tracking() {
    let line_delimited = large_line_delimited_payload();
    let cases = [
        ("large-lines", line_delimited.as_slice()),
        ("large-unterminated", &[b'x'; 70 * 1024][..]),
    ];
    for (mode, input) in cases {
        let (code, out, err) = run_synchronized_eof_helper(mode, input);
        if no_pty(code, &err) {
            eprintln!(
                "skipping exec_large_canonical_streams_reach_eof_without_unbounded_tracking: no PTY"
            );
            return;
        }
        assert_eq!(
            code, 0,
            "large termios mode {mode}; stderr: {err}; stdout: {out:?}"
        );
        assert!(
            out.contains("PTY_TERMIOS_EOF_QUERY_OK"),
            "large termios mode {mode} did not receive EOF: {out:?}"
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn exec_iutf8_verase_tracks_one_complete_character() {
    let (code, out, err) = run_synchronized_eof_helper("iutf8-verase", "é\u{7f}".as_bytes());
    if no_pty(code, &err) {
        eprintln!("skipping exec_iutf8_verase_tracks_one_complete_character: no PTY");
        return;
    }
    assert_eq!(code, 0, "stderr: {err}; stdout: {out:?}");
    assert!(out.contains("PTY_TERMIOS_EOF_QUERY_OK"), "stdout: {out:?}");
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn exec_linux_vwerase_matches_n_tty_word_boundaries() {
    let (code, out, err) = run_synchronized_eof_helper("linux-vwerase", b"abc-def\x17");
    if no_pty(code, &err) {
        eprintln!("skipping exec_linux_vwerase_matches_n_tty_word_boundaries: no PTY");
        return;
    }
    assert_eq!(code, 0, "stderr: {err}; stdout: {out:?}");
    assert!(
        out.contains("PTY_TERMIOS_EOF_QUERY_OK"),
        "Linux VWERASE fixture did not complete cleanly: {out:?}"
    );
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
#[test]
fn exec_extproc_eof_is_explicit_and_preserves_terminal_replies() {
    let (code, out, err) = run_synchronized_eof_helper("extproc", b"");
    if no_pty(code, &err) {
        eprintln!("skipping exec_extproc_eof_is_explicit_and_preserves_terminal_replies: no PTY");
        return;
    }
    assert_eq!(code, 0, "stderr: {err}; stdout: {out:?}");
    assert!(
        out.contains("PTY_EXTPROC_EOF_QUERY_OK"),
        "EXTPROC fixture did not preserve terminal replies: {out:?}"
    );
    assert!(
        err.contains("stdin reached EOF but this PTY has no safe EOF signal"),
        "missing explicit EXTPROC EOF diagnostic: {err:?}"
    );
}

#[cfg(unix)]
#[test]
fn exec_timeout_and_query_handling_survive_stdin_backpressure() {
    let (code, out, err, elapsed) = run_backpressured_exec(
        "sleep 0.2; printf '\\033[5n'; sleep 30",
        "0.5",
        8 * 1024 * 1024,
    );
    assert_eq!(
        code, 124,
        "timeout lost under query/stdin backpressure; stderr: {err}; stdout: {out:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "query/stdin backpressure stalled timeout for {elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn exec_preserves_early_child_exit_when_forwarded_input_is_unconsumed() {
    let (code, out, err, elapsed) = run_backpressured_exec("true", "5.0", 8 * 1024 * 1024);
    assert_eq!(
        code, 0,
        "PTY write failure replaced authoritative early exit; stderr: {err}; stdout: {out:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "early-exit child stalled for {elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn exec_query_reply_flood_fails_at_the_bounded_arbiter_queue() {
    let (code, out, err, elapsed) = run_backpressured_exec(
        "i=0; while [ \"$i\" -lt 10000 ]; do printf '\\033[5n'; i=$((i+1)); done; sleep 30",
        "5.0",
        8 * 1024 * 1024,
    );
    assert_eq!(
        code, 125,
        "query flood did not fail closed; stderr: {err}; stdout: {out:?}"
    );
    assert!(
        err.contains("PTY reply queue exceeded its 64-message bound"),
        "missing bounded-reply diagnostic: {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "query flood stalled for {elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn exec_query_reply_flood_without_stdin_cannot_defeat_timeout() {
    let (code, out, err, elapsed) = run_no_stdin_exec(
        "i=0; while [ \"$i\" -lt 10000 ]; do printf '\\033[5n'; i=$((i+1)); done; sleep 30",
        "5.0",
    );
    assert_eq!(
        code, 125,
        "no-stdin query flood did not fail closed; stderr: {err}; stdout: {out:?}"
    );
    assert!(
        err.contains("PTY reply queue exceeded its 64-message bound"),
        "missing bounded-reply diagnostic: {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "no-stdin query flood stalled for {elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn exec_semantic_event_flood_fails_closed_without_defeating_timeout() {
    let helper = std::env::current_exe().expect("resolve integration-test helper");
    let helper = helper.to_str().expect("integration-test path is UTF-8");
    let (code, out, err, elapsed) = run_no_stdin_command(
        &[
            helper,
            "--exact",
            "pty_semantic_event_flood_helper",
            "--nocapture",
            "--test-threads=1",
        ],
        "5.0",
        &[("KETTLE_EXEC_EVENT_FLOOD_HELPER", "1")],
    );
    assert_eq!(
        code, 125,
        "semantic event flood did not fail closed; stderr: {err}; stdout: {out:?}"
    );
    assert!(
        err.contains("PTY semantic event queue exceeded its 1024-message bound"),
        "missing bounded-event diagnostic: {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "semantic event flood stalled for {elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn pty_semantic_event_flood_helper() {
    if std::env::var_os("KETTLE_EXEC_EVENT_FLOOD_HELPER").is_none() {
        return;
    }
    let frame = b"\x1b]2;hostile-title\x07";
    let mut flood = Vec::with_capacity(frame.len() * 20_000);
    for _ in 0..20_000 {
        flood.extend_from_slice(frame);
    }
    std::io::stdout().write_all(&flood).unwrap();
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(30));
}

#[cfg(unix)]
fn run_no_stdin_exec(script: &str, timeout: &str) -> (i32, String, String, Duration) {
    run_no_stdin_command(&["sh", "-c", script], timeout, &[])
}

#[cfg(unix)]
fn run_no_stdin_command(
    argv: &[&str],
    timeout: &str,
    env: &[(&str, &str)],
) -> (i32, String, String, Duration) {
    let mut cmd = kettle();
    cmd.args(["exec", "--timeout", timeout, "--strip-ansi", "--"]);
    cmd.args(argv);
    cmd.envs(env.iter().copied());
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd.spawn().expect("spawn no-stdin kettle exec");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll no-stdin kettle exec") {
            break status;
        }
        if start.elapsed() >= Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("no-stdin kettle exec exceeded its 10-second test watchdog");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader.join().expect("join stdout reader");
    let stderr = stderr_reader.join().expect("join stderr reader");
    (
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
        start.elapsed(),
    )
}

#[cfg(unix)]
fn run_backpressured_exec(
    script: &str,
    timeout: &str,
    payload_bytes: usize,
) -> (i32, String, String, Duration) {
    let mut cmd = kettle();
    cmd.args([
        "exec",
        "--timeout",
        timeout,
        "--strip-ansi",
        "--",
        "sh",
        "-c",
        script,
    ]);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd.spawn().expect("spawn backpressured kettle exec");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&vec![b'x'; payload_bytes]);
    });
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    let status = child.wait().expect("wait for backpressured exec");
    writer.join().expect("join blocked-stdin writer");
    (status.code().unwrap_or(-1), out, err, start.elapsed())
}

#[cfg(unix)]
fn run_synchronized_eof_helper(mode: &str, input: &[u8]) -> (i32, String, String) {
    use std::io::BufRead;

    let helper = std::env::current_exe().expect("resolve integration-test helper");
    let helper = helper.to_str().expect("integration-test path is UTF-8");
    let mut cmd = kettle();
    cmd.args([
        "exec",
        "--timeout",
        "5.0",
        "--strip-ansi",
        "--",
        helper,
        "--exact",
        "pty_custom_termios_eof_helper",
        "--nocapture",
        "--test-threads=1",
    ]);
    cmd.env("KETTLE_EXEC_TERMIOS_EOF_HELPER", mode);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn kettle exec");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut out = String::new();
    let mut ready = false;
    loop {
        let mut line = String::new();
        if stdout.read_line(&mut line).unwrap() == 0 {
            break;
        }
        out.push_str(&line);
        if line.contains("TERMIOS_MODE_READY") {
            ready = true;
            break;
        }
    }
    if ready {
        stdin.write_all(input).unwrap();
    }
    drop(stdin);
    stdout.read_to_string(&mut out).unwrap();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    let status = child.wait().expect("wait");
    (status.code().unwrap_or(-1), out, err)
}

#[cfg(unix)]
#[test]
fn pty_custom_termios_eof_helper() {
    let Some(mode) = std::env::var_os("KETTLE_EXEC_TERMIOS_EOF_HELPER") else {
        return;
    };
    let mode = mode.to_str().unwrap();

    let mut attrs = unsafe { std::mem::zeroed::<libc::termios>() };
    assert_eq!(unsafe { libc::tcgetattr(0, &mut attrs) }, 0);
    attrs.c_lflag |= libc::ICANON | libc::IEXTEN;
    attrs.c_lflag &= !libc::ECHO;
    attrs.c_iflag &= !(libc::IGNCR | libc::ICRNL | libc::INLCR | libc::ISTRIP);
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        attrs.c_iflag &= !libc::IUCLC;
    }
    let disabled = unsafe { libc::fpathconf(0, libc::_PC_VDISABLE) };
    assert!((0..=u8::MAX as libc::c_long).contains(&disabled));
    attrs.c_cc[libc::VEOL] = disabled as libc::cc_t;
    attrs.c_cc[libc::VEOL2] = disabled as libc::cc_t;
    attrs.c_cc[libc::VERASE] = 0x7f;
    attrs.c_cc[libc::VKILL] = 0x15;
    attrs.c_cc[libc::VLNEXT] = 0x16;
    attrs.c_cc[libc::VWERASE] = 0x17;

    let expected = match mode {
        "inlcr" => {
            attrs.c_iflag |= libc::INLCR;
            b"x\r".to_vec()
        }
        "igncr" => {
            attrs.c_iflag |= libc::IGNCR;
            b"x".to_vec()
        }
        "icrnl" => {
            attrs.c_iflag |= libc::ICRNL;
            b"x\n".to_vec()
        }
        "veol" => {
            attrs.c_cc[libc::VEOL] = b';' as libc::cc_t;
            b"x;".to_vec()
        }
        "veol2" => {
            attrs.c_cc[libc::VEOL2] = b':' as libc::cc_t;
            b"x:".to_vec()
        }
        "vlnext" => b"abc\n".to_vec(),
        "no-iexten-vlnext" => {
            attrs.c_lflag &= !libc::IEXTEN;
            b"x\x16\n".to_vec()
        }
        "verase" | "vkill" => Vec::new(),
        "istrip" => {
            attrs.c_iflag |= libc::ISTRIP;
            b"x".to_vec()
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        "iuclc" => {
            attrs.c_iflag |= libc::IUCLC;
            attrs.c_cc[libc::VEOL] = b'q' as libc::cc_t;
            b"xq".to_vec()
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        "iutf8-verase" => {
            attrs.c_iflag |= libc::IUTF8;
            Vec::new()
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        "linux-vwerase" => b"abc-".to_vec(),
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        "extproc" => {
            attrs.c_lflag |= libc::EXTPROC;
            Vec::new()
        }
        "large-lines" => large_line_delimited_payload(),
        "large-unterminated" => Vec::new(),
        other => panic!("unknown termios fixture: {other}"),
    };
    assert_eq!(unsafe { libc::tcsetattr(0, libc::TCSANOW, &attrs) }, 0);
    println!("TERMIOS_MODE_READY");
    std::io::stdout().flush().unwrap();

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    if mode == "extproc" {
        std::thread::sleep(Duration::from_millis(250));
        assert_post_eof_terminal_queries("EXTPROC mode");
        println!("PTY_EXTPROC_EOF_QUERY_OK");
        return;
    }

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    if mode == "large-unterminated" {
        assert!(
            !input.is_empty() && input.len() <= 70 * 1024 && input.iter().all(|&byte| byte == b'x'),
            "oversized canonical fixture returned invalid input: len={}",
            input.len()
        );
    } else {
        assert_eq!(input, expected, "termios fixture {mode}");
    }
    assert_post_eof_terminal_queries(mode);
    println!("PTY_TERMIOS_EOF_QUERY_OK");
}

#[test]
fn exec_json_emits_start_and_exit_events() {
    let mut argv: Vec<&str> = ECHO.to_vec();
    argv.push("jmark");
    let (code, out, err) = run_exec(&["--json"], &argv, None);
    if no_pty(code, &err) {
        eprintln!("skipping exec_json_emits_start_and_exit_events: no PTY");
        return;
    }
    assert_eq!(code, 0, "stderr: {err}");
    // Each line is a JSON object; assert the start + exit envelope shapes.
    let mut saw_start = false;
    let mut saw_exit = false;
    for line in out.lines() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v.get("event").and_then(|e| e.as_str()) {
            Some("start") => saw_start = true,
            Some("exit") => {
                saw_exit = true;
                assert_eq!(v.get("code").and_then(|c| c.as_i64()), Some(0));
            }
            _ => {}
        }
    }
    assert!(saw_start, "missing start event; out: {out:?}");
    assert!(saw_exit, "missing exit event; out: {out:?}");
}

#[test]
fn exec_record_writes_replayable_asciicast() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = private_test_scratch_root().join(format!(
        "kettle-exec-rec-{}-{nonce}.cast",
        std::process::id()
    ));
    let path_s = path.to_str().unwrap().to_string();
    let mut argv: Vec<&str> = ECHO.to_vec();
    argv.push("recmark-9z");
    let (code, _out, err) = run_exec(&["--record", &path_s, "--strip-ansi"], &argv, None);
    if no_pty(code, &err) {
        eprintln!("skipping exec_record_writes_replayable_asciicast: no PTY");
        let _ = std::fs::remove_file(&path);
        return;
    }
    assert_eq!(code, 0, "stderr: {err}");
    let contents = std::fs::read_to_string(&path).expect("recording file written");
    let mut lines = contents.lines();
    // Header is asciicast v2.
    let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(header["version"], 2);
    // At least one output event carries the marker text.
    let saw_marker = lines.any(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .ok()
            .filter(|v| v[1] == "o")
            .map(|v| v[2].as_str().unwrap_or("").contains("recmark-9z"))
            .unwrap_or(false)
    });
    assert!(
        saw_marker,
        "recording missing the child output; file: {contents:?}"
    );
    let _ = std::fs::remove_file(&path);
}
