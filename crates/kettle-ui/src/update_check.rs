//! Cycle 794: in-app "a newer kettle release is available" checker.
//!
//! Notify-only — never downloads or installs anything (that would pull in a
//! whole signed-artifact / downgrade / elevation security boundary kettle
//! doesn't own; the OS package manager / release page is the correct
//! installer). It does ONE unauthenticated GET of the GitHub "latest release"
//! endpoint on a background thread, compares the tag to the running version,
//! and — if newer — wakes the UI with `UserEvent::UpdateAvailable` so a
//! dismissable bottom-bar overlay + a single desktop toast can appear.
//!
//! Privacy + politeness guardrails (the lessons other terminals learned the
//! hard way):
//!   * **opt-out** via `update-check = false` (default on).
//!   * **never on the first launch** — the first run only stamps the cache and
//!     skips the network call, so kettle doesn't phone home the instant you
//!     open it.
//!   * **throttled to once / 24h** via a cache file in the config dir, shared
//!     across windows (one window stamps `last_check`, the others see "not
//!     due"), so multiple windows don't hammer GitHub's 60-req/hr/IP limit.
//!   * **no re-nagging** — a dismissed version is remembered; only a *newer*
//!     tag re-triggers.
//!   * **fail silent** — offline / timeout / rate-limited / malformed all just
//!     `log::warn` and return; never a blocking dialog, never a panic.
//!   * **source-from-distro builds opt out at compile time** — a build that
//!     sets `KETTLE_PACKAGED` in its env (a distro compiling from source with
//!     its own update channel) compiles the auto-check into a no-op. NOTE: the
//!     official prebuilt binaries are deliberately NOT built that way, so a
//!     directly-downloaded kettle does check; the Homebrew/AUR packages
//!     *repackage those same prebuilt binaries*, so they check too. The runtime
//!     `update-check = false` opt-out (and `--check-update`) apply regardless.

use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

/// GitHub "latest (non-prerelease) release" endpoint for this repo.
const RELEASES_URL: &str = "https://api.github.com/repos/Reddimus/kettle/releases/latest";
/// Default throttle: check at most once per 24h.
const DEFAULT_INTERVAL_SECS: u64 = 24 * 60 * 60;
/// Network timeouts — the check runs off-thread so this never stalls input,
/// but keep it short so a flaky network doesn't leave a thread hanging long.
const NET_TIMEOUT_SECS: u64 = 5;
/// Cycle 901 (audit): overall (whole-request) deadline. The per-phase
/// `timeout_read` resets on every byte received, so a server that trickles one
/// byte at a time could keep the thread — and the synchronous `--check-update`
/// — alive indefinitely. This caps the entire fetch (connect + TLS + body)
/// regardless of how the bytes dribble in. Generous enough (15s) that a legit
/// slow connection (slow DNS + TLS + body, each under the 5s per-phase cap)
/// still completes, but a trickle attack is bounded instead of unbounded.
const NET_TOTAL_TIMEOUT_SECS: u64 = 15;
/// Cap the response we'll read so a hostile/huge body can't balloon memory.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// Set in the build env (any value) by a distro that compiles kettle from
/// source and owns its own update channel, to compile the auto-check out.
/// The official prebuilt binaries are NOT built with it (so a direct download
/// auto-checks), and the Homebrew/AUR packages repackage those binaries, so
/// they check too. `--check-update` + `update-check = false` work regardless.
const PACKAGED: bool = option_env!("KETTLE_PACKAGED").is_some();

/// The two release fields we care about. `#[serde(default)]` + ignoring the
/// dozens of other fields GitHub sends keeps this forward-compatible.
#[derive(Debug, Deserialize)]
struct ReleaseInfo {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    html_url: String,
}

/// On-disk throttle + anti-nag state (`<config-dir>/update-check.json`).
#[derive(Debug, Default, Serialize, Deserialize)]
struct UpdateCache {
    /// Unix seconds of the last completed check (or first-launch stamp).
    #[serde(default)]
    last_check: u64,
    /// Latest tag seen from GitHub (informational / for the manual command).
    #[serde(default)]
    latest_tag: Option<String>,
    /// A tag the user dismissed — never re-nag for this exact version.
    #[serde(default)]
    dismissed_version: Option<String>,
}

