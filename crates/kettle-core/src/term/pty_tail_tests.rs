//! Output a child writes just before it exits reaches the pane even when
//! the reader has fallen behind it, and a child that cannot finish exiting
//! does not outlive its pane.

/// Names the marker file the child creates once its tail is written; set
/// only for the child run of this test binary.
const CHILD_MARKER: &str = "KETTLE_TEST_PTY_TAIL_MARKER";

/// Set, with the marker, for a child that stops its own output first.
#[cfg(unix)]
const CHILD_STOPS_OUTPUT: &str = "KETTLE_TEST_PTY_TAIL_STOPPED";

/// How long a wait below may take before the test calls it hung. Every wait
/// ends on its condition first.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// The child half: print a line, wait for one from the terminal, write a
/// tail, mark that it is written, and exit at once. With
/// `CHILD_STOPS_OUTPUT` it first suspends its terminal's output, as Ctrl+S
/// does, so the tail cannot drain.
#[test]
fn pty_tail_child() {
    use std::io::{BufRead as _, Write as _};
    let Some(marker) = std::env::var_os(CHILD_MARKER) else {
        return;
    };
    let mut output = std::io::stdout().lock();
    output.write_all(b"READY\n").unwrap();
    output.flush().unwrap();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap();
    #[cfg(unix)]
    if std::env::var_os(CHILD_STOPS_OUTPUT).is_some() {
        // SAFETY: tcflow takes a descriptor and an action; stdout is open.
        assert_eq!(unsafe { libc::tcflow(1, libc::TCOOFF) }, 0);
    }
    output.write_all(&[b' '; 200]).unwrap();
    output.write_all(b"<TAIL-END>").unwrap();
    output.flush().unwrap();
    std::fs::File::create(marker).unwrap();
    std::process::exit(0);
}

/// Set for a child run of [`pty_stop_output_child`].
#[cfg(target_os = "macos")]
const STOP_OUTPUT_ONLY: &str = "KETTLE_TEST_PTY_STOP_OUTPUT";

/// The child half that only suspends its terminal's output, as Ctrl+S
/// does, and exits, leaving output stopped for the shell that ran it.
#[cfg(target_os = "macos")]
#[test]
fn pty_stop_output_child() {
    if std::env::var_os(STOP_OUTPUT_ONLY).is_none() {
        return;
    }
    // SAFETY: tcflow takes a descriptor and an action; stdout is open.
    assert_eq!(unsafe { libc::tcflow(1, libc::TCOOFF) }, 0);
    std::process::exit(0);
}

/// A pane running [`pty_tail_child`], its pump behind `gate` when given, and
/// the receiver of every byte the pump reads.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn tail_pane(
    marker: &std::path::Path,
    stops_output: bool,
    gate: Option<crossbeam_channel::Receiver<()>>,
) -> (super::Terminal, crossbeam_channel::Receiver<Vec<u8>>) {
    let mut environment: Vec<(String, String)> =
        vec![(CHILD_MARKER.into(), marker.to_str().unwrap().into())];
    if stops_output {
        environment.push((CHILD_STOPS_OUTPUT.into(), "1".into()));
    }
    let argv = [
        std::env::current_exe().unwrap().to_str().unwrap().into(),
        "--exact".into(),
        "term::pty_tail_tests::pty_tail_child".into(),
        "--nocapture".into(),
        "--quiet".into(),
    ];
    pane_running(&argv, &environment, gate)
}

/// A pane running `argv` with `environment`, its pump behind `gate` when
/// given, and the receiver of every byte the pump reads.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn pane_running(
    argv: &[String],
    environment: &[(String, String)],
    gate: Option<crossbeam_channel::Receiver<()>>,
) -> (super::Terminal, crossbeam_channel::Receiver<Vec<u8>>) {
    use super::{
        CursorShape, LaunchOverrides, PtyGeometry, PtyOutputSender, Terminal, TerminalCapabilities,
        WorkingDirectoryPolicy,
    };
    let (tx, _events) = crossbeam_channel::unbounded();
    let (output_tx, output_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
    let pane = super::test_pump_gate::with_gate(gate, || {
        Terminal::new_with_launch_environment(
            argv,
            None,
            0,
            0,
            PtyGeometry::from_cell_size(80, 6, 8, 16),
            false,
            CursorShape::Block,
            None,
            "xterm-256color",
            "truecolor",
            environment,
            &LaunchOverrides::default(),
            false,
            false,
            &crate::shell_startup::ShellStartup::default(),
            TerminalCapabilities::default(),
            WorkingDirectoryPolicy::FallbackToHome,
            tx,
            std::sync::Arc::new(|| {}),
            Some(PtyOutputSender::lossless(output_tx)),
        )
    })
    .expect("PTY child");
    (pane, output_rx)
}

/// `pid`'s `ps` state letters, or `None` once no process has that pid
/// (`ps -p` then prints nothing at all and exits 1). A `ps` that cannot run,
/// or fails otherwise, such as with a message on stderr, fails the test
/// rather than reading as a vanished child.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn process_state(pid: u32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    let stat = String::from_utf8_lossy(&output.stdout).trim().to_string();
    match output.status.code() {
        Some(0) if !stat.is_empty() => Some(stat),
        Some(1) if stat.is_empty() && output.stderr.is_empty() => None,
        _ => panic!("ps failed for pid {pid}: {output:?}"),
    }
}

