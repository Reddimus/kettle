//! Throttled UI integration for kettle's authenticated stable release feed.
//!
//! Feed parsing, signature verification, download verification, and replacement
//! live in `kettle-update`. This module owns only per-user throttle/dismissal
//! state and delivery of background outcomes to the winit event loop.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use kettle_config::UpdatePolicy;
use kettle_update::decision::{Decision, Latest, Policy, decide};
use kettle_update::{CheckOutcome, FeedClient, InstallSnapshot};
use serde::{Deserialize, Serialize};
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

const MAX_CACHE_BYTES: usize = 256 * 1024;
const PACKAGED: bool = option_env!("KETTLE_PACKAGED").is_some();
static CHECK_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// What a check does after deciding, before any install attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Install,
    Announce,
    RestartPending(kettle_update::Version),
    Nothing,
}

/// A dismissal hides an announcement only: it never stops `auto` from
/// installing, and the restart notice has its own lifetime.
fn next_step(decision: Decision, dismissed: impl FnOnce() -> bool) -> Step {
    match decision {
        Decision::UpToDate => Step::Nothing,
        Decision::RestartPending { installed } => Step::RestartPending(installed),
        Decision::Install => Step::Install,
        Decision::Announce { .. } if dismissed() => Step::Nothing,
        Decision::Announce { .. } => Step::Announce,
    }
}

/// What a failed install attempt means for this process.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InstallFailure {
    /// Another process installed this or a newer version first, or an
    /// update is waiting for its own version to start.
    RestartPending(kettle_update::Version),
    /// Kettle cannot install here, as startup already found: point the user
    /// at the release instead.
    Announce,
    /// Another process is installing right now; the next check catches up.
    Quiet,
    Failed,
}

fn classify(
    error: &kettle_update::UpdateError,
    running: &kettle_update::Version,
    proven_at_startup: bool,
) -> InstallFailure {
    use kettle_update::UpdateError;
    let newer = |version: kettle_update::Version| {
        if &version > running {
            InstallFailure::RestartPending(version)
        } else {
            InstallFailure::Quiet
        }
    };
    match error {
        UpdateError::AwaitingRestart { target } => {
            kettle_update::Version::parse(target).map_or(InstallFailure::Failed, newer)
        }
        UpdateError::Rollback { installed, .. } => newer(installed.clone()),
        UpdateError::UpdateLocked => InstallFailure::Quiet,
        // Startup found an install it manages, so losing it now is a fault,
        // not a reason to send the user to the release page.
        UpdateError::UnmanagedInstall(_) | UpdateError::UnsupportedPlatform
            if !proven_at_startup =>
        {
            InstallFailure::Announce
        }
        _ => InstallFailure::Failed,
    }
}

struct CheckInFlight {
    _file_lock: Option<kettle_state::ExclusiveFileLock>,
}

impl Drop for CheckInFlight {
    fn drop(&mut self) {
        CHECK_IN_FLIGHT.store(false, Ordering::Release);
    }
}

enum CheckLockAttempt {
    Acquired(Option<kettle_state::ExclusiveFileLock>),
    Busy,
}

fn try_acquire_check_lock() -> std::io::Result<CheckLockAttempt> {
    let Some(cache) = cache_path() else {
        return Ok(CheckLockAttempt::Acquired(None));
    };
    let Some(parent) = cache.parent() else {
        return Ok(CheckLockAttempt::Acquired(None));
    };
    kettle_state::create_private_dirs(parent)?;
    try_acquire_file_lock(&parent.join("update-check.lock"))
}

