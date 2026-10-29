//! Real process-boundary regressions for the Linux per-command MCP supervisor.
#![cfg(unix)]

use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::Duration;
use std::time::Instant;

struct Peer {
    child: Child,
    input: ChildStdin,
    replies: Receiver<Value>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    fn new() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kettle"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, replies) = sync_channel(8);
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                let value = serde_json::from_str(&line).unwrap();
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        let mut peer = Self {
            child,
            input,
            replies,
            reader: Some(reader),
        };
        peer.send(
            json!({"jsonrpc":"2.0", "id":0, "method":"initialize", "params":{
            "protocolVersion":"2025-11-25", "capabilities":{},
            "clientInfo":{"name":"supervisor-test", "version":"1"}}}),
        );
        assert_eq!(peer.reply()["id"], 0);
        peer.send(json!({"jsonrpc":"2.0", "method":"notifications/initialized"}));
        peer
    }

    fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").unwrap();
    }

    fn run(&mut self, id: u64, arguments: Value) {
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":"tools/call",
            "params":{"name":"kettle_run", "arguments":arguments}}));
    }

    fn reply(&self) -> Value {
        self.replies
            .recv_timeout(Duration::from_secs(25))
            .expect("bounded MCP reply")
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_capture_matches_internal_in_process_capture() {
    let mut peer = Peer::new();
    let directory = tempfile::tempdir().unwrap();
    for (script, expected_code) in [
        (
            "printf '\\033[31mred\\033[0m'; printf error >&2; pwd; stty size; exit 7",
            7,
        ),
        (
            "head -c 1100000 /dev/zero | tr '\\000' x; printf TAIL; exit 9",
            9,
        ),
        (
            "printf '\\303\\251'; head -c 1048575 /dev/zero | tr '\\000' x; exit 11",
            11,
        ),
    ] {
        for strip in [false, true] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_kettle"));
            command
                .args([
                    "exec",
                    "--internal-capture-tail",
                    "--timeout",
                    "10",
                    "--cols",
                    "91",
                    "--rows",
                    "37",
                    "--cwd",
                ])
                .arg(directory.path());
            if strip {
                command.arg("--strip-ansi");
            }
            let capture = command
                .args(["--", "sh", "-c", script])
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert_eq!(
                capture.status.code(),
                Some(expected_code),
                "{}",
                String::from_utf8_lossy(&capture.stderr)
            );
            let output = String::from_utf8_lossy(&capture.stdout);
            peer.run(
                1,
                json!({"command":["sh", "-c", script], "strip_ansi":strip,
                "cwd":directory.path(), "cols":91, "rows":37, "timeout_s":10}),
            );
            let reply = peer.reply();
            let result = &reply["result"];
            let full = format!("exit code: {}\n\n{output}", capture.status.code().unwrap());
            let truncated = full.len() > 512 * 1024;
            let expected = if truncated {
                const MARKER: &str = "\n\n[... Kettle MCP result truncated ...]\n\n";
                let budget = 512 * 1024 - MARKER.len();
                let mut head = budget / 2;
                while !full.is_char_boundary(head) {
                    head -= 1;
                }
                let mut tail = full.len() - (budget - head);
                while !full.is_char_boundary(tail) {
                    tail += 1;
                }
                format!("{}{}{}", &full[..head], MARKER, &full[tail..])
            } else {
                full
            };
            assert_eq!(result["content"][0]["text"], expected);
            assert_eq!(
                result["structuredContent"],
                json!({
                "exit_code":capture.status.code().unwrap(), "truncated":truncated})
            );
            assert_eq!(result["isError"], true);
        }
    }
}

