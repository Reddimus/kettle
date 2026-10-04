//! The GUI's view of its media worker: where the worker is installed, whether
//! it may run, and running one job in a fresh worker (see `lifecycle`). The
//! platform work (paths, file checks, signatures, starting and killing the
//! process) is injected through [`WorkerPlatform`], so this crate stays free
//! of unsafe code and opens nothing itself. Availability checks the installed
//! worker without starting it; each render also checks the build handshake.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::lifecycle::{self, Budgets};
use crate::{BuildId, FailureCode, Job, Rendered};

/// After this many workers that would not die, media stays off for the life
/// of this process: whatever stops them dying would stop the next one too.
pub const MAX_STUCK_WORKERS: usize = 2;

/// Why media previews are unavailable. Fixed codes only: never a path, a file
/// name or a tool's output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableCause {
    /// This platform has no media worker.
    UnsupportedPlatform,
    /// The running executable's install directory could not be established.
    NoInstallLocation,
    /// No worker beside the running executable.
    WorkerMissing,
    /// The worker is not a regular executable file, is writable by someone
    /// other than its owner, belongs to another user, or could not be read.
    UnsafeWorkerFile,
    /// The worker's signature does not meet this platform's requirement, or
    /// the file changed while it was being checked.
    Unverified,
    /// The check itself could not run.
    CheckFailed,
    /// Workers that were killed would not exit, so media is off until Kettle
    /// restarts.
    StuckWorkers,
}

impl UnavailableCause {
    /// The stable code reported to control clients.
    pub fn code(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::NoInstallLocation => "no_install_location",
            Self::WorkerMissing => "worker_missing",
            Self::UnsafeWorkerFile => "unsafe_worker_file",
            Self::Unverified => "unverified_worker",
            Self::CheckFailed => "check_failed",
            Self::StuckWorkers => "stuck_workers",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaAvailability {
    /// The first check has not finished.
    Checking,
    /// The worker passed its file/platform checks. Each job still requires
    /// a matching Ready and a successful reply and process exit.
    Available,
    Unavailable(UnavailableCause),
}

/// What the worker file looked like when it was checked. Any difference later,
/// including a changed status time from an in-place rewrite that kept the
/// modification time, means it is verified again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_seconds: i64,
    pub mtime_nanos: i64,
    pub ctime_seconds: i64,
    pub ctime_nanos: i64,
}

/// How a worker process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerExit {
    Code(i32),
    Signal(i32),
    /// Something else reaped it (an inherited ignored `SIGCHLD` does that),
    /// so how it ended is unknown and its process group is no longer ours.
    Lost,
}

/// A running worker as the platform started it, in a process group of its
/// own. Implementations must not block or panic.
pub trait WorkerProcess: Send {
    /// Whether it has exited, reaping it if so. Before reaping a worker that
    /// exited by itself, kill its process group: nothing it started outlives
    /// it, and the group id is never signalled once it could be reused.
    fn try_wait(&mut self) -> std::io::Result<Option<WorkerExit>>;
    /// Kill its whole process group, if it is not reaped yet. Safe to repeat.
    fn kill(&mut self);
    /// The memory the worker and everything it started hold now, in bytes.
    /// An error when a live worker cannot be measured: the job then fails
    /// rather than count it as nothing.
    fn footprint(&mut self) -> std::io::Result<u64>;
}

/// A worker just started: the process, and the two pipes to it.
pub struct SpawnedWorker {
    pub process: Box<dyn WorkerProcess>,
    pub stdin: Box<dyn Write + Send>,
    pub stdout: Box<dyn Read + Send>,
}

/// The command every platform starts the worker with: no arguments, an empty
/// environment, `/` as the working directory, stdin and stdout piped, stderr
/// discarded, and on Unix a process group of its own, so killing the group
/// reaches anything it starts.
pub fn worker_command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
}

/// The platform services the client needs. Implementations must not panic:
/// release builds abort on panic, which would take the GUI with them.
pub trait WorkerPlatform: Send + Sync {
    /// The installed worker's path, fixed when the platform was made. Never
    /// searched for in `PATH`, the working directory or the environment.
    fn worker_path(&self) -> Result<&Path, UnavailableCause>;
    /// Check the worker file and its directory without following a link at
    /// the leaf, and report its identity.
    fn inspect(&self, path: &Path) -> Result<FileIdentity, UnavailableCause>;
    /// Check the worker's signature against this platform's requirement.
    fn verify(&self, path: &Path) -> Result<(), UnavailableCause>;
    /// Start the worker at `path` with [`worker_command`].
    fn spawn(&self, path: &Path) -> std::io::Result<SpawnedWorker>;
    /// Called first on the thread that writes to the worker's stdin: make a
    /// write to a pipe whose worker has died fail with an error instead of
    /// raising `SIGPIPE`, which ends a process that keeps its default action
    /// (`kettle` restores it for its command line).
    fn guard_pipe_writes(&self) -> std::io::Result<()>;
}

