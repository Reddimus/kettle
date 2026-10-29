//! Serializable snapshot of the tab/split tree + per-pane working directory,
//! persisted to `$CONFIG/kettle/session.json` and restored on launch.

use std::io::Read as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub(crate) const MAX_RESTORE_WINDOWS: usize = 16;
pub(crate) const MAX_RESTORE_PANES: usize = 256;
// Two JSON objects per split, plus the window/tab envelope and leaf arrays,
// must fit serde_json's unchanged 128-container recursion limit.
pub(crate) const MAX_SPLIT_DEPTH: usize = 59;
pub(crate) const MAX_RESTORE_TOTAL_SURFACE_PIXELS: u64 = 64 * 1024 * 1024;
const MIN_RESTORE_WINDOW_WIDTH: u32 = 160;
const MIN_RESTORE_WINDOW_HEIGHT: u32 = 120;
const MAX_RESTORE_SURFACE_DIMENSION: u32 = 8192;
const MAX_RESTORE_SURFACE_PIXELS: u64 = 32 * 1024 * 1024;
const MAX_SESSION_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone)]
pub enum SNode {
    Leaf {
        cwd: Option<String>,
        /// argv the pane ran (empty = default shell); persists SSH panes.
        #[serde(default)]
        cmd: Vec<String>,
        /// Named broadcast-group membership (mirrors `Pane::group_name`);
        /// `None` = ungrouped, the Terminator default. `#[serde(default)]` lets
        /// an older `session.json` without this field load ungrouped. Populated
        /// by `Mux::snap`, consumed by `Mux::build_node`.
        #[serde(default)]
        group: Option<String>,
    },
    Split {
        /// `true` = stacked (horizontal divider); `false` = side-by-side.
        vertical: bool,
        ratio: f32,
        a: Box<SNode>,
        b: Box<SNode>,
    },
}

impl SNode {
    /// Number of leaf panes in this tree. Only tests call it; restore uses
    /// `bounded_leaf_count` to bound the PTY fan-out of a crafted-but-small
    /// `session.json`. serde_json's default 128-level recursion limit already
    /// bounds nesting depth.
    #[cfg(test)]
    pub fn leaf_count(&self) -> usize {
        self.bounded_leaf_count(usize::MAX).unwrap_or(usize::MAX)
    }

    /// Count leaves without walking or allocating beyond `limit`. This is the
    /// restore preflight primitive: a crafted serialized tree is rejected
    /// before rectangle allocation or PTY spawn.
    pub(crate) fn bounded_leaf_count(&self, limit: usize) -> Option<usize> {
        let mut leaves = 0usize;
        let mut pending = vec![self];
        while let Some(node) = pending.pop() {
            match node {
                SNode::Leaf { .. } => {
                    leaves = leaves.checked_add(1)?;
                    if leaves > limit {
                        return None;
                    }
                }
                SNode::Split { a, b, .. } => {
                    pending.push(b);
                    pending.push(a);
                }
            }
        }
        Some(leaves)
    }

    pub(crate) fn within_split_depth(&self) -> bool {
        let mut pending = vec![(self, 0)];
        while let Some((node, depth)) = pending.pop() {
            if depth > MAX_SPLIT_DEPTH {
                return false;
            }
            if let Self::Split { a, b, .. } = node {
                pending.push((a, depth + 1));
                pending.push((b, depth + 1));
            }
        }
        true
    }

