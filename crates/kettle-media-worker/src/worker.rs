//! The worker's side of the protocol: Hello in, Ready out, one Job in, one
//! reply out, then exit. A watchdog bounds each phase.

use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use kettle_media::wire::{Direction, Frame, WireError, read_frame, write_frame};
use kettle_media::{
    BuildId, Failure, FailureCode, HandshakeOutcome, Job, JobKind, MediaKind, Ready, Rendered,
    ValidationError, check_ready,
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
// From the job's arrival until its reply is written, blocked output
// included, the job has its kind's `JobKind::render_deadline`, the one the
// client waits too. An Auto job narrows it once its kind is known, still
// counted from its arrival.

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
    /// Whether its thread confined itself to nothing. A job counts as
    /// confined only if it did: a signal handler could otherwise run there.
    confined: bool,
}

impl Watchdog {
    /// Start with `first` for the first phase. Fails if its thread cannot
    /// start, and the worker then refuses to run.
    pub(crate) fn start(first: Duration) -> std::io::Result<Self> {
        let deadline = Arc::new((Mutex::new(Instant::now() + first), Condvar::new()));
        let watched = Arc::clone(&deadline);
        let (told, hear) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("kettle-media-watchdog".into())
            .spawn(move || {
                // Started before the job's sandbox exists, it confines itself
                // to nothing: it only waits and ends the process.
                let confined = kettle_media_native::sandbox::confine_thread_to_nothing().is_ok();
                let _ = told.send(confined);
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
        let confined = hear.recv().unwrap_or(false);
        #[cfg(feature = "test-faults")]
        let confined = confined && !faults::watchdog_unconfined();
        Ok(Self { deadline, confined })
    }

    /// Give the next phase `budget` from now.
    fn arm(&self, budget: Duration) {
        let (at, changed) = &*self.deadline;
        *at.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now() + budget;
        changed.notify_one();
    }

    /// End the current phase by `until` at the latest; never extends it.
    fn narrow(&self, until: Instant) {
        let (at, changed) = &*self.deadline;
        let mut at = at.lock().unwrap_or_else(PoisonError::into_inner);
        if until < *at {
            *at = until;
            changed.notify_one();
        }
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
    // Setup completes before Ready: the renderer's shared fonts are built
    // here, outside the job's deadline.
    kettle_media_render::prepare();
    if write_frame(output, &Frame::Ready(ready), Direction::WorkerToParent).is_err() {
        return EXIT_PROTOCOL;
    }
    watchdog.arm(JOB_ARRIVAL_DEADLINE);
    let job = match read_frame(input, Direction::ParentToWorker) {
        Ok(Some(Frame::Job(job))) => job,
        Ok(None) => return 0,
        other => return refuse(output, other),
    };
    let arrived = Instant::now();
    watchdog.arm(job.kind.render_deadline());
    let classified = |kind: MediaKind| {
        if job.kind == JobKind::Auto {
            watchdog.narrow(arrived + kind.render_deadline());
        }
    };
    match answer(&job, classified, watchdog.confined) {
        Ok((kind, rendered)) => {
            // Only an Auto job's reply says what it was; an explicit kind's
            // reply stays the plain frame, so it cannot claim another kind.
            let frame = if job.kind == JobKind::Auto {
                Frame::DetectedRendered { kind, rendered }
            } else {
                Frame::Rendered(rendered)
            };
            match write_frame(output, &frame, Direction::WorkerToParent) {
                Ok(()) => 0,
                Err(_) => EXIT_PROTOCOL,
            }
        }
        Err(code) => reply(output, code, 0),
    }
}

/// Render the job and say what it was, or say why not. `classified` hears
/// the kind before it is rendered.
fn answer(
    job: &Job,
    mut classified: impl FnMut(MediaKind),
    watchdog_confined: bool,
) -> Result<(MediaKind, Rendered), FailureCode> {
    #[cfg(feature = "test-faults")]
    faults::inject(job);
    // A video's stills need a decoder: Apple's own for MP4 and QuickTime on
    // macOS, then the external one the parent named, trusted again here.
    // With neither they are `BackendUnavailable`.
    let decoders = matches!(
        job.kind,
        kettle_media::JobKind::VideoStills(_) | kettle_media::JobKind::Auto
    )
    .then(kettle_media_native::Decoders::configured)
    .filter(|decoders| !decoders.is_empty());
    #[cfg(not(feature = "test-faults"))]
    let confined = confine(job, decoders.as_ref());
    #[cfg(feature = "test-faults")]
    let confined = if faults::unconfined(job) {
        Err(kettle_media_native::sandbox::SandboxError::Unavailable)
    } else {
        confine(job, decoders.as_ref())
    };
    // Every thread must be confined: the watchdog confined itself earlier.
    let confined = confined.and(if watchdog_confined {
        Ok(())
    } else {
        Err(kettle_media_native::sandbox::SandboxError::Failed)
    });
    let refusing = Refusing(FailureCode::SandboxUnavailable);
    // A video is never decoded unconfined. Raster, SVG and Mermaid, which
    // only Kettle's own code parses, still render in this bounded process.
    let decoder: Option<&dyn kettle_media::video::VideoDecoder> = match (&decoders, confined) {
        (Some(decoders), Ok(())) => Some(decoders),
        (Some(_), Err(_)) => Some(&refusing),
        (None, _) => None,
    };
    kettle_media_render::render_with_decoder(
        job,
        |kind| {
            classified(kind);
            #[cfg(feature = "test-faults")]
            faults::after_classification(job);
        },
        decoder,
    )
}

/// Confine this process to what `job` needs, before any byte of its media or
/// fonts is read: the files it names, opened and checked as rendering opens
/// them, and, for a video, the external decoder's programs and the trees
/// they load from.
fn confine(
    job: &Job,
    decoders: Option<&kettle_media_native::Decoders>,
) -> Result<(), kettle_media_native::sandbox::SandboxError> {
    let files = kettle_media_render::source::hold_inputs(job);
    let (programs, trees) = decoders
        .map(|decoders| decoders.sandbox_needs())
        .unwrap_or_default();
    kettle_media_native::sandbox::confine(&kettle_media_native::sandbox::Policy {
        files: files.iter().collect(),
        programs,
        trees,
    })
}

/// A decoder that decodes nothing, answering every video with `code`: what a
/// job gets in place of the real ones when the sandbox could not be applied.
struct Refusing(FailureCode);

impl kettle_media::video::VideoDecoder for Refusing {
    fn stills(
        &self,
        _input: kettle_media::video::VideoInput<'_>,
        _plan: &mut dyn FnMut(
            &kettle_media::VideoInfo,
        ) -> Result<kettle_media::video::StillsPlan, FailureCode>,
        _deadline: Instant,
    ) -> Result<kettle_media::video::DecodedStills, FailureCode> {
        Err(self.0)
    }
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
/// show the message never reaches stderr; and a job whose theme accent is
/// `[b'K', b'T', before, after]` pauses `before` tenths of a second before it
/// is classified and `after` tenths once it is, so a test can show which
/// deadline governs each stretch.
#[cfg(feature = "test-faults")]
mod faults {
    use std::time::Duration;

    use kettle_media::{Job, Source};

    const PANIC: &[u8] = b"kettle-media-worker-test-panic:";
    const PAUSE: [u8; 2] = *b"KT";
    /// Try, from inside the job, to read the file beside its source.
    const PEEK: [u8; 2] = *b"KS";
    /// Act as a system with no sandbox.
    const UNCONFINED: [u8; 2] = *b"KN";

    pub(super) fn inject(job: &Job) {
        if let Source::Bytes(bytes) = &job.source
            && let Some(message) = bytes.strip_prefix(PANIC)
        {
            panic!("{}", String::from_utf8_lossy(message));
        }
        pause(job, 2);
    }

    pub(super) fn after_classification(job: &Job) {
        pause(job, 3);
        peek(job);
    }

    pub(super) fn unconfined(job: &Job) -> bool {
        job.theme.accent[..2] == UNCONFINED
    }

    /// Act as a worker whose watchdog could not confine itself.
    pub(super) fn watchdog_unconfined() -> bool {
        std::env::var_os("KETTLE_MEDIA_TEST_UNCONFINED_WATCHDOG").is_some()
    }

    /// Reading the source's neighbor `secret` succeeding is a panic: the job
    /// then fails where a confined one renders.
    fn peek(job: &Job) {
        use std::os::unix::ffi::OsStrExt as _;
        if job.theme.accent[..2] != PEEK {
            return;
        }
        if let Source::Path { path, .. } = &job.source {
            let source = std::path::Path::new(std::ffi::OsStr::from_bytes(path.as_bytes()));
            if std::fs::read(source.with_file_name("secret")).is_ok() {
                panic!("the job read a file it does not hold");
            }
        }
    }

    fn pause(job: &Job, which: usize) {
        let accent = job.theme.accent;
        if accent[..2] == PAUSE {
            std::thread::sleep(Duration::from_millis(100) * u32::from(accent[which]));
        }
    }
}