/// Answers whether media previews are available, without blocking the caller
/// on the filesystem or a signature check, and renders one job at a time.
pub struct WorkerClient {
    build_id: BuildId,
    pub(crate) shared: Arc<Shared>,
    budgets: Budgets,
    /// One job at a time.
    rendering: Mutex<()>,
}

pub(crate) struct Shared {
    platform: Box<dyn WorkerPlatform>,
    state: Mutex<State>,
    /// How many killed workers would not exit.
    pub(crate) stuck: AtomicUsize,
    /// Those of them not reaped yet, kept so they are reaped once they do
    /// exit, at the next check or render.
    abandoned: Mutex<Vec<Box<dyn WorkerProcess>>>,
    /// Starts still running after their attempt stopped waiting.
    pub(crate) late_spawns: AtomicUsize,
}

#[derive(Default)]
struct State {
    checking: bool,
    last: Option<Result<FileIdentity, UnavailableCause>>,
}

impl std::fmt::Debug for WorkerClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerClient")
            .field("build_id", &self.build_id)
            .finish_non_exhaustive()
    }
}

impl WorkerClient {
    /// Nothing is checked until [`Self::availability`] is first asked.
    pub fn new(build_id: BuildId, platform: Box<dyn WorkerPlatform>) -> Self {
        Self::with(build_id, platform, Budgets::PRODUCTION)
    }

    /// A client with shorter deadlines, so tests of each bound finish in
    /// seconds. Test fixture only.
    #[cfg(feature = "test-worker")]
    pub fn with_test_budgets(
        build_id: BuildId,
        platform: Box<dyn WorkerPlatform>,
        ready: std::time::Duration,
        render: std::time::Duration,
        footprint_limit: u64,
    ) -> Self {
        Self::with(
            build_id,
            platform,
            Budgets {
                ready,
                render: Some(render),
                footprint_limit,
                ..Budgets::PRODUCTION
            },
        )
    }

    pub(crate) fn with(
        build_id: BuildId,
        platform: Box<dyn WorkerPlatform>,
        budgets: Budgets,
    ) -> Self {
        Self {
            build_id,
            shared: Arc::new(Shared {
                platform,
                state: Mutex::new(State::default()),
                stuck: AtomicUsize::new(0),
                abandoned: Mutex::new(Vec::new()),
                late_spawns: AtomicUsize::new(0),
            }),
            budgets,
            rendering: Mutex::new(()),
        }
    }

    /// The identity this GUI's worker must answer with.
    pub fn build_id(&self) -> &BuildId {
        &self.build_id
    }

    /// The last answer, starting a fresh check in the background when none is
    /// running, so a replaced or removed worker shows on the next call. The
    /// first call answers [`MediaAvailability::Checking`].
    pub fn availability(&self) -> MediaAvailability {
        self.shared.reap_abandoned();
        if self.shared.stuck.load(Ordering::Acquire) >= MAX_STUCK_WORKERS {
            return MediaAvailability::Unavailable(UnavailableCause::StuckWorkers);
        }
        let mut state = self.shared.lock();
        if !state.checking {
            let verified = state.last.and_then(Result::ok);
            let shared = Arc::clone(&self.shared);
            let spawned = std::thread::Builder::new()
                .name("kettle-media-check".into())
                .spawn(move || {
                    let result = check(shared.platform.as_ref(), verified);
                    let mut state = shared.lock();
                    state.last = Some(result);
                    state.checking = false;
                });
            match spawned {
                Ok(_) => state.checking = true,
                Err(_) => state.last = Some(Err(UnavailableCause::CheckFailed)),
            }
        }
        match state.last {
            None => MediaAvailability::Checking,
            Some(Ok(_)) => MediaAvailability::Available,
            Some(Err(cause)) => MediaAvailability::Unavailable(cause),
        }
    }

