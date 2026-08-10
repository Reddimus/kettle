//! Private single-instance activation for bare GUI launches.
//!
//! This is intentionally separate from the opt-in agent control plane. Every
//! bare GUI launch may use this endpoint, but the wire can request exactly one
//! action: open a fresh window in the primary process. Explicit CLI launches
//! bypass it in the binary.

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

/// The only request accepted by the activation endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationRequest {
    v: u32,
    action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    identity: LaunchIdentity,
    /// Idempotency key: one value per *launch*, kept across that launch's
    /// retries. Delivery here is at-least-once — the primary opens the window
    /// before its response is written, so a response lost to a slow cold start
    /// makes the secondary re-send the identical request — and without a key
    /// the primary cannot tell that retry apart from a second launcher click.
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

    pub fn requires_recording(&self) -> bool {
        self.identity.recording_key.is_some()
    }

    fn launch_id(&self) -> Option<&str> {
        self.launch_id.as_deref()
    }

    fn is_valid(&self) -> bool {
        self.v == PROTOCOL_VERSION
            && self.action == "open_window"
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
pub struct PrimaryHandle(Arc<Mutex<Option<Primary>>>);

impl std::fmt::Debug for PrimaryHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PrimaryHandle(..)")
    }
}

#[derive(Clone, Debug)]
struct ActivationPaths {
    lock: PathBuf,
    endpoint: String,
    endpoint_dir: Option<PathBuf>,
}

