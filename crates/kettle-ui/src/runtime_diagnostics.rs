//! Privacy-safe event-loop stall and exit diagnostics.

use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
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
    /// When the watchdog's timed sleep ends, or `None` while it runs or is
    /// parked. A phase that would stall sooner wakes it.
    watchdog_deadline: Option<Instant>,
}

struct Shared {
    phase: Mutex<PhaseState>,
    /// Wakes the watchdog when a phase starts while it is parked, and on stop.
    wake: Condvar,
    /// The watchdog is waiting with no deadline, which it does only while the
    /// event loop is idle. Read and written with `phase` locked.
    parked: AtomicBool,
    /// How long a phase may run before it is recorded as a stall.
    stall_after: Duration,
    gpu_init_stall_after: Duration,
    /// Watchdog loop iterations, so tests can see when it wakes.
    #[cfg(test)]
    loops: AtomicUsize,
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
        Self::start_with(cache_dir, version, NORMAL_STALL, GPU_INIT_STALL)
    }

    fn start_with(
        cache_dir: Option<PathBuf>,
        version: String,
        stall_after: Duration,
        gpu_init_stall_after: Duration,
    ) -> Self {
        let shared = Arc::new(Shared {
            phase: Mutex::new(PhaseState {
                name: "idle",
                entered: Instant::now(),
                watchdog_deadline: None,
            }),
            wake: Condvar::new(),
            parked: AtomicBool::new(false),
            stall_after,
            gpu_init_stall_after,
            #[cfg(test)]
            loops: AtomicUsize::new(0),
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
        let mut state = lock(&self.shared.phase);
        let now = Instant::now();
        state.name = phase;
        state.entered = now;
        self.shared.stall_written.store(false, Ordering::Release);
        // A phase wakes the watchdog only when it is parked, or sleeping
        // toward a later deadline than this phase's, as after `gpu_init`. A
        // busy loop starts a phase every turn, each with a later deadline than
        // the one the watchdog already waits for.
        if !is_quiet(phase) {
            let deadline = now + self.shared.threshold(phase);
            let parked = self.shared.parked.swap(false, Ordering::Relaxed);
            if parked
                || state
                    .watchdog_deadline
                    .is_some_and(|sleeping| deadline < sleeping)
            {
                self.shared.wake.notify_one();
            }
        }
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
        // Set under the lock so the watchdog cannot check `stop` and then
        // park after this notification.
        let _state = lock(&self.shared.phase);
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake.notify_one();
    }

    fn snapshot(&self) -> PhaseState {
        lock(&self.shared.phase).clone()
    }
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        self.tracker.set_phase("idle");
    }
}

impl Shared {
    /// How long `phase` may run before it is recorded as a stall.
    fn threshold(&self, phase: &str) -> Duration {
        if phase == "gpu_init" {
            self.gpu_init_stall_after
        } else {
            self.stall_after
        }
    }
}

fn lock(phase: &Mutex<PhaseState>) -> MutexGuard<'_, PhaseState> {
    phase.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Phases in which the event loop is not running a task that could stall.
fn is_quiet(phase: &str) -> bool {
    matches!(phase, "idle" | "suspended" | "exiting")
}

/// Record a phase that outlives its deadline.
///
/// The watchdog sleeps until the current phase's deadline, and parks once the
/// event loop has been quiet for a whole threshold. A busy loop wakes it at
/// most once per threshold, and an idle window not at all.
fn watchdog_loop(shared: Arc<Shared>) {
    let mut state = lock(&shared.phase);
    while !shared.stop.load(Ordering::Acquire) {
        #[cfg(test)]
        shared.loops.fetch_add(1, Ordering::Relaxed);
        if is_quiet(state.name) {
            let quiet_for = state.entered.elapsed();
            state = if quiet_for < shared.stall_after {
                // A busy loop goes quiet between turns. Wait out a threshold
                // before parking, so the next turn does not have to wake it.
                sleep_until(&shared, state, shared.stall_after - quiet_for)
            } else {
                state.watchdog_deadline = None;
                shared.parked.store(true, Ordering::Relaxed);
                shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner)
            };
            continue;
        }
        let threshold = shared.threshold(state.name);
        let elapsed = state.entered.elapsed();
        if elapsed >= threshold
            && shared
                .stall_written
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            let phase = state.clone();
            drop(state);
            if let Err(error) = write_incident(&shared, "event_loop_stall", &phase, None) {
                log::warn!("runtime stall diagnostic write failed: {error}");
            }
            state = lock(&shared.phase);
            continue;
        }
        // Before the deadline, sleep until it. After a recorded stall, check
        // again a threshold later: a new phase re-arms the record.
        let wait = threshold.checked_sub(elapsed).unwrap_or(threshold);
        state = sleep_until(&shared, state, wait);
    }
}

