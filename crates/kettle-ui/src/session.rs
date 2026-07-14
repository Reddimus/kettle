//! Serializable snapshot of the tab/split tree + per-pane working directory,
//! persisted to `$CONFIG/kettle/session.json` and restored on launch.

use std::io::Read as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
        /// pre-cycle behavior. Populated by `Mux::snap`, consumed by
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
    /// crafted-but-small `session.json` (cycle 863, audit). serde_json's default
    /// 128-level recursion limit already bounds nesting depth, so this is a
    /// simple (non-recursive-overflow) count.
    pub fn leaf_count(&self) -> usize {
        match self {
            SNode::Leaf { .. } => 1,
            SNode::Split { a, b, .. } => a.leaf_count() + b.leaf_count(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct STab {
    pub root: SNode,
    /// Focused pane's DFS-order index within `root`'s leaves (0 = first
    /// leaf). `#[serde(default)]` means old session files without this
    /// field still load — they restore to the first leaf, the pre-cycle
    /// behavior. Saved by `Mux::snapshot`, consumed by `Mux::restore`.
    #[serde(default)]
    pub focus: usize,
    /// User-set tab-title override (mirrors `Tab::title_override`). `Some(s)`
    /// = the tab bar shows `s` instead of the focused pane's auto-title;
    /// `None` = auto-title. `#[serde(default)]` (additive wire field) so an
    /// OLD `session.json` lacking it still loads — it restores with no
    /// override, the pre-cycle behavior. Saved by `Mux::snapshot`, consumed
    /// by `Mux::restore`.
    #[serde(default)]
    pub title_override: Option<String>,
    /// Whether this tab was zoomed (focused pane maximized; mirrors
    /// `Tab::zoomed`). `#[serde(default)]` (additive wire field, defaults to
    /// `false`) so an OLD `session.json` without it loads unzoomed, the
    /// pre-cycle behavior. Saved by `Mux::snapshot`, consumed by
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
    /// LEGACY back-compat field. Cycle 919 (audit L7): the theme is now
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

/// Cycle 291 / 789 (audit D1): sanitize a user-supplied layout name (from
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

    /// Cycle 291 named-layout path: returns
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

    /// Cycle 291: load from the named-layout path instead of the default
    /// `session.json`. Used when the user launched with `kettle --layout
    /// <NAME>`. Returns `None` if the layout file doesn't exist yet —
    /// kettle just starts with a default first tab, exactly the same
    /// shape as a fresh install would.
    pub fn load_layout(name: &str) -> Option<Session> {
        let p = Self::path_for_layout(name)?;
        load_from_path(&p)
    }

    /// Cycle 404 (Terminator parity, detachable-tabs Bucket-D
    /// sub-cycle 8 file-fallback): load a one-shot tab-handoff
    /// JSON file written by another kettle process (cycle 384's
    /// Action::MoveTabToNewWindow). Reads the path + deletes it
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

    /// Cycle 708 (Terminator parity, `terminatorlib/layoutlauncher.py`):
    /// list saved layouts by name (alphabetical). Walks
    /// `<config-dir>/layouts/*.json`, strips the extension. Returns
    /// an empty `Vec` when the layouts dir doesn't exist (a fresh
    /// install has none) — that's not an error, just "nothing to
    /// pick from yet". Closes the layout-launcher Bucket-D gap by
    /// giving `Action::OpenLayoutPicker` (cycle 708) a source of
    /// names to filter against.
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

    /// Cycle 291: save to the named-layout path. Creates the parent
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

    /// C7: the session's windows in restore order, whatever vintage the file
    /// is. A v2 file returns its (non-empty) `windows` entries; a legacy
    /// single-window file becomes one geometry-less `SWindow` from the
    /// top-level fields. Empty-tab windows are dropped — nothing to restore.
    pub fn windows_normalized(&self) -> Vec<SWindow> {
        if !self.windows.is_empty() {
            return self
                .windows
                .iter()
                .filter(|w| !w.tabs.is_empty())
                .cloned()
                .collect();
        }
        if self.tabs.is_empty() {
            return Vec::new();
        }
        vec![SWindow {
            tabs: self.tabs.clone(),
            active: self.active,
            geometry: None,
        }]
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
    if monitors.is_empty() {
        return g;
    }
    // The grabbable strip: the window's top 30px, inset 50px from each side
    // (so "barely off-screen left" still counts as reachable if 50px of the
    // titlebar shows).
    let strip_l = g.x + 50;
    let strip_r = g.x + g.w as i32 - 50;
    let strip_t = g.y;
    let strip_b = g.y + 30;
    let visible = monitors.iter().any(|&(mx, my, mw, mh)| {
        let (mr, mb) = (mx + mw as i32, my + mh as i32);
        strip_l < mr && strip_r > mx && strip_t < mb && strip_b > my
    });
    if visible {
        return g;
    }
    let (mx, my, mw, mh) = monitors[0];
    SGeometry {
        x: g.x.clamp(mx, (mx + mw as i32 - 100).max(mx)),
        y: g.y.clamp(my, (my + mh as i32 - 100).max(my)),
        ..g
    }
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
/// Cycle 585: bound the read at 16 MiB. A realistic kettle session is
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

    /// Cycle 863 (audit): `leaf_count` bounds the restore PTY fan-out, so it
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

    /// Cycle 789 drift guard (audit D1, security). `sanitize_layout_name` is
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
        let p = std::env::temp_dir().join(format!(
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

    /// Cycle 585: a session.json swapped out for a multi-MB blob
    /// (filesystem-tampering scenario; out of strict scope but
    /// defense-in-depth) must not be read into RAM. The 16 MiB
    /// pre-read size cap returns None and renames the bomb to
    /// `.json.toobig.<unix-seconds>` for forensics, same pattern
    /// as the cycle-108 corrupted-file recovery path above.
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
        // Cycle-108 contract: a corrupted file (kettle killed mid-write,
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
        // Cycle-109 contract: save writes through a `.tmp` sibling then
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
        // `focus` likewise — pre-cycle sessions restore to first leaf.
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
    fn session_round_trips_focused_pane_index() {
        // Cycle 82: the session now records which leaf was focused so
        // restore brings the user back to the same pane within each tab.
        // Confirm the round-trip and that the default lands on first leaf
        // (preserving the pre-cycle behavior for older sessions).
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
        // keys) must still deserialize cleanly — to the pre-cycle defaults:
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