fn try_acquire_file_lock(path: &std::path::Path) -> std::io::Result<CheckLockAttempt> {
    match kettle_state::ExclusiveFileLock::try_acquire(path)? {
        Some(lock) => Ok(CheckLockAttempt::Acquired(Some(lock))),
        None => Ok(CheckLockAttempt::Busy),
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UpdateCache {
    #[serde(default)]
    last_check: u64,
    #[serde(default)]
    latest_tag: Option<String>,
    #[serde(default)]
    dismissed_version: Option<String>,
    /// Set once the first automatic (`update-policy = auto`) install has been
    /// announced, so the one-time "Kettle now keeps itself up to date; disable
    /// with `update-policy = off`" heads-up fires exactly once per install.
    #[serde(default)]
    auto_update_announced: bool,
}

fn is_due(now: u64, last_check: u64, interval_secs: u64) -> bool {
    interval_secs != 0 && last_check != 0 && now.saturating_sub(last_check) >= interval_secs
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn cache_path() -> Option<std::path::PathBuf> {
    kettle_config::Config::default_path().and_then(|path| {
        path.parent()
            .map(|directory| directory.join("update-check.json"))
    })
}

fn load_cache() -> Option<UpdateCache> {
    let file = kettle_state::open_trusted_file_read(&cache_path()?).ok()?;
    let size = file.metadata().ok()?.len();
    read_cache(file, size)
}

fn read_cache(reader: impl Read, size: u64) -> Option<UpdateCache> {
    if size > MAX_CACHE_BYTES as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    reader
        .take(MAX_CACHE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_CACHE_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn save_cache(cache: &UpdateCache) {
    let Some(path) = cache_path() else { return };
    if let Some(parent) = path.parent()
        && kettle_state::create_private_dirs(parent).is_err()
    {
        return;
    }
    let Ok(json) = serde_json::to_vec_pretty(cache) else {
        return;
    };
    if let Err(error) = kettle_update::write_atomic_file(&path, &json) {
        log::warn!("update-check: could not persist cache: {error}");
    }
}

/// Run at most once per `interval_hours` (default 24 = daily) after the first
/// launch. `Auto` installs only into an official installer-owned layout and
/// never restarts this running process. Safe to call repeatedly (e.g. from the
/// recurring [`spawn_update_check_timer`] tick): the shared cache + file lock
/// make the due check idempotent across every window and process.
pub fn maybe_spawn_check(
    proxy: EventLoopProxy<UserEvent>,
    current: &'static str,
    install: InstallSnapshot,
    policy: UpdatePolicy,
    interval_hours: u32,
) {
    if PACKAGED || policy == UpdatePolicy::Off {
        return;
    }
    let Ok(running) = kettle_update::Version::parse(current) else {
        log::warn!("update-check: invalid running version {current:?}");
        return;
    };
    let check_policy = if policy == UpdatePolicy::Auto {
        Policy::Auto
    } else {
        Policy::Notify
    };
    // Floored at 1h so a misconfigured 0 can't turn into a busy network loop.
    let interval_secs = u64::from(interval_hours.max(1)).saturating_mul(3600);
    if CHECK_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        return;
    }
    let file_lock = match try_acquire_check_lock() {
        Ok(CheckLockAttempt::Acquired(lock)) => lock,
        Ok(CheckLockAttempt::Busy) => {
            CHECK_IN_FLIGHT.store(false, Ordering::Release);
            return;
        }
        Err(error) => {
            CHECK_IN_FLIGHT.store(false, Ordering::Release);
            log::warn!("update-check: could not acquire throttle lock: {error}");
            return;
        }
    };
    let in_flight = CheckInFlight {
        _file_lock: file_lock,
    };
    let now = now_secs();
    let mut cache = load_cache().unwrap_or_default();
    if cache.last_check == 0 {
        cache.last_check = now;
        save_cache(&cache);
        return;
    }
    if !is_due(now, cache.last_check, interval_secs) {
        return;
    }
    // Persist the throttle before starting network I/O. The process guard and
    // OS file lock make the read/update decision exclusive across every window
    // and process sharing this config directory.
    cache.last_check = now;
    save_cache(&cache);
    let _ = std::thread::Builder::new()
        .name("kettle-update-check".into())
        .spawn(move || {
            let _in_flight = in_flight;
            let client = FeedClient::new();
            let outcome = client.check(current);
            let mut cache = load_cache().unwrap_or_default();
            cache.last_check = now_secs();
            if let Ok(CheckOutcome::UpdateAvailable(update)) = &outcome {
                cache.latest_tag = Some(update.tag.clone());
            }
            save_cache(&cache);

            let latest_update = match outcome {
                Ok(CheckOutcome::UpToDate { .. }) => None,
                Ok(CheckOutcome::UpdateAvailable(update)) => Some(update),
                Err(error) => {
                    log::warn!("update check failed: {error}");
                    return;
                }
            };
            // The installed version and journal, read now: another process
            // may have installed an update since this one started (#574).
            let local = install.local_state();
            let decision = decide(
                &running,
                install.kind(),
                &local,
                check_policy,
                latest_update.as_ref().map(|update| Latest {
                    version: &update.version,
                    has_asset: update.asset.is_some(),
                }),
            );
            let restart_pending = |installed: kettle_update::Version| {
                let _ = proxy.send_event(UserEvent::UpdateRestartPending {
                    installed: installed.to_string(),
                });
            };
            let step = next_step(decision, || {
                latest_update
                    .as_ref()
                    .is_some_and(|update| is_dismissed(&update.tag))
            });
            let Some(update) = latest_update else {
                if let Step::RestartPending(installed) = step {
                    restart_pending(installed);
                }
                return;
            };
            let announce = |update: kettle_update::AvailableUpdate| {
                let _ = proxy.send_event(UserEvent::UpdateAvailable {
                    tag: update.tag,
                    url: update.release_url,
                });
            };
            match step {
                Step::Nothing => {}
                Step::RestartPending(installed) => restart_pending(installed),
                Step::Announce => {
                    if matches!(policy, UpdatePolicy::Auto) {
                        log::info!(
                            "automatic update is not available for this install or platform; notifying instead"
                        );
                    }
                    announce(update);
                }
                Step::Install => match install.install(&client, &update) {
                    Ok(outcome) => {
                        let staged = matches!(
                            outcome.disposition,
                            kettle_update::InstallDisposition::Staged { .. }
                        );
                        // One-time heads-up that auto-update is on and how to
                        // opt out, on the first automatic install only. Reuse
                        // the `cache` already loaded and persisted above, with
                        // the fresh last_check/latest_tag, rather than a second
                        // load: a failed reload would wipe that throttle state
                        // and spuriously re-announce.
                        let first_time = !cache.auto_update_announced;
                        if first_time {
                            cache.auto_update_announced = true;
                            save_cache(&cache);
                        }
                        let _ = proxy.send_event(UserEvent::UpdateInstalled {
                            tag: update.tag,
                            staged,
                            first_time,
                        });
                    }
                    Err(error) => match classify(&error, &running, install.proven_at_startup()) {
                        InstallFailure::RestartPending(installed) => {
                            log::info!("update {}: {error}", update.tag);
                            restart_pending(installed);
                        }
                        InstallFailure::Announce => {
                            log::info!("automatic update skipped: {error}");
                            if !is_dismissed(&update.tag) {
                                announce(update);
                            }
                        }
                        InstallFailure::Quiet => {
                            log::info!("update {} not installed now: {error}", update.tag);
                        }
                        InstallFailure::Failed => {
                            let _ = proxy.send_event(UserEvent::UpdateFailed {
                                message: error.to_string(),
                            });
                        }
                    },
                },
            }
        });
}

/// Cadence at which the in-session timer re-attempts a due check. Decoupled
/// from the configured update interval: each tick just calls `maybe_spawn_check`,
/// which re-throttles against the persisted per-interval cache. A fixed hourly
/// nudge keeps a long-lived window current without polling the network hourly.
const RECHECK_TICK_SECS: u64 = 60 * 60;
static TIMER_STARTED: AtomicBool = AtomicBool::new(false);

/// Spawn (once per process) a lightweight timer thread that nudges the event
/// loop to re-run the due check every hour, so a window left open for days keeps
/// current instead of only checking on window resume. The app handles the
/// resulting `UpdateCheckTick` by calling `maybe_spawn_check` with the *current*
/// config (policy + interval), so config edits take effect without a restart.
pub fn spawn_update_check_timer(proxy: EventLoopProxy<UserEvent>) {
    if PACKAGED || TIMER_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("kettle-update-timer".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(RECHECK_TICK_SECS));
                // Stop ticking once the event loop is gone (all windows closed).
                if proxy.send_event(UserEvent::UpdateCheckTick).is_err() {
                    break;
                }
            }
        });
}

pub(crate) fn is_dismissed(tag: &str) -> bool {
    is_dismissed_with(tag, || {
        load_cache().and_then(|cache| cache.dismissed_version)
    })
}

fn is_dismissed_with(tag: &str, load: impl FnOnce() -> Option<String>) -> bool {
    load().as_deref() == Some(tag)
}

pub fn record_dismissed(tag: &str) {
    let mut cache = load_cache().unwrap_or_default();
    cache.dismissed_version = Some(tag.to_string());
    save_cache(&cache);
}

pub fn run_blocking_check(current: &str) -> String {
    match FeedClient::new().check(current) {
        Ok(CheckOutcome::UpdateAvailable(update)) => {
            let mut cache = load_cache().unwrap_or_default();
            cache.last_check = now_secs();
            cache.latest_tag = Some(update.tag.clone());
            save_cache(&cache);
            let guidance = if update.asset.is_none() {
                "this platform has no release artifact; open the release page for supported packages"
            } else if kettle_update::detect_managed_install().is_ok() {
                "run `kettle update` to install it"
            } else {
                "update through the package manager or installer that owns this executable"
            };
            format!(
                "update available: {} (you have {current}) - {guidance}\n  {}",
                update.tag, update.release_url
            )
        }
        Ok(CheckOutcome::UpToDate { .. }) => format!("kettle {current} is up to date"),
        Err(error) => format!("could not check for updates: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use kettle_update::UpdateError;
    use kettle_update::decision::Decision;

    use super::{CheckLockAttempt, InstallFailure, Step, UpdateCache, classify, is_due, next_step};

    /// The daily default (`update-check-interval-hours = 24`) in seconds.
    const DAY: u64 = 24 * 60 * 60;

    #[test]
    fn queued_update_uses_dismissal_recorded_after_the_check() {
        let temp = crate::test_tempdir();
        let path = temp.path().join("update-check.json");
        std::fs::write(&path, b"{}").unwrap();
        let load = || {
            let bytes = std::fs::read(&path).ok()?;
            let cache: UpdateCache = serde_json::from_slice(&bytes).ok()?;
            cache.dismissed_version
        };
        let queued_tag = "v5.0.2";
        assert!(!super::is_dismissed_with(queued_tag, load));

        std::fs::write(&path, br#"{"dismissed_version":"v5.0.2"}"#).unwrap();

        assert!(super::is_dismissed_with(queued_tag, load));
        assert!(!super::is_dismissed_with("v5.0.3", load));
    }

    fn v(text: &str) -> kettle_update::Version {
        kettle_update::Version::parse(text).unwrap()
    }

    /// Every process shares the dismissed tag, so a stale window dismissing
    /// "v5.1.0 available" used to stop every process from installing it.
    #[test]
    fn a_dismissed_tag_does_not_block_an_automatic_install() {
        assert_eq!(next_step(Decision::Install, || true), Step::Install);
        let announce = Decision::Announce {
            restart_pending: None,
        };
        assert_eq!(next_step(announce.clone(), || true), Step::Nothing);
        assert_eq!(next_step(announce, || false), Step::Announce);
        assert_eq!(
            next_step(
                Decision::RestartPending {
                    installed: v("5.1.0")
                },
                || true
            ),
            Step::RestartPending(v("5.1.0"))
        );
    }

    #[test]
    fn a_managed_install_error_is_a_failure_not_an_announcement() {
        let running = v("5.0.0");
        let unmanaged = UpdateError::UnmanagedInstall("cannot resolve bin/kettle (deleted)".into());
        assert_eq!(classify(&unmanaged, &running, true), InstallFailure::Failed);
        assert_eq!(
            classify(&unmanaged, &running, false),
            InstallFailure::Announce
        );
    }

    #[test]
    fn an_install_race_lost_to_another_process_is_restart_pending() {
        let running = v("5.0.0");
        let rollback = UpdateError::Rollback {
            candidate: v("5.1.0"),
            installed: v("5.1.0"),
        };
        assert_eq!(
            classify(&rollback, &running, true),
            InstallFailure::RestartPending(v("5.1.0"))
        );
        let awaiting = UpdateError::AwaitingRestart {
            target: "5.1.0".into(),
        };
        assert_eq!(
            classify(&awaiting, &running, true),
            InstallFailure::RestartPending(v("5.1.0"))
        );
    }

    #[test]
    fn update_locked_is_not_a_failure() {
        assert_eq!(
            classify(&UpdateError::UpdateLocked, &v("5.0.0"), true),
            InstallFailure::Quiet
        );
    }

    #[test]
    fn due_check_skips_first_launch_and_handles_clock_skew() {
        let day = DAY;
        assert!(!is_due(1_000_000, 0, day));
        assert!(!is_due(1_000_000, 1_000_000 - day + 1, day));
        assert!(is_due(1_000_000, 1_000_000 - day, day));
        assert!(!is_due(1_000, 5_000, day));
        assert!(!is_due(1_000, 1, 0));
    }

    #[test]
    fn update_cache_round_trips_and_accepts_old_partial_state() {
        let cache = UpdateCache {
            last_check: 1_700_000_000,
            latest_tag: Some("v2.35.0".into()),
            dismissed_version: Some("v2.35.0".into()),
            auto_update_announced: true,
        };
        let json = serde_json::to_string(&cache).unwrap();
        let back: UpdateCache = serde_json::from_str(&json).unwrap();
        assert_eq!(back.last_check, cache.last_check);
        assert!(back.auto_update_announced);
        // An older cache file missing newer fields still parses (defaults).
        let partial: UpdateCache = serde_json::from_str(r#"{"last_check":42}"#).unwrap();
        assert_eq!(partial.last_check, 42);
        assert_eq!(partial.latest_tag, None);
        assert!(!partial.auto_update_announced);
    }

    #[test]
    fn cadence_is_tunable() {
        // A configured interval scales the due window; the daily default is 24h.
        let hour = 3_600;
        assert_eq!(DAY, 24 * hour);
        // 6h cadence: due after 6h, not before.
        assert!(is_due(100 * hour, 100 * hour - 6 * hour, 6 * hour));
        assert!(!is_due(100 * hour, 100 * hour - 6 * hour + 1, 6 * hour));
    }

    #[test]
    fn cache_reads_reject_oversize_before_reading_and_bound_growth() {
        let limit = super::MAX_CACHE_BYTES;
        let mut source = std::io::Cursor::new(vec![b' '; limit + 2]);
        assert!(super::read_cache(&mut source, limit as u64 + 1).is_none());
        assert_eq!(source.position(), 0);
        assert!(super::read_cache(&mut source, 0).is_none());
        assert_eq!(source.position(), limit as u64 + 1);

        let mut bytes = br#"{"last_check":42}"#.to_vec();
        bytes.resize(limit, b' ');
        assert_eq!(
            super::read_cache(bytes.as_slice(), limit as u64)
                .unwrap()
                .last_check,
            42
        );
        bytes.push(b' ');
        assert!(super::read_cache(bytes.as_slice(), limit as u64).is_none());
    }

    #[test]
    fn background_check_lock_is_exclusive_and_reusable() {
        let temp = crate::test_tempdir();
        let path = temp.path().join("check.lock");
        let first = match super::try_acquire_file_lock(&path).unwrap() {
            CheckLockAttempt::Acquired(Some(lock)) => lock,
            _ => panic!("first lock should be acquired"),
        };
        assert!(matches!(
            super::try_acquire_file_lock(&path).unwrap(),
            CheckLockAttempt::Busy
        ));
        drop(first);
        assert!(matches!(
            super::try_acquire_file_lock(&path).unwrap(),
            CheckLockAttempt::Acquired(Some(_))
        ));
    }
}