/// Parse a `vX.Y.Z` (or `X.Y.Z`) tag into a comparable tuple. Returns `None`
/// for anything that isn't exactly three non-negative integers, so a malformed
/// or pre-release-suffixed tag is treated as "can't compare" (never panics).
fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().strip_prefix('v').unwrap_or(s.trim());
    let mut it = s.split('.');
    let major = it.next()?.parse::<u32>().ok()?;
    let minor = it.next()?.parse::<u32>().ok()?;
    let patch = it.next()?.parse::<u32>().ok()?;
    if it.next().is_some() {
        return None; // more than three components → not a plain release tag
    }
    Some((major, minor, patch))
}

/// Is `latest` a strictly newer release than `current`? `false` (no nag) if
/// either side is unparseable — we only ever prompt on a confident upgrade.
fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Throttle decision (pure — no I/O, no clock). A check is due when at least
/// `interval` has elapsed since `last_check`. `last_check == 0` (a fresh/empty
/// cache) is treated as "not due" so the caller can stamp-and-skip the very
/// first launch instead of phoning home immediately.
fn is_due(now: u64, last_check: u64, interval_secs: u64) -> bool {
    if interval_secs == 0 || last_check == 0 {
        return false;
    }
    now.saturating_sub(last_check) >= interval_secs
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `<config-dir>/update-check.json`, alongside `session.json` / `layouts/`.
fn cache_path() -> Option<std::path::PathBuf> {
    kettle_config::Config::default_path()
        .and_then(|p| p.parent().map(|d| d.join("update-check.json")))
}

fn load_cache() -> Option<UpdateCache> {
    let p = cache_path()?;
    let bytes = std::fs::read(&p).ok()?;
    if bytes.len() > MAX_BODY_BYTES {
        return None; // implausible for this tiny file → ignore rather than parse
    }
    serde_json::from_slice(&bytes).ok()
}

/// Atomically persist the cache (tmp-sibling + rename), mirroring `session.rs`
/// so a kill mid-write can't corrupt it. Best-effort: failure just `log::warn`s.
fn save_cache(cache: &UpdateCache) {
    let Some(p) = cache_path() else { return };
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(json) = serde_json::to_vec_pretty(cache) else {
        return;
    };
    let tmp = p.with_extension(format!("tmp.{}", std::process::id()));
    if std::fs::write(&tmp, &json).is_ok()
        && let Err(e) = std::fs::rename(&tmp, &p)
    {
        log::warn!("update-check: could not persist cache: {e}");
        let _ = std::fs::remove_file(&tmp);
    }
}

/// What a (manual or background) check resolved to.
pub enum CheckOutcome {
    /// A strictly newer release than `current` is available.
    UpdateAvailable { tag: String, url: String },
    /// Already on the latest (or a newer dev build).
    UpToDate,
    /// Couldn't determine (offline, rate-limited, parse error, …).
    Unknown(String),
}

/// Do the actual network fetch + parse + version compare. Blocking; callers run
/// it on a background thread (auto-check) or the main thread (`--check-update`).
fn fetch_outcome(current: &str) -> CheckOutcome {
    let ua = format!(
        "kettle/{} (+https://github.com/Reddimus/kettle)",
        env!("CARGO_PKG_VERSION")
    );
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(NET_TIMEOUT_SECS))
        .timeout_read(Duration::from_secs(NET_TIMEOUT_SECS))
        // Cycle 901 (audit): overall deadline so a trickling server can't keep
        // the request alive past this by resetting the per-byte read timeout.
        .timeout(Duration::from_secs(NET_TOTAL_TIMEOUT_SECS))
        .build();
    let resp = match agent
        .get(RELEASES_URL)
        // GitHub 403s a request with no/!valid User-Agent.
        .set("User-Agent", &ua)
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .call()
    {
        Ok(r) => r,
        Err(e) => return CheckOutcome::Unknown(format!("request failed: {e}")),
    };
    // Bound the body so a hostile/huge response can't balloon memory.
    let mut body = String::new();
    if let Err(e) = resp
        .into_reader()
        .take(MAX_BODY_BYTES as u64)
        .read_to_string(&mut body)
    {
        return CheckOutcome::Unknown(format!("read failed: {e}"));
    }
    let info: ReleaseInfo = match serde_json::from_str(&body) {
        Ok(i) => i,
        Err(e) => return CheckOutcome::Unknown(format!("parse failed: {e}")),
    };
    // Don't suggest a pre-release as an "update" to a stable build.
    if info.prerelease {
        return CheckOutcome::UpToDate;
    }
    if is_newer(&info.tag_name, current) {
        let url = if info.html_url.is_empty() {
            "https://github.com/Reddimus/kettle/releases/latest".to_string()
        } else {
            info.html_url
        };
        CheckOutcome::UpdateAvailable {
            tag: info.tag_name,
            url,
        }
    } else {
        CheckOutcome::UpToDate
    }
}

