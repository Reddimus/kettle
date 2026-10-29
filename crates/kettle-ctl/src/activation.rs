//! Private single-instance activation for bare GUI launches.
//!
//! This is intentionally separate from the opt-in agent control plane. Every
//! bare GUI launch may use this endpoint to open a fresh window or focus
//! the primary's existing window.
//! Explicit CLI launches bypass it in the binary.

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::transport::{self, CtlListener, CtlStream};
use crate::{ensure_private_dir, stable_hash};
#[cfg(unix)]
use crate::{length_safe_unix_socket_path, private_temp_socket_dir, unix_socket_path_fits};

const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME_BYTES: usize = 8 * 1024;
const MAX_CWD_BYTES: usize = 4096;
const MAX_RECORDING_KEY_BYTES: usize = 128;
const MAX_LAUNCH_ID_BYTES: usize = 128;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const PRIMARY_STARTUP_WAIT: Duration = Duration::from_secs(5);
const PRIMARY_RETRY_DELAY: Duration = Duration::from_millis(25);
const MAX_ACTIVE_CLIENTS: usize = 16;
/// Launches the primary remembers the outcome of. Comfortably above the
/// concurrent-client cap so an in-flight launch is never evicted by newer
/// ones, and small enough to stay a linear scan of tiny records.
const MAX_REMEMBERED_LAUNCHES: usize = 64;
/// How long a settled outcome is worth remembering. A retry only happens
/// inside one launch's own election window (a few `IO_TIMEOUT`s), so this is
/// generous; it exists so a long-lived primary does not accumulate keys.
const LAUNCH_RECORD_TTL: Duration = Duration::from_secs(60);
/// How long a duplicate of an in-flight launch waits for the attempt that owns
/// it before answering without one.
///
/// Strictly shorter than [`IO_TIMEOUT`], and by a wide margin, because the
/// requester is reading its response under a deadline of one `IO_TIMEOUT` that
/// started *before* this wait did — it sent the request first. A wait that
/// reaches the same bound produces an answer nobody is left to read: the
/// requester has already failed, and the launch ends up in a separate process
/// having waited the whole time for nothing. The owner it waits for is itself
/// bounded (the UI confirms or refuses a window within its own timeout), so
/// this only has to cover the gap between two attempts at the same launch, not
/// the handler's whole runtime.
const LAUNCH_JOIN_WAIT: Duration = Duration::from_millis(2_500);
const _: () = assert!(
    LAUNCH_JOIN_WAIT.as_millis() * 2 <= IO_TIMEOUT.as_millis(),
    "a duplicate launch's answer must be produced well inside the requester's read deadline"
);

/// Properties that must match before a bare launch can join a primary.
///
/// The recording key is a bounded, path-derived fingerprint rather than the
/// path itself. This prevents a launch from silently joining a process with a
/// different dev-record destination or raw-input policy without putting user
/// paths on the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_key: Option<String>,
    #[serde(default)]
    pub record_raw_input: bool,
}

/// A bounded window request accepted by the activation endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationRequest {
    v: u32,
    action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    identity: LaunchIdentity,
    /// Idempotency key: one value per *launch*, kept across that launch's
    /// retries. Delivery is at-least-once. The primary opens the window before
    /// it writes the response, so when a slow cold start loses the response,
    /// the secondary re-sends the identical request. Without a key the primary
    /// cannot tell that retry apart from a second launcher click.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    launch_id: Option<String>,
}

impl ActivationRequest {
    pub fn new(cwd: Option<String>, identity: LaunchIdentity) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            action: "open_window".to_string(),
            cwd,
            identity,
            launch_id: Some(new_launch_id()),
        }
    }

    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// A native reopen focuses an existing window instead of adding one.
    pub fn for_reopen(cwd: Option<String>, identity: LaunchIdentity) -> Self {
        Self {
            action: "focus_or_open".into(),
            ..Self::new(cwd, identity)
        }
    }

    pub fn focuses_existing(&self) -> bool {
        self.action == "focus_or_open"
    }

    pub fn requires_recording(&self) -> bool {
        self.identity.recording_key.is_some()
    }

    fn launch_id(&self) -> Option<&str> {
        self.launch_id.as_deref()
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.v == PROTOCOL_VERSION
            && matches!(self.action.as_str(), "open_window" | "focus_or_open")
            && self.cwd.as_ref().is_none_or(|cwd| {
                !cwd.is_empty()
                    && cwd.len() <= MAX_CWD_BYTES
                    && !cwd.chars().any(char::is_control)
                    && Path::new(cwd).is_absolute()
            })
            && self.identity.recording_key.as_ref().is_none_or(|key| {
                !key.is_empty()
                    && key.len() <= MAX_RECORDING_KEY_BYTES
                    && key
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-'))
            })
            && (self.identity.recording_key.is_some() || !self.identity.record_raw_input)
            && self.launch_id.as_ref().is_none_or(|id| {
                !id.is_empty()
                    && id.len() <= MAX_LAUNCH_ID_BYTES
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
    }
}

/// A key no other launch shares: this process's pid (unique among live
/// processes), the wall-clock nanosecond it was minted (distinguishing
/// launches from a process whose pid was later recycled), and a counter
/// (distinguishing keys minted inside one nanosecond tick).
///
/// A key is never compared across machines and is only remembered for
/// [`LAUNCH_RECORD_TTL`], so no stronger source of entropy is warranted; it
/// must merely never repeat while a primary still remembers it.
fn new_launch_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let minted = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        minted,
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ResponseStatus {
    Activated,
    Busy,
    Incompatible,
}

#[derive(Debug, Serialize, Deserialize)]
struct ActivationResponse {
    v: u32,
    status: ResponseStatus,
}

/// Result of a bare launch's activation/election attempt.
#[derive(Debug)]
pub enum ActivationOutcome {
    /// The primary confirmed that it opened the requested window.
    Activated,
    /// No primary existed; this process owns the endpoint and must host it.
    Primary(PrimaryHandle),
    /// A primary was present but could not accept this launch. Continue in a
    /// separate process so a click never silently disappears.
    Standalone,
}

struct Primary {
    listener: CtlListener,
    _lock: kettle_state::ExclusiveFileLock,
    identity: LaunchIdentity,
}

/// Cloneable, one-shot handoff from binary election to the UI server startup.
#[derive(Clone)]
pub struct PrimaryHandle {
    primary: Arc<Mutex<Option<Primary>>>,
    other_builds: OtherBuilds,
}

impl std::fmt::Debug for PrimaryHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PrimaryHandle(..)")
    }
}

impl PrimaryHandle {
    /// Primaries of other builds found running when this one was elected.
    pub fn other_builds(&self) -> OtherBuilds {
        self.other_builds.clone()
    }
}

/// One build of Kettle: the 16 lowercase hex digits of the source hash it was
/// built from. Each build elects its own primary, so a launch of a newer
/// installed build never hands its window to a primary still running an
/// older one (#575). A source hash, unlike the package version, also tells a
/// development build from the release of the same version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildKey([u8; 16]);

impl BuildKey {
    /// `None` unless `hex` is exactly 16 lowercase hex digits.
    pub fn new(hex: &str) -> Option<Self> {
        let bytes: [u8; 16] = hex.as_bytes().try_into().ok()?;
        bytes
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            .then_some(Self(bytes))
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("checked ASCII hex")
    }
}

/// Primaries of other builds still running, such as the old version's after
/// an update. Shared per-user state that two primaries would fight over, such
/// as the restorable session, waits until they have exited.
#[derive(Clone, Debug, Default)]
pub struct OtherBuilds(Arc<Mutex<OtherBuildState>>);

