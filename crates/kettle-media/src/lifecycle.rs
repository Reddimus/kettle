//! One render in a fresh worker: start it, send Hello, take Ready within the
//! startup deadline, send the job, take one reply within the job's deadline,
//! then stop the worker and its process group. Two threads move the bytes, so
//! no blocked read or write can hold a deadline; the caller's thread watches
//! the clock. A reply counts only once the worker has exited 0 by itself and
//! nothing followed the reply on its stdout.

use std::io::{Read, Write};

use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::client::{Shared, SpawnedWorker, UnavailableCause, WorkerExit, WorkerProcess};
use crate::wire::{self, Direction, Frame, MAX_READY_FRAME_BYTES, WireError};
use crate::{BuildId, FailureCode, HandshakeOutcome, Hello, Job, JobKind, Rendered, check_ready};

/// How often the caller's thread looks at the worker while it waits for the
/// reader: an exit can come with no frame, when something the worker started
/// still holds its stdout.
const TICK: Duration = Duration::from_millis(25);

/// The signal a kill sends. A worker reaped with any other status ended by
/// itself before the kill landed.
const SIGKILL: i32 = 9;

/// The time each stage may take.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budgets {
    /// From start (the spawn included) until Ready.
    pub(crate) ready: Duration,
    /// From the job's write until its reply; `None` takes each kind's own.
    pub(crate) render: Option<Duration>,
    /// How long a stopping worker gets to exit, first by itself and then
    /// again after it is killed; and how long the reader gets to pass on
    /// what the pipe held once the worker has exited.
    pub(crate) cleanup: Duration,
}

impl Budgets {
    pub(crate) const PRODUCTION: Self = Self {
        ready: Duration::from_secs(5),
        render: None,
        cleanup: Duration::from_millis(250),
    };

    fn render_deadline(&self, kind: JobKind) -> Duration {
        self.render.unwrap_or(match kind {
            JobKind::Raster => Duration::from_secs(2),
            _ => Duration::from_secs(3),
        })
    }
}

/// No Ready before the startup deadline, and the worker was killed and
/// reaped: a cold start may be retried once.
pub(crate) struct NeverReady;

enum Event {
    /// The first frame (Ready, or a refusal at startup) and, for a refusal,
    /// whether end of file followed it with nothing between.
    First(Result<Option<Frame>, WireError>, bool),
    /// The reply, and whether end of file followed it with nothing between.
    Reply(Result<Option<Frame>, WireError>, bool),
}

/// Why waiting for the reader brought no event.
enum Wait {
    /// The deadline passed, or the worker exited and the reader had nothing
    /// more within the cleanup budget.
    Timeout,
    /// The reader thread is gone without a word.
    ReaderGone,
}

/// How a stopped worker ended.
enum Stopped {
    /// It exited by itself, before any kill.
    Exited(WorkerExit),
    /// It was killed, and reaped.
    Killed,
    /// It was killed and would not exit: it is kept for later reaping and
    /// counted.
    Stuck,
}

pub(crate) fn attempt(
    shared: &Arc<Shared>,
    build_id: &BuildId,
    job: &Job,
    budgets: Budgets,
) -> Result<Result<Rendered, FailureCode>, NeverReady> {
    let hello = Hello {
        build_id: build_id.clone(),
    };
    let (Ok(hello_bytes), Ok(job_bytes)) = (
        wire::encode(&Frame::Hello(hello.clone()), Direction::ParentToWorker),
        wire::encode(&Frame::Job(job.clone()), Direction::ParentToWorker),
    ) else {
        return Ok(Err(FailureCode::BadParams));
    };
    let ready_until = Instant::now() + budgets.ready;
    let SpawnedWorker {
        process,
        stdin,
        stdout,
    } = match spawn_within(shared, ready_until, budgets) {
        Ok(spawned) => spawned,
        Err(code) => return Ok(Err(code)),
    };
    let mut worker = Running::new(process, shared, budgets);
    let (events, received) = mpsc::channel();
    let (go, job_ready) = mpsc::channel();
    if spawn_reader(stdout, events).is_err()
        || spawn_writer(Arc::clone(shared), stdin, hello_bytes, job_ready).is_err()
    {
        worker.kill();
        return Ok(Err(FailureCode::WorkerUnavailable));
    }

    match worker.next(&received, ready_until) {
        Err(Wait::Timeout) => {
            // An exit with nothing said, even one just before the kill, is
            // not a cold start: it is not retried.
            if let Some(exit) = worker.own_exit() {
                return Ok(Err(exit_failure(exit)));
            }
            return match worker.kill() {
                Stopped::Exited(exit) => Ok(Err(exit_failure(exit))),
                Stopped::Stuck => Ok(Err(FailureCode::RenderTimeout)),
                Stopped::Killed => Err(NeverReady),
            };
        }
        Ok(Event::First(Ok(Some(Frame::Ready(ready))), _)) => {
            if check_ready(&hello, &ready) != HandshakeOutcome::Compatible {
                worker.kill();
                return Ok(Err(FailureCode::RestartRequired));
            }
        }
        Ok(Event::First(Ok(Some(Frame::Failure(failure))), true)) => {
            // Before a job, only a handshake refusal means anything, and only
            // from a worker that then exits rather than crashes.
            let code = match failure.code {
                FailureCode::RestartRequired | FailureCode::UnknownMethod => failure.code,
                _ => FailureCode::WorkerUnavailable,
            };
            return Ok(Err(match worker.stop() {
                Stopped::Exited(WorkerExit::Code(_)) => code,
                Stopped::Exited(exit) => exit_failure(exit),
                Stopped::Killed | Stopped::Stuck => FailureCode::WorkerUnavailable,
            }));
        }
        // Another frame, a refusal with more after it, one that did not
        // decode, end of file, or a reader that is gone: the exit says why.
        Ok(_) | Err(Wait::ReaderGone) => return Ok(Err(worker.stop_failure())),
    }

    // The job's deadline starts before the job is written.
    let reply_until = Instant::now() + budgets.render_deadline(job.kind);
    if go.send(job_bytes).is_err() {
        return Ok(Err(worker.stop_failure()));
    }
    Ok(match worker.next(&received, reply_until) {
        Err(Wait::Timeout) => match worker.own_exit() {
            Some(exit) => Err(exit_failure(exit)),
            None => match worker.kill() {
                // It ended by itself just as the deadline passed: its exit
                // says why.
                Stopped::Exited(exit) => Err(exit_failure(exit)),
                Stopped::Killed | Stopped::Stuck => Err(FailureCode::RenderTimeout),
            },
        },
        Ok(Event::Reply(Ok(Some(Frame::Rendered(rendered))), true)) => match worker.stop() {
            Stopped::Exited(WorkerExit::Code(0)) if rendered.validate().is_ok() => Ok(rendered),
            Stopped::Exited(WorkerExit::Code(0)) => Err(FailureCode::WorkerUnavailable),
            Stopped::Exited(exit) => Err(exit_failure(exit)),
            Stopped::Killed | Stopped::Stuck => Err(FailureCode::WorkerUnavailable),
        },
        Ok(Event::Reply(Ok(Some(Frame::Failure(failure))), true)) => match worker.stop() {
            Stopped::Exited(WorkerExit::Code(0)) => Err(refusal(failure.code)),
            Stopped::Exited(exit) => Err(exit_failure(exit)),
            Stopped::Killed | Stopped::Stuck => Err(FailureCode::WorkerUnavailable),
        },
        // A second Ready, more after the reply, a reply cut short or that
        // did not decode, or end of file.
        Ok(_) | Err(Wait::ReaderGone) => Err(worker.stop_failure()),
    })
}