/// Called once from `App::resumed`. Decides — using the cache (first-launch
/// skip + 24h throttle) — whether a check is due, and if so spawns a named
/// background thread that fetches and, on a confident newer + non-dismissed
/// release, sends `UserEvent::UpdateAvailable`. No-op in packaged builds.
pub fn maybe_spawn_check(proxy: EventLoopProxy<UserEvent>, current: &'static str) {
    if PACKAGED {
        return;
    }
    let now = now_secs();
    let mut cache = load_cache().unwrap_or_default();

    // First ever launch (or a wiped cache): stamp and skip the network call —
    // kettle should not phone home the instant it's first opened.
    if cache.last_check == 0 {
        cache.last_check = now;
        save_cache(&cache);
        return;
    }
    if !is_due(now, cache.last_check, DEFAULT_INTERVAL_SECS) {
        return;
    }

    let dismissed = cache.dismissed_version.clone();
    std::thread::Builder::new()
        .name("kettle-update-check".into())
        .spawn(move || {
            let outcome = fetch_outcome(current);
            // Stamp the check time regardless of outcome so a failed/ratelimited
            // attempt still backs off the full interval (don't retry-storm).
            let mut cache = load_cache().unwrap_or_default();
            cache.last_check = now_secs();
            if let CheckOutcome::UpdateAvailable { tag, .. } = &outcome {
                cache.latest_tag = Some(tag.clone());
            }
            save_cache(&cache);

            if let CheckOutcome::UpdateAvailable { tag, url } = outcome {
                // Respect a prior dismissal of this exact version.
                if dismissed.as_deref() == Some(tag.as_str()) {
                    return;
                }
                // Loop may already be gone (window closed mid-fetch) — ignore.
                let _ = proxy.send_event(UserEvent::UpdateAvailable { tag, url });
            }
        })
        .ok();
}

/// Persist that the user dismissed `tag`, so it never re-nags (only a newer
/// tag will). Called from the Esc / open handlers.
pub fn record_dismissed(tag: &str) {
    let mut cache = load_cache().unwrap_or_default();
    cache.dismissed_version = Some(tag.to_string());
    save_cache(&cache);
}

