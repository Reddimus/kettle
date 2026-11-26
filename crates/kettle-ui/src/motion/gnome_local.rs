//! GNOME's own animation switch, `org.gnome.desktop.interface
//! enable-animations`, read with `gsettings` in a GNOME session whose
//! Settings portal gives no answer: there is none, or it exports neither
//! `reduced-motion` nor GNOME's key. Each read runs on the watch's own
//! thread, is bounded in time and output, and is killed and reaped when it
//! overruns or the watch is dropped; dropping the watch joins its thread.

use std::ffi::OsStr;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::Reading;

/// How long one read may take before it is killed.
const TIMEOUT: Duration = Duration::from_secs(2);
/// How often the switch is read again, so a change applies live.
const REFRESH: Duration = Duration::from_secs(30);
/// More output than `true` or `false` is not an answer.
const MAX_OUTPUT: usize = 64;
/// How long the read waits between looks at the child.
const POLL: Duration = Duration::from_millis(5);

const PENDING: u8 = 0;
const UNAVAILABLE: u8 = 1;
const OFF: u8 = 2;
const ON: u8 = 3;

/// Whether `XDG_CURRENT_DESKTOP` names GNOME or a desktop that keeps its
/// animation switch in GNOME's schema (GNOME Classic and Flashback, Ubuntu's
/// `ubuntu:GNOME`, Unity).
pub(crate) fn gnome_like(desktop: Option<&OsStr>) -> bool {
    desktop.and_then(OsStr::to_str).is_some_and(|desktop| {
        desktop.split(':').any(|name| {
            let name = name.trim().to_ascii_lowercase();
            name.starts_with("gnome") || name == "unity"
        })
    })
}

/// The first executable `gsettings` file in the absolute entries of `path`
/// (`PATH`); relative entries are skipped.
pub(crate) fn find_gsettings(path: Option<&OsStr>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("gsettings"))
        .find(|file| {
            std::fs::metadata(file)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

/// The `gsettings` command that reads the switch.
pub(crate) fn gsettings_get(program: PathBuf) -> Command {
    let mut command = Command::new(program);
    command.args(["get", "org.gnome.desktop.interface", "enable-animations"]);
    command
}

fn parse(output: &[u8]) -> Option<bool> {
    match std::str::from_utf8(output).ok()?.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn stopped(stop: &mpsc::Receiver<()>) -> bool {
    !matches!(stop.try_recv(), Err(mpsc::TryRecvError::Empty))
}

fn ready(pipe: &std::process::ChildStdout) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd as _;
    let mut poll = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll is writable storage for one pollfd, and pipe keeps its
    // descriptor open during the call.
    let result = unsafe { libc::poll(&mut poll, 1, 0) };
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(result != 0)
    }
}

/// Run `command` and read the switch from its output: `Some` for a `true`
/// or `false` answer from a successful run. `None` for anything else, and
/// once `timeout` passes or `stop` says to; the child is then killed and
/// reaped. Nothing here blocks on the child.
pub(crate) fn read(
    command: &mut Command,
    timeout: Duration,
    stop: &mpsc::Receiver<()>,
) -> Option<bool> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let answer = child.stdout.take().and_then(|mut pipe| {
        let mut buffer = [0; MAX_OUTPUT + 1];
        let mut status = None;
        loop {
            if Instant::now() >= deadline || stopped(stop) {
                return None;
            }
            if ready(&pipe).ok()? {
                let count = pipe.read(&mut buffer).ok()?;
                if count != 0 {
                    if output.len() + count > MAX_OUTPUT {
                        return None;
                    }
                    output.extend_from_slice(&buffer[..count]);
                    continue;
                }
            }
            if let Some(status) = status {
                return Some(status);
            }
            status = child.try_wait().ok()?;
            if status.is_none() {
                let rest = deadline.saturating_duration_since(Instant::now());
                if !matches!(
                    stop.recv_timeout(POLL.min(rest)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    return None;
                }
            }
        }
    });
    match answer {
        Some(status) if status.success() => parse(&output),
        Some(_) => None,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// GNOME's switch as its own thread last read it, again every `REFRESH`.
/// A failed read before any answer leaves it unavailable; a failed read
/// after one keeps that answer.
pub(crate) struct LocalWatch {
    state: Arc<AtomicU8>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LocalWatch {
    /// Nothing to read: not a GNOME session.
    pub(crate) fn unavailable() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(UNAVAILABLE)),
            stop: None,
            thread: None,
        }
    }

    /// Read the switch on a thread with the command `command` builds there,
    /// so finding `gsettings` stays off the caller's thread too; with no
    /// command, or no thread, the switch is unavailable. `wake` runs when
    /// the reading changes.
    pub(crate) fn start(
        command: impl FnOnce() -> Option<Command> + Send + 'static,
        wake: kettle_core::Waker,
    ) -> Self {
        Self::start_every(command, wake, REFRESH)
    }

    fn start_every(
        command: impl FnOnce() -> Option<Command> + Send + 'static,
        wake: kettle_core::Waker,
        refresh: Duration,
    ) -> Self {
        let state = Arc::new(AtomicU8::new(PENDING));
        let worker_state = state.clone();
        let (stop, stopped) = mpsc::channel();
        let publish = move |value: u8| {
            if worker_state.swap(value, Ordering::AcqRel) != value {
                wake();
            }
        };
        let thread = std::thread::Builder::new()
            .name("kettle-gnome-motion".into())
            .spawn(move || {
                let Some(mut command) = command() else {
                    publish(UNAVAILABLE);
                    return;
                };
                let mut answered = false;
                loop {
                    match read(&mut command, TIMEOUT, &stopped) {
                        Some(enabled) => {
                            answered = true;
                            publish(if enabled { ON } else { OFF });
                        }
                        None if !answered => publish(UNAVAILABLE),
                        None => {}
                    }
                    if !matches!(
                        stopped.recv_timeout(refresh),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ) {
                        return;
                    }
                }
            });
        match thread {
            Ok(thread) => Self {
                state,
                stop: Some(stop),
                thread: Some(thread),
            },
            Err(error) => {
                log::debug!("GNOME animation setting reader unavailable: {error}");
                Self::unavailable()
            }
        }
    }

    pub(crate) fn reading(&self) -> Reading<bool> {
        match self.state.load(Ordering::Acquire) {
            PENDING => Reading::Pending,
            OFF => Reading::Value(false),
            ON => Reading::Value(true),
            _ => Reading::Unavailable,
        }
    }
}

impl Drop for LocalWatch {
    fn drop(&mut self) {
        // Closing the channel stops a read in progress (its child is killed
        // and reaped) or the wait for the next one.
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
#[path = "gnome_local_tests.rs"]
mod tests;