enum SpawnSlot {
    Waiting,
    Done(Result<SpawnedWorker, FailureCode>),
    /// The caller stopped waiting: whatever starts now is the helper's to
    /// kill.
    Abandoned,
}

/// Check and start the worker on a helper thread, so a check or a start that
/// blocks (an executable on a stalled network filesystem) cannot hold the
/// caller past `until`. A worker that starts after that is killed by the
/// helper.
fn spawn_within(
    shared: &Arc<Shared>,
    until: Instant,
    budgets: Budgets,
) -> Result<SpawnedWorker, FailureCode> {
    // A start that hung once would hang again: while one is outstanding, no
    // other begins, so hung starts cannot pile up.
    if shared.late_spawns.load(Ordering::Acquire) > 0 {
        return Err(FailureCode::WorkerUnavailable);
    }
    let slot = Arc::new((Mutex::new(SpawnSlot::Waiting), Condvar::new()));
    let helper_slot = Arc::clone(&slot);
    let helper_shared = Arc::clone(shared);
    let started = std::thread::Builder::new()
        .name("kettle-media-spawn".into())
        .spawn(move || {
            let spawned = match helper_shared.checked_path() {
                Ok(path) => helper_shared
                    .platform()
                    .spawn(path)
                    .map_err(|_| FailureCode::WorkerUnavailable),
                Err(UnavailableCause::UnsupportedPlatform) => Err(FailureCode::UnsupportedPlatform),
                Err(_) => Err(FailureCode::WorkerUnavailable),
            };
            let (state, changed) = &*helper_slot;
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(*state, SpawnSlot::Abandoned) {
                drop(state);
                if let Ok(late) = spawned {
                    Running::new(late.process, &helper_shared, budgets).kill();
                }
                helper_shared.late_spawns.fetch_sub(1, Ordering::AcqRel);
                return;
            }
            *state = SpawnSlot::Done(spawned);
            changed.notify_one();
        });
    if started.is_err() {
        return Err(FailureCode::WorkerUnavailable);
    }
    let (state, changed) = &*slot;
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        match std::mem::replace(&mut *state, SpawnSlot::Waiting) {
            SpawnSlot::Done(result) => return result,
            SpawnSlot::Waiting | SpawnSlot::Abandoned => {}
        }
        let now = Instant::now();
        if now >= until {
            *state = SpawnSlot::Abandoned;
            shared.late_spawns.fetch_add(1, Ordering::AcqRel);
            return Err(FailureCode::RenderTimeout);
        }
        state = changed
            .wait_timeout(state, until - now)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

/// A worker's own failure code, kept only when a worker can mean it: one
/// that names GUI or agent state is not the worker's to report.
fn refusal(code: FailureCode) -> FailureCode {
    use FailureCode::*;
    match code {
        BadParams | TooLarge | FileNotFound | FilePermission | FileNotRegular | FileTooLarge
        | Changed | IndexOutOfRange | UnsupportedMedia | UnsupportedPlatform | RenderTimeout
        | RenderResource | RenderParse | RestartRequired | UnknownMethod | WorkerUnavailable => {
            code
        }
        _ => WorkerUnavailable,
    }
}

/// What an exit without a usable reply says.
pub(crate) fn exit_failure(exit: WorkerExit) -> FailureCode {
    match exit {
        // The worker's own watchdog.
        WorkerExit::Code(4) => FailureCode::RenderTimeout,
        // Another build answered.
        WorkerExit::Code(9) => FailureCode::RestartRequired,
        // A CPU, file-size or memory limit, or a crash.
        WorkerExit::Signal(_) => FailureCode::RenderResource,
        WorkerExit::Code(_) | WorkerExit::Lost => FailureCode::WorkerUnavailable,
    }
}

struct Running<'a> {
    process: Option<Box<dyn WorkerProcess>>,
    /// The worker's own exit, seen before any kill, and when.
    exited: Option<(WorkerExit, Instant)>,
    shared: &'a Arc<Shared>,
    budgets: Budgets,
}

