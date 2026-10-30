use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScreenshotWindowAvailability {
    Available,
    Unavailable,
    /// The backend cannot report visibility/minimization (notably Wayland).
    /// Capture remains eligible, but its bounded timeout is the only fallback
    /// if the compositor withholds work for a state the client cannot observe.
    Unknown,
}

pub(super) fn screenshot_window_availability(
    minimized: Option<bool>,
    window_shown: bool,
    visible: Option<bool>,
) -> ScreenshotWindowAvailability {
    if !window_shown || minimized == Some(true) || visible == Some(false) {
        ScreenshotWindowAvailability::Unavailable
    } else if minimized.is_none() || visible.is_none() {
        ScreenshotWindowAvailability::Unknown
    } else {
        ScreenshotWindowAvailability::Available
    }
}

pub(super) fn screenshot_window_accepts_control_capture(
    minimized: Option<bool>,
    window_shown: bool,
    visible: Option<bool>,
) -> bool {
    screenshot_window_availability(minimized, window_shown, visible)
        != ScreenshotWindowAvailability::Unavailable
}

/// Parse an optional explicit full-window screenshot crop. The four values are
/// one unit: accepting a partial rectangle would make a typo silently capture
/// more terminal content than the caller intended.
pub(super) fn screenshot_crop(
    params: &serde_json::Value,
) -> Result<Option<(f32, f32, f32, f32)>, &'static str> {
    const KEYS: [&str; 4] = ["crop_x", "crop_y", "crop_width", "crop_height"];
    let present = KEYS.map(|key| params.get(key).is_some());
    if !present.into_iter().any(|value| value) {
        return Ok(None);
    }
    if !present.into_iter().all(|value| value) {
        return Err("crop_x, crop_y, crop_width, and crop_height must be provided together");
    }
    let read = |key| {
        params
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite() && *value <= f64::from(f32::MAX))
            .map(|value| value as f32)
            .ok_or("screenshot crop values must be finite numbers within the f32 range")
    };
    let crop = (
        read("crop_x")?,
        read("crop_y")?,
        read("crop_width")?,
        read("crop_height")?,
    );
    if crop.0 < 0.0 || crop.1 < 0.0 {
        return Err("crop_x and crop_y must be non-negative");
    }
    if crop.2 <= 0.0 || crop.3 <= 0.0 {
        return Err("crop_width and crop_height must be positive");
    }
    Ok(Some(crop))
}

pub(super) fn pending_screenshot_may_render_offscreen(
    pending: bool,
    minimized: Option<bool>,
    window_shown: bool,
    visible: Option<bool>,
) -> bool {
    pending && screenshot_window_accepts_control_capture(minimized, window_shown, visible)
}

/// Build the default focused-pane screenshot path under the cache directory.
/// Uses `<cache>/kettle/shots/` (mirrors the `session_log_path` scheme below); falls
/// back to `./kettle-shots/` when no cache dir resolves.
///
/// File name shape: `kettle-<unix-secs>-<pid>-<sequence>.png`. `Action::TakeScreenshot`
/// and a ctl `screenshot` request without a `path` capture to it.
///
/// The process-wide sequence distinguishes captures sharing a timestamp.
pub(super) fn session_screenshot_path(
    unix_secs: u64,
    pid: u32,
    cache_dir: Option<&std::path::Path>,
) -> std::path::PathBuf {
    static NEXT_SHOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_SHOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = cache_dir
        .map(|p| p.to_path_buf().join("kettle").join("shots"))
        .unwrap_or_else(|| std::path::PathBuf::from("kettle-shots"));
    dir.join(format!("kettle-{unix_secs}-{pid}-{sequence}.png"))
}

pub(super) fn poll_screenshot_completion(
    ws: &mut WindowState,
) -> Option<Result<std::path::PathBuf, String>> {
    let result = match ws.screenshot_completion.as_ref()?.try_recv() {
        Ok(result) => result,
        Err(std::sync::mpsc::TryRecvError::Empty) => return None,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            Err("screenshot capture worker disconnected".into())
        }
    };
    ws.screenshot_completion = None;
    Some(result)
}

/// Validate a caller-supplied `screenshot` destination. `path` reaches here
/// verbatim from the ctl `screenshot` method and the `kettle_screenshot` MCP
/// tool built on it, both reachable by an agent whose instructions can be
/// steered by prompt-injected terminal content. Writing to an existing sensitive
/// file (`~/.bashrc`, `~/.ssh/authorized_keys`, a Windows Startup-folder item)
/// or through a symlink planted ahead of time would make this a silent
/// arbitrary-file-overwrite primitive.
///
/// `std::fs::symlink_metadata` (NOT `Path::exists`, which follows symlinks and
/// would miss a dangling one) detects ANY existing entry at the destination
/// (file, directory, or symlink) without following it, so a pre-planted symlink
/// is caught even when its target doesn't exist yet. Requiring a brand-new
/// destination is the same never-follow-an-existing-path discipline used
/// elsewhere in this codebase (`kettle_state::atomic_replace`, the recorder).
/// The cost is that a second capture to the same `path` fails instead of
/// clobbering the first. This probe is an up-front fast-fail; the renderer's
/// create-new publish is the security boundary.
///
/// Pure (no filesystem writes, only a metadata probe), so it is unit-testable
/// against a real temp directory.
pub(super) fn validate_screenshot_path(s: &str) -> Result<std::path::PathBuf, &'static str> {
    if s.trim().is_empty() {
        return Err("'path' is empty");
    }
    let path = std::path::PathBuf::from(s);
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(
            "'path' already exists — refusing to overwrite an existing file or follow an \
             existing symlink; choose a new destination",
        );
    }
    Ok(path)
}