/// Whether `pid` has begun to exit: macOS shows an exiting process as `E`,
/// both systems show one awaiting its reap as `Z`, and a reaped one is gone.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exit_has_begun(pid: u32) -> bool {
    process_state(pid).is_none_or(|stat| stat.contains('E') || stat.contains('Z'))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn contains(captured: &[u8], needle: &[u8]) -> bool {
    captured
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Wait for `done`, which must hold within [`HANG`].
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + HANG;
    while !done() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A real PTY child prints a line, and once the pump has read it, writes a
/// tail and exits while the pump is held back: from the moment the child
/// has begun to exit, the pump waits two seconds, well past the half second
/// macOS lets an exiting process wait for a reader before the last close of
/// its slave discards what is unread. The tail must still arrive, then EOF.
/// On macOS it was lost while Kettle let go of its own slave descriptor
/// after the first read; Linux keeps unread output across the last close,
/// so there this covers the same path without the loss.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_child_s_last_output_survives_its_exit_while_the_reader_is_held_back() {
    use std::time::Instant;

    let dir = kettle_test_support::private_tempdir("kettle-pty-tail-");
    let marker = dir.path().join("tail-written");
    // A rendezvous: each send lets exactly one read through, and returns only
    // once the pump, with output waiting, has taken it.
    let (gate_tx, gate_rx) = crossbeam_channel::bounded(0);
    let (pane, output_rx) = tail_pane(&marker, false, Some(gate_rx));
    let pid = pane.child_pid().expect("child pid");
    let deadline = Instant::now() + HANG;
    let mut captured = Vec::new();
    // Let reads through one at a time until the first line is in.
    while !contains(&captured, b"READY") {
        gate_tx
            .send_timeout((), deadline.saturating_duration_since(Instant::now()))
            .expect("the pump never had READY to read");
        captured.extend(
            output_rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("the pump read nothing"),
        );
    }
    // The child writes its tail and exits; the pump stays at the gate.
    pane.write(b"\n");
    wait_until("the child never wrote its tail", || marker.exists());
    wait_until("the child never began to exit", || exit_has_begun(pid));
    std::thread::sleep(std::time::Duration::from_secs(2));
    drop(gate_tx);
    wait_until("the reader never finished", || {
        captured.extend(output_rx.try_iter().flatten());
        pane.take_exited()
    });
    wait_until("the child never exited", || {
        pane.child_exit_code().is_some()
    });
    captured.extend(output_rx.try_iter().flatten());
    assert_eq!(pane.child_exit_code(), Some(0));
    assert_eq!(
        pane.pty_read_status(),
        super::PtyReadStatus::Eof,
        "the reader ends at EOF, not at the bounded wait"
    );
    assert!(
        contains(&captured, b"<TAIL-END>"),
        "the child's last output was lost: {:?}",
        String::from_utf8_lossy(&captured)
    );
}

/// On macOS a child that stops its terminal's output, as Ctrl+S does, then
/// writes and exits cannot drain its tail. While the pump keeps Kettle's slave
/// open for that tail, the child waits in its exit (`E`) with nothing able to
/// read it; even SIGKILL does not end that wait. Closing the pane must still
/// end the child. Linux blocks the write itself while output is stopped, so
/// there the child never reaches its exit.
#[cfg(target_os = "macos")]
#[test]
fn closing_a_pane_ends_a_child_whose_stopped_output_cannot_drain() {
    let dir = kettle_test_support::private_tempdir("kettle-pty-stopped-");
    let marker = dir.path().join("tail-written");
    let (pane, output_rx) = tail_pane(&marker, true, None);
    let pid = pane.child_pid().expect("child pid");
    wait_until("the child never printed READY", || {
        output_rx.try_iter().any(|chunk| contains(&chunk, b"READY"))
    });
    pane.write(b"\n");
    wait_until("the child never wrote its tail", || marker.exists());
    wait_until("the child never began to exit", || {
        process_state(pid).is_some_and(|stat| stat.contains('E'))
    });
    drop(pane);
    wait_until("the closed pane left its child running", || {
        process_state(pid).is_none()
    });
}

/// The same, when a background process that ignores SIGHUP also holds the
/// slave: the shell stops its output, writes, and exits, and waits in its
/// exit to drain output nothing can take. Closing Kettle's slave is then not
/// the last close, so closing the pane must restart the stopped output too.
#[cfg(target_os = "macos")]
#[test]
fn closing_a_pane_ends_a_stopped_exit_while_a_descendant_holds_the_slave() {
    let dir = kettle_test_support::private_tempdir("kettle-pty-held-");
    let marker = dir.path().join("tail-written");
    let holder_pid = dir.path().join("holder-pid");
    let script = r#"(trap '' HUP; exec sleep 30) & echo $! > "$3"
"$1" --exact term::pty_tail_tests::pty_stop_output_child --quiet
echo tail; : > "$2"; exit 0"#;
    let argv = [
        "/bin/sh".to_string(),
        "-c".into(),
        script.into(),
        "sh".into(),
        std::env::current_exe().unwrap().to_str().unwrap().into(),
        marker.to_str().unwrap().into(),
        holder_pid.to_str().unwrap().into(),
    ];
    let (pane, _output) = pane_running(&argv, &[(STOP_OUTPUT_ONLY.into(), "1".into())], None);
    let pid = pane.child_pid().expect("child pid");
    wait_until("the shell never wrote its tail", || marker.exists());
    let holder: libc::pid_t = std::fs::read_to_string(&holder_pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    struct Reap(libc::pid_t);
    impl Drop for Reap {
        fn drop(&mut self) {
            // SAFETY: kill takes a pid and a signal; this pid is the background
            // holder the test's own shell started and recorded.
            unsafe { libc::kill(self.0, libc::SIGKILL) };
        }
    }
    let _reap = Reap(holder);
    wait_until("the shell never began to exit", || {
        process_state(pid).is_some_and(|stat| stat.contains('E'))
    });
    drop(pane);
    wait_until("the closed pane left its stopped shell in its exit", || {
        process_state(pid).is_none()
    });
}