#[derive(Debug, Default)]
struct OtherBuildState {
    locks: Vec<PathBuf>,
    // An incomplete scan cannot authorize shared-state writes.
    rescan: Option<ActivationPaths>,
}

impl OtherBuilds {
    /// Whether any of them is still running. Checks each lock again, and
    /// forgets a build once its lock is free. Repeats incomplete scans before
    /// allowing shared-state writes.
    pub fn running(&self) -> bool {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(paths) = &state.rescan {
            *state = scan_other_builds(paths);
        }
        state.locks.retain(|lock| {
            !matches!(
                kettle_state::ExclusiveFileLock::try_acquire(lock),
                Ok(Some(_))
            )
        });
        state.rescan.is_some() || !state.locks.is_empty()
    }
}

#[derive(Clone, Debug)]
struct ActivationPaths {
    lock: PathBuf,
    endpoint: String,
    endpoint_dir: Option<PathBuf>,
    /// The registry directory every build's lock lives in.
    base: PathBuf,
    build: BuildKey,
    #[cfg(unix)]
    private_temp: PathBuf,
}

/// Activate an existing primary of this build, or atomically elect this
/// process as its primary.
pub fn activate_or_elect(
    request: ActivationRequest,
    build: BuildKey,
) -> io::Result<ActivationOutcome> {
    if !request.is_valid() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid activation request",
        ));
    }
    let paths = activation_paths(&crate::discovery::registry_dir(), build);
    activate_or_elect_at(request, &paths)
}

fn activate_or_elect_at(
    request: ActivationRequest,
    paths: &ActivationPaths,
) -> io::Result<ActivationOutcome> {
    if let Ok(status) = request_activation(&paths.endpoint, &request) {
        return Ok(outcome_from_status(status));
    }
    if let Some(primary) = try_become_primary(paths, request.identity.clone())? {
        return Ok(ActivationOutcome::Primary(primary));
    }

    // Another process holds the election lock but may still be constructing
    // its listener. Give it a bounded startup window, and take over if it dies
    // before publishing the endpoint.
    let deadline = Instant::now() + PRIMARY_STARTUP_WAIT;
    loop {
        match request_activation(&paths.endpoint, &request) {
            Ok(status) => return Ok(outcome_from_status(status)),
            Err(error) if Instant::now() >= deadline => {
                log_activation_failure(&error);
                return Ok(ActivationOutcome::Standalone);
            }
            Err(_) => {}
        }
        if let Some(primary) = try_become_primary(paths, request.identity.clone())? {
            return Ok(ActivationOutcome::Primary(primary));
        }
        std::thread::sleep(PRIMARY_RETRY_DELAY);
    }
}

fn outcome_from_status(status: ResponseStatus) -> ActivationOutcome {
    match status {
        ResponseStatus::Activated => ActivationOutcome::Activated,
        ResponseStatus::Busy | ResponseStatus::Incompatible => ActivationOutcome::Standalone,
    }
}

fn log_activation_failure(error: &io::Error) {
    log::warn!("primary Kettle activation did not complete: {error}; opening a separate process");
}

fn try_become_primary(
    paths: &ActivationPaths,
    identity: LaunchIdentity,
) -> io::Result<Option<PrimaryHandle>> {
    try_become_primary_with(
        paths,
        identity,
        kettle_state::ExclusiveFileLock::try_acquire,
    )
}

fn try_become_primary_with(
    paths: &ActivationPaths,
    identity: LaunchIdentity,
    mut try_lock: impl FnMut(&Path) -> io::Result<Option<kettle_state::ExclusiveFileLock>>,
) -> io::Result<Option<PrimaryHandle>> {
    let lock_dir = paths.lock.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "activation lock has no parent")
    })?;
    ensure_private_dir(lock_dir)?;
    if let Some(endpoint_dir) = &paths.endpoint_dir {
        ensure_private_dir(endpoint_dir)?;
    }
    // A sweep in another process may remove this lock file between our open
    // and our lock; the next open finds the current file.
    let mut attempts = 0;
    let lock = loop {
        match try_lock(&paths.lock) {
            Ok(Some(lock)) => break lock,
            Ok(None) => return Ok(None),
            Err(error) if kettle_state::is_retired_lock(&error) && attempts < 3 => attempts += 1,
            Err(error) => return Err(error),
        }
    };
    let listener = CtlListener::bind(&paths.endpoint)?;
    let other_builds = sweep_other_builds(paths);
    Ok(Some(PrimaryHandle {
        primary: Arc::new(Mutex::new(Some(Primary {
            listener,
            _lock: lock,
            identity,
        }))),
        other_builds,
    }))
}

/// How many other builds' lock files one sweep handles. Every name in the
/// private registry directory is read, but only lock files count, so other
/// entries cannot keep a sweep from ever completing.
const MAX_SWEEP_ENTRIES: usize = 256;

/// Removes the activation files of builds that are no longer running and
/// returns the ones that are. Older Kettle versions used one unkeyed
/// `activation.lock` for every build; it is swept the same way once its
/// primary exits.
///
/// Whether a build is running is decided by its lock alone, never by name or
/// age: a lock this process can take has no primary, because a primary holds
/// its lock for its whole life and binds its endpoint only while holding it.
/// So any endpoint removed here is dead. The lock file is removed while still
/// held; a launcher that opened it just before then finds it retired and
/// retries.
fn sweep_other_builds(paths: &ActivationPaths) -> OtherBuilds {
    OtherBuilds(Arc::new(Mutex::new(scan_other_builds(paths))))
}

fn scan_other_builds(paths: &ActivationPaths) -> OtherBuildState {
    let mut state = OtherBuildState {
        locks: Vec::new(),
        rescan: Some(paths.clone()),
    };
    let entries = match std::fs::read_dir(&paths.base) {
        Ok(entries) => entries,
        // No activation directory: no other build ever started here.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            state.rescan = None;
            return state;
        }
        Err(_) => return state,
    };
    let mut incomplete = false;
    let mut locks_seen = 0;
    for entry in entries {
        let Ok(entry) = entry else {
            incomplete = true;
            continue;
        };
        let name = entry.file_name();
        let Some(build) = name.to_str().and_then(other_build_of_lock) else {
            continue;
        };
        if build == Some(paths.build) {
            continue;
        }
        locks_seen += 1;
        if locks_seen > MAX_SWEEP_ENTRIES {
            incomplete = true;
            break;
        }
        let lock_path = entry.path();
        // Only regular files: never follow a symlink someone left here.
        match std::fs::symlink_metadata(&lock_path) {
            Ok(meta) if meta.file_type().is_file() => {}
            Ok(_) => continue,
            Err(_) => {
                incomplete = true;
                continue;
            }
        }
        match kettle_state::ExclusiveFileLock::try_acquire(&lock_path) {
            Ok(None) => state.locks.push(lock_path),
            Ok(Some(lock)) => {
                #[cfg(unix)]
                for endpoint in other_build_endpoints(paths, build) {
                    remove_own_socket(&endpoint);
                }
                // Windows pipes vanish with their process, so there only the
                // lock file remains to remove.
                let _ = lock.remove(&lock_path);
            }
            Err(error) => {
                incomplete = true;
                log::debug!("activation sweep skipped {}: {error}", lock_path.display());
            }
        }
    }
    if !incomplete {
        state.rescan = None;
    }
    state
}

/// The build an activation lock file belongs to: `Some(None)` for the
/// unkeyed lock older versions used, `None` for anything else.
fn other_build_of_lock(name: &str) -> Option<Option<BuildKey>> {
    if name == "activation.lock" {
        return Some(None);
    }
    let key = name.strip_prefix("activation-")?.strip_suffix(".lock")?;
    BuildKey::new(key).map(Some)
}

