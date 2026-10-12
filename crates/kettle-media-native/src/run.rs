//! Running a trusted tool: contained, with an empty environment, `/` as its
//! working directory, the input file (or nothing) as stdin, stderr
//! discarded, stdout read up to a cap and one byte past it, all within a
//! deadline. A tool still running at the deadline, or writing past its cap,
//! is killed and reaped; so is one whose caller stops waiting for it.
//!
//! A tool that plays a video is started the same way and read as it writes,
//! a frame at a time, within a processor-time limit instead of a deadline:
//! it lasts as long as its session, and is killed and reaped when the
//! session lets it go. Its output ending says how it then exited.

use std::ffi::OsStr;
use std::fs::File;
use std::io::Read as _;
use std::os::unix::process::ExitStatusExt as _;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::tools::Tool;

/// Why a run gave no answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunError {
    /// The tool changed since it was trusted, or could not be started
    /// contained.
    Unavailable,
    /// The deadline passed first.
    Deadline,
}

/// What a run wrote and how it ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Ran {
    /// Its output, up to the cap.
    pub(crate) stdout: Vec<u8>,
    /// It wrote more than the cap (and was killed).
    pub(crate) overran: bool,
    /// It exited with status 0.
    pub(crate) success: bool,
}

/// How long a streamed tool may take to exit once its output has ended.
const EXIT_WITHIN: Duration = Duration::from_secs(1);

/// A started tool, killed and reaped unless it was waited for.
struct Running(Option<Child>);

impl Running {
    fn child(&mut self) -> &mut Child {
        self.0
            .as_mut()
            .expect("a running tool's child is only taken by wait or drop")
    }

    /// Wait for it to exit until `deadline`, polling; killed past it.
    fn wait_until(mut self, deadline: Instant) -> Result<bool, RunError> {
        let mut pause = Duration::from_micros(250);
        loop {
            match self.child().try_wait() {
                Ok(Some(status)) => {
                    self.0 = None;
                    return Ok(status.success());
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(Duration::from_millis(5));
                }
                // Dropping it kills and reaps it.
                Ok(None) => return Err(RunError::Deadline),
                Err(_) => return Ok(false),
            }
        }
    }

    /// How it exits within `within`, reaped if it does; one that does not
    /// is killed and reaped when dropped.
    fn exit_within(&mut self, within: Duration) -> Exit {
        let deadline = Instant::now() + within;
        let mut pause = Duration::from_micros(250);
        loop {
            let Some(child) = self.0.as_mut() else {
                return Exit::Failed;
            };
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.0 = None;
                    return if status.success() {
                        Exit::Clean
                    } else if status.signal().is_some() {
                        Exit::Killed
                    } else {
                        Exit::Failed
                    };
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(Duration::from_millis(5));
                }
                Ok(None) => return Exit::Stuck,
                Err(_) => return Exit::Failed,
            }
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            // A sandbox may refuse the kill (Linux before Landlock can scope
            // signals): the job has failed anyway, and the decoder, which
            // cannot leave the worker's process group, goes when Kettle kills
            // that group after the worker exits. Waiting for it here would
            // only hold the worker up; one that has already exited is still
            // reaped.
            if child.kill().is_ok() {
                let _ = child.wait();
            } else {
                let _ = child.try_wait();
            }
        }
    }
}

/// Start `tool` contained with `args`, `input` as its stdin (none reads as
/// empty) and its output piped, within `cpu_seconds` of processor time when
/// given.
fn start<A: AsRef<OsStr>>(
    tool: &Tool,
    args: &[A],
    input: Option<File>,
    cpu_seconds: Option<u64>,
) -> Result<(Running, ChildStdout), RunError> {
    tool.check().map_err(|_| RunError::Unavailable)?;
    let mut command = Command::new(tool.path());
    command
        .args(args)
        .env_clear()
        .current_dir("/")
        .stdin(input.map_or_else(Stdio::null, Stdio::from))
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::contain::contain(&mut command, cpu_seconds);
    let mut running = Running(Some(command.spawn().map_err(|_| RunError::Unavailable)?));
    let stdout = running.child().stdout.take().ok_or(RunError::Unavailable)?;
    Ok((running, stdout))
}

/// A tool read as it writes, in pieces of a fixed size: killed and reaped
/// once dropped, its output closed first.
pub(crate) struct Streaming {
    stdout: ChildStdout,
    /// Waited for once the output ends; otherwise killed and reaped after
    /// the output is closed.
    running: Running,
}

/// What a streamed tool gave for a piece.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    /// The piece, whole.
    Filled,
    /// Its output ended at a piece's edge, and it then exited so.
    Ended(Exit),
}

/// How a streamed tool exited once its output ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Exit {
    /// With status 0.
    Clean,
    /// With another status.
    Failed,
    /// By a signal: its processor-time limit, or a crash.
    Killed,
    /// Not within a second: it is killed when the stream is dropped.
    Stuck,
}

impl Streaming {
    /// Fill `piece` from the output: `Filled` once it is full, `Ended` with
    /// how the tool exited when the output ended before its first byte, and
    /// an error when it ended inside it or could not be read.
    pub(crate) fn next_piece(&mut self, piece: &mut [u8]) -> Result<Piece, ()> {
        let mut filled = 0;
        while filled < piece.len() {
            match self.stdout.read(&mut piece[filled..]) {
                Ok(0) if filled == 0 => {
                    return Ok(Piece::Ended(self.running.exit_within(EXIT_WITHIN)));
                }
                Ok(0) => return Err(()),
                Ok(read) => filled += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(()),
            }
        }
        Ok(Piece::Filled)
    }
}

