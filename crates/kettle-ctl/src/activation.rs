//! Private single-instance activation for bare GUI launches.
//!
//! This is intentionally separate from the opt-in agent control plane. Every
//! bare GUI launch may use this endpoint, but the wire can request exactly one
//! action: open a fresh window in the primary process. Explicit CLI launches
//! bypass it in the binary.

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::transport::{self, CtlListener, CtlStream};

const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME_BYTES: usize = 8 * 1024;
const MAX_CWD_BYTES: usize = 4096;
const MAX_RECORDING_KEY_BYTES: usize = 128;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const PRIMARY_STARTUP_WAIT: Duration = Duration::from_secs(5);
const PRIMARY_RETRY_DELAY: Duration = Duration::from_millis(25);

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
}

impl ActivationRequest {
    pub fn new(cwd: Option<String>, identity: LaunchIdentity) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            action: "open_window".to_string(),
            cwd,
            identity,
        }
    }

    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    pub fn requires_recording(&self) -> bool {
        self.identity.recording_key.is_some()
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
    }
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
    handler: impl Fn(ActivationRequest) -> bool + Send + 'static,
) -> io::Result<()> {
    let primary = handle
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "primary already consumed"))?;
    std::thread::Builder::new()
        .name("kettle-activation".to_string())
        .spawn(move || server_loop(primary, handler))?;
    Ok(())
}

fn server_loop(primary: Primary, handler: impl Fn(ActivationRequest) -> bool) {
    loop {
        let mut stream = match primary.listener.accept() {
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
        let request = match read_json_frame::<ActivationRequest>(&mut stream, IO_TIMEOUT) {
            Ok(request) if request.is_valid() => request,
            Ok(_) => continue,
            Err(error) => {
                log::warn!("invalid Kettle activation request: {error}");
                continue;
            }
        };
        let status = if request.identity != primary.identity {
            ResponseStatus::Incompatible
        } else if handler(request) {
            ResponseStatus::Activated
        } else {
            ResponseStatus::Busy
        };
        let response = ActivationResponse {
            v: PROTOCOL_VERSION,
            status,
        };
        if let Err(error) = write_json_frame(&mut stream, &response, IO_TIMEOUT) {
            log::warn!("Kettle activation response failed: {error}");
        }
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
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "activation frame exceeds 8 KiB",
        ));
    }
    stream.write_all_until(&bytes, Instant::now() + timeout, None)
}

fn read_json_frame<T: for<'de> Deserialize<'de>>(
    stream: &mut CtlStream,
    timeout: Duration,
) -> io::Result<T> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || !stream.wait_readable(remaining)? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "activation frame timed out",
            ));
        }
        let read = stream.read(&mut chunk)?;
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
        if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
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
    let direct_endpoint = base.join("activation.sock");
    #[cfg(unix)]
    let (endpoint, endpoint_dir) = {
        use std::os::unix::ffi::OsStrExt as _;
        if direct_endpoint.as_os_str().as_bytes().len() <= 100 {
            (
                direct_endpoint.to_string_lossy().into_owned(),
                Some(base.to_path_buf()),
            )
        } else {
            let dir = private_temp_activation_dir();
            let hash = stable_hash(base.as_os_str().as_bytes().iter().copied());
            (
                dir.join(format!("activation-{hash:016x}.sock"))
                    .to_string_lossy()
                    .into_owned(),
                Some(dir),
            )
        }
    };
    #[cfg(windows)]
    let (endpoint, endpoint_dir) = {
        use std::os::windows::ffi::OsStrExt as _;
        let hash = stable_hash(base.as_os_str().encode_wide().flat_map(u16::to_le_bytes));
        (format!(r"\\.\pipe\kettle-activation-{hash:016x}"), None)
    };
    ActivationPaths {
        lock: base.join("activation.lock"),
        endpoint,
        endpoint_dir,
    }
}

#[cfg(unix)]
fn private_temp_activation_dir() -> PathBuf {
    std::env::temp_dir().join(format!("kettle-{}", unsafe { libc::geteuid() }))
}

fn stable_hash(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};

        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let metadata = std::fs::symlink_metadata(dir)?;
        if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "activation directory is not owned by the current user",
            ));
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    Ok(())
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

    fn test_paths(label: &str) -> (PathBuf, ActivationPaths) {
        let dir = crate::test_scratch_root().join(format!(
            "kettle-activation-test-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = activation_paths(&dir);
        (dir, paths)
    }

    #[test]
    fn request_validation_bounds_security_fields() {
        assert!(request(Some("dir:0123456789abcdef")).is_valid());
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

    #[test]
    fn stable_hash_is_deterministic_and_order_sensitive() {
        assert_eq!(stable_hash(b"kettle".iter().copied()), 0x1958d25dd8f8aae2);
        assert_ne!(
            stable_hash(b"kettle".iter().copied()),
            stable_hash(b"elttlek".iter().copied())
        );
    }
}