    /// Saved-tree leaf rectangles in the same DFS order `Mux::restore` spawns
    /// them. This lets the UI compute each child PTY's exact initial geometry
    /// from the live surface before the process observes its first winsize.
    pub(crate) fn leaf_rects(&self, rect: (f32, f32, f32, f32)) -> Vec<(f32, f32, f32, f32)> {
        fn walk(node: &SNode, rect: (f32, f32, f32, f32), out: &mut Vec<(f32, f32, f32, f32)>) {
            match node {
                SNode::Leaf { .. } => out.push(rect),
                SNode::Split {
                    vertical,
                    ratio,
                    a,
                    b,
                } => {
                    let (x, y, width, height) = rect;
                    if *vertical {
                        let first_height = crate::mux::split_extent_px(height, *ratio);
                        walk(a, (x, y, width, first_height), out);
                        walk(b, (x, y + first_height, width, height - first_height), out);
                    } else {
                        let first_width = crate::mux::split_extent_px(width, *ratio);
                        walk(a, (x, y, first_width, height), out);
                        walk(b, (x + first_width, y, width - first_width, height), out);
                    }
                }
            }
        }

        let Some(leaves) = self.bounded_leaf_count(MAX_RESTORE_PANES) else {
            return Vec::new();
        };
        let mut rectangles = Vec::with_capacity(leaves);
        walk(self, rect, &mut rectangles);
        rectangles
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct STab {
    pub root: SNode,
    /// Focused pane's DFS-order index within `root`'s leaves (0 = first
    /// leaf). `#[serde(default)]` means old session files without this
    /// field still load — they restore to the first leaf, the original
    /// default. Saved by `Mux::snapshot`, consumed by `Mux::restore`.
    #[serde(default)]
    pub focus: usize,
    /// User-set tab-title override (mirrors `Tab::title_override`). `Some(s)`
    /// = the tab bar shows `s` instead of the focused pane's auto-title;
    /// `None` = auto-title. `#[serde(default)]` lets an older `session.json`
    /// without it load with no override. Saved by `Mux::snapshot`, consumed
    /// by `Mux::restore`.
    #[serde(default)]
    pub title_override: Option<String>,
    /// Whether this tab was zoomed (focused pane maximized; mirrors
    /// `Tab::zoomed`). `#[serde(default)]` lets an older `session.json`
    /// without it load unzoomed. Saved by `Mux::snapshot`, consumed by
    /// `Mux::restore`.
    #[serde(default)]
    pub zoomed: bool,
}

/// A window's saved outer position + inner size, in physical pixels. Restore
/// clamps it to the visible monitors (the saved monitor may be unplugged)
/// before applying.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SGeometry {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// One window's tabs within a session. `geometry` is `None` when the platform
/// can't report an outer position (Wayland); the WM then places the window on
/// restore.
#[derive(Serialize, Deserialize, Clone)]
pub struct SWindow {
    pub tabs: Vec<STab>,
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub geometry: Option<SGeometry>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Session {
    /// LEGACY single-window fields. `save_session` mirrors window 1 here so an
    /// older kettle (or a hand-rolled tool) reading the file still restores
    /// something sensible; `windows` is the source of truth when present.
    pub tabs: Vec<STab>,
    pub active: usize,
    /// LEGACY back-compat field, kept only so older `session.json` files that
    /// stored a theme here still deserialize. The config governs the theme
    /// (`persist_pref` writes each runtime change to its `theme =` line), so
    /// `save_session` writes `None` here and restore ignores it.
    #[serde(default)]
    pub theme: Option<String>,
    /// Every window's tabs + geometry, ordered window 1 first.
    /// `#[serde(default)]` so pre-multi-window files still load;
    /// `validated_restore_windows` falls back to the legacy top-level fields.
    #[serde(default)]
    pub windows: Vec<SWindow>,
}

/// Borrowed, preflight-approved restore input. Keeping this borrowed avoids
/// cloning a potentially large serialized structure before its global window
/// and pane fan-out has been validated.
#[derive(Clone, Copy)]
pub(crate) struct RestoreWindowRef<'a> {
    pub tabs: &'a [STab],
    pub active: usize,
    pub geometry: Option<SGeometry>,
}

/// How a restored window opens: at `size` always, the size the aggregate
/// budget counted, and at `position` only when the session saved one, so the
/// OS places the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RestoreSurface {
    pub size: (u32, u32),
    pub position: Option<(i32, i32)>,
}

impl RestoreSurface {
    /// The saved geometry to restore at, clamped, if the session saved one.
    pub(crate) fn geometry(self) -> Option<SGeometry> {
        self.position.map(|(x, y)| SGeometry {
            x,
            y,
            w: self.size.0,
            h: self.size.1,
        })
    }
}

/// Each window's surface, clamped to the live monitors, refused as a whole
/// past the aggregate GPU budget. A window without saved geometry is counted
/// at `default_size` clamped the same way, and opens at that size.
pub(crate) fn validated_restore_surface_geometries(
    windows: &[RestoreWindowRef<'_>],
    monitors: &[(i32, i32, u32, u32)],
    default_size: (u32, u32),
) -> Result<Vec<RestoreSurface>, String> {
    let mut total_pixels = 0u64;
    let mut geometries = Vec::with_capacity(windows.len());
    for (index, window) in windows.iter().enumerate() {
        let raw = window.geometry.unwrap_or(SGeometry {
            x: 0,
            y: 0,
            w: default_size.0,
            h: default_size.1,
        });
        let clamped = clamp_geometry_to_monitors(raw, monitors);
        let pixels = u64::from(clamped.w)
            .checked_mul(u64::from(clamped.h))
            .ok_or_else(|| format!("restored window {index} surface area overflowed"))?;
        total_pixels = total_pixels
            .checked_add(pixels)
            .ok_or_else(|| "aggregate restored surface area overflowed".to_string())?;
        if total_pixels > MAX_RESTORE_TOTAL_SURFACE_PIXELS {
            return Err(format!(
                "restored windows require {total_pixels} surface pixels, exceeding the \
                 {MAX_RESTORE_TOTAL_SURFACE_PIXELS}-pixel aggregate cap"
            ));
        }
        geometries.push(RestoreSurface {
            size: (clamped.w, clamped.h),
            position: window.geometry.map(|_| (clamped.x, clamped.y)),
        });
    }
    Ok(geometries)
}

/// Sanitize a user-supplied layout name (from
/// `kettle --layout <NAME>`) to `[A-Za-z0-9._-]`, replacing every other byte —
/// crucially path separators `/` and `\`, plus the Windows drive `:` — with
/// `_`. This is the only guard stopping `--layout ../../etc/passwd` from
/// reading an arbitrary file: after sanitizing it becomes the in-`layouts/`
/// filename `.._.._etc_passwd.json`, with no separator to traverse on. Chars
/// are *replaced*, never dropped, so a non-empty name always yields a
/// non-empty, separator-free result; only an empty name returns `None`. The
/// `.json` suffix appended by the caller also defuses a bare `..` (→ `...json`,
/// a regular filename, not a parent-dir reference).
fn sanitize_layout_name(name: &str) -> Option<String> {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() { None } else { Some(safe) }
}

impl Session {
    pub fn path() -> Option<PathBuf> {
        kettle_config::Config::default_path()
            .and_then(|p| p.parent().map(|d| d.join("session.json")))
    }

    /// The named-layout path: returns
    /// `<config-dir>/layouts/<sanitized>.json`. The name is sanitized
    /// to `[A-Za-z0-9._-]` so a user can't traverse out of the
    /// layouts directory via `--layout ../../etc/passwd`. Returns
    /// `None` if the config dir isn't resolvable.
    pub fn path_for_layout(name: &str) -> Option<PathBuf> {
        let safe = sanitize_layout_name(name)?;
        kettle_config::Config::default_path().and_then(|p| {
            p.parent()
                .map(|d| d.join("layouts").join(format!("{safe}.json")))
        })
    }

    pub(crate) fn load_with_identity() -> std::io::Result<Option<(Session, SnapshotIdentity)>> {
        let Some(p) = Self::path() else {
            return Ok(None);
        };
        load_snapshot_result(&p)
    }

    /// Load from the named-layout path instead of the default
    /// `session.json`. Used when the user launched with `kettle --layout
    /// <NAME>`. Returns `None` if the layout file doesn't exist yet —
    /// kettle just starts with a default first tab, exactly the same
    /// shape as a fresh install would.
    pub(crate) fn load_layout_with_identity(
        name: &str,
    ) -> std::io::Result<Option<(Session, SnapshotIdentity)>> {
        let Some(p) = Self::path_for_layout(name) else {
            return Ok(None);
        };
        load_snapshot_result(&p)
    }

    /// Load a one-shot tab-handoff JSON file (`--tab-handoff`) written by an
    /// older kettle's `Action::MoveTabToNewWindow`, deleting it after the read.
    pub fn load_tab_handoff(path: &std::path::Path) -> Option<Session> {
        let session = load_from_path(path)?;
        // One-shot: delete after read so a subsequent kettle
        // launch with the same args doesn't accidentally
        // re-restore stale handoff state.
        let _ = std::fs::remove_file(path);
        Some(session)
    }

    pub fn save(&self) {
        let Some(p) = Self::path() else {
            return;
        };
        if let Err(e) = save_to_path(self, &p) {
            log::warn!("could not save session to {}: {e}", p.display());
        }
    }

    /// List saved layouts by name, alphabetically (Terminator parity,
    /// `terminatorlib/layoutlauncher.py`). Walks `<config-dir>/layouts/*.json`
    /// and strips the extension; `Action::OpenLayoutPicker` filters against
    /// these names. A missing layouts dir (a fresh install has none) returns an
    /// empty `Vec`, not an error.
    ///
    /// Layouts always live at `<default config dir>/layouts/`, the same place
    /// [`Session::path_for_layout`] loads and saves them. `--config FILE`
    /// does not relocate them, so listing must not pretend otherwise.
    pub fn list_layouts() -> Vec<String> {
        let Some(default) = kettle_config::Config::default_path() else {
            return Vec::new();
        };
        let Some(dir) = default.parent().map(|d| d.join("layouts")) else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) != Some("json") {
                    return None;
                }
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .collect();
        names.sort();
        names
    }

    /// Save to the named-layout path. Creates the parent
    /// directory if it doesn't exist (a first-time
    /// `kettle --layout dev` from a fresh install needs to create
    /// `<config-dir>/layouts/` itself).
    pub fn save_layout(&self, name: &str) {
        let Some(p) = Self::path_for_layout(name) else {
            return;
        };
        if let Some(parent) = p.parent() {
            let _ = kettle_state::create_private_dirs(parent);
        }
        if let Err(e) = save_to_path(self, &p) {
            log::warn!("could not save layout {name:?} to {}: {e}", p.display());
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty() && self.windows.iter().all(|w| w.tabs.is_empty())
    }

    /// Validate the entire restore fan-out before any window, renderer, split
    /// rectangle, or PTY is created. Limits are global across all serialized
    /// windows; rejecting the whole restore avoids attacker-controlled partial
    /// state and repeated spawn/reap work.
    pub(crate) fn validated_restore_windows(&self) -> Result<Vec<RestoreWindowRef<'_>>, String> {
        fn push<'a>(
            restore: &mut Vec<RestoreWindowRef<'a>>,
            total_panes: &mut usize,
            window: RestoreWindowRef<'a>,
        ) -> Result<(), String> {
            if window.tabs.is_empty() {
                return Ok(());
            }
            if restore.len() >= MAX_RESTORE_WINDOWS {
                return Err(format!(
                    "session requests more than {MAX_RESTORE_WINDOWS} non-empty windows"
                ));
            }
            for tab in window.tabs {
                if !tab.root.within_split_depth() {
                    return Err(format!("session split depth exceeds {MAX_SPLIT_DEPTH}"));
                }
                let remaining = MAX_RESTORE_PANES.saturating_sub(*total_panes);
                let leaves = tab.root.bounded_leaf_count(remaining).ok_or_else(|| {
                    format!(
                        "session requests more than {MAX_RESTORE_PANES} panes across all windows"
                    )
                })?;
                *total_panes = total_panes
                    .checked_add(leaves)
                    .ok_or_else(|| "session pane count overflowed".to_string())?;
            }
            restore.push(window);
            Ok(())
        }

        let mut restore = Vec::new();
        let mut total_panes = 0usize;
        if self.windows.is_empty() {
            push(
                &mut restore,
                &mut total_panes,
                RestoreWindowRef {
                    tabs: &self.tabs,
                    active: self.active,
                    geometry: None,
                },
            )?;
        } else {
            for window in &self.windows {
                push(
                    &mut restore,
                    &mut total_panes,
                    RestoreWindowRef {
                        tabs: &window.tabs,
                        active: window.active,
                        geometry: window.geometry,
                    },
                )?;
            }
        }
        Ok(restore)
    }

    /// The session's windows in restore order, whatever vintage the file is.
    /// A v2 file returns its (non-empty) `windows` entries; a legacy
    /// single-window file becomes one geometry-less `SWindow` from the
    /// top-level fields. Empty-tab windows have nothing to restore and are
    /// dropped.
    #[cfg(test)]
    pub fn windows_normalized(&self) -> Vec<SWindow> {
        match self.validated_restore_windows() {
            Ok(windows) => windows
                .into_iter()
                .map(|window| SWindow {
                    tabs: window.tabs.to_vec(),
                    active: window.active,
                    geometry: window.geometry,
                })
                .collect(),
            Err(error) => {
                log::warn!("session restore rejected during preflight: {error}");
                Vec::new()
            }
        }
    }
}