/// Where another build's endpoint is, or was: in the registry directory, or
/// in the private temporary directory when that path was too long.
#[cfg(unix)]
fn other_build_endpoints(paths: &ActivationPaths, build: Option<BuildKey>) -> [PathBuf; 2] {
    match build {
        Some(build) => [
            paths.base.join(keyed_socket_name(build)),
            length_safe_unix_socket_path(
                &fallback_socket_name(&paths.base, Some(build)),
                &paths.private_temp,
            ),
        ],
        None => [
            paths.base.join("activation.sock"),
            length_safe_unix_socket_path(
                &fallback_socket_name(&paths.base, None),
                &paths.private_temp,
            ),
        ],
    }
}

/// Removes `path` only if it is a socket this user owns.
#[cfg(unix)]
fn remove_own_socket(path: &Path) {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_socket() && meta.uid() == unsafe { libc::geteuid() } {
        let _ = std::fs::remove_file(path);
    }
}

/// Start the primary accept loop. The handler runs once per compatible request
/// and returns true only after the UI confirms that the window opened.
///
/// The spawned thread owns both the listener and election lock for the process
/// lifetime. A failed thread spawn drops both, allowing a later process to
/// become primary instead of leaving a dead endpoint advertised.
pub fn spawn_server(
    handle: PrimaryHandle,
    handler: impl Fn(ActivationRequest) -> bool + Send + Sync + 'static,
) -> io::Result<()> {
    spawn_server_inner(handle, Arc::new(handler), Arc::new(|| {}))
}

fn spawn_server_inner(
    handle: PrimaryHandle,
    handler: Arc<dyn Fn(ActivationRequest) -> bool + Send + Sync>,
    worker_started: Arc<dyn Fn() + Send + Sync>,
) -> io::Result<()> {
    let primary = handle
        .primary
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "primary already consumed"))?;
    std::thread::Builder::new()
        .name("kettle-activation".to_string())
        .spawn(move || server_loop(primary, handler, worker_started))?;
    Ok(())
}

/// What the primary remembers about one launch key.
struct LaunchRecord {
    launch_id: String,
    /// `None` while the owning connection is still inside the handler.
    status: Option<ResponseStatus>,
    /// When the record was opened, refreshed when it settles — only settled
    /// records expire.
    at: Instant,
}

/// The primary's memory of launches it has already acted on, so a retry of a
/// launch whose response was lost is answered from the record instead of
/// opening a second window for the same click.
#[derive(Default)]
struct LaunchLedger {
    records: Mutex<Vec<LaunchRecord>>,
    settled: Condvar,
}

/// The outcome of asking the ledger for permission to run the handler.
enum LaunchClaim<'ledger> {
    /// First time this launch is seen: run the handler and record what it did.
    Owner(LaunchGuard<'ledger>),
    /// Already acted on — reuse the recorded outcome.
    Settled(ResponseStatus),
    /// The ledger is full of launches that are all still in flight, so this one
    /// cannot be remembered. Run it unrecorded rather than evict a record whose
    /// owner is still inside the handler — forgetting that one would let *its*
    /// retry open a second window, trading this launch's duplicate protection
    /// for another launch's.
    Unrecorded,
    /// Another connection owns it and did not finish while we waited.
    Undecided,
}

/// Held by the connection that owns a launch; releases the record if the
/// handler never produces an outcome.
struct LaunchGuard<'ledger> {
    ledger: &'ledger LaunchLedger,
    launch_id: String,
    settled: bool,
}

impl LaunchGuard<'_> {
    fn settle(mut self, status: ResponseStatus) {
        self.ledger.settle(&self.launch_id, status);
        self.settled = true;
    }
}

impl Drop for LaunchGuard<'_> {
    fn drop(&mut self) {
        if !self.settled {
            // The owner never reached an outcome (a panicking handler). Forget
            // the launch so a retry may try again, rather than let it inherit
            // a decision that was never made.
            self.ledger.forget(&self.launch_id);
        }
    }
}