    /// Render `job` in a fresh worker, checked again first (inside the startup
    /// deadline, so a stalled filesystem cannot hold it). Blocks the calling
    /// thread, which must not be the UI's, for at most the startup deadline
    /// (twice, when the first worker never answers), the job's deadline and
    /// cleanup; jobs run one at a time. A worker that never becomes ready is
    /// retried once; after Ready nothing is retried.
    pub fn render(&self, job: &Job) -> Result<Rendered, FailureCode> {
        let _one_at_a_time = self
            .rendering
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.shared.reap_abandoned();
        for attempt in 0..2 {
            if self.shared.stuck.load(Ordering::Acquire) >= MAX_STUCK_WORKERS {
                return Err(FailureCode::WorkerUnavailable);
            }
            match lifecycle::attempt(&self.shared, &self.build_id, job, self.budgets) {
                Err(lifecycle::NeverReady) if attempt == 0 => {}
                Err(lifecycle::NeverReady) => return Err(FailureCode::RenderTimeout),
                Ok(result) => return result,
            }
        }
        Err(FailureCode::RenderTimeout)
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn platform(&self) -> &dyn WorkerPlatform {
        self.platform.as_ref()
    }

    /// The worker's path, after the same check [`WorkerClient::availability`]
    /// runs, done here and now, and recorded for it.
    pub(crate) fn checked_path(&self) -> Result<&Path, UnavailableCause> {
        let verified = self.lock().last.and_then(Result::ok);
        let result = check(self.platform.as_ref(), verified);
        self.lock().last = Some(result);
        result?;
        self.platform.worker_path()
    }

    /// Count a killed worker that would not exit, and keep it to reap later.
    pub(crate) fn abandon(&self, process: Box<dyn WorkerProcess>) {
        self.stuck.fetch_add(1, Ordering::AcqRel);
        self.abandoned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(process);
    }

    /// Reap the abandoned workers that have exited since; never waits.
    fn reap_abandoned(&self) {
        self.abandoned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain_mut(|process| !matches!(process.try_wait(), Ok(Some(_))));
    }

    #[cfg(test)]
    pub(crate) fn abandoned_count(&self) -> usize {
        self.abandoned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// One check: the file, then its signature unless this exact file already
/// passed, then the file again, so a replacement during the signature check is
/// refused rather than trusted.
fn check(
    platform: &dyn WorkerPlatform,
    verified: Option<FileIdentity>,
) -> Result<FileIdentity, UnavailableCause> {
    let path = platform.worker_path()?;
    let before = platform.inspect(path)?;
    if verified == Some(before) {
        return Ok(before);
    }
    platform.verify(path)?;
    if platform.inspect(path)? != before {
        return Err(UnavailableCause::Unverified);
    }
    Ok(before)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PROTOCOL_VERSION;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn identity(ino: u64) -> FileIdentity {
        FileIdentity {
            dev: 1,
            ino,
            size: 10,
            mtime_seconds: 2,
            mtime_nanos: 3,
            ctime_seconds: 4,
            ctime_nanos: 5,
        }
    }

    fn build_id() -> BuildId {
        BuildId::from_embedded("5.0.0", "0123456789abcdef").unwrap()
    }

    /// A platform that answers from a script: each `inspect` takes the next
    /// identity (repeating the last), and `verify` answers `verifies`.
    struct Scripted {
        path: Result<PathBuf, UnavailableCause>,
        inspections: Mutex<Vec<Result<FileIdentity, UnavailableCause>>>,
        verifies: Result<(), UnavailableCause>,
        verify_calls: AtomicUsize,
        inspect_gate: Option<Mutex<mpsc::Receiver<()>>>,
    }

    impl Scripted {
        fn new(inspections: Vec<Result<FileIdentity, UnavailableCause>>) -> Self {
            Self {
                path: Ok(PathBuf::from("/install/kettle-media-worker")),
                inspections: Mutex::new(inspections),
                verifies: Ok(()),
                verify_calls: AtomicUsize::new(0),
                inspect_gate: None,
            }
        }
    }

    impl WorkerPlatform for Arc<Scripted> {
        fn worker_path(&self) -> Result<&Path, UnavailableCause> {
            self.path.as_deref().map_err(|cause| *cause)
        }
        fn inspect(&self, _: &Path) -> Result<FileIdentity, UnavailableCause> {
            if let Some(gate) = &self.inspect_gate {
                let _ = gate.lock().unwrap().recv();
            }
            let mut inspections = self.inspections.lock().unwrap();
            if inspections.len() > 1 {
                inspections.remove(0)
            } else {
                inspections[0]
            }
        }
        fn verify(&self, _: &Path) -> Result<(), UnavailableCause> {
            self.verify_calls.fetch_add(1, Ordering::SeqCst);
            self.verifies
        }
        fn spawn(&self, _: &Path) -> std::io::Result<SpawnedWorker> {
            panic!("availability must not start a worker")
        }
        fn guard_pipe_writes(&self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn client(platform: &Arc<Scripted>) -> WorkerClient {
        WorkerClient::new(build_id(), Box::new(Arc::clone(platform)))
    }

    /// Ask until the answer is no longer `Checking`, then let the check that
    /// last question started finish too.
    fn settle(client: &WorkerClient) -> MediaAvailability {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let answer = client.availability();
            if answer != MediaAvailability::Checking {
                wait_idle(client);
                return answer;
            }
            assert!(Instant::now() < deadline, "the check never finished");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn wait_idle(client: &WorkerClient) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while client.shared.lock().checking {
            assert!(Instant::now() < deadline, "the check never finished");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn worker_missing_is_typed() {
        let platform = Arc::new(Scripted::new(vec![Err(UnavailableCause::WorkerMissing)]));
        let client = client(&platform);
        assert_eq!(
            settle(&client),
            MediaAvailability::Unavailable(UnavailableCause::WorkerMissing)
        );
        assert_eq!(platform.verify_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_unsupported_platform_is_never_inspected() {
        let mut scripted = Scripted::new(vec![Ok(identity(1))]);
        scripted.path = Err(UnavailableCause::UnsupportedPlatform);
        let platform = Arc::new(scripted);
        let client = client(&platform);
        assert_eq!(
            settle(&client),
            MediaAvailability::Unavailable(UnavailableCause::UnsupportedPlatform)
        );
        assert_eq!(platform.verify_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_verified_worker_is_available_without_being_spawned() {
        let platform = Arc::new(Scripted::new(vec![Ok(identity(1))]));
        let client = client(&platform);
        assert_eq!(settle(&client), MediaAvailability::Available);
    }

    #[test]
    fn verification_is_cached_by_file_identity() {
        let platform = Arc::new(Scripted::new(vec![Ok(identity(1))]));
        let client = client(&platform);
        settle(&client);
        for _ in 0..5 {
            client.availability();
            wait_idle(&client);
        }
        assert_eq!(platform.verify_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_replaced_worker_is_verified_again() {
        // First check: before and after the signature check. Every later
        // inspection sees a new file.
        let platform = Arc::new(Scripted::new(vec![
            Ok(identity(1)),
            Ok(identity(1)),
            Ok(identity(2)),
        ]));
        let client = client(&platform);
        // `settle` runs two checks: the first verifies file 1, the second
        // sees file 2 and verifies it. A third sees file 2 again and does not.
        settle(&client);
        client.availability();
        wait_idle(&client);
        assert_eq!(platform.verify_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_worker_replaced_during_verification_is_refused() {
        let platform = Arc::new(Scripted::new(vec![Ok(identity(1)), Ok(identity(2))]));
        let client = client(&platform);
        assert_eq!(
            settle(&client),
            MediaAvailability::Unavailable(UnavailableCause::Unverified)
        );
    }

    #[test]
    fn a_failed_verification_is_not_cached() {
        let mut scripted = Scripted::new(vec![Ok(identity(1))]);
        scripted.verifies = Err(UnavailableCause::Unverified);
        let platform = Arc::new(scripted);
        let client = client(&platform);
        assert_eq!(
            settle(&client),
            MediaAvailability::Unavailable(UnavailableCause::Unverified)
        );
        let verified = platform.verify_calls.load(Ordering::SeqCst);
        client.availability();
        wait_idle(&client);
        assert_eq!(platform.verify_calls.load(Ordering::SeqCst), verified + 1);
    }

    #[test]
    fn availability_never_waits_and_runs_one_check_at_a_time() {
        let (open, gate) = mpsc::channel();
        let mut scripted = Scripted::new(vec![Ok(identity(1))]);
        scripted.inspect_gate = Some(Mutex::new(gate));
        let platform = Arc::new(scripted);
        let client = client(&platform);
        // The check is held inside `inspect`: every question answers at once,
        // and none starts a second check.
        for _ in 0..3 {
            assert_eq!(client.availability(), MediaAvailability::Checking);
        }
        // One check inspects twice (before and after the signature check),
        // and the second question after it finishes starts one more.
        for _ in 0..4 {
            open.send(()).unwrap();
        }
        assert_eq!(settle(&client), MediaAvailability::Available);
        assert_eq!(platform.verify_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn embedded_build_id_carries_this_protocol() {
        let id = build_id();
        assert_eq!(id.crate_version, "5.0.0");
        assert_eq!(id.source_hash, "0123456789abcdef");
        assert_eq!(id.protocol_version, PROTOCOL_VERSION);
        assert!(BuildId::from_embedded("5.0.0", "").is_err());
        assert!(BuildId::from_embedded("5.0.0", "4.9.0 (0123)").is_err());
        assert!(BuildId::from_embedded("", "0123").is_err());
    }
}