#[test]
fn real_mcp_stdout_failure_returns_failure_status() {
    let mut peer = Peer::new();
    // Stop the reader, then let its next response close the actual pipe.
    let (unused, replacement) = sync_channel(1);
    drop(unused);
    drop(std::mem::replace(&mut peer.replies, replacement));
    peer.send(json!({"jsonrpc":"2.0", "id":1, "method":"ping"}));
    let reader = peer.reader.take().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !reader.is_finished() {
        assert!(
            Instant::now() < deadline,
            "stdout reader did not close its pipe"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    reader.join().unwrap();
    peer.send(json!({"jsonrpc":"2.0", "id":2, "method":"ping"}));
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = peer.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "broken stdout did not stop MCP");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(1));
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::path::Path;

    struct Descendant {
        pid: i32,
        handle: OwnedFd,
    }

    impl Descendant {
        fn read(path: &Path) -> Self {
            let deadline = Instant::now() + Duration::from_secs(10);
            let pid = loop {
                if let Ok(text) = std::fs::read_to_string(path)
                    && let Ok(pid) = text.trim().parse::<i32>()
                {
                    break pid;
                }
                assert!(
                    Instant::now() < deadline,
                    "fixture did not publish sleep PID"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            // SAFETY: pidfd_open takes a positive fixture PID and zero flags.
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            assert!(fd >= 0, "fixture must be alive when its identity is pinned");
            // SAFETY: successful pidfd_open returned a new owned descriptor.
            let handle = unsafe { OwnedFd::from_raw_fd(fd as i32) };
            Self { pid, handle }
        }

        fn wait_stopped(&self) {
            let deadline = Instant::now() + Duration::from_secs(8);
            while self.alive() {
                assert!(
                    Instant::now() < deadline,
                    "sleep PID {} survived cancellation",
                    self.pid
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn alive(&self) -> bool {
            let mut poll = libc::pollfd {
                fd: self.handle.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll points to one initialized descriptor, timeout is zero.
            let ready = unsafe { libc::poll(&raw mut poll, 1, 0) };
            assert!(
                ready >= 0,
                "pidfd poll failed: {}",
                std::io::Error::last_os_error()
            );
            if ready == 0 {
                return true;
            }
            assert_ne!(poll.revents & libc::POLLIN, 0, "unexpected pidfd events");
            false
        }
    }

    impl Drop for Descendant {
        fn drop(&mut self) {
            // SAFETY: this live pidfd pins only the fixture, even after PID reuse.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.handle.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }

    fn script(pid: &Path, release: &Path) -> String {
        format!(
            "setsid sh -c 'sleep 300 & p=$!; echo $p > \"$1\"; wait' sh '{}' </dev/null >/dev/null 2>&1 & \
            while [ ! -s '{}' ]; do sleep 0.01; done; echo ESCAPED $(cat '{}'); \
            while [ ! -f '{}' ]; do sleep 0.01; done; printf finished",
            pid.display(),
            pid.display(),
            pid.display(),
            release.display()
        )
    }

    #[test]
    fn mcp_supervisor_preserves_setsid_on_exit_and_stops_on_timeout_and_cancel() {
        for mode in ["exit", "timeout", "cancel"] {
            let directory = tempfile::tempdir().unwrap();
            let pid = directory.path().join("pid");
            let release = directory.path().join("release");
            let mut peer = Peer::new();
            peer.run(
                1,
                json!({"command":["sh", "-c", script(&pid, &release)],
                "timeout_s":if mode == "timeout" { 2 } else { 20 }}),
            );
            let descendant = Descendant::read(&pid);
            if mode == "exit" {
                std::fs::write(&release, "").unwrap();
            }
            if mode == "cancel" {
                peer.send(json!({"jsonrpc":"2.0", "method":"notifications/cancelled",
                "params":{"requestId":1}}));
            }
            if mode == "cancel" {
                descendant.wait_stopped();
                peer.send(json!({"jsonrpc":"2.0", "id":9, "method":"ping"}));
                assert_eq!(peer.reply()["id"], 9, "cancelled calls emit no reply");
            } else {
                let reply = peer.reply();
                let code = if mode == "exit" { 0 } else { 124 };
                assert_eq!(
                    reply["result"]["structuredContent"]["exit_code"], code,
                    "{reply}"
                );
                assert!(
                    reply["result"]["content"][0]["text"]
                        .as_str()
                        .unwrap()
                        .contains(&format!("ESCAPED {}", descendant.pid)),
                    "{reply}"
                );
            }
            assert_eq!(
                descendant.alive(),
                mode == "exit",
                "{mode}: unexpected lifetime for sleep PID {}",
                descendant.pid
            );
        }
    }

    #[test]
    fn cancelling_one_mcp_supervisor_preserves_the_other_tree_and_output() {
        let directory = tempfile::tempdir().unwrap();
        let mut peer = Peer::new();
        let mut descendants = Vec::new();
        for id in [1, 2] {
            let pid = directory.path().join(format!("pid{id}"));
            let release = directory.path().join(format!("release{id}"));
            peer.run(
                id,
                json!({"command":["sh", "-c", script(&pid, &release)], "timeout_s":20}),
            );
            descendants.push(Descendant::read(&pid));
        }
        peer.send(
            json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":1}}),
        );
        descendants[0].wait_stopped();
        assert!(!descendants[0].alive());
        assert!(
            descendants[1].alive(),
            "cancelling run 1 killed run 2's sleep"
        );
        std::fs::write(directory.path().join("release2"), "").unwrap();
        let reply = peer.reply();
        assert_eq!(reply["id"], 2);
        assert_eq!(
            reply["result"]["structuredContent"]["exit_code"], 0,
            "{reply}"
        );
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("finished") && text.contains(&format!("ESCAPED {}", descendants[1].pid)),
            "{text}"
        );
        assert!(
            descendants[1].alive(),
            "normal exit must preserve escaped daemons"
        );
    }

    #[test]
    fn server_death_stops_supervisor_and_escaped_command() {
        for signal in [libc::SIGTERM, libc::SIGKILL] {
            let directory = tempfile::tempdir().unwrap();
            let pid = directory.path().join("pid");
            let release = directory.path().join("release");
            let mut peer = Peer::new();
            peer.run(
                1,
                json!({"command":["sh", "-c", script(&pid, &release)], "timeout_s":600}),
            );
            let descendant = Descendant::read(&pid);
            let children = std::fs::read_dir(format!("/proc/{}/task", peer.child.id()))
                .unwrap()
                .flat_map(|entry| {
                    std::fs::read_to_string(entry.unwrap().path().join("children"))
                        .unwrap()
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            assert_eq!(children.len(), 1, "one supervisor per command");
            let supervisor_pid = directory.path().join("supervisor-pid");
            std::fs::write(&supervisor_pid, &children[0]).unwrap();
            let supervisor = Descendant::read(&supervisor_pid);
            // SAFETY: the server Child is owned and unreaped by this test.
            assert_eq!(unsafe { libc::kill(peer.child.id() as i32, signal) }, 0);
            peer.child.wait().unwrap();
            descendant.wait_stopped();
            supervisor.wait_stopped();
        }
    }

    #[test]
    fn stdout_failure_stops_running_supervised_tools_before_server_exit() {
        let directory = tempfile::tempdir().unwrap();
        let pid = directory.path().join("pid");
        let release = directory.path().join("release");
        let mut child = Command::new(env!("CARGO_BIN_EXE_kettle"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        for message in [
            json!({"jsonrpc":"2.0", "id":0, "method":"initialize", "params":{
                "protocolVersion":"2025-11-25", "capabilities":{},
                "clientInfo":{"name":"stdout-failure-test", "version":"1"}}}),
            json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
        ] {
            writeln!(input, "{message}").unwrap();
        }
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0", "id":1, "method":"tools/call",
            "params":{"name":"kettle_run", "arguments":{
                "command":["sh", "-c", script(&pid, &release)], "timeout_s":20}}})
        )
        .unwrap();
        let descendant = Descendant::read(&pid);
        drop(child.stdout.take());
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0", "id":2, "method":"ping"})
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("stdout failure shutdown hung");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(1));
        assert!(
            !descendant.alive(),
            "server exit leaked sleep PID {}",
            descendant.pid
        );
    }

    #[test]
    fn exec_normal_exit_preserves_escaped_daemon() {
        let directory = tempfile::tempdir().unwrap();
        let pid = directory.path().join("pid");
        let release = directory.path().join("release");
        let mut child = Command::new(env!("CARGO_BIN_EXE_kettle"))
            .args([
                "exec",
                "--timeout",
                "20",
                "--",
                "sh",
                "-c",
                &script(&pid, &release),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let descendant = Descendant::read(&pid);
        std::fs::write(&release, "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("normal completion hung");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(0));
        assert!(
            descendant.alive(),
            "normal CLI completion killed escaped daemon"
        );
    }

    #[test]
    fn exec_sigterm_cancels_and_contains_setsid_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let pid = directory.path().join("pid");
        let release = directory.path().join("release");
        let mut child = Command::new(env!("CARGO_BIN_EXE_kettle"))
            .args([
                "exec",
                "--timeout",
                "20",
                "--",
                "sh",
                "-c",
                &script(&pid, &release),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let descendant = Descendant::read(&pid);
        // SAFETY: child is an unreaped process owned by this test, SIGTERM is valid.
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("SIGTERM teardown hung");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(130));
        assert!(
            !descendant.alive(),
            "sleep PID {} survived SIGTERM",
            descendant.pid
        );
    }
}
