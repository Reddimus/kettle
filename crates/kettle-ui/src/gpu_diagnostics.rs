//! GPU recovery scheduling and fault-only diagnostics.
//!
//! wgpu callbacks only latch a bounded [`kettle_render::GpuFault`] in memory.
//! The event-loop thread calls this module, keeping filesystem I/O out of
//! driver callbacks and keeping terminal contents out of every record.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kettle_render::{GpuAdapterInfo, GpuFault};
use serde::Serialize;

const SCHEMA_VERSION: u32 = 1;
const MAX_INCIDENT_BYTES: u64 = 256 * 1024;
const RETAINED_INCIDENTS: usize = 10;
const MAX_MESSAGE_CHARS: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryAction {
    Wait(Duration),
    Attempt { attempt_index: u32 },
}

/// Pure device-loss scheduler. `attempt_index` is zero-based and maps directly
/// to `kettle_render::escalation_for_attempt`.
#[derive(Debug, Default)]
pub(crate) struct RecoveryState {
    attempts: u32,
    next_at: Option<Instant>,
}

impl RecoveryState {
    pub(crate) fn poll(&mut self, now: Instant, settle: Duration) -> RecoveryAction {
        let Some(due) = self.next_at else {
            self.next_at = Some(now.checked_add(settle).unwrap_or(now));
            return RecoveryAction::Wait(settle);
        };
        let remaining = due.saturating_duration_since(now);
        if !remaining.is_zero() {
            return RecoveryAction::Wait(remaining);
        }
        RecoveryAction::Attempt {
            attempt_index: self.attempts,
        }
    }

    pub(crate) fn failed(&mut self, now: Instant, backoff: Duration) {
        self.attempts = self.attempts.saturating_add(1);
        self.next_at = Some(now.checked_add(backoff).unwrap_or(now));
    }

    pub(crate) fn recovered(&mut self) {
        self.attempts = 0;
        self.next_at = None;
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AdapterIdentity {
    name: String,
    vendor: u32,
    device: u32,
    kind: String,
    backend: String,
}

impl From<GpuAdapterInfo> for AdapterIdentity {
    fn from(info: GpuAdapterInfo) -> Self {
        Self {
            name: sanitize(&info.name),
            vendor: info.vendor,
            device: info.device,
            kind: info.kind.to_string(),
            backend: info.backend.to_string(),
        }
    }
}

#[derive(Serialize)]
struct FaultIdentity {
    kind: String,
    message: String,
}

impl From<GpuFault> for FaultIdentity {
    fn from(fault: GpuFault) -> Self {
        Self {
            kind: sanitize(&fault.kind),
            message: sanitize(&fault.message),
        }
    }
}

#[derive(Serialize)]
struct DiagnosticEvent<'a> {
    schema_version: u32,
    timestamp_unix_ms: u128,
    kettle_version: &'a str,
    phase: &'a str,
    adapter: &'a AdapterIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    fault: Option<&'a FaultIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    escalation: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
}

pub(crate) struct IncidentLog {
    path: PathBuf,
    file: File,
    bytes_written: u64,
    kettle_version: String,
    initial_adapter: AdapterIdentity,
    capped: bool,
}

impl IncidentLog {
    pub(crate) fn start(
        cache_dir: Option<&Path>,
        kettle_version: &str,
        adapter: GpuAdapterInfo,
        fault: Option<GpuFault>,
    ) -> io::Result<Self> {
        Self::start_at(
            cache_dir,
            unix_millis(),
            std::process::id(),
            kettle_version,
            adapter,
            fault,
        )
    }