impl LaunchLedger {
    fn records(&self) -> std::sync::MutexGuard<'_, Vec<LaunchRecord>> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Claim `launch_id` for this connection, waiting up to `wait` for an
    /// outcome when another connection already owns it.
    fn claim(&self, launch_id: &str, wait: Duration) -> LaunchClaim<'_> {
        let deadline = Instant::now() + wait;
        let mut records = self.records();
        loop {
            records.retain(|record| {
                record.status.is_none() || record.at.elapsed() < LAUNCH_RECORD_TTL
            });
            let Some(existing) = records.iter().find(|record| record.launch_id == launch_id) else {
                if records.len() >= MAX_REMEMBERED_LAUNCHES {
                    // Evict the oldest settled record. In-flight records must
                    // survive, or a duplicate could re-enter the handler. With
                    // nothing settled to drop, this launch goes unremembered
                    // instead. The concurrent-client cap keeps that unreachable
                    // today. If it ever happens, the new launch must be the one
                    // to lose duplicate protection, never an older launch.
                    let Some(victim) = records.iter().position(|record| record.status.is_some())
                    else {
                        return LaunchClaim::Unrecorded;
                    };
                    records.remove(victim);
                }
                records.push(LaunchRecord {
                    launch_id: launch_id.to_string(),
                    status: None,
                    at: Instant::now(),
                });
                return LaunchClaim::Owner(LaunchGuard {
                    ledger: self,
                    launch_id: launch_id.to_string(),
                    settled: false,
                });
            };
            if let Some(status) = existing.status {
                return LaunchClaim::Settled(status);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return LaunchClaim::Undecided;
            }
            records = self
                .settled
                .wait_timeout(records, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn settle(&self, launch_id: &str, status: ResponseStatus) {
        let mut records = self.records();
        if let Some(record) = records
            .iter_mut()
            .find(|record| record.launch_id == launch_id)
        {
            record.status = Some(status);
            record.at = Instant::now();
        }
        drop(records);
        self.settled.notify_all();
    }

    fn forget(&self, launch_id: &str) {
        let mut records = self.records();
        records.retain(|record| record.launch_id != launch_id);
        drop(records);
        self.settled.notify_all();
    }
}

fn server_loop(
    primary: Primary,
    handler: Arc<dyn Fn(ActivationRequest) -> bool + Send + Sync>,
    worker_started: Arc<dyn Fn() + Send + Sync>,
) {
    server_loop_until(primary, handler, worker_started, None);
}

fn server_loop_until(
    primary: Primary,
    handler: Arc<dyn Fn(ActivationRequest) -> bool + Send + Sync>,
    worker_started: Arc<dyn Fn() + Send + Sync>,
    stop: Option<&std::sync::atomic::AtomicBool>,
) {
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ledger = Arc::new(LaunchLedger::default());
    let mut consecutive_errors = 0u32;
    loop {
        if stop.is_some_and(|stop| stop.load(std::sync::atomic::Ordering::Acquire)) {
            break;
        }
        let stream = match primary.listener.accept() {
            Ok(stream) => {
                consecutive_errors = 0;
                stream
            }
            Err(error) => {
                consecutive_errors += 1;
                if consecutive_errors > 32 {
                    log::warn!(
                        "Kettle activation accept loop ending after repeated errors: {error}"
                    );
                    return;
                }
                log::debug!("Kettle activation accept failed transiently: {error}");
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
        };
        if stop.is_some_and(|stop| stop.load(std::sync::atomic::Ordering::Acquire)) {
            break;
        }
        match stream.peer_is_same_user() {
            Ok(true) => {}
            Ok(false) => {
                log::warn!("refusing Kettle activation from another user");
                continue;
            }
            Err(error) => {
                log::warn!("cannot verify Kettle activation peer: {error}");
                continue;
            }
        }
        if active.load(std::sync::atomic::Ordering::Relaxed) >= MAX_ACTIVE_CLIENTS {
            log::warn!("Kettle activation client cap ({MAX_ACTIVE_CLIENTS}) reached");
            continue;
        }
        active.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let identity = primary.identity.clone();
        let handler = handler.clone();
        let worker_started = worker_started.clone();
        let active_for_worker = active.clone();
        let ledger = ledger.clone();
        let spawn = std::thread::Builder::new()
            .name("kettle-activation-client".to_string())
            .spawn(move || {
                worker_started();
                handle_client(stream, &identity, &ledger, handler.as_ref());
                active_for_worker.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            });
        if let Err(error) = spawn {
            active.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            log::warn!("cannot spawn Kettle activation worker: {error}");
        }
    }
}

fn handle_client(
    mut stream: CtlStream,
    identity: &LaunchIdentity,
    ledger: &LaunchLedger,
    handler: &(dyn Fn(ActivationRequest) -> bool + Send + Sync),
) {
    let request = match read_json_frame::<ActivationRequest>(&mut stream, IO_TIMEOUT) {
        Ok(request) if request.is_valid() => request,
        Ok(_) => return,
        Err(error) => {
            log::warn!("invalid Kettle activation request: {error}");
            return;
        }
    };
    let status = if request.identity != *identity {
        // An incompatible launch never reaches the handler, so re-deciding it
        // for a retry costs nothing and can open nothing.
        ResponseStatus::Incompatible
    } else {
        activate_once(request, ledger, handler)
    };
    let response = ActivationResponse {
        v: PROTOCOL_VERSION,
        status,
    };
    if let Err(error) = write_json_frame(&mut stream, &response, IO_TIMEOUT) {
        log::warn!("Kettle activation response failed: {error}");
    }
}

/// Run the handler for a launch at most once, however many times that launch's
/// request arrives.
///
/// The window opens before the response is written, so a retry that follows a
/// lost or slow response must not repeat the work — it must learn what the
/// first attempt did. A retry that arrives while the first attempt is still
/// inside the handler (the cold-start case that makes a secondary give up)
/// waits for that outcome rather than racing it.
fn activate_once(
    request: ActivationRequest,
    ledger: &LaunchLedger,
    handler: &(dyn Fn(ActivationRequest) -> bool + Send + Sync),
) -> ResponseStatus {
    let run = |request| {
        if handler(request) {
            ResponseStatus::Activated
        } else {
            ResponseStatus::Busy
        }
    };
    let Some(launch_id) = request.launch_id().map(str::to_string) else {
        // A launch from a build that predates the key can only be handled
        // at-least-once; nothing here can recognize its retry.
        return run(request);
    };
    match ledger.claim(&launch_id, LAUNCH_JOIN_WAIT) {
        LaunchClaim::Owner(guard) => {
            let status = run(request);
            guard.settle(status);
            status
        }
        LaunchClaim::Settled(status) => status,
        LaunchClaim::Unrecorded => run(request),
        // The first attempt is still inside the handler and this wait is up.
        //
        // `LAUNCH_JOIN_WAIT` (2.5 s) is deliberately shorter than the handler's
        // bound, the UI's 5 s `UI_CONFIRM_TIMEOUT`, because the requester's read
        // deadline bounds it, not the handler (see the static assertion on
        // `LAUNCH_JOIN_WAIT`). Waiting the handler out would produce an answer
        // nobody is left to read.
        //
        // So `Busy` here means "no answer in time", not "the owner has certainly
        // failed". It sends the launcher down its "carry on in your own process"
        // path, so the click still opens a window. Running the handler again
        // could not promise that, because this launch's outcome is the owner's
        // to produce.
        //
        // The cost: a duplicate arriving 2.5 s to 5 s into the owner's handler
        // gets `Busy` and opens a second window, where a full 5 s wait would have
        // inherited the owner's success. That band is hard to reach, since a
        // retry follows the previous attempt's 5 s read timeout, by which point
        // the 5 s-bounded handler has settled. A longer wait would answer after
        // the requester gave up, opening a second window anyway and taking twice
        // as long.
        LaunchClaim::Undecided => ResponseStatus::Busy,
    }
}

fn request_activation(endpoint: &str, request: &ActivationRequest) -> io::Result<ResponseStatus> {
    let mut stream = transport::connect(endpoint)?;
    write_json_frame(&mut stream, request, IO_TIMEOUT)?;
    let response = read_json_frame::<ActivationResponse>(&mut stream, IO_TIMEOUT)?;
    if response.v != PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported activation response version",
        ));
    }
    Ok(response.status)
}

fn write_json_frame<T: Serialize>(
    stream: &mut CtlStream,
    value: &T,
    timeout: Duration,
) -> io::Result<()> {
    let mut bytes = match crate::protocol::to_json_vec_bounded(value, MAX_FRAME_BYTES - 1) {
        Ok(bytes) => bytes,
        Err(crate::protocol::BoundedJsonError::Limit { .. }) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "activation frame exceeds 8 KiB",
            ));
        }
        Err(crate::protocol::BoundedJsonError::Serialize(error)) => {
            return Err(io::Error::other(error));
        }
    };
    bytes.push(b'\n');
    stream.write_all_until(&bytes, Instant::now() + timeout, None)
}

fn read_json_frame<T: for<'de> Deserialize<'de>>(
    stream: &mut CtlStream,
    timeout: Duration,
) -> io::Result<T> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::with_capacity(512);
    let mut scan_offset = 0;
    let mut chunk = [0u8; 1024];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || !stream.wait_readable(remaining)? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "activation frame timed out",
            ));
        }
        let remaining = (MAX_FRAME_BYTES + 1).saturating_sub(bytes.len());
        if remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "activation frame exceeds 8 KiB",
            ));
        }
        let read_len = remaining.min(chunk.len());
        let read = stream.read(&mut chunk[..read_len])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "activation peer closed before a complete frame",
            ));
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "activation frame exceeds 8 KiB",
            ));
        }
        if let Some(newline) = crate::protocol::find_newline(&bytes, &mut scan_offset) {
            if newline + 1 != bytes.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "activation connection sent more than one frame",
                ));
            }
            return serde_json::from_slice(&bytes[..newline]).map_err(io::Error::other);
        }
    }
}

fn activation_paths(base: &Path, build: BuildKey) -> ActivationPaths {
    #[cfg(unix)]
    {
        activation_paths_with_temp(base, &private_temp_socket_dir(), build)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        let hash = stable_hash(base.as_os_str().encode_wide().flat_map(u16::to_le_bytes));
        ActivationPaths {
            lock: base.join(keyed_lock_name(build)),
            endpoint: format!(r"\\.\pipe\kettle-activation-{hash:016x}-{}", build.as_str()),
            endpoint_dir: None,
            base: base.to_path_buf(),
            build,
        }
    }
}

fn keyed_lock_name(build: BuildKey) -> String {
    format!("activation-{}.lock", build.as_str())
}

#[cfg(unix)]
fn keyed_socket_name(build: BuildKey) -> String {
    format!("activation-{}.sock", build.as_str())
}

/// The endpoint's name in the private temporary directory when the registry
/// path is too long for a socket. A fixed 32 bytes, so it fits under macOS's
/// long per-user `$TMPDIR`. `None` is the unkeyed name older versions used.
#[cfg(unix)]
fn fallback_socket_name(base: &Path, build: Option<BuildKey>) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let base_bytes = base.as_os_str().as_bytes().iter().copied();
    let hash = match build {
        Some(build) => stable_hash(
            base_bytes
                .chain(std::iter::once(0))
                .chain(build.as_str().bytes()),
        ),
        None => stable_hash(base_bytes),
    };
    format!("activation-{hash:016x}.sock")
}