/// Clamp a saved window geometry so the window is actually reachable on
/// the CURRENT monitor layout (the saved monitor may be unplugged or the
/// resolution changed). Width, height, and area are capped at the largest any
/// monitor offers and at the fixed surface caps, but never below the minimum
/// window size. If the window's top strip (the part you grab to move it)
/// intersects no monitor, snap the position into the first monitor. With no
/// usable monitor, the position passes through and only the fixed caps bound
/// the size. Pure for testability; monitors are `(x, y, w, h)` rects in
/// physical px.
pub(crate) fn clamp_geometry_to_monitors(
    g: SGeometry,
    monitors: &[(i32, i32, u32, u32)],
) -> SGeometry {
    let valid_monitors: Vec<_> = monitors
        .iter()
        .copied()
        .filter(|(_, _, width, height)| *width > 0 && *height > 0)
        .collect();
    let (max_width, max_height, max_area) = if valid_monitors.is_empty() {
        (
            MAX_RESTORE_SURFACE_DIMENSION,
            MAX_RESTORE_SURFACE_DIMENSION,
            MAX_RESTORE_SURFACE_PIXELS,
        )
    } else {
        let max_width = valid_monitors
            .iter()
            .map(|(_, _, width, _)| *width)
            .max()
            .unwrap_or(MIN_RESTORE_WINDOW_WIDTH)
            .clamp(MIN_RESTORE_WINDOW_WIDTH, MAX_RESTORE_SURFACE_DIMENSION);
        let max_height = valid_monitors
            .iter()
            .map(|(_, _, _, height)| *height)
            .max()
            .unwrap_or(MIN_RESTORE_WINDOW_HEIGHT)
            .clamp(MIN_RESTORE_WINDOW_HEIGHT, MAX_RESTORE_SURFACE_DIMENSION);
        let max_area = valid_monitors
            .iter()
            .map(|(_, _, width, height)| u64::from(*width) * u64::from(*height))
            .max()
            .unwrap_or(MAX_RESTORE_SURFACE_PIXELS)
            .min(MAX_RESTORE_SURFACE_PIXELS);
        (max_width, max_height, max_area)
    };
    let mut sanitized = SGeometry {
        w: g.w.clamp(MIN_RESTORE_WINDOW_WIDTH, max_width),
        h: g.h.clamp(MIN_RESTORE_WINDOW_HEIGHT, max_height),
        ..g
    };
    let area = u64::from(sanitized.w) * u64::from(sanitized.h);
    if area > max_area {
        let scale = (max_area as f64 / area as f64).sqrt();
        sanitized.w = ((f64::from(sanitized.w) * scale).floor() as u32)
            .clamp(MIN_RESTORE_WINDOW_WIDTH, max_width);
        sanitized.h = ((f64::from(sanitized.h) * scale).floor() as u32)
            .clamp(MIN_RESTORE_WINDOW_HEIGHT, max_height);
        while u64::from(sanitized.w) * u64::from(sanitized.h) > max_area {
            if sanitized.w >= sanitized.h && sanitized.w > MIN_RESTORE_WINDOW_WIDTH {
                sanitized.w -= 1;
            } else if sanitized.h > MIN_RESTORE_WINDOW_HEIGHT {
                sanitized.h -= 1;
            } else {
                break;
            }
        }
    }
    if valid_monitors.is_empty() {
        return sanitized;
    }

    // The grabbable strip: the window's top 30px, inset 50px from each side
    // (so "barely off-screen left" still counts as reachable if 50px of the
    // titlebar shows).
    let strip_l = i64::from(sanitized.x) + 50;
    let strip_r = i64::from(sanitized.x) + i64::from(sanitized.w) - 50;
    let strip_t = i64::from(sanitized.y);
    let strip_b = i64::from(sanitized.y) + 30;
    let visible = valid_monitors.iter().any(|&(mx, my, mw, mh)| {
        let (mx, my) = (i64::from(mx), i64::from(my));
        let (mr, mb) = (mx + i64::from(mw), my + i64::from(mh));
        strip_l < mr && strip_r > mx && strip_t < mb && strip_b > my
    });
    if visible {
        return sanitized;
    }
    let (mx, my, mw, mh) = valid_monitors[0];
    let (mx, my) = (i64::from(mx), i64::from(my));
    let max_x = (mx + i64::from(mw) - 100).max(mx);
    let max_y = (my + i64::from(mh) - 100).max(my);
    let clamp_i32 = |value: i64| value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    sanitized.x = clamp_i32(i64::from(sanitized.x).clamp(mx, max_x));
    sanitized.y = clamp_i32(i64::from(sanitized.y).clamp(my, max_y));
    sanitized
}

#[derive(Default)]
struct PendingSave {
    snapshot: Option<(Session, Option<String>)>,
    closing: bool,
}

pub(crate) struct SessionWriter {
    pending: std::sync::Arc<(std::sync::Mutex<PendingSave>, std::sync::Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl SessionWriter {
    pub(crate) fn new() -> std::io::Result<Self> {
        Self::with_sink(|session, layout| match layout {
            Some(name) => session.save_layout(&name),
            None => session.save(),
        })
    }

    fn with_sink(
        mut write: impl FnMut(Session, Option<String>) + Send + 'static,
    ) -> std::io::Result<Self> {
        let pending = std::sync::Arc::new((
            std::sync::Mutex::new(PendingSave::default()),
            std::sync::Condvar::new(),
        ));
        let shared = pending.clone();
        let worker = std::thread::Builder::new()
            .name("kettle-session-save".into())
            .spawn(move || {
                let (lock, ready) = &*shared;
                loop {
                    let next = {
                        let mut state = lock.lock().expect("session save lock");
                        while state.snapshot.is_none() && !state.closing {
                            state = ready.wait(state).expect("session save lock");
                        }
                        match state.snapshot.take() {
                            Some(snapshot) => snapshot,
                            None => break,
                        }
                    };
                    write(next.0, next.1);
                }
            })?;
        Ok(Self {
            pending,
            worker: Some(worker),
        })
    }

    pub(crate) fn submit(&self, session: Session, layout: Option<String>) {
        let (lock, ready) = &*self.pending;
        lock.lock().expect("session save lock").snapshot = Some((session, layout));
        ready.notify_one();
    }
}

impl Drop for SessionWriter {
    fn drop(&mut self) {
        let (lock, ready) = &*self.pending;
        lock.lock().expect("session save lock").closing = true;
        ready.notify_one();
        // The last pending snapshot follows any write already in progress.
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::warn!("session save worker panicked");
        }
    }
}

#[derive(Clone)]
pub(crate) struct SnapshotIdentity {
    file: std::sync::Arc<std::fs::File>,
    size: u64,
    modified: std::time::SystemTime,
}

impl SnapshotIdentity {
    fn of(file: &std::fs::File) -> std::io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            file: std::sync::Arc::new(file.try_clone()?),
            size: metadata.len(),
            modified: metadata.modified()?,
        })
    }

    fn matches(&self, path: &std::path::Path) -> std::io::Result<bool> {
        if !kettle_state::path_matches_file(&self.file, path)? {
            return Ok(false);
        }
        let (file, size) = open_session_file(path)?;
        Ok(size == self.size
            && file.metadata()?.modified()? == self.modified
            && kettle_state::path_matches_file(&self.file, path)?)
    }
}

fn session_lock(path: &std::path::Path) -> std::io::Result<kettle_state::ExclusiveFileLock> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    kettle_state::ExclusiveFileLock::acquire_timeout(
        &path.with_file_name(name),
        std::time::Duration::from_millis(500),
    )
}

fn changed_session() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "session changed since it was read",
    )
}

/// Save a private, durable session snapshot. Exact on-disk bytes are compared
/// before `atomic_replace` because this path follows many actions that do not
/// change session state. Reading the file, rather than trusting cached bytes or
/// length, also detects same-size edits made by another window.
pub(crate) fn save_to_path(s: &Session, p: &std::path::Path) -> std::io::Result<()> {
    if let Some(dir) = p.parent() {
        kettle_state::create_private_dirs(dir)?;
    }
    let _lock = session_lock(p)?;
    struct SessionBytes(Vec<u8>);
    impl std::io::Write for SessionBytes {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) as u64 > MAX_SESSION_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "outgoing session exceeds the size limit",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut text = SessionBytes(Vec::new());
    serde_json::to_writer_pretty(&mut text, s).map_err(|error| {
        std::io::Error::new(
            error.io_error_kind().unwrap_or(std::io::ErrorKind::Other),
            format!("serialize session: {error}"),
        )
    })?;
    let text = text.0;
    // A missing or unreadable file is simply "not what we are about to write".
    match open_session_file(p) {
        Ok((file, size)) => {
            if size > MAX_SESSION_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "existing session exceeds the size limit",
                ));
            }
            // Unchanged bytes skip the write, but not the permissions the
            // write would set: a file another tool loosened is tightened in
            // place, or rewritten when that fails.
            if size == text.len() as u64 {
                let mut on_disk = Vec::with_capacity(text.len());
                if (&file)
                    .take(text.len() as u64 + 1)
                    .read_to_end(&mut on_disk)
                    .is_ok()
                    && on_disk == text.as_slice()
                    && kettle_state::restrict_private_file(&file).is_ok()
                {
                    return Ok(());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => return Err(error),
        Err(_) => {}
    }
    kettle_state::atomic_replace(
        p,
        text.as_slice(),
        kettle_state::AtomicWriteOptions::PRIVATE,
    )?;
    Ok(())
}

/// Read and parse a one-shot handoff or test snapshot at `path`. Read errors
/// return `None`; startup uses `load_snapshot_result` to preserve those errors.
/// A JSON parse error is a real signal — kettle was killed mid-write, the
/// disk filled up, the file got hand-edited badly — so we log a warning
/// AND rename the broken file to `<path>.broken.<unix-seconds>` so the
/// next launch starts fresh while the user keeps a forensic artifact.
///
/// Pure (well, takes a path; no other state) so the rename-on-corruption
/// contract is testable without standing up the full app.
///
/// Bound the read at 16 MiB. A realistic kettle session is
/// at most a few KB (a handful of panes × short cwd / title / cmd
/// strings). The session file is auto-generated by kettle, but a
/// swap-attack with filesystem access could replace it with a multi-GB
/// payload that `std::fs::read_to_string` would happily load into RAM.
/// 16 MiB is a 1000× margin over real sessions. The limit and file type
/// are checked on the opened handle, and the read itself stops at limit + 1,
/// so a path replacement or concurrent append cannot bypass the cap.
pub(crate) fn load_from_path(p: &std::path::Path) -> Option<Session> {
    load_snapshot(p).map(|(session, _)| session)
}

