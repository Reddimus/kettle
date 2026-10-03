//! The worker's side of the protocol: Hello in, Ready out, one Job in, one
//! reply out, then exit. A watchdog bounds each phase.

use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use kettle_media::wire::{Direction, Frame, WireError, read_frame, write_frame};
use kettle_media::{
    BuildId, Failure, FailureCode, HandshakeOutcome, Job, Ready, Rendered, ValidationError,
    check_ready,
};

/// Exit codes. 4, 8 and 9 mean what they mean for the video-preview worker.
pub(crate) const EXIT_TIMEOUT: i32 = 4;
pub(crate) const EXIT_SETUP: i32 = 8;
pub(crate) const EXIT_SKEW: i32 = 9;
/// The parent broke the protocol, or a pipe failed.
pub(crate) const EXIT_PROTOCOL: i32 = 2;

/// From start until Ready is written: the parent's Hello and this worker's
/// setup.
pub(crate) const READY_DEADLINE: Duration = Duration::from_secs(5);
/// From Ready until the job has arrived.
const JOB_ARRIVAL_DEADLINE: Duration = Duration::from_secs(5);
/// From the job's arrival until its reply is written, blocked output included.
/// The client's own deadline is shorter for most kinds.
const JOB_DEADLINE: Duration = Duration::from_secs(3);

/// The panic report: one fixed line, never the message, its payload, a
/// location or a backtrace, and no crash file.
pub(crate) fn report_panic() {
    let _ = std::io::stderr().write_all(b"media worker panic\n");
}

/// Ends the process with [`EXIT_TIMEOUT`] when the current phase's deadline
/// passes, whatever the main thread is blocked on. It needs nothing from the
/// parent, so a parent that stalls or dies cannot keep the worker alive.
pub(crate) struct Watchdog {
    deadline: Arc<(Mutex<Instant>, Condvar)>,
}

impl Watchdog {
    /// Start with `first` for the first phase. Fails if its thread cannot
    /// start, and the worker then refuses to run.
    pub(crate) fn start(first: Duration) -> std::io::Result<Self> {
        let deadline = Arc::new((Mutex::new(Instant::now() + first), Condvar::new()));
        let watched = Arc::clone(&deadline);
        std::thread::Builder::new()
            .name("kettle-media-watchdog".into())
            .spawn(move || {
                let (at, changed) = &*watched;
                let mut at = at.lock().unwrap_or_else(PoisonError::into_inner);
                loop {
                    let now = Instant::now();
                    if now >= *at {
                        std::process::exit(EXIT_TIMEOUT);
                    }
                    let wait = *at - now;
                    at = changed
                        .wait_timeout(at, wait)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            })?;
        Ok(Self { deadline })
    }

    /// Give the next phase `budget` from now.
    fn arm(&self, budget: Duration) {
        let (at, changed) = &*self.deadline;
        *at.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now() + budget;
        changed.notify_one();
    }
}

/// Serve one job read from `input`, answering on `output`, and return the
/// exit code.
pub(crate) fn serve(input: &mut impl Read, output: &mut impl Write, watchdog: &Watchdog) -> i32 {
    let Ok(own) = BuildId::from_embedded(env!("CARGO_PKG_VERSION"), env!("KETTLE_SOURCE_HASH"))
    else {
        return EXIT_SETUP;
    };
    let hello = match read_frame(input, Direction::ParentToWorker) {
        Ok(Some(Frame::Hello(hello))) => hello,
        // The parent left before saying Hello.
        Ok(None) => return 0,
        other => return refuse(output, other),
    };
    let ready = Ready { build_id: own };
    if check_ready(&hello, &ready) != HandshakeOutcome::Compatible {
        return reply(output, FailureCode::RestartRequired, EXIT_SKEW);
    }
    // Setup is complete: this build has no fonts or renderer to prepare.
    if write_frame(output, &Frame::Ready(ready), Direction::WorkerToParent).is_err() {
        return EXIT_PROTOCOL;
    }
    watchdog.arm(JOB_ARRIVAL_DEADLINE);
    let job = match read_frame(input, Direction::ParentToWorker) {
        Ok(Some(Frame::Job(job))) => job,
        Ok(None) => return 0,
        other => return refuse(output, other),
    };
    watchdog.arm(JOB_DEADLINE);
    match answer(&job) {
        Ok(rendered) => {
            let frame = Frame::Rendered(rendered);
            match write_frame(output, &frame, Direction::WorkerToParent) {
                Ok(()) => 0,
                Err(_) => EXIT_PROTOCOL,
            }
        }
        Err(code) => reply(output, code, 0),
    }
}

/// Render the job, or say why not.
fn answer(job: &Job) -> Result<Rendered, FailureCode> {
    #[cfg(feature = "test-faults")]
    faults::inject(job);
    kettle_media_render::render(job)
}

/// Answer a frame other than the one expected, or one that could not be read.
fn refuse(output: &mut impl Write, read: Result<Option<Frame>, WireError>) -> i32 {
    match read {
        Err(WireError::RestartRequired) => reply(output, FailureCode::RestartRequired, EXIT_SKEW),
        Err(WireError::Validation(ValidationError::TooLarge)) => {
            reply(output, FailureCode::TooLarge, EXIT_PROTOCOL)
        }
        // A frame cut short, by a parent that may still be reading, is
        // answered like any other bad frame.
        _ => reply(output, FailureCode::BadParams, EXIT_PROTOCOL),
    }
}

fn reply(output: &mut impl Write, code: FailureCode, exit: i32) -> i32 {
    let failure = Frame::Failure(Failure { code });
    match write_frame(output, &failure, Direction::WorkerToParent) {
        Ok(()) => exit,
        Err(_) => EXIT_PROTOCOL,
    }
}

/// Test fixture only (feature `test-faults`): a raster job whose bytes start
/// with [`faults::PANIC`] panics with the rest as its message, so a test can
/// show the message never reaches stderr.
#[cfg(feature = "test-faults")]
mod faults {
    use kettle_media::{Job, Source};

    const PANIC: &[u8] = b"kettle-media-worker-test-panic:";

    pub(super) fn inject(job: &Job) {
        if let Source::Bytes(bytes) = &job.source
            && let Some(message) = bytes.strip_prefix(PANIC)
        {
            panic!("{}", String::from_utf8_lossy(message));
        }
    }
}
