//! Throttled UI integration for kettle's authenticated stable release feed.
//!
//! Feed parsing, signature verification, download verification, and replacement
//! live in `kettle-update`. This module owns only per-user throttle/dismissal
//! state and delivery of background outcomes to the winit event loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use kettle_config::UpdatePolicy;
use kettle_update::{CheckOutcome, FeedClient};
use serde::{Deserialize, Serialize};
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

const DEFAULT_INTERVAL_SECS: u64 = 24 * 60 * 60;
const MAX_CACHE_BYTES: usize = 256 * 1024;
const PACKAGED: bool = option_env!("KETTLE_PACKAGED").is_some();
static CHECK_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

struct CheckInFlight {
    _file_lock: Option<std::fs::File>,
}

impl Drop for CheckInFlight {
    fn drop(&mut self) {
        CHECK_IN_FLIGHT.store(false, Ordering::Release);
    }
}

enum CheckLockAttempt {
    Acquired(Option<std::fs::File>),
    Busy,
}

fn try_acquire_check_lock() -> std::io::Result<CheckLockAttempt> {
    let Some(cache) = cache_path() else {
        return Ok(CheckLockAttempt::Acquired(None));
    };
    let Some(parent) = cache.parent() else {
        return Ok(CheckLockAttempt::Acquired(None));
    };
    std::fs::create_dir_all(parent)?;
    try_acquire_file_lock(&parent.join("update-check.lock"))
}

fn try_acquire_file_lock(path: &std::path::Path) -> std::io::Result<CheckLockAttempt> {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    match fs4::FileExt::try_lock(&lock) {
        Ok(()) => Ok(CheckLockAttempt::Acquired(Some(lock))),
        Err(fs4::TryLockError::WouldBlock) => Ok(CheckLockAttempt::Busy),
        Err(fs4::TryLockError::Error(error)) => Err(error),
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
    let bytes = std::fs::read(cache_path()?).ok()?;
    if bytes.len() > MAX_CACHE_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn save_cache(cache: &UpdateCache) {
    let Some(path) = cache_path() else { return };
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
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

/// Run at most once per day after the first launch. `Auto` installs only into
/// an official installer-owned layout and never restarts this running process.
pub fn maybe_spawn_check(
    proxy: EventLoopProxy<UserEvent>,
    current: &'static str,
    policy: UpdatePolicy,
) {
    if PACKAGED || policy == UpdatePolicy::Off {
        return;
    }
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
    if !is_due(now, cache.last_check, DEFAULT_INTERVAL_SECS) {
        return;
    }
    let dismissed = cache.dismissed_version.clone();
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

            match outcome {
                Ok(CheckOutcome::UpToDate { .. }) => {}
                Err(error) => log::warn!("update check failed: {error}"),
                Ok(CheckOutcome::UpdateAvailable(update)) => {
                    if dismissed.as_deref() == Some(update.tag.as_str()) {
                        return;
                    }
                    if policy == UpdatePolicy::Auto {
                        match kettle_update::install_update(&client, &update) {
                            Ok(_) => {
                                let _ = proxy
                                    .send_event(UserEvent::UpdateInstalled { tag: update.tag });
                            }
                            Err(kettle_update::UpdateError::UnmanagedInstall(reason)) => {
                                log::info!(
                                    "automatic update skipped for unmanaged install: {reason}"
                                );
                                let _ = proxy.send_event(UserEvent::UpdateAvailable {
                                    tag: update.tag,
                                    url: update.release_url,
                                });
                            }
                            Err(kettle_update::UpdateError::UnsupportedPlatform) => {
                                log::info!(
                                    "automatic update is unsupported on this platform; notifying instead"
                                );
                                let _ = proxy.send_event(UserEvent::UpdateAvailable {
                                    tag: update.tag,
                                    url: update.release_url,
                                });
                            }
                            Err(error) => {
                                let _ = proxy.send_event(UserEvent::UpdateFailed {
                                    message: error.to_string(),
                                });
                            }
                        }
                    } else {
                        let _ = proxy.send_event(UserEvent::UpdateAvailable {
                            tag: update.tag,
                            url: update.release_url,
                        });
                    }
                }
            }
        });
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
            let guidance = if kettle_update::detect_managed_install().is_ok() {
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
    use super::{CheckLockAttempt, DEFAULT_INTERVAL_SECS, UpdateCache, is_due};

    #[test]
    fn due_check_skips_first_launch_and_handles_clock_skew() {
        let day = DEFAULT_INTERVAL_SECS;
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
        };
        let json = serde_json::to_string(&cache).unwrap();
        let back: UpdateCache = serde_json::from_str(&json).unwrap();
        assert_eq!(back.last_check, cache.last_check);
        let partial: UpdateCache = serde_json::from_str(r#"{"last_check":42}"#).unwrap();
        assert_eq!(partial.last_check, 42);
        assert_eq!(partial.latest_tag, None);
    }

    #[test]
    fn background_check_lock_is_exclusive_and_reusable() {
        let temp = tempfile::tempdir().unwrap();
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
