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

#[derive(Default)]
pub(crate) struct FactsRefresh {
    reading: bool,
    pending: bool,
}

impl FactsRefresh {
    pub fn begin(&mut self) -> bool {
        if self.reading {
            self.pending = true;
            false
        } else {
            self.reading = true;
            true
        }
    }

    /// A requested refresh supersedes the result currently arriving.
    pub fn complete(&mut self) -> bool {
        self.reading = false;
        !std::mem::take(&mut self.pending)
    }
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

/// The record every Kettle process sharing a config directory reads and
/// writes, in `update-check.json`. Every field has a default, so older files
/// load; fields a newer Kettle adds survive a rewrite through `extra`. A
/// still-running older version rewrites the file with its own fields only,
/// which can cost one duplicate notification.
#[derive(Debug, Default, Serialize, Deserialize)]
struct UpdateCache {
    #[serde(default)]
    last_check: u64,
    /// The latest release the feed reported, even when it is not newer.
    #[serde(default)]
    latest_tag: Option<String>,
    /// What the last check decided about `latest_tag`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_outcome: Option<LastOutcome>,
    /// The tag whose "available" notice was dismissed.
    #[serde(default)]
    dismissed_version: Option<String>,
    /// The tag whose "failed" notice was dismissed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dismissed_failed: Option<String>,
    /// Compatibility markers for older processes sharing this cache.
    #[serde(default)]
    auto_update_announced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notified_available: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notified_failed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notified_installed: Option<String>,
    /// Counted install failures, all for `failure_tag`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure_tag: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    failure_count: u32,
    /// Fields from a newer Kettle, kept when this one rewrites the file.
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, serde_json::Value>,
}

/// What the last check decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LastOutcome {
    UpToDate,
    Available,
    Installing,
    RestartPending,
    Failed,
}

/// The first launch only seeds the throttle and skips the check. True when
/// it did; false when the record could not be written, because then every
/// launch would take this branch and none would ever check.
fn seed_first_run(
    cache: &mut UpdateCache,
    now: u64,
    save: impl FnOnce(&UpdateCache) -> bool,
) -> bool {
    cache.last_check = now;
    save(cache)
}

fn is_zero(count: &u32) -> bool {
    *count == 0
}

/// How many counted failures of one release make it worth telling the user.
/// At the default daily interval the notice comes on the third day.
const FAILURES_BEFORE_NOTICE: u32 = 3;

impl UpdateCache {
    fn note_up_to_date(&mut self, latest_tag: String) {
        self.latest_tag = Some(latest_tag);
        self.last_outcome = Some(LastOutcome::UpToDate);
    }

    /// Records an announced release; true when this is the first notice of
    /// it, or of a newer one, from any process.
    fn note_available(&mut self, tag: &str) -> bool {
        self.latest_tag = Some(tag.into());
        self.last_outcome = Some(LastOutcome::Available);
        claim(&mut self.notified_available, tag)
    }

    fn note_restart_pending(&mut self) {
        self.last_outcome = Some(LastOutcome::RestartPending);
    }

    fn note_installing(&mut self, tag: &str) {
        self.latest_tag = Some(tag.into());
        self.last_outcome = Some(LastOutcome::Installing);
    }

    /// Records a finished install: `(notify, first_time)`. Success clears
    /// the failure count.
    fn note_installed(&mut self, tag: &str) -> (bool, bool) {
        self.failure_tag = None;
        self.failure_count = 0;
        self.last_outcome = Some(LastOutcome::UpToDate);
        let first_time = !self.auto_update_announced;
        self.auto_update_announced = true;
        (claim(&mut self.notified_installed, tag), first_time)
    }