pub(crate) fn load_snapshot(p: &std::path::Path) -> Option<(Session, SnapshotIdentity)> {
    load_snapshot_result(p).ok().flatten()
}

/// Missing snapshots allow a fresh start; unread snapshots must block autosaving.
pub(crate) fn load_snapshot_result(
    p: &std::path::Path,
) -> std::io::Result<Option<(Session, SnapshotIdentity)>> {
    match std::fs::symlink_metadata(p) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session is not a regular file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let _lock = session_lock(p)?;
    let (file, size) = open_session_file(p)?;
    let identity = SnapshotIdentity::of(&file)?;
    if size > MAX_SESSION_BYTES {
        stash_oversize_session(p, &identity, size, MAX_SESSION_BYTES)?;
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(MAX_SESSION_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SESSION_BYTES {
        stash_oversize_session(p, &identity, bytes.len() as u64, MAX_SESSION_BYTES)?;
        return Ok(None);
    }
    match serde_json::from_slice(&bytes) {
        Ok(s) => Ok(Some((s, identity))),
        Err(e) => {
            // Outside the log call: a disabled level skips its arguments.
            let backup = stash_session_file_locked(p, "broken", &identity);
            log::warn!(
                "session file {} is corrupted ({e}); {}",
                p.display(),
                match &backup {
                    Ok(dst) => format!("backed up to {}", dst.display()),
                    Err(_) => "could not rename; automatic saving is disabled".into(),
                }
            );
            backup?;
            Ok(None)
        }
    }
}

/// Move a session file aside, to `<path>.json.<kind>.<unix-seconds>`.
fn stash_session_file_locked(
    path: &std::path::Path,
    kind: &str,
    identity: &SnapshotIdentity,
) -> std::io::Result<std::path::PathBuf> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    stash_session_file_checked(path, kind, seconds, identity, || {})
}

/// How many names one second's stashes of one kind may take.
const MAX_STASHES_PER_SECOND: u32 = 64;

/// [`stash_session_file_locked`] at a given time. A second stash in the same second
/// gets `.1`, `.2`… rather than replacing the first: the name is taken with a
/// hard link, which never replaces, and only then is `path` removed. Where
/// hard links are unsupported, a rename to a name not yet taken.
#[cfg(test)]
fn stash_session_file_at(
    path: &std::path::Path,
    kind: &str,
    seconds: u64,
) -> std::io::Result<std::path::PathBuf> {
    stash_session_file_at_with(path, kind, seconds, || {})
}

#[cfg(test)]
fn stash_session_file_at_with(
    path: &std::path::Path,
    kind: &str,
    seconds: u64,
    before_remove: impl FnOnce(),
) -> std::io::Result<std::path::PathBuf> {
    let _lock = session_lock(path)?;
    let (file, _) = open_session_file(path)?;
    let identity = SnapshotIdentity::of(&file)?;
    stash_session_file_checked(path, kind, seconds, &identity, before_remove)
}

fn stash_session_file_checked(
    path: &std::path::Path,
    kind: &str,
    seconds: u64,
    identity: &SnapshotIdentity,
    before_remove: impl FnOnce(),
) -> std::io::Result<std::path::PathBuf> {
    if !identity.matches(path)? {
        return Err(changed_session());
    }
    let base = path.with_extension(format!("json.{kind}.{seconds}"));
    for attempt in 0..MAX_STASHES_PER_SECOND {
        let mut name = base.clone().into_os_string();
        if attempt > 0 {
            name.push(format!(".{attempt}"));
        }
        let candidate = std::path::PathBuf::from(name);
        match std::fs::hard_link(path, &candidate) {
            Ok(()) => {
                before_remove();
                if !identity.matches(path)? || !identity.matches(&candidate)? {
                    return Err(changed_session());
                }
                return std::fs::remove_file(path).map(|()| candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(error),
            Err(_) if std::fs::symlink_metadata(&candidate).is_err() => {
                before_remove();
                if !identity.matches(path)? {
                    return Err(changed_session());
                }
                return std::fs::rename(path, &candidate).map(|()| candidate);
            }
            Err(_) => {}
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "every session backup name for this second is taken",
    ))
}

fn invalid_session_file(path: &std::path::Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("session path is not a regular file: {}", path.display()),
    )
}

fn set_session_read_flags(options: &mut std::fs::OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    }

    #[cfg(not(any(unix, windows)))]
    let _ = options;
}

fn open_session_file(path: &std::path::Path) -> std::io::Result<(std::fs::File, u64)> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(invalid_session_file(path));
        }
        Ok(_) => {}
        Err(error) => return Err(error),
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    set_session_read_flags(&mut options);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) => {
            #[cfg(unix)]
            if error.raw_os_error() == Some(libc::ELOOP) {
                return Err(invalid_session_file(path));
            }
            return Err(error);
        }
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(invalid_session_file(path));
    }
    Ok((file, metadata.len()))
}

/// Move aside a session file kettle loaded but then refused to restore.
///
/// The parse-error and oversize branches do the same. A rejected restore still
/// opens one default tab, so without the stash the next save (the action tail
/// or the two-second sweep) rewrites the file from that single tab, losing
/// every window, split ratio and working directory within seconds of a launch
/// that looks ordinary.
///
/// The rejections are reachable without a hostile file, because nothing on the
/// save side enforces the limits the load side checks: seventeen open windows,
/// or more than 256 panes, or a large enough aggregate surface area, and kettle
/// writes a file it will refuse to read back.
#[cfg(test)]
fn stash_unrestorable_session(path: &std::path::Path, reason: &str) {
    let Ok((file, _)) = open_session_file(path) else {
        return;
    };
    let Ok(identity) = SnapshotIdentity::of(&file) else {
        return;
    };
    stash_unrestorable_snapshot(path, &identity, reason);
}

pub(crate) fn stash_unrestorable_snapshot(
    path: &std::path::Path,
    identity: &SnapshotIdentity,
    reason: &str,
) {
    let backup = session_lock(path)
        .and_then(|_lock| stash_session_file_locked(path, "unrestored", identity));
    match backup {
        Ok(destination) => log::warn!(
            "session at {} could not be restored ({reason}); kept at {} so the \
             next save cannot overwrite it",
            path.display(),
            destination.display()
        ),
        // Saying "kept at ..." when the rename failed would be worse than
        // saying nothing: the original is still in place and the next save
        // will overwrite it, which is exactly what the reader would believe
        // had been prevented.
        Err(error) => log::error!(
            "session at {} could not be restored ({reason}) and could not be moved \
             aside ({error}); the next save will overwrite it",
            path.display()
        ),
    }
}

fn stash_oversize_session(
    path: &std::path::Path,
    identity: &SnapshotIdentity,
    size: u64,
    limit: u64,
) -> std::io::Result<()> {
    log::warn!(
        "session file {} is {size} bytes (cap {limit}); \
         refusing to load and renaming to .toobig",
        path.display()
    );
    // Stash for forensics like the parse-error branch above; future launches
    // start fresh rather than repeatedly encountering the oversized file.
    stash_session_file_locked(path, "toobig", identity).map(|_| ())
}

#[cfg(test)]
mod tests {

