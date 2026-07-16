//! Privacy-safe event-loop stall and exit diagnostics.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

const SCHEMA_VERSION: u32 = 1;
const RETAINED_INCIDENTS: usize = 10;
const MAX_ERROR_CHARS: usize = 2048;
const NORMAL_STALL: Duration = Duration::from_secs(10);
// Deliberately precedes the renderer's 30-second fatal watchdog so the
// incident is durably written before that watchdog terminates the process.
const GPU_INIT_STALL: Duration = Duration::from_secs(25);

#[derive(Clone, Debug)]
struct PhaseState {
    name: &'static str,
    entered: Instant,
}

struct Shared {
    phase: Mutex<PhaseState>,
    windows: AtomicUsize,
    stop: AtomicBool,
    stall_written: AtomicBool,
    cache_dir: Option<PathBuf>,
    version: String,
}

/// Cloneable phase heartbeat. It records only fixed phase names and counts;
/// terminal contents, paths, commands, and environment values never enter it.
#[derive(Clone)]
pub(crate) struct RuntimeTracker {
    shared: Arc<Shared>,
}

pub(crate) struct PhaseGuard {
    tracker: RuntimeTracker,
}

impl RuntimeTracker {
    pub(crate) fn start(cache_dir: Option<PathBuf>, version: String) -> Self {
        let shared = Arc::new(Shared {
            phase: Mutex::new(PhaseState {
                name: "idle",
                entered: Instant::now(),
            }),
            windows: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            stall_written: AtomicBool::new(false),
            cache_dir,
            version,
        });
        let watchdog = shared.clone();
        let _ = std::thread::Builder::new()
            .name("kettle-event-loop-watchdog".to_string())
            .spawn(move || watchdog_loop(watchdog));
        Self { shared }
    }

    pub(crate) fn enter(&self, phase: &'static str) -> PhaseGuard {
        self.set_phase(phase);
        PhaseGuard {
            tracker: self.clone(),
        }
    }

    pub(crate) fn set_phase(&self, phase: &'static str) {
        let mut state = self
            .shared
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.name = phase;
        state.entered = Instant::now();
        self.shared.stall_written.store(false, Ordering::Release);
    }

    pub(crate) fn set_window_count(&self, count: usize) {
        self.shared.windows.store(count, Ordering::Release);
    }

    pub(crate) fn record_exit(&self, error: &str) {
        let phase = self.snapshot();
        if let Err(write_error) =
            write_incident(&self.shared, "event_loop_exit", &phase, Some(error))
        {
            log::warn!("runtime diagnostic write failed: {write_error}");
        }
    }

    pub(crate) fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
    }

    fn snapshot(&self) -> PhaseState {
        self.shared
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        self.tracker.set_phase("idle");
    }
}

fn watchdog_loop(shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_secs(1));
        let phase = shared
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if phase.name == "idle" || phase.name == "suspended" || phase.name == "exiting" {
            continue;
        }
        let threshold = if phase.name == "gpu_init" {
            GPU_INIT_STALL
        } else {
            NORMAL_STALL
        };
        if phase.entered.elapsed() < threshold
            || shared
                .stall_written
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            continue;
        }
        if let Err(error) = write_incident(&shared, "event_loop_stall", &phase, None) {
            log::warn!("runtime stall diagnostic write failed: {error}");
        }
    }
}

#[derive(Serialize)]
struct Incident<'a> {
    schema_version: u32,
    timestamp_unix_ms: u128,
    kettle_version: &'a str,
    pid: u32,
    kind: &'a str,
    backend: &'static str,
    phase: &'static str,
    phase_elapsed_ms: u128,
    windows: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn write_incident(
    shared: &Shared,
    kind: &str,
    phase: &PhaseState,
    error: Option<&str>,
) -> io::Result<PathBuf> {
    let dir = diagnostic_dir(shared.cache_dir.as_deref());
    create_private_dir(&dir)?;
    let timestamp = unix_millis();
    let (path, mut file) = create_private_file(&dir, timestamp, std::process::id())?;
    let incident = Incident {
        schema_version: SCHEMA_VERSION,
        timestamp_unix_ms: timestamp,
        kettle_version: &shared.version,
        pid: std::process::id(),
        kind,
        backend: display_backend(),
        phase: phase.name,
        phase_elapsed_ms: phase.entered.elapsed().as_millis(),
        windows: shared.windows.load(Ordering::Acquire),
        error: error.map(sanitize),
    };
    serde_json::to_writer(&mut file, &incident).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.flush()?;
    prune_incidents(&dir, &path)?;
    log::error!("runtime diagnostics: {}", path.display());
    Ok(path)
}

fn diagnostic_dir(cache_dir: Option<&Path>) -> PathBuf {
    cache_dir
        .map(|base| base.join("kettle").join("diagnostics"))
        .unwrap_or_else(|| PathBuf::from("kettle-diagnostics"))
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    Ok(())
}

fn create_private_file(dir: &Path, unix_ms: u128, pid: u32) -> io::Result<(PathBuf, File)> {
    for suffix in 0..100u8 {
        let suffix = if suffix == 0 {
            String::new()
        } else {
            format!("-{suffix}")
        };
        let path = dir.join(format!("runtime-{unix_ms}-{pid}{suffix}.json"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique runtime diagnostic filename",
    ))
}

fn prune_incidents(dir: &Path, preserve: &Path) -> io::Result<()> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("runtime-") && name.ends_with(".json"))
        })
        .collect();
    files.sort();
    while files.len() > RETAINED_INCIDENTS {
        let Some(index) = files.iter().position(|path| path != preserve) else {
            break;
        };
        let stale = files.remove(index);
        match std::fs::remove_file(stale) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn display_backend() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        "wayland"
    } else if std::env::var_os("DISPLAY").is_some() {
        "x11"
    } else {
        "unknown"
    }
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .take(MAX_ERROR_CHARS)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_and_bounds_error_text() {
        let input = format!("line\n{}", "x".repeat(MAX_ERROR_CHARS + 50));
        let output = sanitize(&input);
        assert!(!output.contains('\n'));
        assert_eq!(output.chars().count(), MAX_ERROR_CHARS);
    }

    #[cfg(unix)]
    #[test]
    fn incident_is_private_and_rotated() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = std::env::temp_dir().join(format!(
            "kettle-runtime-diagnostic-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let shared = Shared {
            phase: Mutex::new(PhaseState {
                name: "redraw",
                entered: Instant::now(),
            }),
            windows: AtomicUsize::new(2),
            stop: AtomicBool::new(false),
            stall_written: AtomicBool::new(false),
            cache_dir: Some(root.clone()),
            version: "test".to_string(),
        };
        for _ in 0..12 {
            let phase = shared.phase.lock().unwrap().clone();
            write_incident(&shared, "test", &phase, Some("safe error")).unwrap();
        }
        let dir = diagnostic_dir(Some(&root));
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(files.len(), RETAINED_INCIDENTS);
        for file in files {
            assert_eq!(
                file.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