impl<'a> Running<'a> {
    fn new(process: Box<dyn WorkerProcess>, shared: &'a Arc<Shared>, budgets: Budgets) -> Self {
        Self {
            process: Some(process),
            exited: None,
            shared,
            budgets,
        }
    }

    fn own_exit(&self) -> Option<WorkerExit> {
        self.exited.map(|(exit, _)| exit)
    }

    /// The next thing the reader says before `until`, looking at the worker
    /// meanwhile. Once it has exited, the reader gets the cleanup budget to
    /// pass on what the pipe still held.
    fn next(&mut self, received: &Receiver<Event>, until: Instant) -> Result<Event, Wait> {
        loop {
            let limit = match self.exited {
                Some((_, seen)) => until.min(seen + self.budgets.cleanup),
                None => until,
            };
            let now = Instant::now();
            if now >= limit {
                return Err(Wait::Timeout);
            }
            match received.recv_timeout((limit - now).min(TICK)) {
                Ok(event) => return Ok(event),
                Err(RecvTimeoutError::Disconnected) => return Err(Wait::ReaderGone),
                Err(RecvTimeoutError::Timeout) => self.look(),
            }
        }
    }

    /// Note the worker's exit if it has exited by itself (the platform kills
    /// its process group before reaping it, which also frees its pipes).
    fn look(&mut self) {
        if self.exited.is_some() {
            return;
        }
        if let Some(process) = self.process.as_mut()
            && let Ok(Some(exit)) = process.try_wait()
        {
            self.process = None;
            self.exited = Some((exit, Instant::now()));
        }
    }

    /// Let the worker exit by itself within the cleanup budget, as it does
    /// after a reply or a refusal, and kill it if it does not.
    fn stop(&mut self) -> Stopped {
        if let Some(exit) = self.own_exit() {
            return Stopped::Exited(exit);
        }
        let Some(process) = self.process.as_mut() else {
            return Stopped::Killed;
        };
        if let Some(exit) = poll(process.as_mut(), self.budgets.cleanup) {
            self.process = None;
            self.exited = Some((exit, Instant::now()));
            return Stopped::Exited(exit);
        }
        self.kill()
    }

    /// [`Self::stop`] after something went wrong: the failure its exit names,
    /// or `WorkerUnavailable` when it had to be killed.
    fn stop_failure(&mut self) -> FailureCode {
        match self.stop() {
            Stopped::Exited(exit) => exit_failure(exit),
            Stopped::Killed | Stopped::Stuck => FailureCode::WorkerUnavailable,
        }
    }

    /// Kill the worker's process group now and reap it. One that will not
    /// exit within the cleanup budget is kept for later reaping and counted;
    /// enough of those turn media off.
    fn kill(&mut self) -> Stopped {
        let Some(mut process) = self.process.take() else {
            return Stopped::Killed;
        };
        // It may have exited by itself a moment ago; that exit is its own.
        if let Ok(Some(exit)) = process.try_wait() {
            self.exited = Some((exit, Instant::now()));
            return Stopped::Exited(exit);
        }
        process.kill();
        match poll(process.as_mut(), self.budgets.cleanup) {
            Some(WorkerExit::Signal(SIGKILL) | WorkerExit::Lost) => Stopped::Killed,
            // Reaped with another status: it ended by itself as the kill went.
            Some(exit) => {
                self.exited = Some((exit, Instant::now()));
                Stopped::Exited(exit)
            }
            None => {
                self.shared.abandon(process);
                Stopped::Stuck
            }
        }
    }
}

impl Drop for Running<'_> {
    /// No path leaves a worker behind unkilled.
    fn drop(&mut self) {
        if self.process.is_some() {
            self.kill();
        }
    }
}

/// The worker's exit, if it comes within `budget`.
fn poll(process: &mut dyn WorkerProcess, budget: Duration) -> Option<WorkerExit> {
    let until = Instant::now() + budget;
    loop {
        match process.try_wait() {
            Ok(Some(exit)) => return Some(exit),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
            _ => return None,
        }
    }
}