/// Synchronous check for the `kettle --check-update` CLI flag. Bypasses the
/// throttle (the user asked explicitly) and returns a human-readable line. This
/// runs even in packaged builds — only the *automatic* background check is
/// suppressed there; a deliberate manual check is always honored (and notes the
/// package-manager path, since kettle never self-installs).
pub fn run_blocking_check(current: &str) -> String {
    let how_to = if PACKAGED {
        "update via your package manager"
    } else {
        "see the release page"
    };
    match fetch_outcome(current) {
        CheckOutcome::UpdateAvailable { tag, url } => {
            // Refresh the cache so the background path agrees on `last_check`.
            let mut cache = load_cache().unwrap_or_default();
            cache.last_check = now_secs();
            cache.latest_tag = Some(tag.clone());
            save_cache(&cache);
            format!("update available: {tag} (you have {current}) — {how_to}\n  {url}")
        }
        CheckOutcome::UpToDate => format!("kettle {current} is up to date"),
        CheckOutcome::Unknown(why) => format!("could not check for updates: {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_INTERVAL_SECS, is_due, is_newer, parse_version};

    /// Cycle 901 (audit): the fetch must set an OVERALL request deadline, not
    /// just per-phase read/connect timeouts — a trickling server resets the
    /// per-byte read timeout and could otherwise keep the thread (and the
    /// synchronous `--check-update`) alive indefinitely. A behavioral test
    /// needs a malicious slow server; pin the overall `.timeout(...)` at the
    /// source level.
    #[test]
    fn fetch_sets_overall_request_timeout() {
        let src = include_str!("update_check.rs");
        assert!(
            src.contains(".timeout(Duration::from_secs(NET_TOTAL_TIMEOUT_SECS))"),
            "fetch_outcome must set an overall request deadline so a trickling \
             server can't keep the request alive past it"
        );
    }

    #[test]
    fn parse_version_handles_v_prefix_and_rejects_junk() {
        assert_eq!(parse_version("v2.5.0"), Some((2, 5, 0)));
        assert_eq!(parse_version("2.5.0"), Some((2, 5, 0)));
        assert_eq!(parse_version(" v10.20.30 "), Some((10, 20, 30)));
        // Not exactly three integer components → None (never panics).
        assert_eq!(parse_version("2.5"), None);
        assert_eq!(parse_version("2.5.0.1"), None);
        assert_eq!(parse_version("v2.5.0-rc1"), None);
        assert_eq!(parse_version("abc"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("v2.x.0"), None);
    }

    #[test]
    fn is_newer_only_on_confident_strict_upgrade() {
        assert!(is_newer("v2.6.0", "2.5.0"));
        assert!(is_newer("v2.5.1", "2.5.0"));
        assert!(is_newer("v3.0.0", "2.9.9"));
        assert!(!is_newer("v2.5.0", "2.5.0")); // same
        assert!(!is_newer("v2.4.9", "2.5.0")); // older → never suggest downgrade
        // Unparseable either side → no nag (fail safe).
        assert!(!is_newer("garbage", "2.5.0"));
        assert!(!is_newer("v2.6.0", "garbage"));
        assert!(!is_newer("v2.6.0-rc1", "2.5.0"));
    }

    #[test]
    fn is_due_respects_first_launch_and_interval() {
        let day = DEFAULT_INTERVAL_SECS;
        // Fresh/empty cache (last_check == 0) is never "due" → stamp-and-skip.
        assert!(!is_due(1_000_000, 0, day));
        // Interval not yet elapsed.
        assert!(!is_due(1_000_000, 1_000_000 - (day - 1), day));
        // Exactly / more than the interval elapsed → due.
        assert!(is_due(1_000_000, 1_000_000 - day, day));
        assert!(is_due(1_000_000, 1, day));
        // interval == 0 disables checking entirely.
        assert!(!is_due(1_000_000, 1, 0));
        // Clock skew (last_check in the future) must not over/underflow.
        assert!(!is_due(1_000, 5_000, day));
    }

    #[test]
    fn update_cache_serde_round_trips_and_tolerates_partial_files() {
        use super::UpdateCache;
        let cache = UpdateCache {
            last_check: 1_700_000_000,
            latest_tag: Some("v2.7.0".to_string()),
            dismissed_version: Some("v2.7.0".to_string()),
        };
        let json = serde_json::to_string(&cache).expect("serialize");
        let back: UpdateCache = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.last_check, cache.last_check);
        assert_eq!(back.latest_tag, cache.latest_tag);
        assert_eq!(back.dismissed_version, cache.dismissed_version);

        // Forward/back compatibility: a partial or older cache file (missing
        // fields) must load via serde defaults rather than failing — a future
        // schema field can't brick the throttle / anti-nag state on disk.
        let partial: UpdateCache =
            serde_json::from_str(r#"{"last_check":42}"#).expect("partial loads");
        assert_eq!(partial.last_check, 42);
        assert_eq!(partial.latest_tag, None);
        assert_eq!(partial.dismissed_version, None);
        let empty: UpdateCache = serde_json::from_str("{}").expect("empty object loads");
        assert_eq!(empty.last_check, 0);
        assert_eq!(empty.dismissed_version, None);
    }
}
