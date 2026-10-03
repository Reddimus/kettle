//! The GUI's view of its media worker: where the worker is installed and
//! whether it may run. The platform work (paths, file checks, signatures) is
//! injected through [`WorkerPlatform`], so this crate stays free of unsafe code
//! and opens nothing itself. This version renders nothing: a worker that
//! passes every check is still reported as [`UnavailableCause::Incomplete`].

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use crate::BuildId;

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
    /// The worker passed its checks, but this build cannot render yet.
    Incomplete,
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
            Self::Incomplete => "incomplete",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaAvailability {
    /// The first check has not finished.
    Checking,
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
}

/// Answers whether media previews are available, without blocking the caller
/// on the filesystem or a signature check.
pub struct WorkerClient {
    build_id: BuildId,
    shared: Arc<Shared>,
}

struct Shared {
    platform: Box<dyn WorkerPlatform>,
    state: Mutex<State>,
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
        Self {
            build_id,
            shared: Arc::new(Shared {
                platform,
                state: Mutex::new(State::default()),
            }),
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
            Some(Ok(_)) => MediaAvailability::Unavailable(UnavailableCause::Incomplete),
            Some(Err(cause)) => MediaAvailability::Unavailable(cause),
        }
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
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
    fn a_verified_worker_is_still_incomplete() {
        let platform = Arc::new(Scripted::new(vec![Ok(identity(1))]));
        let client = client(&platform);
        assert_eq!(
            settle(&client),
            MediaAvailability::Unavailable(UnavailableCause::Incomplete)
        );
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
        assert_eq!(
            settle(&client),
            MediaAvailability::Unavailable(UnavailableCause::Incomplete)
        );
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