/// Sleep for `wait` or until woken, recording the deadline for `set_phase`.
fn sleep_until<'a>(
    shared: &'a Shared,
    mut state: MutexGuard<'a, PhaseState>,
    wait: Duration,
) -> MutexGuard<'a, PhaseState> {
    state.watchdog_deadline = Some(Instant::now() + wait);
    let mut state = shared
        .wake
        .wait_timeout(state, wait)
        .unwrap_or_else(PoisonError::into_inner)
        .0;
    state.watchdog_deadline = None;
    state
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
    // below creates missing ancestors at 0700 but returns early when the
    // directory already exists. A `<cache>/kettle` that an earlier run left
    // group-writable under a 002 umask would never be narrowed, and the
    // private-path verifier would refuse the write.
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
    // Never fatal. The repair only clears the way for the write, so its failure
    // does not matter if the write still succeeds. On this crash path, losing
    // the incident is worse than any repair failure. On Linux, for example, the
    // verifier holds directories with `O_PATH`, which needs no read permission,
    // so a `0300` directory this cannot even open for inspection still accepts
    // the write.
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

    /// Nothing can stall while the event loop is idle, so an idle watchdog has
    /// no reason to wake. A timer that fires anyway is a wakeup every second
    /// for the life of every idle window. After one quiet threshold it parks.
    #[test]
    fn an_idle_watchdog_parks() {
        let stall = Duration::from_millis(200);
        let tracker = RuntimeTracker::start_with(None, "test".to_string(), stall, stall);
        std::thread::sleep(Duration::from_millis(3500));
        let loops = tracker.shared.loops.load(Ordering::Relaxed);
        tracker.stop();
        assert!(loops <= 2, "an idle watchdog woke {loops} times in 3.5 s");
    }

    /// A busy event loop enters a phase on every turn. Only the first phase
    /// after an idle stretch may wake the watchdog; then it waits for that
    /// phase's stall deadline.
    #[test]
    fn a_busy_event_loop_wakes_the_watchdog_at_most_once_per_deadline() {
        let tracker = RuntimeTracker::start(None, "test".to_string());
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(1500) {
            drop(tracker.enter("about_to_wait"));
            std::thread::sleep(Duration::from_millis(5));
        }
        let loops = tracker.shared.loops.load(Ordering::Relaxed);
        tracker.stop();
        assert!(
            loops <= 3,
            "300 phase changes woke the watchdog {loops} times"
        );
    }

    #[cfg(unix)]
    const REPAIR_CHILD_ENV: &str = "KETTLE_UI_DIAGNOSTIC_REPAIR_CHILD";

    /// Run `body` in a re-executed child running this one test alone.
    ///
    /// The scratch root has to look like an XDG base, and `kettle_base_dirs`
    /// reads the real environment. The harness runs tests on a thread pool, so
    /// `set_var` in the shared harness process is a data race against every
    /// other test's `getenv`. It also leaks the variable into whatever runs
    /// afterwards, pointing at a scratch directory that no longer exists.
    /// `kettle-ctl`'s permissive-umask tests re-exec for the same reason.
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

    /// A machine that ran an older kettle under a 002 umask already has a
    /// group-writable `<cache>/kettle`.
    ///
    /// Creating missing ancestors at 0700 does not help, because the directory
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
                        watchdog_deadline: None,
                    }),
                    wake: Condvar::new(),
                    parked: AtomicBool::new(false),
                    stall_after: NORMAL_STALL,
                    gpu_init_stall_after: GPU_INIT_STALL,
                    loops: AtomicUsize::new(0),
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

    /// A phase that runs past its deadline is still recorded, once, while the
    /// watchdog sleeps between deadlines instead of polling.
    #[cfg(unix)]
    #[test]
    fn a_stuck_phase_is_recorded_once() {
        in_child(
            "runtime_diagnostics::tests::a_stuck_phase_is_recorded_once",
            || {
                let root = kettle_test_support::private_tempdir("kettle-runtime-stall-test-");
                // SAFETY: only the isolated child reaches this closure.
                unsafe { std::env::set_var("XDG_CACHE_HOME", root.path()) };
                let stall = Duration::from_millis(300);
                let tracker = RuntimeTracker::start_with(
                    Some(root.path().to_path_buf()),
                    "test".to_string(),
                    stall,
                    stall,
                );
                let dir = diagnostic_dir(Some(root.path()));
                let incidents = || std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);

                tracker.set_phase("redraw");
                let deadline = Instant::now() + Duration::from_secs(5);
                while incidents() == 0 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                assert_eq!(
                    incidents(),
                    1,
                    "a phase stuck past its deadline was not recorded"
                );
                let text = std::fs::read_to_string(
                    std::fs::read_dir(&dir)
                        .unwrap()
                        .next()
                        .unwrap()
                        .unwrap()
                        .path(),
                )
                .unwrap();
                assert!(text.contains("\"event_loop_stall\"") && text.contains("\"redraw\""));

                // Still stuck: no second incident for the same phase.
                std::thread::sleep(stall * 3);
                tracker.stop();
                assert_eq!(incidents(), 1, "the same stall was recorded twice");
            },
        );
    }

    /// A short-threshold phase that hangs right after a long one is recorded
    /// at its own deadline. The watchdog was sleeping toward the long phase's
    /// deadline, so the new phase has to wake it.
    #[cfg(unix)]
    #[test]
    fn a_hang_after_gpu_init_is_recorded_at_its_own_deadline() {
        in_child(
            "runtime_diagnostics::tests::a_hang_after_gpu_init_is_recorded_at_its_own_deadline",
            || {
                let root = kettle_test_support::private_tempdir("kettle-runtime-stall-order-");
                // SAFETY: only the isolated child reaches this closure.
                unsafe { std::env::set_var("XDG_CACHE_HOME", root.path()) };
                let tracker = RuntimeTracker::start_with(
                    Some(root.path().to_path_buf()),
                    "test".to_string(),
                    Duration::from_millis(300),
                    Duration::from_secs(5),
                );
                let dir = diagnostic_dir(Some(root.path()));
                let incidents = || std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);

                // Long enough for the watchdog to read `gpu_init` and sleep
                // toward its 5 s deadline.
                tracker.set_phase("gpu_init");
                std::thread::sleep(Duration::from_millis(600));
                let hung = Instant::now();
                tracker.set_phase("redraw");
                while incidents() == 0 && hung.elapsed() < Duration::from_secs(6) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                let recorded_after = hung.elapsed();
                tracker.stop();
                assert_eq!(incidents(), 1, "the hang after gpu_init was not recorded");
                assert!(
                    recorded_after < Duration::from_secs(2),
                    "recorded {recorded_after:?} after the hang, not near its 300 ms deadline"
                );
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn incident_is_private_and_rotated() {
        in_child(
            "runtime_diagnostics::tests::incident_is_private_and_rotated",
            || {
                use std::os::unix::fs::PermissionsExt as _;

                // The cache root is part of the private-path trust chain. Use
                // an owner-only scratch guard and make it the real XDG cache
                // base, rather than creating an ad-hoc child at the ambient
                // umask. The re-executed child makes that environment change
                // safe for the parallel Rust test harness.
                let root = kettle_test_support::private_tempdir("kettle-runtime-diagnostic-test-");
                // SAFETY: only the isolated child reaches this closure.
                unsafe { std::env::set_var("XDG_CACHE_HOME", root.path()) };
                let shared = Shared {
                    phase: Mutex::new(PhaseState {
                        name: "redraw",
                        entered: Instant::now(),
                        watchdog_deadline: None,
                    }),
                    wake: Condvar::new(),
                    parked: AtomicBool::new(false),
                    stall_after: NORMAL_STALL,
                    gpu_init_stall_after: GPU_INIT_STALL,
                    loops: AtomicUsize::new(0),
                    windows: AtomicUsize::new(2),
                    stop: AtomicBool::new(false),
                    stall_written: AtomicBool::new(false),
                    cache_dir: Some(root.path().to_path_buf()),
                    version: "test".to_string(),
                };
                for _ in 0..12 {
                    let phase = shared.phase.lock().unwrap().clone();
                    write_incident(&shared, "test", &phase, Some("safe error")).unwrap();
                }
                let dir = diagnostic_dir(Some(root.path()));
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
            },
        );
    }
}