    /// Counts a failed install of `tag` unless the error says nothing about
    /// the release, such as a dropped connection. Returns `Some(notify)` once
    /// the failures are worth a notice; a new tag starts the count again.
    fn note_failure(&mut self, tag: &str, counts: bool) -> Option<bool> {
        if counts {
            if self.failure_tag.as_deref() == Some(tag) {
                self.failure_count = self.failure_count.saturating_add(1);
            } else {
                self.failure_tag = Some(tag.into());
                self.failure_count = 1;
            }
        }
        if self.failure_tag.as_deref() != Some(tag) || self.failure_count < FAILURES_BEFORE_NOTICE {
            return None;
        }
        self.last_outcome = Some(LastOutcome::Failed);
        Some(claim(&mut self.notified_failed, tag))
    }
}

/// Moves `seen` to `tag` if `tag` is newer; true when it did. A notice is then
/// sent once per tag, and never again for an older one.
fn claim(seen: &mut Option<String>, tag: &str) -> bool {
    let newer = match (seen.as_deref().and_then(tag_version), tag_version(tag)) {
        (Some(seen), Some(tag)) => tag > seen,
        (_, Some(_)) => true,
        (_, None) => seen.as_deref() != Some(tag),
    };
    if newer {
        *seen = Some(tag.into());
    }
    newer
}