    fn start_at(
        cache_dir: Option<&Path>,
        unix_ms: u128,
        pid: u32,
        kettle_version: &str,
        adapter: GpuAdapterInfo,
        fault: Option<GpuFault>,
    ) -> io::Result<Self> {
        let dir = diagnostic_dir(cache_dir);
        std::fs::create_dir_all(&dir)?;
        let (path, file) = create_incident_file(&dir, unix_ms, pid)?;
        prune_incidents(&dir, &path)?;

        let mut log = Self {
            path,
            file,
            bytes_written: 0,
            kettle_version: sanitize(kettle_version),
            initial_adapter: adapter.into(),
            capped: false,
        };
        let fault = fault.map(FaultIdentity::from);
        log.write_event(
            "fault",
            &log.initial_adapter.clone(),
            fault.as_ref(),
            None,
            None,
            None,
            None,
        )?;
        Ok(log)
    }

    pub(crate) fn record_attempt(&mut self, attempt: u32, escalation: &str) -> io::Result<()> {
        let adapter = self.initial_adapter.clone();
        self.write_event(
            "recovery_attempt",
            &adapter,
            None,
            Some(attempt),
            Some(escalation),
            None,
            None,
        )
    }

    pub(crate) fn record_failure(
        &mut self,
        attempt: u32,
        escalation: &str,
        message: &str,
    ) -> io::Result<()> {
        let adapter = self.initial_adapter.clone();
        let message = sanitize(message);
        self.write_event(
            "recovery_failed",
            &adapter,
            None,
            Some(attempt),
            Some(escalation),
            Some("failed"),
            Some(&message),
        )
    }

