//! Serializable snapshot of the tab/split tree + per-pane working directory,
//! persisted to `$CONFIG/kettle/session.json` and restored on launch.

use std::io::Read as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub(crate) const MAX_RESTORE_WINDOWS: usize = 16;
pub(crate) const MAX_RESTORE_PANES: usize = 256;
pub(crate) const MAX_RESTORE_TOTAL_SURFACE_PIXELS: u64 = 64 * 1024 * 1024;
const MIN_RESTORE_WINDOW_WIDTH: u32 = 160;
const MIN_RESTORE_WINDOW_HEIGHT: u32 = 120;
const MAX_RESTORE_SURFACE_DIMENSION: u32 = 8192;
const MAX_RESTORE_SURFACE_PIXELS: u64 = 32 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone)]
pub enum SNode {
    Leaf {
        cwd: Option<String>,
        /// argv the pane ran (empty = default shell); persists SSH panes.
        #[serde(default)]
        cmd: Vec<String>,
        /// Named broadcast-group membership (mirrors `Pane::group_name`).
        /// `Some(name)` = the pane belongs to that named broadcast group;
        /// `None` = ungrouped (the Terminator default). `#[serde(default)]`
        /// (additive wire field) so an OLD `session.json` written before this
        /// field existed still deserializes — it just restores ungrouped, the
        /// original default. Populated by `Mux::snap`, consumed by
        /// `Mux::build_node`.
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
    /// Number of leaf panes in this tree — each leaf becomes one real PTY on
    /// restore, so `Mux::restore` uses this to bound the spawn fan-out against a
    /// crafted-but-small `session.json`. serde_json's default
    /// 128-level recursion limit already bounds nesting depth, so this is a
    /// simple (non-recursive-overflow) count.
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
                    let ratio = ratio.clamp(0.05, 0.95);
                    if *vertical {
                        let first_height = (height * ratio).round();
                        walk(a, (x, y, width, first_height), out);
                        walk(b, (x, y + first_height, width, height - first_height), out);
                    } else {
                        let first_width = (width * ratio).round();
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
    /// `None` = auto-title. `#[serde(default)]` (additive wire field) so an
    /// OLD `session.json` lacking it still loads — it restores with no
    /// override, the original default. Saved by `Mux::snapshot`, consumed
    /// by `Mux::restore`.
    #[serde(default)]
    pub title_override: Option<String>,
    /// Whether this tab was zoomed (focused pane maximized; mirrors
    /// `Tab::zoomed`). `#[serde(default)]` (additive wire field, defaults to
    /// `false`) so an OLD `session.json` without it loads unzoomed, the
    /// original default. Saved by `Mux::snapshot`, consumed by
    /// `Mux::restore`.
    #[serde(default)]
    pub zoomed: bool,
}

/// C7 (multi-window): a window's saved outer position + inner size,
/// physical pixels. Restore clamps it to the visible monitors (the saved
/// monitor may be unplugged) before applying.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SGeometry {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// C7 (multi-window): one window's tabs within a session. `geometry` is
/// `None` when the platform can't report an outer position (Wayland) — the
/// WM places the window on restore.
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
    /// LEGACY single-window fields. C7 dual-writes window 1 here so an older
    /// kettle (or a hand-rolled tool) reading the file still restores
    /// something sensible; `windows` is the source of truth when present.
    pub tabs: Vec<STab>,
    pub active: usize,
    /// LEGACY back-compat field. The theme is now
    /// CONFIG-governed — every runtime theme change is persisted to the config
    /// `theme =` line via `persist_pref`, and `save_session` writes this as
    /// `None` while restore IGNORES it. Kept only so older `session.json` files
    /// (which stored a theme here) still deserialize. `default` so absent is OK.
    #[serde(default)]
    pub theme: Option<String>,
    /// C7 (multi-window): every window's tabs + geometry, ordered window 1
    /// first. `#[serde(default)]` so pre-multi-window files still load —
    /// `windows_normalized` falls back to the legacy top-level fields.
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

pub(crate) fn validated_restore_surface_geometries(
    windows: &[RestoreWindowRef<'_>],
    monitors: &[(i32, i32, u32, u32)],
    default_size: (u32, u32),
) -> Result<Vec<Option<SGeometry>>, String> {
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
        geometries.push(window.geometry.map(|_| clamped));
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

    pub fn load() -> Option<Session> {
        let p = Self::path()?;
        load_from_path(&p)
    }

    /// Load from the named-layout path instead of the default
    /// `session.json`. Used when the user launched with `kettle --layout
    /// <NAME>`. Returns `None` if the layout file doesn't exist yet —
    /// kettle just starts with a default first tab, exactly the same
    /// shape as a fresh install would.
    pub fn load_layout(name: &str) -> Option<Session> {
        let p = Self::path_for_layout(name)?;
        load_from_path(&p)
    }

    /// Terminator parity, detachable-tabs Bucket-D file-fallback: load a
    /// one-shot tab-handoff JSON file written by another kettle process
    /// (via `Action::MoveTabToNewWindow`). Reads the path + deletes it
    /// after read (one-shot handoff — avoids accidental re-use
    /// across launches).
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

    /// Terminator parity, `terminatorlib/layoutlauncher.py`:
    /// list saved layouts by name (alphabetical). Walks
    /// `<config-dir>/layouts/*.json`, strips the extension. Returns
    /// an empty `Vec` when the layouts dir doesn't exist (a fresh
    /// install has none) — that's not an error, just "nothing to
    /// pick from yet". Closes the layout-launcher Bucket-D gap by
    /// giving `Action::OpenLayoutPicker` a source of
    /// names to filter against.
    /// Layouts always live at `<default config dir>/layouts/`, the same place
    /// [`Session::path_for_layout`] loads and saves them — `--config FILE`
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
            let _ = std::fs::create_dir_all(parent);
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

    /// C7: the session's windows in restore order, whatever vintage the file
    /// is. A v2 file returns its (non-empty) `windows` entries; a legacy
    /// single-window file becomes one geometry-less `SWindow` from the
    /// top-level fields. Empty-tab windows are dropped — nothing to restore.
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

/// C7: clamp a saved window geometry so the window is actually reachable on
/// the CURRENT monitor layout (the saved monitor may be unplugged or the
/// resolution changed). If the window's top strip — the part you grab to
/// move it — intersects no monitor, snap the position into the first
/// monitor; the size is left alone (the WM clips oversize windows fine).
/// Pure for testability; monitors are `(x, y, w, h)` rects in physical px.
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

/// Durable private save. The shared state writer stages the complete JSON in
/// the destination directory, syncs it, atomically replaces the old snapshot,
/// and syncs the directory. The resulting file is private (`0600` on Unix).
pub(crate) fn save_to_path(s: &Session, p: &std::path::Path) -> std::io::Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(s)
        .map_err(|e| std::io::Error::other(format!("serialize session: {e}")))?;
    kettle_state::atomic_replace(
        p,
        text.as_bytes(),
        kettle_state::AtomicWriteOptions::PRIVATE,
    )
}

/// Read and parse a session file at `path`. A read error (no file, HOME
/// changed) is the expected first-launch case and returns `None` silently;
/// a JSON parse error is a real signal — kettle was killed mid-write, the
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
    const MAX_SESSION_BYTES: u64 = 16 * 1024 * 1024;
    let (file, size) = open_session_file(p).ok()?;
    if size > MAX_SESSION_BYTES {
        stash_oversize_session(p, size, MAX_SESSION_BYTES);
        return None;
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(MAX_SESSION_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_SESSION_BYTES {
        stash_oversize_session(p, bytes.len() as u64, MAX_SESSION_BYTES);
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    match serde_json::from_str(&text) {
        Ok(s) => Some(s),
        Err(e) => {
            let dst = p.with_extension(format!(
                "json.broken.{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            ));
            let renamed = std::fs::rename(p, &dst).is_ok();
            log::warn!(
                "session file {} is corrupted ({e}); {}",
                p.display(),
                if renamed {
                    format!("backed up to {}", dst.display())
                } else {
                    "could not rename — kettle will overwrite on the next save".into()
                }
            );
            None
        }
    }
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

fn stash_oversize_session(path: &std::path::Path, size: u64, limit: u64) {
    log::warn!(
        "session file {} is {size} bytes (cap {limit}); \
         refusing to load and renaming to .toobig",
        path.display()
    );
    // Stash for forensics like the parse-error branch above; future launches
    // start fresh rather than repeatedly encountering the oversized file.
    let destination = path.with_extension(format!(
        "json.toobig.{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
    ));
    let _ = std::fs::rename(path, destination);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// `leaf_count` bounds the restore PTY fan-out, so it
    /// must count every leaf across an arbitrarily nested split tree.
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

    fn tmp_dir(label: &str) -> std::path::PathBuf {
        let p = crate::test_scratch_root().join(format!(
            "kettle-session-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn load_from_path_returns_none_silently_when_file_missing() {
        // First-launch case — no file at the path. Must NOT panic, must
        // NOT log (would be noisy on every fresh install). load_from_path
        // returning None covers both — log::warn! is only called from the
        // parse-error branch.
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
        // Write a 17 MiB file (1 MiB over the 16 MiB cap). All
        // zero bytes — `read_to_string` would still allocate the
        // whole buffer if the cap weren't enforced, so just one
        // extra byte past the cap exercises the size branch.
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
    fn load_from_path_backs_up_corrupted_file_and_returns_none() {
        // A corrupted file (kettle killed mid-write,
        // disk full, hand-edit) used to silently drop the user's tabs/
        // splits state on the next launch. Now: return None *and* rename
        // the file out of the way so the user can inspect / restore.
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
        let dir = tempfile::tempdir().unwrap();
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

        let dir = tempfile::tempdir().unwrap();
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
        // C7: the multi-window session shape — windows with geometry survive
        // a JSON round-trip, and windows_normalized returns them in order.
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
        // C7: a pre-multi-window file (no `windows` field) loads and
        // normalizes to a single geometry-less window from the top-level
        // fields — and an OLD kettle reading a NEW dual-written file sees
        // window 1 via those same top-level fields.
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
        assert_eq!(geometries, vec![Some(full_hd); MAX_RESTORE_WINDOWS]);
    }

    #[test]
    fn clamp_geometry_snaps_offscreen_windows_into_a_monitor() {
        // C7: a window saved on a now-unplugged monitor must come back
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
        // Three pieces of per-pane / per-tab state were previously DROPPED on
        // save/restore (the wire format had no slot for them, so restore
        // hardcoded None/false): a pane's named broadcast-group membership
        // (`SNode::Leaf::group`), a tab's user-set title override
        // (`STab::title_override`), and a tab's zoom state (`STab::zoomed`).
        // They're now additive `#[serde(default)]` wire fields. Confirm all
        // three survive a JSON round-trip with their exact values.
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
        // The three new fields are all `#[serde(default)]`, so a session.json
        // written by an OLDER kettle (no `group`/`title_override`/`zoomed`
        // keys) must still deserialize cleanly — to the original defaults:
        // ungrouped pane, no title override, not zoomed. This is the
        // backward-compat guarantee that makes the wire-format extension safe.
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