    /// A session kettle loads but refuses to restore must survive the launch.
    ///
    /// A rejection that only logs leaves everything downstream behaving as if
    /// there were no session, including the next save, which replaces the file
    /// with a single fresh tab within seconds.
    #[test]
    fn a_session_that_cannot_be_restored_is_kept_rather_than_overwritten() {
        let temp = kettle_test_support::private_tempdir("kettle-session-stash-");
        let path = temp.path().join("session.json");
        std::fs::write(&path, b"{\"windows\":[]}").unwrap();

        super::stash_unrestorable_session(&path, "too many windows");

        assert!(
            !path.exists(),
            "the original path must be free, or the next save writes over it"
        );
        let kept: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("json.unrestored."))
            .collect();
        assert_eq!(kept.len(), 1, "exactly one stashed copy");
        assert_eq!(
            std::fs::read(kept[0].path()).unwrap(),
            b"{\"windows\":[]}",
            "the stash keeps the bytes verbatim"
        );
    }

    /// A rename that fails must leave the original alone.
    ///
    /// The log line for this case, which must not claim the file was kept, is
    /// not tested. Asserting on log output would need a capture layer this crate
    /// does not have, and a test that cannot fail is worse than none. The claim
    /// matters because it tells the reader their session was preserved while
    /// the next save is about to overwrite it.
    #[cfg(unix)]
    #[test]
    fn a_stash_that_cannot_move_the_file_leaves_it_alone() {
        use std::os::unix::fs::PermissionsExt as _;

        // Root ignores the mode bits, so the setup cannot be arranged there.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped: as root a read-only directory still permits rename");
            return;
        }
        let temp = kettle_test_support::private_tempdir("kettle-session-stashfail-");
        let holder = temp.path().join("holder");
        std::fs::create_dir(&holder).unwrap();
        let path = holder.join("session.json");
        std::fs::write(&path, b"keep me").unwrap();
        // Renaming an entry needs write permission on its directory.
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o500)).unwrap();

        super::stash_unrestorable_session(&path, "some reason");
        let still_there = path.exists();
        let _ = std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o700));

        assert!(
            still_there,
            "a failed move must leave the original in place"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
    }

    /// Stashing something that is not there must not invent a file.
    #[test]
    fn stashing_a_missing_session_does_nothing() {
        let temp = kettle_test_support::private_tempdir("kettle-session-nostash-");
        let path = temp.path().join("session.json");
        super::stash_unrestorable_session(&path, "no reason");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
    use super::*;

    /// A directory the PRIVATE atomic write will accept.
    ///
    /// `%TEMP%` is not one on Windows: it grants deletion rights to an
    /// untrusted principal, and `atomic_replace` refuses to create a private
    /// file under a parent like that. `kettle-state`'s own tests solve this
    /// the same way, by staging under the user-private profile instead.
    fn private_tempdir() -> kettle_test_support::PrivateTempDir {
        kettle_test_support::private_tempdir("kettle-session-test-")
    }

    #[test]
    fn session_writer_coalesces_without_waiting_and_drains_the_final_save() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = private_tempdir();
        let path = dir.path().join("session.json");
        let destination = path.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (written_tx, written_rx) = mpsc::channel();
        let writer = SessionWriter::with_sink(move |session, _| {
            if session.active == 1 {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            save_to_path(&session, &destination).unwrap();
            written_tx.send(session.active).unwrap();
        })
        .unwrap();

        writer.submit(
            Session {
                active: 1,
                ..Session::default()
            },
            None,
        );
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        for active in 2..=1000 {
            writer.submit(
                Session {
                    active,
                    ..Session::default()
                },
                None,
            );
        }
        release_tx.send(()).unwrap();
        drop(writer);

        assert_eq!(written_rx.into_iter().collect::<Vec<_>>(), vec![1, 1000]);
        assert_eq!(load_from_path(&path).unwrap().active, 1000);
    }

    /// An unchanged session must not be written again, and a changed one must.
    ///
    /// `save_to_path` runs from `handle_action`'s unconditional tail, so it
    /// fires on every keybound action — including scrolling, which changes
    /// nothing in the session. Each write fsyncs the file and its parent
    /// directory on the event-loop thread, so a repeat is a measurable stall,
    /// not just wasted I/O.
    ///
    /// Detected by the file's modification time rather than its contents,
    /// because the bytes are identical either way.
    #[test]
    fn an_unchanged_session_skips_the_durable_write() {
        let dir = private_tempdir();
        let path = dir.path().join("session.json");
        let session = Session {
            tabs: Vec::new(),
            active: 0,
            theme: None,
            windows: Vec::new(),
        };

        save_to_path(&session, &path).expect("first write");
        let first = std::fs::metadata(&path).expect("written").modified().ok();
        assert!(path.exists(), "the first save must create the file");

        // Coarse filesystem timestamps would make an "unchanged mtime" pass
        // for the wrong reason, so move the recorded time far enough back
        // that any rewrite is unambiguous.
        let backdated = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("reopen");
        file.set_modified(backdated).expect("backdate");
        drop(file);
        let stamped = std::fs::metadata(&path).expect("stat").modified().ok();
        assert_ne!(
            stamped, first,
            "the fixture must actually move the timestamp, or the assertion \
             below cannot fail"
        );

        save_to_path(&session, &path).expect("second write");
        assert_eq!(
            std::fs::metadata(&path).expect("stat").modified().ok(),
            stamped,
            "an identical session must not be rewritten"
        );

        // A real change still writes, immediately.
        let changed = Session {
            active: 3,
            ..Session {
                tabs: Vec::new(),
                active: 0,
                theme: None,
                windows: Vec::new(),
            }
        };
        save_to_path(&changed, &path).expect("third write");
        assert_ne!(
            std::fs::metadata(&path).expect("stat").modified().ok(),
            stamped,
            "a changed session must be written"
        );
        let reloaded: Session =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("parse");
        assert_eq!(reloaded.active, 3);
    }

    /// The skip must check the file itself. A session file deleted out from
    /// under the process has to be recreated, not assumed present.
    #[test]
    fn a_session_file_changed_underneath_us_is_rewritten() {
        let dir = private_tempdir();
        let path = dir.path().join("session.json");
        let session = Session {
            tabs: Vec::new(),
            active: 0,
            theme: None,
            windows: Vec::new(),
        };
        save_to_path(&session, &path).expect("first write");
        let ours = std::fs::read_to_string(&path).expect("read back");

        // Gone entirely.
        std::fs::remove_file(&path).expect("remove");
        assert!(!path.exists(), "fixture must actually remove the file");
        save_to_path(&session, &path).expect("rewrite");
        assert!(
            path.exists(),
            "an identical session whose file is gone must still be written"
        );

        // Replaced with different content of exactly the SAME LENGTH, which a
        // size check alone would miss. Two kettle windows share session.json
        // and a hand-edited file is supported, so "different bytes, same count"
        // is a real state to be in.
        let mut tampered: Vec<u8> = ours.clone().into_bytes();
        let last = tampered.len() - 1;
        tampered[last] = b' ';
        assert_eq!(
            tampered.len(),
            ours.len(),
            "the fixture must keep the length identical, or it tests nothing"
        );
        assert_ne!(
            tampered.as_slice(),
            ours.as_bytes(),
            "and it must actually differ"
        );
        std::fs::write(&path, &tampered).expect("tamper");

        save_to_path(&session, &path).expect("rewrite after tampering");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            ours,
            "a same-length replacement must be corrected, not mistaken for our \
             own last write"
        );
    }

    #[test]
    fn a_dual_written_legacy_session_cannot_replace_the_file_with_oversized_output() {
        let dir = private_tempdir();
        let path = dir.path().join("session.json");
        let tab = STab {
            root: SNode::Leaf {
                cwd: None,
                cmd: vec![],
                group: None,
            },
            focus: 0,
            title_override: Some("x".repeat(9 * 1024 * 1024)),
            zoomed: false,
        };
        let legacy = Session {
            tabs: vec![tab],
            ..Session::default()
        };
        let original = serde_json::to_vec(&legacy).unwrap();
        assert!(original.len() as u64 <= MAX_SESSION_BYTES);
        std::fs::write(&path, &original).unwrap();
        let mut loaded = load_from_path(&path).expect("a valid compact legacy session");
        loaded.windows.push(SWindow {
            tabs: loaded.tabs.clone(),
            active: 0,
            geometry: None,
        });
        assert!(
            save_to_path(&loaded, &path).is_err(),
            "oversized outgoing sessions must be rejected"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "the previous snapshot must survive"
        );
    }

    #[test]
    fn save_to_path_rejects_an_oversize_destination() {
        let dir = private_tempdir();
        let path = dir.path().join("session.json");
        let size = 16 * 1024 * 1024 + 1;
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(size).unwrap();
        drop(file);

        let error = save_to_path(&Session::default(), &path).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_rejects_a_symlink_even_when_its_bytes_match() {
        let dir = private_tempdir();
        let target = dir.path().join("target.json");
        let path = dir.path().join("session.json");
        let session = Session::default();
        let bytes = serde_json::to_vec_pretty(&session).unwrap();
        std::fs::write(&target, &bytes).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        let error = save_to_path(&session, &path).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        assert!(std::fs::symlink_metadata(&path).unwrap().is_symlink());
    }

    fn leaf_node() -> SNode {
        SNode::Leaf {
            cwd: None,
            cmd: Vec::new(),
            group: None,
        }
    }

    fn tree_with_leaves(count: usize) -> SNode {
        assert!(count > 0);
        if count == 1 {
            return leaf_node();
        }
        let left = count / 2;
        SNode::Split {
            vertical: count.is_multiple_of(2),
            ratio: 0.5,
            a: Box::new(tree_with_leaves(left)),
            b: Box::new(tree_with_leaves(count - left)),
        }
    }

    fn tab_with_leaves(count: usize) -> STab {
        STab {
            root: tree_with_leaves(count),
            focus: 0,
            title_override: None,
            zoomed: false,
        }
    }

    /// `leaf_count` wraps `bounded_leaf_count`, which bounds the restore PTY
    /// fan-out, so it must count every leaf across an arbitrarily nested split
    /// tree.
    #[test]
    fn snode_leaf_count_walks_the_tree() {
        let leaf = || SNode::Leaf {
            cwd: None,
            cmd: vec![],
            group: None,
        };
        assert_eq!(leaf().leaf_count(), 1);
        let tree = SNode::Split {
            vertical: false,
            ratio: 0.5,
            a: Box::new(leaf()),
            b: Box::new(SNode::Split {
                vertical: true,
                ratio: 0.5,
                a: Box::new(leaf()),
                b: Box::new(leaf()),
            }),
        };
        assert_eq!(tree.leaf_count(), 3);
    }

    #[test]
    fn saved_leaf_rects_match_mux_rounding_and_dfs_spawn_order() {
        let leaf = || SNode::Leaf {
            cwd: None,
            cmd: Vec::new(),
            group: None,
        };
        let tree = SNode::Split {
            vertical: false,
            ratio: 0.5,
            a: Box::new(leaf()),
            b: Box::new(SNode::Split {
                vertical: true,
                ratio: 0.4,
                a: Box::new(leaf()),
                b: Box::new(leaf()),
            }),
        };

        assert_eq!(
            tree.leaf_rects((0.0, 0.0, 101.0, 51.0)),
            vec![
                (0.0, 0.0, 51.0, 51.0),
                (51.0, 0.0, 50.0, 20.0),
                (51.0, 20.0, 50.0, 31.0),
            ]
        );
    }

    /// Drift guard (security). `sanitize_layout_name` is
    /// the sole barrier between an untrusted `--layout <NAME>` CLI argument and
    /// the filesystem; a regression that stopped replacing path separators
    /// would reopen `--layout ../../etc/passwd` as an arbitrary-file read.
    #[test]
    fn sanitize_layout_name_blocks_path_traversal() {
        // Benign names round-trip byte-for-byte.
        assert_eq!(
            sanitize_layout_name("my-layout").as_deref(),
            Some("my-layout")
        );
        assert_eq!(
            sanitize_layout_name("test.json").as_deref(),
            Some("test.json")
        );
        assert_eq!(sanitize_layout_name("a_b.1-2").as_deref(), Some("a_b.1-2"));
        // No separator (forward, back, or Windows drive colon) and no NUL /
        // control byte survives — the whole point of the guard.
        for hostile in [
            "../../etc/passwd",
            "..\\..\\windows\\system32",
            "a/../b",
            "/abs/path",
            "C:\\evil",
            "a\0b\nc\td",
        ] {
            let s = sanitize_layout_name(hostile).expect("non-empty input stays Some");
            assert!(!s.contains('/'), "`{hostile}` left a forward slash: {s}");
            assert!(!s.contains('\\'), "`{hostile}` left a backslash: {s}");
            assert!(!s.contains(':'), "`{hostile}` left a drive colon: {s}");
            assert!(!s.contains('\0'), "`{hostile}` left a NUL: {s}");
        }
        // Exact shape of the canonical traversal attempt: dots survive,
        // separators collapse to `_`, so it lands inside `layouts/`.
        assert_eq!(
            sanitize_layout_name("../../etc/passwd").as_deref(),
            Some(".._.._etc_passwd")
        );
        // Chars are *replaced*, never dropped: only an empty name → None
        // (so the "all-special → None" intuition is wrong, and that matters —
        // a separator-only name must still produce a safe in-dir filename).
        assert_eq!(sanitize_layout_name(""), None);
        assert_eq!(sanitize_layout_name("/\\:").as_deref(), Some("___"));
    }

    fn tmp_dir(label: &str) -> kettle_test_support::PrivateTempDir {
        kettle_test_support::private_tempdir(&format!("kettle-session-{label}-"))
    }

    #[test]
    fn load_from_path_returns_none_silently_when_file_missing() {
        // First-launch case: no file at the path. Must NOT panic, must NOT log
        // (would be noisy on every fresh install). A failed open returns None
        // before any warning; only the parse-error and oversize branches warn.
        let dir = tmp_dir("missing");
        let path = dir.join("session.json");
        assert!(load_from_path(&path).is_none());
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn load_from_path_rejects_a_non_regular_file() {
        let dir = tmp_dir("non-regular");
        let path = dir.join("session.json");
        std::fs::create_dir(&path).unwrap();
        assert!(load_from_path(&path).is_none());
        assert!(path.is_dir(), "the rejected path must not be renamed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn load_from_path_rejects_a_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let dir = tmp_dir("symlink");
        let target = dir.join("real-session.json");
        let path = dir.join("session.json");
        let serialized = serde_json::to_string_pretty(&Session::default()).unwrap();
        std::fs::write(&target, &serialized).unwrap();
        symlink(&target, &path).unwrap();

        assert!(load_from_path(&path).is_none());
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), serialized);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A session.json swapped out for a multi-MB blob
    /// (filesystem-tampering scenario; out of strict scope but
    /// defense-in-depth) must not be read into RAM. The 16 MiB
    /// pre-read size cap returns None and renames the bomb to
    /// `.json.toobig.<unix-seconds>` for forensics, same pattern
    /// as the corrupted-file recovery branch of `load_from_path` above.
    #[test]
    fn load_from_path_rejects_oversize_file_without_reading_into_memory() {
        let dir = tmp_dir("oversize");
        let path = dir.join("session.json");
        // Write a 17 MiB file (1 MiB over the 16 MiB cap). The content does not
        // matter, because the size check rejects the file before reading it;
        // even one byte past the cap would exercise that branch.
        let oversize = vec![b'A'; 17 * 1024 * 1024];
        std::fs::write(&path, &oversize).unwrap();
        assert!(
            load_from_path(&path).is_none(),
            "oversize session must not deserialize"
        );
        assert!(!path.exists(), "original should be renamed to .toobig");
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".toobig."))
            .collect();
        assert_eq!(
            backups.len(),
            1,
            "expected one .toobig stash, got {backups:?}"
        );
        for n in &backups {
            let _ = std::fs::remove_file(dir.join(n));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn load_from_path_preserves_invalid_utf8_before_the_next_save() {
        let dir = private_tempdir();
        let path = dir.path().join("session.json");
        let bytes = b"{\"tabs\":[],\"active\":0,\"theme\":\"caf\xe9\",\"windows\":[]}";
        std::fs::write(&path, bytes).unwrap();

        assert!(load_from_path(&path).is_none());
        assert!(!path.exists());
        save_to_path(&Session::default(), &path).unwrap();

        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_string_lossy().contains(".broken."))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(&backups[0]).unwrap(), bytes);
        assert!(load_from_path(&path).is_some());
    }

    #[test]
    fn load_from_path_backs_up_corrupted_file_and_returns_none() {
        // A corrupted file (kettle killed mid-write, disk full, hand-edit) must
        // return None *and* be renamed out of the way so the user can inspect
        // or restore it, rather than silently losing their tabs and splits.
        let dir = tmp_dir("corrupted");
        let path = dir.join("session.json");
        std::fs::write(&path, "{ this is not valid json at all").unwrap();
        assert!(
            load_from_path(&path).is_none(),
            "corrupted file should not deserialize"
        );
        assert!(!path.exists(), "original should be renamed out of the way");
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".broken."))
            .collect();
        assert_eq!(backups.len(), 1, "expected one backup, got {backups:?}");
        for n in &backups {
            let _ = std::fs::remove_file(dir.join(n));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn save_to_path_is_atomic_and_round_trips() {
        // Save writes through a `.tmp` sibling then
        // renames into place. Asserts the rename happened (dest exists,
        // tmp doesn't), and that the saved content round-trips through
        // load_from_path back to an equivalent Session.
        let dir = tmp_dir("save");
        let path = dir.join("session.json");
        let s = Session {
            tabs: vec![STab {
                root: SNode::Leaf {
                    cwd: Some("/tmp".into()),
                    cmd: vec!["bash".into()],
                    group: None,
                },
                focus: 0,
                title_override: None,
                zoomed: false,
            }],
            active: 0,
            theme: Some("Dracula".into()),
            windows: vec![],
        };
        save_to_path(&s, &path).expect("save");
        assert!(path.exists(), "destination written");
        // No leftover .tmp.* sibling (proves the rename succeeded, not
        // a stray crash that left the tmp file behind).
        let tmps: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(
            tmps.is_empty(),
            "tmp file should have been renamed away: {tmps:?}"
        );
        // Round-trip back through the load path.
        let loaded = load_from_path(&path).expect("load");
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.theme.as_deref(), Some("Dracula"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn save_to_path_overwrites_atomically() {
        // The rename-into-place semantics must also replace an existing
        // file, not error or leave the old contents. Save twice with
        // different state; second one wins.
        let dir = tmp_dir("save-overwrite");
        let path = dir.join("session.json");
        let s1 = Session::default();
        save_to_path(&s1, &path).expect("first save");
        let s2 = Session {
            tabs: vec![STab {
                root: SNode::Leaf {
                    cwd: None,
                    cmd: vec![],
                    group: None,
                },
                focus: 0,
                title_override: None,
                zoomed: false,
            }],
            active: 0,
            theme: None,
            windows: vec![],
        };
        save_to_path(&s2, &path).expect("second save");
        let loaded = load_from_path(&path).expect("load");
        assert_eq!(loaded.tabs.len(), 1, "second save should win");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_creates_private_session_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = private_tempdir();
        let path = dir.path().join("session.json");
        let session = Session::default();
        save_to_path(&session, &path).unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_tightens_a_legacy_world_readable_session_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = private_tempdir();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"legacy").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        save_to_path(&Session::default(), &path).unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn recovery_refuses_a_snapshot_replaced_since_it_was_loaded() {
        let directory = private_tempdir();
        let path = directory.path().join("session.json");
        save_to_path(&Session::default(), &path).unwrap();
        let (_, identity) = load_snapshot(&path).unwrap();
        kettle_state::atomic_replace(
            &path,
            b"new snapshot",
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
        .unwrap();
        stash_unrestorable_snapshot(&path, &identity, "rejected loaded session");
        assert_eq!(std::fs::read(&path).unwrap(), b"new snapshot");
        assert!(
            !directory
                .path()
                .read_dir()
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().contains(".unrestored."))
        );
    }

    #[test]
    fn review_session_lock_timeout_is_a_load_error() {
        let directory = private_tempdir();
        let path = directory.path().join("layout.json");
        save_to_path(&Session::default(), &path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let lock = session_lock(&path).unwrap();
        let result = load_snapshot_result(&path);
        assert!(
            result.is_err(),
            "lock timeout must not look like a missing layout"
        );
        assert_eq!(result.err().unwrap().kind(), std::io::ErrorKind::TimedOut);
        drop(lock);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn session_saving_uses_the_recovery_lock() {
        let directory = private_tempdir();
        let path = directory.path().join("session.json");
        let lock = session_lock(&path).unwrap();
        let (done, finished) = std::sync::mpsc::channel();
        let worker_path = path.clone();
        let worker = std::thread::spawn(move || {
            let result = save_to_path(&Session::default(), &worker_path);
            done.send(result).unwrap();
        });
        assert!(
            finished
                .recv_timeout(std::time::Duration::from_millis(30))
                .is_err(),
            "saving must wait while recovery owns the path lock"
        );
        drop(lock);
        finished
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
        assert!(load_from_path(&path).is_some());
    }

    #[test]
    fn stashing_does_not_unlink_a_session_replaced_after_the_backup_link() {
        let directory = private_tempdir();
        let path = directory.path().join("session.json");
        std::fs::write(&path, b"rejected snapshot").unwrap();
        let _ = stash_session_file_at_with(&path, "broken", 17, || {
            kettle_state::atomic_replace(
                &path,
                b"new durable snapshot",
                kettle_state::AtomicWriteOptions::PRIVATE,
            )
            .unwrap();
        });
        assert_eq!(
            std::fs::read(&path).ok().as_deref(),
            Some(b"new durable snapshot".as_slice()),
            "recovery must not unlink a concurrently replaced session"
        );
        assert_eq!(
            std::fs::read(path.with_extension("json.broken.17")).unwrap(),
            b"rejected snapshot"
        );
    }

    /// Two stashes in the same second keep both files: the second takes the
    /// next free name instead of replacing the first.
    #[test]
    fn stashes_in_the_same_second_keep_every_file() {
        let dir = private_tempdir();
        let path = dir.path().join("session.json");
        let mut kept = Vec::new();
        for contents in ["first", "second", "third"] {
            std::fs::write(&path, contents).unwrap();
            let backup = stash_session_file_at(&path, "broken", 1_700_000_000).unwrap();
            assert!(!path.exists());
            kept.push((backup, contents));
        }
        for (backup, contents) in &kept {
            assert_eq!(std::fs::read_to_string(backup).unwrap(), *contents);
        }
        assert_eq!(
            kept[1].0.file_name().unwrap(),
            "session.json.broken.1700000000.1"
        );
    }

    /// Saving the bytes already on disk still leaves the file owner-only.
    #[cfg(unix)]
    #[test]
    fn save_to_path_tightens_an_unchanged_session_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = private_tempdir();
        let path = dir.path().join("session.json");
        let session = Session::default();
        let bytes = serde_json::to_string_pretty(&session).unwrap();
        for mode in [0o644, 0o666] {
            std::fs::write(&path, &bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            save_to_path(&session, &path).unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn load_from_path_round_trips_valid_session() {
        // Healthy session round-trips through serde_json. The empty
        // default is enough to confirm the rename-on-error logic
        // doesn't fire on the happy path.
        let dir = tmp_dir("ok");
        let path = dir.join("session.json");
        let s = Session::default();
        std::fs::write(&path, serde_json::to_string_pretty(&s).unwrap()).unwrap();
        let loaded = load_from_path(&path).expect("valid session loads");
        assert!(loaded.is_empty());
        assert!(path.exists());
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".broken."))
            .collect();
        assert!(
            backups.is_empty(),
            "no backup expected on happy path: {backups:?}"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn session_json_round_trips_ssh_panes() {
        let s = Session {
            tabs: vec![STab {
                root: SNode::Split {
                    vertical: false,
                    ratio: 0.5,
                    a: Box::new(SNode::Leaf {
                        cwd: Some("/tmp".into()),
                        cmd: vec![],
                        group: None,
                    }),
                    b: Box::new(SNode::Leaf {
                        cwd: None,
                        cmd: vec!["ssh".into(), "-t".into(), "me@host".into()],
                        group: None,
                    }),
                },
                focus: 1, // second leaf focused — round-trip should keep it
                title_override: None,
                zoomed: false,
            }],
            active: 0,
            theme: Some("Dracula".into()),
            windows: vec![],
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tabs.len(), 1);
        assert_eq!(back.theme.as_deref(), Some("Dracula"), "theme persists");
        match &back.tabs[0].root {
            SNode::Split { a, b, .. } => {
                assert!(matches!(**a, SNode::Leaf { ref cwd, .. } if cwd.as_deref()==Some("/tmp")));
                assert!(matches!(**b, SNode::Leaf { ref cmd, .. } if cmd.len()==3));
            }
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn old_session_without_cmd_field_still_loads() {
        // `cmd` has #[serde(default)] so pre-SSH sessions remain loadable.
        let json = r#"{"tabs":[{"root":{"Leaf":{"cwd":null}}}],"active":0}"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert_eq!(s.tabs.len(), 1);
        // `theme` is also #[serde(default)] → None on pre-theme files.
        assert_eq!(s.theme, None);
        // `focus` likewise — sessions predating it restore to first leaf.
        assert_eq!(s.tabs[0].focus, 0);
    }

    #[test]
    fn session_v2_windows_round_trip_with_geometry() {
        // Windows with geometry survive a JSON round-trip, and
        // windows_normalized returns them in order.
        let leaf = || STab {
            root: SNode::Leaf {
                cwd: None,
                cmd: vec![],
                group: None,
            },
            focus: 0,
            title_override: None,
            zoomed: false,
        };
        let s = Session {
            // Dual-write mirror of window 1 (what save_session produces).
            tabs: vec![leaf()],
            active: 0,
            theme: None,
            windows: vec![
                SWindow {
                    tabs: vec![leaf()],
                    active: 0,
                    geometry: Some(SGeometry {
                        x: 100,
                        y: 200,
                        w: 800,
                        h: 600,
                    }),
                },
                SWindow {
                    tabs: vec![leaf(), leaf()],
                    active: 1,
                    geometry: None,
                },
            ],
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        let wins = back.windows_normalized();
        assert_eq!(wins.len(), 2, "v2 windows are the source of truth");
        assert_eq!(
            wins[0].geometry,
            Some(SGeometry {
                x: 100,
                y: 200,
                w: 800,
                h: 600
            })
        );
        assert_eq!(wins[1].tabs.len(), 2);
        assert_eq!(wins[1].active, 1);
        assert!(!back.is_empty());
    }

    #[test]
    fn legacy_session_normalizes_to_one_window() {
        // A pre-multi-window file (no `windows` field) loads and normalizes to
        // a single geometry-less window from the top-level fields. An OLD
        // kettle reading a NEW dual-written file sees window 1 via those same
        // top-level fields.
        let legacy = r#"{"tabs":[{"root":{"Leaf":{"cwd":null}}},{"root":{"Leaf":{"cwd":null}}}],"active":1}"#;
        let s: Session = serde_json::from_str(legacy).unwrap();
        let wins = s.windows_normalized();
        assert_eq!(wins.len(), 1);
        assert_eq!(wins[0].tabs.len(), 2);
        assert_eq!(wins[0].active, 1);
        assert_eq!(wins[0].geometry, None);
        // Empty-tab windows are dropped; fully-empty sessions normalize to [].
        let empty = Session::default();
        assert!(empty.windows_normalized().is_empty());
        assert!(empty.is_empty());
    }

    #[test]
    fn restore_preflight_enforces_global_window_and_pane_caps() {
        let exact = Session {
            windows: (0..MAX_RESTORE_WINDOWS)
                .map(|_| SWindow {
                    tabs: vec![tab_with_leaves(MAX_RESTORE_PANES / MAX_RESTORE_WINDOWS)],
                    active: 0,
                    geometry: None,
                })
                .collect(),
            ..Session::default()
        };
        let validated = exact
            .validated_restore_windows()
            .expect("exact global limits must restore");
        assert_eq!(validated.len(), MAX_RESTORE_WINDOWS);

        let too_many_windows = Session {
            windows: (0..=MAX_RESTORE_WINDOWS)
                .map(|_| SWindow {
                    tabs: vec![tab_with_leaves(1)],
                    active: 0,
                    geometry: None,
                })
                .collect(),
            ..Session::default()
        };
        assert!(
            too_many_windows.validated_restore_windows().is_err(),
            "window fan-out must be rejected before any window is cloned or opened"
        );

        let too_many_panes = Session {
            windows: vec![SWindow {
                tabs: vec![tab_with_leaves(MAX_RESTORE_PANES + 1)],
                active: 0,
                geometry: None,
            }],
            ..Session::default()
        };
        assert!(
            too_many_panes.validated_restore_windows().is_err(),
            "pane fan-out must be global and checked before leaf_rects allocation"
        );
        assert!(
            too_many_panes.windows[0].tabs[0]
                .root
                .leaf_rects((0.0, 0.0, 800.0, 600.0))
                .is_empty(),
            "leaf_rects must independently refuse over-cap allocations"
        );
    }

    #[test]
    fn restore_surface_preflight_enforces_aggregate_gpu_budget() {
        let window = |geometry| SWindow {
            tabs: vec![tab_with_leaves(1)],
            active: 0,
            geometry: Some(geometry),
        };
        let four_k = SGeometry {
            x: 0,
            y: 0,
            w: 3840,
            h: 2160,
        };
        let oversized = Session {
            windows: (0..MAX_RESTORE_WINDOWS).map(|_| window(four_k)).collect(),
            ..Session::default()
        };
        let windows = oversized
            .validated_restore_windows()
            .expect("window and pane counts are independently valid");
        let error =
            validated_restore_surface_geometries(&windows, &[(0, 0, 3840, 2160)], (800, 600))
                .expect_err("sixteen 4K swapchains must exceed the aggregate budget");
        assert!(error.contains("aggregate cap"), "{error}");

        let full_hd = SGeometry {
            w: 1920,
            h: 1080,
            ..four_k
        };
        let valid = Session {
            windows: (0..MAX_RESTORE_WINDOWS).map(|_| window(full_hd)).collect(),
            ..Session::default()
        };
        let windows = valid.validated_restore_windows().expect("valid fan-out");
        let geometries =
            validated_restore_surface_geometries(&windows, &[(0, 0, 1920, 1080)], (800, 600))
                .expect("sixteen 1080p surfaces fit the aggregate budget");
        assert_eq!(
            geometries.iter().map(|s| s.geometry()).collect::<Vec<_>>(),
            vec![Some(full_hd); MAX_RESTORE_WINDOWS]
        );
    }

    /// A window without saved geometry is counted at the default size
    /// clamped to the monitors, and must open at that size: opening it at
    /// the configured size let sixteen of them past the aggregate budget.
    #[test]
    fn a_window_without_geometry_opens_at_the_size_its_budget_counted() {
        let windows = vec![
            RestoreWindowRef {
                tabs: &[],
                active: 0,
                geometry: None,
            };
            MAX_RESTORE_WINDOWS
        ];
        let surfaces =
            validated_restore_surface_geometries(&windows, &[(0, 0, 1920, 1080)], (3200, 3200))
                .unwrap();
        for surface in surfaces {
            assert_eq!(surface.position, None);
            assert!(
                surface.size.0 <= 1920 && surface.size.1 <= 1080,
                "{surface:?}"
            );
        }
    }

    #[test]
    fn clamp_geometry_snaps_offscreen_windows_into_a_monitor() {
        // A window saved on a now-unplugged monitor must come back
        // reachable. One 1920x1080 monitor at the origin:
        let mons = [(0, 0, 1920u32, 1080u32)];
        // Fully on-screen geometry is untouched.
        let g = SGeometry {
            x: 100,
            y: 100,
            w: 800,
            h: 600,
        };
        assert_eq!(clamp_geometry_to_monitors(g, &mons), g);
        // A window on a (gone) second monitor to the right snaps back in.
        let off = SGeometry {
            x: 2500,
            y: 300,
            w: 800,
            h: 600,
        };
        let c = clamp_geometry_to_monitors(off, &mons);
        assert!(c.x <= 1920 - 100, "x clamped into the monitor: {c:?}");
        assert_eq!(c.y, 300, "y already visible-range");
        assert_eq!((c.w, c.h), (800, 600), "size untouched");
        // A window above the screen comes down.
        let above = SGeometry {
            x: 100,
            y: -900,
            w: 800,
            h: 600,
        };
        let c = clamp_geometry_to_monitors(above, &mons);
        assert!(c.y >= 0, "y clamped down: {c:?}");
        // Barely-overlapping titlebar (50px visible) is accepted as-is.
        let edge = SGeometry {
            x: -700,
            y: 10,
            w: 800,
            h: 600,
        };
        assert_eq!(clamp_geometry_to_monitors(edge, &mons), edge);
        // No monitors reported (headless oddity): pass through.
        assert_eq!(clamp_geometry_to_monitors(off, &[]), off);
    }

    #[test]
    fn clamp_geometry_handles_persisted_integer_extrema_and_zero_sizes() {
        let monitors = [(0, 0, 1920u32, 1080u32)];
        for geometry in [
            SGeometry {
                x: i32::MAX,
                y: i32::MAX,
                w: u32::MAX,
                h: u32::MAX,
            },
            SGeometry {
                x: i32::MIN,
                y: i32::MIN,
                w: 0,
                h: 0,
            },
        ] {
            let clamped = clamp_geometry_to_monitors(geometry, &monitors);
            assert!((MIN_RESTORE_WINDOW_WIDTH..=1920).contains(&clamped.w));
            assert!((MIN_RESTORE_WINDOW_HEIGHT..=1080).contains(&clamped.h));
            assert!((0..=1820).contains(&clamped.x));
            assert!((0..=980).contains(&clamped.y));
        }

        let headless = clamp_geometry_to_monitors(
            SGeometry {
                x: i32::MAX,
                y: i32::MIN,
                w: u32::MAX,
                h: 0,
            },
            &[],
        );
        assert_eq!(headless.x, i32::MAX);
        assert_eq!(headless.y, i32::MIN);
        assert_eq!(headless.w, MAX_RESTORE_SURFACE_DIMENSION);
        assert_eq!(headless.h, MIN_RESTORE_WINDOW_HEIGHT);

        let extreme_monitor = [(i32::MAX - 10, i32::MIN, u32::MAX, u32::MAX)];
        let clamped = clamp_geometry_to_monitors(
            SGeometry {
                x: 0,
                y: 0,
                w: u32::MAX,
                h: u32::MAX,
            },
            &extreme_monitor,
        );
        assert!(clamped.w <= MAX_RESTORE_SURFACE_DIMENSION);
        assert!(clamped.h <= MAX_RESTORE_SURFACE_DIMENSION);
    }

    #[test]
    fn session_round_trips_focused_pane_index() {
        // The session records which leaf was focused so
        // restore brings the user back to the same pane within each tab.
        // Confirm the round-trip and that the default lands on first leaf
        // (preserving the original default for older sessions).
        let s = Session {
            tabs: vec![STab {
                root: SNode::Split {
                    vertical: true,
                    ratio: 0.6,
                    a: Box::new(SNode::Leaf {
                        cwd: None,
                        cmd: vec![],
                        group: None,
                    }),
                    b: Box::new(SNode::Leaf {
                        cwd: None,
                        cmd: vec![],
                        group: None,
                    }),
                },
                focus: 1,
                title_override: None,
                zoomed: false,
            }],
            active: 0,
            theme: None,
            windows: vec![],
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tabs[0].focus, 1, "focus index round-trips");
        // Old session without focus field defaults to 0.
        let legacy = r#"{"tabs":[{"root":{"Leaf":{"cwd":null}}}],"active":0}"#;
        let l: Session = serde_json::from_str(legacy).unwrap();
        assert_eq!(l.tabs[0].focus, 0);
    }

    #[test]
    fn session_round_trips_group_title_override_and_zoom() {
        // A pane's named broadcast-group membership (`SNode::Leaf::group`), a
        // tab's user-set title override (`STab::title_override`), and a tab's
        // zoom state (`STab::zoomed`) must all survive a JSON round-trip with
        // their exact values.
        let s = Session {
            tabs: vec![STab {
                root: SNode::Split {
                    vertical: false,
                    ratio: 0.5,
                    // First leaf is a member of the "fleet" broadcast group.
                    a: Box::new(SNode::Leaf {
                        cwd: Some("/home/me".into()),
                        cmd: vec![],
                        group: Some("fleet".into()),
                    }),
                    // Second leaf is ungrouped (None must round-trip too).
                    b: Box::new(SNode::Leaf {
                        cwd: None,
                        cmd: vec![],
                        group: None,
                    }),
                },
                focus: 0,
                title_override: Some("deploys".into()),
                zoomed: true,
            }],
            active: 0,
            theme: None,
            windows: vec![],
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        let tab = &back.tabs[0];
        assert_eq!(
            tab.title_override.as_deref(),
            Some("deploys"),
            "tab title override round-trips"
        );
        assert!(tab.zoomed, "zoom state round-trips");
        match &tab.root {
            SNode::Split { a, b, .. } => {
                assert!(
                    matches!(**a, SNode::Leaf { ref group, .. }
                        if group.as_deref() == Some("fleet")),
                    "grouped pane's group name round-trips"
                );
                assert!(
                    matches!(**b, SNode::Leaf { ref group, .. } if group.is_none()),
                    "ungrouped pane stays ungrouped"
                );
            }
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn legacy_session_without_group_title_zoom_fields_loads_to_defaults() {
        // `group`, `title_override`, and `zoomed` are all `#[serde(default)]`,
        // so a session.json written by an OLDER kettle without those keys must
        // still load, as an ungrouped pane with no title override, not zoomed.
        let legacy = r#"{"tabs":[{"root":{"Leaf":{"cwd":"/tmp"}},"focus":0}],"active":0}"#;
        let s: Session = serde_json::from_str(legacy).expect("legacy json must still load");
        let tab = &s.tabs[0];
        assert_eq!(tab.title_override, None, "missing title_override → None");
        assert!(!tab.zoomed, "missing zoomed → false");
        match &tab.root {
            SNode::Leaf { group, .. } => {
                assert_eq!(*group, None, "missing group → None (ungrouped)");
            }
            _ => panic!("expected leaf"),
        }
    }
}