/// Start `tool` with `args` and `input` as its stdin, to be read as it
/// writes, within `cpu_seconds` of processor time.
pub(crate) fn stream<A: AsRef<OsStr>>(
    tool: &Tool,
    args: &[A],
    input: File,
    cpu_seconds: u64,
) -> Result<Streaming, RunError> {
    let (running, stdout) = start(tool, args, Some(input), Some(cpu_seconds))?;
    Ok(Streaming { stdout, running })
}

/// Run `tool` with `args`, `input` as its stdin (none reads as empty), and
/// read at most `cap` bytes of its output before `deadline`.
pub(crate) fn run<A: AsRef<OsStr>>(
    tool: &Tool,
    args: &[A],
    input: Option<File>,
    cap: usize,
    deadline: Instant,
) -> Result<Ran, RunError> {
    let (running, stdout) = start(tool, args, input, None)?;
    let (sender, received) = mpsc::channel();
    // The tool can start no process to hold the pipe open, so the reader
    // ends once the tool does, killed or not.
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::with_capacity(cap.min(1 << 20));
        let read = stdout
            .take(cap as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = sender.send(read);
    });
    let remaining = deadline.saturating_duration_since(Instant::now());
    let read = received.recv_timeout(remaining);
    let Ok(read) = read else {
        // Killed and reaped; the reader is left to end with the pipe rather
        // than waited for, in case anything else still holds it open.
        drop(running);
        drop(reader);
        return Err(RunError::Deadline);
    };
    let _ = reader.join();
    let Ok(mut stdout) = read else {
        return Ok(Ran {
            stdout: Vec::new(),
            overran: false,
            success: false,
        });
    };
    if stdout.len() > cap {
        stdout.truncate(cap);
        return Ok(Ran {
            stdout,
            overran: true,
            success: false,
        });
    }
    let success = running.wait_until(deadline)?;
    Ok(Ran {
        stdout,
        overran: false,
        success,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shell script trusted as a tool, in a private directory.
    pub(crate) fn script(directory: &std::path::Path, name: &str, body: &str) -> Tool {
        let source = directory.join(format!("{name}.sh"));
        std::fs::write(&source, format!("#!/bin/sh\n{body}\n")).unwrap();
        // Copied so that no child another test forks meanwhile holds it
        // open for writing, which fails running it (ETXTBSY) on Linux.
        let path = directory.join(name);
        kettle_test_support::copy_executable_fixture(&source, &path).unwrap();
        crate::tools::trust(&path).unwrap()
    }

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    /// Output comes back to the cap, its input is the file given (or
    /// nothing), its environment is empty and it runs in `/`; one byte past
    /// the cap is an overrun, a failing exit is no success, and a tool
    /// replaced after it was trusted is not run.
    #[test]
    fn a_run_reads_bounded_output_from_a_clean_start() {
        let directory = kettle_test_support::private_tempdir("kettle-native-run-");
        let echo = script(
            directory.path(),
            "echo",
            r#"read line; printf '%s|%s|%s|%s' "$line" "$PWD" "${HOME-none}" "$1""#,
        );
        let input = directory.path().join("input");
        std::fs::write(&input, b"from the file\n").unwrap();
        let ran = run(
            &echo,
            &["argument"],
            Some(File::open(&input).unwrap()),
            64,
            later(),
        )
        .unwrap();
        assert_eq!(
            (
                String::from_utf8(ran.stdout).unwrap(),
                ran.overran,
                ran.success
            ),
            ("from the file|/|none|argument".to_string(), false, true)
        );
        let ran = run(&echo, &["x"], None, 64, later()).unwrap();
        assert_eq!(String::from_utf8(ran.stdout).unwrap(), "|/|none|x");

        let ten = script(directory.path(), "ten", "printf 0123456789");
        let ran = run(&ten, &[] as &[&str], None, 10, later()).unwrap();
        assert_eq!(
            (ran.stdout.len(), ran.overran, ran.success),
            (10, false, true)
        );
        let ran = run(&ten, &[] as &[&str], None, 9, later()).unwrap();
        assert_eq!(
            (ran.stdout.len(), ran.overran, ran.success),
            (9, true, false)
        );

        let fails = script(directory.path(), "fails", "printf partial; exit 3");
        let ran = run(&fails, &[] as &[&str], None, 64, later()).unwrap();
        assert_eq!((ran.stdout, ran.success), (b"partial".to_vec(), false));

        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(directory.path().join("ten"), "#!/bin/sh\nprintf other\n").unwrap();
        assert_eq!(
            run(&ten, &[] as &[&str], None, 64, later()).unwrap_err(),
            RunError::Unavailable
        );
    }

    /// A tool that never finishes, writing or not, is killed at the
    /// deadline and nothing of it is left running.
    #[test]
    fn a_run_ends_at_its_deadline() {
        let directory = kettle_test_support::private_tempdir("kettle-native-run-");
        // `exec` replaces the shell without forking, which containment allows.
        let silent = script(directory.path(), "silent", "exec /bin/sleep 30");
        let started = Instant::now();
        let deadline = started + Duration::from_millis(300);
        assert_eq!(
            run(&silent, &[] as &[&str], None, 64, deadline).unwrap_err(),
            RunError::Deadline
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        let writer = script(
            directory.path(),
            "writer",
            "printf done; exec 1>&-; exec /bin/sleep 30",
        );
        let started = Instant::now();
        assert_eq!(
            run(
                &writer,
                &[] as &[&str],
                None,
                64,
                started + Duration::from_millis(300)
            )
            .unwrap_err(),
            RunError::Deadline,
            "output closed but still running"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