/// Reads the first frame, capped at Ready's size, then the reply, checking
/// for end of file after a refusal or the reply.
fn spawn_reader(mut stdout: Box<dyn Read + Send>, events: Sender<Event>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("kettle-media-read".into())
        .spawn(move || {
            let first = wire::read_frame_within(
                &mut stdout,
                Direction::WorkerToParent,
                MAX_READY_FRAME_BYTES,
            );
            match first {
                Ok(Some(Frame::Ready(_))) => {
                    if events.send(Event::First(first, true)).is_err() {
                        return;
                    }
                }
                Ok(Some(Frame::Failure(_))) => {
                    let clean = at_end(&mut *stdout);
                    let _ = events.send(Event::First(first, clean));
                    return;
                }
                _ => {
                    let _ = events.send(Event::First(first, false));
                    return;
                }
            }
            let reply = wire::read_frame(&mut stdout, Direction::WorkerToParent);
            let clean = matches!(reply, Ok(Some(_))) && at_end(&mut *stdout);
            let _ = events.send(Event::Reply(reply, clean));
        })
        .map(drop)
}

/// Whether `reader` is at end of file, with nothing more to read.
fn at_end(reader: &mut dyn Read) -> bool {
    let mut byte = [0];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return true,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            _ => return false,
        }
    }
}