fn tag_version(tag: &str) -> Option<kettle_update::Version> {
    kettle_update::Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

/// The longest a UI-side write waits for a check that holds the record's lock
/// through a download.
const RECORD_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// A day, the most a recorded check may lie in the future before it counts as
/// due anyway.
const DAY_SECS: u64 = 24 * 60 * 60;

fn is_due(now: u64, last_check: u64, interval_secs: u64) -> bool {
    if interval_secs == 0 || last_check == 0 {
        return false;
    }
    // A check recorded in the future, by a clock that ran ahead or a damaged
    // file, would otherwise hold off every check until the clock caught up.
    if last_check > now.saturating_add(DAY_SECS) {
        return true;
    }
    now.saturating_sub(last_check) >= interval_secs
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
    load_cache_at(&cache_path()?)
}

fn load_cache_at(path: &std::path::Path) -> Option<UpdateCache> {
    let file = kettle_state::open_trusted_file_read(path).ok()?;
    let size = file.metadata().ok()?.len();
    read_cache(file, size)
}

fn read_cache(reader: impl Read, size: u64) -> Option<UpdateCache> {
    read_record(reader, size)
}

fn read_record<T: serde::de::DeserializeOwned>(reader: impl Read, size: u64) -> Option<T> {
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

/// Writes the record; false when it could not.
fn save_cache(cache: &UpdateCache) -> bool {
    cache_path().is_some_and(|path| save_cache_at(&path, cache))
}

fn save_cache_at(path: &std::path::Path, cache: &UpdateCache) -> bool {
    if let Some(parent) = path.parent()
        && kettle_state::create_private_dirs(parent).is_err()
    {
        return false;
    }
    let Ok(json) = serde_json::to_vec_pretty(cache) else {
        return false;
    };
    match kettle_update::write_atomic_file(path, &json) {
        Ok(()) => true,
        Err(error) => {
            log::warn!("update-check: could not persist cache: {error}");
            false
        }
    }
}

/// Reads, changes and writes the record. Callers hold `update-check.lock`, so
/// reading again right before the write keeps another process's change, such
/// as a dismissal, instead of overwriting it with an older copy.
fn mutate_cache<T>(change: impl FnOnce(&mut UpdateCache) -> T) -> T {
    match cache_path() {
        Some(path) => mutate_cache_at(&path, change),
        None => change(&mut UpdateCache::default()),
    }
}

fn mutate_cache_at<T>(path: &std::path::Path, change: impl FnOnce(&mut UpdateCache) -> T) -> T {
    let mut cache = load_cache_at(path).unwrap_or_default();
    let result = change(&mut cache);
    save_cache_at(path, &cache);
    result
}

/// Runs `change` on the record under `update-check.lock`, waiting up to
/// `RECORD_LOCK_WAIT` for a check that holds it; false when it could not.
fn with_locked_cache(change: impl FnOnce(&mut UpdateCache)) -> bool {
    let Some(path) = cache_path() else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return false;
    };
    if kettle_state::create_private_dirs(parent).is_err() {
        return false;
    }
    match kettle_state::ExclusiveFileLock::acquire_timeout(
        &parent.join("update-check.lock"),
        RECORD_LOCK_WAIT,
    ) {
        Ok(_lock) => {
            mutate_cache_at(&path, change);
            true
        }
        Err(error) => {
            log::warn!("update-check: could not record a change: {error}");
            false
        }
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
        if seed_first_run(&mut cache, now, save_cache) {
            return;
        }
        log::warn!("update-check: the record is not writable; checking without it");
    } else if !is_due(now, cache.last_check, interval_secs) {
        return;
    }
    // Persist the throttle before starting network I/O. The process guard and
    // OS file lock make the read/update decision exclusive across every window
    // and process sharing this config directory.
    mutate_cache(|cache| cache.last_check = now);
    let _ = std::thread::Builder::new()
        .name("kettle-update-check".into())
        .spawn(move || {
            let _in_flight = in_flight;
            let client = FeedClient::new();
            let outcome = client.check(current);
            let latest_update = match outcome {
                Ok(CheckOutcome::UpToDate { latest }) => {
                    mutate_cache(|cache| {
                        cache.last_check = now_secs();
                        cache.note_up_to_date(format!("v{latest}"));
                    });
                    None
                }
                Ok(CheckOutcome::UpdateAvailable(update)) => {
                    mutate_cache(|cache| {
                        cache.last_check = now_secs();
                        cache.latest_tag = Some(update.tag.clone());
                    });
                    Some(update)
                }
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
                mutate_cache(UpdateCache::note_restart_pending);
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
                let notify = mutate_cache(|cache| cache.note_available(&update.tag));
                let _ = proxy.send_event(UserEvent::UpdateAvailable {
                    tag: update.tag,
                    notify,
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
                Step::Install => {
                    mutate_cache(|cache| cache.note_installing(&update.tag));
                    match install.install(&client, &update) {
                        Ok(outcome) => {
                            let staged = matches!(
                                outcome.disposition,
                                kettle_update::InstallDisposition::Staged { .. }
                            );
                            let (notify, first_time) =
                                mutate_cache(|cache| cache.note_installed(&update.tag));
                            let _ = proxy.send_event(UserEvent::UpdateInstalled {
                                tag: update.tag,
                                staged,
                                first_time,
                                notify,
                            });
                        }
                        Err(error) => {
                            match classify(&error, &running, install.proven_at_startup()) {
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
                                    log::info!(
                                        "update {} not installed now: {error}",
                                        update.tag
                                    );
                                }
                                InstallFailure::Failed => {
                                    // The raw error can hold paths and is not
                                    // translated: it goes to the log only.
                                    log::warn!("automatic update to {} failed: {error}", update.tag);
                                    let counts = failure_counts(&error);
                                    if let Some(notify) = mutate_cache(|cache| {
                                        cache.note_failure(&update.tag, counts)
                                    }) {
                                        let _ = proxy.send_event(UserEvent::UpdateFailed {
                                            tag: update.tag,
                                            notify,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
}

/// Whether a failed install says something about the release. A dropped
/// connection does not; the next check simply tries again.
fn failure_counts(error: &kettle_update::UpdateError) -> bool {
    !matches!(error, kettle_update::UpdateError::Request(_))
}

/// What is true about updates for this process, from the shared record and
/// the install found at startup. Reads small local files, and on macOS runs
/// `PlistBuddy`; never the network. Call it off the UI thread.
pub(crate) fn read_facts(
    install: &InstallSnapshot,
    policy: UpdatePolicy,
    current: &str,
) -> crate::update_notice::UpdateFacts {
    let Ok(running) = kettle_update::Version::parse(current) else {
        return crate::update_notice::UpdateFacts::default();
    };
    facts_from(
        &load_cache().unwrap_or_default(),
        &install.local_state(),
        install.kind(),
        policy,
        &running,
    )
}

fn facts_from(
    record: &UpdateCache,
    local: &kettle_update::decision::LocalState,
    kind: kettle_update::decision::InstallKind,
    policy: UpdatePolicy,
    running: &kettle_update::Version,
) -> crate::update_notice::UpdateFacts {
    use crate::update_notice::{Release, ReleaseState, UpdateFacts};
    use kettle_update::decision::{InstallKind, InstalledVersion};
    let installed_newer = match decide(running, kind, local, Policy::Notify, None) {
        Decision::RestartPending { installed } => Some(format!("v{installed}")),
        _ => None,
    };
    let installed = match (kind, &local.installed) {
        (InstallKind::Managed, InstalledVersion::Known(version)) if version > running => version,
        _ => running,
    };
    let latest = record
        .latest_tag
        .as_deref()
        .and_then(|tag| tag_version(tag).map(|version| (tag, version)));
    let release = latest
        .as_ref()
        .filter(|(_, version)| version > installed)
        .map(|(tag, _)| {
            let state = if record.failure_tag.as_deref() == Some(*tag)
                && record.failure_count >= FAILURES_BEFORE_NOTICE
            {
                ReleaseState::Failed
            } else if record.last_outcome == Some(LastOutcome::Installing) {
                ReleaseState::Installing
            } else {
                ReleaseState::Available
            };
            Release {
                tag: (*tag).to_string(),
                state,
            }
        });
    let up_to_date = latest
        .filter(|(_, version)| version <= running && installed_newer.is_none())
        .map(|(tag, _)| tag.to_string());
    UpdateFacts {
        off: policy == UpdatePolicy::Off,
        running: format!("v{running}"),
        release,
        installed_newer,
        up_to_date,
        managed: kind == InstallKind::Managed,
        dismissed_available: record.dismissed_version.clone(),
        dismissed_failed: record.dismissed_failed.clone(),
    }
}

/// Records the dismissal of a failed update's notice for every process; see
/// [`record_dismissed`].
pub fn record_dismissed_failed(tag: &str) {
    let tag = tag.to_string();
    let _ = std::thread::Builder::new()
        .name("kettle-update-dismiss".into())
        .spawn(move || {
            with_locked_cache(|cache| cache.dismissed_failed = Some(tag));
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

/// Records the dismissal of `tag`'s notice. A check can hold the record's lock
/// through a whole download, so this waits for it on a short-lived thread
/// instead of the caller's; the caller hides the notice at once.
pub fn record_dismissed(tag: &str) {
    let tag = tag.to_string();
    let _ = std::thread::Builder::new()
        .name("kettle-update-dismiss".into())
        .spawn(move || {
            with_locked_cache(|cache| cache.dismissed_version = Some(tag));
        });
}

pub fn run_blocking_check(current: &str) -> String {
    match FeedClient::new().check(current) {
        Ok(CheckOutcome::UpdateAvailable(update)) => {
            with_locked_cache(|cache| {
                cache.last_check = now_secs();
                cache.latest_tag = Some(update.tag.clone());
            });
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

    use super::{
        CheckLockAttempt, InstallFailure, LastOutcome, Step, UpdateCache, classify, is_due,
        next_step,
    };

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
            ..UpdateCache::default()
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
    fn a_check_recorded_in_the_future_is_due() {
        let now = 1_000_000_000;
        assert!(is_due(now, now + DAY + 1, DAY));
        assert!(is_due(now, u64::MAX, DAY));
        // Clock skew within a day still waits.
        assert!(!is_due(now, now + 60, DAY));
    }

    #[test]
    fn an_unwritable_record_does_not_skip_every_check() {
        let mut cache = UpdateCache::default();
        assert!(super::seed_first_run(&mut cache, 42, |_| true));
        let mut cache = UpdateCache::default();
        assert!(!super::seed_first_run(&mut cache, 42, |_| false));
    }

    #[test]
    fn old_cache_loads_and_unknown_fields_survive_rewrite() {
        let cache: UpdateCache = serde_json::from_str(
            r#"{"last_check":42,"dismissed_version":"v5.1.0","from_a_newer_kettle":{"n":7}}"#,
        )
        .unwrap();
        assert_eq!(cache.dismissed_version.as_deref(), Some("v5.1.0"));
        let written = serde_json::to_value(&cache).unwrap();
        assert_eq!(written["from_a_newer_kettle"]["n"], 7);
    }

    /// Several processes check and notify; each kind of notice goes out once
    /// per tag, and never again for an older one.
    #[test]
    fn a_tag_is_notified_once_per_state_across_writers() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("update-check.json");
        let lock = dir.path().join("update-check.lock");
        let writers: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                let lock = lock.clone();
                std::thread::spawn(move || {
                    let _held = kettle_state::ExclusiveFileLock::acquire(&lock).unwrap();
                    super::mutate_cache_at(&path, |cache| cache.note_available("v5.2.0"))
                })
            })
            .collect();
        let notified = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .filter(|notify| *notify)
            .count();
        assert_eq!(notified, 1, "exactly one writer sends the notice");
        let mut cache = super::load_cache_at(&path).unwrap();
        assert_eq!(cache.notified_available.as_deref(), Some("v5.2.0"));
        assert!(!cache.note_available("v5.2.0"));
        assert!(!cache.note_available("v5.1.9"));
        assert!(cache.note_available("v5.2.1"));
    }

    #[test]
    fn third_counted_failure_for_one_tag_is_failed() {
        let mut cache = UpdateCache::default();
        assert_eq!(cache.note_failure("v5.2.0", true), None);
        assert_eq!(cache.note_failure("v5.2.0", true), None);
        assert_eq!(cache.note_failure("v5.2.0", true), Some(true));
        assert_eq!(cache.last_outcome, Some(LastOutcome::Failed));
        // Notified once per tag.
        assert_eq!(cache.note_failure("v5.2.0", true), Some(false));
    }

    #[test]
    fn new_tag_or_success_resets_count() {
        let mut cache = UpdateCache::default();
        cache.note_failure("v5.2.0", true);
        cache.note_failure("v5.2.0", true);
        assert_eq!(cache.note_failure("v5.2.1", true), None);
        assert_eq!(cache.failure_count, 1);
        cache.note_installed("v5.2.1");
        assert_eq!(cache.failure_count, 0);
        assert_eq!(cache.failure_tag, None);
    }

    #[test]
    fn locked_feed_and_network_errors_do_not_count() {
        assert!(!super::failure_counts(&UpdateError::Request(
            "timed out".into()
        )));
        assert!(super::failure_counts(&UpdateError::HashMismatch));
        let mut cache = UpdateCache::default();
        for _ in 0..5 {
            assert_eq!(cache.note_failure("v5.2.0", false), None);
        }
        assert_eq!(cache.failure_count, 0);
    }

    /// A check holds the record's lock through a download. A dismissal
    /// written before its update of the record must survive it: the check
    /// reads the record again right before writing.
    #[test]
    fn a_dismissal_written_mid_check_survives() {
        let dir = crate::test_tempdir();
        let path = dir.path().join("update-check.json");
        super::mutate_cache_at(&path, |cache| cache.last_check = 42);
        super::mutate_cache_at(&path, |cache| {
            cache.dismissed_version = Some("v5.2.0".into());
        });
        super::mutate_cache_at(&path, |cache| cache.note_available("v5.2.0"));
        let cache = super::load_cache_at(&path).unwrap();
        assert_eq!(cache.dismissed_version.as_deref(), Some("v5.2.0"));
        assert_eq!(cache.last_check, 42);
    }

    #[test]
    fn up_to_date_records_latest_tag() {
        let mut cache = UpdateCache::default();
        cache.note_up_to_date("v5.1.0".into());
        assert_eq!(cache.latest_tag.as_deref(), Some("v5.1.0"));
        assert_eq!(cache.last_outcome, Some(LastOutcome::UpToDate));
    }

    /// An update is never urgent enough to ask for the user's attention.
    #[test]
    fn no_update_event_requests_attention() {
        let source = kettle_test_support::production_source(include_str!("app.rs"));
        for event in [
            "UserEvent::UpdateAvailable {",
            "UserEvent::UpdateInstalled {",
            "UserEvent::UpdateFailed {",
        ] {
            let arm = source
                .split(event)
                .nth(1)
                .and_then(|rest| rest.split("            UserEvent::").next())
                .unwrap_or_else(|| panic!("{event} has a handler"));
            assert!(!arm.contains("request_user_attention"), "{event}");
        }
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

/// The caller runs delivery transactions off the event loop.
pub(crate) fn claim_toast(notice: crate::update_notice::Notice) -> crate::update_toast::ClaimReply {
    use crate::update_toast::ClaimReply;
    let Some(path) = toast_path() else {
        return ClaimReply::Unavailable;
    };
    toast_transaction(&path, |ledger| {
        let now = now_secs();
        if let Some(claim) = ledger.claim(
            notice,
            now,
            std::process::id(),
            kettle_ctl::presence::process_start_token(std::process::id()),
        ) {
            ClaimReply::Acquired(claim)
        } else if let Some(until) = ledger.busy_until(now) {
            ClaimReply::Busy(until)
        } else {
            ClaimReply::Seen
        }
    })
    .unwrap_or_else(|error| {
        log::warn!("update toast receipt unavailable: {error}");
        if error.kind() == std::io::ErrorKind::TimedOut {
            ClaimReply::Busy(now_secs().saturating_add(2))
        } else {
            ClaimReply::Unavailable
        }
    })
}

pub(crate) fn finish_toast(claim: &crate::update_toast::Claim, seen: bool) -> bool {
    toast_path().and_then(|path| toast_transaction(&path, |ledger| ledger.finish(claim, seen)).ok())
        == Some(true)
}

fn toast_path() -> Option<std::path::PathBuf> {
    cache_path().map(|path| path.with_file_name("update-toasts.json"))
}

#[derive(Default, Serialize, Deserialize)]
struct ToastRecord {
    #[serde(default)]
    toast: crate::update_toast::Ledger,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, serde_json::Value>,
}

fn toast_transaction<T>(
    path: &std::path::Path,
    change: impl FnOnce(&mut crate::update_toast::Ledger) -> T,
) -> std::io::Result<T> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing update record directory"))?;
    kettle_state::create_private_dirs(parent)?;
    let _lock = kettle_state::ExclusiveFileLock::acquire_timeout(
        &parent.join("update-toasts.lock"),
        std::time::Duration::from_secs(2),
    )?;
    let mut cache = match kettle_state::open_trusted_file_read(path) {
        Ok(file) => read_record::<ToastRecord>(&file, file.metadata()?.len()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unreadable delivery record",
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ToastRecord::default(),
        Err(error) => return Err(error),
    };
    let result = change(&mut cache.toast);
    let bytes = serde_json::to_vec(&cache)?;
    if bytes.len() > MAX_CACHE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "delivery record exceeds its limit",
        ));
    }
    kettle_update::write_atomic_file(path, &bytes).map_err(std::io::Error::other)?;
    Ok(result)
}

#[cfg(test)]
mod toast_tests {
    use super::*;
    use crate::update_notice::{Notice, NoticeKind};

    fn notice() -> Notice {
        Notice {
            kind: NoticeKind::Available,
            tag: "v6.0.0".into(),
        }
    }

    #[test]
    fn overlapping_fact_refreshes_skip_the_old_result_and_coalesce_reads() {
        let mut refresh = FactsRefresh::default();
        assert!(refresh.begin());
        for _ in 0..100 {
            assert!(!refresh.begin());
        }
        assert!(
            !refresh.complete(),
            "the old result must not replace newly requested facts"
        );
        assert!(refresh.begin());
        assert!(refresh.complete(), "the fresh result can be applied");
        assert!(refresh.begin());
    }

    #[test]
    fn update_download_locks_do_not_block_delivery_receipts() {
        let directory = kettle_test_support::private_tempdir("kettle-toast-download-");
        let _download = kettle_state::ExclusiveFileLock::acquire_timeout(
            &directory.path().join("update-check.lock"),
            std::time::Duration::ZERO,
        )
        .unwrap();
        let path = directory.path().join("update-toasts.json");
        let claim = toast_transaction(&path, |ledger| ledger.claim(notice(), 100, 1, None))
            .unwrap()
            .unwrap();
        assert!(toast_transaction(&path, |ledger| ledger.finish(&claim, true)).unwrap());
    }

    #[test]
    fn delivery_does_not_publish_a_record_it_cannot_read_back() {
        let directory = kettle_test_support::private_tempdir("kettle-toast-limit-");
        let path = directory.path().join("update-toasts.json");
        let original =
            serde_json::to_vec(&serde_json::json!({"future": "x".repeat(MAX_CACHE_BYTES - 64)}))
                .unwrap();
        assert!(original.len() < MAX_CACHE_BYTES);
        std::fs::write(&path, &original).unwrap();
        assert!(toast_transaction(&path, |ledger| ledger.claim(notice(), 100, 1, None)).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn concurrent_delivery_claims_preserve_existing_and_future_cache_fields() {
        let directory = kettle_test_support::private_tempdir("kettle-toast-cache-");
        let path = directory.path().join("update-check.json");
        std::fs::write(
            &path,
            br#"{"last_check":17,"dismissed_version":"v5.0.0","future":{"keep":true}}"#,
        )
        .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let jobs: Vec<_> = (1..=4)
            .map(|owner| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    toast_transaction(&path, |ledger| {
                        ledger.claim(notice(), 100, owner, Some(owner as u64))
                    })
                    .unwrap()
                })
            })
            .collect();
        let claims: Vec<_> = jobs
            .into_iter()
            .filter_map(|job| job.join().unwrap())
            .collect();
        assert_eq!(claims.len(), 1);
        toast_transaction(&path, |ledger| ledger.finish(&claims[0], true)).unwrap();
        assert!(
            toast_transaction(&path, |ledger| ledger.claim(notice(), 200, 5, Some(5)))
                .unwrap()
                .is_none()
        );
        let data: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(data["last_check"], 17);
        assert_eq!(data["dismissed_version"], "v5.0.0");
        assert_eq!(data["future"]["keep"], true);
    }

    #[test]
    fn delivery_preserves_an_unreadable_update_record() {
        let directory = kettle_test_support::private_tempdir("kettle-toast-corrupt-");
        let path = directory.path().join("update-check.json");
        let original = b"{incomplete record";
        std::fs::write(&path, original).unwrap();
        assert!(toast_transaction(&path, |ledger| ledger.claim(notice(), 100, 1, None)).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn delivery_does_not_report_success_when_its_record_cannot_be_written() {
        let directory = kettle_test_support::private_tempdir("kettle-toast-cache-");
        let file = directory.path().join("file");
        std::fs::write(&file, b"preserve").unwrap();
        assert!(
            toast_transaction(&file.join("record.json"), |ledger| ledger.claim(
                notice(),
                100,
                1,
                None
            ))
            .is_err()
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"preserve");
    }
}
