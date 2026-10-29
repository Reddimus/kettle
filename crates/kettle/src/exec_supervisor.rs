//! One process per Linux MCP command. Kept portable across Unix for watchdog
//! and pipe-pressure tests on hosts without Linux process containment.

use super::{EXIT_INTERNAL, ExecOpts, OutputMode, TailSink};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const CAP: usize = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const CANCEL_GRACE: Duration = Duration::from_secs(5);
const OUTER_GRACE: Duration = Duration::from_secs(10);
const PIPE_GRACE: Duration = Duration::from_millis(250);
const KILL_GRACE: Duration = Duration::from_millis(250);

#[cfg(target_os = "linux")]
pub(super) fn capture(opts: ExecOpts, cancelled: Option<&AtomicBool>) -> (i32, String) {
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            return (
                EXIT_INTERNAL,
                format!("kettle exec: cannot locate supervisor: {error}\n"),
            );
        }
    };
    let mut command = command(&executable, &opts);
    supervise(
        &mut command,
        cancelled,
        opts.timeout.unwrap_or(Duration::from_secs(600)) + OUTER_GRACE,
        CANCEL_GRACE,
    )
}

#[cfg(any(target_os = "linux", test))]
fn command(executable: &std::path::Path, opts: &ExecOpts) -> Command {
    let mut command = Command::new(executable);
    command
        .args(["exec", "--internal-capture-tail", "--cols"])
        .arg(opts.cols.to_string())
        .arg("--rows")
        .arg(opts.rows.to_string());
    if let Some(cwd) = &opts.cwd {
        let mut argument = std::ffi::OsString::from("--cwd=");
        argument.push(cwd);
        command.arg(argument);
    }
    if let Some(timeout) = opts.timeout {
        command
            .arg("--timeout")
            .arg(timeout.as_secs_f64().to_string());
    }
    if opts.mode == OutputMode::StripAnsi {
        command.arg("--strip-ansi");
    }
    command.arg("--").args(&opts.argv);
    use std::os::unix::process::CommandExt as _;
    // Block cancellation before exec. A request can be cancelled as soon as
    // spawn returns, before the new program has installed its handlers. The
    // internal CLI unblocks only after those handlers and adoption are ready.
    // SAFETY: pre_exec calls only async-signal-safe sigset/sigprocmask functions,
    // allocates nothing, and uses initialized stack storage.
    unsafe {
        command.pre_exec(|| cancellation_mask(libc::SIG_BLOCK));
    }
    command
}

