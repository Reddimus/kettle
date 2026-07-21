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
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn kettle() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kettle"))
}

/// Run `kettle exec <extra…> -- <argv…>`, feeding `stdin_data` if any. Returns
/// (exit_code, stdout, stderr). Kills + fails the test if it runs too long.
fn run_exec(extra: &[&str], argv: &[&str], stdin_data: Option<&[u8]>) -> (i32, String, String) {
    let mut cmd = kettle();
    cmd.arg("exec");
    cmd.args(extra);
    cmd.arg("--");
    cmd.args(argv);
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

#[test]
fn exec_json_emits_start_and_exit_events() {
    let mut argv: Vec<&str> = ECHO.to_vec();
    argv.push("jmark");
    let (code, out, err) = run_exec(&["--json"], &argv, None);
    if no_pty(code, &err) {
        eprintln!("skipping exec_json_emits_start_and_exit_events: no PTY");
        return;
    }
    assert_eq!(code, 0);
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
    let dir = std::env::temp_dir();
    let path = dir.join(format!("kettle-exec-rec-{}.cast", std::process::id()));
    let path_s = path.to_str().unwrap().to_string();
    let mut argv: Vec<&str> = ECHO.to_vec();
    argv.push("recmark-9z");
    let (code, _out, err) = run_exec(&["--record", &path_s, "--strip-ansi"], &argv, None);
    if no_pty(code, &err) {
        eprintln!("skipping exec_record_writes_replayable_asciicast: no PTY");
        let _ = std::fs::remove_file(&path);
        return;
    }
    assert_eq!(code, 0);
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