#[cfg(unix)]
fn activation_paths_with_temp(
    base: &Path,
    private_temp_dir: &Path,
    build: BuildKey,
) -> ActivationPaths {
    let direct = base.join(keyed_socket_name(build));
    let endpoint = if unix_socket_path_fits(&direct) {
        direct
    } else {
        length_safe_unix_socket_path(&fallback_socket_name(base, Some(build)), private_temp_dir)
    };
    let endpoint_dir = endpoint.parent().map(Path::to_path_buf);
    ActivationPaths {
        lock: base.join(keyed_lock_name(build)),
        endpoint: endpoint.to_string_lossy().into_owned(),
        endpoint_dir,
        base: base.to_path_buf(),
        build,
        private_temp: private_temp_dir.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_accept_errors_back_off_and_eventually_release_primary() {
        let source = kettle_test_support::production_source(include_str!("activation.rs"));
        let body = source
            .split("fn server_loop_until(")
            .nth(1)
            .and_then(|rest| rest.split("fn handle_client(").next())
            .expect("activation accept loop");
        for needle in [
            "consecutive_errors",
            "Duration::from_millis(20)",
            "if consecutive_errors > 32",
        ] {
            assert!(body.contains(needle), "accept loop lost {needle:?}");
        }
    }

    fn request(recording_key: Option<&str>) -> ActivationRequest {
        ActivationRequest::new(
            None,
            LaunchIdentity {
                recording_key: recording_key.map(str::to_string),
                record_raw_input: false,
            },
        )
    }

    /// A scratch directory that removes itself.
    ///
    /// `PrivateTempDir` owns a `TempDir`, so the directory goes away when the
    /// returned guard drops instead of leaking one per test run. Callers that
    /// name it `_dir` keep it alive for the test body.
    fn test_paths(label: &str) -> (kettle_test_support::PrivateTempDir, ActivationPaths) {
        let dir = kettle_test_support::private_tempdir(&format!("kettle-activation-{label}-"));
        let paths = activation_paths(dir.path(), test_build());
        (dir, paths)
    }

    fn test_build() -> BuildKey {
        BuildKey::new("0123456789abcdef").unwrap()
    }

    struct TestServer {
        stop: Arc<std::sync::atomic::AtomicBool>,
        endpoint: String,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Release);
            // The accept loop is deliberately blocking in production. A local
            // connection is its platform-neutral wakeup for test teardown; the
            // stop check runs before that stream is dispatched to a worker.
            let _ = transport::connect(&self.endpoint);
            if let Some(thread) = self.thread.take() {
                thread.join().expect("activation test server panicked");
            }
        }
    }

    fn spawn_test_server(
        handle: PrimaryHandle,
        endpoint: &str,
        handler: impl Fn(ActivationRequest) -> bool + Send + Sync + 'static,
    ) -> io::Result<TestServer> {
        spawn_test_server_inner(handle, endpoint, Arc::new(handler), Arc::new(|| {}))
    }

    fn spawn_test_server_inner(
        handle: PrimaryHandle,
        endpoint: &str,
        handler: Arc<dyn Fn(ActivationRequest) -> bool + Send + Sync>,
        worker_started: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<TestServer> {
        let primary = handle
            .primary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::AlreadyExists, "primary already consumed")
            })?;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let thread = std::thread::Builder::new()
            .name("kettle-activation-test".to_string())
            .spawn(move || {
                server_loop_until(
                    primary,
                    handler,
                    worker_started,
                    Some(stop_for_thread.as_ref()),
                )
            })?;
        Ok(TestServer {
            stop,
            endpoint: endpoint.to_string(),
            thread: Some(thread),
        })
    }

    /// Both halves of the key's contract: launches never share one, and a
    /// retry — the same request value sent again — keeps the one it has.
    #[test]
    fn each_launch_mints_its_own_key_and_carries_it_over_the_wire() {
        let launch = request(None);
        assert!(launch.launch_id().is_some());
        assert_ne!(
            launch.launch_id(),
            request(None).launch_id(),
            "two launches must not share an idempotency key"
        );

        let parsed: ActivationRequest =
            serde_json::from_slice(&serde_json::to_vec(&launch).unwrap()).unwrap();
        assert_eq!(parsed.launch_id(), launch.launch_id());
        assert!(parsed.is_valid());

        // A launch from a build that predates the key still activates; it is
        // simply not recognizable as a retry.
        let legacy: ActivationRequest = serde_json::from_str(
            r#"{"v":1,"action":"open_window","identity":{"record_raw_input":false}}"#,
        )
        .unwrap();
        assert_eq!(legacy.launch_id(), None);
        assert!(legacy.is_valid());
    }

    #[test]
    fn request_validation_bounds_security_fields() {
        assert!(request(Some("dir:0123456789abcdef")).is_valid());
        let mut hostile_key = request(None);
        hostile_key.launch_id = Some("../../etc".to_string());
        assert!(!hostile_key.is_valid());
        hostile_key.launch_id = Some(String::new());
        assert!(!hostile_key.is_valid());
        hostile_key.launch_id = Some("a".repeat(MAX_LAUNCH_ID_BYTES + 1));
        assert!(!hostile_key.is_valid());
        assert!(
            !ActivationRequest::new(Some("relative".to_string()), LaunchIdentity::default())
                .is_valid()
        );
        assert!(
            !ActivationRequest::new(
                None,
                LaunchIdentity {
                    recording_key: None,
                    record_raw_input: true,
                }
            )
            .is_valid()
        );
        assert!(!request(Some("bad/path")).is_valid());
    }

    #[test]
    fn primary_round_trip_activates_matching_launch() {
        let (dir, paths) = test_paths("matching");
        let first = activate_or_elect_at(request(Some("dir:aaaa")), &paths).unwrap();
        let ActivationOutcome::Primary(primary) = first else {
            panic!("first launch must become primary");
        };
        let server = spawn_test_server(primary, &paths.endpoint, |_| true).unwrap();
        let second = activate_or_elect_at(request(Some("dir:aaaa")), &paths).unwrap();
        assert!(matches!(second, ActivationOutcome::Activated));
        drop(server);
        drop(dir);
    }

    #[test]
    fn native_reopen_round_trip_preserves_focus_action_and_identity() {
        let (_dir, paths) = test_paths("reopen");
        let identity = LaunchIdentity {
            recording_key: Some("dir:aaaa".into()),
            record_raw_input: true,
        };
        let request = ActivationRequest::for_reopen(None, identity);
        assert!(request.is_valid());
        assert!(request.focuses_existing());
        let ActivationOutcome::Primary(primary) =
            activate_or_elect_at(request.clone(), &paths).unwrap()
        else {
            panic!("first reopen elects a primary");
        };
        let server = spawn_test_server(primary, &paths.endpoint, |received| {
            assert!(received.focuses_existing());
            assert!(received.requires_recording());
            true
        })
        .unwrap();
        assert!(matches!(
            activate_or_elect_at(request, &paths).unwrap(),
            ActivationOutcome::Activated
        ));
        drop(server);
    }

    /// What the guard can and cannot clean, asserted rather than assumed.
    ///
    /// With nothing holding the directory open it goes on every platform, which
    /// covers most callers here.
    #[test]
    fn a_guard_removes_its_scratch_directory_when_nothing_holds_it_open() {
        let (dir, paths) = test_paths("cleanup-idle");
        let outcome = activate_or_elect_at(request(Some("dir:aaaa")), &paths).unwrap();
        assert!(matches!(outcome, ActivationOutcome::Primary(_)));
        // The elected primary owns the lock; it has to close before the guard.
        drop(outcome);

        let path = dir.path().to_path_buf();
        drop(dir);
        assert!(
            !path.exists(),
            "the scratch directory outlived its guard at {}",
            path.display()
        );
    }

    /// The test server must close the election lock before its scratch guard
    /// drops. Windows cannot remove an open lock file, so a detached
    /// process-lifetime thread would leak one directory per test run there.
    #[test]
    fn a_stopped_test_server_releases_its_scratch_directory() {
        let (dir, paths) = test_paths("cleanup-stopped");
        let outcome = activate_or_elect_at(request(Some("dir:aaaa")), &paths).unwrap();
        let ActivationOutcome::Primary(primary) = outcome else {
            panic!("the first launch must become primary");
        };
        let server = spawn_test_server(primary, &paths.endpoint, |_| true).unwrap();

        let path = dir.path().to_path_buf();
        drop(server);
        drop(dir);
        assert!(
            !path.exists(),
            "the stopped activation server kept its scratch directory pinned: {}",
            path.display()
        );
    }

    #[test]
    fn incompatible_or_rejected_launch_falls_back_to_standalone() {
        let (_dir, paths) = test_paths("incompatible");
        let first = activate_or_elect_at(request(Some("dir:aaaa")), &paths).unwrap();
        let ActivationOutcome::Primary(primary) = first else {
            panic!("first launch must become primary");
        };
        let _server = spawn_test_server(primary, &paths.endpoint, |_| false).unwrap();
        assert!(matches!(
            activate_or_elect_at(request(Some("dir:bbbb")), &paths).unwrap(),
            ActivationOutcome::Standalone
        ));
        assert!(matches!(
            activate_or_elect_at(request(Some("dir:aaaa")), &paths).unwrap(),
            ActivationOutcome::Standalone
        ));
    }

    /// Activation is at-least-once: the window opens before the response is
    /// written, so a lost or slow response makes the secondary send the very
    /// same request again. The primary must recognize it instead of opening a
    /// second window for one launcher click.
    #[test]
    fn a_retried_launch_opens_one_window_while_a_new_launch_still_opens_its_own() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (_dir, paths) = test_paths("retry-idempotent");
        let ActivationOutcome::Primary(primary) =
            activate_or_elect_at(request(None), &paths).unwrap()
        else {
            panic!("first launch must become primary");
        };
        let opened = Arc::new(AtomicUsize::new(0));
        let counter = opened.clone();
        let _server = spawn_test_server(primary, &paths.endpoint, move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
            true
        })
        .unwrap();

        let launch = request(None);
        assert_eq!(
            request_activation(&paths.endpoint, &launch).unwrap(),
            ResponseStatus::Activated
        );
        assert_eq!(
            request_activation(&paths.endpoint, &launch).unwrap(),
            ResponseStatus::Activated,
            "a retry is still answered, from the recorded outcome"
        );
        assert_eq!(
            opened.load(Ordering::Relaxed),
            1,
            "the retried launch opened a second window"
        );

        assert_eq!(
            request_activation(&paths.endpoint, &request(None)).unwrap(),
            ResponseStatus::Activated
        );
        assert_eq!(
            opened.load(Ordering::Relaxed),
            2,
            "a genuinely separate launch must still open its own window"
        );
    }

    /// The cold-start shape of the same bug: the retry arrives while the first
    /// attempt is still inside the handler, which is exactly why the secondary
    /// gave up on it. It must wait for that attempt's outcome, not race it.
    #[test]
    fn a_retry_arriving_mid_handler_waits_for_the_first_attempt() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (_dir, paths) = test_paths("retry-in-flight");
        let ActivationOutcome::Primary(primary) =
            activate_or_elect_at(request(None), &paths).unwrap()
        else {
            panic!("first launch must become primary");
        };
        let opened = Arc::new(AtomicUsize::new(0));
        let counter = opened.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let _server = spawn_test_server(primary, &paths.endpoint, move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
            let _ = entered_tx.send(());
            let _ = release_rx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv_timeout(Duration::from_secs(5));
            true
        })
        .unwrap();

        let launch = request(None);
        let first = {
            let (endpoint, launch) = (paths.endpoint.clone(), launch.clone());
            std::thread::spawn(move || request_activation(&endpoint, &launch).unwrap())
        };
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the first attempt reached the handler");

        let retry = {
            let (endpoint, launch) = (paths.endpoint.clone(), launch.clone());
            std::thread::spawn(move || request_activation(&endpoint, &launch).unwrap())
        };
        // Long enough for the retry to be accepted and consult the ledger. It
        // must be parked there, not opening a window of its own.
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            opened.load(Ordering::Relaxed),
            1,
            "the retry entered the handler while the first attempt was still inside it"
        );

        release_tx.send(()).expect("release the handler");
        assert_eq!(first.join().unwrap(), ResponseStatus::Activated);
        assert_eq!(
            retry.join().unwrap(),
            ResponseStatus::Activated,
            "the retry must inherit the first attempt's outcome"
        );
        assert_eq!(opened.load(Ordering::Relaxed), 1);
    }

    /// The wait a duplicate spends in the ledger is only worth spending if its
    /// answer can still be delivered. The requester sent the request first, so
    /// its one-`IO_TIMEOUT` read deadline started *before* the wait did. A wait
    /// that runs to the same bound writes a status into a socket nobody is
    /// reading, and the launch has waited the whole time for nothing.
    #[test]
    fn a_duplicate_of_a_stuck_launch_is_answered_before_its_requester_gives_up() {
        assert!(
            LAUNCH_JOIN_WAIT < IO_TIMEOUT,
            "the ledger must give up waiting before the requester gives up reading"
        );
        let (_dir, paths) = test_paths("duplicate-answered");
        let ActivationOutcome::Primary(primary) =
            activate_or_elect_at(request(None), &paths).unwrap()
        else {
            panic!("first launch must become primary");
        };
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let _server = spawn_test_server(primary, &paths.endpoint, move |_| {
            let _ = entered_tx.send(());
            let _ = release_rx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv_timeout(IO_TIMEOUT * 4);
            true
        })
        .unwrap();

        let launch = request(None);
        let first = {
            let (endpoint, launch) = (paths.endpoint.clone(), launch.clone());
            std::thread::spawn(move || request_activation(&endpoint, &launch))
        };
        entered_rx
            .recv_timeout(IO_TIMEOUT)
            .expect("the first attempt reached the handler");

        // The duplicate: the owner is parked for longer than the ledger will
        // wait, so this exercises the arm that answers without an outcome.
        let started = Instant::now();
        let duplicate = request_activation(&paths.endpoint, &launch)
            .expect("the duplicate is answered, not left reading a socket nobody writes to");
        assert_eq!(duplicate, ResponseStatus::Busy);
        assert!(
            started.elapsed() < IO_TIMEOUT,
            "and answered inside the requester's own read deadline"
        );

        release_tx.send(()).expect("release the handler");
        assert_eq!(first.join().unwrap().unwrap(), ResponseStatus::Activated);
    }

    /// A full ledger makes room by forgetting a *settled* launch. Forgetting
    /// one that is still inside the handler would let its own retry re-enter
    /// the handler and open the second window this ledger exists to prevent —
    /// trading a new launch's guarantee for an older launch's. The
    /// concurrent-client cap keeps the ledger from filling today; this pins
    /// the rule so raising that cap cannot quietly break it.
    #[test]
    fn a_full_ledger_never_evicts_a_launch_that_is_still_in_flight() {
        let ledger = LaunchLedger::default();
        let ids: Vec<String> = (0..MAX_REMEMBERED_LAUNCHES)
            .map(|n| format!("launch-{n}"))
            .collect();
        let mut guards: Vec<LaunchGuard<'_>> = ids
            .iter()
            .map(|id| match ledger.claim(id, Duration::ZERO) {
                LaunchClaim::Owner(guard) => guard,
                _ => panic!("a launch the ledger has never seen owns its own record"),
            })
            .collect();

        assert!(
            matches!(
                ledger.claim("one-too-many", Duration::ZERO),
                LaunchClaim::Unrecorded
            ),
            "with nothing settled to drop, the new launch goes unremembered"
        );
        assert!(
            matches!(
                ledger.claim(&ids[0], Duration::ZERO),
                LaunchClaim::Undecided
            ),
            "the oldest launch is still owned, so its retry still defers to it"
        );

        // A settled record, by contrast, is exactly what eviction is for.
        guards.remove(0).settle(ResponseStatus::Activated);
        assert!(matches!(
            ledger.claim("room-now", Duration::ZERO),
            LaunchClaim::Owner(_)
        ));
        assert!(
            matches!(
                ledger.claim(&ids[1], Duration::ZERO),
                LaunchClaim::Undecided
            ),
            "and it was the settled record that went, not an in-flight one"
        );
    }

    #[test]
    fn stalled_activation_peer_does_not_block_the_next_launch() {
        let (_dir, paths) = test_paths("concurrent-stalled-peer");
        let first = activate_or_elect_at(request(None), &paths).unwrap();
        let ActivationOutcome::Primary(primary) = first else {
            panic!("first launch must become primary");
        };
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let _server = spawn_test_server_inner(
            primary,
            &paths.endpoint,
            Arc::new(|_| true),
            Arc::new(move || {
                let _ = started_tx.send(());
            }),
        )
        .unwrap();

        let mut stalled = transport::connect(&paths.endpoint).expect("connect stalled peer");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stalled peer reached its own worker");
        stalled
            .write_all_until(b"{", Instant::now() + Duration::from_secs(1), None)
            .expect("send incomplete frame");

        let started = Instant::now();
        assert_eq!(
            request_activation(&paths.endpoint, &request(None)).unwrap(),
            ResponseStatus::Activated
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a stalled peer delayed an independent activation for {:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn election_files_are_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let (dir, paths) = test_paths("permissions");
        let outcome = activate_or_elect_at(request(None), &paths).unwrap();
        assert!(matches!(outcome, ActivationOutcome::Primary(_)));
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&paths.lock).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn activation_fallback_stays_short_when_registry_and_tmpdir_are_long() {
        let base = PathBuf::from("/tmp").join("registry-component".repeat(8));
        let private_temp = PathBuf::from("/tmp")
            .join("an-unusually-long-tmpdir-component".repeat(5))
            .join("kettle-1234");
        assert!(!unix_socket_path_fits(
            &base.join(keyed_socket_name(test_build()))
        ));
        assert!(!unix_socket_path_fits(
            &private_temp.join(fallback_socket_name(&base, Some(test_build())))
        ));

        let paths = activation_paths_with_temp(&base, &private_temp, test_build());
        let endpoint = PathBuf::from(&paths.endpoint);
        assert!(
            unix_socket_path_fits(&endpoint),
            "completed activation endpoint must fit sun_path: {endpoint:?}"
        );
        let fixed = Path::new("/tmp").join(format!("kettle-{}", unsafe { libc::geteuid() }));
        assert!(endpoint.starts_with(&fixed));
        assert_eq!(paths.endpoint_dir.as_deref(), endpoint.parent());
    }

    /// No activation directory means no other build has started, not an
    /// incomplete scan: shared-state writes must not wait on it forever.
    #[test]
    fn a_missing_activation_directory_means_no_other_build() {
        let dir = kettle_test_support::private_tempdir("kettle-activation-missing-");
        let paths = activation_paths(&dir.join("absent"), build('a'));
        assert!(!sweep_other_builds(&paths).running());
    }

    /// Other entries in the registry directory do not count against the
    /// sweep's bound, so many of them cannot keep it from completing.
    #[test]
    fn many_other_entries_do_not_keep_a_sweep_incomplete() {
        let dir = kettle_test_support::private_tempdir("kettle-activation-crowded-");
        ensure_private_dir(&dir).unwrap();
        for index in 0..MAX_SWEEP_ENTRIES + 44 {
            std::fs::write(dir.join(format!("{index}.json")), b"{}").unwrap();
        }
        // A build that has exited leaves its lock file free.
        drop(
            kettle_state::ExclusiveFileLock::try_acquire(&dir.join(keyed_lock_name(build('b'))))
                .unwrap()
                .unwrap(),
        );
        let paths = activation_paths(&dir, build('a'));
        assert!(!sweep_other_builds(&paths).running());
    }

    #[cfg(unix)]
    #[test]
    fn sweep_candidates_include_the_second_temp_fallback() {
        let base = PathBuf::from("/tmp").join("registry-component".repeat(8));
        let private_temp = PathBuf::from("/tmp").join("temp-component".repeat(12));
        let paths = activation_paths_with_temp(&base, &private_temp, build('a'));
        for build in [Some(build('b')), None] {
            let endpoint =
                length_safe_unix_socket_path(&fallback_socket_name(&base, build), &private_temp);
            assert!(
                other_build_endpoints(&paths, build).contains(&endpoint),
                "cleanup must include the endpoint in the second temp fallback"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn sweeping_removes_sockets_in_the_second_temp_fallback() {
        let dir = kettle_test_support::private_tempdir("kettle-activation-second-fallback-");
        let base = dir.join("registry-component".repeat(8));
        let private_temp = dir.join("temp-component".repeat(12));
        ensure_private_dir(&base).unwrap();
        let paths = activation_paths_with_temp(&base, &private_temp, build('a'));
        for build in [Some(build('b')), None] {
            let name = build.map_or_else(
                || "activation.lock".to_string(),
                |build| format!("activation-{}.lock", build.as_str()),
            );
            drop(
                kettle_state::ExclusiveFileLock::try_acquire(&base.join(name))
                    .unwrap()
                    .unwrap(),
            );
            let endpoint =
                length_safe_unix_socket_path(&fallback_socket_name(&base, build), &private_temp);
            ensure_private_dir(endpoint.parent().unwrap()).unwrap();
            drop(std::os::unix::net::UnixListener::bind(&endpoint).unwrap());
            sweep_other_builds(&paths);
            let remains = endpoint.exists();
            let _ = std::fs::remove_file(&endpoint);
            assert!(!remains, "the second-fallback socket must be swept");
        }
    }

    #[cfg(unix)]
    #[test]
    fn activation_measures_non_utf8_paths_after_lossy_conversion() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let mut raw_base = b"/tmp/".to_vec();
        raw_base.extend(std::iter::repeat_n(0xff, 30));
        let base = PathBuf::from(OsString::from_vec(raw_base));
        let direct = base.join(keyed_socket_name(test_build()));
        assert!(direct.as_os_str().as_bytes().len() <= crate::MAX_UNIX_SOCKET_PATH_BYTES);
        assert!(direct.to_string_lossy().len() > crate::MAX_UNIX_SOCKET_PATH_BYTES);
        let paths = activation_paths_with_temp(&base, Path::new("/tmp/kettle-test"), test_build());
        assert!(unix_socket_path_fits(Path::new(&paths.endpoint)));
        assert_ne!(paths.endpoint, direct.to_string_lossy());

        let long_base = PathBuf::from("/tmp").join("registry-component".repeat(8));
        let mut raw_temp = b"/tmp/".to_vec();
        raw_temp.extend(std::iter::repeat_n(0xff, 25));
        let non_utf8_temp = PathBuf::from(OsString::from_vec(raw_temp));
        let unchecked = non_utf8_temp.join("activation-0123456789abcdef.sock");
        assert!(unchecked.as_os_str().as_bytes().len() <= crate::MAX_UNIX_SOCKET_PATH_BYTES);
        assert!(unchecked.to_string_lossy().len() > crate::MAX_UNIX_SOCKET_PATH_BYTES);
        let paths = activation_paths_with_temp(&long_base, &non_utf8_temp, test_build());
        let endpoint = PathBuf::from(&paths.endpoint);
        assert!(unix_socket_path_fits(&endpoint));
        assert!(
            endpoint.starts_with(
                Path::new("/tmp").join(format!("kettle-{}", unsafe { libc::geteuid() }))
            )
        );
    }

    fn build(hex_digit: char) -> BuildKey {
        BuildKey::new(&hex_digit.to_string().repeat(16)).unwrap()
    }

    /// A lock file the sweep cannot open leaves the sweep incomplete: it may
    /// belong to a running build, so shared state waits until a rescan can
    /// settle it.
    #[cfg(unix)]
    #[test]
    fn an_incomplete_sweep_blocks_shared_state_until_it_can_finish() {
        use std::os::unix::fs::PermissionsExt as _;
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped: root opens any file");
            return;
        }
        let dir = kettle_test_support::private_tempdir("kettle-activation-unreadable-");
        let paths = activation_paths(dir.path(), build('a'));
        let lock = dir.join(keyed_lock_name(build('b')));
        std::fs::write(&lock, b"").unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o000)).unwrap();
        let others = sweep_other_builds(&paths);
        assert!(
            others.running(),
            "an incomplete sweep cannot authorize shared-state writes"
        );
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            !others.running(),
            "a complete rescan establishes that no other builds run"
        );
    }

    #[test]
    fn a_build_key_is_sixteen_lowercase_hex_digits() {
        assert!(BuildKey::new("0123456789abcdef").is_some());
        for bad in [
            "0123456789ABCDEF",
            "0123456789abcde",
            "0123456789abcdef0",
            "../activation....",
        ] {
            assert!(BuildKey::new(bad).is_none(), "{bad}");
        }
    }

    /// #575: after an update, a launch of the new build must open in a
    /// process of that build, not in the old build's primary, and without
    /// waiting for it.
    #[test]
    fn a_launch_of_another_build_opens_its_own_primary_without_waiting() {
        let dir = kettle_test_support::private_tempdir("kettle-activation-builds-");
        let old = activation_paths(dir.path(), build('a'));
        let new = activation_paths(dir.path(), build('b'));
        let ActivationOutcome::Primary(old_primary) =
            activate_or_elect_at(request(None), &old).unwrap()
        else {
            panic!("the first launch of the old build is its primary");
        };
        let old_calls = Arc::new(AtomicU64::new(0));
        let counted = old_calls.clone();
        let _old_server = spawn_test_server(old_primary, &old.endpoint, move |_| {
            counted.fetch_add(1, Ordering::Relaxed);
            true
        })
        .unwrap();

        let started = Instant::now();
        let outcome = activate_or_elect_at(request(None), &new).unwrap();
        assert!(
            matches!(outcome, ActivationOutcome::Primary(_)),
            "a launch of the new build opened elsewhere: {outcome:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(old_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn later_launches_join_the_primary_of_their_own_build() {
        let dir = kettle_test_support::private_tempdir("kettle-activation-join-");
        let mut counts = Vec::new();
        let mut servers = Vec::new();
        let mut all_paths = Vec::new();
        for digit in ['a', 'b'] {
            let paths = activation_paths(dir.path(), build(digit));
            let ActivationOutcome::Primary(primary) =
                activate_or_elect_at(request(None), &paths).unwrap()
            else {
                panic!("the first launch of each build is its primary");
            };
            let calls = Arc::new(AtomicU64::new(0));
            let counted = calls.clone();
            servers.push(
                spawn_test_server(primary, &paths.endpoint, move |_| {
                    counted.fetch_add(1, Ordering::Relaxed);
                    true
                })
                .unwrap(),
            );
            counts.push(calls);
            all_paths.push(paths);
        }
        let outcome = activate_or_elect_at(request(None), &all_paths[1]).unwrap();
        assert!(matches!(outcome, ActivationOutcome::Activated));
        assert_eq!(counts[0].load(Ordering::Relaxed), 0);
        assert_eq!(counts[1].load(Ordering::Relaxed), 1);
    }

    /// The sweep decides by lock alone: free locks of other builds, and the
    /// unkeyed one older versions used, go with their sockets; a held lock,
    /// this build's files and anything unrelated stay.
    #[cfg(unix)]
    #[test]
    fn election_removes_endpoints_left_by_builds_no_longer_running() {
        // Socket paths are short-limited, so this registry lives directly in
        // /tmp rather than under a long per-user TMPDIR.
        struct ShortDir(PathBuf);
        impl Drop for ShortDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = ShortDir(PathBuf::from(format!(
            "/tmp/kas-{}-{nanos:x}",
            std::process::id()
        )));
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir.0)
                .unwrap();
        }
        let base = dir.0.as_path();
        let socket = |name: &str| {
            let path = base.join(name);
            // Dropping a bound listener leaves its socket file behind.
            drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
            path
        };
        let lock_file = |name: &str| {
            let path = base.join(name);
            drop(
                kettle_state::ExclusiveFileLock::try_acquire(&path)
                    .unwrap()
                    .unwrap(),
            );
            path
        };
        let legacy = [lock_file("activation.lock"), socket("activation.sock")];
        let stale = [
            lock_file("activation-dddddddddddddddd.lock"),
            socket("activation-dddddddddddddddd.sock"),
        ];
        let held_lock_path = lock_file("activation-eeeeeeeeeeeeeeee.lock");
        let held = kettle_state::ExclusiveFileLock::try_acquire(&held_lock_path)
            .unwrap()
            .unwrap();
        let kept = [
            held_lock_path.clone(),
            socket("activation-eeeeeeeeeeeeeeee.sock"),
            socket("ctl-123.sock"),
            base.join("notes.txt"),
        ];
        std::fs::write(&kept[3], b"not ours").unwrap();

        let paths = activation_paths(base, build('c'));
        let ActivationOutcome::Primary(primary) =
            activate_or_elect_at(request(None), &paths).unwrap()
        else {
            panic!("the launch is this build's primary");
        };
        for path in legacy.iter().chain(&stale) {
            assert!(!path.exists(), "left behind: {path:?}");
        }
        for path in kept.iter().chain([&paths.lock]) {
            assert!(path.exists(), "removed: {path:?}");
        }
        let others = primary.other_builds();
        assert!(others.running());
        drop(held);
        assert!(!others.running());
    }

    /// Unix only: Windows cannot delete a file another handle holds open,
    /// so a lock file there is never retired under a waiter this way.
    #[cfg(unix)]
    #[test]
    fn election_retries_a_lock_retired_under_it() {
        let (_dir, paths) = test_paths("retired");
        let mut calls = 0;
        let primary = try_become_primary_with(&paths, LaunchIdentity::default(), |path| {
            calls += 1;
            if calls == 1 {
                // A real retired-lock error: the path stops naming the file
                // a lock was taken on.
                let lock = kettle_state::ExclusiveFileLock::try_acquire(path)?.unwrap();
                std::fs::remove_file(path)?;
                drop(std::fs::File::create(path)?);
                return Err(lock.remove(path).unwrap_err());
            }
            kettle_state::ExclusiveFileLock::try_acquire(path)
        })
        .unwrap();
        assert!(primary.is_some());
        assert_eq!(calls, 2);
    }

    #[test]
    fn stable_hash_is_deterministic_and_order_sensitive() {
        assert_eq!(stable_hash(b"kettle".iter().copied()), 0x1958d25dd8f8aae2);
        assert_ne!(
            stable_hash(b"kettle".iter().copied()),
            stable_hash(b"elttlek".iter().copied())
        );
    }
}