/// Activate an existing primary or atomically elect this process.
pub fn activate_or_elect(request: ActivationRequest) -> io::Result<ActivationOutcome> {
    if !request.is_valid() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid activation request",
        ));
    }
    let paths = activation_paths(&crate::discovery::registry_dir());
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
    let lock_dir = paths.lock.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "activation lock has no parent")
    })?;
    ensure_private_dir(lock_dir)?;
    if let Some(endpoint_dir) = &paths.endpoint_dir {
        ensure_private_dir(endpoint_dir)?;
    }
    let Some(lock) = kettle_state::ExclusiveFileLock::try_acquire(&paths.lock)? else {
        return Ok(None);
    };
    let listener = CtlListener::bind(&paths.endpoint)?;
    Ok(Some(PrimaryHandle(Arc::new(Mutex::new(Some(Primary {
        listener,
        _lock: lock,
        identity,
    }))))))
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
        .0
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
                    // Oldest settled first; in-flight records must survive or
                    // a duplicate could re-enter the handler. With nothing
                    // settled to drop, this launch goes unremembered instead —
                    // the concurrent-client cap keeps that unreachable today,
                    // and it must stay a lost guarantee for the new launch
                    // rather than a broken one for an older launch.
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
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ledger = Arc::new(LaunchLedger::default());
    loop {
        let stream = match primary.listener.accept() {
            Ok(stream) => stream,
            Err(error) => {
                log::warn!("Kettle activation accept failed: {error}");
                continue;
            }
        };
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
        // The wait is deliberately SHORTER than the handler's own bound —
        // `LAUNCH_JOIN_WAIT` is 2.5 s against the UI's 5 s
        // `UI_CONFIRM_TIMEOUT` — because it is bounded by the requester's read
        // deadline, not by the handler (see the static assertion on
        // `LAUNCH_JOIN_WAIT`). Waiting the handler out would produce an answer
        // nobody is left to read.
        //
        // So this is not "the owner has certainly failed"; it is "no answer
        // can be had in the time available". `Busy` is the launcher's "carry on
        // in your own process" path, so the click still opens a window —
        // which running the handler again here could not promise, because the
        // outcome this launch is waiting on is not ours to produce.
        //
        // The cost is stated plainly: a duplicate that arrives while the owner
        // is between 2.5 s and 5 s into the handler gets `Busy` and opens a
        // second window, where waiting the full 5 s would have let it inherit
        // the owner's success. That band is hard to reach — a retry follows the
        // previous attempt's own 5 s read timeout, by which point the 5 s-bounded
        // handler has settled — and the alternative is an answer that arrives
        // after the requester has given up, which opens a second window anyway
        // and takes twice as long to do it.
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

fn activation_paths(base: &Path) -> ActivationPaths {
    #[cfg(unix)]
    {
        activation_paths_with_temp(base, &private_temp_socket_dir())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        let hash = stable_hash(base.as_os_str().encode_wide().flat_map(u16::to_le_bytes));
        ActivationPaths {
            lock: base.join("activation.lock"),
            endpoint: format!(r"\\.\pipe\kettle-activation-{hash:016x}"),
            endpoint_dir: None,
        }
    }
}

#[cfg(unix)]
fn activation_paths_with_temp(base: &Path, private_temp_dir: &Path) -> ActivationPaths {
    use std::os::unix::ffi::OsStrExt as _;

    let direct = base.join("activation.sock");
    let endpoint = if unix_socket_path_fits(&direct) {
        direct
    } else {
        let hash = stable_hash(base.as_os_str().as_bytes().iter().copied());
        length_safe_unix_socket_path(&format!("activation-{hash:016x}.sock"), private_temp_dir)
    };
    let endpoint_dir = endpoint.parent().map(Path::to_path_buf);
    ActivationPaths {
        lock: base.join("activation.lock"),
        endpoint: endpoint.to_string_lossy().into_owned(),
        endpoint_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    /// This used to build a path from the pid and delete whatever the *previous*
    /// run with that pid and label had left, never its own — so every test run
    /// leaked one directory, and since the pid varies they accumulated without
    /// bound. A sweep of a Windows machine found 148 `kettle*` entries in
    /// `%TEMP%`, most of them from this helper. `PrivateTempDir` owns a
    /// `TempDir`, so the directory goes away when the returned guard drops;
    /// callers that name it `_dir` keep it alive for the test body, which is
    /// what the binding was already doing.
    fn test_paths(label: &str) -> (kettle_test_support::PrivateTempDir, ActivationPaths) {
        let dir = kettle_test_support::private_tempdir(&format!("kettle-activation-{label}-"));
        let paths = activation_paths(dir.path());
        (dir, paths)
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
        spawn_server(primary, |_| true).unwrap();
        let second = activate_or_elect_at(request(Some("dir:aaaa")), &paths).unwrap();
        assert!(matches!(second, ActivationOutcome::Activated));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The leak this helper was rewritten to stop, checked instead of assumed.
    ///
    /// `spawn_server` moves the `Primary` — and with it the open `activation.lock`
    /// handle — into a thread that outlives the test, so the guard drops while a
    /// descriptor is still open. Unix unlinks a file somebody still holds without
    /// complaint. Windows may refuse to remove a directory that still contains an
    /// open handle's name, and `TempDir::drop` discards the error, so the leak
    /// would have moved from `%TEMP%` to `%LOCALAPPDATA%` while looking fixed —
    /// a sweep found 148 of the old ones on a real machine, and nothing here
    /// would have noticed the new ones.
    ///
    /// Asserting the removal, on every platform, is the only version of this
    /// claim worth making.
    #[test]
    fn the_scratch_directory_is_really_gone_once_its_guard_drops() {
        let (dir, paths) = test_paths("cleanup");
        let outcome = activate_or_elect_at(request(Some("dir:aaaa")), &paths).unwrap();
        let ActivationOutcome::Primary(primary) = outcome else {
            panic!("the first launch must become primary");
        };
        spawn_server(primary, |_| true).unwrap();

        let path = dir.path().to_path_buf();
        drop(dir);
        assert!(
            !path.exists(),
            "the scratch directory outlived its guard at {}: the leak moved \
             rather than stopped",
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
        spawn_server(primary, |_| false).unwrap();
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
        spawn_server(primary, move |_| {
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
        spawn_server(primary, move |_| {
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
    /// answer can still be delivered. The requester is reading under a deadline
    /// of one `IO_TIMEOUT` that started *before* the wait did — it had to send
    /// the request first — so a wait that runs to the same bound produces a
    /// status written into a socket nobody is reading, and the launch has
    /// waited the whole time for nothing.
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
        spawn_server(primary, move |_| {
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
        spawn_server_inner(
            primary,
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
        assert!(!unix_socket_path_fits(&base.join("activation.sock")));
        assert!(!unix_socket_path_fits(
            &private_temp.join("activation-0123456789abcdef.sock")
        ));

        let paths = activation_paths_with_temp(&base, &private_temp);
        let endpoint = PathBuf::from(&paths.endpoint);
        assert!(
            unix_socket_path_fits(&endpoint),
            "completed activation endpoint must fit sun_path: {endpoint:?}"
        );
        let fixed = Path::new("/tmp").join(format!("kettle-{}", unsafe { libc::geteuid() }));
        assert!(endpoint.starts_with(&fixed));
        assert_eq!(paths.endpoint_dir.as_deref(), endpoint.parent());
    }

    #[cfg(unix)]
    #[test]
    fn activation_measures_non_utf8_paths_after_lossy_conversion() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let mut raw_base = b"/tmp/".to_vec();
        raw_base.extend(std::iter::repeat_n(0xff, 30));
        let base = PathBuf::from(OsString::from_vec(raw_base));
        let direct = base.join("activation.sock");
        assert!(direct.as_os_str().as_bytes().len() <= crate::MAX_UNIX_SOCKET_PATH_BYTES);
        assert!(direct.to_string_lossy().len() > crate::MAX_UNIX_SOCKET_PATH_BYTES);
        let paths = activation_paths_with_temp(&base, Path::new("/tmp/kettle-test"));
        assert!(unix_socket_path_fits(Path::new(&paths.endpoint)));
        assert_ne!(paths.endpoint, direct.to_string_lossy());

        let long_base = PathBuf::from("/tmp").join("registry-component".repeat(8));
        let mut raw_temp = b"/tmp/".to_vec();
        raw_temp.extend(std::iter::repeat_n(0xff, 25));
        let non_utf8_temp = PathBuf::from(OsString::from_vec(raw_temp));
        let unchecked = non_utf8_temp.join("activation-0123456789abcdef.sock");
        assert!(unchecked.as_os_str().as_bytes().len() <= crate::MAX_UNIX_SOCKET_PATH_BYTES);
        assert!(unchecked.to_string_lossy().len() > crate::MAX_UNIX_SOCKET_PATH_BYTES);
        let paths = activation_paths_with_temp(&long_base, &non_utf8_temp);
        let endpoint = PathBuf::from(&paths.endpoint);
        assert!(unix_socket_path_fits(&endpoint));
        assert!(
            endpoint.starts_with(
                Path::new("/tmp").join(format!("kettle-{}", unsafe { libc::geteuid() }))
            )
        );
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