    pub(crate) fn record_recovered(
        &mut self,
        attempt: u32,
        escalation: &str,
        adapter: GpuAdapterInfo,
        secondary_window_failures: usize,
    ) -> io::Result<()> {
        let adapter = AdapterIdentity::from(adapter);
        let message = (secondary_window_failures != 0).then(|| {
            format!("{secondary_window_failures} secondary window renderer(s) failed to rebind")
        });
        self.write_event(
            "recovered",
            &adapter,
            None,
            Some(attempt),
            Some(escalation),
            Some(if secondary_window_failures == 0 {
                "recovered"
            } else {
                "degraded"
            }),
            message.as_deref(),
        )
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[allow(clippy::too_many_arguments)]
    fn write_event(
        &mut self,
        phase: &str,
        adapter: &AdapterIdentity,
        fault: Option<&FaultIdentity>,
        attempt: Option<u32>,
        escalation: Option<&str>,
        result: Option<&str>,
        message: Option<&str>,
    ) -> io::Result<()> {
        if self.capped {
            return Ok(());
        }
        let event = DiagnosticEvent {
            schema_version: SCHEMA_VERSION,
            timestamp_unix_ms: unix_millis(),
            kettle_version: &self.kettle_version,
            phase,
            adapter,
            fault,
            attempt,
            escalation,
            result,
            message,
        };
        let mut line = serde_json::to_vec(&event).map_err(io::Error::other)?;
        line.push(b'\n');
        if self.bytes_written.saturating_add(line.len() as u64) > MAX_INCIDENT_BYTES {
            self.capped = true;
            return Ok(());
        }
        self.file.write_all(&line)?;
        self.file.flush()?;
        self.bytes_written += line.len() as u64;
        Ok(())
    }
}

fn diagnostic_dir(cache_dir: Option<&Path>) -> PathBuf {
    cache_dir
        .map(|path| path.join("kettle").join("diagnostics"))
        .unwrap_or_else(|| PathBuf::from("kettle-diagnostics"))
}

fn create_incident_file(dir: &Path, unix_ms: u128, pid: u32) -> io::Result<(PathBuf, File)> {
    for suffix in 0..100u8 {
        let suffix = if suffix == 0 {
            String::new()
        } else {
            format!("-{suffix}")
        };
        let path = dir.join(format!("gpu-{unix_ms}-{pid}{suffix}.jsonl"));
        match OpenOptions::new().create_new(true).append(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique GPU diagnostic filename",
    ))
}

fn prune_incidents(dir: &Path, preserve: &Path) -> io::Result<()> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("gpu-") && name.ends_with(".jsonl"))
        })
        .collect();
    files.sort();
    while files.len() > RETAINED_INCIDENTS {
        let index = files.iter().position(|path| path != preserve);
        let Some(index) = index else { break };
        let stale = files.remove(index);
        match std::fs::remove_file(stale) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn sanitize(value: &str) -> String {
    let mut out = String::with_capacity(value.len().min(MAX_MESSAGE_CHARS));
    for ch in value.chars().take(MAX_MESSAGE_CHARS) {
        out.push(if ch.is_control() { ' ' } else { ch });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kettle-gpu-diagnostics-{label}-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn adapter() -> GpuAdapterInfo {
        GpuAdapterInfo {
            name: "Test GPU".to_string(),
            vendor: 0x10de,
            device: 0x1e87,
            kind: "Discrete",
            backend: "DX12",
        }
    }

    #[test]
    fn recovery_state_settles_escalates_and_resets() {
        let base = Instant::now();
        let settle = Duration::from_millis(700);
        let mut state = RecoveryState::default();

        assert_eq!(state.poll(base, settle), RecoveryAction::Wait(settle));
        assert_eq!(
            state.poll(base + Duration::from_millis(699), settle),
            RecoveryAction::Wait(Duration::from_millis(1))
        );
        assert_eq!(
            state.poll(base + settle, settle),
            RecoveryAction::Attempt { attempt_index: 0 }
        );
        state.failed(base + settle, Duration::from_millis(500));
        assert_eq!(
            state.poll(base + Duration::from_millis(1200), settle),
            RecoveryAction::Attempt { attempt_index: 1 }
        );
        state.recovered();
        assert_eq!(
            state.poll(base + Duration::from_secs(2), settle),
            RecoveryAction::Wait(settle)
        );
    }

    #[test]
    fn diagnostic_is_jsonl_bounded_private_and_versioned() {
        let dir = TestDir::new("schema");
        let fault = GpuFault {
            kind: "device_lost".to_string(),
            message: "driver\nreset\u{7}".to_string(),
        };
        let mut log =
            IncidentLog::start_at(Some(&dir.0), 1234, 42, "2.34.3 abc", adapter(), Some(fault))
                .unwrap();
        log.record_attempt(1, "preferred").unwrap();
        log.record_failure(1, "preferred", "adapter\nmissing")
            .unwrap();

        let contents = std::fs::read_to_string(log.path()).unwrap();
        assert!(!contents.contains("driver\nreset"));
        for line in contents.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["schema_version"], 1);
            assert_eq!(value["kettle_version"], "2.34.3 abc");
            assert!(value.get("terminal_text").is_none());
            assert!(value.get("command").is_none());
            assert!(value.get("cwd").is_none());
        }
        assert!(std::fs::metadata(log.path()).unwrap().len() <= MAX_INCIDENT_BYTES);
    }

    #[test]
    fn diagnostic_rotation_retains_ten_incidents() {
        let dir = TestDir::new("rotation");
        let diagnostics = diagnostic_dir(Some(&dir.0));
        std::fs::create_dir_all(&diagnostics).unwrap();
        for index in 0..12 {
            std::fs::write(diagnostics.join(format!("gpu-{index:04}-1.jsonl")), b"{}\n").unwrap();
        }

        let log = IncidentLog::start_at(Some(&dir.0), 9999, 1, "test", adapter(), None).unwrap();
        let count = std::fs::read_dir(&diagnostics).unwrap().count();
        assert_eq!(count, RETAINED_INCIDENTS);
        assert!(log.path().exists());
    }

    #[test]
    fn diagnostic_stops_before_incident_size_cap() {
        let dir = TestDir::new("cap");
        let mut log =
            IncidentLog::start_at(Some(&dir.0), 4321, 7, "test", adapter(), None).unwrap();
        let message = "x".repeat(MAX_MESSAGE_CHARS);
        for attempt in 1..=1000 {
            log.record_failure(attempt, "force_software", &message)
                .unwrap();
        }
        assert!(log.capped);
        assert!(std::fs::metadata(log.path()).unwrap().len() <= MAX_INCIDENT_BYTES);
    }
}
