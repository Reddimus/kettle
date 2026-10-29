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
    let mut command = command(&opts);
    supervise(
        &mut command,
        cancelled,
        opts.timeout.unwrap_or(Duration::from_secs(600)) + OUTER_GRACE,
        CANCEL_GRACE,
    )
}

#[cfg(any(target_os = "linux", test))]
fn command(opts: &ExecOpts) -> Command {
    // The procfs handle still names our running image after an update unlinks
    // it; current_exe() instead returns a path suffixed with " (deleted)".
    let mut command = Command::new("/proc/self/exe");
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
    #[cfg(target_os = "linux")]
    // SAFETY: getpid has no arguments or memory requirements.
    let parent = unsafe { libc::getpid() };
    // SAFETY: the hook uses only signal-mask, prctl and PID/signal syscalls;
    // it allocates nothing and uses initialized stack storage.
    unsafe {
        command.pre_exec(move || {
            cancellation_mask(libc::SIG_BLOCK)?;
            #[cfg(target_os = "linux")]
            parent_death_signal(parent)?;
            Ok(())
        });
    }
    command
}

#[cfg(target_os = "linux")]
fn parent_death_signal(parent: libc::pid_t) -> std::io::Result<()> {
    // SAFETY: PR_SET_PDEATHSIG takes a valid signal number and no pointers.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Registration is not retroactive. If the server died after fork but
    // before prctl, queue the same blocked signal ourselves. The internal CLI
    // unblocks it only after handlers and descendant adoption are ready.
    // SAFETY: getppid has no arguments or memory requirements.
    if unsafe { libc::getppid() } != parent {
        // SAFETY: kill targets ourselves with a valid signal number.
        if unsafe { libc::kill(libc::getpid(), libc::SIGTERM) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
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
struct Supervisor {
    child: Option<Child>,
    reaper: crossbeam_channel::Sender<Child>,
}

fn supervisor_reaper() -> std::io::Result<crossbeam_channel::Sender<Child>> {
    static REAPER: std::sync::OnceLock<Result<crossbeam_channel::Sender<Child>, String>> =
        std::sync::OnceLock::new();
    REAPER
        .get_or_init(|| {
            let (sender, receiver) = crossbeam_channel::unbounded::<Child>();
            std::thread::Builder::new()
                .name("kettle-supervisor-reaper".into())
                .spawn(move || {
                    let mut pending = Vec::<Child>::new();
                    loop {
                        if pending.is_empty() {
                            match receiver.recv() {
                                Ok(child) => pending.push(child),
                                Err(_) => return,
                            }
                        }
                        // Bound admission so even a busy sender cannot postpone
                        // polling the children whose exits we already own.
                        pending.extend(receiver.try_iter().take(64));
                        pending.retain_mut(|child| match child.try_wait() {
                            Ok(Some(_)) => false,
                            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => false,
                            _ => true,
                        });
                        std::thread::sleep(POLL_INTERVAL);
                    }
                })
                .map(|_| sender)
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .cloned()
        .map_err(|error| std::io::Error::other(error.clone()))
}

fn retire_child(
    mut child: Child,
    reaper: &crossbeam_channel::Sender<Child>,
    grace: Duration,
    kill: impl FnOnce(&mut Child),
) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    kill(&mut child);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    // The reaper is started before the child, and its loop has no shutdown
    // path while this sender exists. Ownership must survive the bounded wait.
    reaper.send(child).expect("supervisor reaper stays alive");
}

impl Supervisor {
    fn child(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("supervisor owns child until drop")
    }

    fn kill_for(&mut self, reason: &str) -> std::io::Error {
        let action = match self.child().kill() {
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
        if let Some(child) = self.child.take() {
            retire_child(child, &self.reaper, KILL_GRACE, |child| {
                let _ = child.kill();
            });
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
        let reaper = supervisor_reaper()?;
        let mut child = Supervisor {
            reaper,
            child: Some(
                command
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()?,
            ),
        };
        let mut stdout = child.child().stdout.take().expect("piped stdout");
        let mut stderr = child.child().stderr.take().expect("piped stderr");
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
                if unsafe { libc::kill(child.child().id() as libc::pid_t, libc::SIGTERM) } != 0 {
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
                && let Some(status) = child.child().try_wait()?
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

    #[cfg(target_os = "linux")]
    #[test]
    fn supervisor_parent_death_during_startup_queues_cancellation() {
        use std::os::unix::process::CommandExt as _;
        let mut command = shell("exit 0");
        // SAFETY: the forked child uses only initialized stack values and
        // signal/prctl syscalls. It consumes pending TERM before exec.
        unsafe {
            command.pre_exec(|| {
                cancellation_mask(libc::SIG_BLOCK)?;
                // A stale parent PID models death between fork and prctl.
                parent_death_signal(libc::getpid())?;
                let mut pending: libc::sigset_t = std::mem::zeroed();
                let mut signal: libc::c_int = 0;
                if libc::sigpending(&raw mut pending) != 0
                    || libc::sigismember(&raw const pending, libc::SIGTERM) != 1
                    || libc::prctl(libc::PR_GET_PDEATHSIG, &raw mut signal, 0, 0, 0) != 0
                    || signal != libc::SIGTERM
                {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                }
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&raw mut set);
                libc::sigaddset(&raw mut set, libc::SIGTERM);
                let result = libc::sigwait(&raw const set, &raw mut signal);
                if result != 0 || signal != libc::SIGTERM {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                }
                Ok(())
            });
        }
        assert!(
            command
                .status()
                .expect("parent death must queue TERM before exec")
                .success()
        );
    }

    #[test]
    fn supervisor_reaps_a_child_that_exits_after_kill_grace() {
        let directory = tempfile::tempdir().unwrap();
        let release = directory.path().join("release");
        let mut command = shell("while [ ! -f \"$1\" ]; do sleep 0.01; done");
        command.arg("fixture").arg(&release);
        let child = command.spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        let started = Instant::now();
        // Deliberately delay termination beyond the bounded owner wait. This
        // models SIGKILL waiting on uninterruptible I/O without requiring it.
        retire_child(child, &supervisor_reaper().unwrap(), Duration::ZERO, |_| {});
        assert!(started.elapsed() < Duration::from_secs(1));
        std::fs::write(&release, "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            // SAFETY: waitid observes only this fixture child and WNOWAIT
            // leaves reaping to the production owner, even after it exits.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: info is writable and pid selects this unreaped child.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as _,
                    &raw mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == -1 {
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ECHILD)
                );
                break;
            }
            if Instant::now() >= deadline {
                // SAFETY: the unreaped fixture PID remains reserved to us.
                unsafe {
                    libc::waitpid(pid, std::ptr::null_mut(), 0);
                }
                panic!("delayed supervisor exit was never reaped");
            }
            std::thread::sleep(POLL_INTERVAL);
        }
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
    fn supervisor_command_uses_stable_linux_executable() {
        let opts = ExecOpts {
            argv: vec!["true".into()],
            cols: 80,
            rows: 24,
            cwd: None,
            timeout: None,
            mode: OutputMode::Raw,
            record: None,
            forward_stdin: false,
        };
        let command = command(&opts);
        assert_eq!(command.get_program(), "/proc/self/exe");
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
        let command = command(&opts);
        assert_eq!(command.get_program(), "/proc/self/exe");
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