pub(super) fn cancellation_mask(how: libc::c_int) -> std::io::Result<()> {
    // SAFETY: all-zero sigset_t is valid storage, initialized by sigemptyset;
    // all functions operate only on this local mask, with valid signals.
    let result = unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&raw mut mask) != 0
            || libc::sigaddset(&raw mut mask, libc::SIGTERM) != 0
            || libc::sigaddset(&raw mut mask, libc::SIGINT) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        libc::sigprocmask(how, &raw const mask, std::ptr::null_mut())
    };
    if result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Kill on every error path, but never perform an unbounded wait (including
/// when the supervisor is stuck in uninterruptible kernel sleep).
struct Supervisor(Child);
impl Supervisor {
    fn kill_for(&mut self, reason: &str) -> std::io::Error {
        let action = match self.0.kill() {
            Ok(()) => "sent SIGKILL".to_string(),
            Err(error) => format!("SIGKILL failed: {error}"),
        };
        std::io::Error::other(format!(
            "{reason}; {action}; descendant teardown is unverified"
        ))
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let deadline = Instant::now() + KILL_GRACE;
            while Instant::now() < deadline {
                if matches!(self.0.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> std::io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: fd belongs to a live pipe; F_GETFL takes no extra argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the same live pipe and its existing flags plus O_NONBLOCK.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// A continuously refilled pipe cannot starve the other pipe or the watchdog.
fn drain(pipe: &mut impl Read, tail: &mut TailSink) -> std::io::Result<(bool, bool)> {
    let mut bytes = [0; 8192];
    for _ in 0..8 {
        match pipe.read(&mut bytes) {
            Ok(0) => return Ok((true, false)),
            Ok(count) => {
                tail.write_all(&bytes[..count])?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok((false, false));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok((false, true))
}

fn supervise(
    command: &mut Command,
    cancelled: Option<&AtomicBool>,
    outer: Duration,
    cancel_grace: Duration,
) -> (i32, String) {
    let mut output = TailSink::new(CAP);
    let mut diagnostics = TailSink::new(CAP);
    let result = (|| -> std::io::Result<i32> {
        let started = Instant::now();
        if super::children_reap_themselves()? {
            return Err(std::io::Error::other(
                "SIGCHLD auto-reaps children; cannot safely own a supervisor PID",
            ));
        }
        let mut child = Supervisor(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let mut stdout = child.0.stdout.take().expect("piped stdout");
        let mut stderr = child.0.stderr.take().expect("piped stderr");
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let mut stdout_eof = false;
        let mut stderr_eof = false;
        let mut cancelling = None;
        let mut exited = None;
        loop {
            let now = Instant::now();
            if started.elapsed() >= outer {
                return Err(
                    child.kill_for("supervisor exceeded command timeout plus 10s outer bound")
                );
            }
            if exited.is_none()
                && cancelled.is_some_and(|flag| flag.load(Ordering::Acquire))
                && cancelling.is_none()
            {
                // An unreaped Child reserves its PID, so this signal cannot
                // target an unrelated process after PID reuse.
                // SAFETY: this is the positive PID of our unreaped child.
                if unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGTERM) } != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                cancelling = Some(now);
            }
            if cancelling.is_some_and(|sent: Instant| now.duration_since(sent) >= cancel_grace)
                && exited.is_none()
            {
                return Err(child.kill_for("supervisor ignored SIGTERM beyond cancellation grace"));
            }
            // Multiplex both nonblocking pipes in bounded slices; neither a
            // full stderr pipe nor an inherited open writer can park a worker.
            let mut backlog = false;
            if !stdout_eof {
                let (eof, more) = drain(&mut stdout, &mut output)?;
                stdout_eof = eof;
                backlog |= more;
            }
            if !stderr_eof {
                let (eof, more) = drain(&mut stderr, &mut diagnostics)?;
                stderr_eof = eof;
                backlog |= more;
            }
            if exited.is_none()
                && let Some(status) = child.0.try_wait()?
            {
                exited = Some((status, Instant::now()));
            }
            if let Some((status, at)) = exited {
                if stdout_eof && stderr_eof {
                    return status.code().ok_or_else(|| {
                        std::io::Error::other(
                            "supervisor died by signal; descendant teardown is unverified",
                        )
                    });
                }
                if at.elapsed() >= PIPE_GRACE {
                    return Err(std::io::Error::other(
                        "supervisor pipes did not close after exit; output or descendant teardown is incomplete",
                    ));
                }
            }
            if !backlog {
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    })();
    let code = match result {
        Ok(code) => code,
        Err(error) => {
            let _ = output.write_all(diagnostics.tail());
            let _ = write!(output, "\nkettle exec: supervisor failure: {error}\n");
            EXIT_INTERNAL
        }
    };
    (code, String::from_utf8_lossy(output.tail()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn supervisor_drains_both_pipes_with_bounded_tails() {
        let (code, output) = supervise(
            &mut shell(
                "head -c 2100000 /dev/zero >&2; head -c 2100000 /dev/zero | tr '\\000' x; printf TAIL; exit 17",
            ),
            None,
            OUTER_GRACE,
            CANCEL_GRACE,
        );
        assert_eq!(code, 17);
        assert_eq!(output.len(), CAP);
        assert!(output.ends_with("TAIL"));
    }

    #[test]
    fn supervisor_outer_bound_kills_a_wedged_child() {
        let started = Instant::now();
        let (code, output) = supervise(
            &mut shell("exec sleep 300"),
            None,
            Duration::from_millis(100),
            CANCEL_GRACE,
        );
        assert_eq!(code, EXIT_INTERNAL);
        assert!(
            output.contains("outer bound") && output.contains("SIGKILL"),
            "{output}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn supervisor_escalates_when_sigterm_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let mut command =
            shell("head -c 2100000 /dev/zero >&2; trap '' TERM; : > \"$1\"; while :; do :; done");
        command.arg("fixture").arg(&ready);
        let trigger = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !ready.exists() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(POLL_INTERVAL);
            }
            flag.store(true, Ordering::Release);
        });
        let (code, output) = supervise(
            &mut command,
            Some(&cancelled),
            OUTER_GRACE,
            Duration::from_millis(100),
        );
        trigger.join().unwrap();
        assert_eq!(code, EXIT_INTERNAL);
        assert!(
            output.contains("ignored SIGTERM") && output.contains("SIGKILL"),
            "{output}"
        );
    }

    #[test]
    fn supervisor_does_not_wait_forever_for_inherited_pipes() {
        let started = Instant::now();
        let (code, output) = supervise(
            &mut shell("sleep 1 & exit 0"),
            None,
            OUTER_GRACE,
            CANCEL_GRACE,
        );
        assert_eq!(code, EXIT_INTERNAL);
        assert!(output.contains("pipes did not close"), "{output}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn supervisor_refuses_auto_reaping_children() {
        kettle_test_support::run_alone_in_child(
            "exec::supervisor::tests::supervisor_refuses_auto_reaping_children",
            || {
                // SAFETY: this isolated test process installs an integer disposition;
                // no other test or command can depend on its SIGCHLD behavior.
                unsafe {
                    libc::signal(libc::SIGCHLD, libc::SIG_IGN);
                }
                let directory = tempfile::tempdir().unwrap();
                let marker = directory.path().join("started");
                let mut command = shell(": > \"$1\"");
                command.arg("fixture").arg(&marker);
                let (code, output) = supervise(&mut command, None, OUTER_GRACE, CANCEL_GRACE);
                assert_eq!(code, EXIT_INTERNAL);
                assert!(output.contains("SIGCHLD auto-reaps"), "{output}");
                assert!(!marker.exists());
            },
        );
    }

    #[test]
    fn supervisor_command_preserves_arguments_and_geometry() {
        let opts = ExecOpts {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "printf '%s' \"$1\"".into(),
                "--cwd".into(),
                "a b; $HOME".into(),
            ],
            cols: 91,
            rows: 37,
            cwd: Some("/a b".into()),
            timeout: Some(Duration::from_millis(125)),
            mode: OutputMode::StripAnsi,
            record: None,
            forward_stdin: false,
        };
        let command = command(std::path::Path::new("/absolute/kettle"), &opts);
        assert_eq!(command.get_program(), "/absolute/kettle");
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "exec",
                "--internal-capture-tail",
                "--cols",
                "91",
                "--rows",
                "37",
                "--cwd=/a b",
                "--timeout",
                "0.125",
                "--strip-ansi",
                "--",
                "sh",
                "-c",
                "printf '%s' \"$1\"",
                "--cwd",
                "a b; $HOME"
            ]
        );
    }
}