/// Writes Hello, then the job once Ready has matched, then closes stdin. If
/// the attempt ends first, the job never comes and stdin closes anyway. The
/// platform first guards this thread's pipe writes: a worker that has died
/// must turn a write into an error, never a signal that ends Kettle.
fn spawn_writer(
    shared: Arc<Shared>,
    mut stdin: Box<dyn Write + Send>,
    hello: Vec<u8>,
    job: Receiver<Vec<u8>>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("kettle-media-write".into())
        .spawn(move || {
            if shared.platform().guard_pipe_writes().is_err() {
                return;
            }
            if stdin
                .write_all(&hello)
                .and_then(|()| stdin.flush())
                .is_err()
            {
                return;
            }
            if let Ok(job) = job.recv() {
                let _ = stdin.write_all(&job).and_then(|()| stdin.flush());
            }
        })
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{
        FileIdentity, MAX_STUCK_WORKERS, MediaAvailability, UnavailableCause, WorkerClient,
        WorkerPlatform,
    };
    use crate::{Canvas, Failure, PROTOCOL_VERSION, Ready, Source, Target, Theme, content_digest};
    use std::collections::VecDeque;
    use std::io::{PipeReader, PipeWriter};
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const FAST: Budgets = Budgets {
        ready: Duration::from_millis(300),
        render: Some(Duration::from_millis(300)),
        cleanup: Duration::from_millis(50),
    };

    fn build_id() -> BuildId {
        BuildId::from_embedded("5.0.0", "ab12").unwrap()
    }

    fn job(bytes: usize) -> Job {
        Job {
            kind: JobKind::Raster,
            source: Source::Bytes(vec![7; bytes]),
            theme: Theme {
                background: [0; 4],
                foreground: [255; 4],
                palette: [[0; 4]; 16],
                accent: [0; 4],
                is_dark: true,
            },
            canvas: Canvas::Theme,
            target: Target {
                width: 1,
                height: 1,
                scale: 1.0,
                crop: None,
            },
            fallback_fonts: vec![],
        }
    }

    fn rendered() -> Rendered {
        Rendered {
            width: 1,
            height: 1,
            rgba: vec![255, 0, 0, 255],
            digest: content_digest(&[7], None).unwrap(),
            source_text: vec![],
            fence_sources: vec![],
            fence_count: 0,
            fence_index: None,
            uncovered_scripts: vec![],
            warnings: vec![],
        }
    }

    fn frame(frame: &Frame) -> Vec<u8> {
        wire::encode(frame, Direction::WorkerToParent).unwrap()
    }

    fn ready(build_id: BuildId) -> Vec<u8> {
        frame(&Frame::Ready(Ready { build_id }))
    }

    fn failure(code: FailureCode) -> Vec<u8> {
        frame(&Frame::Failure(Failure { code }))
    }

    /// How a scripted worker behaves.
    #[derive(Clone)]
    struct Script {
        /// What it writes to stdout once it has read Hello.
        output: Vec<u8>,
        /// Whether it then closes stdout (it exits) or holds it open.
        closes: bool,
        /// How it exits by itself once stdout is closed, if it does.
        exits: Option<WorkerExit>,
        /// Whether a kill ends it.
        killable: bool,
        /// Whether it exits right after its output while something it
        /// started keeps stdout open (until its group is killed).
        exits_holding_stdout: bool,
        /// Whether it exits by itself, with `exits`, just as a kill is sent.
        exits_at_kill: bool,
    }

    impl Script {
        fn replies(output: Vec<u8>, exit: WorkerExit) -> Self {
            Self {
                output,
                closes: true,
                exits: Some(exit),
                killable: true,
                exits_holding_stdout: false,
                exits_at_kill: false,
            }
        }
        fn silent() -> Self {
            Self {
                output: vec![],
                closes: false,
                exits: None,
                killable: true,
                exits_holding_stdout: false,
                exits_at_kill: false,
            }
        }
        /// Writes `output` and exits with `exit`, leaving stdout open.
        fn exits_holding(output: Vec<u8>, exit: WorkerExit) -> Self {
            Self {
                exits: Some(exit),
                exits_holding_stdout: true,
                ..Self::silent_with(output)
            }
        }
    }

    struct FakeProcess {
        exits: Option<WorkerExit>,
        killable: bool,
        killed: Arc<(Mutex<bool>, std::sync::Condvar)>,
        done: Arc<Mutex<bool>>,
        kills: Arc<AtomicUsize>,
        /// For an unkillable worker: set once it finally exits.
        released: Arc<std::sync::atomic::AtomicBool>,
        reaped: Arc<AtomicUsize>,
        exits_at_kill: bool,
    }

    impl FakeProcess {
        /// What a group kill does to the script: whatever holds stdout lets
        /// go.
        fn free_stdout(&self) {
            *self.killed.0.lock().unwrap() = true;
            self.killed.1.notify_all();
        }
    }

    impl WorkerProcess for FakeProcess {
        fn try_wait(&mut self) -> std::io::Result<Option<WorkerExit>> {
            let killed = *self.killed.0.lock().unwrap();
            let exit = if (self.killable && killed)
                || (!self.killable && self.released.load(Ordering::SeqCst))
            {
                Some(WorkerExit::Signal(9))
            } else if *self.done.lock().unwrap() {
                // Reaping an exited worker kills its group first.
                self.free_stdout();
                self.exits
            } else {
                None
            };
            if exit.is_some() {
                self.reaped.fetch_add(1, Ordering::SeqCst);
            }
            Ok(exit)
        }
        fn kill(&mut self) {
            if self.exits_at_kill {
                *self.done.lock().unwrap() = true;
            } else if self.killable {
                self.free_stdout();
            }
            self.kills.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct Fake {
        scripts: Mutex<VecDeque<Script>>,
        spawns: AtomicUsize,
        kills: Arc<AtomicUsize>,
        /// How long each spawn and file check take, and how many spawns have
        /// begun.
        spawn_delay: Mutex<Duration>,
        inspect_delay: Mutex<Duration>,
        spawn_calls: AtomicUsize,
        released: Arc<std::sync::atomic::AtomicBool>,
        reaped: Arc<AtomicUsize>,
        /// Threads that guarded their pipe writes, and writes from any other.
        guarded: Arc<Mutex<Vec<std::thread::ThreadId>>>,
        unguarded_writes: Arc<AtomicUsize>,
    }

    /// The worker's stdin, counting writes from a thread that never guarded
    /// its pipe writes.
    struct CheckedStdin {
        inner: PipeWriter,
        guarded: Arc<Mutex<Vec<std::thread::ThreadId>>>,
        unguarded_writes: Arc<AtomicUsize>,
    }

    impl Write for CheckedStdin {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if !self
                .guarded
                .lock()
                .unwrap()
                .contains(&std::thread::current().id())
            {
                self.unguarded_writes.fetch_add(1, Ordering::SeqCst);
            }
            self.inner.write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    impl Fake {
        fn new(scripts: Vec<Script>) -> Arc<Self> {
            Arc::new(Self {
                scripts: Mutex::new(scripts.into()),
                ..Self::default()
            })
        }
    }

    impl WorkerPlatform for Arc<Fake> {
        fn worker_path(&self) -> Result<&Path, UnavailableCause> {
            Ok(Path::new("/install/kettle-media-worker"))
        }
        fn inspect(&self, _: &Path) -> Result<FileIdentity, UnavailableCause> {
            let delay = *self.inspect_delay.lock().unwrap();
            std::thread::sleep(delay);
            Ok(FileIdentity {
                dev: 1,
                ino: 2,
                size: 3,
                mtime_seconds: 4,
                mtime_nanos: 5,
                ctime_seconds: 6,
                ctime_nanos: 7,
            })
        }
        fn verify(&self, _: &Path) -> Result<(), UnavailableCause> {
            Ok(())
        }
        fn guard_pipe_writes(&self) -> std::io::Result<()> {
            self.guarded
                .lock()
                .unwrap()
                .push(std::thread::current().id());
            Ok(())
        }
        fn spawn(&self, _: &Path) -> std::io::Result<SpawnedWorker> {
            self.spawn_calls.fetch_add(1, Ordering::SeqCst);
            let delay = *self.spawn_delay.lock().unwrap();
            std::thread::sleep(delay);
            self.spawns.fetch_add(1, Ordering::SeqCst);
            let script = self
                .scripts
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(Script::silent);
            let (stdin_reader, stdin_writer) = std::io::pipe()?;
            let (stdout_reader, stdout_writer) = std::io::pipe()?;
            let done = Arc::new(Mutex::new(false));
            let killed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
            run_script(
                script.clone(),
                stdin_reader,
                stdout_writer,
                Arc::clone(&done),
                Arc::clone(&killed),
            );
            Ok(SpawnedWorker {
                process: Box::new(FakeProcess {
                    exits: script.exits,
                    killable: script.killable,
                    killed,
                    done,
                    kills: Arc::clone(&self.kills),
                    released: Arc::clone(&self.released),
                    reaped: Arc::clone(&self.reaped),
                    exits_at_kill: script.exits_at_kill,
                }),
                stdin: Box::new(CheckedStdin {
                    inner: stdin_writer,
                    guarded: Arc::clone(&self.guarded),
                    unguarded_writes: Arc::clone(&self.unguarded_writes),
                }),
                stdout: Box::new(stdout_reader),
            })
        }
    }

    /// The worker's side: read Hello, write the script's output, then close
    /// stdout (and exit), or hold it open until killed, as a hung worker
    /// would. stdin is drained all the while.
    fn run_script(
        script: Script,
        mut stdin: PipeReader,
        mut stdout: PipeWriter,
        done: Arc<Mutex<bool>>,
        killed: Arc<(Mutex<bool>, std::sync::Condvar)>,
    ) {
        std::thread::spawn(move || {
            let hello = wire::read_frame(&mut stdin, Direction::ParentToWorker);
            if matches!(hello, Ok(Some(Frame::Hello(_)))) {
                let _ = stdout.write_all(&script.output);
            }
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut stdin, &mut std::io::sink());
            });
            if script.closes {
                drop(stdout);
                *done.lock().unwrap() = true;
            } else {
                if script.exits_holding_stdout {
                    *done.lock().unwrap() = true;
                }
                let (lock, changed) = &*killed;
                let mut killed = lock.lock().unwrap();
                while !*killed {
                    killed = changed.wait(killed).unwrap();
                }
                drop(stdout);
            }
        });
    }

    fn client(fake: &Arc<Fake>) -> WorkerClient {
        WorkerClient::with(build_id(), Box::new(Arc::clone(fake)), FAST)
    }

    fn reply(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn ready_requires_exact_build() {
        let other = BuildId::from_embedded("5.0.0", "cd34").unwrap();
        let fake = Fake::new(vec![Script::silent_with(ready(other))]);
        assert_eq!(
            client(&fake).render(&job(1)),
            Err(FailureCode::RestartRequired)
        );
        // Skew is never retried.
        assert_eq!(fake.spawns.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_reply_counts_once_the_worker_exits_cleanly() {
        let output = reply(&[ready(build_id()), frame(&Frame::Rendered(rendered()))]);
        let fake = Fake::new(vec![Script::replies(output, WorkerExit::Code(0))]);
        assert_eq!(client(&fake).render(&job(1)), Ok(rendered()));
    }

    #[test]
    fn complete_reply_before_crash_is_discarded() {
        let output = reply(&[ready(build_id()), frame(&Frame::Rendered(rendered()))]);
        for (exit, expected) in [
            (WorkerExit::Signal(6), FailureCode::RenderResource),
            (WorkerExit::Code(1), FailureCode::WorkerUnavailable),
        ] {
            let fake = Fake::new(vec![Script::replies(output.clone(), exit)]);
            assert_eq!(client(&fake).render(&job(1)), Err(expected), "{exit:?}");
        }
    }

    #[test]
    fn extra_frame_or_trailing_output_is_rejected() {
        let rendered = frame(&Frame::Rendered(rendered()));
        for extra in [failure(FailureCode::BadParams), vec![0]] {
            let output = reply(&[ready(build_id()), rendered.clone(), extra]);
            let fake = Fake::new(vec![Script::replies(output, WorkerExit::Code(0))]);
            assert_eq!(
                client(&fake).render(&job(1)),
                Err(FailureCode::WorkerUnavailable)
            );
        }
        // A second Ready where the reply belongs.
        let output = reply(&[ready(build_id()), ready(build_id())]);
        let fake = Fake::new(vec![Script::replies(output, WorkerExit::Code(0))]);
        assert_eq!(
            client(&fake).render(&job(1)),
            Err(FailureCode::WorkerUnavailable)
        );
    }

    #[test]
    fn oversize_header_allocates_no_payload() {
        // A Ready header claiming 50 MB: refused from the header alone, at
        // once, without waiting for (or allocating) a payload.
        let mut header = ready(build_id())[..wire::HEADER_BYTES].to_vec();
        header[7..11].copy_from_slice(&50_000_000u32.to_le_bytes());
        let fake = Fake::new(vec![Script::silent_with(header)]);
        let started = Instant::now();
        assert_eq!(
            client(&fake).render(&job(1)),
            Err(FailureCode::WorkerUnavailable)
        );
        assert!(started.elapsed() < FAST.ready);
    }

    #[test]
    fn worker_refusals_keep_only_worker_codes() {
        // (code, as the reply, at startup)
        for (code, as_reply, at_startup) in [
            (
                FailureCode::UnsupportedMedia,
                FailureCode::UnsupportedMedia,
                FailureCode::WorkerUnavailable,
            ),
            (
                FailureCode::UnknownMethod,
                FailureCode::UnknownMethod,
                FailureCode::UnknownMethod,
            ),
            (
                FailureCode::RestartRequired,
                FailureCode::RestartRequired,
                FailureCode::RestartRequired,
            ),
            (
                FailureCode::DisplayDisabled,
                FailureCode::WorkerUnavailable,
                FailureCode::WorkerUnavailable,
            ),
            (
                FailureCode::ReadOnly,
                FailureCode::WorkerUnavailable,
                FailureCode::WorkerUnavailable,
            ),
        ] {
            let fake = Fake::new(vec![
                Script::replies(failure(code), WorkerExit::Code(9)),
                Script::replies(
                    reply(&[ready(build_id()), failure(code)]),
                    WorkerExit::Code(0),
                ),
            ]);
            let client = client(&fake);
            assert_eq!(
                client.render(&job(1)),
                Err(at_startup),
                "{code:?} at startup"
            );
            assert_eq!(
                client.render(&job(1)),
                Err(as_reply),
                "{code:?} as the reply"
            );
        }
    }

    #[test]
    fn startup_refusals_must_end_cleanly() {
        for (script, expected) in [
            // More after the refusal.
            (
                Script::replies(
                    reply(&[failure(FailureCode::RestartRequired), vec![0]]),
                    WorkerExit::Code(0),
                ),
                FailureCode::WorkerUnavailable,
            ),
            // A refusal, then a crash rather than an exit.
            (
                Script::replies(failure(FailureCode::RestartRequired), WorkerExit::Signal(6)),
                FailureCode::RenderResource,
            ),
        ] {
            let fake = Fake::new(vec![script]);
            assert_eq!(client(&fake).render(&job(1)), Err(expected));
        }
    }

    #[test]
    fn an_exit_is_seen_while_something_holds_stdout() {
        // The worker exits 9 at startup, or crashes after Ready, while a
        // child it started keeps stdout open: the exit decides, at once,
        // with no retry.
        for (script, expected) in [
            (
                Script::exits_holding(vec![], WorkerExit::Code(9)),
                FailureCode::RestartRequired,
            ),
            (
                Script::exits_holding(ready(build_id()), WorkerExit::Signal(11)),
                FailureCode::RenderResource,
            ),
        ] {
            let fake = Fake::new(vec![script]);
            let started = Instant::now();
            assert_eq!(client(&fake).render(&job(1)), Err(expected));
            assert!(started.elapsed() < FAST.ready, "{:?}", started.elapsed());
            assert_eq!(fake.spawns.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn a_spawn_that_hangs_is_bounded() {
        let fake = Fake::new(vec![Script::silent()]);
        *fake.spawn_delay.lock().unwrap() = FAST.ready * 3;
        let started = Instant::now();
        assert_eq!(
            client(&fake).render(&job(1)),
            Err(FailureCode::RenderTimeout)
        );
        assert!(
            started.elapsed() < FAST.ready * 2,
            "{:?}",
            started.elapsed()
        );
        // The worker that finally starts is killed by the helper.
        let deadline = Instant::now() + Duration::from_secs(10);
        while fake.kills.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "the late worker was never killed"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_spawn_still_hanging_blocks_the_next() {
        let output = reply(&[ready(build_id()), frame(&Frame::Rendered(rendered()))]);
        let fake = Fake::new(vec![
            Script::silent(),
            Script::replies(output, WorkerExit::Code(0)),
        ]);
        *fake.spawn_delay.lock().unwrap() = FAST.ready * 3;
        let client = client(&fake);
        assert_eq!(client.render(&job(1)), Err(FailureCode::RenderTimeout));
        // The first start is still hung: no second one begins.
        assert_eq!(client.render(&job(1)), Err(FailureCode::WorkerUnavailable));
        assert_eq!(fake.spawn_calls.load(Ordering::SeqCst), 1);
        // Once it returns (and its worker is killed), starts resume.
        let deadline = Instant::now() + Duration::from_secs(10);
        while fake.kills.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "the late worker was never killed"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        *fake.spawn_delay.lock().unwrap() = Duration::ZERO;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match client.render(&job(1)) {
                Ok(result) => {
                    assert_eq!(result, rendered());
                    break;
                }
                Err(FailureCode::WorkerUnavailable) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn an_exit_as_the_startup_deadline_passes_is_its_own_answer() {
        // Silent until the deadline, then it exits 9 by itself just as the
        // kill goes out: another build, not a cold start to retry.
        let script = Script {
            exits: Some(WorkerExit::Code(9)),
            exits_at_kill: true,
            ..Script::silent()
        };
        let fake = Fake::new(vec![script]);
        assert_eq!(
            client(&fake).render(&job(1)),
            Err(FailureCode::RestartRequired)
        );
        assert_eq!(fake.spawns.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_check_that_hangs_is_bounded() {
        let fake = Fake::new(vec![Script::silent()]);
        *fake.inspect_delay.lock().unwrap() = FAST.ready * 3;
        let client = client(&fake);
        let started = Instant::now();
        assert_eq!(client.render(&job(1)), Err(FailureCode::RenderTimeout));
        assert!(
            started.elapsed() < FAST.ready * 2,
            "{:?}",
            started.elapsed()
        );
        // Still hung: the next render starts no second check.
        assert_eq!(client.render(&job(1)), Err(FailureCode::WorkerUnavailable));
        assert!(
            started.elapsed() < FAST.ready * 2,
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_crash_as_the_reply_deadline_passes_is_its_own_answer() {
        let script = Script {
            exits: Some(WorkerExit::Signal(11)),
            exits_at_kill: true,
            ..Script::silent_with(ready(build_id()))
        };
        let fake = Fake::new(vec![script]);
        assert_eq!(
            client(&fake).render(&job(1)),
            Err(FailureCode::RenderResource)
        );
    }

    #[test]
    fn pipe_writes_are_guarded_before_the_first_write() {
        let output = reply(&[ready(build_id()), frame(&Frame::Rendered(rendered()))]);
        let fake = Fake::new(vec![Script::replies(output, WorkerExit::Code(0))]);
        assert_eq!(client(&fake).render(&job(1)), Ok(rendered()));
        assert_eq!(fake.guarded.lock().unwrap().len(), 1);
        assert_eq!(fake.unguarded_writes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn startup_timeout_retries_once() {
        let fake = Fake::new(vec![Script::silent(), Script::silent()]);
        let started = Instant::now();
        assert_eq!(
            client(&fake).render(&job(1)),
            Err(FailureCode::RenderTimeout)
        );
        assert_eq!(fake.spawns.load(Ordering::SeqCst), 2);
        assert!(started.elapsed() >= FAST.ready * 2);
        // A second worker that answers is used.
        let output = reply(&[ready(build_id()), frame(&Frame::Rendered(rendered()))]);
        let fake = Fake::new(vec![
            Script::silent(),
            Script::replies(output, WorkerExit::Code(0)),
        ]);
        assert_eq!(client(&fake).render(&job(1)), Ok(rendered()));
    }

    #[test]
    fn identity_failure_never_retries() {
        // A worker that exits 9 at startup (another build), and one that
        // dies before Ready, are each tried once.
        for (exit, expected) in [
            (WorkerExit::Code(9), FailureCode::RestartRequired),
            (WorkerExit::Code(8), FailureCode::WorkerUnavailable),
            (WorkerExit::Signal(11), FailureCode::RenderResource),
        ] {
            let fake = Fake::new(vec![Script::replies(vec![], exit)]);
            assert_eq!(client(&fake).render(&job(1)), Err(expected), "{exit:?}");
            assert_eq!(fake.spawns.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn no_render_retry_after_ready() {
        // Ready, then silence: the job's deadline, once.
        let fake = Fake::new(vec![Script::silent_with(ready(build_id()))]);
        let started = Instant::now();
        assert_eq!(
            client(&fake).render(&job(1)),
            Err(FailureCode::RenderTimeout)
        );
        assert_eq!(fake.spawns.load(Ordering::SeqCst), 1);
        assert!(started.elapsed() < FAST.ready + FAST.render.unwrap() * 2);
    }

    #[test]
    fn partial_stdout_is_deadline_bounded() {
        let whole = frame(&Frame::Rendered(rendered()));
        let output = reply(&[ready(build_id()), whole[..whole.len() / 2].to_vec()]);
        let fake = Fake::new(vec![Script::silent_with(output)]);
        assert_eq!(
            client(&fake).render(&job(1)),
            Err(FailureCode::RenderTimeout)
        );
        assert_eq!(fake.kills.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unreapable_child_abandons_without_hanging_shutdown() {
        let unkillable = Script {
            killable: false,
            ..Script::silent()
        };
        let fake = Fake::new(vec![unkillable.clone(), unkillable]);
        let client = client(&fake);
        // Never ready, and it will not die: no retry, since cleanup never
        // finished, and the call returns within its bounds.
        let started = Instant::now();
        assert_eq!(client.render(&job(1)), Err(FailureCode::RenderTimeout));
        assert!(started.elapsed() < FAST.ready + FAST.cleanup * 4);
        assert_eq!(fake.spawns.load(Ordering::SeqCst), 1);
        assert_ne!(
            client.availability(),
            MediaAvailability::Unavailable(UnavailableCause::StuckWorkers)
        );
        second_abandoned_child_disables_media(&client, &fake);
    }

    /// The second stuck worker turns media off: nothing more is started.
    /// Both are still reaped once they finally exit.
    fn second_abandoned_child_disables_media(client: &WorkerClient, fake: &Arc<Fake>) {
        assert_eq!(client.render(&job(1)), Err(FailureCode::RenderTimeout));
        assert_eq!(
            client.availability(),
            MediaAvailability::Unavailable(UnavailableCause::StuckWorkers)
        );
        assert_eq!(client.render(&job(1)), Err(FailureCode::WorkerUnavailable));
        assert_eq!(fake.spawns.load(Ordering::SeqCst), MAX_STUCK_WORKERS);
        assert_eq!(client.shared.abandoned_count(), 2);
        fake.released.store(true, Ordering::SeqCst);
        client.availability();
        assert_eq!(client.shared.abandoned_count(), 0);
        assert_eq!(fake.reaped.load(Ordering::SeqCst), 2);
        // Media stays off for the life of the process.
        assert_eq!(
            client.availability(),
            MediaAvailability::Unavailable(UnavailableCause::StuckWorkers)
        );
    }

    #[test]
    fn exits_name_their_failures() {
        assert_eq!(
            exit_failure(WorkerExit::Code(4)),
            FailureCode::RenderTimeout
        );
        assert_eq!(
            exit_failure(WorkerExit::Code(9)),
            FailureCode::RestartRequired
        );
        assert_eq!(
            exit_failure(WorkerExit::Signal(24)),
            FailureCode::RenderResource
        );
        assert_eq!(
            exit_failure(WorkerExit::Code(0)),
            FailureCode::WorkerUnavailable
        );
        assert_eq!(
            exit_failure(WorkerExit::Code(2)),
            FailureCode::WorkerUnavailable
        );
        assert_eq!(
            exit_failure(WorkerExit::Lost),
            FailureCode::WorkerUnavailable
        );
    }

    #[test]
    fn production_budgets_match_the_plan() {
        let budgets = Budgets::PRODUCTION;
        assert_eq!(budgets.ready, Duration::from_secs(5));
        assert_eq!(
            budgets.render_deadline(JobKind::Raster),
            Duration::from_secs(2)
        );
        assert_eq!(
            budgets.render_deadline(JobKind::Svg),
            Duration::from_secs(3)
        );
        assert_eq!(budgets.cleanup, Duration::from_millis(250));
        let _ = PROTOCOL_VERSION;
    }

    impl Script {
        /// Writes `output` and then holds stdout open, never exiting.
        fn silent_with(output: Vec<u8>) -> Self {
            Self {
                output,
                ..Self::silent()
            }
        }
    }
}
