//! Privacy-safe event-loop stall and exit diagnostics.

use std::fs::File;
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
    // Repair the directory before writing into it. `create_private_file_new`
    // below creates missing ancestors at 0700, but it returns early when the
    // directory already exists — so a `<cache>/kettle` an earlier run left
    // group-writable under a 002 umask is never narrowed, and the private-path
    // verifier then refuses the write. Recognizing that directory as kettle's
    // own was necessary but not sufficient; something has to ask for the
    // repair, and this is the caller that needs it.
    //
    // Unix only. The Windows `create_private_dirs` is a plain `create_dir_all`,
    // while `create_private_file_new` builds each missing parent with
    // `CreateDirectoryW` under an explicit owner-only security descriptor and
    // then verifies it. Running the plain create first would win the race to
    // create `diagnostics`, leave the hardened path nothing to build, and hand
    // the crash directory whatever inheritable ACEs its parent carries. That is
    // a downgrade wearing a repair's name; Windows has no umask and needs none
    // of this.
    //
    // Never fatal. The repair exists to clear the way for the write, so if the
    // write succeeds anyway the failure did not matter — and this is the crash
    // path, where losing the incident is worse than every reason the repair can
    // fail. Linux makes that concrete: the verifier holds directories with
    // `O_PATH`, which needs no read permission, so a `0300` directory this
    // cannot even open for inspection still accepts the write. Gating on `?`
    // turned that into a lost diagnostic.
    #[cfg(unix)]
    if let Err(error) = kettle_state::create_private_dirs(&dir) {
        log::debug!(
            "diagnostic directory repair skipped for {}: {error}",
            dir.display()
        );
    }
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

fn create_private_file(dir: &Path, unix_ms: u128, pid: u32) -> io::Result<(PathBuf, File)> {
    for suffix in 0..100u8 {
        let suffix = if suffix == 0 {
            String::new()
        } else {
            format!("-{suffix}")
        };
        let path = dir.join(format!("runtime-{unix_ms}-{pid}{suffix}.json"));
        match kettle_state::create_private_file_new(&path) {
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
    const REPAIR_CHILD_ENV: &str = "KETTLE_UI_DIAGNOSTIC_REPAIR_CHILD";

    /// Run `body` in a re-executed child running this one test alone.
    ///
    /// The scratch root has to look like an XDG base, and `kettle_base_dirs`
    /// reads the real environment. `set_var` in the shared harness process is a
    /// data race against every other test's `getenv` — the harness runs tests on
    /// a thread pool — and it leaks the variable into whatever runs afterwards,
    /// pointing at a scratch directory that no longer exists. An earlier version
    /// of this test did that behind a SAFETY note claiming a single thread it
    /// never had. `kettle-ctl`'s permissive-umask tests already re-exec for the
    /// same reason; this is that helper.
    #[cfg(unix)]
    fn in_child(name: &str, body: impl FnOnce()) {
        if std::env::var_os(REPAIR_CHILD_ENV).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--nocapture"])
                .env(REPAIR_CHILD_ENV, "1")
                .status()
                .expect("re-exec the test binary");
            assert!(status.success(), "diagnostic-repair child failed: {status}");
            return;
        }
        body();
    }

    /// The state a machine that ran an older kettle under a 002 umask is
    /// actually in: `<cache>/kettle` already exists and is group-writable.
    ///
    /// Creating missing ancestors at 0700 does not help there — the directory
    /// is not missing. Without an explicit repair the private-path verifier
    /// refuses the write, and a crash diagnostic is exactly the thing you want
    /// to survive that.
    ///
    /// Both levels are asserted. The verifier walks ancestors, so repairing only
    /// `<cache>/kettle` and leaving `diagnostics` at 0775 refuses the write just
    /// as surely, and an assertion on the top directory alone would not notice.
    #[cfg(unix)]
    #[test]
    fn an_existing_group_writable_cache_directory_is_repaired_before_writing() {
        in_child(
            "runtime_diagnostics::tests::\
             an_existing_group_writable_cache_directory_is_repaired_before_writing",
            || {
                use std::os::unix::fs::PermissionsExt as _;

                let root = kettle_test_support::private_tempdir("kettle-diag-repair-");
                let kettle_dir = root.path().join("kettle");
                let diagnostics = kettle_dir.join("diagnostics");
                std::fs::create_dir_all(&diagnostics).expect("pre-existing tree");
                for dir in [&kettle_dir, &diagnostics] {
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o775))
                        .expect("pre-existing mode");
                }
                // SAFETY: the re-executed child runs this test alone, before it
                // starts any thread. The base list keys on the real environment,
                // so the scratch root has to look like a base.
                unsafe { std::env::set_var("XDG_CACHE_HOME", root.path()) };

                let shared = Shared {
                    phase: Mutex::new(PhaseState {
                        name: "redraw",
                        entered: Instant::now(),
                    }),
                    windows: AtomicUsize::new(1),
                    stop: AtomicBool::new(false),
                    stall_written: AtomicBool::new(false),
                    cache_dir: Some(root.path().to_path_buf()),
                    version: "test".to_string(),
                };
                let phase = shared.phase.lock().unwrap().clone();
                let written = write_incident(&shared, "test", &phase, None)
                    .expect("a group-writable cache directory must not block a diagnostic");

                assert!(written.exists(), "the incident was written");
                for dir in [&kettle_dir, &diagnostics] {
                    assert_eq!(
                        std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
                        0o700,
                        "{} should have been repaired",
                        dir.display()
                    );
                }
            },
        );
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
