//! winit application: window lifecycle, input routing, the tiled multiplexer,
//! the search overlay, clipboard, and live config reload.

use std::sync::Arc;

use anyhow::Result;
use kettle_config::{Action, Config, Key as KKey, Mods, Trigger};
use kettle_config::{TabBarMode, TabBarPos};
use kettle_core::{Scroll, TermEvent};
use kettle_render::{
    ContextMenu, ContextMenuRow, HighlightRect, HintLabel, Overlay, PaneSnapshot, PaneView,
    Renderer, TabActivity as RenderTabActivity, TabBar, TabSeg,
};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{
    CursorIcon, Fullscreen, Theme as WindowTheme, UserAttentionType, Window, WindowId,
};

use crate::input;
use crate::mux::{Dir, Mux, Rect};
use crate::window_state::WindowState;

/// Cycle 904 (audit): live state for a split-divider mouse drag — the addressed
/// split (`path`) and its orientation. The split's rect is re-fetched from
/// `split_seams` on every move (keyed by `path`) so a layout change mid-drag
/// (another pane closing) can't desync the ratio math.
pub(crate) struct SplitDrag {
    path: Vec<bool>,
    dir: Dir,
}

/// Cycle 929 (agent-first A2): an in-flight `run_command` awaiting its OSC-133
/// completion. The control server wrote `cmd\n` to the pane; the next
/// `CommandFinished` for that pane resolves the request with the exit code,
/// duration, and the output captured since `start_line`. A deadline guards the
/// no-shell-integration case (the command runs but no `CommandEnd` ever fires).
struct PendingRun {
    /// Connection that issued the run (for disconnect cleanup).
    conn_id: u64,
    /// Request id, echoed in the completion response.
    req_id: u64,
    /// Absolute scrollback line where the command's output begins, so the reply
    /// can slice just this command's output out of the grid.
    start_line: usize,
    /// When the request gives up waiting for OSC-133 and replies `timed_out`.
    deadline: std::time::Instant,
    /// Where the completion response is sent (the connection thread is blocked
    /// reading this until the command finishes or the deadline fires).
    reply: crate::ctl_server::ReplyTx,
}

/// Cycle 904 (audit): grab tolerance (px) for the thin split divider line, so
/// it's easy to hit with the mouse without pixel-perfect aim.
const SPLIT_SEAM_TOL: f32 = 5.0;

#[derive(Debug, Clone)]
pub enum UserEvent {
    Wakeup,
    ReloadConfig,
    /// Cycle 302 remote control: the remote-command file changed and
    /// the watcher needs the main thread to read + process new lines.
    /// One event per change (notify coalesces consecutive writes), so
    /// the main thread can batch-read all pending lines at once.
    RemoteCommand,
    /// Cycle 928 (agent-first A2): the control server enqueued a message
    /// (new connection / request / disconnect). The main thread drains the
    /// server channel and dispatches each request against `ws.mux`.
    Ctl,
    /// Cycle 794: the background update-check thread found a newer GitHub
    /// release. Carries the tag (e.g. `v2.6.0`) + the release-page URL so the
    /// UI can show a passive bottom banner. The banner is mouse-driven
    /// (left-click opens the release page + dismisses, right-click dismisses);
    /// keyboard users can bind the `open_update` / `dismiss_update` actions
    /// (cycle 809) — it stays non-modal so it never steals the terminal's keys.
    UpdateAvailable {
        tag: String,
        url: String,
    },
}

/// Cycle 752: decode kettle's embedded PNG into a winit window icon for the
/// *running* window — the title-bar system-menu glyph (top-left, beside the
/// minimize/maximize/close controls), the taskbar button, and the Alt-Tab
/// thumbnail. winit leaves the window icon unset by default, so Windows showed
/// the generic placeholder even though `build.rs` embeds the same art as an
/// `.exe` resource (that resource only covers Explorer / the file glyph / a
/// pinned shortcut — not the live window's `WM_SETICON`). The 256px source is
/// downscaled by the OS for the small title-bar icon and picked at the right
/// size for the taskbar / switcher. Best-effort: a decode failure leaves the
/// icon unset rather than aborting startup. No-op on Wayland (uses the
/// `.desktop` app_id) and macOS (uses the `.app` bundle icon); effective on
/// Windows and X11.
fn load_window_icon() -> Option<winit::window::Icon> {
    const ICON_PNG: &[u8] = include_bytes!("../../../packaging/linux/kettle-256.png");
    let img = image::load_from_memory(ICON_PNG).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    winit::window::Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// Cycle 768: macOS `sticky = true` — make the window appear on every Space
/// (Mission Control workspace). winit 0.30 dropped the native method, so we
/// reach the underlying `NSWindow` through the raw AppKit handle and set its
/// collection behavior — exactly what `set_visible_on_all_workspaces` did. The
/// objc2 / objc2-app-kit versions are pinned to winit's own (0.5 / 0.2) so the
/// ObjC class layout matches. Best-effort: a missing handle just leaves the
/// window on its current Space.
#[cfg(target_os = "macos")]
fn set_visible_on_all_spaces(window: &winit::window::Window) {
    use objc2_app_kit::{NSView, NSWindowCollectionBehavior};
    // winit re-exports raw-window-handle, so we don't need it as a direct dep.
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    // SAFETY: on macOS winit hands us a live `NSView*`, and `resumed` runs on
    // the main thread where AppKit requires UI mutations. We only borrow it.
    let view: &NSView = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
    if let Some(ns_window) = view.window() {
        // SAFETY: `setCollectionBehavior:` is an objc2 `unsafe fn` (any ObjC
        // message send is). The behavior bits are valid and the window is live.
        unsafe {
            ns_window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary,
            );
        }
    }
}

/// Translate a winit `MouseScrollDelta` into terminal lines, scaled by the
/// configured `scroll-multiplier`. `LineDelta` ticks are ~3 lines × mult;
/// `PixelDelta` is `y/20` × mult (~3 lines per typical notch). Pure.
fn wheel_lines(delta: &winit::event::MouseScrollDelta, multiplier: f32) -> i32 {
    let m = multiplier.max(0.0);
    let raw = match delta {
        winit::event::MouseScrollDelta::LineDelta(_, y) => y.round() * 3.0,
        winit::event::MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 20.0,
    };
    (raw * m).round() as i32
}

/// Cycle 609 (Terminator parity, `terminal.py:real_copy_clipboard` +
/// `config.py:smart_copy`): pure decision for what `Action::Copy` should
/// write to the clipboard.
///
///   * `selection` — `Some(s)` when the user actually has text
///     selected; `None` when the pane has no active selection.
///   * `smart_copy` — `cfg.smart_copy`. `true` (default) preserves
///     the existing clipboard if there's no selection; `false`
///     clobbers it with an empty string (Terminator's
///     deliberate-UX-choice mode).
///
/// Returns:
///   * `Some(s)` → write `s` to the clipboard (the new content).
///   * `None`    → don't touch the clipboard at all.
///
/// The `Some("")` case is the clobber path — the caller writes
/// the empty string AND treats the action as "no real copy" for
/// the `clear_select_on_copy` follow-up (no selection existed to
/// clear). Pure; unit-testable without a clipboard fixture.
fn copy_clipboard_decision(selection: Option<&str>, smart_copy: bool) -> Option<String> {
    match (selection, smart_copy) {
        (Some(s), _) => Some(s.to_string()),
        (None, false) => Some(String::new()),
        (None, true) => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PasteSource {
    Clipboard,
    Primary,
}

/// Terminator parity for `putty_paste_style_source_clipboard`: right-click
/// PuTTY paste defaults to the PRIMARY selection on Linux, matching terminal
/// mouse-paste convention. Users who expect Windows/PuTTY-style clipboard
/// paste can opt into the regular clipboard source.
fn putty_paste_source(source_clipboard: bool) -> PasteSource {
    if source_clipboard {
        PasteSource::Clipboard
    } else {
        PasteSource::Primary
    }
}

/// Cycle 604 (Terminator parity, `key_zoom_in` / `key_zoom_out` via
/// Ctrl+wheel): pure decision for whether the wheel notch should resize
/// the font.
///
///   * `ctrl` — Ctrl modifier held (the canonical zoom modifier across
///     gnome-terminal, Terminator, xterm-via-ctrl-shift-plus, etc.).
///   * `lines` — already-scaled `wheel_lines` result; sign drives the
///     zoom direction, zero short-circuits.
///   * `disabled` — `cfg.disable_mousewheel_zoom`. Opt-out for users
///     who scroll-zoom by accident (laptop touchpads + a Ctrl-meta
///     remap is a common collision).
///
/// Returns `Some(+1)` to grow, `Some(-1)` to shrink, `None` for no-op.
/// Extracted as a pure helper so the policy is unit-testable without
/// constructing a full App + winit event loop.
fn should_zoom_font(ctrl: bool, lines: i32, disabled: bool) -> Option<i32> {
    if !ctrl || disabled || lines == 0 {
        None
    } else if lines > 0 {
        Some(1)
    } else {
        Some(-1)
    }
}

/// Cycle 616 (Terminator parity, `plugins/auto_theme.py`):
/// pick the theme to switch to on `Action::ToggleLightDark`.
///
/// Rules (case-insensitive):
///   - both `light` and `dark` set:
///       - current matches `dark`  → return `light`
///       - current matches `light` → return `dark`
///       - otherwise               → return `dark` (default landing)
///   - only one set: return that one (so a half-configured user
///     still gets a one-way switch).
///   - neither set: return `None`; dispatch logs a warn.
///
/// Pure — unit-testable without constructing a Config or App.
/// Cycle 617: bridge from kettle-config's `SearchCaseSensitivity`
/// to kettle-core's `CaseSensitivity`. Kept as a pure helper so
/// the two crates don't grow a circular trait dependency.
fn map_case_sensitivity(m: kettle_config::SearchCaseSensitivity) -> kettle_core::CaseSensitivity {
    match m {
        kettle_config::SearchCaseSensitivity::Smart => kettle_core::CaseSensitivity::Smart,
        kettle_config::SearchCaseSensitivity::Always => kettle_core::CaseSensitivity::Always,
        kettle_config::SearchCaseSensitivity::Never => kettle_core::CaseSensitivity::Never,
    }
}

/// Cycle 622 (Terminator parity, `plugins/run_cmd_on_match.py`):
/// fire the configured argv as a fire-and-forget subprocess.
///
/// Security posture:
///   - argv form (no shell). The configured command is treated as
///     data, not as a shell string — kettle never invokes `sh -c`
///     on it. A configured `trigger = .* :: rm -rf $HOME` would
///     spawn `rm -rf $HOME` *literally*, with `$HOME` as a literal
///     argv element, not as the user's home dir.
///   - empty argv ⇒ no-op (parser already rejects, but be safe).
///   - spawn errors logged but otherwise ignored so a missing
///     binary doesn't loop-fail.
///
/// Stripped into a free fn so the dispatch arm stays small.
fn spawn_trigger_command(argv: &[String]) {
    if argv.is_empty() {
        return;
    }
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    // Don't capture stdout/stderr; the child inherits kettle's
    // file descriptors (where applicable). On Unix this means
    // stdout goes to wherever kettle was launched from — which
    // is acceptable for a user-configured fire-and-forget hook.
    if let Err(e) = cmd.spawn() {
        log::warn!(
            "trigger run-command failed to spawn {:?}: {e}",
            argv.first().map(String::as_str).unwrap_or("<empty>")
        );
    }
}

/// Cycle 652 (sub-cycle 4 of [`TERMINATOR-CONFIRM-DIALOG-DESIGN.md`](
/// ../../../docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md)): which named
/// key was pressed in the confirm-modal keyboard handler. Lifted out
/// of winit's `NamedKey` so the pure helper isn't coupled to a UI
/// framework type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKey {
    Escape,
    Enter,
    Tab,
    ShiftTab,
    Left,
    Right,
    /// v2.20.0 (`vim-menu-nav`): `y` — answer the dialog's QUESTION with
    /// yes, regardless of which button is focused.
    Yes,
    /// v2.20.0 (`vim-menu-nav`): `n` — dismiss without dispatching.
    No,
}

/// Cycle 652: outcome of a key press in the confirm dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKeyResult {
    /// Update `focus_idx` to the new value + redraw.
    Move(usize),
    /// Dispatch `on_confirm` + close the modal.
    Confirm,
    /// Close the modal without dispatching.
    Cancel,
    /// Key wasn't a nav key for the modal — caller can pass through
    /// or ignore (we suppress non-nav input while a modal is open).
    Ignore,
}

/// Cycle 652: pure helper that maps a (current_focus, num_buttons,
/// key) tuple to the next action for the confirm-dialog state
/// machine. Sub-cycle 5 wires this to the App's winit key handler.
/// Cycle 662 (sub-cycle 6 of confirm-dialog design): count the
/// leaf panes in a split-tree node. Used by the `Action::CloseTab`
/// dispatch to ask `should_prompt(scope_count)`.
fn count_leaves(node: &crate::mux::Node) -> usize {
    match node {
        crate::mux::Node::Leaf(_) => 1,
        crate::mux::Node::Split { a, b, .. } => count_leaves(a) + count_leaves(b),
    }
}

fn confirm_dialog_keypress(
    current_focus: usize,
    num_buttons: usize,
    key: ConfirmKey,
) -> ConfirmKeyResult {
    if num_buttons == 0 {
        return ConfirmKeyResult::Cancel;
    }
    match key {
        ConfirmKey::Escape => ConfirmKeyResult::Cancel,
        // Cycle 861 (audit): Enter activates the FOCUSED button, not always
        // Confirm. The close-confirm dialogs open focused on `Cancel` (index 0,
        // the safe default the renderer highlights); firing the destructive
        // action on Enter regardless of focus contradicted that highlight and
        // was a data-loss footgun. Buttons are `[Cancel, Confirm]`, so only the
        // last button confirms; any other focused button (Cancel) cancels.
        ConfirmKey::Enter => {
            if current_focus + 1 == num_buttons {
                ConfirmKeyResult::Confirm
            } else {
                ConfirmKeyResult::Cancel
            }
        }
        ConfirmKey::Tab => ConfirmKeyResult::Move((current_focus + 1) % num_buttons),
        ConfirmKey::ShiftTab => {
            ConfirmKeyResult::Move((current_focus + num_buttons - 1) % num_buttons)
        }
        ConfirmKey::Left => {
            if current_focus == 0 {
                ConfirmKeyResult::Ignore
            } else {
                ConfirmKeyResult::Move(current_focus - 1)
            }
        }
        ConfirmKey::Right => {
            if current_focus + 1 >= num_buttons {
                ConfirmKeyResult::Ignore
            } else {
                ConfirmKeyResult::Move(current_focus + 1)
            }
        }
        // v2.20.0 (`vim-menu-nav`): `y`/`n` answer the dialog directly —
        // unlike Enter (which fires the FOCUSED button, cycle 861), `y` is
        // an explicit answer to the question, so focus is irrelevant.
        ConfirmKey::Yes => ConfirmKeyResult::Confirm,
        ConfirmKey::No => ConfirmKeyResult::Cancel,
    }
}

/// Cycle 665 (sub-cycle 3 of [`TERMINATOR-VERTICAL-TABS-DESIGN.md`](
/// ../../../docs/TERMINATOR-VERTICAL-TABS-DESIGN.md)): default
/// strip width for vertical (Left / Right) tab bars when the
/// config doesn't supply one. The cycle-673 `tab-bar-width`
/// config key supersedes this in production; this constant is
/// the documented Firefox-sidebar-style fallback (180 px) the
/// cycle-651 + cycle-665 unit-tested layout helpers use when
/// invoked without an app-level cfg handle.
#[allow(dead_code)] // doc-only reference + cycle-651 test fixture; production uses cfg.tab_bar_width
pub const VERTICAL_TAB_STRIP_W: f32 = 180.0;

/// Cycle 651 + 665 (sub-cycles 2 + 3 of vertical-tabs design):
/// pure helper that computes the pane-content rect from the
/// surface size + bar metrics + edge each occupies.
///
/// Returns `(x, y, width, height)` in pixel coordinates.
///
/// Sub-cycle 3 now honors `TabBarPos::Left` and `Right` — the
/// strip claims a per-side width slice (`VERTICAL_TAB_STRIP_W`,
/// 180 px) instead of falling through to a per-edge height like
/// in cycle-651 v1.
///
/// Pure — no `&self`, no renderer, no winit. Drives the `App::area`
/// method (which now wraps this helper) so vertical-strip wiring
/// can be unit-tested without constructing a full App.
#[allow(dead_code)] // production callers use content_rect_for_with_strip; this wrapper drives the cycle-651 layout-math drift guards (app.rs:9411+)
fn content_rect_for(
    surface: (u32, u32),
    tab_bar_h: f32,
    status_bar_h: f32,
    tab_bar_pos: kettle_config::TabBarPos,
    status_bar_mode: kettle_config::StatusBarMode,
) -> Rect {
    content_rect_for_with_strip(
        surface,
        tab_bar_h,
        status_bar_h,
        tab_bar_pos,
        status_bar_mode,
        VERTICAL_TAB_STRIP_W,
    )
}

/// Cycle 673 (sub-cycle 7 of vertical-tabs design): explicit
/// strip-width variant so callers with `cfg.tab_bar_width` in
/// scope can pass it through. The non-`_with_strip` wrapper
/// above keeps the cycle-651 signature for code paths that
/// don't have a Config available.
fn content_rect_for_with_strip(
    surface: (u32, u32),
    tab_bar_h: f32,
    status_bar_h: f32,
    tab_bar_pos: kettle_config::TabBarPos,
    status_bar_mode: kettle_config::StatusBarMode,
    strip_w: f32,
) -> Rect {
    let (sw, sh) = (surface.0 as f32, surface.1 as f32);
    let tb_on_top = matches!(tab_bar_pos, kettle_config::TabBarPos::Top);
    let tb_on_bottom = matches!(tab_bar_pos, kettle_config::TabBarPos::Bottom);
    let tb_on_left = matches!(tab_bar_pos, kettle_config::TabBarPos::Left);
    let tb_on_right = matches!(tab_bar_pos, kettle_config::TabBarPos::Right);
    let sb_on_top = matches!(status_bar_mode, kettle_config::StatusBarMode::Top);
    let sb_on_bottom = matches!(status_bar_mode, kettle_config::StatusBarMode::Bottom);

    // Vertical: status bar still claims y-band (status is
    // always horizontal in v1); the strip claims an x-band.
    let top_offset =
        (if tb_on_top { tab_bar_h } else { 0.0 }) + (if sb_on_top { status_bar_h } else { 0.0 });
    let bot_offset = (if tb_on_bottom { tab_bar_h } else { 0.0 })
        + (if sb_on_bottom { status_bar_h } else { 0.0 });
    let left_offset = if tb_on_left { strip_w } else { 0.0 };
    let right_offset = if tb_on_right { strip_w } else { 0.0 };
    let content_h = (sh - top_offset - bot_offset).max(1.0);
    let content_w = (sw - left_offset - right_offset).max(1.0);
    (left_offset, top_offset, content_w, content_h)
}

/// Cycle 650 (sub-cycle 2 of [`TERMINATOR-TERMINALSHOT-DESIGN.md`](
/// ../../../docs/TERMINATOR-TERMINALSHOT-DESIGN.md)): build the
/// per-pane screenshot path. Lives under `<cache>/kettle/shots/`
/// (mirrors cycle-621 logger path scheme); falls back to
/// `./kettle-shots/` when no cache dir resolves.
///
/// File name shape: `kettle-<unix-secs>-<pid>.png`. Sub-cycle 3+
/// of the terminalshot design will call this from
/// `Action::TakeScreenshot` dispatch + queue a wgpu readback
/// request keyed on the path.
///
/// Pure modulo `unix_secs` + `cache_dir` — caller pins both.
// Cycle 720 (2026-05-23): removed stale `#[allow(dead_code)]`.
// Called from `Action::TakeScreenshot` dispatch at app.rs ~5426
// since cycle 689 (per-pane crop + toast notification).
fn session_screenshot_path(
    unix_secs: u64,
    pid: u32,
    cache_dir: Option<&std::path::Path>,
) -> std::path::PathBuf {
    let dir = cache_dir
        .map(|p| p.to_path_buf().join("kettle").join("shots"))
        .unwrap_or_else(|| std::path::PathBuf::from("kettle-shots"));
    dir.join(format!("kettle-{unix_secs}-{pid}.png"))
}

/// Cycle 621 (Terminator parity, `plugins/logger.py`): build the
/// per-pane session-log path. Lives under `<cache>/kettle/logs/`
/// (XDG-respecting via env probe; falls back to `./kettle-logs/`
/// when no cache dir is available).
///
/// File name shape: `kettle-<unix-secs>-<pid>.log`. unix-secs is
/// sortable; pid disambiguates simultaneous starts across windows.
///
/// Pure modulo the explicit `unix_secs` + `cache_dir` inputs —
/// caller pins both so the helper is fully unit-testable.
fn session_log_path(
    unix_secs: u64,
    pid: u32,
    cache_dir: Option<&std::path::Path>,
) -> std::path::PathBuf {
    let dir = cache_dir
        .map(|p| p.to_path_buf().join("kettle").join("logs"))
        .unwrap_or_else(|| std::path::PathBuf::from("kettle-logs"));
    dir.join(format!("kettle-{unix_secs}-{pid}.log"))
}

/// Cycle 621: locate the XDG cache dir for the current user without
/// pulling in the `dirs` crate. Probes `$XDG_CACHE_HOME` first
/// (the spec-canonical var) then `$HOME/.cache` on Linux/macOS,
/// then `$LOCALAPPDATA` on Windows-ish, returning `None` if none
/// are set (CI / container envs). Pure modulo the env-var reader
/// fn so tests can pin the env.
fn cache_dir_from_env<F: Fn(&str) -> Option<String>>(get: F) -> Option<std::path::PathBuf> {
    let var = |k: &str| get(k).filter(|s| !s.is_empty());
    // XDG_CACHE_HOME is the explicit cross-platform override.
    if let Some(p) = var("XDG_CACHE_HOME") {
        return Some(std::path::PathBuf::from(p));
    }
    // Cycle 919 (audit L1): per-OS fallback, matching `default_path_from`. On
    // Windows the canonical per-user cache dir is `%LOCALAPPDATA%`; a stray
    // `HOME` (git-bash / MSYS / WSL-interop export one) must NOT redirect
    // screenshots / crash logs to `~/.cache` — the same config-dir split-brain
    // that bit the config path (a shell launch vs a Start-menu launch would
    // disagree). On Unix, `HOME/.cache` is the standard XDG fallback.
    if cfg!(windows) {
        var("LOCALAPPDATA").map(std::path::PathBuf::from)
    } else {
        var("HOME").map(|h| std::path::PathBuf::from(h).join(".cache"))
    }
}

/// Cycle 620 (Terminator parity, terminatorlib/config.py:88
/// `homogeneous_tabbar`): per-tab widths for the tab-bar strip.
///
/// `homogeneous = true` (kettle + Terminator default) divides the
/// strip evenly across all tabs — `strip / n` per tab.
///
/// `homogeneous = false` sizes each tab by its title length: a
/// natural width of `title_chars * cell_w + chrome_w * 2 + close_btn_w`
/// where `chrome_w` is half the tab height (matching kettle's
/// existing inner padding) and `close_btn_w` is one tab-height
/// (matching the existing ✕ hit-zone in cycle-46). If the sum of
/// natural widths exceeds the strip, we silently fall back to
/// homogeneous so a many-tab window doesn't overflow.
///
/// Pure — no `&self`, no renderer, no winit. Tests can hand it
/// any title slice + strip width + cell metric.
fn compute_tab_segment_widths<'a>(
    titles: impl ExactSizeIterator<Item = &'a str>,
    strip: f32,
    cell_w: f32,
    tab_h: f32,
    homogeneous: bool,
) -> Vec<f32> {
    let titles: Vec<&str> = titles.collect();
    let n = titles.len().max(1);
    let strip = strip.max(1.0);
    if homogeneous {
        return vec![strip / n as f32; titles.len().max(1)];
    }
    let chrome = (tab_h * 0.5).max(4.0);
    let close_w = tab_h;
    let natural: Vec<f32> = titles
        .iter()
        .map(|t| {
            let chars = t.chars().count().max(1) as f32;
            (chars * cell_w + chrome * 2.0 + close_w).max(close_w * 1.5)
        })
        .collect();
    let sum: f32 = natural.iter().sum();
    if sum > strip {
        // Doesn't fit naturally — fall back to homogeneous so
        // every tab stays visible (no truncation of the strip).
        vec![strip / n as f32; titles.len().max(1)]
    } else {
        natural
    }
}

/// Cycle 805: split the trailing new-tab button into `(arrow_left, plus_right)`
/// hit-rects — the `▾` dropdown on the LEFT, the `+` on the RIGHT. `arrow_w` is
/// clamped to the button width so a degenerate (tiny) button can't yield a
/// negative `+` width. Pure → unit-tested and shared by the renderer geometry
/// + the click hit-test (single source of truth).
fn split_new_tab_button(
    button: kettle_render::Rect4,
    arrow_w: f32,
) -> (kettle_render::Rect4, kettle_render::Rect4) {
    let (x, y, w, h) = button;
    let aw = arrow_w.clamp(0.0, w);
    ((x, y, aw, h), (x + aw, y, (w - aw).max(0.0), h))
}

/// Cycle 618 (Terminator parity, key_next_profile / key_previous_profile):
/// pick the next profile name after `current`, wrapping at the end.
/// If `current` isn't in the list (e.g. user launched without `--profile`,
/// or with an explicit `--config FILE`), the cycle starts at index 0.
/// Pure — unit-testable without touching disk.
fn pick_next_profile(current: Option<&str>, names: &[String], forward: bool) -> String {
    let n = names.len();
    let cur_idx = current
        .and_then(|c| names.iter().position(|x| x == c))
        .unwrap_or(0);
    let next_idx = if forward {
        (cur_idx + 1) % n
    } else {
        (cur_idx + n - 1) % n
    };
    names[next_idx].clone()
}

fn pick_light_dark_target(current: &str, light: &str, dark: &str) -> Option<String> {
    let cur = current.trim().to_ascii_lowercase();
    let l = light.trim();
    let d = dark.trim();
    match (l.is_empty(), d.is_empty()) {
        (true, true) => None,
        (false, true) => Some(l.to_string()),
        (true, false) => Some(d.to_string()),
        (false, false) => {
            // Round-trip current ↔ {light, dark}. The "current is
            // a third-party theme" branch is collapsed into the
            // dark-default arm (cur == l ⇒ d, else ⇒ d would trip
            // clippy::if_same_then_else): we only need to check
            // whether current matches *light* explicitly, and
            // everything else (including current==dark and
            // third-party) ends up at dark — but dark→light needs
            // its own arm so the round-trip works.
            if cur == d.to_ascii_lowercase() {
                Some(l.to_string())
            } else {
                Some(d.to_string())
            }
        }
    }
}

/// Cap an OSC 52 clipboard payload so a hostile program can't make the
/// terminal allocate/set an unbounded clipboard. Truncates on a UTF-8
/// char boundary at or below `max` bytes (xterm/kitty also bound this).
fn clamp_osc52(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// 1 MiB — generous for real copies, bounded against abuse.
const OSC52_MAX: usize = 1 << 20;

/// 4 MiB cap on a *local* clipboard paste. The OSC 52 cap above guards
/// against a hostile remote program pushing an unbounded payload into
/// the system clipboard; this guards the reverse direction — a user
/// accidentally pastes a multi-GB file and kettle would otherwise feed
/// every byte into the PTY in one shot, freezing the terminal until
/// the program at the other end (cat? vim?) drained the pipe. 4 MiB
/// fits any realistic code-review / log-snippet paste with room to
/// spare; bigger pastes are almost certainly a fat-finger.
const LOCAL_PASTE_MAX: usize = 4 << 20;

/// Pure: when the mouse is over chrome (tab bar or any modal overlay), the
/// OS cursor should be the standard arrow rather than the text I-beam —
/// matches iTerm2 / WezTerm / Ghostty / kitty: chrome surfaces are
/// clickable, not selectable, so the I-beam is visually misleading there.
/// Returns `Some(Default)` for chrome, `None` to let the content-area
/// caller decide between `Pointer` (URL-hover) and `Text`.
///
/// Cycle 320: `in_chrome_band` extended to also be true when the
/// cursor is over the cycle-295 status bar. Same logic — over any
/// kettle-chrome strip, show the OS arrow cursor rather than the
/// I-beam terminal text-input style.
fn chrome_cursor_icon(in_chrome_band: bool, modal_open: bool) -> Option<CursorIcon> {
    if in_chrome_band || modal_open {
        Some(CursorIcon::Default)
    } else {
        None
    }
}

/// Pure: when the cursor is over a tab's `✕` close-button zone, override the
/// chrome `Default` with `Pointer` — the same "hand" icon every browser
/// (Chrome / Firefox / Safari) uses to telegraph "this glyph is clickable."
/// Composes with `chrome_cursor_icon`: close-hover wins over the bar's
/// `Default` so the affordance is visible the moment the cursor lands on
/// the close zone. Returns `None` when not over a close button so the
/// content-area decision still gets a chance.
fn tab_close_hover_icon(over_close: bool) -> Option<CursorIcon> {
    if over_close {
        Some(CursorIcon::Pointer)
    } else {
        None
    }
}

/// Pure: which tab segment's close-button (`✕`) rect contains the cursor,
/// if any. Walks `segments` once and returns the first hit (segments don't
/// overlap in the tab bar layout; cycle-tab-bar invariant). Used both to
/// drive the OS pointer-cursor swap and the renderer's hover-background
/// quad — the visual affordance that makes the `✕` read as a button rather
/// than a trailing character in the title text.
fn hovered_close_button(segments: &[kettle_render::TabSeg], px: f32, py: f32) -> Option<usize> {
    let in_rect = |(rx, ry, rw, rh): (f32, f32, f32, f32)| {
        px >= rx && px < rx + rw && py >= ry && py < ry + rh
    };
    segments.iter().find(|s| in_rect(s.close)).map(|s| s.idx)
}

/// Pure geometry: is the mouse y-coordinate inside the tab bar's vertical
/// band? `bar_h` is the bar height in pixels, `surface_h` is total window
/// height, `pos` is the tab-bar position config. Extracted so the cycle-tab-
/// on-wheel-over-tab-bar decision is fully unit-tested.
/// Cycle 393 (Terminator parity, titlebar Bucket-D sub-cycle 10):
/// pure geometry helper for per-pane titlebar hit-testing. Returns
/// Some(idx) when the click landed inside the titlebar y-band of
/// pane idx; None otherwise. Pulled out of App::pane_at_titlebar_click
/// so it can be drift-guarded.
#[allow(clippy::type_complexity)]
pub(crate) fn pane_titlebar_hit(
    px: f32,
    py: f32,
    pane_rects: &[(u64, (f32, f32, f32, f32))],
    title_at_bottom: bool,
    bar_h: f32,
) -> Option<u64> {
    for (id, (rx, ry, rw, rh)) in pane_rects {
        let (bar_top, bar_bot) = if title_at_bottom {
            (*ry + *rh - bar_h, *ry + *rh)
        } else {
            (*ry + 1.0, *ry + 1.0 + bar_h)
        };
        if px >= *rx && px < *rx + *rw && py >= bar_top && py < bar_bot {
            return Some(*id);
        }
    }
    None
}

/// SGR mouse base code for the extra "side" mouse buttons, or `None` for any
/// button kettle handles locally (left/middle/right) or doesn't forward.
///
/// Cycle 810 (audit): the press/release handlers used to drop every button
/// past right-click (`_ => return`), so a 5-button mouse's Back / Forward
/// never reached a mouse-tracking TUI (tmux/vim bindings, pagers). xterm
/// encodes buttons 8–11 as `128 + (button - 8)`; winit's `Back` is XBUTTON1
/// (button 8 → 128) and `Forward` is XBUTTON2 (button 9 → 129). These have no
/// local UI meaning, so they only do anything while mouse tracking is on.
fn extra_mouse_sgr(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Back => Some(128),
        MouseButton::Forward => Some(129),
        _ => None,
    }
}

/// Whether an OSC 7 working directory is safe to turn into a `file://` URL and
/// hand to the OS opener.
///
/// Cycle 816 (audit): the cwd comes from untrusted PTY output (`parse_osc7`
/// stores whatever a program reports), and a crafted `file://x//evil.host/share`
/// yields a stored cwd of `//evil.host/share`. Building `file://{cwd}` from that
/// produces a UNC-style URL that, when opened on Windows, connects to the
/// attacker over SMB and leaks the user's NTLM hash. The cycle-815
/// `is_safe_url` locality check already rejects the resulting URL, but this is
/// the one call site that *constructs* `file://` from raw untrusted input, so
/// it gets an explicit local-only guard too (defense-in-depth). Crucially this
/// check does NO filesystem stat — `Path::is_dir` on a `//host` path would
/// itself route over SMB — it's a pure string check: reject UNC/authority
/// (`//`, leading `\`) and traversal.
fn cwd_is_local(cwd: &str) -> bool {
    !cwd.is_empty() && !cwd.starts_with("//") && !cwd.starts_with('\\') && !cwd.contains("..")
}

/// Pure pointer → (col, line) math for a pane, shared by `px_to_point` so the
/// per-pane-titlebar inset is drift-tested.
///
/// Cycle 817 (audit): the renderer draws a multi-pane tab's cell content at
/// `oy = ry + padding_y + titlebar_h` (the per-pane titlebar reserves
/// `titlebar_h` at the top), but the hit-test used to map from `ry + padding_y`
/// — so in the DEFAULT config the moment you split a pane, every pointer landed
/// ~1 row too high: selection, link targeting, and the mouse-tracking row
/// reported to vim/tmux/htop were all off by one. Subtracting the same
/// `titlebar_h` here realigns the hit-test with what's drawn. Col/line clamp
/// to ≥ 0 so a click in the chrome/padding doesn't underflow.
/// Cycle 876: record a keystroke into the dev recorder as a privacy-preserving
/// token. Named keys and modified chords (`Enter`, `Ctrl+c`, `ArrowUp`) are
/// recorded by name — they aren't secret. A bare printable character is recorded
/// only as a redacted class glyph unless raw-input was opted into, so a typed
/// password never lands in the trace (its keystroke count + timing still do).
#[cfg(feature = "dev-record")]
fn dev_record_key(
    rec: &mut crate::dev_record::Recorder,
    key: &winit::keyboard::Key,
    mods: ModifiersState,
) {
    use winit::keyboard::Key;
    let mut prefix = String::new();
    if mods.control_key() {
        prefix.push_str("Ctrl+");
    }
    if mods.alt_key() {
        prefix.push_str("Alt+");
    }
    if mods.super_key() {
        prefix.push_str("Super+");
    }
    let token = match key {
        Key::Named(nk) => Some(format!("{prefix}{nk:?}")),
        Key::Character(s) if !prefix.is_empty() => Some(format!("{prefix}{}", s.as_str())),
        Key::Character(s) => Some(crate::dev_record::printable_token(
            s.as_str(),
            rec.raw_input(),
        )),
        _ => None,
    };
    if let Some(t) = token {
        rec.record_input(&t);
    }
}

/// Pure pointer → `(col, line, side)` math for a pane, shared by `px_to_point`.
///
/// `side` is which half of the hit cell the pointer sits in, derived from the
/// sub-cell x offset: the left half is `Side::Left`, the right half (and the
/// exact midpoint) is `Side::Right`. This matches xterm / Alacritty / iTerm2 and
/// is exactly what alacritty's `Selection::to_range` (`range_simple`/`range_block`)
/// needs to decide whether each boundary cell is included — without it a drag is
/// biased one cell wide (the historical "selection off by one letter"). The side
/// is computed from the SAME clamped non-negative offset as `col`, so a pointer
/// left of the content origin maps to `(col 0, Side::Left)` — the first cell is
/// included, never trimmed. NOTE: only Simple/Block *drag* selections consume this
/// side; word / line / smart-select snap to token boundaries and ignore it (see
/// `begin_selection` / `apply_smart_selection`).
fn px_to_cell(
    px: f32,
    py: f32,
    rect: Rect,
    cell: (f32, f32),
    pad: (f32, f32),
    titlebar_h: f32,
) -> (usize, i32, kettle_core::Side) {
    let (rx, ry, _, _) = rect;
    let (cw, ch) = cell;
    let (pad_x, pad_y) = pad;
    // Derive BOTH col and side from the same non-negative offset so they agree at
    // the left clamp: a pointer in the left padding (offset < 0) maps to
    // (col 0, Side::Left) — the first cell is included, not trimmed. Computing the
    // side from the raw (negative) offset via `rem_euclid` would wrap it into the
    // cell's right half and wrongly drop column 0 from a drag (audit, v2.25.0).
    let offx = (px - rx - pad_x).max(0.0);
    let col = (offx / cw).floor() as usize;
    let line = ((py - ry - pad_y - titlebar_h) / ch).floor().max(0.0) as i32;
    let side = if offx.rem_euclid(cw) < cw / 2.0 {
        kettle_core::Side::Left
    } else {
        kettle_core::Side::Right
    };
    (col, line, side)
}

/// Cycle 909 (R1 — selection/copy while scrolled back): map a VIEWPORT-relative
/// point (line 0 = top visible row, what `px_to_point` returns) to the
/// GRID-ABSOLUTE point that alacritty's `Selection` / `selection_to_string` /
/// `to_range` and `grid[..]` indexing expect. It subtracts the focused pane's
/// `display_offset` via alacritty's own `viewport_to_point`. Without this, a
/// selection or grid-row read taken while scrolled into history addressed the
/// active-screen row instead of the scrolled-to row — so the copy returned the
/// wrong/empty text and the highlight slipped down by the scroll amount (the
/// bug is invisible at the bottom, where `display_offset == 0` makes viewport
/// and absolute coincide). Pure (drift-tested in
/// `viewport_point_to_grid_applies_display_offset`).
fn viewport_point_to_grid(
    viewport: kettle_core::Point,
    display_offset: usize,
) -> kettle_core::Point {
    let line = viewport.line.0.max(0) as usize;
    kettle_core::viewport_to_point(
        display_offset,
        kettle_core::Point::new(line, viewport.column),
    )
}

/// Cycle 910 (R2): minimum wall-clock between output-driven frames — a 60 fps
/// paint cap (the standard terminal/display refresh target; Alacritty/WezTerm do
/// the same). Imperceptible for keystroke echo / streaming output, large enough
/// to collapse a multi-read repaint burst into one settled frame, and — vs an
/// uncapped or 125 fps repaint — it roughly halves the paint-side CPU a chatty
/// re-rendering TUI (Claude Code's spinner, a progress bar) would otherwise burn.
const OUTPUT_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(16);

/// Cycle 910 (R2): whether an output-driven repaint should be DEFERRED
/// (coalesced) rather than painted now — true when the previous frame painted
/// less than `budget` ago. Capping PTY-output paints to one per budget lets a
/// non-atomic repaint burst (an app that doesn't bracket frames with DEC 2026
/// synchronized output, e.g. Claude Code) settle before kettle snapshots the
/// grid, avoiding the transient mid-repaint cursor jump. Pure (drift-tested in
/// `output_paint_coalesces_within_frame_budget`).
fn should_defer_output_paint(
    now: std::time::Instant,
    last_paint: Option<std::time::Instant>,
    budget: std::time::Duration,
) -> bool {
    match last_paint {
        Some(t) => now.saturating_duration_since(t) < budget,
        None => false,
    }
}

/// v2.21.1 (throughput): the output-paint budget GROWS under a sustained flood.
/// Each output-driven frame that had to be coalesced — i.e. output arriving
/// faster than the base 60 fps budget — bumps the window's `flood_paints`
/// counter; once a flood is sustained kettle paints less often (30 fps, then
/// 20 fps). On-screen content during a flood is unreadable scrolling anyway,
/// and every frame NOT painted is one fewer O(cells) `PaneSnapshot::capture`
/// taken under the pane's `Term` mutex — the SAME mutex the PTY reader thread
/// must hold to run `Processor::advance`. At 60 fps the main thread grabs that
/// lock ~60×/s under flood, throttling the parser on a CPU-contended box;
/// stretching the budget hands the lock (and the cores) back to the reader, so
/// flood throughput rises. A brief burst (< 4 coalesced frames ≈ 64 ms) never
/// throttles, so keystroke echo and short bursts stay at full 60 fps, and the
/// counter resets the instant output drops below the budget (see `redraw`), so
/// the settled post-flood frame paints within one budget. Pure; drift-tested in
/// `effective_output_budget_grows_under_sustained_flood`.
fn effective_output_budget(flood_paints: u32) -> std::time::Duration {
    match flood_paints {
        0..=3 => OUTPUT_FRAME_BUDGET, // 16 ms / 60 fps — responsive default
        4..=15 => std::time::Duration::from_millis(33), // ~30 fps
        _ => std::time::Duration::from_millis(50), // ~20 fps — sustained flood
    }
}

/// PERF (key-repeat stutter fix): output that lands within this window of a
/// keystroke is ECHO — it paints immediately, skipping the coalescer. Long
/// enough to bridge OS key-repeat intervals (~33ms at default rates) plus
/// ConPTY echo latency; short enough that an unrelated burst (a build log
/// kicking in a beat after you pressed Enter twice) re-enters the coalescer
/// quickly.
const TYPING_ECHO_WINDOW: std::time::Duration = std::time::Duration::from_millis(150);

/// Pure (drift-tested in `typed_echo_bypasses_the_output_coalescer`): is
/// `now` inside the typing-echo window after the last keystroke?
fn typed_recently(
    now: std::time::Instant,
    last_typed: Option<std::time::Instant>,
    window: std::time::Duration,
) -> bool {
    last_typed.is_some_and(|t| now.saturating_duration_since(t) < window)
}

/// Pure cols/rows-that-fit math for a pane rect, shared by `grid_of`. Cycle 817
/// (audit): a multi-pane tab's per-pane titlebar steals `titlebar_h` of height,
/// so the PTY must be sized for the rows that actually fit *below* it — without
/// this, `grid_of` over-reported rows by ~1 and the bottom row was drawn under
/// the chrome / clipped. `max(1)` keeps a degenerate tiny pane at ≥ 1×1.
fn grid_dims_px(
    size: (f32, f32),
    cell: (f32, f32),
    pad: (f32, f32),
    titlebar_h: f32,
) -> (usize, usize) {
    let (w, h) = size;
    let (cw, ch) = cell;
    let (pad_x, pad_y) = pad;
    let cols = ((w - pad_x * 2.0) / cw).floor().max(1.0) as usize;
    let rows = ((h - pad_y * 2.0 - titlebar_h) / ch).floor().max(1.0) as usize;
    (cols, rows)
}

fn cursor_in_tab_bar_band(y: f32, bar_h: f32, surface_h: f32, pos: TabBarPos) -> bool {
    if bar_h <= 0.0 {
        return false;
    }
    // Cycle 668 (vertical-tabs sub-cycle 4): Left/Right strips
    // span the full window height — every y-coordinate inside
    // the window is in the "tab bar band" along the y-axis.
    // The x-axis distinction (which side of the window) is
    // handled by `cursor_in_tab_bar` which checks the cursor's
    // x against the strip's edge.
    match pos {
        TabBarPos::Top => y >= 0.0 && y < bar_h,
        TabBarPos::Bottom => y >= (surface_h - bar_h) && y <= surface_h,
        TabBarPos::Left | TabBarPos::Right => y >= 0.0 && y <= surface_h,
    }
}

/// Cycle 320: sibling of `cursor_in_tab_bar_band` for the cycle-295
/// status bar. Without this, hovering on the status strip showed
/// the terminal I-beam cursor (because the strip isn't part of any
/// pane's rect but isn't part of the tab-bar band either, so it
/// falls through to the "over a pane" branch by default). Now the
/// chrome-cursor logic can treat both bars uniformly.
fn cursor_in_status_bar_band(
    y: f32,
    bar_h: f32,
    surface_h: f32,
    pos: kettle_config::StatusBarMode,
) -> bool {
    if bar_h <= 0.0 {
        return false;
    }
    match pos {
        kettle_config::StatusBarMode::Off => false,
        kettle_config::StatusBarMode::Top => y >= 0.0 && y < bar_h,
        kettle_config::StatusBarMode::Bottom => y >= (surface_h - bar_h) && y <= surface_h,
    }
}

/// Auto-scroll rate when the user drags a selection past the focused pane's
/// content area. Positive value = scroll *up* into history (cursor above
/// the top edge); negative = scroll *down* toward the present (cursor below
/// the bottom edge); zero when inside the pane (no autoscroll needed).
///
/// Speed scales with how far past the edge the cursor sits — a small
/// overshoot crawls (1 line/frame), a big one (40+ px) chases at 3 lines
/// per frame. Pure so the cadence is unit-tested without spinning up a
/// renderer or PTY.
fn selection_autoscroll_lines(y: f32, rect_top: f32, rect_bottom: f32) -> i32 {
    let dist = if y < rect_top {
        rect_top - y
    } else if y > rect_bottom {
        rect_bottom - y // negative
    } else {
        return 0;
    };
    // Magnitude → 1..=3 lines/frame ladder.
    let mag = if dist.abs() >= 40.0 {
        3
    } else if dist.abs() >= 10.0 {
        2
    } else {
        1
    };
    if dist > 0.0 { mag } else { -mag }
}

/// Render the OS window title from the user's `window-title-format`
/// template with the active pane's title / cwd / 1-based tab index. While
/// the pane title is still the initial placeholder ("kettle") and the cwd
/// is known, substitute the cwd's basename so `~/Repos/kettle` shows up
/// as `kettle — kettle` and `~/Documents` as `Documents — kettle`,
/// matching the cycle-89 tab-title fallback so the OS window title and
/// the in-app tab agree pre-OSC 2. Real shell-set titles still win the
/// moment they arrive.
fn window_title(template: &str, pane_title: &str, cwd: &str, tab: usize) -> String {
    let t_raw = pane_title.trim();
    let pane_placeholder = t_raw.is_empty() || t_raw == "kettle";
    let cwd_basename = std::path::Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty());
    // If the shell hasn't set a real title yet but we know the cwd,
    // substitute the cwd basename — same behavior as the cycle-89 tab
    // title fallback so the OS window title and the in-app tab agree
    // pre-OSC 2. The cwd basename is allowed to literally equal
    // "kettle" (e.g. cwd `~/Repos/kettle`); only the *placeholder* case
    // bails out, otherwise the template would never get to use a real
    // directory just because the name collides with the app's.
    if pane_placeholder {
        return match cwd_basename {
            Some(name) => {
                let tab = tab.to_string();
                kettle_config::template::fill(
                    template,
                    &[("title", name), ("cwd", cwd), ("tab", &tab)],
                )
            }
            None => "kettle".to_string(),
        };
    }
    let tab = tab.to_string();
    kettle_config::template::fill(template, &[("title", t_raw), ("cwd", cwd), ("tab", &tab)])
}

/// Shell-quote a dropped file path so the user can press Enter without
/// having to escape spaces / special chars by hand. POSIX-style single
/// quoting: wrap in `'…'`, replace internal `'` with `'\''` (close the
/// quote, escape the literal apostrophe, reopen). This is the most
/// portable form — bash / zsh / fish accept it identically, and so does
/// PowerShell 7+ (which kettle users on Windows typically run; the
/// single-quote-string syntax there matches POSIX for non-apostrophe
/// content). cmd.exe is the outlier, but it's a rare top-level shell on
/// modern Windows + the user can always re-edit before Enter. Always
/// quotes — even for plain paths — to keep the output predictable and
/// avoid a regex matching exercise on what's "special" across shells.
/// Pure so the quoting rule is unit-tested.
fn shell_quote_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Cycle 298-301 vi-mode (Alacritty parity). Carries the vi cursor's
/// position + the visual-selection anchor when vi-mode is active.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ViState {
    /// Vi cursor row in the focused pane's grid coordinates (0 = top
    /// of viewport).
    row: usize,
    /// Vi cursor column.
    col: usize,
    /// Cycle 301 sub-cycle 4: `Some((row, col))` after the user
    /// presses `v` to start a visual selection. The selection spans
    /// from this anchor to the current (row, col). `y` yanks the
    /// selection to the clipboard and exits vi-mode.
    visual_anchor: Option<(usize, usize)>,
}

/// Cycle 308: pure-helper char-boundary truncation for status-bar
/// titles. Caps at `max` chars; appends `…` when truncated so the
/// elision is visible. Uses char count (not bytes) so UTF-8
/// multibyte glyphs aren't split. Returns the original string if it
/// already fits.
///
/// Before this helper, a long pane title fed to the cycle-296
/// status bar would wrap past the strip's 1-cell height — the user
/// saw the first ~80 chars and the rest was invisible with no
/// indication.
fn cap_title_for_status_bar(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let mut out: String = title.chars().take(max).collect();
    out.push('…');
    out
}

/// Cycle 937 (Peacock): a STABLE per-window seed for `accent-color = auto`,
/// hashed from the window's working directory (the launch `-d DIR`, else the
/// process cwd). Same project → same seed → same accent across launches;
/// different projects → different accents. Pure given the cwd.
fn accent_seed_from_cwd(cwd: Option<&std::path::Path>) -> u64 {
    use std::hash::{Hash, Hasher};
    let dir = cwd
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    // Cycle 942 (audit): canonicalize so every spelling of the same project
    // (`-d .`, `-d C:\proj`, a relative path, a trailing slash) hashes to the
    // SAME seed — the documented "same project → same accent" stability.
    // Falls back to the raw path when it doesn't resolve (nonexistent dir).
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut h);
    h.finish()
}

/// Cycle 934 (agent-first A4) + cycle 941 (Terminator parity "Read only"):
/// the per-pane titlebar label, composed from the pane's state badges —
/// `[RO] ` while the pane is read-only (input dropped before the PTY), then
/// the `agent-badge` when an agent control connection has the pane attached.
/// Pure (unit-tested). No badges → the title unchanged (zero cost for the
/// common case).
fn compose_pane_title(badge: &str, attached: bool, read_only: bool, title: &str) -> String {
    let agent = attached && !badge.is_empty();
    if !read_only && !agent {
        return title.to_string();
    }
    let mut out = String::new();
    if read_only {
        out.push_str("[RO] ");
    }
    if agent {
        out.push_str(badge);
    }
    out.push_str(title);
    out
}

/// Map a click count + the Alt modifier to a selection type: double =
/// word, triple = line, single = a normal drag, and Alt+single =
/// rectangular/block selection (iTerm2/Alacritty/WezTerm parity).
fn selection_kind(clicks: u8, alt: bool) -> kettle_core::SelectionType {
    use kettle_core::SelectionType::*;
    match clicks {
        2 => Semantic,
        3 => Lines,
        _ if alt => Block,
        _ => Simple,
    }
}

/// Cycle 290: compile each configured `OutputTrigger`'s pattern to a
/// `regex::Regex`, log + drop invalid patterns. Pure helper so the
/// App constructor + `reload_config` path use exactly the same
/// regex set after a config edit. An empty input returns an empty
/// vec — `match_triggers` short-circuits on that.
fn compile_triggers(
    triggers: &[kettle_config::OutputTrigger],
) -> Vec<(regex::Regex, kettle_config::TriggerAction)> {
    let mut out = Vec::with_capacity(triggers.len());
    for t in triggers {
        match regex::Regex::new(&t.pattern) {
            Ok(re) => out.push((re, t.action.clone())),
            Err(e) => {
                log::warn!("trigger pattern {:?} failed to compile: {e}", t.pattern);
            }
        }
    }
    out
}

/// Cycle 290: scan `text` for any compiled trigger match, returning the
/// first action that fires. Pure helper used by `App::run_triggers` and
/// the drift guard. Returns `None` when no trigger fires so the caller
/// can skip the urgency-attention call.
fn match_triggers(
    text: &str,
    triggers: &[(regex::Regex, kettle_config::TriggerAction)],
) -> Option<kettle_config::TriggerAction> {
    triggers.iter().find_map(|(re, action)| {
        let caps = re.captures(text)?;
        Some(match action {
            // v2.20.0 (Terminator `run_cmd_on_match.py` parity completion):
            // the matched pattern's capture groups substitute into the
            // command's argv (`{0}` whole match, `{1}`… numbered groups) —
            // Terminator does `cmd.format(*groups)`.
            kettle_config::TriggerAction::RunCommand(argv) => {
                kettle_config::TriggerAction::RunCommand(substitute_trigger_groups(argv, &caps))
            }
            other => other.clone(),
        })
    })
}

/// v2.20.0: replace `{0}`/`{1}`… in each argv element with the trigger
/// match's capture groups (`{0}` = whole match; a non-participating group
/// substitutes empty; an OUT-OF-RANGE reference like `{9}` with two groups
/// stays literal so the config typo is visible in the spawned command
/// rather than silently vanishing). Substitution is per-element string
/// replacement and argv STAYS argv — matched output can inject an
/// argument's VALUE but never new arguments or shell metacharacters (the
/// spawn is `std::process::Command`, no shell). Pure (unit-tested).
fn substitute_trigger_groups(argv: &[String], caps: &regex::Captures) -> Vec<String> {
    argv.iter()
        .map(|a| {
            if !a.contains('{') {
                return a.clone();
            }
            // Single LEFT-TO-RIGHT pass over the TEMPLATE (review fix): the
            // old sequential `String::replace` loop re-scanned its own
            // output, so a capture whose MATCHED TEXT contained `{2}` got
            // expanded a second time with attacker-controlled content.
            // Substituted text is emitted verbatim and never re-scanned.
            let bytes = a.as_bytes();
            let mut out = String::with_capacity(a.len());
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'{' {
                    let mut j = i + 1;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j > i + 1
                        && j < bytes.len()
                        && bytes[j] == b'}'
                        && let Ok(idx) = a[i + 1..j].parse::<usize>()
                        && idx < caps.len()
                    {
                        out.push_str(caps.get(idx).map(|m| m.as_str()).unwrap_or(""));
                        i = j + 1;
                        continue;
                    }
                    // Not a valid in-range placeholder: the `{` stays
                    // literal (an out-of-range `{9}` keeps the typo visible).
                    out.push('{');
                    i += 1;
                } else {
                    let ch = a[i..].chars().next().expect("in-bounds char");
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
            out
        })
        .collect()
}

/// Cycle 288 smart selection (iTerm2 parity). When a double-click
/// lands inside a `kettle_core::hints::detect` match (URL, file path,
/// IPv4, git SHA), return the match's `[start_col, end_col]` inclusive
/// range so the caller can build a `Simple` selection spanning the
/// whole match instead of the alacritty_terminal `Semantic` word that
/// usually under- or over-shoots a structured token.
///
/// Pure helper: takes the single line of text the click landed on
/// plus the click's column, returns `Some((start, end))` if any hint
/// match contains the cursor, or `None` to fall through to the
/// existing word-boundary semantic selection.
fn smart_selection_at(line: &str, col: usize) -> Option<(usize, usize)> {
    let spans = kettle_core::hints::detect(&[line]);
    spans
        .into_iter()
        .find(|s| s.start <= col && col <= s.end)
        .map(|s| (s.start, s.end))
}

/// One on-screen quick-select target: where its label sits and what it is.
#[derive(Clone)]
pub(crate) struct HintTarget {
    row: usize,
    col: usize,
    label: String,
    kind: kettle_core::hints::Kind,
    text: String,
}

/// One entry in the right-click context menu. `Separator` rows render
/// as a thin divider in the menu and are skipped during keyboard nav
/// Cycle 375: a context-menu click resolves to either a kettle
/// Action (built-in items) or a Lua callback index (kettle.add_menu_item
/// entries).
#[derive(Clone)]
enum ContextMenuClick {
    Action(Action),
    LuaMenuItem(usize),
    /// Cycle 611 (Terminator parity, `custom_commands.py`): a
    /// `menu-item = LABEL = CMD` config entry. Dispatch writes
    /// `CMD\n` to the focused pane's PTY.
    ConfigCommand(String),
    /// Cycle 685 (Terminator parity, sub-cycle 2 of
    /// [`TERMINATOR-THEME-SUBMENU-DESIGN.md`](
    /// ../../../docs/TERMINATOR-THEME-SUBMENU-DESIGN.md)):
    /// theme picked from the right-click "Theme ▸" submenu.
    /// Dispatch sets cfg.theme_name + cfg.theme and triggers a
    /// redraw (same path as cycle-3514 `NextTheme`).
    SetTheme(String),
    /// Cycle 686 (sub-cycle 8 of theme-submenu design): profile
    /// picked from the right-click "Profile ▸" submenu. Dispatch
    /// sets `App::config_path` to the cycle-618 profile path
    /// and calls `reload_config`.
    SetProfile(String),
    /// Cycle 687 (sub-cycle 3 of theme-submenu design): drill
    /// into a `Submenu` row by index. The click handler pushes
    /// the current items onto `drill_stack` + replaces them
    /// with the submenu's items.
    DrillIntoSubmenu(usize),
    /// Cycle 805: open a new tab running an explicit argv (a shell picked from
    /// the new-tab `▾` dropdown). Dispatch calls `Mux::new_tab_with` with the
    /// focused tab's current working directory.
    NewTabWithArgv(Vec<String>),
    /// Cycle 941 (Terminator parity, terminal_popup_menu.py "Open link" /
    /// "Copy address"): a click on one of the URL-aware leading rows.
    /// `copy: true` puts the address on the clipboard; `copy: false` opens it
    /// through the cycle-374 `open_url` chain (Lua handler →
    /// custom_url_handler → system open, `is_safe_url`-guarded).
    Url {
        url: String,
        copy: bool,
    },
}

/// and click dispatch; `Item` rows carry the action to fire.
#[derive(Clone)]
enum ContextMenuItem {
    Item {
        label: &'static str,
        action: Action,
        enabled: bool,
    },
    /// Cycle 717 (Preferences submenu, C8): like `Item` but with an
    /// owned `String` label so dynamic state markers (radio `● / ○`,
    /// check `✓ /  `) can be baked into the label at build time
    /// without leaking memory via `Box::leak`. Same dispatch surface
    /// as `Item` (typed `Action`).
    DynamicItem {
        label: String,
        action: Action,
        enabled: bool,
    },
    Separator,
    /// Cycle 375 (Terminator plugin parity, plugin sub-cycle 8):
    /// menu item supplied by a Lua plugin via `kettle.add_menu_item(
    /// label, callback)`. Dispatch invokes the registered Lua
    /// callback (looked up by `lua_idx` in the kettle_menu_items
    /// table) instead of an Action.
    LuaItem {
        label: String,
        lua_idx: usize,
    },
    /// Cycle 611 (Terminator parity, `custom_commands.py`): user-
    /// defined menu entry from a config-file `menu-item = LABEL =
    /// CMD` line. Dispatch writes `command + "\n"` to the focused
    /// PTY. Simpler than `LuaItem` — no callback, just literal
    /// text. Use this for "Run `clear`" / "Open `~/.bashrc` in
    /// `$EDITOR`" / etc.
    ConfigItem {
        label: String,
        command: String,
    },
    /// Cycle 684 (Terminator parity, sub-cycle 1 of
    /// [`TERMINATOR-THEME-SUBMENU-DESIGN.md`](
    /// ../../../docs/TERMINATOR-THEME-SUBMENU-DESIGN.md)):
    /// recursive variant carrying a nested item list. v1 of
    /// the renderer just appends "▸" to the label (no flyout
    /// yet); sub-cycle 3 wires the second-panel flyout +
    /// hover-delay state machine + window-edge clipping.
    /// Lands the type now so the renderer + dispatch can
    /// compile against the final shape ahead of the
    /// interaction wiring.
    Submenu {
        label: String,
        // `items` is the nested item list. Consumed by the cycle-687
        // drill-in dispatch (`ContextMenuClick::DrillIntoSubmenu`)
        // at app.rs ~7345 — clicking a Submenu row pushes the
        // parent items onto `drill_stack` and replaces them with
        // the submenu's items.
        items: Vec<ContextMenuItem>,
    },
    /// Cycle 685 (Terminator parity, sub-cycle 2 of theme-submenu
    /// design): a theme-choice leaf row used inside a
    /// `Submenu { label: "Theme", … }`. Clicking dispatches
    /// `ContextMenuClick::SetTheme(theme)` which swaps the
    /// current theme to the named one.
    // Cycle 720 (2026-05-23): removed stale `#[allow(dead_code)]`.
    // The flyout-side click dispatch landed at cycle 687/688.
    ThemeChoice {
        label: String,
        theme: String,
    },
    /// Cycle 686 (sub-cycle 8 of theme-submenu design): a profile-
    /// choice leaf row used inside a `Submenu { label: "Profile",
    /// … }`. Clicking dispatches
    /// `ContextMenuClick::SetProfile(profile)` which switches the
    /// active profile (`App::config_path` + reload_config).
    // Cycle 720 (2026-05-23): removed stale `#[allow(dead_code)]`.
    // The flyout-side click dispatch landed at cycle 687/688.
    ProfileChoice {
        label: String,
        profile: String,
    },
    /// Cycle 805: a shell-choice leaf in the new-tab `▾` dropdown. Clicking
    /// dispatches `ContextMenuClick::NewTabWithArgv(argv)` to open a tab
    /// running that shell.
    NewTabShell {
        label: String,
        argv: Vec<String>,
    },
    /// Cycle 941 (Terminator parity, terminal_popup_menu.py "Open link" /
    /// "Copy address"): URL-aware leading rows, present only when the
    /// right-click landed on a detected hyperlink. The URL is captured at
    /// menu-open time so a subsequent output scroll can't retarget the click.
    /// Clicking dispatches `ContextMenuClick::Url { url, copy }`.
    UrlItem {
        label: &'static str,
        url: String,
        copy: bool,
    },
    /// Dropdown-parity cycle: a static, non-dispatchable information line
    /// (the About panel's version/update rows). Rendered like a disabled row
    /// (dimmed), survives `filter_disabled`, never highlighted, claims no
    /// mnemonic, and maps to no click.
    Info {
        label: String,
    },
}

/// UI-side context-menu state (Terminator / GNOME / iTerm2 parity).
/// Anchor is the post-clamp panel top-left; rows mirror the renderer's
/// `ContextMenu` slice but carry the live `Action` for dispatch.
/// Cycle 369 (Terminator parity): title-edit overlay state.
#[derive(Debug, Clone)]
pub enum TitleEditScope {
    /// Edit the OS window title (winit Window::set_title).
    Window,
    /// Edit the active tab's title_override (overrides what the
    /// tab-bar shows independent of any OSC 1/2 from a pane).
    Tab,
    /// Edit the focused pane's title (used for the future per-pane
    /// titlebar render Bucket-D + as the OSC-1 equivalent).
    Pane,
    /// Cycle 407 (Terminator parity, titlebar Bucket-D sub-cycle 8):
    /// edit the focused pane's broadcast-group name. Writes to
    /// pane.group_name. Empty input clears the group.
    Group,
}

/// Cycle 680 (sub-cycle 4 of [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](
/// ../../../docs/TERMINATOR-NAMED-GROUPS-DESIGN.md)):
/// when a `Group` edit fires, this carries which set of panes the
/// typed name applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupBulkScope {
    /// Default: just the focused pane (existing cycle-407
    /// `EditPaneGroup` / cycle-642 `CreateGroup` behavior).
    #[default]
    Single,
    /// Every pane in the focused tab gets the typed group name.
    Tab,
    /// Every pane in every tab.
    Window,
}

#[derive(Debug, Clone)]
pub struct TitleEditState {
    pub scope: TitleEditScope,
    /// Current text the user has typed. Pre-filled with the existing
    /// title so the user can edit in place vs starting blank.
    pub input: String,
    /// Cycle 680: when `scope == Group`, which panes Apply writes
    /// to. Single = focused only (existing behavior); Tab/Window
    /// = bulk-assign via `Action::GroupTab`/`GroupWindow`.
    pub bulk: GroupBulkScope,
}

/// Cycle 648 (sub-cycle 2 of [`TERMINATOR-CONFIRM-DIALOG-DESIGN.md`](
/// ../../../docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md)):
/// the action a confirmed modal will dispatch when the user accepts.
///
/// First user is the cycle-637 `ask_before_closing` flow
/// (`Action::CloseWindow` / `CloseTab` / `ClosePane`). Future cycles
/// add `KillProcess`, `DiscardLayout`, `ResetConfig` etc. — the
/// enum is intentionally extensible.
#[allow(clippy::enum_variant_names)] // close-family prefix is intentional
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    /// Close the entire window (every tab + every pane).
    CloseWindow,
    /// Close the focused tab (every pane in the tab).
    CloseTab,
    /// Close the focused pane.
    ClosePane,
}

/// Cycle 648: which buttons a confirm modal shows. v1 is just
/// the two-button [Cancel] / [Confirm] shape; future cycles can
/// add a third "Apply to all" or similar without rippling.
#[derive(Debug, Clone)]
pub enum ConfirmButton {
    /// Dismiss the modal without action. Always the safe default.
    Cancel,
    /// Dispatch the dialog's `on_confirm` action. `destructive: true`
    /// renders the button with the accent-red color (Close/Delete);
    /// `false` uses the standard accent (OK/Apply).
    Confirm { label: String, destructive: bool },
}

/// Cycle 648: live state for an open confirm dialog. `focus_idx`
/// points into `buttons` and the renderer / keyboard nav follow it.
#[derive(Debug, Clone)]
pub struct ConfirmDialogState {
    pub prompt: String,
    pub buttons: Vec<ConfirmButton>,
    pub focus_idx: usize,
    pub on_confirm: ConfirmAction,
}

pub(crate) struct ContextMenuState {
    anchor: (f32, f32),
    items: Vec<ContextMenuItem>,
    /// Index of the currently highlighted item — always points at an
    /// enabled `Item`, never a `Separator` or disabled row. Updated by
    /// keyboard nav (`↑↓`) and mouse hover.
    highlight: usize,
    /// Cycle 687 (sub-cycle 3 of [`TERMINATOR-THEME-SUBMENU-DESIGN.md`](
    /// ../../../docs/TERMINATOR-THEME-SUBMENU-DESIGN.md)):
    /// drill-in stack. When the user clicks a `Submenu` row, the
    /// parent's items are pushed here and replaced by the submenu's
    /// items. Esc / "Back" pops back to the parent.
    ///
    /// v1 is a single-level drill-in (matches the design's
    /// "no nested-nested submenus in v1" carveout). The Vec
    /// shape is forward-compatible for arbitrary depth.
    drill_stack: Vec<Vec<ContextMenuItem>>,
    /// Cycle 714 (Terminator menu UX, C5): scroll offset for long
    /// submenus. The Theme submenu has ~512 entries; pre-cycle-714
    /// the panel grew off-screen with no scroll handling. Now
    /// `panel_h` is clamped to fit the surface, the visible window
    /// is `[scroll_offset, scroll_offset + max_visible_rows)`, and
    /// wheel / `↑↓` past the last visible row advances `scroll_offset`.
    /// Reset to 0 on drill-in / drill-pop (each level has its own
    /// view).
    scroll_offset: usize,
    /// Parallel stack to `drill_stack`: the scroll_offset to restore
    /// when popping back to each level. Same length as `drill_stack`
    /// at all times.
    scroll_stack: Vec<usize>,
    /// Cycle 715 (Terminator menu UX, C6): typeahead buffer. As the
    /// user types A-Z chars, we accumulate them here and best-match
    /// against item labels (case-insensitive prefix). A single char
    /// also resolves to a mnemonic (first matchable char of any
    /// row), so common items like Copy = 'C' / Theme = 'T' are
    /// one-keystroke; multi-char "th" → Theme by prefix.
    ///
    /// Cleared after 750ms of inactivity (`typeahead_until`) so a
    /// pause restarts the buffer instead of accumulating forever.
    typeahead_buf: String,
    /// Cycle 715. Deadline after which `typeahead_buf` is cleared
    /// on the next key. `None` when the buffer is empty.
    typeahead_until: Option<std::time::Instant>,
}

/// Pure: which segment-index a tab-bar cursor x-coordinate falls in,
/// given `n` segments tiling a strip of width `strip_w`. Used by the
/// cycle-249 drag-to-reorder handler — the user grabs a tab, drags,
/// and the bar reorders to keep the dragged segment under the cursor.
/// Clamped to `[0, n-1]` so a cursor that overshoots either edge of
/// the strip still produces a valid target. Returns 0 for an empty
/// or zero-width bar (the no-op case).
fn tab_drag_target_index(cursor_x: f32, n: usize, strip_w: f32) -> usize {
    if n == 0 || strip_w <= 0.0 {
        return 0;
    }
    let seg_w = strip_w / n as f32;
    let raw = (cursor_x / seg_w).floor() as isize;
    raw.clamp(0, n as isize - 1) as usize
}

/// v2.19.0 (tear-off UX): Euclidean distance from a point to the nearest
/// edge of a rect — `0.0` when the point is inside. The tear decision is
/// "distance from the tab band ≥ threshold", which gives UNIFORM hysteresis
/// in every direction away from the band (the Chromium model): drag along
/// the strip = reorder, drag perpendicular past the slop = tear. Because a
/// `Top` band's upper edge coincides with the window's top edge, "above the
/// window" falls out of the same math — no separate outside-the-window
/// clause needed.
fn dist_to_rect(cx: f32, cy: f32, rect: (f32, f32, f32, f32)) -> f32 {
    let (rx, ry, rw, rh) = rect;
    let dx = (rx - cx).max(cx - (rx + rw)).max(0.0);
    let dy = (ry - cy).max(cy - (ry + rh)).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

/// v2.19.0 (tear-off UX): has the cursor moved far enough from the tab
/// band to tear the dragged tab off into its own window? Pure so the
/// per-orientation cases are unit-testable without a window.
fn tear_threshold_crossed(cx: f32, cy: f32, band: (f32, f32, f32, f32), threshold: f32) -> bool {
    threshold > 0.0 && dist_to_rect(cx, cy, band) >= threshold
}

/// v2.19.0 (tear-off UX, re-dock): insertion index for a tab dropped at
/// `cursor` (main-axis coordinate) given the existing segments' main-axis
/// midpoints, in order. First segment whose midpoint exceeds the cursor
/// wins; past every midpoint appends. Distinct from `tab_drag_target_index`
/// (which picks the segment UNDER the cursor for reorder): docking inserts
/// BETWEEN segments, so a strip of n tabs has n+1 valid slots.
fn dock_insertion_index(seg_mids: &[f32], cursor: f32) -> usize {
    seg_mids
        .iter()
        .position(|&m| cursor < m)
        .unwrap_or(seg_mids.len())
}

/// Width of the strip that horizontal tab segments tile across, given the
/// surface width and the trailing new-tab button geometry.
///
/// Shared by `tab_bar()` (segment layout) and the cycle-249 drag-to-reorder
/// handler so the drag target can't drift from the rendered segments. Cycle 821
/// (audit): the drag had subtracted only `plus_w`, ignoring the cycle-805 `▾`
/// arrow, so the strip was one button too wide and the reorder target lagged the
/// cursor near the right edge. `arrow_w` is `0.0` when the dropdown is absent
/// (vertical bars). Floored at `plus_w` so a very narrow bar still reserves room
/// for the `+` button.
fn tab_segment_strip_width(surface_w: f32, plus_w: f32, arrow_w: f32) -> f32 {
    (surface_w - plus_w - arrow_w).max(plus_w)
}

/// Cycle 917 (#4, user-requested): should the new-tab `▾` shell-dropdown arrow
/// be shown? Hidden when there's only one shell to choose — e.g. a stock Ubuntu
/// with just `bash` — so the arrow never opens a pointless one-item menu. On
/// Windows there are always multiple launch targets (cmd / pwsh / WSL distros)
/// and counting them would mean spawning `wsl.exe` (a bounded but ~2s call), so
/// the arrow always shows there. The Unix count is a cheap PATH probe, cached
/// process-wide since the installed shells don't change during a session.
fn new_tab_dropdown_visible() -> bool {
    // Dropdown-parity cycle: ALWAYS visible, superseding the cycle-917
    // single-shell gating — the dropdown now carries Settings / Command
    // palette / About rows (Windows Terminal's bottom section), so it is
    // never a pointless one-item menu even on a bash-only Ubuntu.
    true
}

/// Cycle 708 (Terminator parity, `layoutlauncher.py`): rank saved
/// layouts against the user-typed query. Empty query returns
/// every layout in original (alphabetical) order; non-empty query
/// keeps only entries whose lower-cased name contains every
/// lower-cased query token. Same shape as
/// `kettle_config::palette::rank` but layouts have only a name
/// field (no description), so the inner predicate is simpler.
/// Pure — separated from `layout_picker_key` so a drift guard
/// can exercise it without touching App state.
/// Cycle 715 (Terminator menu UX, C6). Compute mnemonics for the
/// context-menu rows: for each row, returns `Some((byte_index,
/// char))` where `char` is the first lowercase A-Z letter in the
/// label that hasn't already been claimed by an earlier row, or
/// `None` for rows without any A-Z (separators, choice rows with
/// no label letters, etc.).
///
/// First-letter is the canonical priority (matches GTK / Win32);
/// fall through to subsequent letters only if the first letter is
/// already taken by an earlier row. Pure so the collision rules
/// are unit-tested without spinning up App.
/// v2.20.0 (`vim-menu-nav`): letters the menu's vim navigation layer consumes
/// (`g`/`G` first/last, `h` back, `j`/`k` move, `l` activate). While the
/// setting is on, `assign_mnemonics` must not hand any of these to a row —
/// the nav layer intercepts them BEFORE mnemonic dispatch, so a row keyed on
/// one would silently lose its hotkey.
const VIM_NAV_RESERVED: &[char] = &['g', 'h', 'j', 'k', 'l'];

fn assign_mnemonics(items: &[ContextMenuItem], reserved: &[char]) -> Vec<Option<(usize, char)>> {
    let labels: Vec<&str> = items
        .iter()
        .map(|it| match it {
            ContextMenuItem::Item { label, .. } => *label,
            ContextMenuItem::DynamicItem { label, .. } => label.as_str(),
            ContextMenuItem::LuaItem { label, .. } => label.as_str(),
            ContextMenuItem::ConfigItem { label, .. } => label.as_str(),
            ContextMenuItem::Submenu { label, .. } => label.as_str(),
            ContextMenuItem::ThemeChoice { label, .. } => label.as_str(),
            ContextMenuItem::ProfileChoice { label, .. } => label.as_str(),
            ContextMenuItem::NewTabShell { label, .. } => label.as_str(),
            ContextMenuItem::UrlItem { label, .. } => *label,
            // Info rows are non-dispatchable — no mnemonic to claim.
            ContextMenuItem::Info { .. } => "",
            ContextMenuItem::Separator => "",
        })
        .collect();
    let mut claimed: std::collections::HashSet<char> = std::collections::HashSet::new();
    let mut out: Vec<Option<(usize, char)>> = vec![None; labels.len()];
    // Cycle 942 (audit): two rounds — the stable core rows claim their
    // letters FIRST, the context-dependent UrlItem rows (only present when
    // the right-click landed on a link) claim from what's left. Otherwise
    // "Open Link" / "Copy Link Address" leading the menu stole 'c'/'o',
    // silently remapping muscle-memory mnemonics ('p' fired Copy instead of
    // Paste whenever the menu happened to open over a URL).
    let round = |items: &[ContextMenuItem], idx: usize| -> usize {
        usize::from(matches!(items[idx], ContextMenuItem::UrlItem { .. }))
    };
    for pass in 0..2 {
        for (i, label) in labels.iter().enumerate() {
            if round(items, i) != pass {
                continue;
            }
            let mut chosen: Option<(usize, char)> = None;
            for (bi, c) in label.char_indices() {
                if !c.is_ascii_alphabetic() {
                    continue;
                }
                let low = c.to_ascii_lowercase();
                // v2.20.0: letters owned by vim-menu-nav are never assignable.
                if reserved.contains(&low) {
                    continue;
                }
                if !claimed.contains(&low) {
                    claimed.insert(low);
                    chosen = Some((bi, low));
                    break;
                }
            }
            out[i] = chosen;
        }
    }
    out
}

/// Cycle 715. Match the user's typeahead buffer to a row by
/// case-insensitive prefix on the label. Returns the first
/// dispatchable row whose label (lowercased) starts with `buf`
/// (also lowercased). Separators/empty labels are skipped.
fn typeahead_match(items: &[ContextMenuItem], buf: &str) -> Option<usize> {
    if buf.is_empty() {
        return None;
    }
    let needle = buf.to_ascii_lowercase();
    items.iter().position(|it| match it {
        ContextMenuItem::Item {
            label,
            enabled: true,
            ..
        } => label.to_ascii_lowercase().starts_with(&needle),
        ContextMenuItem::DynamicItem {
            label,
            enabled: true,
            ..
        } => label.to_ascii_lowercase().starts_with(&needle),
        ContextMenuItem::LuaItem { label, .. }
        | ContextMenuItem::ConfigItem { label, .. }
        | ContextMenuItem::Submenu { label, .. }
        | ContextMenuItem::ThemeChoice { label, .. }
        | ContextMenuItem::ProfileChoice { label, .. }
        | ContextMenuItem::NewTabShell { label, .. } => {
            label.to_ascii_lowercase().starts_with(&needle)
        }
        ContextMenuItem::UrlItem { label, .. } => label.to_ascii_lowercase().starts_with(&needle),
        // Info rows aren't dispatchable, so typeahead skips them.
        _ => false,
    })
}

/// Cycle 714 (Terminator menu UX, C5). How many rows starting at
/// `start` fit within `panel_h` pixels. Separators take `sep_h`,
/// every other row takes `row_h`. Used by `step_context_menu_highlight`
/// and `scroll_context_menu` to keep `scroll_offset` honest when the
/// panel is height-clamped by `context_menu_geometry`. Pure so the
/// arithmetic is drift-guarded without standing up the App.
fn count_rows_fitting(
    items: &[ContextMenuItem],
    start: usize,
    panel_h: f32,
    row_h: f32,
    sep_h: f32,
) -> usize {
    let mut used = 0.0_f32;
    let mut count = 0;
    for it in items.iter().skip(start) {
        let h = if matches!(it, ContextMenuItem::Separator) {
            sep_h
        } else {
            row_h
        };
        if used + h > panel_h {
            break;
        }
        used += h;
        count += 1;
    }
    count
}

/// Cycle 713 (Terminator menu UX, C4). Drop disabled `Item`s from
/// the context-menu and collapse the separators that would orphan
/// around them. Pre-cycle-713 disabled rows rendered greyed-out —
/// after this filter they're hidden entirely, matching Terminator /
/// GNOME Terminal: only-show-what-you-can-click.
///
/// Three passes:
///   1. drop any `Item { enabled: false }`. Other variants (LuaItem,
///      ConfigItem, Submenu, Theme/ProfileChoice, Separator) stay.
///   2. collapse runs of `Separator` to a single one.
///   3. trim leading + trailing separators (orphaned by step 1).
///
/// Pure so the contract is unit-tested without spinning up App.
fn filter_disabled(items: Vec<ContextMenuItem>) -> Vec<ContextMenuItem> {
    // Step 1: drop disabled Items.
    let kept: Vec<ContextMenuItem> = items
        .into_iter()
        .filter(|it| {
            !matches!(
                it,
                ContextMenuItem::Item { enabled: false, .. }
                    | ContextMenuItem::DynamicItem { enabled: false, .. }
            )
        })
        .collect();
    // Step 2: collapse separator runs. Walk linearly; only push a
    // Separator when the previous pushed item wasn't already one.
    let mut collapsed: Vec<ContextMenuItem> = Vec::with_capacity(kept.len());
    let mut last_was_sep = true; // pretend the "before-start" was a sep so leading sep gets dropped
    for it in kept {
        let is_sep = matches!(it, ContextMenuItem::Separator);
        if is_sep && last_was_sep {
            continue;
        }
        last_was_sep = is_sep;
        collapsed.push(it);
    }
    // Step 3: trim a trailing separator (orphan).
    if let Some(ContextMenuItem::Separator) = collapsed.last() {
        collapsed.pop();
    }
    collapsed
}

/// Cycle 712 (Terminator menu UX, hover-to-highlight). Walk the
/// vertical pixel layout of a context-menu's rows and return the
/// row index containing `cursor_y`, or `None` if the cursor landed
/// on a separator (visual gap) or beyond the last row. Pure so the
/// arithmetic + the separator-skip contract is unit-tested without
/// standing up an App + a winit window. `kinds[i] = true` flags row
/// `i` as a separator (uses `sep_h` instead of `row_h`).
pub(crate) fn find_menu_row_y(
    cursor_y: f32,
    anchor_y: f32,
    row_h: f32,
    sep_h: f32,
    kinds: &[bool],
) -> Option<usize> {
    let mut row_y = anchor_y;
    for (idx, &is_sep) in kinds.iter().enumerate() {
        let h = if is_sep { sep_h } else { row_h };
        if cursor_y >= row_y && cursor_y < row_y + h {
            return if is_sep { None } else { Some(idx) };
        }
        row_y += h;
    }
    None
}

pub(crate) fn rank_layouts(q: &str, layouts: &[String]) -> Vec<usize> {
    let q = q.trim().to_ascii_lowercase();
    if q.is_empty() {
        return (0..layouts.len()).collect();
    }
    let tokens: Vec<&str> = q.split_whitespace().collect();
    layouts
        .iter()
        .enumerate()
        .filter(|(_, name)| {
            let lower = name.to_ascii_lowercase();
            tokens.iter().all(|t| lower.contains(t))
        })
        .map(|(i, _)| i)
        .collect()
}

/// v2.20.0 (`vim-menu-nav`): the `Ctrl+d`/`Ctrl+u` half-page target. Moves
/// `current` by `rows` items in `dir` (no wrap — vim half-page semantics
/// clamp at the ends), then snaps to the nearest dispatchable row in the
/// direction of travel (falling back to the other direction so a
/// trailing separator can't strand the highlight). Pure so the
/// clamp+snap math is unit-testable without App state.
fn half_page_menu_target(
    items: &[ContextMenuItem],
    current: usize,
    rows: usize,
    dir: isize,
) -> usize {
    if items.is_empty() {
        return current;
    }
    let n = items.len() as isize;
    let step = rows.max(1) as isize;
    let raw = (current as isize + dir * step).clamp(0, n - 1) as usize;
    let scan_down = |from: usize| (from..items.len()).find(|&i| item_is_dispatchable(&items[i]));
    let scan_up = |from: usize| (0..=from).rev().find(|&i| item_is_dispatchable(&items[i]));
    let snapped = if dir > 0 {
        scan_down(raw).or_else(|| scan_up(raw))
    } else {
        scan_up(raw).or_else(|| scan_down(raw))
    };
    snapped.unwrap_or(current)
}

/// Pure: walk the menu item list to find the next enabled, non-
/// separator row index, given a `delta` (±1) and a wrap-around at the
/// list ends. Used by both `↑` and `↓` keyboard nav. Returns `current`
/// unchanged if no enabled rows exist at all (defensive — the menu
/// shouldn't have been opened with zero actionable rows).
fn item_is_dispatchable(item: &ContextMenuItem) -> bool {
    matches!(
        item,
        ContextMenuItem::Item { enabled: true, .. }
            | ContextMenuItem::DynamicItem { enabled: true, .. }
            | ContextMenuItem::LuaItem { .. }
            | ContextMenuItem::ConfigItem { .. }
            // Cycle 684: Submenu rows are dispatchable for keyboard
            // nav (↑↓ lands on them); clicks/Enter on a Submenu row
            // will open the flyout once sub-cycle 3 lands. For now
            // the click no-ops with an info log.
            | ContextMenuItem::Submenu { .. }
            // Cycle 890 (audit): theme / profile choice leaves are the
            // *contents* of a drilled-in Theme ▸ / Profile ▸ submenu.
            // They were absent here, so once you drilled in via the
            // keyboard, ↑/↓ could not land on any row and Enter could
            // not pick a theme — a keyboard dead-end. Mouse clicks
            // worked, so the rows were reachable by mouse only.
            | ContextMenuItem::ThemeChoice { .. }
            | ContextMenuItem::ProfileChoice { .. }
            // Cycle 805: new-tab ▾ shell choices are always clickable + keyboard-
            // navigable.
            | ContextMenuItem::NewTabShell { .. }
            // Cycle 941: the URL-aware "Open Link" / "Copy Link Address" rows.
            | ContextMenuItem::UrlItem { .. } // ContextMenuItem::Info is deliberately absent: a static info line
                                              // (About panel) is not highlightable or clickable.
    )
}

/// Map a context-menu row at position `idx` (within the current level)
/// to the click it dispatches. Shared by the mouse hit-test
/// ([`App::context_menu_click_action`]) and the keyboard Enter / Space and
/// mnemonic paths so every dispatchable row type (submenu, Lua,
/// config-command, theme/profile choice, new-tab shell) is reachable
/// identically from mouse and keyboard.
///
/// Cycle 890 (audit): the keyboard Enter / Space and mnemonic handlers
/// previously inlined a *partial* match that only recognised `Item`
/// (Enter / Space) or `Item`/`Submenu`/theme/profile (mnemonic), so
/// Lua items, config commands and the new-tab ▾ dropdown were keyboard
/// dead-ends. Routing all three input paths through this one mapper
/// closes that gap and prevents the matches from drifting apart again.
fn item_to_click(item: &ContextMenuItem, idx: usize) -> Option<ContextMenuClick> {
    match item {
        ContextMenuItem::Item {
            action,
            enabled: true,
            ..
        }
        | ContextMenuItem::DynamicItem {
            action,
            enabled: true,
            ..
        } => Some(ContextMenuClick::Action(action.clone())),
        ContextMenuItem::LuaItem { lua_idx, .. } => Some(ContextMenuClick::LuaMenuItem(*lua_idx)),
        ContextMenuItem::ConfigItem { command, .. } => {
            Some(ContextMenuClick::ConfigCommand(command.clone()))
        }
        ContextMenuItem::Submenu { .. } => Some(ContextMenuClick::DrillIntoSubmenu(idx)),
        ContextMenuItem::ThemeChoice { theme, .. } => {
            Some(ContextMenuClick::SetTheme(theme.clone()))
        }
        ContextMenuItem::ProfileChoice { profile, .. } => {
            Some(ContextMenuClick::SetProfile(profile.clone()))
        }
        ContextMenuItem::NewTabShell { argv, .. } => {
            Some(ContextMenuClick::NewTabWithArgv(argv.clone()))
        }
        ContextMenuItem::UrlItem { url, copy, .. } => Some(ContextMenuClick::Url {
            url: url.clone(),
            copy: *copy,
        }),
        ContextMenuItem::Item { enabled: false, .. }
        | ContextMenuItem::DynamicItem { enabled: false, .. }
        | ContextMenuItem::Info { .. }
        | ContextMenuItem::Separator => None,
    }
}

fn next_context_menu_highlight(items: &[ContextMenuItem], current: usize, delta: isize) -> usize {
    if items.is_empty() {
        return current;
    }
    let len = items.len() as isize;
    let step = if delta >= 0 { 1 } else { -1 };
    let mut i = current as isize;
    for _ in 0..len {
        i = (i + step).rem_euclid(len);
        if item_is_dispatchable(&items[i as usize]) {
            return i as usize;
        }
    }
    current
}

/// Pure: clamp the requested panel anchor so the panel of size
/// `(panel_w, panel_h)` stays fully inside a surface of size
/// `(surface_w, surface_h)`. A 4-px screen-edge margin so the panel
/// reads as floating rather than glued to the edge. Right-click near
/// the bottom-right corner therefore flips the menu up-and-left
/// instead of rendering off-screen.
fn clamp_context_menu_anchor(
    (req_x, req_y): (f32, f32),
    (panel_w, panel_h): (f32, f32),
    (surface_w, surface_h): (f32, f32),
) -> (f32, f32) {
    let margin = 4.0_f32;
    let max_x = (surface_w - panel_w - margin).max(margin);
    let max_y = (surface_h - panel_h - margin).max(margin);
    let x = req_x.clamp(margin, max_x);
    let y = req_y.clamp(margin, max_y);
    (x, y)
}

/// `(active-tab-index, focused-leaf-id)` — the value `App::focus_key` returns.
pub(crate) type FocusKey = (usize, Option<u64>);
/// Cycle 803 cache key for the search re-scan: `(query, focus, tab last-output)`.
pub(crate) type SearchScanKey = (String, FocusKey, Option<std::time::Instant>);
/// Cycle 803 cache key for the viewport link re-scan: `(focus, tab last-output,
/// scroll display_offset)`.
/// v2.20.0 (review fix): the middle component is the focused pane's
/// `output_generation` — the old key used the tab's `last_output_at`, which
/// the activity latch only updates for BACKGROUND tabs, so active-tab output
/// never invalidated the link scan at all (links went stale until a scroll
/// or focus change) and the P6 debounce was unreachable.
pub(crate) type LinksScanKey = (FocusKey, Option<u64>, Option<usize>);

/// v2.20.0 P6 (perf): minimum interval between viewport link re-scans when
/// only the OUTPUT timestamp changed (streaming). Focus/scroll changes bypass
/// it. 150ms keeps worst-case link-detection latency imperceptible while
/// cutting the per-frame regex pass under flood by ~9×.
pub(crate) const LINKS_SCAN_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);

/// v2.20.0 (Ghostty `resize-overlay` parity): how long the transient
/// `cols×rows` chip stays up after the last resize event (Ghostty's
/// `resize-overlay-duration` default).
pub(crate) const RESIZE_OVERLAY_DURATION: std::time::Duration =
    std::time::Duration::from_millis(750);

pub struct App {
    cfg: Config,
    /// C1 (multi-window foundation): all per-window state, keyed by the
    /// window's stable sequence number (`WindowState::seq`, 1-based, never
    /// reused). BTreeMap so iteration order is deterministic (window 1, 2,
    /// ...). The `ApplicationHandler` entry points remove the addressed entry,
    /// run the inner handler with disjoint `&mut self` (globals) +
    /// `&mut WindowState` borrows, then reinsert — see window_state.rs for the
    /// dispatch contract.
    windows: std::collections::BTreeMap<u64, WindowState>,
    /// Seq of the window that has (or most recently had) OS focus. Routes
    /// window-less events (UserEvent wakeups, remote/ctl/Lua commands).
    focused_seq: u64,
    /// Next `WindowState::seq` to assign (consumed by `open_window`).
    next_window_seq: u64,
    /// C4: the shared GPU context, cached when window 1's renderer comes up
    /// in `resumed`; `open_window` reuses it for the synchronous (no
    /// adapter/device request) renderer init of windows 2..N.
    gpu: Option<kettle_render::GpuContext>,
    /// v2.23.0: detected GPUs as `(token, label)` pairs for the Settings →
    /// Graphics device picker. Enumerated ONCE when the settings overlay first
    /// opens (a wgpu instance + adapter walk is ~tens of ms — too heavy per
    /// frame) and cached for the session; `categories()` reads it. Empty until
    /// the overlay is first opened (the picker then shows just "Automatic").
    gpu_choices: Vec<(String, String)>,
    /// C4: set by any "this window is done" path (last tab closed, X button,
    /// reap drained the panes) while its WindowState is checked out of the
    /// map. The dispatch wrappers consume it via `finish_window_dispatch`:
    /// the window is dropped instead of reinserted, and the loop exits once
    /// no windows remain.
    pending_window_close: bool,
    /// C4: Quit semantics — drop every window and exit, regardless of how
    /// many are open.
    quit_requested: bool,
    /// Cycle 875: developer session recorder (asciicast trace). `Some` only in
    /// a `dev-record` feature build when `--record PATH` / `KETTLE_RECORD` was
    /// given; compiled out entirely of shipped builds.
    #[cfg(feature = "dev-record")]
    recorder: Option<crate::dev_record::Recorder>,
    proxy: EventLoopProxy<UserEvent>,
    /// v2.20.0 P4 (perf): wakeup-dedup latch shared by every pane's `Waker`.
    /// Under output flood the PTY readers fire once per 64KiB read — dozens
    /// of queued `UserEvent::Wakeup`s per paint window, each one fanning out
    /// over every window just to discover the generations already matched.
    /// The waker enqueues only when it flips this false→true; the Wakeup arm
    /// re-opens the latch BEFORE reading generations, so output that lands
    /// after the clear enqueues a fresh event (no lost wakeups).
    wake_pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Cycle 928 (agent-first A2): the in-process control server, present when
    /// `agent-server` is enabled (config or `--agent-server`). `None` keeps the
    /// zero-cost default path. Started in `resumed`, dropped on exit (which
    /// unregisters the discovery entry).
    ctl: Option<crate::ctl_server::CtlServer>,
    /// Cycle 929 (agent-first A2): pending `run_command` correlations keyed by
    /// pane id. A request writes `cmd\n`, records the start line + deadline
    /// here, and the next OSC-133 `CommandFinished` for that pane resolves it.
    pending_runs: std::collections::HashMap<u64, PendingRun>,
    clipboard: Option<arboard::Clipboard>,
    /// Cycle 290 triggers: compiled regex set built from `cfg.triggers` at App
    /// construction (and after live reload). Invalid patterns are logged via
    /// `log::warn!` and dropped.
    compiled_triggers: Vec<(regex::Regex, kettle_config::TriggerAction)>,
    /// Cycle 290: per-trigger last-fire timestamps. Dedupes a fast-arriving
    /// match flood; cleared when any trigger fires past a 2-second window.
    last_trigger_fire: std::time::Instant,
    /// Cycle 656/851: shared snapshot scanner — refreshes the OS process list
    /// and parent→children index once per poll tick, then answers every pane
    /// from it. Used by the per-pane remote-session detector.
    remote_scanner: kettle_remote::RemoteScanner,
    /// Cycle 656: throttle the remote-detect poll to ~5 Hz.
    last_remote_poll: std::time::Instant,
    /// Cycle 666: the most-recent auto-theme "schedule decision" (true=dark)
    /// we've applied, so a boundary-crossing fires the swap exactly once.
    last_schedule_decision: Option<bool>,
    /// Explicit `--config` file (persists for live reload).
    config_path: Option<std::path::PathBuf>,
    /// First-tab CLI overrides (`-e cmd`, `-d dir`); consumed once.
    startup: crate::Options,
    _watcher: Option<notify::RecommendedWatcher>,
    /// Cycle 302: drop guard for the remote-control watcher.
    _remote_watcher: Option<notify::RecommendedWatcher>,
    /// Cycle 325 Lua scripting: bytes the user's `--lua-script` queued via
    /// `kettle.send_text(s)` before the first pane existed.
    pending_lua_send: Vec<u8>,
    /// Cycle 326 Lua scripting: Actions queued via `kettle.exec_action(name)`,
    /// drained after the first pane spawns.
    pending_lua_actions: Vec<kettle_config::Action>,
    /// Cycle 366: the live LuaEngine persisted across the App's lifetime so
    /// `kettle.on(event, callback)` registrations stay in scope. All event
    /// hooks share `drain_lua_hook_commands` for the LuaCommand dispatch.
    lua_engine: Option<crate::LuaEngine>,
    /// Cycle 366: set after LuaEvent::Startup fired once (Wayland can re-emit
    /// `resumed`).
    lua_startup_fired: bool,
    /// Cycle 794: `Some((tag, url))` while the "a newer kettle release is
    /// available" banner is showing. Esc dismisses; Enter opens the URL.
    update_available: Option<(String, String)>,
    /// Dropdown-parity cycle: the full version string the About panel shows —
    /// the bin crate passes its `KETTLE_VERSION` (version + git hash, exactly
    /// what `--version` prints); falls back to the bare crate version.
    version_line: String,
    /// v2.19.0 (tear-off UX): `Some` while a torn-off window is riding an
    /// OS-native move loop (`drag_window()`) or the manual-follow fallback.
    /// Drives the Phase-2 re-dock: hit-testing sibling tab bands on `Moved`,
    /// the insertion preview, and the drop-merge. App-level (not per-window)
    /// because exactly one tear-off drag can be in flight per pointer.
    torn_drag: Option<TornDrag>,
}

/// v2.19.0 (tear-off UX): tracking for the one in-flight torn-window drag.
struct TornDrag {
    /// Seq of the torn-off window being dragged.
    seq: u64,
    /// Seq of the mouse-capture holder: the tear's SOURCE window (it keeps
    /// streaming CursorMoved while the button is held), or `seq` itself
    /// for a lone-tab whole-window drag. Manual-follow only listens to the
    /// carrier — without this gate, stale tracking would hijack EVERY
    /// window's cursor stream (cycle-943 review).
    carrier: u64,
    /// Handoff instant. The X11/macOS pointer-event drop heuristics are
    /// suppressed for a short window after it (a stray client motion can
    /// race the WM actually taking the move grab), and the native→manual
    /// demotion fires off it when the WM never takes the drag at all.
    started: std::time::Instant,
    /// Cursor offset from the torn window's FRAME top-left in physical px,
    /// chosen at tear time so the pointer holds the tab. `Moved(pos) + grab`
    /// approximates the live screen cursor during the native move loop
    /// (winit's `Moved` reports the frame position on Windows — verified in
    /// the vendored 0.30.13 WM_WINDOWPOSCHANGED handler).
    grab: (f64, f64),
    /// Latched dock target: (sibling window seq, insertion index). Updated
    /// by every hit-test; the drop commits whatever is latched.
    dock: Option<(u64, usize)>,
    /// `drag_window()` succeeded — the OS carries the window. The drop then
    /// arrives as winit's synthesized post-WM_EXITSIZEMOVE left-release
    /// (Windows) or as the first client pointer event after the WM's pointer
    /// grab ends (X11/macOS — the client receives NO pointer events while
    /// the WM moves the window). `false` = manual-follow: the SOURCE window
    /// still holds mouse capture, repositions the torn window from its own
    /// CursorMoved stream, and its left-release is the drop.
    native: bool,
    /// At least one `Moved` arrived since the handoff. Gates the
    /// pointer-event drop heuristic against the PostMessage race on
    /// Windows (a stray client mouse-move can slip in between
    /// `drag_window()`'s posted WM_NCLBUTTONDOWN and the modal loop
    /// actually starting) and against an X11 tear the user never moved.
    saw_move: bool,
    /// Last signal (tear or `Moved`) — the stale-tracking failsafe: a
    /// torn drag with no movement for 30s is abandoned by `about_to_wait`
    /// (covers an X11 tear whose release we never observe).
    last_signal: std::time::Instant,
    /// The torn window's HWND (Windows; `None` elsewhere) — the dock-hover
    /// translucency and the z-order walk both key off it, and tracking
    /// teardown needs it to restore full opacity from any code path.
    /// Only Windows code reads it; populated unconditionally so the
    /// struct shape stays identical across platforms.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    hwnd: Option<isize>,
}

/// Cycle 371 (Terminator plugin parity, plugin sub-cycle 7): fire a
/// desktop notification with the given title + optional body. Wraps
/// `notify-rust::Notification` so the caller doesn't need to import
/// it directly. Failure modes degrade silently to log::warn — a
/// headless run (no DBUS_SESSION_BUS_ADDRESS) or a sandboxed
/// environment (snap/flatpak) where the notification daemon isn't
/// reachable doesn't crash kettle, just skips the notification.
pub(crate) fn fire_notify(title: &str, body: &str) {
    let mut n = notify_rust::Notification::new();
    n.summary(title);
    if !body.is_empty() {
        n.body(body);
    }
    n.appname("kettle");
    if let Err(e) = n.show() {
        log::warn!("kettle.notify: notification send failed: {e}");
    }
}

impl App {
    /// The map key of the WindowState whose OS window is `wid`, if any.
    /// Returns `WindowState::seq`, which is the map key by invariant.
    fn seq_of_window(&self, wid: WindowId) -> Option<u64> {
        self.windows
            .values()
            .find_map(|w| (w.window.as_ref().map(|x| x.id()) == Some(wid)).then_some(w.seq))
    }

    /// C4: the dispatch wrappers' epilogue. Reinserts the checked-out window
    /// — or, when the inner handler flagged a close (`pending_window_close`)
    /// or a quit, drops it (panes' PTYs die with their Mux) and exits the
    /// event loop once no windows remain. This is the ONLY place a window
    /// close reaches `event_loop.exit()`, so "exit only when the map is
    /// empty" holds by construction (drift-guarded in
    /// `event_loop_exit_sites_are_allowlisted`).
    fn finish_window_dispatch(&mut self, event_loop: &ActiveEventLoop, seq: u64, ws: WindowState) {
        if self.quit_requested {
            drop(ws);
            self.windows.clear();
            event_loop.exit();
            return;
        }
        if self.pending_window_close {
            self.pending_window_close = false;
            // v2.19.0 (tear-off UX, cycle-943): a dying window that is the
            // torn window or the manual-follow capture holder takes its
            // drag with it — abandon eagerly (clears the latched preview on
            // the still-mapped target and restores opacity) instead of
            // leaving stale tracking for the heuristics to trip over.
            if self
                .torn_drag
                .as_ref()
                .is_some_and(|t| t.seq == seq || t.carrier == seq)
            {
                self.abandon_torn_drag(None);
            }
            drop(ws);
            if self.windows.is_empty() {
                event_loop.exit();
            } else if self.focused_seq == seq
                && let Some(&next) = self.windows.keys().next_back()
            {
                // Focus falls to the most recently opened remaining window;
                // the OS will confirm with a Focused(true) shortly.
                self.focused_seq = next;
            }
            return;
        }
        self.windows.insert(seq, ws);
    }

    pub fn run() -> Result<()> {
        Self::run_with(crate::Options::default())
    }

    pub fn run_with(startup: crate::Options) -> Result<()> {
        let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();

        // Watch the chosen config file's directory for live reload.
        // Cycle 151: filter notify events by path so we only reload
        // when the config file itself changes. Pre-fix, any file
        // event in the config dir (session.json save, palette
        // edits, theme cache, the user's text-editor swap files,
        // …) triggered a reload. Particularly bad with cycle 109's
        // atomic session save which writes `session.json.tmp.*`
        // then `rename`s — each save fires 3+ notify events that
        // all pointlessly reloaded the config. Match on `paths`
        // containing the watched config file exactly.
        let mut watcher = None;
        if let Some(path) = startup.config.clone().or_else(Config::default_path)
            && let Some(dir) = path.parent().map(|p| p.to_path_buf())
        {
            let p = proxy.clone();
            let watched = path.clone();
            use notify::Watcher;
            if let Ok(mut w) =
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    if let Ok(ev) = res
                        && ev.paths.iter().any(|p| p == &watched)
                    {
                        let _ = p.send_event(UserEvent::ReloadConfig);
                    }
                })
            {
                let _ = std::fs::create_dir_all(&dir);
                let _ = w.watch(&dir, notify::RecursiveMode::NonRecursive);
                watcher = Some(w);
            }
        }

        // Cycle 302 remote-control watcher. Same notify pattern as the
        // config-reload watcher above. When startup.remote_file is
        // Some, watch its parent directory; on a change to the file
        // itself, send UserEvent::RemoteCommand so the main thread
        // reads + dispatches lines.
        let mut remote_watcher = None;
        if let Some(path) = startup.remote_file.clone()
            && let Some(dir) = path.parent().map(|p| p.to_path_buf())
        {
            let p = proxy.clone();
            let watched = path.clone();
            use notify::Watcher;
            if let Ok(mut w) =
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    if let Ok(ev) = res
                        && ev.paths.iter().any(|p| p == &watched)
                    {
                        let _ = p.send_event(UserEvent::RemoteCommand);
                    }
                })
            {
                let _ = std::fs::create_dir_all(&dir);
                // Cycle 306: truncate any leftover content on
                // startup so commands that arrived after a previous
                // kettle crashed mid-process don't replay as bytes
                // typed into the NEW kettle's focused pane. Subtle
                // bug surfaced by audit. Failing to truncate is
                // fine (file may not exist yet) — the watcher will
                // still fire on next write.
                let _ = std::fs::write(&path, "");
                let _ = w.watch(&dir, notify::RecursiveMode::NonRecursive);
                remote_watcher = Some(w);
            }
        }

        // Cycle 290: hoist the config load so the OutputTrigger
        // compile + the cfg field assignment can both reference it.
        // Inlining the `Config::load*` inside the struct-init lost
        // access to a local name for the triggers; the bare `cfg.…`
        // would otherwise hit the `cfg!()` macro.
        let mut initial_cfg = startup
            .config
            .as_deref()
            .map(Config::load_from)
            .unwrap_or_else(Config::load);
        // Cycle 293 peacock parity: --accent CLI flag wins over the
        // config `accent-color` key. Applied here once at startup;
        // a runtime reload (cycle 151) would reread the config but
        // we don't currently re-thread the CLI overrides, which is
        // intended — CLI flags are launch-time intent.
        if let Some(rgb) = startup.accent_override {
            initial_cfg.accent_color = Some(rgb);
        }
        // Cycle 938 (Terminator parity): launch-time window-state CLI flags
        // (`-m/-f/-H/-b/-T`) override the config for THIS launch — same
        // "CLI flags are launch-time intent" precedent as --accent above.
        if let Some(wstate) = startup.window_state_override {
            initial_cfg.window_state = wstate;
        }
        if let Some(b) = startup.borderless_override {
            initial_cfg.borderless = b;
        }
        if let Some(title) = startup.title_override.clone() {
            // A literal title (no `{title}` placeholder) renders verbatim.
            initial_cfg.window_title_format = title;
        }
        // Cycle 937 (Peacock): seed the per-window accent variation from this
        // window's working directory, so `accent-color = auto` gives a window
        // in a different project a different (but per-project stable) accent.
        initial_cfg.accent_seed = accent_seed_from_cwd(startup.cwd.as_deref());
        // Cycle 942 (audit): seed the ToggleFullscreen tracking flag from the
        // effective window-state (config `window-state = fullscreen` or `-f`).
        // It used to start `false` unconditionally, so a fullscreen launch
        // needed TWO ToggleFullscreen presses to exit (the first "entered"
        // the state kettle was already in).
        let start_fullscreen = matches!(
            initial_cfg.window_state,
            kettle_config::WindowState::Fullscreen
        );
        let initial_triggers = compile_triggers(&initial_cfg.triggers);
        // Dropdown-parity cycle: capture before `startup` moves into the App.
        let startup_version = startup
            .version
            .clone()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        // Cycle 324: Lua scripting foundation. If `--lua-script PATH`
        // was set, init a LuaEngine + run the script once. Failures
        // log::warn but don't block the launch (same shape as the
        // cycle-289 trigger compile fallthrough).
        //
        // Cycle 325: also drain pending side-effect commands queued
        // by Lua and stash them on App so the first focused pane
        // gets them written to its PTY once it's ready (the pane
        // doesn't exist yet at this point in App::new).
        let mut pending_lua_send: Vec<u8> = Vec::new();
        let mut pending_lua_actions: Vec<kettle_config::Action> = Vec::new();
        // Cycle 366: keep the LuaEngine alive on App so kettle.on(...)
        // registrations survive past App::new + can be fire_event'd
        // from emission sites. If no --lua-script was passed AND no
        // ~/.config/kettle/init.lua exists, skip engine init entirely
        // so non-Lua kettle runs stay zero-cost.
        //
        // Cycle 370 (plugin sub-cycle 11): auto-load
        // `<config-dir>/init.lua` if present. Explicit --lua-script
        // CLI flag wins (overrides auto-load). Path resolution:
        //   1. --lua-script PATH (explicit; overrides)
        //   2. <config-dir>/init.lua  (auto-loaded; default for plugins)
        // where <config-dir> is the parent of Config::default_path().
        let init_lua_path: Option<std::path::PathBuf> = startup.lua_script.clone().or_else(|| {
            kettle_config::Config::default_path()
                .and_then(|p| p.parent().map(|d| d.join("init.lua")))
                .filter(|p| p.exists())
        });
        let mut lua_engine: Option<crate::LuaEngine> = None;
        if let Some(script) = &init_lua_path {
            let safe_sandbox = matches!(initial_cfg.lua_sandbox, kettle_config::LuaSandbox::Safe);
            match crate::LuaEngine::new_with_sandbox(&initial_cfg.theme_name, safe_sandbox) {
                Ok(eng) => {
                    if let Err(e) = eng.exec_file(script) {
                        log::warn!("lua script {}: {e:#}", script.display());
                    } else {
                        log::info!("lua script {}: executed", script.display());
                    }
                    for cmd in eng.drain_commands() {
                        match cmd {
                            crate::LuaCommand::SendText(s) => {
                                pending_lua_send.extend_from_slice(s.as_bytes());
                            }
                            crate::LuaCommand::ExecAction(name) => {
                                if let Some(a) = kettle_config::Action::from_name(&name) {
                                    pending_lua_actions.push(a);
                                } else {
                                    log::warn!(
                                        "lua kettle.exec_action: unknown action name {name:?}"
                                    );
                                }
                            }
                            crate::LuaCommand::Notify { title, body } => {
                                fire_notify(&title, &body);
                            }
                            crate::LuaCommand::SetTheme(name) => {
                                // Cycle 373: in App::new, mutate
                                // initial_cfg directly because
                                // self.cfg doesn't exist yet.
                                if let Some(canonical) = kettle_config::Theme::find_name(&name) {
                                    initial_cfg.theme_name = canonical.to_string();
                                    initial_cfg.theme = kettle_config::Theme::by_name(canonical);
                                } else {
                                    log::warn!("lua kettle.set_theme: unknown theme {name:?}");
                                }
                            }
                        }
                    }
                    lua_engine = Some(eng);
                }
                Err(e) => {
                    log::warn!("lua engine init failed: {e:#}");
                }
            }
        }
        // Cycle 378 (plugin sub-cycle 3): if a LuaEngine is active,
        // the Mux must subscribe to per-PTY output bytes so the
        // App can fire LuaEvent::Output. Set before the Mux moves
        // into the struct.
        let lua_output_subscribed = lua_engine.is_some();
        // Cycle 875: also subscribe to per-pane PTY output when a dev recording
        // is requested, so the recorder can tee output into the asciicast trace.
        #[cfg(feature = "dev-record")]
        let lua_output_subscribed = lua_output_subscribed || startup.record.is_some();
        // Cycle 560 (BUG FIX): cycle 357 misread Terminator's
        // `broadcast_default` config key. The Terminator semantics
        // are: when the user ENABLES broadcast (via a chord), what
        // scope applies — `all` / `group` / `off`. The default value
        // `group` does NOT mean "broadcast is on at startup". But
        // cycle 357 mapped `!matches!(broadcast_default, Off)` to
        // `initial_broadcast = true`, so every new kettle window
        // started with broadcast ON. Users typing in one pane saw
        // their keystrokes mirrored across every other pane in the
        // tab — the bug the cycle-560 user-report flagged.
        //
        // Correct mapping: broadcast STATE always starts off.
        //
        // NOTE (cycle 562): with the cycle-360 mapping removed, the
        // `broadcast_default` config field currently has no runtime
        // effect — it parses but no consumer reads it. The field is
        // kept in `kettle_config::Config` for forward-compatibility:
        // a future cycle wiring the scope-when-enabled semantics
        // (cycle-406 named-group integration with Terminator's
        // `broadcast_default = all` route) will read it. Until then,
        // setting `broadcast-default = all` in a config has no
        // visible effect; broadcast scope defaults to the cycle-178
        // active-tab leaves.
        // C1 (multi-window foundation): per-window state lives in a
        // WindowState; the first window is seq 1. The Mux is built here
        // because its construction flags (`lua_output_subscribed`,
        // `record_lossless`) are process-global decisions.
        let mux = {
            let mut m = Mux::new();
            m.lua_output_subscribed = lua_output_subscribed;
            // Cycle 881: a dev recording needs a lossless output channel so
            // the asciicast trace can't drop chunks under a fast burst.
            #[cfg(feature = "dev-record")]
            {
                m.record_lossless = startup.record.is_some();
            }
            m
        };
        let mut windows = std::collections::BTreeMap::new();
        windows.insert(1, WindowState::new(1, start_fullscreen, mux));
        let mut app = App {
            cfg: initial_cfg,
            windows,
            focused_seq: 1,
            next_window_seq: 2,
            gpu: None,
            gpu_choices: Vec::new(),
            pending_window_close: false,
            quit_requested: false,
            #[cfg(feature = "dev-record")]
            recorder: None,
            proxy,
            wake_pending: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            // Cycle 928 (agent-first A2): server is started later in `resumed`
            // (needs the pid + a live event-loop proxy for the waker).
            ctl: None,
            pending_runs: std::collections::HashMap::new(),
            // Cycle 754: surface why the clipboard is unavailable instead of a
            // silent `None`. On headless/SSH-without-X11-forwarding/sandboxed
            // Linux, arboard can't connect to a display server, and copy/paste
            // + OSC 52 then silently no-op.
            clipboard: {
                match arboard::Clipboard::new() {
                    Ok(cb) => Some(cb),
                    Err(e) => {
                        log::warn!(
                            "clipboard unavailable ({e}); copy/paste and OSC 52 \
                             will no-op — no DISPLAY/Wayland, headless SSH, or a \
                             sandbox without clipboard-portal access?"
                        );
                        None
                    }
                }
            },
            compiled_triggers: initial_triggers,
            last_trigger_fire: std::time::Instant::now() - std::time::Duration::from_secs(60),
            remote_scanner: kettle_remote::RemoteScanner::new(),
            last_remote_poll: std::time::Instant::now() - std::time::Duration::from_secs(60),
            last_schedule_decision: None,
            config_path: startup.config.clone(),
            startup,
            _watcher: watcher,
            _remote_watcher: remote_watcher,
            pending_lua_send,
            pending_lua_actions,
            lua_engine,
            lua_startup_fired: false,
            update_available: None,
            version_line: startup_version,
            torn_drag: None,
        };
        event_loop.run_app(&mut app)?;
        Ok(())
    }

    /// Drain commands a Lua callback (event hook or menu-item)
    /// just enqueued. Handles every LuaCommand variant: SendText
    /// (cycle 325), ExecAction (cycle 326), Notify (cycle 371),
    /// SetTheme (cycle 373).
    ///
    /// Shared canonical drain path. Six call sites:
    ///   - fire_tab_add_event       (cycles 425-426)
    ///   - fire_tab_close_event     (cycles 424-426)
    ///   - bell-event drain         (cycle 427)
    ///   - output-event drain       (cycle 427)
    ///   - startup-event drain      (cycle 428)
    ///   - lua-menu-item click      (cycle 433)
    ///
    /// `App::new` cannot use this (early init operates on locals
    /// before `self` exists). All other LuaCommand consumers route
    /// here.
    ///
    /// `hook_name` is the hook label used in unknown-action warn
    /// messages (e.g. "tab_add hook", "lua menu-item").
    fn drain_lua_hook_commands(&mut self, hook_name: &str) {
        if let Some(eng) = &self.lua_engine {
            for cmd in eng.drain_commands() {
                match cmd {
                    crate::LuaCommand::SendText(s) => {
                        self.pending_lua_send.extend_from_slice(s.as_bytes());
                    }
                    crate::LuaCommand::ExecAction(name) => {
                        if let Some(a) = kettle_config::Action::from_name(&name) {
                            self.pending_lua_actions.push(a);
                        } else {
                            log::warn!(
                                "lua kettle.exec_action ({hook_name}): \
                                 unknown action {name:?}"
                            );
                        }
                    }
                    crate::LuaCommand::Notify { title, body } => {
                        fire_notify(&title, &body);
                    }
                    crate::LuaCommand::SetTheme(name) => {
                        if let Some(canonical) = kettle_config::Theme::find_name(&name) {
                            self.cfg.theme_name = canonical.to_string();
                            self.cfg.theme = kettle_config::Theme::by_name(canonical);
                        } else {
                            log::warn!("lua kettle.set_theme: unknown theme {name:?}");
                        }
                    }
                }
            }
        }
    }

    /// Cycle 425+426: fire LuaEvent::TabAdd + drain commands.
    /// Every Mux::new_tab / new_tab_with call site that USER-VISIBLY
    /// creates a tab (i.e. NOT startup-time first-tab init before
    /// plugins load) should call this. Centralizes the plugin-
    /// contract dispatch so future new_tab callers can't drift.
    fn fire_tab_add_event(&mut self, ws: &mut WindowState) {
        #[cfg(feature = "dev-record")]
        if let Some(rec) = self.recorder.as_mut() {
            rec.record_marker("kettle:tab_add");
        }
        if let Some(eng) = &self.lua_engine {
            eng.fire_event(&crate::LuaEvent::TabAdd(ws.mux.active));
        }
        self.drain_lua_hook_commands("tab_add hook");
    }

    /// Cycle 424+426: fire LuaEvent::TabClose + drain commands.
    /// Every close_tab call site should call this so plugins
    /// listening for tab_close see every close regardless of
    /// trigger source.
    fn fire_tab_close_event(&mut self, closing_idx: usize) {
        #[cfg(feature = "dev-record")]
        if let Some(rec) = self.recorder.as_mut() {
            rec.record_marker("kettle:tab_close");
        }
        if let Some(eng) = &self.lua_engine {
            eng.fire_event(&crate::LuaEvent::TabClose(closing_idx));
        }
        self.drain_lua_hook_commands("tab_close hook");
    }

    /// Cycle 888: the shell the focused pane has effectively entered (e.g. the
    /// user opened pwsh then typed `wsl`) — its argv + cwd, so a split can clone
    /// it in the same directory instead of a fresh default shell. Walks the
    /// focused pane's process tree via the remote scanner (one refresh on the
    /// split keystroke — not per frame). `None` for a plain pane, in which case
    /// the split falls back to cloning the pane's own launch command (cycle 886).
    fn focused_foreground_shell(
        &mut self,
        ws: &mut WindowState,
    ) -> Option<kettle_remote::ShellLaunch> {
        let (pid, pane_cwd) = {
            let pane = ws.mux.focused()?;
            (pane.term.child_pid()?, pane.term.current_dir())
        };
        self.remote_scanner.refresh();
        let mut shell = self.remote_scanner.foreground_shell(pid)?;
        // Cycle 917 (#2): assert the interactive-shell contract at the boundary.
        // The detector already rejects one-shot helpers, but re-checking here
        // means a split can never clone a non-interactive argv into a dead pane —
        // `None` routes the caller to `Mux::split`, which clones the pane's own
        // launch shell (falling back to the configured default).
        if !kettle_remote::shell_launch_is_interactive(&shell.argv) {
            return None;
        }
        // Prefer the pane's OSC 7-reported cwd — where the user actually is (e.g.
        // the WSL bash dir) — over the detected process's cwd, which Windows
        // sysinfo typically can't read for `wsl.exe` (it returned None, so the
        // split fell back to WSL's home `~`). `launch_cwd` carries this via
        // `wsl --cd`, which accepts both `/mnt/c/...` and native Linux paths.
        if pane_cwd.is_some() {
            shell.cwd = pane_cwd;
        }
        Some(shell)
    }

    /// Cycle 750: fire LuaEvent::PaneClose + drain commands — the pane analog
    /// of `fire_tab_close_event`. Every close-pane call site calls this with
    /// the id captured *before* `Mux::close_focused` removes the pane, so
    /// plugins listening for `pane_close` see the right id regardless of
    /// trigger source (keybind, confirm dialog, menu).
    fn fire_pane_close_event(&mut self, pane_id: u64) {
        if let Some(eng) = &self.lua_engine {
            eng.fire_event(&crate::LuaEvent::PaneClose(pane_id));
        }
        self.drain_lua_hook_commands("pane_close hook");
    }

    fn waker(&self) -> kettle_core::Waker {
        let p = self.proxy.clone();
        let pending = self.wake_pending.clone();
        Arc::new(move || {
            // v2.20.0 P4: enqueue at most ONE Wakeup per paint window. A
            // queued Wakeup re-checks every pane's output generation when it
            // lands, so every enqueue past the first is pure event-loop spam
            // (a flood produced one per 64KiB read). `swap` returning false
            // means this call owns the enqueue; the Wakeup arm reopens the
            // latch before it reads generations, so nothing is ever missed.
            // Events the proxy queues on the crossbeam channel before waking
            // are drained by that same pending Wakeup — suppression never
            // delays them.
            if !pending.swap(true, std::sync::atomic::Ordering::AcqRel) {
                let _ = p.send_event(UserEvent::Wakeup);
            }
        })
    }

    fn cell_px(&self, ws: &WindowState) -> (u16, u16) {
        ws.renderer
            .as_ref()
            .map(|r| (r.cell_w.max(1.0) as u16, r.cell_h.max(1.0) as u16))
            .unwrap_or((8, 16))
    }

    /// Hide the OS mouse cursor; idempotent. Called when the user starts
    /// typing if `mouse-hide-while-typing` is on. The cursor reappears on
    /// the next mouse move or window-enter event.
    fn hide_mouse_cursor(&mut self, ws: &mut WindowState) {
        if ws.mouse_hidden || !self.cfg.mouse_hide_while_typing {
            return;
        }
        if let Some(w) = &ws.window {
            w.set_cursor_visible(false);
            ws.mouse_hidden = true;
        }
    }

    /// Show the OS mouse cursor; idempotent. Called whenever the mouse
    /// moves or re-enters the window.
    fn show_mouse_cursor(&mut self, ws: &mut WindowState) {
        if !ws.mouse_hidden {
            return;
        }
        if let Some(w) = &ws.window {
            w.set_cursor_visible(true);
            ws.mouse_hidden = false;
        }
    }

    /// True when the mouse cursor is inside the tab bar's vertical band.
    /// Used to route wheel events away from scrollback so spinning the
    /// wheel over the tab bar cycles tabs (kitty / iTerm2 / Ghostty
    /// parity). When the tab bar is hidden (`tab-bar = off` or
    /// `auto` with one tab) this returns `false`.
    /// Cycle 320: cursor is over the cycle-295 status bar. Used
    /// by `cursor_in_chrome_band` so the OS arrow icon overrides
    /// the terminal I-beam over the status strip too.
    fn cursor_in_status_bar(&self, ws: &WindowState) -> bool {
        let h = self.status_bar_h(ws);
        if h <= 0.0 {
            return false;
        }
        let (_, sh) = ws
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        cursor_in_status_bar_band(ws.cursor.y as f32, h, sh as f32, self.cfg.status_bar)
    }

    /// Cycle 320: combined chrome-band hit-test. True when the
    /// cursor is over either the tab bar or the status bar — both
    /// belong in the "OS arrow cursor" group.
    fn cursor_in_chrome_band(&self, ws: &WindowState) -> bool {
        self.cursor_in_tab_bar(ws) || self.cursor_in_status_bar(ws)
    }

    fn cursor_in_tab_bar(&self, ws: &WindowState) -> bool {
        let h = self.tab_bar_h(ws);
        if h <= 0.0 {
            return false;
        }
        let (sw, sh) = ws
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        // Cycle 668 (vertical-tabs sub-cycle 4): for Left/Right
        // strips, the cursor needs to be within
        // `VERTICAL_TAB_STRIP_W` of the configured edge.
        match self.cfg.tab_bar_pos {
            TabBarPos::Left => {
                let x = ws.cursor.x as f32;
                (0.0..self.cfg.tab_bar_width).contains(&x)
            }
            TabBarPos::Right => {
                let x = ws.cursor.x as f32;
                x >= sw as f32 - self.cfg.tab_bar_width && x <= sw as f32
            }
            TabBarPos::Top | TabBarPos::Bottom => {
                cursor_in_tab_bar_band(ws.cursor.y as f32, h, sh as f32, self.cfg.tab_bar_pos)
            }
        }
    }

    /// Cycle 904 (audit): the resize cursor for a split of the given
    /// orientation. A Horizontal split places panes side-by-side (a vertical
    /// divider you drag left/right → column resize); a Vertical split stacks
    /// them (a horizontal divider you drag up/down → row resize).
    fn resize_cursor_for(dir: Dir) -> CursorIcon {
        match dir {
            Dir::Horizontal => CursorIcon::ColResize,
            Dir::Vertical => CursorIcon::RowResize,
        }
    }

    /// Cycle 904: a SplitDrag for the divider seam under content-area pixel
    /// `(px, py)`, or None. Used to start a drag-to-resize on left-press.
    fn split_drag_at(&self, ws: &WindowState, area: Rect, px: f32, py: f32) -> Option<SplitDrag> {
        let seams = ws.mux.split_seams(ws.mux.active, area);
        let i = crate::mux::seam_at(&seams, px, py, SPLIT_SEAM_TOL)?;
        let s = &seams[i];
        Some(SplitDrag {
            path: s.path.clone(),
            dir: s.dir,
        })
    }

    /// Cycle 904: the resize cursor to show while merely HOVERING a divider
    /// seam (no drag yet), or None when the cursor isn't over one. Suppressed
    /// while a modal owns the pointer. Cheap in the common single-pane case —
    /// `split_seams` returns empty when the tab root is a lone leaf.
    fn split_seam_hover_icon(&self, ws: &WindowState) -> Option<CursorIcon> {
        if self.any_modal_open(ws) {
            return None;
        }
        let area = self.area(ws);
        let seams = ws.mux.split_seams(ws.mux.active, area);
        let i = crate::mux::seam_at(
            &seams,
            ws.cursor.x as f32,
            ws.cursor.y as f32,
            SPLIT_SEAM_TOL,
        )?;
        Some(Self::resize_cursor_for(seams[i].dir))
    }

    /// Set the OS mouse-cursor icon, deduped against the last value pushed
    /// to the window. Called on CursorMoved (position changes the
    /// hit-test) and on ModifiersChanged (the modifier state gates the
    /// click-to-open affordance).
    fn sync_cursor_icon(&mut self, ws: &mut WindowState) {
        // Browser / iTerm2 / Ghostty convention: the OS cursor turns into
        // a "pointing hand" while the user holds the same modifier that
        // would open a URL on click (Ctrl on Linux/Windows, Cmd on
        // macOS — winit's `super_key` maps Cmd) and the pointer sits on
        // a clickable link. Otherwise show the standard text-I-beam, the
        // affordance every modern terminal uses for "this surface accepts
        // mouse selection."
        //
        // Chrome surfaces (tab bar, open modal overlays) override that —
        // they're clickable, not selectable, so the I-beam there is
        // visually misleading. `chrome_cursor_icon` is the pure decision.
        //
        // Tab close-buttons (`✕`) override the chrome `Default` with
        // `Pointer` so the trailing glyph reads as a clickable
        // affordance rather than a character in the title. Recomputed
        // here every cursor-move so the chip-hover state in the
        // renderer stays in sync (the renderer reads
        // `tabbar.hovered_close_idx`, which we pass through in
        // `tab_bar()`).
        let bar = self.tab_bar(ws);
        ws.hovered_close_idx =
            hovered_close_button(&bar.segments, ws.cursor.x as f32, ws.cursor.y as f32);
        let close_hover = tab_close_hover_icon(ws.hovered_close_idx.is_some());
        let chrome = chrome_cursor_icon(self.cursor_in_chrome_band(ws), self.any_modal_open(ws));
        // Cycle 904: a split divider under the cursor shows the resize cursor
        // (after chrome/close-button, before the link-pointer / I-beam default).
        let want = close_hover
            .or(chrome)
            .or_else(|| self.split_seam_hover_icon(ws))
            .unwrap_or_else(|| {
                let want_pointer = (ws.mods.control_key() || ws.mods.super_key())
                    && self.link_at_cursor(ws).is_some();
                if want_pointer {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Text
                }
            });
        if ws.last_cursor_icon != Some(want)
            && let Some(w) = &ws.window
        {
            w.set_cursor(want);
            ws.last_cursor_icon = Some(want);
        }
    }

    /// Clear the focused pane's selection (called when the user types —
    /// every modern terminal does this so a stale highlight doesn't
    /// confuse the next copy/paste). No-op when nothing is selected.
    fn clear_selection_on_input(&mut self, ws: &mut WindowState) {
        if let Some(p) = ws.mux.focused()
            && let Ok(mut t) = p.term.term.lock()
            && t.selection.is_some()
        {
            t.selection = None;
        }
    }

    fn tab_bar_h(&self, ws: &WindowState) -> f32 {
        let show = match self.cfg.tab_bar {
            TabBarMode::Off => false,
            // v2.19.0 (tear-off UX, re-dock): a live dock preview
            // MATERIALIZES the bar on a single-tab auto window — the
            // strip appears under the hovering torn window so the drop
            // target is visible before the drop (Chrome's always-on
            // strip affordance, on demand).
            TabBarMode::Auto => ws.mux.tabs.len() > 1 || ws.dock_preview.is_some(),
            TabBarMode::Always => true,
        };
        if show {
            ws.renderer.as_ref().map(|r| r.cell_h + 8.0).unwrap_or(24.0)
        } else {
            0.0
        }
    }

    /// Cycle 296: status-bar height (0 when off, cell_h + 6 px when
    /// enabled). Pair with `cfg.status_bar` (StatusBarMode) for
    /// position. Slightly shorter than the tab bar so the two strips
    /// read as distinct horizontal bands.
    fn status_bar_h(&self, ws: &WindowState) -> f32 {
        match self.cfg.status_bar {
            kettle_config::StatusBarMode::Off => 0.0,
            _ => ws.renderer.as_ref().map(|r| r.cell_h + 6.0).unwrap_or(22.0),
        }
    }

    /// Content area for panes (excludes both the tab bar and the
    /// cycle-296 status bar), in physical pixels.
    /// Cycle 389 (Terminator parity, titlebar Bucket-D sub-cycle 5):
    /// hit-test for a click in any pane's per-pane titlebar region.
    /// Returns the pane id whose titlebar was clicked, or None if
    /// the click fell outside titlebar regions. Honors
    /// cfg.show_titlebar gate + the cycle-385 title_at_bottom flip.
    /// Used by the click handler to dispatch EditPaneTitle on the
    /// clicked pane.
    fn pane_at_titlebar_click(&self, ws: &WindowState, px: f32, py: f32) -> Option<u64> {
        if !self.cfg.show_titlebar {
            return None;
        }
        let active = ws.mux.active;
        let rects = ws.mux.layout(active, self.area(ws));
        if rects.len() < 2 {
            // Single-pane tab: titlebar isn't rendered (cycle-379
            // gates on >1 pane).
            return None;
        }
        let bar_h = ws.renderer.as_ref().map(|r| r.cell_h + 6.0).unwrap_or(20.0);
        pane_titlebar_hit(px, py, &rects, self.cfg.title_at_bottom, bar_h)
    }

    fn area(&self, ws: &WindowState) -> Rect {
        let surface = ws
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        // Cycle 651 + 673: delegate to the pure helper, threading
        // `cfg.tab_bar_width` so a user-configured strip width
        // is honored.
        content_rect_for_with_strip(
            surface,
            self.tab_bar_h(ws),
            self.status_bar_h(ws),
            self.cfg.tab_bar_pos,
            self.cfg.status_bar,
            self.cfg.tab_bar_width,
        )
    }

    /// Tab-bar geometry — the single source of truth shared by the renderer
    /// (drawing) and the click hit-testing below.
    fn tab_bar(&self, ws: &WindowState) -> TabBar {
        let height = self.tab_bar_h(ws);
        if height <= 0.0 {
            return TabBar::hidden();
        }
        let (w, h) = ws
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        let (sw, sh) = (w as f32, h as f32);
        let is_vertical = self.cfg.tab_bar_pos.is_vertical();
        // Cycle 668 (vertical-tabs sub-cycle 4): Left / Right
        // route through a separate vertical-stacked path.
        // Horizontal Top/Bottom keeps the cycle-620
        // compute_tab_segment_widths flow.
        if is_vertical {
            return self.tab_bar_vertical(ws, sw, sh, height);
        }
        let y = match self.cfg.tab_bar_pos {
            TabBarPos::Top | TabBarPos::Left | TabBarPos::Right => 0.0,
            TabBarPos::Bottom => sh - height,
        };
        let titles = ws.mux.tab_titles();
        let n = titles.len().max(1);
        // Trailing "▾ +" button: a `▾` dropdown arrow (left) + the `+` (right),
        // each `height` wide. Cycle 805: the strip must reserve the WHOLE
        // button (arrow + plus), not just `plus_w`, or the last tab segment
        // overlaps it.
        let plus_w = height;
        // Cycle 917 (#4): hide the ▾ shell-dropdown when only one shell is
        // available (e.g. stock Ubuntu = just bash). A zero-width arrow drops it
        // from both the render pass (`new_tab_menu.2 > 0.0`) and the click
        // hit-test, and the `+` button reclaims the space.
        let arrow_w = if new_tab_dropdown_visible() {
            height
        } else {
            0.0
        };
        let button_w = plus_w + arrow_w;
        // Cycle 821: the drag-to-reorder handler derives its strip width from
        // the same helper, so the two can't disagree on where segments end.
        let strip = tab_segment_strip_width(sw, plus_w, arrow_w);
        let (arrow_rect, plus_rect) =
            split_new_tab_button((sw - button_w, y, button_w, height), arrow_w);
        let cell_w = ws
            .renderer
            .as_ref()
            .map(|r| r.cell_w)
            .unwrap_or(8.0)
            .max(1.0);
        // Cycle 620 (Terminator parity, terminatorlib/config.py:88
        // `homogeneous_tabbar`): per-tab widths from a pure helper.
        // `homogeneous = true` (kettle default) keeps the equal-width
        // strip; `false` sizes each tab to its title length so a
        // short tab like "fish" isn't padded out as wide as a long
        // tab like "vim ~/Projects/some-long-path".
        let widths = compute_tab_segment_widths(
            titles.iter().map(|s| s.as_str()),
            strip,
            cell_w,
            height,
            self.cfg.homogeneous_tabbar,
        );
        let active = ws.mux.active;
        // Pre-compute x offsets from the cumulative widths so the
        // closure stays a stateless map. (Slight allocation, but
        // n is small — tab counts cap in the dozens.)
        let mut x_offsets: Vec<f32> = Vec::with_capacity(n + 1);
        x_offsets.push(0.0);
        for w in &widths {
            let last = *x_offsets.last().unwrap();
            x_offsets.push(last + w);
        }
        let segments = titles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let x = x_offsets[i];
                let seg_w = widths[i];
                // Cycle 246: pull per-tab activity into the segment so
                // the renderer can draw the indicator dot. Active tabs
                // short-circuit to Normal (the focused-tab accent
                // already signals "you are here").
                let now = std::time::Instant::now();
                let silence = std::time::Duration::from_millis(self.cfg.tab_silence_threshold_ms);
                let activity = ws
                    .mux
                    .tabs
                    .get(i)
                    .map(|tab| {
                        match crate::mux::classify_tab_activity(
                            i == active,
                            tab.bell,
                            tab.last_output_at,
                            tab.last_seen_at,
                            now,
                            silence,
                        ) {
                            crate::mux::TabActivity::Normal => RenderTabActivity::Normal,
                            crate::mux::TabActivity::Output => RenderTabActivity::Output,
                            crate::mux::TabActivity::Bell => RenderTabActivity::Bell,
                            crate::mux::TabActivity::Silent => RenderTabActivity::Silent,
                        }
                    })
                    .unwrap_or(RenderTabActivity::Normal);
                TabSeg {
                    idx: i,
                    rect: (x, y, seg_w, height),
                    // ✕ hit zone = the trailing `height`-wide square.
                    close: (x + seg_w - height, y, height, height),
                    title: t.clone(),
                    active: i == active,
                    activity,
                }
            })
            .collect();
        TabBar {
            height,
            y,
            segments,
            new_tab: plus_rect,
            new_tab_menu: arrow_rect,
            // Cycle 178: broadcast indicator on the active tab.
            broadcast: ws.mux.is_broadcast_on(),
            // Hover-on-✕ chip: renderer paints a red highlight behind
            // the close glyph; UI's `sync_cursor_icon` flips the OS
            // cursor to Pointer at the same time.
            hovered_close_idx: ws.hovered_close_idx,
            // Cycle 255: while a tab-bar drag is in progress, hand
            // the renderer the cursor x so it paints a translucent
            // ghost of the dragged segment under the cursor — gives
            // the cycle-249 reorder a "I'm picking this tab up"
            // affordance instead of the bare snap behavior.
            drag_cursor_x: if ws.tab_drag_active {
                Some(ws.cursor.x as f32)
            } else {
                None
            },
            // v2.19.0 (re-dock): vertical 2-px insertion line at the
            // docked tab's landing slot, full bar height.
            insert_marker: ws.dock_preview.map(|idx| {
                let x = x_offsets[idx.min(n)].min(strip - 2.0).max(0.0);
                (x, y, 2.0, height)
            }),
        }
    }

    /// Cycle 668 (vertical-tabs sub-cycle 4): tab-bar layout for
    /// `TabBarPos::Left` / `Right`. Stacks segments vertically,
    /// each one (`VERTICAL_TAB_STRIP_W` × `tab_bar_h`).
    /// New-tab `+` button anchors at the bottom of the strip.
    fn tab_bar_vertical(&self, ws: &WindowState, sw: f32, sh: f32, height: f32) -> TabBar {
        let strip_w = self.cfg.tab_bar_width;
        let strip_x = match self.cfg.tab_bar_pos {
            TabBarPos::Left => 0.0,
            TabBarPos::Right => sw - strip_w,
            _ => 0.0, // unreachable in this branch
        };
        let titles = ws.mux.tab_titles();
        let active = ws.mux.active;
        let now = std::time::Instant::now();
        let silence = std::time::Duration::from_millis(self.cfg.tab_silence_threshold_ms);
        let segments: Vec<TabSeg> = titles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let seg_y = i as f32 * height;
                let activity = ws
                    .mux
                    .tabs
                    .get(i)
                    .map(|tab| {
                        match crate::mux::classify_tab_activity(
                            i == active,
                            tab.bell,
                            tab.last_output_at,
                            tab.last_seen_at,
                            now,
                            silence,
                        ) {
                            crate::mux::TabActivity::Normal => RenderTabActivity::Normal,
                            crate::mux::TabActivity::Output => RenderTabActivity::Output,
                            crate::mux::TabActivity::Bell => RenderTabActivity::Bell,
                            crate::mux::TabActivity::Silent => RenderTabActivity::Silent,
                        }
                    })
                    .unwrap_or(RenderTabActivity::Normal);
                TabSeg {
                    idx: i,
                    rect: (strip_x, seg_y, strip_w, height),
                    // ✕ hit zone = the trailing-right square of the
                    // segment (same axis convention as horizontal).
                    close: (strip_x + strip_w - height, seg_y, height, height),
                    title: t.clone(),
                    active: i == active,
                    activity,
                }
            })
            .collect();
        // `+` button at the bottom of the strip.
        let plus_y = (titles.len() as f32 * height).min(sh - height);
        TabBar {
            height,
            // `y` is the *band start* (top of the strip) for the
            // renderer; for vertical strips the band spans the
            // whole window height, so `y = 0`.
            y: 0.0,
            segments,
            new_tab: (strip_x, plus_y, strip_w, height),
            // Cycle 805: no dropdown arrow on vertical bars — the bottom-of-
            // strip full-width `+` has nowhere sensible for a left-side arrow.
            new_tab_menu: (0.0, 0.0, 0.0, 0.0),
            broadcast: ws.mux.is_broadcast_on(),
            hovered_close_idx: ws.hovered_close_idx,
            // Drag-cursor preview is x-only in v1; vertical drag
            // reorder is sub-cycle 6 of the design.
            drag_cursor_x: if ws.tab_drag_active {
                Some(ws.cursor.x as f32)
            } else {
                None
            },
            // v2.19.0 (re-dock): horizontal 2-px insertion line across
            // the strip at the docked tab's landing slot.
            insert_marker: ws.dock_preview.map(|idx| {
                let iy = (idx.min(titles.len()) as f32 * height)
                    .min(sh - 2.0)
                    .max(0.0);
                (strip_x, iy, strip_w, 2.0)
            }),
        }
    }

    /// Height the per-pane titlebar reserves at the top of each pane in a tab
    /// with `pane_count` panes, matching the renderer's `pane_titlebar_h`
    /// (`cfg.show_titlebar && panes > 1 → cell_h + 6`) and the cycle-389
    /// titlebar hit-test. `0.0` (no inset) for a single-pane tab or when
    /// titlebars are off. Cycle 817 (audit).
    fn pane_titlebar_inset(&self, ws: &WindowState, pane_count: usize) -> f32 {
        if self.cfg.show_titlebar && pane_count > 1 {
            ws.renderer.as_ref().map(|r| r.cell_h + 6.0).unwrap_or(20.0)
        } else {
            0.0
        }
    }

    fn grid_of(&self, ws: &WindowState, rect: Rect) -> (usize, usize) {
        // Full-area / single-pane sizing: no titlebar inset. Per-pane sizing in
        // a split tab goes through `grid_of_inset` (cycle 817).
        self.grid_of_inset(ws, rect, 0.0)
    }

    fn grid_of_inset(&self, ws: &WindowState, rect: Rect, titlebar_h: f32) -> (usize, usize) {
        let (cw, ch) = ws
            .renderer
            .as_ref()
            .map(|r| (r.cell_w, r.cell_h))
            .unwrap_or((8.0, 16.0));
        let (_, _, w, h) = rect;
        grid_dims_px(
            (w, h),
            (cw, ch),
            (self.cfg.padding_x, self.cfg.padding_y),
            titlebar_h,
        )
    }

    fn focused_rect(&self, ws: &WindowState, area: Rect) -> Option<Rect> {
        let f = ws.mux.active_focus()?;
        ws.mux
            .layout(ws.mux.active, area)
            .into_iter()
            .find(|(id, _)| *id == f)
            .map(|(_, r)| r)
    }

    /// If `(px, py)` is on the focused pane's scrollbar (right edge, ~8 px)
    /// and the bar is visible, jump the viewport to the clicked position.
    /// Returns `true` if it handled the click (so it won't start a
    /// selection).
    fn scrollbar_jump(&mut self, ws: &mut WindowState, area: Rect, px: f32, py: f32) -> bool {
        self.scrollbar_at(ws, area, px, py, true)
    }

    /// Map a pointer position to a viewport jump on the focused pane's
    /// scrollbar. With `require_zone`, only the right-edge ~8 px strip
    /// counts (initial click); during a drag the x is ignored so the
    /// grab follows the pointer's y anywhere.
    fn scrollbar_at(
        &mut self,
        ws: &mut WindowState,
        area: Rect,
        px: f32,
        py: f32,
        require_zone: bool,
    ) -> bool {
        if self.cfg.scrollbar == kettle_config::ScrollbarMode::Never {
            return false;
        }
        let Some((rx, ry, rw, rh)) = self.focused_rect(ws, area) else {
            return false;
        };
        if require_zone && (px < rx + rw - 8.0 || px > rx + rw || py < ry || py > ry + rh) {
            return false;
        }
        let Some(p) = ws.mux.focused() else {
            return false;
        };
        let Ok(mut t) = p.term.term.lock() else {
            return false;
        };
        use kettle_core::Dimensions;
        let g = t.grid();
        let (rows, hist, off) = (g.screen_lines(), g.history_size(), g.display_offset());
        let visible = self.cfg.scrollbar == kettle_config::ScrollbarMode::Always || off > 0;
        if !visible || rows + hist <= rows {
            return false;
        }
        let target = kettle_core::scrollbar::target_offset(py - ry, rh, rows, hist);
        let delta = target as i32 - off as i32;
        if delta != 0 {
            t.scroll_display(kettle_core::Scroll::Delta(delta));
        }
        true
    }

    fn px_to_point(
        &self,
        ws: &WindowState,
        rect: Rect,
        px: f32,
        py: f32,
    ) -> (kettle_core::Point, kettle_core::Side) {
        let (cw, ch) = ws
            .renderer
            .as_ref()
            .map(|r| (r.cell_w, r.cell_h))
            .unwrap_or((8.0, 16.0));
        // Cycle 817 (audit): apply the same per-pane-titlebar inset the renderer
        // draws content with, or a split pane's pointer maps ~1 row too high.
        // The focused pane lives in the active tab, so its inset depends on the
        // active tab's pane count.
        let titlebar_h =
            self.pane_titlebar_inset(ws, ws.mux.layout(ws.mux.active, self.area(ws)).len());
        let (col, line, side) = px_to_cell(
            px,
            py,
            rect,
            (cw, ch),
            (self.cfg.padding_x, self.cfg.padding_y),
            titlebar_h,
        );
        (
            kettle_core::Point::new(kettle_core::Line(line), kettle_core::Column(col)),
            side,
        )
    }

    /// Cycle 288: pull the on-screen text of `row` (viewport-relative)
    /// from the focused pane's grid so `smart_selection_at` can run its
    /// regex against the actual cells the user clicked on. Returns
    /// `None` if there's no focused pane, the lock can't be acquired,
    /// or the row is out of range.
    /// Cycle 301 sub-cycle 4: extract the text of the vi visual
    /// selection from the focused pane's grid. Inclusive on both
    /// ends. Joins rows with `\n`. Returns "" on no focus / lock
    /// failure. Same row→column iteration shape as
    /// `line_text_for_smart_select` to keep grid-read paths
    /// consistent.
    fn yank_vi_selection(
        &mut self,
        ws: &mut WindowState,
        start: (usize, usize),
        end: (usize, usize),
    ) -> String {
        let Some(pane) = ws.mux.focused() else {
            return String::new();
        };
        let Ok(t) = pane.term.term.lock() else {
            return String::new();
        };
        use kettle_core::Dimensions;
        let cols = t.columns();
        let rows = t.screen_lines();
        let (sr, sc) = start;
        let (er, ec) = end;
        if sr >= rows {
            return String::new();
        }
        // Cycle 912 (R1 completion): the vi rows are viewport-relative (clamped
        // to 0..screen_lines, rendered at `oy + row*ch`), so convert each to a
        // grid-absolute line — a visual-yank made while scrolled back then reads
        // the VISIBLE rows, consistent with where the vi highlight is drawn,
        // instead of the active screen. No-op at the bottom (display_offset == 0).
        let off = t.grid().display_offset();
        let mut out = String::new();
        for r in sr..=er.min(rows.saturating_sub(1)) {
            let base = viewport_point_to_grid(
                kettle_core::Point::new(kettle_core::Line(r as i32), kettle_core::Column(0)),
                off,
            );
            let first = if r == sr { sc } else { 0 };
            let last = if r == er { ec.min(cols - 1) } else { cols - 1 };
            for c in first..=last {
                let p = kettle_core::Point::new(base.line, kettle_core::Column(c));
                out.push(t.grid()[p].c);
            }
            if r != er {
                out.push('\n');
            }
        }
        // Trim trailing whitespace from each line so blank cells at
        // the end of a yanked range don't pollute the clipboard.
        out.lines()
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn line_text_for_smart_select(&mut self, ws: &mut WindowState, row: usize) -> Option<String> {
        use kettle_core::Dimensions;
        let pane = ws.mux.focused()?;
        let t = pane.term.term.lock().ok()?;
        let cols = t.columns();
        let rows = t.screen_lines();
        if row >= rows {
            return None;
        }
        // R1: `row` is viewport-relative; index the grid-absolute line so the
        // smart-select regex runs on the row the user double-clicked even when
        // scrolled back into history.
        let base = viewport_point_to_grid(
            kettle_core::Point::new(kettle_core::Line(row as i32), kettle_core::Column(0)),
            t.grid().display_offset(),
        );
        let mut out = String::with_capacity(cols);
        for c in 0..cols {
            let p = kettle_core::Point::new(base.line, kettle_core::Column(c));
            out.push(t.grid()[p].c);
        }
        // Trim trailing spaces so the regex doesn't try to match across
        // padding.
        Some(out.trim_end().to_string())
    }

    /// Cycle 288: install a `Simple` selection from `(row, start)` to
    /// `(row, end)` inclusive in the focused pane. Returns true if the
    /// selection was set, false if there's no focused pane or the lock
    /// failed (in which case the caller falls through to its normal
    /// `begin_selection` path).
    fn apply_smart_selection(
        &mut self,
        ws: &mut WindowState,
        area: Rect,
        row: usize,
        start: usize,
        end: usize,
    ) -> bool {
        // The `_area` is unused but kept in the signature to mirror
        // `begin_selection`'s API — future viewport-aware variants may
        // need it for clamping.
        let _ = area;
        let Some(pane) = ws.mux.focused() else {
            return false;
        };
        let Ok(mut t) = pane.term.term.lock() else {
            return false;
        };
        // R1: the click `row` is viewport-relative; convert both ends to
        // grid-absolute so a double-click word-select while scrolled back
        // selects (and copies) the row the user actually clicked.
        let off = t.grid().display_offset();
        let anchor = viewport_point_to_grid(
            kettle_core::Point::new(kettle_core::Line(row as i32), kettle_core::Column(start)),
            off,
        );
        let end_pt = viewport_point_to_grid(
            kettle_core::Point::new(kettle_core::Line(row as i32), kettle_core::Column(end)),
            off,
        );
        let mut sel = kettle_core::Selection::new(
            kettle_core::SelectionType::Simple,
            anchor,
            kettle_core::Side::Left,
        );
        sel.update(end_pt, kettle_core::Side::Right);
        t.selection = Some(sel);
        // Like Semantic, a smart selection resolves on press; the
        // caller treats `selecting=false` so motion doesn't extend it.
        ws.selecting = false;
        true
    }

    fn begin_selection(
        &mut self,
        ws: &mut WindowState,
        area: Rect,
        ty: kettle_core::SelectionType,
    ) {
        // Simple + Block are drags; word/line select immediately on click.
        ws.selecting = matches!(
            ty,
            kettle_core::SelectionType::Simple | kettle_core::SelectionType::Block
        );
        if let Some(rect) = self.focused_rect(ws, area) {
            let (vp, side) = self.px_to_point(ws, rect, ws.cursor.x as f32, ws.cursor.y as f32);
            if let Some(pane) = ws.mux.focused()
                && let Ok(mut t) = pane.term.term.lock()
            {
                // R1: store the grid-absolute point (viewport − display_offset)
                // so a selection started while scrolled back anchors on the
                // history row the user sees, not the active screen.
                let p = viewport_point_to_grid(vp, t.grid().display_offset());
                // The anchor carries the pointer's sub-cell side so a Simple/Block
                // drag trims the start cell when the press lands in its right half
                // (Semantic/Lines ignore the side — they snap to token bounds).
                t.selection = Some(kettle_core::Selection::new(ty, p, side));
            }
        }
    }

    /// Click count for the press at `(row,col)` within ~400 ms of the last.
    fn click_count(&mut self, ws: &mut WindowState, row: usize, col: usize) -> u8 {
        let now = std::time::Instant::now();
        let n = match ws.last_click {
            Some((t, r, c, n))
                if r == row
                    && c == col
                    && now.duration_since(t) < std::time::Duration::from_millis(400) =>
            {
                n % 3 + 1
            }
            _ => 1,
        };
        ws.last_click = Some((now, row, col, n));
        n
    }

    /// Copy the focused pane's selection to the clipboard (call on release).
    /// Paste the clipboard into the focused pane, bracketed-paste-safe.
    /// Shared by `Action::Paste` and middle-click.
    /// Cycle 351 (Terminator parity, terminatorlib/config.py:86-87
    /// `use_custom_url_handler` + `custom_url_handler`): open a URL
    /// either via the custom external program (if configured + non-
    /// empty) or fall through to the cross-platform `open` crate.
    /// The custom program is invoked as
    ///   <custom_url_handler> <uri>
    /// detached, so kettle doesn't block on the handler exiting.
    /// Errors log::warn.
    fn open_url(&self, uri: &str) {
        if !kettle_core::links::is_safe_url(uri) {
            log::warn!("refused to open unsafe URL: {uri}");
            return;
        }
        // Cycle 705 (Terminator plugin parity, plugin sub-cycle:
        // `LuaEvent::UrlClicked`). Fired before pattern-handler
        // dispatch so analytics / logging / workflow-trigger
        // plugins see ALL URL clicks, regardless of which
        // handler eventually opens them. The cycle-374
        // `try_url_handler` chain below still owns the "actually
        // launch" decision; this event is observation-only.
        if let Some(eng) = &self.lua_engine {
            eng.fire_event(&crate::LuaEvent::UrlClicked(uri.to_string()));
        }
        // Cycle 374 (Terminator plugin parity, plugin sub-cycle 9):
        // Lua URL handlers get first dispatch. If a handler claims
        // the URL (its pattern matches), kettle does NOT fall
        // through to the cfg.custom_url_handler or system-open
        // paths — the handler decides what (if anything) to launch.
        if let Some(eng) = &self.lua_engine
            && eng.try_url_handler(uri)
        {
            return;
        }
        if self.cfg.use_custom_url_handler && !self.cfg.custom_url_handler.is_empty() {
            // Custom handler — spawn detached so a long-running
            // browser launch doesn't freeze kettle.
            let cmd = self.cfg.custom_url_handler.clone();
            let uri = uri.to_string();
            match std::process::Command::new(&cmd)
                .arg(&uri)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(_) => {}
                Err(e) => log::warn!("custom-url-handler {cmd:?}: {e}; falling through to system"),
            }
            return;
        }
        if let Err(e) = open::that_detached(uri) {
            log::warn!("failed to open {uri}: {e}");
        }
    }

    fn paste_clipboard(&mut self, ws: &mut WindowState) {
        let text = self
            .clipboard
            .as_mut()
            .and_then(|c| c.get_text().ok())
            .unwrap_or_default();
        self.paste_text(ws, text);
    }

    /// Cycle 755: paste the **X11 PRIMARY selection** (middle-click). On X11 the
    /// PRIMARY selection holds whatever was last highlighted with the mouse —
    /// distinct from the CLIPBOARD (Ctrl+C / Ctrl+Shift+C). The standard
    /// terminal convention is middle-click = paste PRIMARY, which kettle
    /// previously got wrong by aliasing `PastePrimary` straight to the regular
    /// clipboard. arboard exposes PRIMARY on Linux via `GetExtLinux`; on
    /// Wayland (no separate PRIMARY surfaced here), macOS, and Windows there is
    /// no PRIMARY selection, so we fall back to the regular clipboard — the
    /// historical behavior. Shares `paste_text` so the clamp + bracketed-paste
    /// + broadcast scoping match `Action::Paste`.
    fn paste_primary(&mut self, ws: &mut WindowState) {
        #[cfg(target_os = "linux")]
        let text = {
            use arboard::{GetExtLinux, LinuxClipboardKind};
            let primary = self
                .clipboard
                .as_mut()
                .and_then(|c| c.get().clipboard(LinuxClipboardKind::Primary).text().ok())
                .filter(|t| !t.is_empty());
            match primary {
                Some(t) => t,
                // PRIMARY empty/unset (or under Wayland) → fall back to clipboard.
                None => self
                    .clipboard
                    .as_mut()
                    .and_then(|c| c.get_text().ok())
                    .unwrap_or_default(),
            }
        };
        #[cfg(not(target_os = "linux"))]
        let text = self
            .clipboard
            .as_mut()
            .and_then(|c| c.get_text().ok())
            .unwrap_or_default();
        self.paste_text(ws, text);
    }

    /// Cycle 755: shared paste path — clamp, broadcast scoping, bracketed-paste
    /// wrap, write to the focused PTY. Extracted so `paste_clipboard` and
    /// `paste_primary` (and any future paste channel) can't drift on the
    /// safety/scoping rules.
    fn paste_text(&mut self, ws: &mut WindowState, text: String) {
        if text.is_empty() {
            return;
        }
        // Cap a runaway paste at 4 MiB on a UTF-8 char boundary so an
        // accidental "paste this 1 GB log" doesn't shove every byte into
        // the PTY in one go. `clamp_osc52` is named for OSC 52 but it's a
        // generic byte-clamper that preserves char boundaries — exactly
        // what we want for any paste channel.
        let text = clamp_osc52(&text, LOCAL_PASTE_MAX);
        // Cycle 876: record that a paste happened and its length — NEVER the
        // pasted content (a common secret vector). The per-key hook captures
        // the Ctrl+V chord; this marker captures the size without the bytes.
        #[cfg(feature = "dev-record")]
        if let Some(rec) = self.recorder.as_mut() {
            rec.record_marker(&format!("kettle:paste len={}", text.chars().count()));
        }
        // Broadcast paste (cycle 174 sibling to cycle 173): with the
        // group-input mode on (Ctrl+Shift+G), keystrokes go to every
        // pane in the active tab — paste is also user input and
        // should follow the same scoping. Each pane gets its own
        // `BRACKETED_PASTE` decision (different panes may have
        // different mode state), so the wrap is per-pane, not a
        // single shared payload.
        if ws.mux.is_broadcast_on() {
            ws.mux.broadcast_paste(text);
            return;
        }
        let bracketed = self
            .focused_mode(ws)
            .contains(kettle_core::TermMode::BRACKETED_PASTE);
        let bytes = input::paste_payload(text, bracketed);
        if let Some(p) = ws.mux.focused() {
            // Cycle 941: paste is user input — a read-only pane drops it.
            p.feed_input(&bytes);
        }
    }

    fn copy_selection(&mut self, ws: &mut WindowState) {
        let sel = ws
            .mux
            .focused()
            .and_then(|p| {
                p.term
                    .term
                    .lock()
                    .ok()
                    .and_then(|t| t.selection_to_string())
            })
            .filter(|s| !s.is_empty());
        if let (Some(s), Some(cb)) = (sel, self.clipboard.as_mut()) {
            // Cycle 777: log clipboard failures (was silently swallowed) so a
            // broken clipboard is diagnosable — matches the vi-mode yank path.
            if let Err(e) = cb.set_text(s) {
                log::warn!("clipboard set_text failed (selection copy): {e}");
            }
        }
    }

    fn update_selection(&mut self, ws: &mut WindowState, area: Rect) {
        if !ws.selecting {
            return;
        }
        if let Some(rect) = self.focused_rect(ws, area) {
            let (vp, side) = self.px_to_point(ws, rect, ws.cursor.x as f32, ws.cursor.y as f32);
            if let Some(pane) = ws.mux.focused()
                && let Ok(mut t) = pane.term.term.lock()
            {
                // R1: convert to grid-absolute before the mutable `selection`
                // borrow so the dragged end-point tracks scrollback too.
                let p = viewport_point_to_grid(vp, t.grid().display_offset());
                if let Some(sel) = t.selection.as_mut() {
                    // The drag end carries the pointer's sub-cell side so the
                    // boundary cell is included only once the pointer crosses its
                    // midpoint (xterm/Alacritty parity) — not always.
                    sel.update(p, side);
                }
            }
        }
    }

    /// Extend the existing selection to the cursor point (Shift+Click).
    /// Returns `true` when a selection was present *and* extended — the
    /// caller falls back to a fresh `begin_selection` when no selection
    /// existed (so Shift+Click on empty space starts a normal selection).
    /// Matches xterm / Alacritty / iTerm2: Shift+Click anchors the
    /// existing selection's start and pulls the end to the click.
    fn extend_selection_to_cursor(&mut self, ws: &mut WindowState, area: Rect) -> bool {
        let rect = match self.focused_rect(ws, area) {
            Some(r) => r,
            None => return false,
        };
        let (vp, side) = self.px_to_point(ws, rect, ws.cursor.x as f32, ws.cursor.y as f32);
        if let Some(pane) = ws.mux.focused()
            && let Ok(mut t) = pane.term.term.lock()
        {
            // R1: grid-absolute end-point so Shift+Click extends to the right
            // history row while scrolled back.
            let p = viewport_point_to_grid(vp, t.grid().display_offset());
            if let Some(sel) = t.selection.as_mut() {
                // Sub-cell side so Shift+Click lands the boundary on the same
                // half-cell rule as a drag.
                sel.update(p, side);
                // Enter drag mode so a follow-up mouse-move keeps extending —
                // matches every Mac/Linux text-control: shift-click, then drag.
                ws.selecting = true;
                return true;
            }
        }
        false
    }

    /// Resize every pane's PTY to match its tile in the layout.
    fn resize_all(&mut self, ws: &mut WindowState) {
        let (cw, ch) = self.cell_px(ws);
        let area = self.area(ws);
        let mut plan: Vec<(u64, usize, usize)> = Vec::new();
        for ti in 0..ws.mux.tabs.len() {
            // Cycle 817 (audit): a split tab's panes each lose `titlebar_h` of
            // height to their per-pane titlebar, so size each PTY for the rows
            // that fit below it (per-tab, since the inset depends on that tab's
            // own pane count).
            let panes = ws.mux.layout(ti, area);
            let titlebar_h = self.pane_titlebar_inset(ws, panes.len());
            for (id, r) in panes {
                let (cols, rows) = self.grid_of_inset(ws, r, titlebar_h);
                plan.push((id, cols, rows));
            }
        }
        for (id, cols, rows) in plan {
            if let Some(p) = ws.mux.panes.get_mut(&id) {
                p.term.resize(cols, rows, cw, ch);
            }
        }
    }

    fn drain_events(&mut self, ws: &mut WindowState) {
        let mut bell = false;
        // Cycle 246: pane ids that fired `TermEvent::Bell` this drain
        // pass — latched onto their containing tabs *after* the
        // values_mut() iteration so we don't double-borrow mux.panes.
        let mut bell_panes: Vec<u64> = Vec::new();
        // Cycle 378: pane ids + raw-output bytes accumulated during
        // this drain pass. Fired as LuaEvent::Output after the
        // values_mut iteration completes (to avoid borrow conflicts).
        let mut output_chunks: Vec<(u64, Vec<u8>)> = Vec::new();
        // Cycle 929 (agent-first A2): collect OSC-133 CommandFinished events per
        // pane so the App is the SINGLE drainer — `drain_command_finished_events`
        // is destructive, so the command-notify, the run_command correlator, and
        // event subscribers must all be fed from one place, AFTER this pane loop.
        let mut command_finished_local: Vec<(u64, kettle_core::CommandFinished)> = Vec::new();
        // Cycle 412: pane ids whose shell exited with cfg.exit_action
        // = Restart. Queued during the drain; appended to
        // ws.pending_pane_restarts after the iteration so the
        // post-drain handler can process them with a fresh borrow.
        let mut pending_restarts_local: Vec<u64> = Vec::new();
        // Cell size is renderer-owned and uniform across panes, so resolve it
        // once per drain rather than per event (a sixel/kitty app polling CSI
        // 14 t doesn't need a renderer lookup per CSI).
        let (cell_w, cell_h) = self.cell_px(ws);
        for (&pane_id, pane) in ws.mux.panes.iter_mut() {
            // Cycle 378: drain the optional output sidechannel
            // BEFORE the regular event channel. Coalesces multiple
            // chunks into a single Vec per pane per drain pass.
            if let Some(out_rx) = &pane.output_rx {
                let mut combined: Vec<u8> = Vec::new();
                while let Ok(chunk) = out_rx.try_recv() {
                    combined.extend_from_slice(&chunk);
                }
                if !combined.is_empty() {
                    output_chunks.push((pane_id, combined));
                }
            }
            while let Ok(ev) = pane.rx.try_recv() {
                match ev {
                    TermEvent::Title(t) => {
                        pane.title = t;
                    }
                    TermEvent::ResetTitle => pane.title = "kettle".into(),
                    TermEvent::PtyWrite(s) => pane.term.write(s.as_bytes()),
                    TermEvent::ClipboardStore(_, s) => {
                        // OSC 52 write — gated by policy (default: allowed).
                        if self.cfg.osc52.can_copy()
                            && let Some(cb) = &mut self.clipboard
                        {
                            // Cycle 777: log instead of silently swallowing.
                            if let Err(e) = cb.set_text(clamp_osc52(&s, OSC52_MAX).to_string()) {
                                log::warn!("clipboard set_text failed (OSC 52 write): {e}");
                            }
                        }
                    }
                    TermEvent::ClipboardLoad(_, fmt) => {
                        // OSC 52 read lets a (remote) program exfiltrate the
                        // clipboard; denied by default — reply empty so the
                        // protocol stays well-formed without leaking.
                        let text = if self.cfg.osc52.can_paste() {
                            self.clipboard
                                .as_mut()
                                .and_then(|c| c.get_text().ok())
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };
                        pane.term.write(fmt(&text).as_bytes());
                    }
                    TermEvent::TextAreaSizeRequest(fmt) => {
                        // CSI 14 t — text-area size in pixels. Sixel / kitty
                        // graphics / iTerm2-OSC-1337 apps probe this to do
                        // pixel-perfect image placements; the engine raises
                        // the event but expects us to fill in the cell + grid
                        // dimensions before the formatter produces the reply
                        // (CSI 4 ; <height> ; <width> t).
                        let (cols, rows) = pane
                            .term
                            .term
                            .lock()
                            .ok()
                            .map(|t| {
                                use kettle_core::Dimensions;
                                (t.columns() as u16, t.screen_lines() as u16)
                            })
                            .unwrap_or((0, 0));
                        let reply = kettle_render::reply_for_text_area_size(
                            cols, rows, cell_w, cell_h, &*fmt,
                        );
                        pane.term.write(reply.as_bytes());
                    }
                    TermEvent::ColorRequest(idx, fmt) => {
                        // OSC 4 ; n ; ? / OSC 10 / 11 / 12 — resolve against
                        // the active theme + any runtime overrides, then let
                        // the engine-supplied formatter render the canonical
                        // xparsecolor reply and write it back to the PTY. No
                        // reply for out-of-range indices keeps the protocol
                        // well-formed (apps that probe just see a timeout,
                        // exactly as on terminals that don't support OSC).
                        let reply = pane.term.term.lock().ok().and_then(|t| {
                            kettle_render::reply_for_query(idx, &self.cfg.theme, t.colors(), &*fmt)
                        });
                        if let Some(s) = reply {
                            pane.term.write(s.as_bytes());
                        }
                    }
                    TermEvent::CursorBlinkingChange => {
                        // DEC mode 12 (`CSI ?12 h/l`) just flipped. The
                        // next redraw will pick up the new state from
                        // `Terminal::cursor_blinking()`, but reset the
                        // blink phase so going *blink-on* starts visible
                        // and *blink-off* makes the cursor solid right
                        // away — not on whatever half-period we'd
                        // otherwise land in. (We're inside a
                        // `ws.mux.panes.values_mut()` loop here so
                        // we can't call `self.reset_blink_phase()`;
                        // the two field writes are the same body.)
                        ws.blink_on = true;
                        ws.last_blink = std::time::Instant::now();
                    }
                    // Cycle 349 (Terminator parity, terminatorlib/
                    // config.py:103 `force_no_bell`): silence every
                    // bell flavor regardless of cfg.bell mode. The
                    // match-guard combines the variant + the cfg
                    // check so a future tweak doesn't fight clippy's
                    // collapsible-if lint.
                    TermEvent::Bell if !self.cfg.force_no_bell => {
                        bell = true;
                        bell_panes.push(pane_id);
                    }
                    TermEvent::Bell => {}
                    // Cycle 357 (Terminator parity, terminatorlib/config.py:118
                    // `exit_action`): when the shell exits, choose
                    // whether to close the pane, restart the shell,
                    // or hold the dead shell visible (so the user can
                    // read final output / scrollback before closing
                    // manually).
                    //
                    // Hold: don't mark closed; pane shows the last
                    // output until user explicitly closes via
                    // Ctrl+Shift+W.
                    // Restart (cycle 412): queue the pane id for
                    // post-drain respawn so we don't double-borrow
                    // ws.mux during this iteration. Close the
                    // current PTY (pane.closed = true) — the
                    // post-drain handler resurrects with the same
                    // argv via Mux::spawn_pane.
                    // Close (default): unchanged kettle behavior.
                    TermEvent::Exit | TermEvent::ChildExit(_) => match self.cfg.exit_action {
                        // Cycle 912 (audit): keep the dead shell on screen. Set
                        // `held` so `reap()` skips this child-exited pane until
                        // the user explicitly closes it — the previous empty arm
                        // let reap remove it anyway, so Hold behaved like Close.
                        kettle_config::ExitAction::Hold => pane.held = true,
                        kettle_config::ExitAction::Restart => {
                            // Cycle 418: queue for post-drain
                            // respawn via Mux::new_tab_with. The
                            // dead pane closes here; the cycle-418
                            // handler spawns a fresh shell with
                            // the same argv + cwd in a new tab.
                            //
                            // Cycle 452: alacritty_terminal v0.26
                            // fires BOTH `Event::ChildExit(status)`
                            // (event_loop.rs:263, only when status
                            // is Some) and `Event::Exit` (term/
                            // mod.rs:810, unconditionally via
                            // terminal.lock().exit()) for the same
                            // shell exit. Normal exits hit both;
                            // signal exits hit only the second.
                            // Without the dedup contains-check,
                            // normal exits spawned TWO new tabs
                            // per dead shell. pane.closed = true
                            // is idempotent so the second event
                            // just no-ops on it.
                            if !pending_restarts_local.contains(&pane_id) {
                                pending_restarts_local.push(pane_id);
                                log::info!(
                                    "exit-action = restart: queued pane {pane_id} for respawn"
                                );
                            }
                            pane.closed = true;
                        }
                        kettle_config::ExitAction::Close => pane.closed = true,
                    },
                    _ => {}
                }
            }
            // Cycle 612 / 929: drain OSC 133 D (CommandEnd) events ONCE here and
            // defer processing to after the pane loop, where the App can fan a
            // single drain out to the command-notify, the run_command
            // correlator, and event subscribers (each consumer would otherwise
            // race for the destructive drain). Always drain (not gated on the
            // notify threshold) so a pending run_command still resolves when
            // notifications are off.
            for ev in pane.term.drain_command_finished_events() {
                command_finished_local.push((pane_id, ev));
            }
        }
        if bell {
            if self.cfg.bell.visual() {
                ws.last_bell = Some(std::time::Instant::now());
            }
            if self.cfg.bell.attention()
                && !ws.window_focused
                && let Some(w) = &ws.window
            {
                w.request_user_attention(Some(UserAttentionType::Informational));
                ws.attention_active = true;
            }
        }
        // Cycle 246: latch any per-pane bells onto their tab's
        // activity flag so the tab-bar dot survives even on tabs the
        // user isn't currently looking at. Active-tab bells were
        // already handled visually (`last_bell` above triggers the
        // visual-bell flash); the latching helper skips the active
        // tab so we don't double-signal.
        //
        // Cycle 367 (Terminator plugin parity, plugin sub-cycle 5):
        // fire LuaEvent::Bell(pane_id) for every belled pane after
        // the kettle-side bell handling is done. Callbacks may queue
        // LuaCommands (kettle.notify, kettle.send_text); they get
        // drained at the next App tick — same as the cycle-366
        // startup flow.
        // Cycle 427: route Bell + Output event drains through the
        // cycle-426 drain_lua_hook_commands helper so all 4 hook
        // event drains (TabAdd, TabClose, Bell, Output) share the
        // same canonical LuaCommand-match path.
        for id in bell_panes {
            ws.mux.touch_tab_bell(id);
            if let Some(eng) = &self.lua_engine {
                eng.fire_event(&crate::LuaEvent::Bell(id));
            }
            self.drain_lua_hook_commands("bell hook");
        }
        // Cycle 378 (Terminator plugin parity, plugin sub-cycle 3):
        // fire LuaEvent::Output(pane_id, bytes) for each pane that
        // accumulated PTY-output chunks this drain pass.
        for (pane_id, bytes) in output_chunks {
            // Cycle 875: tee PTY output into the asciicast trace (borrow before
            // the bytes move into the Lua event below). All panes feed one
            // stream — fine for the common single-pane recording.
            #[cfg(feature = "dev-record")]
            if let Some(rec) = self.recorder.as_mut() {
                rec.record_output(&bytes);
            }
            if let Some(eng) = &self.lua_engine {
                eng.fire_event(&crate::LuaEvent::Output(pane_id, bytes));
            }
            self.drain_lua_hook_commands("output hook");
        }
        // Cycle 929 (agent-first A2): fan out the OSC-133 CommandFinished events
        // drained above — command-notify (existing behavior), the run_command
        // correlator, and event subscribers all from this single place.
        for (pane_id, ev) in command_finished_local {
            // (a) Desktop notification for a long background command.
            if self.cfg.command_notify_threshold_ms > 0 {
                let elapsed_ms = ev.duration.as_millis() as u64;
                if !ws.window_focused && elapsed_ms >= self.cfg.command_notify_threshold_ms {
                    let secs = ev.duration.as_secs();
                    let exit_text = match ev.exit_code {
                        Some(0) => "✓ ok".to_string(),
                        Some(code) => format!("✗ exit {code}"),
                        None => String::new(),
                    };
                    let body = if exit_text.is_empty() {
                        format!("pane {pane_id} command ran for {secs}s")
                    } else {
                        format!("pane {pane_id} • {secs}s • {exit_text}")
                    };
                    fire_notify("kettle: command finished", &body);
                }
            }
            // (b) Resolve a pending run_command for this pane.
            self.resolve_pending_run(ws, pane_id, &ev);
            // (c) Notify event subscribers.
            self.ctl_broadcast(
                "command_finished",
                Some(pane_id),
                serde_json::json!({
                    "exit_code": ev.exit_code,
                    "duration_ms": ev.duration.as_millis() as u64,
                }),
            );
        }
        // Cycle 412: stash the per-tick restart list on App so the
        // post-drain handler can process it with a fresh
        // &mut ws.mux borrow (the drain_events loop above held a
        // &mut iter into ws.mux.panes, so spawn_pane couldn't run
        // there).
        if !pending_restarts_local.is_empty() {
            ws.pending_pane_restarts.extend(pending_restarts_local);
        }
    }

    /// Cycle 908 (dev-record completeness): tee any PTY output still queued in
    /// the recorder output sidechannels into the trace, right now. The recorder
    /// is otherwise fed ONLY by `drain_events()` (on redraw), so output that
    /// lands after the last redraw-drain and before a pane is reaped or the
    /// window closes — a fast `-e cmd`'s final line, or bytes in flight when the
    /// user clicks X — would be dropped with the pane: the sidechannel is
    /// unbounded + lossless so it accumulates, but it's never read once the
    /// pane is gone. Call this immediately before `mux.reap()` and on close so
    /// the trace keeps its tail. Events batch through the recorder's BufWriter
    /// (~250ms interval flush); clean close paths flush via `finish()`. No-op
    /// when not recording. Verified by `dev-record`-feature test
    /// `recorder_captures_fast_command_tail` + the live close-path tests.
    #[cfg(feature = "dev-record")]
    fn flush_recorder_output(&mut self, ws: &mut WindowState) {
        if self.recorder.is_none() {
            return;
        }
        // Always drain what's queued right now (cheap, non-blocking).
        self.drain_recorder_output_once(ws);
        // A pane marked `closed` (its shell exited under exit-action = close /
        // restart) is about to be reaped + its sidechannel dropped. The PTY
        // reader may still be teeing the shell's FINAL output: the child-exit
        // signal can beat the reader's last write, so a sub-frame-lifetime
        // session (e.g. `-e cmd /c echo x`) would lose its output entirely.
        // Give the reader a brief BOUNDED window to finish, draining as we go.
        // Bounded to ~60 ms worst case and gated on a closed pane being present,
        // so steady-state frames (and exit-action = hold dead panes, which keep
        // `closed = false`) pay nothing. Early-out after 3 idle rounds.
        let mut idle = 0u8;
        let mut rounds = 0u8;
        while rounds < 30
            && ws
                .mux
                .panes
                .values()
                .any(|p| p.closed && p.output_rx.is_some())
        {
            std::thread::sleep(std::time::Duration::from_millis(2));
            if self.drain_recorder_output_once(ws) {
                idle = 0;
            } else {
                idle += 1;
                if idle >= 3 {
                    break;
                }
            }
            rounds += 1;
        }
    }

    /// Drain every pane's recorder output sidechannel into the trace once.
    /// Returns whether any bytes were captured. (Events batch through the
    /// Recorder's BufWriter with a ~250ms interval flush; clean close paths
    /// flush via `finish()`.)
    #[cfg(feature = "dev-record")]
    fn drain_recorder_output_once(&mut self, ws: &mut WindowState) -> bool {
        // Collect first (immutable borrow of the panes), then feed the recorder
        // (mutable borrow of self.recorder) — two sequential borrows.
        let mut tail: Vec<Vec<u8>> = Vec::new();
        for pane in ws.mux.panes.values() {
            if let Some(rx) = &pane.output_rx {
                let mut combined: Vec<u8> = Vec::new();
                while let Ok(chunk) = rx.try_recv() {
                    combined.extend_from_slice(&chunk);
                }
                if !combined.is_empty() {
                    tail.push(combined);
                }
            }
        }
        let got = !tail.is_empty();
        if let Some(rec) = self.recorder.as_mut() {
            for chunk in &tail {
                rec.record_output(chunk);
            }
        }
        got
    }

    /// Keep the OS window title in sync with the *active* pane's title —
    /// including after tab/focus switches, not only on OSC title events.
    /// Deduped so it isn't a syscall every frame.
    fn sync_window_title(&mut self, ws: &mut WindowState) {
        let pane = ws.mux.active_focus().and_then(|id| ws.mux.panes.get(&id));
        let title = pane.map(|p| p.title.as_str()).unwrap_or("kettle");
        let cwd = pane.and_then(|p| p.term.current_dir()).unwrap_or_default();
        let tab = ws.mux.active + 1;
        let want = window_title(&self.cfg.window_title_format, title, &cwd, tab);
        // Cycle 876: an always-visible recording indicator in the title bar so
        // the dev recorder is never silently capturing.
        #[cfg(feature = "dev-record")]
        let want = if self.recorder.is_some() {
            format!("{want}  ● REC")
        } else {
            want
        };
        if want != ws.last_title {
            if let Some(w) = &ws.window {
                w.set_title(&want);
            }
            ws.last_title = want;
        }
    }

    fn update_search(&mut self, ws: &mut WindowState) {
        if !ws.mux.search.open {
            // Clear the scan cache so the next open re-scans from scratch.
            ws.search_scan_key = None;
            return;
        }
        let query = ws.mux.search.query.clone();
        // Cycle 803: re-run the (potentially full-scrollback) regex scan only
        // when something that affects the match set changed — the query, the
        // focused pane, or that tab's last-output instant (new text could add
        // or remove matches). Match *navigation* (n/N changes `index`) and the
        // follow-to-match scroll below still run every call, so only the
        // expensive scan is skipped on an idle frame.
        let scan_key = (
            query.clone(),
            self.focus_key(ws),
            ws.mux
                .tabs
                .get(ws.mux.active)
                .and_then(|t| t.last_output_at),
        );
        if ws.search_scan_key.as_ref() != Some(&scan_key) {
            let matches = if let Some(p) = ws.mux.focused() {
                p.term
                    .term
                    .lock()
                    .ok()
                    .map(|t| {
                        kettle_core::search_with(
                            &t,
                            &query,
                            map_case_sensitivity(self.cfg.search_case_sensitive),
                        )
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let s = &mut ws.mux.search;
            s.matches = matches;
            if s.index >= s.matches.len() {
                s.index = 0;
            }
            ws.search_scan_key = Some(scan_key);
        }
        // Follow the active match into scrollback when it (or the query)
        // changed — once, so the user can still wheel-scroll freely.
        let active = {
            let s = &ws.mux.search;
            s.matches
                .get(s.index)
                .copied()
                .map(|m| ((s.query.clone(), s.index), m.line))
        };
        if let Some((key, line)) = active
            && ws.search_revealed.as_ref() != Some(&key)
        {
            if let Some(p) = ws.mux.focused()
                && let Ok(mut t) = p.term.term.lock()
            {
                use kettle_core::Dimensions;
                let g = t.grid();
                let (hist, off, rows) = (g.history_size(), g.display_offset(), g.screen_lines());
                let want = kettle_core::search::reveal_offset(line, off, hist, rows);
                if want != off {
                    t.scroll_display(kettle_core::Scroll::Delta(want as i32 - off as i32));
                }
            }
            ws.search_revealed = Some(key);
        }
    }

    /// `(row, col)` of the mouse within the focused pane, if any.
    ///
    /// Clamped to the focused pane's live grid: a click in the right/bottom
    /// padding (the rect rounds up past an exact cell multiple) must report the
    /// LAST cell, never one past the edge. A mouse-tracking app that sees
    /// `col == cols` or `row == rows` mis-renders (cycle 842, audit) — xterm
    /// itself clamps the reported coordinate to the window.
    fn cursor_cell(&self, ws: &WindowState) -> Option<(usize, usize)> {
        let rect = self.focused_rect(ws, self.area(ws))?;
        // Mouse-tracking reports a cell, not a selection boundary — the sub-cell
        // side is irrelevant here, so discard it.
        let (p, _) = self.px_to_point(ws, rect, ws.cursor.x as f32, ws.cursor.y as f32);
        let (row, col) = (p.line.0.max(0) as usize, p.column.0);
        // Clamp to the pane's geometric grid (same cell size AND titlebar inset
        // `px_to_point` used): a click in the right/bottom padding rounds up to
        // `cols`/`rows`, one past the edge, which a mouse-tracking app
        // mis-renders. Cycle 916 (file-by-file audit): clamp against the INSET
        // grid (the size the split pane's PTY was actually given by resize_all) —
        // the zero-inset `grid_of` left the row ceiling ~1 too high in a
        // titlebar'd split, so a bottom-edge click reported one row past the
        // PTY's last valid row to mouse-tracking TUIs.
        let titlebar_h =
            self.pane_titlebar_inset(ws, ws.mux.layout(ws.mux.active, self.area(ws)).len());
        let (cols, rows) = self.grid_of_inset(ws, rect, titlebar_h);
        Some((
            row.min(rows.saturating_sub(1)),
            col.min(cols.saturating_sub(1)),
        ))
    }

    /// Scan the focused pane's visible grid for quick-select targets and
    /// assign each a short label.
    fn collect_hints(&mut self, ws: &mut WindowState) -> Vec<HintTarget> {
        use kettle_core::hints;
        use kettle_core::{Column, Dimensions, Line, Point};
        let Some(p) = ws.mux.focused() else {
            return Vec::new();
        };
        let Ok(t) = p.term.term.lock() else {
            return Vec::new();
        };
        let g = t.grid();
        let (rows, cols) = (g.screen_lines(), g.columns());
        // Cycle 912 (R1 completion): convert each viewport row to its grid-
        // absolute line so hint detection scans the VISIBLE rows (incl. history
        // when scrolled back), not the active screen — otherwise a quick-select
        // label drawn over a visible URL would open the active-screen URL at the
        // same index. `HintTarget.row` stays viewport-relative for label placement.
        let off = g.display_offset();
        let lines: Vec<String> = (0..rows)
            .map(|r| {
                let base = viewport_point_to_grid(Point::new(Line(r as i32), Column(0)), off);
                let s: String = (0..cols)
                    .map(|c| g[Point::new(base.line, Column(c))].c)
                    .collect();
                s.trim_end().to_string()
            })
            .collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let spans = hints::detect(&refs);
        let labels = hints::labels(spans.len(), hints::ALPHABET);
        spans
            .into_iter()
            .zip(labels)
            .map(|(s, label)| HintTarget {
                row: s.row,
                col: s.start,
                label,
                kind: s.kind,
                text: s.text,
            })
            .collect()
    }

    fn link_at_cursor<'a>(&self, ws: &'a WindowState) -> Option<&'a kettle_core::Link> {
        let (row, col) = self.cursor_cell(ws)?;
        ws.links
            .iter()
            .find(|l| l.row == row && col >= l.start_col && col <= l.end_col)
    }

    fn focused_mode(&mut self, ws: &mut WindowState) -> kettle_core::TermMode {
        ws.mux
            .focused()
            .and_then(|p| p.term.term.lock().ok().map(|t| *t.mode()))
            .unwrap_or(kettle_core::TermMode::empty())
    }

    /// Forward a mouse event to the app via the active tracking protocol.
    /// Returns `true` when it was consumed (so kettle skips local handling).
    /// Whether a mouse event at `cur` should be reported to a tracking app,
    /// given the `last` reported cell. Motion (1002/1003) coalesces to cell
    /// crossings; a press/release (`motion == false`) always reports. Pure so
    /// the coalescing rule is unit-tested without a live PTY (cycle 842).
    fn motion_should_report(
        motion: bool,
        last: Option<(usize, usize)>,
        cur: (usize, usize),
    ) -> bool {
        !(motion && last == Some(cur))
    }

    fn send_mouse(&mut self, ws: &mut WindowState, btn: u8, pressed: bool, motion: bool) -> bool {
        // Shift held = "bypass mouse tracking, let kettle handle this
        // locally" — the xterm convention every modern terminal honors.
        // Without it, running htop/vim/tmux with mouse-mode locks out
        // kettle's selection entirely: every click is consumed by the
        // TUI and the user has to disable mouse mode to copy text.
        // Returning `false` here makes the caller fall through to
        // selection / scrollbar / extend logic exactly as if tracking
        // were off.
        if ws.mods.shift_key() {
            return false;
        }
        // Cycle 942 (audit, VTE input-enabled parity): a read-only pane gets
        // no mouse-tracking reports either. Returning false falls through to
        // kettle-local handling, so selection / scrollback still work for the
        // user — the same degradation VTE applies when input is disabled.
        if ws.mux.focused().is_some_and(|p| p.read_only) {
            return false;
        }
        let (track, sgr) = input::mouse_tracking(self.focused_mode(ws));
        if track == input::MouseTracking::Off {
            return false;
        }
        if motion && track != input::MouseTracking::Motion && ws.mouse_btn.is_none() {
            return track != input::MouseTracking::Off; // consume, no report
        }
        let Some((row, col)) = self.cursor_cell(ws) else {
            return false;
        };
        // Cell-motion coalescing: a drag that stays inside one cell must not
        // re-report. xterm fires a 1002/1003 motion event only when the
        // pointer crosses into a new cell; without this a fast drag emits one
        // SGR report per pixel of travel, flooding the TUI (cycle 842, audit).
        // Press/release always report (the guard is motion-only) and refresh
        // the baseline, so the next motion is compared against the right cell.
        if !Self::motion_should_report(motion, ws.last_mouse_cell, (row, col)) {
            return true;
        }
        let seq = input::mouse_encode(sgr, btn, pressed, motion, col, row, ws.mods);
        if let Some(p) = ws.mux.focused() {
            p.term.write(&seq);
        }
        ws.last_mouse_cell = Some((row, col));
        true
    }

    fn overlay(&self, ws: &WindowState) -> Overlay {
        let hover = self.cursor_cell(ws);
        let links = ws
            .links
            .iter()
            .map(|l| kettle_render::LinkRect {
                col: l.start_col,
                row: l.row,
                width: (l.end_col + 1).saturating_sub(l.start_col).max(1),
                hover: hover
                    .map(|(r, c)| r == l.row && c >= l.start_col && c <= l.end_col)
                    .unwrap_or(false),
            })
            .collect();

        let (ssh_query, ssh_hint) = match &ws.ssh_input {
            Some(q) => {
                let hint = if self.cfg.ssh_hosts.is_empty() {
                    "(type user@host)".to_string()
                } else {
                    let names: Vec<&str> =
                        self.cfg.ssh_hosts.iter().map(|(n, _)| n.as_str()).collect();
                    format!("hosts: {}", names.join(", "))
                };
                (Some(q.clone()), hint)
            }
            None => (None, String::new()),
        };

        let (palette_query, palette_hint) = match &ws.palette_input {
            Some((q, sel)) => {
                let cmds = kettle_config::palette::commands();
                let ranked = kettle_config::palette::rank(q, &cmds);
                let hint = if ranked.is_empty() {
                    "(no matching command)".to_string()
                } else {
                    let sel = (*sel).min(ranked.len() - 1);
                    let start = if sel < 8 { 0 } else { sel - 7 };
                    ranked
                        .iter()
                        .enumerate()
                        .skip(start)
                        .take(8)
                        .map(|(i, &ci)| {
                            let l = cmds[ci].0;
                            if i == sel {
                                format!("«{l}»")
                            } else {
                                l.to_string()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("  ·  ")
                };
                (Some(q.clone()), hint)
            }
            None => (None, String::new()),
        };

        // Cycle 708 (Terminator parity, layoutlauncher.py):
        // compute the layout-picker overlay's query + hint
        // string the same way as the command palette. Empty
        // layouts dir is fine — the hint reads `(no saved
        // layouts; run kettle --save-layout NAME)`.
        let (layout_picker_query, layout_picker_hint) = match &ws.layout_picker_input {
            Some((q, sel)) => {
                let layouts = crate::session::Session::list_layouts();
                let ranked = rank_layouts(q, &layouts);
                let hint = if layouts.is_empty() {
                    "(no saved layouts; run `kettle --save-layout NAME`)".to_string()
                } else if ranked.is_empty() {
                    "(no matching layout)".to_string()
                } else {
                    let sel = (*sel).min(ranked.len() - 1);
                    let start = if sel < 8 { 0 } else { sel - 7 };
                    ranked
                        .iter()
                        .enumerate()
                        .skip(start)
                        .take(8)
                        .map(|(i, &li)| {
                            let l = &layouts[li];
                            if i == sel {
                                format!("«{l}»")
                            } else {
                                l.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("  ·  ")
                };
                (Some(q.clone()), hint)
            }
            None => (None, String::new()),
        };

        let hint_labels: Vec<HintLabel> = match &ws.hint_state {
            Some((targets, typed)) => targets
                .iter()
                .map(|t| HintLabel {
                    row: t.row,
                    col: t.col,
                    label: t.label.clone(),
                    dim: !typed.is_empty() && !t.label.starts_with(typed.as_str()),
                })
                .collect(),
            None => Vec::new(),
        };

        let window_focused = ws.window_focused;
        // Cursor blink is the *intersection* of the user config and the
        // running app's wishes — programs flip it via DEC private mode 12
        // (`CSI ?12 h/l`), which the engine tracks per-pane on its
        // `cursor_style().blinking`. Read the active pane's live state so
        // editors like vim that disable blink for their own cursor are
        // honored even when the global config wants blink. (Goes through
        // `active_focus` + `panes.get` so the `overlay()` builder stays a
        // pure `&self` reader.)
        let pane_blink = ws
            .mux
            .active_focus()
            .and_then(|id| ws.mux.panes.get(&id))
            .map(|p| p.term.cursor_blinking())
            .unwrap_or(true);
        let blink_enabled = self.cfg.cursor_blink && pane_blink;
        let cursor_visible = if !blink_enabled
            || !window_focused
            || ws.ssh_input.is_some()
            || ws.palette_input.is_some()
            || ws.layout_picker_input.is_some()
            || ws.hint_state.is_some()
            || ws.mux.search.open
            // Cycle 763: the title-edit and confirm-dialog input bars are also
            // active text surfaces — keep the cursor steady (not mid-blink-off)
            // while the user is typing/navigating them, like the other modals.
            || ws.editing_title.is_some()
            || ws.confirm_dialog.is_some()
            || ws.settings_nav.is_some()
        {
            true
        } else {
            ws.blink_on
        };
        let bell = ws
            .last_bell
            .map(|t| {
                let e = t.elapsed().as_secs_f32();
                if e >= 0.30 { 0.0 } else { 1.0 - e / 0.30 }
            })
            .unwrap_or(0.0);
        // v2.20.0: the transient resize chip (about_to_wait drives the
        // expiry repaint and clears the state).
        let resize_overlay = ws
            .resize_overlay
            .and_then(|(c, r, t)| (t.elapsed() < RESIZE_OVERLAY_DURATION).then_some((c, r)));

        let context_menu = self.context_menu_overlay(ws);
        // Cycle 372: marshal the in-progress Edit-title state for
        // the render layer so the user sees what they're typing.
        //
        // Cycle 395 (Terminator parity, titlebar Bucket-D sub-cycle 7):
        // for Pane scope, also pass the focused pane's titlebar y so
        // the overlay anchors near the clicked pane vs the window-
        // bottom (window/tab scopes still use window-bottom).
        let edit_title: Option<(String, String, Option<f32>)> =
            ws.editing_title.as_ref().map(|s| {
                let label = match s.scope {
                    TitleEditScope::Window => "Edit window title:",
                    TitleEditScope::Tab => "Edit tab title:",
                    TitleEditScope::Pane => "Edit pane title:",
                    TitleEditScope::Group => "Edit pane group:",
                };
                let anchor_y = if matches!(s.scope, TitleEditScope::Pane | TitleEditScope::Group) {
                    let area = self.area(ws);
                    let active = ws.mux.active;
                    let rects = ws.mux.layout(active, area);
                    let focus = ws.mux.active_focus();
                    rects
                        .iter()
                        .find(|(id, _)| Some(*id) == focus)
                        .map(|(_, (_, ry, _, rh))| {
                            // Anchor just below the focused pane's titlebar.
                            // Falls below the bar (top mode) or just above
                            // the bottom bar (bottom mode); either way the
                            // user's eye-line stays near where they clicked.
                            if self.cfg.title_at_bottom {
                                *ry + *rh - 60.0
                            } else {
                                *ry + 30.0
                            }
                        })
                } else {
                    None
                };
                (label.to_string(), s.input.clone(), anchor_y)
            });
        // Cycle 660: project the App's confirm_dialog into the
        // renderer's projection (so it shows even when no
        // search is open — confirm modals are independent).
        let confirm_dialog_early =
            ws.confirm_dialog
                .as_ref()
                .map(|d| kettle_render::ConfirmDialogOverlay {
                    prompt: d.prompt.clone(),
                    buttons: d
                        .buttons
                        .iter()
                        .map(|b| match b {
                            ConfirmButton::Cancel => kettle_render::ConfirmDialogButton {
                                label: "Cancel".to_string(),
                                destructive: false,
                            },
                            ConfirmButton::Confirm { label, destructive } => {
                                kettle_render::ConfirmDialogButton {
                                    label: label.clone(),
                                    destructive: *destructive,
                                }
                            }
                        })
                        .collect(),
                    focus_idx: d.focus_idx,
                });
        // Cycle 756: project the settings overlay (independent of search, like
        // the confirm dialog). Values are read from the live Config so the
        // panel reflects the current state (incl. external reloads).
        let settings_overlay = self.settings_overlay_projection(ws);
        let s = &ws.mux.search;
        if !s.open {
            return Overlay {
                links,
                ssh_query,
                ssh_hint,
                palette_query,
                palette_hint,
                layout_picker_query,
                layout_picker_hint,
                edit_title,
                hint_labels,
                window_focused,
                cursor_visible,
                bell,
                resize_overlay,
                context_menu,
                confirm_dialog: confirm_dialog_early,
                settings: settings_overlay,
                update_available: self.update_available.clone(),
                ..Overlay::default()
            };
        }
        let highlights = s
            .matches
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                if m.line < 0 {
                    return None;
                }
                Some(HighlightRect {
                    col: m.start_col,
                    row: m.line as usize,
                    width: (m.end_col + 1).saturating_sub(m.start_col).max(1),
                    active: i == s.index,
                })
            })
            .collect();
        let confirm_dialog = confirm_dialog_early;
        Overlay {
            search_query: Some(s.query.clone()),
            search_count: s.matches.len(),
            search_index: s.index,
            highlights,
            links,
            ssh_query,
            ssh_hint,
            palette_query,
            palette_hint,
            layout_picker_query,
            layout_picker_hint,
            edit_title,
            hint_labels,
            window_focused,
            cursor_visible,
            bell,
            resize_overlay,
            context_menu,
            vi_cursor: ws.vi_mode.map(|v| (v.row, v.col)),
            vi_visual_anchor: ws.vi_mode.and_then(|v| v.visual_anchor),
            confirm_dialog,
            settings: settings_overlay,
            update_available: self.update_available.clone(),
        }
    }

    fn update_links(&mut self, ws: &mut WindowState) {
        // Cycle 803: build a cheap key (focus + tab output instant + scroll
        // offset) and skip the viewport URL re-scan when the visible content
        // can't have changed. The brief lock to read display_offset is cheap;
        // the avoided work is `kettle_core::links`' per-cell regex pass.
        let key = {
            // v2.20.0 (review fix): the FOCUSED pane's output generation (a
            // cheap atomic read) — the old `last_output_at` component was
            // never updated for the ACTIVE tab (the activity latch skips
            // it), so active-tab output left `ws.links` stale until a
            // scroll/focus change.
            let out_gen = ws.mux.focused().map(|p| p.term.output_generation());
            let off = ws
                .mux
                .focused()
                .and_then(|p| p.term.term.lock().ok().map(|t| t.grid().display_offset()));
            (self.focus_key(ws), out_gen, off)
        };
        if ws.links_scan_key.as_ref() == Some(&key) {
            return;
        }
        // v2.20.0 P6: during streaming, `last_output_at` moves with every
        // read, so the cycle-803 key misses on EVERY painted frame and the
        // per-cell regex pass ran at up to 60/s. Debounce output-only
        // changes; focus / scroll changes still rescan immediately (there
        // the viewport jumped — stale link rects would be visibly wrong).
        // While debounced, `links_scan_key` is left at the old value so the
        // next painted frame re-evaluates; any interaction that could USE a
        // link (mouse move for hover, key for hint mode) repaints first, so
        // a post-stream stale window can't be observed by the user.
        if let (Some(prev), Some(at)) = (ws.links_scan_key.as_ref(), ws.last_links_scan) {
            let output_only = prev.0 == key.0 && prev.2 == key.2 && prev.1 != key.1;
            if output_only && at.elapsed() < LINKS_SCAN_DEBOUNCE {
                return;
            }
        }
        ws.links = ws
            .mux
            .focused()
            .and_then(|p| p.term.term.lock().ok().map(|t| kettle_core::links(&t)))
            .unwrap_or_default();
        ws.links_scan_key = Some(key);
        ws.last_links_scan = Some(std::time::Instant::now());
    }

    fn redraw(&mut self, ws: &mut WindowState) {
        // v2.21.1 (throughput): is THIS paint flushing coalesced PTY output?
        // Captured before the clear below so the flood detector can count
        // consecutive output-coalesced frames and stretch the paint budget
        // under a sustained flood (see `effective_output_budget`).
        let was_coalescing_paint = ws.coalescing_paint;
        // B (Peacock): resolve/refresh this window's accent claim (cheap in
        // the steady state; full pool walk only on first frame/theme switch).
        self.sync_window_accent(ws);
        // C4: record the per-pane output generations this paint consumes —
        // BEFORE drain_events pulls the channels, so output landing during
        // the paint stays "unseen" and re-triggers a wakeup repaint (an
        // extra frame beats a missed one).
        ws.seen_output_gen.clear();
        for (id, p) in &ws.mux.panes {
            ws.seen_output_gen.insert(*id, p.term.output_generation());
        }
        self.drain_events(ws);
        self.poll_remote_contexts(ws);
        self.poll_theme_schedule(ws);
        self.poll_focus_event(ws);
        self.poll_title_event(ws);
        // Cycle 745: reflect the focused pane's OSC 9;4 progress onto the OS
        // taskbar button (pwsh 7 / Windows Terminal parity). No-op off Windows.
        self.poll_taskbar_progress(ws);
        // Cycle 418: process any pane-restart requests queued during
        // drain_events. Done HERE (after drain) so we don't hold a
        // &mut iter into ws.mux.panes when spawning a new tab.
        // event_loop arg is unused for now (the spawn doesn't need it);
        // kept in the signature for symmetry with other dispatchers.
        if !ws.pending_pane_restarts.is_empty() {
            let pane_ids: Vec<u64> = std::mem::take(&mut ws.pending_pane_restarts);
            let (cw, ch) = self.cell_px(ws);
            let waker = self.waker();
            // Cycle 420: use the live grid (matches the existing surface)
            // for the new tab. cycle-418 hardcoded 80×24 which mismatched
            // any non-default kettle window size — the new shell would
            // start with a tiny grid then grow on next resize. Pulling
            // from the current area means the restart shell starts at
            // the size the user is actually using.
            let (cols, rows) = self.grid_of(ws, self.area(ws));
            for pane_id in pane_ids {
                let restart_info: Option<(Vec<String>, Option<String>)> = ws
                    .mux
                    .panes
                    .get(&pane_id)
                    .map(|p| (p.argv.clone(), p.term.current_dir()));
                if let Some((argv, cwd)) = restart_info {
                    if let Err(e) = ws.mux.new_tab_with(
                        &self.cfg,
                        cols,
                        rows,
                        cw,
                        ch,
                        waker.clone(),
                        &argv,
                        cwd.as_deref(),
                    ) {
                        log::warn!("exit-action = restart: spawn failed for pane {pane_id}: {e}");
                    } else {
                        // Cycle 425: respawned tab is a fresh tab
                        // from the plugin's POV; fire TabAdd.
                        self.fire_tab_add_event(ws);
                    }
                }
            }
        }
        // Reflect the active pane (incl. after tab/focus switches).
        self.sync_window_title(ws);
        // Advance the cursor blink phase (configurable half-period). Skip
        // the increment when the active pane has DEC mode 12 cleared so the
        // cursor sits solid — without this, vim-style "solid block while
        // editing" requests are ignored even though the engine honored them.
        let pane_blink_redraw = ws
            .mux
            .active_focus()
            .and_then(|id| ws.mux.panes.get(&id))
            .map(|p| p.term.cursor_blinking())
            .unwrap_or(true);
        if self.cfg.cursor_blink
            && pane_blink_redraw
            && ws.window_focused
            && ws.last_blink.elapsed()
                >= std::time::Duration::from_millis(self.cfg.cursor_blink_interval)
        {
            ws.blink_on = !ws.blink_on;
            ws.last_blink = std::time::Instant::now();
        }
        // Cycle 908: capture a just-exited pane's final output before reap drops
        // its sidechannel — otherwise the shell's last line is lost from the trace.
        #[cfg(feature = "dev-record")]
        self.flush_recorder_output(ws);
        if ws.mux.reap() {
            return;
        }
        // scroll-on-output: if new output landed in any pane since the
        // previous frame, optionally yank that pane back to the bottom.
        // Tracking is per-pane (each one drifts independently when only
        // its background process emits) and uses the pure
        // `should_scroll_on_output` rule so the "what counts as new
        // output" decision lives outside the render path.
        let want_sob = self.cfg.scroll_on_output;
        // Cycle 246: track which panes produced output this frame so
        // we can latch their tab's `last_output_at`. Collected here
        // and dispatched after the borrow ends — same shape as the
        // `bell_panes` collection in `drain_events`.
        let mut output_panes: Vec<u64> = Vec::new();
        for (&pane_id, pane) in ws.mux.panes.iter_mut() {
            let now = pane
                .term
                .term
                .lock()
                .ok()
                .map(|t| {
                    use kettle_core::Dimensions;
                    t.grid().history_size()
                })
                .unwrap_or(0);
            let advanced = match pane.last_history {
                Some(prev) => now > prev,
                None => false,
            };
            if advanced {
                output_panes.push(pane_id);
            }
            if kettle_core::scrollbar::should_scroll_on_output(want_sob, pane.last_history, now)
                && let Ok(mut t) = pane.term.term.lock()
            {
                t.scroll_display(Scroll::Bottom);
            }
            pane.last_history = Some(now);
        }
        for id in output_panes {
            ws.mux.touch_tab_output(id);
        }
        // Auto-scroll while dragging a selection past the focused pane's
        // top/bottom edge — every modern terminal does this so the user
        // doesn't have to release / scroll-back / shift-click to extend.
        // Pure `selection_autoscroll_lines` chooses the per-frame rate;
        // scrolling the viewport here naturally re-fires `update_selection`
        // below to anchor the selection's end to the new visible line.
        if ws.selecting {
            let area = self.area(ws);
            if let Some(rect) = self.focused_rect(ws, area) {
                let lines = selection_autoscroll_lines(ws.cursor.y as f32, rect.1, rect.1 + rect.3);
                if lines != 0
                    && let Some(p) = ws.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    t.scroll_display(Scroll::Delta(lines));
                }
                if lines != 0 {
                    // Re-anchor the selection's end at the (now-moved)
                    // cursor row so the highlight grows in step with the
                    // scroll, not stuck on the original click-time row.
                    self.update_selection(ws, area);
                }
            }
        }
        self.update_search(ws);
        self.update_links(ws);
        // Link set may have changed (scroll, output, mode flip) — re-sync
        // the cursor icon so a URL scrolling out from under a held Ctrl
        // doesn't leave the pointer-hand icon stuck on a now-empty cell.
        // Deduped via `last_cursor_icon` so this is a cheap per-frame
        // recheck when nothing changed.
        self.sync_cursor_icon(ws);
        let overlay = self.overlay(ws);
        let area = self.area(ws);
        let tabbar = self.tab_bar(ws);
        // Cycle 296: build status bar BEFORE the &mut renderer borrow
        // since `build_status_bar` reads ws.mux / self.cfg
        // immutably.
        let status = self.build_status_bar(ws);
        let active = ws.mux.active;
        let layout = ws.mux.layout(active, area);
        let focus = ws.mux.active_focus();

        let Some(renderer) = ws.renderer.as_mut() else {
            return;
        };

        // v2.20.0 P2 (perf): capture each visible pane's renderable state into
        // a pooled snapshot UNDER the Term lock, then drop the guard
        // immediately — a µs-scale flat copy per pane. The renderer works from
        // the snapshots, so the GPU frame (shaping + surface-acquire + submit
        // + present, milliseconds) no longer serializes the PTY reader
        // threads. Before this, `redraw` held EVERY pane's Term mutex across
        // the whole frame; under output flood (frames at the 16ms coalescer
        // budget) the parser starved on `term.lock()` nearly continuously —
        // the v2.19.0 baseline measured 0.42–0.8 MB/s throughput vs 3–9 MB/s
        // for WT / Alacritty / WezTerm on the identical harness.
        // Cycle 382: also pass the pane's title so the cycle-379
        // titlebar can render the text.
        let mut snaps = std::mem::take(&mut ws.pane_snapshots);
        let mut metas = Vec::with_capacity(layout.len());
        for (id, r) in &layout {
            if let Some(p) = ws.mux.panes.get(id) {
                let mut imgs = p.term.placements();
                imgs.extend(p.term.placeholder_tiles());
                imgs.extend(p.term.relative_tiles());
                if let Ok(g) = p.term.term.lock() {
                    let si = metas.len();
                    if snaps.len() <= si {
                        snaps.push(PaneSnapshot::default());
                    }
                    snaps[si].capture(&g);
                    drop(g); // lock released — the render below is lock-free
                    metas.push((
                        *id,
                        *r,
                        Some(*id) == focus,
                        imgs,
                        compose_pane_title(
                            &self.cfg.agent_badge,
                            p.agent_attached,
                            p.read_only,
                            &p.title,
                        ),
                        snaps[si].columns as u16,
                        snaps[si].screen_lines as u16,
                        false,
                        p.group_name.clone(),
                    ));
                }
            }
        }
        snaps.truncate(metas.len());
        // Cycle 852 (audit): hand the renderer borrows into `metas` (which lives
        // for the whole frame) instead of a second per-pane clone of the images
        // Vec / title String / group_name. `snap` borrows the pooled snapshot
        // the same way; both outlive `panes`, which drops before the pool
        // returns to `ws.pane_snapshots`.
        let panes: Vec<PaneView> = metas
            .iter()
            .zip(snaps.iter())
            .map(
                |((id, r, f, imgs, title, cols, rows, bell, group_name), snap)| PaneView {
                    id: *id,
                    rect: *r,
                    snap,
                    focused: *f,
                    images: imgs.as_slice(),
                    title: title.as_str(),
                    size_cols: *cols,
                    size_rows: *rows,
                    bell: *bell,
                    group_name: group_name.as_deref(),
                },
            )
            .collect();
        // Cycle 296: status bar built BEFORE the &mut renderer borrow
        // (the helper reads `ws.mux` immutably). Cheap when off.
        if let Err(e) =
            renderer.render_frame_with_status(&panes, &tabbar, &self.cfg, &overlay, &status)
        {
            log::warn!("render error: {e}");
        }
        // Return the snapshot pool (cell-Vec capacity recycles next frame).
        drop(panes);
        ws.pane_snapshots = snaps;
        // Fallback reveal: normal startup reveals as soon as renderer init
        // succeeds, then paints immediately. Keep this guard so any future path
        // that reaches a rendered frame while still hidden cannot leave a
        // visible-state window invisible forever. No-op after the first reveal
        // and for `window_state = hidden`.
        if !ws.window_shown {
            if let Some(w) = &ws.window {
                w.set_visible(true);
            }
            ws.window_shown = true;
        }
        // Cycle 910 (R2): record the paint time and clear any pending coalesced
        // output paint now that this settled frame is on the surface.
        ws.last_paint = Some(std::time::Instant::now());
        ws.coalescing_paint = false;
        // v2.21.1 (throughput): track sustained-flood depth. A frame that
        // flushed coalesced output (output faster than the budget) bumps the
        // counter; any other paint (idle/blink/input echo, or output slower
        // than the budget) resets it — so a brief burst never throttles and the
        // settled post-flood frame drops back to 60 fps. Drives
        // `effective_output_budget`.
        ws.flood_paints = if was_coalescing_paint {
            ws.flood_paints.saturating_add(1)
        } else {
            0
        };
    }

    /// Cycle 296: compose the status-bar contents (HH:MM:SS · theme ·
    /// focused-pane title). Returns `StatusBar::hidden` when the
    /// config has it off. The renderer's draw is a no-op on a
    /// hidden status bar so this is cheap even when never visible.
    /// Takes `&mut self` only because `Mux::focused` does — no state
    /// is actually mutated here.
    fn build_status_bar(&mut self, ws: &mut WindowState) -> kettle_render::StatusBar {
        if matches!(self.cfg.status_bar, kettle_config::StatusBarMode::Off) {
            return kettle_render::StatusBar::hidden();
        }
        let h = self.status_bar_h(ws);
        let surface_h = ws
            .renderer
            .as_ref()
            .map(|r| r.surface_size().1 as f32)
            .unwrap_or(600.0);
        let y = match self.cfg.status_bar {
            kettle_config::StatusBarMode::Top => 0.0,
            kettle_config::StatusBarMode::Bottom => surface_h - h,
            kettle_config::StatusBarMode::Off => 0.0,
        };
        // Compose text: HH:MM:SS · theme · focused pane title.
        // SystemTime → seconds since UNIX → HH:MM:SS via div/mod, no
        // dep on chrono. The displayed time is UTC by design (a
        // future cycle could honor $TZ — std::time has no built-in
        // local-tz conversion, would need chrono or time crate).
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let day = secs % 86400;
        let (hh, mm, ss) = (day / 3600, (day % 3600) / 60, day % 60);
        let title = ws
            .mux
            .focused()
            .map(|p| p.title.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "kettle".to_string());
        // Cycle 308: cap the title at a character budget so a chatty
        // prompt that puts the full cwd in the window title
        // (e.g. `PROMPT_COMMAND='echo -ne "\033]0;$PWD\007"'`)
        // doesn't overflow the strip's 1-cell height. Pure helper
        // so we can drift-guard it in tests.
        let title_capped = cap_title_for_status_bar(&title, 60);
        let text = format!(
            "{hh:02}:{mm:02}:{ss:02} UTC  ·  {}  ·  {title_capped}",
            self.cfg.theme_name
        );
        kettle_render::StatusBar { height: h, y, text }
    }

    /// Snapshot the focused `(tab, leaf)` pair. Paired with
    /// `note_focus_change` to detect whether an operation moved focus
    /// and, if so, reset the cursor blink phase so the new pane's
    /// cursor is visible immediately (cycle 135 pattern, extracted to
    /// a helper in cycle 136 so the mouse-driven paths can share it).
    fn focus_key(&self, ws: &WindowState) -> (usize, Option<u64>) {
        (ws.mux.active, ws.mux.active_focus())
    }

    /// If the focused `(tab, leaf)` changed since `pre`, land the
    /// cursor visible on the new pane right away.
    fn note_focus_change(&mut self, ws: &mut WindowState, pre: (usize, Option<u64>)) {
        if self.focus_key(ws) != pre {
            self.reset_blink_phase(ws);
            // Cycle 802 (audit): repaint immediately so the focused-pane
            // border and the cursor's solid/hollow state track the new pane.
            // Without this, a focus-follows-mouse (`focus = sloppy`) change
            // left a stale focus border until some *other* event happened to
            // trigger a redraw — the pane under the cursor looked unfocused.
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
    }

    /// Close every modal overlay (search bar, command palette, hint
    /// mode, SSH launcher). Cycle 111's Reset path inlined the same
    /// four-line clear; cycle 154 extracts it so the modal-opening
    /// actions can call it first to avoid stacking two visible
    /// modals at once (palette opened while ssh launcher was up
    /// would render both, with palette capturing keys; visually
    /// confusing).
    fn close_all_modals(&mut self, ws: &mut WindowState) {
        ws.mux.search.open = false;
        ws.palette_input = None;
        ws.settings_nav = None;
        // v2.24.0: also drop any open inline settings text prompt (the image-path
        // editor) so it can't linger after the panel closes / reopens.
        ws.settings_text_edit = None;
        ws.layout_picker_input = None;
        ws.hint_state = None;
        ws.ssh_input = None;
        ws.context_menu = None;
        ws.editing_title = None;
        // Cycle 298 vi-mode behaves like a modal — Esc exits it,
        // close_all_modals exits it. Sub-cycle 1.
        ws.vi_mode = None;
        // Cycle 754: the confirm dialog ("Close this pane?", "Quit?") is a
        // modal too, but was omitted here — so opening search / palette / a
        // menu while a confirm prompt was up rendered BOTH overlays at once
        // with ambiguous key focus. Every modal-opener calls close_all_modals
        // first (then sets its own modal), so clearing the confirm dialog here
        // is safe: the confirm-open path clears-then-sets in that order.
        ws.confirm_dialog = None;
    }

    /// Cycle 369: apply the in-progress title edit + clear the
    /// overlay. The scope decides which setter is invoked.
    fn apply_title_edit(&mut self, ws: &mut WindowState) {
        if let Some(state) = ws.editing_title.take() {
            let value = state.input;
            match state.scope {
                TitleEditScope::Window => {
                    if let Some(w) = &ws.window {
                        w.set_title(&value);
                    }
                    ws.last_title = value;
                }
                TitleEditScope::Tab => {
                    if let Some(t) = ws.mux.tabs.get_mut(ws.mux.active) {
                        t.title_override = if value.is_empty() { None } else { Some(value) };
                    }
                }
                TitleEditScope::Pane => {
                    if let Some(p) = ws.mux.focused() {
                        p.title = value;
                    }
                }
                TitleEditScope::Group => {
                    // Cycle 680: bulk-apply branches on
                    // `state.bulk`. Single = focused pane only
                    // (preserves cycle-407 behavior); Tab/Window
                    // = bulk-assign via Action::GroupTab/Window.
                    let next = if value.is_empty() { None } else { Some(value) };
                    match state.bulk {
                        GroupBulkScope::Single => {
                            if let Some(p) = ws.mux.focused() {
                                p.group_name = next;
                            }
                        }
                        GroupBulkScope::Tab => {
                            let ids: Vec<u64> = ws
                                .mux
                                .tabs
                                .get(ws.mux.active)
                                .map(|t| t.root.leaf_ids())
                                .unwrap_or_default();
                            for id in ids {
                                if let Some(p) = ws.mux.panes.get_mut(&id) {
                                    p.group_name = next.clone();
                                }
                            }
                        }
                        GroupBulkScope::Window => {
                            let ids: Vec<u64> = ws.mux.panes.keys().copied().collect();
                            for id in ids {
                                if let Some(p) = ws.mux.panes.get_mut(&id) {
                                    p.group_name = next.clone();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// `true` while any modal overlay (search bar, command palette, hint
    /// mode, SSH launcher, context menu) is up. Mirrors `close_all_modals`
    /// so the two stay in lock-step — extracted in cycle 161 to drive the
    /// cursor-icon override (the OS arrow, not the I-beam, belongs over
    /// modal chrome) and extended in cycle 245 for the right-click menu.
    fn any_modal_open(&self, ws: &WindowState) -> bool {
        ws.mux.search.open
            || ws.palette_input.is_some()
            || ws.settings_nav.is_some()
            || ws.layout_picker_input.is_some()
            || ws.hint_state.is_some()
            || ws.ssh_input.is_some()
            || ws.context_menu.is_some()
            || ws.editing_title.is_some()
            || ws.vi_mode.is_some()
            // Cycle 754: the confirm dialog is a modal too. Its key input has a
            // dedicated priority branch, but without it here mouse/scroll/cursor
            // gating let clicks fall through to the terminal behind a "Quit?" /
            // "Close pane?" prompt.
            || ws.confirm_dialog.is_some()
    }

    /// Build the right-click context-menu item list. Each `Item`'s
    /// `enabled` flag is computed from current state: Copy needs a
    /// selection; Ungroup needs the focused pane to actually be in a
    /// group. Cycle 713 wraps the whole list in `filter_disabled` at
    /// the `open_context_menu` call-site so disabled rows + the
    /// separators that would orphan them are hidden entirely
    /// (Terminator-style) rather than shown greyed-out — less visual
    /// clutter, every visible row is actionable.
    fn context_menu_items(&mut self, ws: &mut WindowState) -> Vec<ContextMenuItem> {
        let has_selection = ws
            .mux
            .focused()
            .and_then(|p| p.term.term.lock().ok().map(|t| t.selection.is_some()))
            .unwrap_or(false);
        // Cycle 713: only enable Ungroup when the focused pane has a
        // group_name set. Otherwise the row used to greyed-out
        // confuse new users ("why's that here if I can't click it?");
        // now it's filtered out entirely until it's actionable.
        let has_group = ws
            .mux
            .focused()
            .map(|p| p.group_name.as_ref().is_some_and(|g| !g.is_empty()))
            .unwrap_or(false);
        // Cycle 941 (Terminator parity, terminal_popup_menu.py "Read only"):
        // checked while the focused pane drops user input.
        let read_only = ws.mux.focused().map(|p| p.read_only).unwrap_or(false);
        vec![
            ContextMenuItem::Item {
                label: "Copy",
                action: Action::Copy,
                enabled: has_selection,
            },
            ContextMenuItem::Item {
                label: "Paste",
                action: Action::Paste,
                enabled: true,
            },
            ContextMenuItem::Separator,
            ContextMenuItem::Item {
                label: "Split Right",
                action: Action::SplitRight,
                enabled: true,
            },
            ContextMenuItem::Item {
                label: "Split Down",
                action: Action::SplitDown,
                enabled: true,
            },
            ContextMenuItem::Item {
                label: "Close Pane",
                action: Action::ClosePane,
                enabled: true,
            },
            ContextMenuItem::Separator,
            ContextMenuItem::Item {
                label: "New Tab",
                action: Action::NewTab,
                enabled: true,
            },
            // Cycle 941 (Terminator parity): per-pane read-only toggle. The
            // check marker mirrors the Preferences-submenu convention
            // ("✓ on / off"); dispatch goes through the same
            // `Action::TogglePaneReadOnly` the keybind uses.
            ContextMenuItem::Separator,
            ContextMenuItem::DynamicItem {
                label: format!("{}Read only", if read_only { "✓ " } else { "  " }),
                action: Action::TogglePaneReadOnly,
                enabled: true,
            },
            // Cycle 683 (named-groups sub-cycle 7): right-click
            // entries for the broadcast-group surface. Layered
            // below the close-family + new-tab so they don't
            // hijack muscle memory; users who never use groups
            // see them at the bottom and can ignore.
            ContextMenuItem::Separator,
            ContextMenuItem::Item {
                label: "Set Group…",
                action: Action::CreateGroup,
                enabled: true,
            },
            ContextMenuItem::Item {
                label: "Group This Tab…",
                action: Action::GroupTab,
                enabled: true,
            },
            ContextMenuItem::Item {
                label: "Ungroup This Tab",
                action: Action::UngroupTab,
                enabled: has_group,
            },
        ]
    }

    /// Cycle 611 (Terminator parity, `custom_commands.py`): append
    /// every `menu-item = LABEL = CMD` config entry to the context-
    /// menu item list. Called by `open_context_menu` AFTER the
    /// built-in items + BEFORE the Lua-supplied items so the visual
    /// order from top to bottom is:
    ///     built-in actions → separator → config-file commands →
    ///     separator → Lua-registered items
    /// (matching the layered priority: kettle's own → user's config-
    /// file customization → user's Lua plugin customization).
    fn append_config_menu_items(&self, items: &mut Vec<ContextMenuItem>) {
        if self.cfg.menu_items.is_empty() {
            return;
        }
        items.push(ContextMenuItem::Separator);
        for mi in &self.cfg.menu_items {
            items.push(ContextMenuItem::ConfigItem {
                label: mi.label.clone(),
                command: mi.command.clone(),
            });
        }
    }

    /// Cycle 658 (sub-cycle 7 of [`TERMINATOR-REMOTE-DESIGN.md`](
    /// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): append the
    /// "Reconnect to …" / "Re-attach …" menu entry when the
    /// focused pane has a detected remote-session context.
    ///
    /// Click → cycle-611 `ContextMenuItem::ConfigItem` dispatch
    /// writes `clone_session_command(ctx) + "\n"` to the focused
    /// pane's PTY. The user can then split first if they want
    /// the reconnect to land in a new pane, or hit the entry
    /// directly to reconnect in-place after the original session
    /// exits.
    /// Cycle 686 (sub-cycle 8 of [`TERMINATOR-THEME-SUBMENU-DESIGN.md`](
    /// ../../../docs/TERMINATOR-THEME-SUBMENU-DESIGN.md)):
    /// append a `Submenu { "Profile", … }` entry populated from
    /// `Config::list_profiles()`. Same machinery as the cycle-
    /// 685 Theme submenu; click on a profile entry sets
    /// `App::config_path` to the cycle-618 profile path and
    /// reloads. The flyout-render side is still sub-cycle 3
    /// (shared with Theme).
    /// Cycle 717 (Preferences submenu, C8): append a `Preferences ▸`
    /// submenu with runtime-mutable toggles. Each toggle dispatches
    /// through a dedicated `Action::*` variant (cycle-717) that
    /// updates `self.cfg` AND writes back to the user's config file
    /// atomically via cycle-716's `persist_config_toggle`.
    ///
    /// Submenu layout (radio = "● selected / ○ other"; check =
    /// "✓ on /   off"):
    ///   - Scrollbar (radio: Always / Auto / Never)
    ///   - Cursor blink (check)
    ///   - Copy on select (check)
    ///   - Bell (radio: Off / Visual / Attention / Both)
    ///   - Mouse-hide while typing (check)
    ///   - Font size + / Font size − (reuses cycle-X actions)
    ///   - Separator
    ///   - Advanced… (cycle-718 / C9 — `Action::EditConfig`)
    fn append_preferences_submenu_items(&self, items: &mut Vec<ContextMenuItem>) {
        items.push(ContextMenuItem::Separator);
        let mut inner: Vec<ContextMenuItem> = Vec::new();
        let r = |sel: bool| if sel { "● " } else { "○ " };
        let c = |sel: bool| if sel { "✓ " } else { "  " };
        let dyn_item = |label: String, action: Action| ContextMenuItem::DynamicItem {
            label,
            action,
            enabled: true,
        };
        // Scrollbar radio.
        let sb = self.cfg.scrollbar;
        inner.push(dyn_item(
            format!(
                "{}Scrollbar always",
                r(sb == kettle_config::ScrollbarMode::Always)
            ),
            Action::SetScrollbarAlways,
        ));
        inner.push(dyn_item(
            format!(
                "{}Scrollbar auto",
                r(sb == kettle_config::ScrollbarMode::Auto)
            ),
            Action::SetScrollbarAuto,
        ));
        inner.push(dyn_item(
            format!(
                "{}Scrollbar hidden",
                r(sb == kettle_config::ScrollbarMode::Never)
            ),
            Action::SetScrollbarNever,
        ));
        inner.push(ContextMenuItem::Separator);
        // Boolean toggles.
        inner.push(dyn_item(
            format!("{}Cursor blink", c(self.cfg.cursor_blink)),
            Action::ToggleCursorBlink,
        ));
        inner.push(dyn_item(
            format!("{}Copy on select", c(self.cfg.copy_on_select)),
            Action::ToggleCopyOnSelect,
        ));
        inner.push(dyn_item(
            format!(
                "{}Mouse-hide while typing",
                c(self.cfg.mouse_hide_while_typing)
            ),
            Action::ToggleMouseHide,
        ));
        inner.push(ContextMenuItem::Separator);
        // Bell radio.
        let bell = self.cfg.bell;
        inner.push(dyn_item(
            format!("{}Bell off", r(bell == kettle_config::BellMode::Off)),
            Action::SetBellOff,
        ));
        inner.push(dyn_item(
            format!(
                "{}Bell visual flash",
                r(bell == kettle_config::BellMode::Visual)
            ),
            Action::SetBellVisual,
        ));
        inner.push(dyn_item(
            format!(
                "{}Bell attention",
                r(bell == kettle_config::BellMode::Attention)
            ),
            Action::SetBellAttention,
        ));
        inner.push(dyn_item(
            format!(
                "{}Bell visual + attention",
                r(bell == kettle_config::BellMode::Both)
            ),
            Action::SetBellBoth,
        ));
        inner.push(ContextMenuItem::Separator);
        // Font size +/- (reuse existing actions).
        inner.push(ContextMenuItem::Item {
            label: "Font size +",
            action: kettle_config::Action::IncreaseFontSize,
            enabled: true,
        });
        inner.push(ContextMenuItem::Item {
            label: "Font size −",
            action: kettle_config::Action::DecreaseFontSize,
            enabled: true,
        });
        inner.push(ContextMenuItem::Separator);
        // Cycle 718 (C9): the Advanced… escape hatch for everything
        // not exposed as a toggle.
        inner.push(ContextMenuItem::Item {
            label: "Advanced… (open config in $EDITOR)",
            action: kettle_config::Action::EditConfig,
            enabled: true,
        });
        items.push(ContextMenuItem::Submenu {
            label: "Preferences".to_string(),
            items: inner,
        });
    }

    fn append_profile_submenu_items(&self, items: &mut Vec<ContextMenuItem>) {
        let profile_names = kettle_config::Config::list_profiles();
        if profile_names.is_empty() {
            return;
        }
        items.push(ContextMenuItem::Separator);
        let inner: Vec<ContextMenuItem> = profile_names
            .into_iter()
            .map(|name| ContextMenuItem::ProfileChoice {
                label: name.clone(),
                profile: name,
            })
            .collect();
        items.push(ContextMenuItem::Submenu {
            label: "Profile".to_string(),
            items: inner,
        });
    }

    /// Cycle 685 (sub-cycle 2 of [`TERMINATOR-THEME-SUBMENU-DESIGN.md`](
    /// ../../../docs/TERMINATOR-THEME-SUBMENU-DESIGN.md)):
    /// append a `Submenu { "Theme", … }` entry populated from
    /// `Theme::list()`. The flyout-render side (sub-cycle 3)
    /// will surface the submenu items in a side panel; for now
    /// the parent menu shows "Theme ▸" and clicking it logs an
    /// info nudge (cycle 684).
    fn append_theme_submenu_items(&self, items: &mut Vec<ContextMenuItem>) {
        let theme_names = kettle_config::Theme::list();
        if theme_names.is_empty() {
            return;
        }
        items.push(ContextMenuItem::Separator);
        let inner: Vec<ContextMenuItem> = theme_names
            .into_iter()
            .map(|name| ContextMenuItem::ThemeChoice {
                label: name.to_string(),
                theme: name.to_string(),
            })
            .collect();
        items.push(ContextMenuItem::Submenu {
            label: "Theme".to_string(),
            items: inner,
        });
    }

    fn append_remote_menu_items(&mut self, ws: &mut WindowState, items: &mut Vec<ContextMenuItem>) {
        let Some(pane) = ws.mux.focused() else {
            return;
        };
        let Some(ctx) = &pane.remote_context else {
            return;
        };
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::ConfigItem {
            label: kettle_remote::clone_session_label(ctx),
            command: kettle_remote::clone_session_command(ctx),
        });
    }

    /// Cycle 375 (Terminator plugin parity, plugin sub-cycle 8):
    /// append every Lua-registered menu item to the context-menu
    /// item list. Called by `open_context_menu` after the built-in
    /// items so Lua items always render below the kettle defaults.
    /// Each entry's label is shown; clicking dispatches the
    /// registered Lua callback (via `LuaEngine::invoke_menu_item`).
    fn append_lua_menu_items(&self, items: &mut Vec<ContextMenuItem>) {
        if let Some(eng) = &self.lua_engine
            && let Ok(labels) = eng.list_menu_item_labels()
            && !labels.is_empty()
        {
            items.push(ContextMenuItem::Separator);
            for (idx, label) in labels.into_iter().enumerate() {
                items.push(ContextMenuItem::LuaItem {
                    label,
                    lua_idx: idx,
                });
            }
        }
    }

    /// Open the right-click context menu at `(px, py)`. Closes any other
    /// open modal first so we don't render two overlays at once
    /// (cycle 156 close_all_modals discipline), then computes the panel
    /// size from the cell metrics and clamps the anchor so the menu fits
    /// the surface (right-click near the bottom-right corner flips up-
    /// and-left rather than rendering off-screen).
    fn open_context_menu(&mut self, ws: &mut WindowState, px: f32, py: f32) {
        self.close_all_modals(ws);
        // Cycle 941 (Terminator parity, terminal_popup_menu.py "Open link" /
        // "Copy address"): when the right-click landed on a detected
        // hyperlink, lead with the URL rows. The URL is captured NOW — fresh
        // output scrolling the grid between open and click must not retarget
        // the action. `update_links` is keyed (cycle 803) so this is a no-op
        // when the viewport hasn't changed since the last scan.
        self.update_links(ws);
        // Cycle 942 (audit): only offer the rows when the click is INSIDE the
        // focused pane's rect. `cursor_cell` clamps out-of-rect coordinates to
        // the nearest cell (xterm parity — right for mouse reports), which
        // here could surface "Open Link" for a link the user never pointed at
        // (right-click on chrome / another pane mapping into the focused grid).
        let in_focused_pane = self
            .focused_rect(ws, self.area(ws))
            .is_some_and(|(rx, ry, rw, rh)| px >= rx && px < rx + rw && py >= ry && py < ry + rh);
        let mut items = Vec::new();
        if in_focused_pane && let Some(url) = self.link_at_cursor(ws).map(|l| l.uri.clone()) {
            items.push(ContextMenuItem::UrlItem {
                label: "Open Link",
                url: url.clone(),
                copy: false,
            });
            items.push(ContextMenuItem::UrlItem {
                label: "Copy Link Address",
                url,
                copy: true,
            });
            items.push(ContextMenuItem::Separator);
        }
        items.extend(self.context_menu_items(ws));
        // Cycle 611: append config-file menu items (if any).
        self.append_config_menu_items(&mut items);
        // Cycle 375: append Lua-supplied items (if any).
        self.append_lua_menu_items(&mut items);
        // Cycle 658 (remote.py sub-cycle 7): append the remote-
        // session reconnect entry when the focused pane has a
        // detected SSH/Docker/Podman/kubectl context.
        self.append_remote_menu_items(ws, &mut items);
        // Cycle 685 (theme-submenu sub-cycle 2): append the
        // Theme submenu populated from Theme::list(). The flyout
        // open machinery lands in sub-cycle 3.
        self.append_theme_submenu_items(&mut items);
        // Cycle 686 (theme-submenu sub-cycle 8): same machinery
        // for Profile (only appended when ~/.config/kettle/
        // profiles/ has any *.config files).
        self.append_profile_submenu_items(&mut items);
        // Cycle 756: top-level "Settings…" entry opens the full in-app
        // settings overlay (the richer, keyboard-navigable panel). The
        // Preferences ▸ submenu below stays as the quick-toggle surface.
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::Item {
            label: "Settings…",
            action: kettle_config::Action::OpenSettings,
            enabled: true,
        });
        // Cycle 717 (Preferences submenu, C8): runtime-mutable
        // settings + the Advanced… escape hatch.
        self.append_preferences_submenu_items(&mut items);
        self.show_context_menu(ws, items, px, py);
    }

    /// Cycle 805: shared tail of context-menu opening — drop disabled rows +
    /// collapse orphaned separators, compute panel geometry, clamp the anchor
    /// on-screen, and install the `ContextMenuState`. Used by both the
    /// right-click menu and the new-tab `▾` dropdown so they render
    /// pixel-identically.
    fn show_context_menu(
        &mut self,
        ws: &mut WindowState,
        items: Vec<ContextMenuItem>,
        px: f32,
        py: f32,
    ) {
        // Cycle 713 (Terminator menu UX, C4): every visible row is actionable.
        let items = filter_disabled(items);
        let highlight = items.iter().position(item_is_dispatchable).unwrap_or(0);
        let (cw, ch) = self.menu_cell(ws);
        let row_h = ch + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        let panel_h: f32 = items
            .iter()
            .map(|it| match it {
                ContextMenuItem::Separator => sep_h,
                ContextMenuItem::Item { .. }
                | ContextMenuItem::DynamicItem { .. }
                | ContextMenuItem::LuaItem { .. }
                | ContextMenuItem::ConfigItem { .. }
                | ContextMenuItem::Submenu { .. }
                | ContextMenuItem::ThemeChoice { .. }
                | ContextMenuItem::ProfileChoice { .. }
                | ContextMenuItem::NewTabShell { .. }
                | ContextMenuItem::UrlItem { .. }
                | ContextMenuItem::Info { .. } => row_h,
            })
            .sum();
        // Dropdown-parity cycle: the width budget includes each row's
        // right-aligned shortcut hint (+2 spacer columns) — the same formula
        // the renderer's `menu_row_chars` uses, kept in lockstep.
        let hints = self.menu_item_hints(&items);
        let max_chars = items
            .iter()
            .zip(hints.iter())
            .filter_map(|(it, hint)| {
                let hint_chars = if hint.is_empty() {
                    0
                } else {
                    hint.chars().count() + 2
                };
                match it {
                    ContextMenuItem::Item { label, .. } => Some(label.chars().count() + hint_chars),
                    // Cycle 941: count DynamicItem labels too — the hit-test twin
                    // (`context_menu_geometry`) already did, so the anchor clamp
                    // here used to underestimate the panel the renderer draws.
                    ContextMenuItem::DynamicItem { label, .. } => {
                        Some(label.chars().count() + hint_chars)
                    }
                    ContextMenuItem::LuaItem { label, .. } => Some(label.chars().count()),
                    ContextMenuItem::ConfigItem { label, .. } => Some(label.chars().count()),
                    ContextMenuItem::NewTabShell { label, .. } => {
                        Some(label.chars().count() + hint_chars)
                    }
                    ContextMenuItem::UrlItem { label, .. } => Some(label.chars().count()),
                    ContextMenuItem::Info { label } => Some(label.chars().count()),
                    // Cycle 684: submenu rows show "label ▸" so the
                    // max-width budget needs +2 for the suffix.
                    ContextMenuItem::Submenu { label, .. } => Some(label.chars().count() + 2),
                    // Cycle 685/686: Theme/Profile choices surface only inside an
                    // open flyout; the parent menu's width budget shouldn't grow.
                    ContextMenuItem::ThemeChoice { .. } => None,
                    ContextMenuItem::ProfileChoice { .. } => None,
                    _ => None,
                }
            })
            .max()
            .unwrap_or(0) as f32;
        let panel_w = (max_chars * cw + kettle_render::menu::H_PAD).max(kettle_render::menu::MIN_W);
        let (sw, sh) = ws
            .renderer
            .as_ref()
            .map(|r| {
                let (w, h) = r.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        let anchor = clamp_context_menu_anchor((px, py), (panel_w, panel_h), (sw, sh));
        ws.context_menu = Some(ContextMenuState {
            anchor,
            items,
            highlight,
            drill_stack: Vec::new(),
            scroll_offset: 0,
            scroll_stack: Vec::new(),
            typeahead_buf: String::new(),
            typeahead_until: None,
        });
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// Dropdown-parity cycle: the renderer's FRACTIONAL cell metrics, for
    /// menu geometry. `cell_px` rounds to integers for the PTY grid; the
    /// renderer positions menu rows with the f32 metrics, and using the
    /// rounded value here under-measured the panel by ~0.5px per row —
    /// enough, once the dropdown grew to 9 rows, to clip the last row and
    /// draw a phantom "more rows" scroll marker.
    fn menu_cell(&self, ws: &WindowState) -> (f32, f32) {
        ws.renderer
            .as_ref()
            .map(|r| (r.cell_w, r.cell_h))
            .unwrap_or((8.0, 16.0))
    }

    /// v2.24.0: reconcile the live theme preview with the current context-menu
    /// highlight. Applying snapshots the pre-preview `(theme_name, theme)` once
    /// into `ws.theme_preview`; reverting restores + clears it. A committed pick
    /// (`SetTheme`) clears the baseline first, so this becomes a no-op for it.
    fn sync_theme_preview(&mut self, ws: &mut WindowState) {
        let target = ws
            .context_menu
            .as_ref()
            .and_then(|m| match m.items.get(m.highlight) {
                Some(ContextMenuItem::ThemeChoice { theme, .. }) => Some(theme.clone()),
                _ => None,
            });
        match target {
            Some(name) => {
                if ws.theme_preview.is_none() {
                    ws.theme_preview = Some((self.cfg.theme_name.clone(), self.cfg.theme.clone()));
                }
                if self.cfg.theme_name != name {
                    self.cfg.theme_name = name.clone();
                    self.cfg.theme = kettle_config::Theme::by_name(&name);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
            None => self.revert_theme_preview(ws),
        }
    }

    /// Restore the theme captured before a preview began (if any) and clear the
    /// snapshot. No-op when no preview is active (the common case).
    fn revert_theme_preview(&mut self, ws: &mut WindowState) {
        if let Some((name, theme)) = ws.theme_preview.take() {
            self.cfg.theme_name = name;
            self.cfg.theme = theme;
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
    }

    /// Build the renderer-side settings projection from the live `settings_nav`
    /// and config. Used by the draw path AND the v2.24.0 mouse hit-test, so the
    /// painted panel and the clickable regions are computed from one source.
    fn settings_overlay_projection(
        &self,
        ws: &WindowState,
    ) -> Option<kettle_render::SettingsOverlay> {
        let nav = ws.settings_nav.as_ref()?;
        let cats = crate::settings::categories(&self.gpu_choices);
        let cat = nav.category.min(cats.len().saturating_sub(1));
        let active = &cats[cat];
        let fld = nav.field.min(active.fields.len().saturating_sub(1));
        Some(kettle_render::SettingsOverlay {
            categories: cats.iter().map(|c| c.name.to_string()).collect(),
            active_category: cat,
            rows: active
                .fields
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    // v2.24.0: an open inline text edit for this field shows its
                    // editable buffer + a caret instead of the stored value.
                    let editing = ws.settings_text_edit.as_ref().filter(|e| e.key == f.key);
                    let value = if let Some(e) = editing {
                        format!("{}\u{258f}", settings_edit_display(&e.buf))
                    } else if i == fld && nav.capturing {
                        // Cycle 766: capture prompt on the focused keybind row.
                        "‹press a chord — Esc to cancel›".to_string()
                    } else {
                        crate::settings::read(&self.cfg, f)
                    };
                    kettle_render::SettingsRow {
                        label: f.label.to_string(),
                        value,
                        disabled: crate::settings::field_disabled(&self.cfg, f.key),
                    }
                })
                .collect(),
            focused_row: fld,
            vim_nav: self.cfg.vim_menu_nav,
            // v2.23.0: on the Graphics tab, show which GPU is LIVE right now
            // (from the shared adapter) plus a restart hint when a GPU setting
            // was changed this session (it applies on next launch).
            footer_note: if active.name == "Graphics" {
                let active_line = self
                    .gpu
                    .as_ref()
                    .map(|g| {
                        let i = g.adapter_info();
                        format!("Active GPU: {} ({}, {})", i.name, i.kind, i.backend)
                    })
                    .unwrap_or_else(|| "Active GPU: (initializing)".to_string());
                if ws.settings_restart_pending {
                    Some(format!("{active_line}    •    ⚠ restart kettle to apply"))
                } else {
                    Some(active_line)
                }
            } else {
                None
            },
        })
    }

    /// v2.24.0: handle a mouse interaction with the settings overlay. `dir` is
    /// the adjust direction for a field (left-click `+1`, right-click `-1`,
    /// wheel `±1`); a category-tab hit switches category. `is_click` (vs a wheel)
    /// makes a hit OUTSIDE the panel close settings and makes inert hits consume
    /// the event; a wheel outside/inert is left for the modal swallow so a stray
    /// scroll can't dismiss the panel. Returns `true` if the event was consumed.
    fn settings_mouse(&mut self, ws: &mut WindowState, dir: i32, is_click: bool) -> bool {
        if ws.settings_nav.is_none() {
            return false;
        }
        // A mouse interaction while the inline path prompt is open cancels it
        // (the typed value is discarded; re-open to edit again).
        if ws.settings_text_edit.is_some() {
            ws.settings_text_edit = None;
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
            return true;
        }
        let Some(set) = self.settings_overlay_projection(ws) else {
            return false;
        };
        let (cw, ch) = self.menu_cell(ws);
        let (sw, sh) = ws
            .renderer
            .as_ref()
            .map(|r| {
                let (w, h) = r.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        let (mx, my) = (ws.cursor.x as f32, ws.cursor.y as f32);
        let hit = kettle_render::settings_hit_test(&set, cw, ch, sw, sh, mx, my);
        match hit {
            kettle_render::SettingsHit::Outside => {
                if !is_click {
                    return false; // let a stray wheel fall through to the swallow
                }
                ws.settings_nav = None;
            }
            kettle_render::SettingsHit::Inert => {
                if !is_click {
                    return false;
                }
            }
            kettle_render::SettingsHit::Category(i) => {
                if let Some(nav) = ws.settings_nav.as_mut() {
                    let cats = crate::settings::categories(&self.gpu_choices);
                    nav.category = i.min(cats.len().saturating_sub(1));
                    // Clamp the focused field into the new category + cancel any
                    // in-progress keybind capture.
                    let fcount = cats[nav.category].fields.len().max(1);
                    nav.field = nav.field.min(fcount - 1);
                    nav.capturing = false;
                }
            }
            kettle_render::SettingsHit::Field(f) => {
                let cats = crate::settings::categories(&self.gpu_choices);
                let cat = ws
                    .settings_nav
                    .as_ref()
                    .map(|n| n.category.min(cats.len().saturating_sub(1)))
                    .unwrap_or(0);
                let fcount = cats[cat].fields.len();
                if f < fcount {
                    let key = cats[cat].fields[f].key;
                    // A dimmed/inapplicable row is a no-op (but consumes the click).
                    if !crate::settings::field_disabled(&self.cfg, key) {
                        if let Some(nav) = ws.settings_nav.as_mut() {
                            nav.field = f;
                            nav.capturing = false;
                        }
                        // Keybind + text rows ACTIVATE on click (dir 0 — capture /
                        // open prompt); a wheel just focuses them. Value rows cycle.
                        let activate = crate::settings::is_keybind(&cats[cat].fields[f])
                            || crate::settings::is_text(&cats[cat].fields[f]);
                        if activate {
                            if is_click {
                                self.settings_adjust(ws, &cats, cat, f, 0);
                            }
                        } else {
                            self.settings_adjust(ws, &cats, cat, f, dir);
                        }
                    }
                }
            }
        }
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
        true
    }

    /// Dropdown-parity cycle: per-row shortcut hints for a menu, from the
    /// LIVE keybind map (a user rebind shows their actual chord). `Item` /
    /// `DynamicItem` rows look up their own Action; the Nth `NewTabShell`
    /// row maps to `Action::NewTabShell(N)` (the Ctrl+Shift+N family).
    /// Everything else gets no hint. Empty string = none.
    fn menu_item_hints(&self, items: &[ContextMenuItem]) -> Vec<String> {
        let mut shell_ordinal: u8 = 0;
        items
            .iter()
            .map(|it| match it {
                ContextMenuItem::Item { action, .. }
                | ContextMenuItem::DynamicItem { action, .. } => {
                    kettle_config::keybinds::hint_label(&self.cfg.keybinds, action)
                        .unwrap_or_default()
                }
                ContextMenuItem::NewTabShell { .. } => {
                    let h = if shell_ordinal < 9 {
                        kettle_config::keybinds::hint_label(
                            &self.cfg.keybinds,
                            &Action::NewTabShell(shell_ordinal),
                        )
                        .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    shell_ordinal = shell_ordinal.saturating_add(1);
                    h
                }
                _ => String::new(),
            })
            .collect()
    }

    /// Dropdown-parity cycle: the new-tab `▾` dropdown's rows — the detected
    /// shells, then Windows Terminal's bottom section (Settings / Command
    /// palette / About) behind a separator. Pure over the shell list so the
    /// menu shape is unit-testable.
    fn new_tab_menu_items(shells: &[kettle_core::term::ShellChoice]) -> Vec<ContextMenuItem> {
        let mut items: Vec<ContextMenuItem> = shells
            .iter()
            .cloned()
            .map(|(label, argv)| ContextMenuItem::NewTabShell { label, argv })
            .collect();
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::Item {
            label: "Settings…",
            action: Action::OpenSettings,
            enabled: true,
        });
        items.push(ContextMenuItem::Item {
            label: "Command palette",
            action: Action::CommandPalette,
            enabled: true,
        });
        items.push(ContextMenuItem::Item {
            label: "About kettle",
            action: Action::About,
            enabled: true,
        });
        items
    }

    /// Dropdown-parity cycle: the About panel — version + git hash, update
    /// status, and link rows, reusing the context-menu machinery (Info rows
    /// render dimmed and are not clickable; UrlItem rows copy/open).
    fn open_about_panel(&mut self, ws: &mut WindowState) {
        self.close_all_modals(ws);
        let v = &self.version_line;
        let mut items = vec![
            ContextMenuItem::Info {
                label: format!("kettle {v}"),
            },
            match &self.update_available {
                Some((tag, _)) => ContextMenuItem::Info {
                    label: format!("Update available: {tag}"),
                },
                None => ContextMenuItem::Info {
                    label: "Up to date".to_string(),
                },
            },
            ContextMenuItem::Separator,
            ContextMenuItem::UrlItem {
                label: "Copy version info",
                url: format!("kettle {v}"),
                copy: true,
            },
            ContextMenuItem::UrlItem {
                label: "Open GitHub page",
                url: "https://github.com/Reddimus/kettle".to_string(),
                copy: false,
            },
        ];
        if let Some((_, url)) = &self.update_available {
            items.push(ContextMenuItem::UrlItem {
                label: "Open release page",
                url: url.clone(),
                copy: false,
            });
        }
        let (sw, sh) = ws
            .renderer
            .as_ref()
            .map(|r| {
                let (w, h) = r.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        self.show_context_menu(ws, items, sw * 0.5 - 140.0, sh * 0.4);
    }

    /// Cycle 805 / dropdown-parity cycle: open the new-tab `▾` dropdown at
    /// `(px, py)` — detected shells (process-cached in kettle-core, prewarmed
    /// at startup) plus the Settings / Command palette / About bottom rows.
    fn open_new_tab_menu(&mut self, ws: &mut WindowState, px: f32, py: f32) {
        self.close_all_modals(ws);
        let shells = kettle_core::term::detect_shells();
        let items = Self::new_tab_menu_items(&shells);
        self.show_context_menu(ws, items, px, py);
    }

    /// Cycle 805: open a new tab running `argv`, inheriting the focused tab's
    /// current working directory. Shared by the new-tab `▾` dropdown's mouse +
    /// keyboard dispatch. Mirrors the cycle-802 NewTab pattern: log on failure,
    /// fire the `TabAdd` plugin event only when a tab was actually created.
    fn open_tab_with_argv(&mut self, ws: &mut WindowState, argv: &[String]) {
        let area = self.area(ws);
        let (cols, rows) = self.grid_of(ws, area);
        let (cw, ch) = self.cell_px(ws);
        let waker = self.waker();
        let cwd = ws.mux.focused().and_then(|p| p.term.current_dir());
        // Cycle 912 (audit): route through new_tab_with_launch so a WSL ▾-dropdown
        // entry's Linux cwd is carried via `wsl --cd` instead of being dropped
        // (a Windows spawn can't `cd` into a Linux path, so it fell back home).
        match ws
            .mux
            .new_tab_with_launch(&self.cfg, cols, rows, cw, ch, waker, argv.to_vec(), cwd)
        {
            Ok(()) => self.fire_tab_add_event(ws),
            Err(e) => log::warn!("could not open shell tab ({argv:?}): {e}"),
        }
    }

    /// Move the context-menu highlight by `delta` (±1), skipping
    /// `Separator` rows and disabled `Item` rows. Wraps at the ends so
    /// `↑` on the first row jumps to the last enabled row and vice
    /// versa — Chrome / Firefox menu convention. Pure on `(items,
    /// current)` so the wrap+skip math is unit-testable independent of
    /// the App / cursor state.
    fn step_context_menu_highlight(&mut self, ws: &mut WindowState, delta: isize) {
        let next = ws
            .context_menu
            .as_ref()
            .map(|m| next_context_menu_highlight(&m.items, m.highlight, delta));
        if let Some(next) = next {
            self.set_context_menu_highlight(ws, next);
        }
    }

    /// Move the context-menu highlight to `next` and sync `scroll_offset` so
    /// it stays visible (cycle 714). Split out of
    /// `step_context_menu_highlight` in v2.20.0 so the vim-menu-nav jumps
    /// (`g`/`G`, `Ctrl+d`/`Ctrl+u`) reuse the exact same scroll-window math.
    fn set_context_menu_highlight(&mut self, ws: &mut WindowState, next: usize) {
        let Some(((_, _), (_, panel_h))) = self.context_menu_geometry(ws) else {
            return;
        };
        let (_, ch) = self.menu_cell(ws);
        let row_h = ch + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        let Some(menu) = ws.context_menu.as_mut() else {
            return;
        };
        menu.highlight = next;
        // Cycle 714: if the new highlight is outside the visible
        // window, advance scroll_offset to bring it into view.
        let visible = count_rows_fitting(&menu.items, menu.scroll_offset, panel_h, row_h, sep_h);
        if next < menu.scroll_offset {
            menu.scroll_offset = next;
        } else if next >= menu.scroll_offset + visible {
            // Pull scroll_offset back until `next` is the LAST fully visible
            // row. v2.20.0 (review fix): probe the candidate one above the
            // current offset — the old form checked the current offset, so a
            // far jump (`G` into a 500-row theme list) stopped immediately at
            // `off = next` and rendered the highlight as the panel's only
            // row with an empty page below it.
            let mut off = next;
            while off > 0 {
                let fit = count_rows_fitting(&menu.items, off - 1, panel_h, row_h, sep_h);
                if off - 1 + fit > next {
                    off -= 1;
                } else {
                    break;
                }
            }
            menu.scroll_offset = off;
        }
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// Cycle 714. Scroll the context-menu by `delta` rows (positive
    /// = down). Clamped so we can't scroll past the last row that
    /// would still fill the visible window.
    fn scroll_context_menu(&mut self, ws: &mut WindowState, delta: isize) {
        let Some(((_, _), (_, panel_h))) = self.context_menu_geometry(ws) else {
            return;
        };
        let (_, ch) = self.menu_cell(ws);
        let row_h = ch + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        let Some(menu) = ws.context_menu.as_mut() else {
            return;
        };
        let n = menu.items.len();
        let new_off = (menu.scroll_offset as isize + delta).max(0) as usize;
        // Clamp: never scroll past the point where the last visible
        // row is the final item.
        let mut max_off = 0usize;
        for cand in 0..n {
            let fit = count_rows_fitting(&menu.items, cand, panel_h, row_h, sep_h);
            if cand + fit >= n {
                max_off = cand;
                break;
            }
            max_off = cand;
        }
        let clamped = new_off.min(max_off);
        if clamped != menu.scroll_offset {
            menu.scroll_offset = clamped;
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
    }

    /// Resolve a mouse-button press into a context-menu action, if any.
    /// Only a *left*-click (bcode 0) inside the panel can fire a row
    /// — right and middle clicks are ignored so right-click re-anchor
    /// still feels distinct from "select this menu item." Returns
    /// `None` if the click missed the panel, hit a separator, or hit a
    /// disabled row; the caller then either dismisses (left-click
    /// Cycle 712 (Terminator menu UX, hover-to-highlight).
    /// Return the row index under the cursor when the context menu
    /// is open, `None` if the cursor is outside the panel OR landed
    /// on a separator. Thin wrapper around the pure `find_menu_row_y`
    /// helper so the row-walk is unit-testable without standing up an
    /// App. Used by `update_menu_highlight_from_cursor` on every
    /// `CursorMoved` so the highlight tracks the pointer the way every
    /// desktop menu does (GTK / macOS NSMenu / Windows).
    fn menu_row_at_cursor(&self, ws: &WindowState) -> Option<usize> {
        let menu = ws.context_menu.as_ref()?;
        let ((ax, ay), (panel_w, panel_h)) = self.context_menu_geometry(ws)?;
        let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
        if px < ax || px >= ax + panel_w || py < ay || py >= ay + panel_h {
            return None;
        }
        let (_, ch) = self.menu_cell(ws);
        let row_h = ch + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        // Cycle 714: row-walk starts at scroll_offset; only the
        // visible slice is hit-tested. Off-by-one is handled by
        // find_menu_row_y's half-open interval [y, y+h).
        let start = menu.scroll_offset.min(menu.items.len());
        let kinds: Vec<bool> = menu.items[start..]
            .iter()
            .map(|it| matches!(it, ContextMenuItem::Separator))
            .collect();
        find_menu_row_y(py, ay, row_h, sep_h, &kinds).map(|i| i + start)
    }

    /// Cycle 712. Set `menu.highlight` to whichever row the cursor is
    /// over right now; no-op when the cursor is outside the panel or
    /// on a separator. Called from `CursorMoved`. Requests a redraw
    /// only when the highlight actually changed so we don't churn the
    /// GPU on every sub-pixel motion event.
    fn update_menu_highlight_from_cursor(&mut self, ws: &mut WindowState) {
        let Some(idx) = self.menu_row_at_cursor(ws) else {
            return;
        };
        let Some(menu) = ws.context_menu.as_mut() else {
            return;
        };
        if menu.highlight == idx {
            return;
        }
        menu.highlight = idx;
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// outside) or falls through to the regular click handling
    /// (right-click → re-open at the new point).
    /// Dispatch one resolved context-menu selection. The single sink for
    /// mouse clicks, keyboard Enter / Space, and mnemonic / typeahead
    /// activation (cycle 889/890) so all three behave identically.
    ///
    /// Every *leaf* click closes the menu first, then acts. Only
    /// `DrillIntoSubmenu` keeps the menu open — it replaces the visible
    /// level with the submenu's items (parent pushed onto `drill_stack`).
    ///
    /// Cycle 889 (audit): the mouse path used to set `ws.context_menu =
    /// None` *before* matching the click, which made the
    /// `DrillIntoSubmenu` arm — which needs `ws.context_menu.as_mut()`
    /// — dead code: a mouse-clicked submenu row silently dismissed the
    /// whole menu instead of drilling in. Closing per-leaf here (and
    /// leaving the menu intact for the drill) fixes that.
    fn dispatch_context_menu_click(
        &mut self,
        ws: &mut WindowState,
        click: ContextMenuClick,
        event_loop: &ActiveEventLoop,
    ) {
        match click {
            // Keep the menu open: swap the visible level for the submenu.
            ContextMenuClick::DrillIntoSubmenu(idx) => {
                if let Some(menu) = ws.context_menu.as_mut() {
                    let nested_items = match menu.items.get(idx) {
                        Some(ContextMenuItem::Submenu { items, .. }) => items.clone(),
                        _ => Vec::new(),
                    };
                    if !nested_items.is_empty() {
                        let parent = std::mem::replace(&mut menu.items, nested_items);
                        menu.drill_stack.push(parent);
                        // Cycle 714: each level keeps its own scroll view.
                        menu.scroll_stack.push(menu.scroll_offset);
                        menu.scroll_offset = 0;
                        // Drilling resets any in-progress typeahead so a
                        // half-typed parent prefix doesn't bleed into the
                        // child level.
                        menu.typeahead_buf.clear();
                        menu.typeahead_until = None;
                        menu.highlight = menu
                            .items
                            .iter()
                            .position(item_is_dispatchable)
                            .unwrap_or(0);
                    }
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            ContextMenuClick::Action(action) => {
                ws.context_menu = None;
                self.handle_action(ws, action, event_loop);
            }
            ContextMenuClick::LuaMenuItem(idx) => {
                ws.context_menu = None;
                // Cycle 375/433: invoke the Lua callback + drain any
                // LuaCommands it queued through the canonical helper.
                if let Some(eng) = &self.lua_engine {
                    eng.invoke_menu_item(idx);
                }
                self.drain_lua_hook_commands("lua menu-item");
            }
            ContextMenuClick::ConfigCommand(command) => {
                ws.context_menu = None;
                // Cycle 611 (Terminator parity): write `CMD\n` to the PTY.
                // Cycle 941: acts as the user — a read-only pane drops it.
                if let Some(p) = ws.mux.focused() {
                    let mut bytes = command.into_bytes();
                    bytes.push(b'\n');
                    p.feed_input(&bytes);
                }
            }
            ContextMenuClick::SetTheme(name) => {
                ws.context_menu = None;
                // v2.24.0: commit the preview — drop the revert baseline so the
                // post-event `sync_theme_preview` keeps this pick instead of
                // restoring the pre-hover theme.
                ws.theme_preview = None;
                self.cfg.theme_name = name.clone();
                self.cfg.theme = kettle_config::Theme::by_name(&name);
                // Cycle 918: theme is config-governed — persist to the config
                // file (not the session). A session-pinned theme used to OVERRIDE
                // the config/compile-time default on restore, so a default change
                // (or a fresh-config user) never saw the new theme.
                // Cycle 919 (audit L4): notify if it can't be written, so the
                // pick isn't silently lost on the next launch.
                if !self.persist_pref("theme", &name) {
                    fire_notify(
                        "kettle: theme not saved",
                        "Applied for this session — couldn't write it to your config file.",
                    );
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            ContextMenuClick::SetProfile(name) => {
                ws.context_menu = None;
                if let Some(p) = kettle_config::Config::path_for_profile(&name) {
                    self.config_path = Some(p);
                    self.reload_config(ws);
                }
            }
            ContextMenuClick::NewTabWithArgv(argv) => {
                ws.context_menu = None;
                self.open_tab_with_argv(ws, &argv);
            }
            // Cycle 941 (Terminator parity): the URL-aware leading rows.
            // Open routes through the cycle-374 `open_url` chain (Lua URL
            // handlers → custom_url_handler → system open, with the
            // `is_safe_url` guard); Copy puts the address on the clipboard.
            ContextMenuClick::Url { url, copy } => {
                ws.context_menu = None;
                if copy {
                    if let Some(cb) = &mut self.clipboard
                        && let Err(e) = cb.set_text(url)
                    {
                        log::warn!("clipboard set_text failed (link address copy): {e}");
                    }
                } else {
                    self.open_url(&url);
                }
            }
        }
    }

    fn context_menu_click_action(&self, ws: &WindowState, bcode: u8) -> Option<ContextMenuClick> {
        if bcode != 0 {
            return None;
        }
        let menu = ws.context_menu.as_ref()?;
        let ((ax, ay), (panel_w, panel_h)) = self.context_menu_geometry(ws)?;
        let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
        if px < ax || px >= ax + panel_w || py < ay || py >= ay + panel_h {
            return None;
        }
        let (_, ch) = self.menu_cell(ws);
        let row_h = ch + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        // Cycle 714: skip the scrolled-off rows above scroll_offset
        // before walking. `row_y` starts at the panel top; iteration
        // begins at item `scroll_offset`.
        let start = menu.scroll_offset.min(menu.items.len());
        let mut row_y = ay;
        for (idx, item) in menu.items.iter().enumerate().skip(start) {
            let h = match item {
                ContextMenuItem::Separator => sep_h,
                ContextMenuItem::Item { .. }
                | ContextMenuItem::DynamicItem { .. }
                | ContextMenuItem::LuaItem { .. }
                | ContextMenuItem::ConfigItem { .. }
                | ContextMenuItem::Submenu { .. }
                | ContextMenuItem::ThemeChoice { .. }
                | ContextMenuItem::ProfileChoice { .. }
                | ContextMenuItem::NewTabShell { .. }
                | ContextMenuItem::UrlItem { .. }
                | ContextMenuItem::Info { .. } => row_h,
            };
            if py >= row_y && py < row_y + h {
                // Cycle 890: shared mapper — the same row→click table the
                // keyboard Enter / Space + mnemonic paths use, so mouse and
                // keyboard dispatch can never diverge again.
                return item_to_click(item, idx);
            }
            row_y += h;
        }
        None
    }

    /// `(items, anchor, panel_w, panel_h)` snapshot for the click /
    /// hover hit-tests — returned in pixels so callers don't have to
    /// re-derive the layout. `None` when the menu isn't open.
    fn context_menu_geometry(&self, ws: &WindowState) -> Option<((f32, f32), (f32, f32))> {
        let menu = ws.context_menu.as_ref()?;
        let (cw, ch) = self.menu_cell(ws);
        let row_h = ch + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        // Natural height: sum every row + separator.
        let natural_h: f32 = menu
            .items
            .iter()
            .map(|it| match it {
                ContextMenuItem::Separator => sep_h,
                _ => row_h,
            })
            .sum();
        // Cycle 714 (Terminator menu UX, C5): clamp the panel
        // height to the surface so a ~512-entry Theme submenu
        // can't grow off-screen. We reserve 80px of vertical
        // breathing room (40px top + 40px bottom) so the menu
        // doesn't bump into the window edge.
        let (_, surface_h) = ws
            .renderer
            .as_ref()
            .map(|r| {
                let (w, h) = r.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        let max_h = (surface_h - kettle_render::menu::PANEL_BREATHING).max(row_h);
        let panel_h = natural_h.min(max_h);
        // Dropdown-parity cycle: include the right-aligned hint budget —
        // kept in lockstep with `show_context_menu` and the renderer's
        // `menu_row_chars`.
        let hints = self.menu_item_hints(&menu.items);
        let max_chars = menu
            .items
            .iter()
            .zip(hints.iter())
            .filter_map(|(it, hint)| {
                let hint_chars = if hint.is_empty() {
                    0
                } else {
                    hint.chars().count() + 2
                };
                match it {
                    ContextMenuItem::Item { label, .. } => Some(label.chars().count() + hint_chars),
                    ContextMenuItem::DynamicItem { label, .. } => {
                        Some(label.chars().count() + hint_chars)
                    }
                    ContextMenuItem::LuaItem { label, .. } => Some(label.chars().count()),
                    ContextMenuItem::ConfigItem { label, .. } => Some(label.chars().count()),
                    // Cycle 805: count shell-dropdown labels so this hit-test width
                    // matches the panel `show_context_menu` actually rendered.
                    ContextMenuItem::NewTabShell { label, .. } => {
                        Some(label.chars().count() + hint_chars)
                    }
                    // Cycle 941: same for the URL-aware leading rows.
                    ContextMenuItem::UrlItem { label, .. } => Some(label.chars().count()),
                    ContextMenuItem::Info { label } => Some(label.chars().count()),
                    _ => None,
                }
            })
            .max()
            .unwrap_or(0) as f32;
        let panel_w = (max_chars * cw + kettle_render::menu::H_PAD).max(kettle_render::menu::MIN_W);
        Some((menu.anchor, (panel_w, panel_h)))
    }

    /// Build the renderer-side `ContextMenu` slice from the App-side
    /// state. Splits the labels (owned `String`) from the dispatch
    /// actions so the renderer stays Action-agnostic.
    fn context_menu_overlay(&self, ws: &WindowState) -> Option<ContextMenu> {
        let menu = ws.context_menu.as_ref()?;
        // Dropdown-parity cycle: per-row shortcut hints from the LIVE keybind
        // map (computed per frame; the map only changes on reload).
        let hints = self.menu_item_hints(&menu.items);
        let rows = menu
            .items
            .iter()
            .zip(hints)
            .map(|(it, hint)| match it {
                ContextMenuItem::Item { label, enabled, .. } => ContextMenuRow {
                    label: (*label).to_string(),
                    separator: false,
                    enabled: *enabled,
                    hint,
                },
                ContextMenuItem::DynamicItem { label, enabled, .. } => ContextMenuRow {
                    label: label.clone(),
                    separator: false,
                    enabled: *enabled,
                    hint,
                },
                ContextMenuItem::Separator => ContextMenuRow {
                    label: String::new(),
                    separator: true,
                    enabled: false,
                    hint: String::new(),
                },
                ContextMenuItem::LuaItem { label, .. } => ContextMenuRow {
                    label: label.clone(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
                ContextMenuItem::ConfigItem { label, .. } => ContextMenuRow {
                    label: label.clone(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
                ContextMenuItem::Submenu { label, .. } => ContextMenuRow {
                    // Cycle 684: append "▸" to signal "this row
                    // opens a submenu". Sub-cycle 3 wires the
                    // actual flyout; for now the affordance is
                    // visible but clicking it just no-ops.
                    label: format!("{label} ▸"),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
                // Cycle 687 (theme-submenu sub-cycle 3 drill-in):
                // ThemeChoice and ProfileChoice ARE rendered when
                // they appear in the current items list. Since
                // the drill-in click replaces menu.items with the
                // submenu's items, they naturally appear here.
                // In the parent menu (before drill-in) they never
                // appear in menu.items, so this arm is unreached
                // — the parent's items don't contain ThemeChoice/
                // ProfileChoice directly, only inside Submenu.
                ContextMenuItem::ThemeChoice { label, .. } => ContextMenuRow {
                    label: label.clone(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
                ContextMenuItem::ProfileChoice { label, .. } => ContextMenuRow {
                    label: label.clone(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
                // Cycle 805: new-tab ▾ shell choice — a normal clickable row.
                ContextMenuItem::NewTabShell { label, .. } => ContextMenuRow {
                    label: label.clone(),
                    separator: false,
                    enabled: true,
                    hint,
                },
                // Cycle 941: URL-aware leading rows ("Open Link" /
                // "Copy Link Address") — normal clickable rows.
                ContextMenuItem::UrlItem { label, .. } => ContextMenuRow {
                    label: (*label).to_string(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
                // Dropdown-parity cycle: Info renders as a dimmed
                // (disabled-style) static line.
                ContextMenuItem::Info { label } => ContextMenuRow {
                    label: label.clone(),
                    separator: false,
                    enabled: false,
                    hint: String::new(),
                },
            })
            .collect();
        // Cycle 714 (Terminator menu UX, C5): pass through the
        // scroll state + clamped panel height the renderer needs to
        // draw only the visible slice.
        let panel_h_clamped = self
            .context_menu_geometry(ws)
            .map(|(_, (_, h))| h)
            .unwrap_or(0.0);
        Some(ContextMenu {
            anchor: menu.anchor,
            rows,
            highlight: menu.highlight,
            scroll_offset: menu.scroll_offset,
            panel_h_clamped,
        })
    }

    /// Force the next redraw to render the cursor visible. Shared by:
    /// - the focus-change path (`note_focus_change`)
    /// - `Action::Reset` (cycle 134) so a "fresh start" cursor is visible
    /// - `CursorBlinkingChange` events (DEC ?12 program-driven toggle)
    /// - the four modal Escape handlers (cycle 140) so closing the
    ///   search/palette/hints/SSH overlay reveals the cursor immediately
    ///   instead of waiting up to one blink interval.
    fn reset_blink_phase(&mut self, ws: &mut WindowState) {
        ws.blink_on = true;
        ws.last_blink = std::time::Instant::now();
    }

    /// Cycle 717 (Preferences submenu, C8): write a `key = value`
    /// line to the user's active config file via the cycle-716
    /// atomic helper. Resolves the path the same way Action::EditConfig
    /// does (App::config_path → `Config::default_path` fallback).
    /// Logs + ignores any I/O error so a transient FS issue doesn't
    /// kill the menu dispatch; the in-memory toggle still applied,
    /// so the user's next session will pick up the runtime change
    /// once it persists.
    /// Cycle 919 (audit L4): returns `true` iff the value was written to the
    /// config file. Callers of user-initiated changes (theme picks, Settings)
    /// notify the user on `false` so a change that's live this session but lost
    /// on restart isn't silent.
    fn persist_pref(&self, key: &str, value: &str) -> bool {
        let Some(path) = self
            .config_path
            .clone()
            .or_else(kettle_config::Config::default_path)
        else {
            log::warn!(
                "persist_pref: no config path resolved (set $XDG_CONFIG_HOME or pass --config)"
            );
            return false;
        };
        match kettle_config::persist_config_toggle(&path, key, value) {
            Ok(bak) => {
                log::info!(
                    "persist_pref: wrote {key} = {value} to {} (backup at {})",
                    path.display(),
                    bak.display()
                );
                true
            }
            Err(e) => {
                log::warn!(
                    "persist_pref: failed to write {key} = {value} to {}: {e}",
                    path.display()
                );
                false
            }
        }
    }

    /// Act on the cycle-794 update banner: dismiss it (recording the tag so it
    /// won't re-nag) and, when `open` is true, open the release page first.
    /// Returns `false` when no banner is showing (the caller decides whether
    /// that's a no-op or worth a debug log). Shared by the banner mouse
    /// handler and the cycle-809 `OpenUpdate` / `DismissUpdate` keyboard
    /// actions so all three paths stay in lockstep.
    fn act_on_update_banner(&mut self, ws: &mut WindowState, open: bool) -> bool {
        let Some((tag, url)) = self.update_available.clone() else {
            return false;
        };
        if open {
            self.open_url(&url);
        }
        crate::update_check::record_dismissed(&tag);
        self.update_available = None;
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
        true
    }

    fn handle_action(
        &mut self,
        ws: &mut WindowState,
        action: Action,
        event_loop: &ActiveEventLoop,
    ) {
        let area = self.area(ws);
        let (cols, rows) = self.grid_of(ws, area);
        let (cw, ch) = self.cell_px(ws);
        let waker = self.waker();
        // Snapshot the (tab, pane-leaf) the cursor lives in so we can
        // detect any focus change the action causes. Cycles 134/135
        // landed this for keyboard-driven actions; cycle 136 extended
        // to mouse paths via the shared `focus_key` / `note_focus_change`
        // helpers.
        let pre_focus = self.focus_key(ws);
        match action {
            Action::NewTab => {
                // Cycle 368 (plugin sub-cycle 4): fires LuaEvent::TabAdd
                // with the new active tab index after Mux::new_tab.
                // Cycle 426 collapsed the inline event/drain into the
                // shared fire_tab_add_event helper.
                // Cycle 802 (audit): surface a PTY-spawn failure instead of
                // swallowing it with `let _ =` (the `-e` launch path already
                // logs), and only fire TabAdd when a tab was actually created
                // — firing it on failure announced a tab that doesn't exist.
                match ws.mux.new_tab(&self.cfg, cols, rows, cw, ch, waker) {
                    Ok(()) => self.fire_tab_add_event(ws),
                    Err(e) => log::warn!("could not open a new tab (shell spawn failed?): {e}"),
                }
            }
            Action::NewWindow => {
                // C4 (multi-window): open a real second window IN-PROCESS.
                // Replaces the old spawn-a-separate-kettle-process behavior:
                // tabs can now move live between windows, and the GPU device
                // is shared. Falls back to a new tab if the window can't be
                // created (degraded but useful, the cycle-425 precedent).
                if let Err(_unopened) =
                    self.open_window(event_loop, WindowOpen::Fresh { cwd: None }, None, None)
                {
                    match ws.mux.new_tab(&self.cfg, cols, rows, cw, ch, waker) {
                        Ok(()) => self.fire_tab_add_event(ws),
                        Err(e) => {
                            log::warn!("new-window fallback: could not open a tab: {e}")
                        }
                    }
                }
            }
            Action::SplitRight => {
                // Cycle 888: if the focused pane has entered a shell it launched
                // (e.g. typed `wsl` in pwsh), clone THAT shell + its dir; else
                // clone the pane's own launch command (cycle 886). Cycle 802:
                // log a spawn failure instead of swallowing it.
                let detected = self.focused_foreground_shell(ws);
                let res = match detected {
                    Some(s) => ws.mux.split_with(
                        Dir::Horizontal,
                        &self.cfg,
                        cols,
                        rows,
                        cw,
                        ch,
                        waker,
                        s.argv,
                        s.cwd,
                    ),
                    None => ws
                        .mux
                        .split(Dir::Horizontal, &self.cfg, cols, rows, cw, ch, waker),
                };
                if let Err(e) = res {
                    log::warn!("could not split pane (right): {e}");
                }
            }
            Action::SplitDown | Action::SplitAuto => {
                let detected = self.focused_foreground_shell(ws);
                let res = match detected {
                    Some(s) => ws.mux.split_with(
                        Dir::Vertical,
                        &self.cfg,
                        cols,
                        rows,
                        cw,
                        ch,
                        waker,
                        s.argv,
                        s.cwd,
                    ),
                    None => ws
                        .mux
                        .split(Dir::Vertical, &self.cfg, cols, rows, cw, ch, waker),
                };
                if let Err(e) = res {
                    log::warn!("could not split pane (down): {e}");
                }
            }
            Action::ClosePane => {
                // Cycle 662 (confirm-dialog sub-cycle 6): per-pane
                // close prompts when ask_before_closing = Always.
                // MultipleTerminals doesn't prompt (single pane); see
                // cycle-638's should_prompt for the matrix.
                // v2.20.0 (Ghostty `confirm-close-surface` parity): a pane
                // sitting idle at an integrated-shell prompt has no work to
                // lose — skip the confirm. Plain shells (no OSC 133 marks)
                // never report idle, so their behavior is unchanged.
                let busy = ws
                    .mux
                    .focused()
                    .map(|p| !p.term.shell_idle())
                    .unwrap_or(true);
                if busy && self.cfg.ask_before_closing.should_prompt(1) {
                    self.close_all_modals(ws);
                    ws.confirm_dialog = Some(ConfirmDialogState {
                        prompt: "Close this pane?".to_string(),
                        buttons: vec![
                            ConfirmButton::Cancel,
                            ConfirmButton::Confirm {
                                label: "Close".to_string(),
                                destructive: true,
                            },
                        ],
                        focus_idx: 0,
                        on_confirm: ConfirmAction::ClosePane,
                    });
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Cycle 750: capture the focused pane id BEFORE the close —
                // afterward active_focus() returns the promoted sibling.
                let closing_pane = ws.mux.active_focus();
                let was_last = ws.mux.close_focused();
                if let Some(id) = closing_pane {
                    self.fire_pane_close_event(id);
                }
                if was_last {
                    self.pending_window_close = true;
                } else {
                    // Cycle 735: explicit redraw + focus-event
                    // refresh after a successful close-pane. Pre-735
                    // the path returned without scheduling a frame
                    // OR re-emitting the focus event; the split tree
                    // had collapsed (sibling promoted to root) but
                    // the renderer cache + the cycle-703 PaneFocus
                    // event's last-fired pane id were both stale
                    // until the next user input implicitly nudged
                    // them. The CloseTab path (~30 lines below) gets
                    // an analogous refresh implicitly via the
                    // fire_tab_close_event Lua dispatch; ClosePane
                    // had nothing equivalent.
                    //
                    // On Windows under wgpu DX12, the stale-layout
                    // window has been reported as a crash via the
                    // user's Surface Book 3 testing. The most likely
                    // upstream chain: stale focus id -> tab-bar
                    // render path indexes into a removed pane ->
                    // panic on the Arc<Mutex<Terminal>> lock of a
                    // dropped pane. The fix here is preventative:
                    // the redraw + focus-event refresh forces the
                    // renderer + lua to see the new, consistent
                    // tree on the same frame as the close.
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    // The cycle-703 PaneFocus event needs to fire
                    // with the new focused id (the sibling that
                    // got promoted), so plugins that observe focus
                    // don't keep stale per-pane state. Mirrors the
                    // poll_focus_event helper's pattern at ~5987.
                    self.poll_focus_event(ws);
                }
            }
            Action::CloseTab => {
                // Cycle 662 (confirm-dialog sub-cycle 6): close the
                // active tab via the modal when ask_before_closing
                // says so. scope_count = leaves in the active tab
                // (panes_in_tab below).
                let panes_in_tab = ws
                    .mux
                    .tabs
                    .get(ws.mux.active)
                    .map(|t| count_leaves(&t.root))
                    .unwrap_or(1);
                // v2.20.0 (Ghostty parity): only panes NOT idle at an
                // integrated-shell prompt count toward the confirm decision
                // (an idle prompt has no work to lose; no marks → counts
                // as busy → unchanged behavior).
                let busy_in_tab = ws
                    .mux
                    .tabs
                    .get(ws.mux.active)
                    .map(|t| {
                        t.root
                            .leaf_ids()
                            .iter()
                            .filter(|id| {
                                ws.mux
                                    .panes
                                    .get(id)
                                    .map(|p| !p.term.shell_idle())
                                    .unwrap_or(true)
                            })
                            .count()
                    })
                    .unwrap_or(panes_in_tab);
                if busy_in_tab > 0 && self.cfg.ask_before_closing.should_prompt(busy_in_tab) {
                    self.close_all_modals(ws);
                    ws.confirm_dialog = Some(ConfirmDialogState {
                        prompt: format!("Close tab with {panes_in_tab} pane(s)?"),
                        buttons: vec![
                            ConfirmButton::Cancel,
                            ConfirmButton::Confirm {
                                label: "Close".to_string(),
                                destructive: true,
                            },
                        ],
                        focus_idx: 0,
                        on_confirm: ConfirmAction::CloseTab,
                    });
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Cycle 368: capture the active index BEFORE close
                // so the LuaEvent::TabClose payload is meaningful
                // (after close, ws.mux.active points at a
                // different tab).
                //
                // Cycle 426 collapsed the inline event/drain into
                // the shared fire_tab_close_event helper.
                let closing_idx = ws.mux.active;
                if ws.mux.close_tab() {
                    self.pending_window_close = true;
                }
                self.fire_tab_close_event(closing_idx);
            }
            Action::CloseWindow => {
                // Cycle 660 (sub-cycle 5 of confirm-dialog design):
                // intercept via the cycle-638 should_prompt helper.
                // When ask-before-closing fires, open the modal
                // with on_confirm=CloseWindow; the modal's Confirm
                // dispatch (in the key handler) re-runs the close
                // path below.
                let scope = ws.mux.panes.len();
                // v2.20.0 (Ghostty parity): idle-at-prompt panes don't
                // count (see ClosePane above).
                let busy = ws
                    .mux
                    .panes
                    .values()
                    .filter(|p| !p.term.shell_idle())
                    .count();
                if busy > 0 && self.cfg.ask_before_closing.should_prompt(busy) {
                    self.close_all_modals(ws);
                    ws.confirm_dialog = Some(ConfirmDialogState {
                        prompt: format!("Close {scope} pane(s)?"),
                        buttons: vec![
                            ConfirmButton::Cancel,
                            ConfirmButton::Confirm {
                                label: "Close".to_string(),
                                destructive: true,
                            },
                        ],
                        focus_idx: 0, // Cancel — safe default.
                        on_confirm: ConfirmAction::CloseWindow,
                    });
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Distinct from `CloseTab`: drop *every* tab + pane in
                // this window, not just the focused tab. Previously
                // both actions did `close_tab()` so binding `close_window`
                // gave the user a confusingly-misnamed alias for
                // `close_tab`. Now they're genuinely different.
                ws.mux.close_window();
                // Cycle 157: save the (now-empty) session so next
                // launch starts fresh. Otherwise the previous
                // multi-tab state from before close_window stays
                // in session.json and silently restores.
                self.save_session(ws);
                self.pending_window_close = true;
            }
            Action::NextTab => ws.mux.next_tab(),
            Action::PrevTab => ws.mux.prev_tab(),
            Action::FocusNext => ws.mux.focus_cycle(area, true),
            Action::FocusPrev => ws.mux.focus_cycle(area, false),
            Action::FocusLeft => ws.mux.focus_dir(area, -1, 0),
            Action::FocusRight => ws.mux.focus_dir(area, 1, 0),
            Action::FocusUp => ws.mux.focus_dir(area, 0, -1),
            Action::FocusDown => ws.mux.focus_dir(area, 0, 1),
            Action::ResizeLeft => ws.mux.resize_focus(Dir::Horizontal, -0.03),
            Action::ResizeRight => ws.mux.resize_focus(Dir::Horizontal, 0.03),
            Action::ResizeUp => ws.mux.resize_focus(Dir::Vertical, -0.03),
            Action::ResizeDown => ws.mux.resize_focus(Dir::Vertical, 0.03),
            Action::Copy => {
                // Cycle 609 (Terminator parity, terminatorlib/config.py
                // `smart_copy` + terminal.py:real_copy_clipboard):
                //   smart_copy = true  (default) → if no selection, skip
                //     the clipboard write so the existing clipboard
                //     content is preserved. Lets a user Ctrl+Shift+C
                //     without losing a previous copy when they forgot
                //     to highlight first.
                //   smart_copy = false → always trigger the copy. If
                //     there's no selection, write empty — this CLOBBERS
                //     the clipboard. Deliberate UX choice for users who
                //     want Ctrl+Shift+C to consistently mean "the
                //     clipboard now reflects the current selection
                //     (empty or not)" without smart heuristics.
                // Pre-cycle-609 kettle hardcoded smart_copy = true.
                let selection_text = ws.mux.focused().and_then(|p| {
                    p.term
                        .term
                        .lock()
                        .ok()
                        .and_then(|t| t.selection_to_string())
                });
                let payload =
                    copy_clipboard_decision(selection_text.as_deref(), self.cfg.smart_copy);
                let mut copied = false;
                if let Some(s) = payload
                    && let Some(cb) = &mut self.clipboard
                {
                    let had_selection = !s.is_empty();
                    // Cycle 777: log instead of silently swallowing.
                    if let Err(e) = cb.set_text(s) {
                        log::warn!("clipboard set_text failed (copy): {e}");
                    }
                    // Only treat it as a "real" copy when something was
                    // actually selected — the smart_copy = false
                    // clobber path writes empty but shouldn't clear a
                    // (nonexistent) selection.
                    copied = had_selection;
                }
                // Cycle 333 (Terminator parity, terminatorlib/config.py:91
                // `clear_select_on_copy`): if the config asked, drop the
                // selection so the user sees the copy "took". Default
                // false matches Terminator's default — the selection
                // stays so re-Copy still works.
                if copied && self.cfg.clear_select_on_copy {
                    self.clear_selection_on_input(ws);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
            Action::Paste => self.paste_clipboard(ws),
            Action::IncreaseFontSize | Action::DecreaseFontSize | Action::ResetFontSize => {
                if let Some(r) = ws.renderer.as_mut() {
                    // Cycle 747: step the logical font size directly. Back-
                    // deriving from `r.cell_h` (now physical-px after the DPI
                    // fix) would double-apply the scale factor on HiDPI.
                    let new = match action {
                        Action::IncreaseFontSize => r.font_size() + 1.0,
                        Action::DecreaseFontSize => (r.font_size() - 1.0).max(6.0),
                        _ => self.cfg.font_size,
                    };
                    r.set_font_size(new);
                }
                // Cycle 936: the user changed the font OUTSIDE a scaled-zoom
                // enter/exit, so the saved pre-zoom size is now stale — drop it
                // so a later zoom-out keeps this new chosen size instead of
                // reverting to the old one.
                ws.scaled_zoom_prev_font_size = None;
            }
            Action::StartSearch => {
                // Cycle 154: close any other modal first so we don't
                // stack two visible overlays. (Opening only sets one
                // of the four state fields; the others would stay
                // None already on the happy path, but defending in
                // depth here lets a future "open X without closing"
                // bug stay sane.)
                self.close_all_modals(ws);
                ws.mux.search.open = true;
                ws.mux.search.query.clear();
                ws.mux.search.matches.clear();
                ws.mux.search.index = 0;
                ws.search_revealed = None; // re-reveal on this new search
            }
            Action::ToggleBroadcastAll => {
                // Cycle 679: cycle-178 "broadcast-all" is actually
                // per-tab (the action's misnaming was a known
                // tech-debt). The Tab variant preserves the
                // existing UX exactly. The new All / Group
                // variants are reachable via the upcoming
                // GroupTab/GroupWindow/CreateGroup actions
                // (cycle 642 surface, dispatch follow-up).
                ws.mux.broadcast = crate::mux::BroadcastScope::Tab;
            }
            Action::ToggleBroadcastOff => {
                ws.mux.broadcast = crate::mux::BroadcastScope::Off;
            }
            Action::ToggleBroadcastGroup => {
                // Cycle 681 (named-groups sub-cycle 5): toggle
                // broadcast scope between Off and
                // Group(focused_pane.group_name). If focused
                // pane has no group, log + no-op.
                let focused_group = ws.mux.focused().and_then(|p| p.group_name.clone());
                let Some(group) = focused_group else {
                    log::info!(
                        "toggle-broadcast-group: focused pane has no group_name; \
                         use Action::CreateGroup or Action::GroupTab first"
                    );
                    return;
                };
                ws.mux.broadcast = match &ws.mux.broadcast {
                    crate::mux::BroadcastScope::Group(name) if name == &group => {
                        crate::mux::BroadcastScope::Off
                    }
                    _ => crate::mux::BroadcastScope::Group(group),
                };
            }
            Action::ToggleBroadcastWindow => {
                // Cycle 681 (named-groups sub-cycle 5): toggle
                // window-wide broadcast on/off. Distinct from
                // ToggleBroadcastAll (which is misnamed —
                // actually per-tab).
                ws.mux.broadcast = match &ws.mux.broadcast {
                    crate::mux::BroadcastScope::All => crate::mux::BroadcastScope::Off,
                    _ => crate::mux::BroadcastScope::All,
                };
            }
            Action::ToggleZoom => {
                ws.mux.toggle_zoom();
                self.resize_all(ws);
            }
            // v2.20.0 (Ghostty `equalize_splits` parity): rebalance the
            // active tab's split tree to equal pane areas, then push the new
            // geometry into the PTYs.
            Action::EqualizeSplits => {
                if let Some(t) = ws.mux.tabs.get_mut(ws.mux.active) {
                    t.root.equalize();
                }
                self.resize_all(ws);
                self.save_session(ws);
            }
            // Cycle 702 Terminator parity (`key_send_newline`).
            // Write a literal `\n` to the focused pane's PTY.
            // Useful for shell line-editors that consume Enter
            // normally but expect explicit `\n` for line
            // continuation (multi-line readline prompts).
            Action::SendNewline => {
                if let Some(p) = ws.mux.focused() {
                    // Cycle 941: typed-input semantics — read-only drops it.
                    p.feed_input(b"\n");
                }
            }
            // Cycle 696 Terminator parity (`key_preferences` /
            // `key_preferences_keybindings`). Terminator's GUI
            // Preferences dialog is config-file-driven for
            // kettle, so the preferences keybind opens the user's
            // config file in $EDITOR (or any registered handler
            // via `open::that_detached`). If no config file is
            // loaded — kettle started with `--config` pointing
            // at a missing path, or no profile resolved — falls
            // back to `Config::default_path()`. Closes the
            // "preferences GUI is a paradigm choice" Bucket E
            // rationale by making the equivalent UX one
            // keystroke away.
            Action::EditConfig => {
                let path = self
                    .config_path
                    .clone()
                    .or_else(kettle_config::Config::default_path);
                if let Some(path) = path {
                    if let Err(e) = open::that_detached(&path) {
                        log::warn!("Action::EditConfig: failed to open {}: {e}", path.display());
                    }
                } else {
                    log::warn!(
                        "Action::EditConfig: no config path resolved \
                         (set $XDG_CONFIG_HOME or pass --config)"
                    );
                }
            }
            // Cycle 717 (Preferences submenu, C8): runtime-mutable
            // toggles. Each dispatch (a) mutates `self.cfg` so the
            // change applies immediately + (b) writes the new
            // `key = value` back to the user's config file via the
            // cycle-716 `persist_config_toggle` helper (atomic
            // temp+rename, comment-preserving, with a first-write
            // backup at `<config>.bak`). Re-opening the menu picks
            // up the new state.
            Action::SetScrollbarAlways => {
                self.cfg.scrollbar = kettle_config::ScrollbarMode::Always;
                self.persist_pref("scrollbar", "always");
            }
            Action::SetScrollbarAuto => {
                self.cfg.scrollbar = kettle_config::ScrollbarMode::Auto;
                self.persist_pref("scrollbar", "auto");
            }
            Action::SetScrollbarNever => {
                self.cfg.scrollbar = kettle_config::ScrollbarMode::Never;
                self.persist_pref("scrollbar", "never");
            }
            Action::ToggleCursorBlink => {
                self.cfg.cursor_blink = !self.cfg.cursor_blink;
                self.persist_pref(
                    "cursor-blink",
                    if self.cfg.cursor_blink {
                        "true"
                    } else {
                        "false"
                    },
                );
            }
            Action::ToggleCopyOnSelect => {
                self.cfg.copy_on_select = !self.cfg.copy_on_select;
                self.persist_pref(
                    "copy-on-select",
                    if self.cfg.copy_on_select {
                        "true"
                    } else {
                        "false"
                    },
                );
            }
            Action::SetBellOff => {
                self.cfg.bell = kettle_config::BellMode::Off;
                self.persist_pref("bell", "off");
            }
            Action::SetBellVisual => {
                self.cfg.bell = kettle_config::BellMode::Visual;
                self.persist_pref("bell", "visual");
            }
            Action::SetBellAttention => {
                self.cfg.bell = kettle_config::BellMode::Attention;
                self.persist_pref("bell", "attention");
            }
            Action::SetBellBoth => {
                self.cfg.bell = kettle_config::BellMode::Both;
                self.persist_pref("bell", "both");
            }
            Action::ToggleMouseHide => {
                self.cfg.mouse_hide_while_typing = !self.cfg.mouse_hide_while_typing;
                self.persist_pref(
                    "mouse-hide-while-typing",
                    if self.cfg.mouse_hide_while_typing {
                        "true"
                    } else {
                        "false"
                    },
                );
            }
            // Cycle 695 Terminator parity (`key_help`).
            // Terminator's F1 opens its HTML manual via xdg-open;
            // kettle opens its README at the canonical GitHub URL
            // via the cycle-X `open::that_detached` dispatch path
            // (same one URL clicks already use, so it works on
            // Linux/macOS/Windows without spawning a per-platform
            // helper).
            Action::ShowHelp => {
                let url = "https://github.com/Reddimus/kettle#readme";
                if let Err(e) = open::that_detached(url) {
                    log::warn!("Action::ShowHelp: failed to open {url}: {e}");
                }
            }
            // Cycle 693 Terminator parity (`key_scaled_zoom`).
            // Toggle pane zoom + scale the font 1.5× so glyphs
            // grow with the enlarged pane area, then restore the
            // saved size on exit. Idempotent across other
            // `ToggleZoom` interactions: if the user toggles zoom
            // some other way and then hits ScaledZoom, the second
            // call still flips state correctly because we look at
            // the post-toggle zoom flag and pair save/restore via
            // a single `Option<f32>`.
            Action::ScaledZoom => {
                ws.mux.toggle_zoom();
                self.resize_all(ws);
                let now_zoomed = ws.mux.is_zoomed();
                if let Some(r) = ws.renderer.as_mut() {
                    if now_zoomed {
                        // Cycle 846 (audit): baseline off the LIVE renderer size,
                        // not `self.cfg.font_size`. Increase/DecreaseFontSize
                        // only call `r.set_font_size` (they never write
                        // `cfg.font_size`), so a prior manual zoom left the
                        // config value stale — ScaledZoom would otherwise scale
                        // from the original config size and, on exit, *discard*
                        // the user's manual zoom by restoring it.
                        let cur = r.font_size();
                        if ws.scaled_zoom_prev_font_size.is_none() {
                            ws.scaled_zoom_prev_font_size = Some(cur);
                        }
                        let new_size = (cur * 1.5).clamp(6.0, 96.0);
                        r.set_font_size(new_size);
                    } else if let Some(prev) = ws.scaled_zoom_prev_font_size.take() {
                        r.set_font_size(prev);
                    }
                }
            }
            Action::ToggleFullscreen => {
                ws.fullscreen = !ws.fullscreen;
                if let Some(w) = &ws.window {
                    w.set_fullscreen(if ws.fullscreen {
                        Some(Fullscreen::Borderless(None))
                    } else {
                        None
                    });
                }
            }
            Action::ClearHistory => {
                // CSI 3 J (ED 3) — clear scrollback only, keep the
                // visible screen and grid state. Distinct from Reset
                // (`\e c`, RIS) which wipes everything including
                // current screen contents. kitty / iTerm2 / WezTerm
                // all expose this as "Clear Scrollback" or similar.
                // Honors broadcast (cycle-173/174 invariant): when
                // group input is on, clear every pane's scrollback,
                // not just the focused one. The user pressing
                // clear_history with broadcast on intends "clean
                // slate for all the panes I'm typing into."
                // Cycle 942 (audit): route through feed_input — both branches
                // now agree that a read-only pane drops the clear (the
                // broadcast branch inherited the gate from broadcast_write;
                // the focused branch used to bypass it, a split-brain).
                if ws.mux.is_broadcast_on() {
                    ws.mux.broadcast_write(b"\x1b[3J");
                } else if let Some(p) = ws.mux.focused() {
                    p.feed_input(b"\x1b[3J");
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::Reset => {
                // RIS (`ESC c`) full-resets the engine: clears the grid,
                // restores DEC modes to defaults, drops the alt-screen
                // back. The PTY child then sees a fresh prompt next
                // time it draws. But kettle owns several pieces of UI
                // state OUTSIDE the engine — selection, scrollback
                // display-offset, and the modal overlays (search,
                // command palette, hint mode, SSH launcher) — and
                // those used to survive a "reset" chord, leaving a
                // half-cleared screen with stale highlight or an open
                // modal floating over a freshly-reset terminal.
                // Sweep them too so the chord really does mean
                // "fresh start". Matches Alacritty's `Reset` action.
                // Cycle 942 (audit): feed_input — injecting ESC c into a
                // read-only pane's child (e.g. a locked agent TUI, where ESC
                // is the interrupt key) is exactly what the toggle prevents.
                if let Some(p) = ws.mux.focused() {
                    p.feed_input(b"\x1bc");
                }
                self.clear_selection_on_input(ws);
                // Cycle 111's modal sweep, extracted to a helper in
                // cycle 154 so the modal-opening actions can reuse it.
                self.close_all_modals(ws);
                // Cycle 134: also reset the blink phase so the cursor
                // is immediately visible. Without this, hitting Reset
                // right as `blink_on` was false left the user staring
                // at a missing cursor for up to one blink interval —
                // confusing, because Reset is the chord users hit to
                // recover from a visually-jammed terminal. Shares
                // `reset_blink_phase` with cycle-135 focus-change and
                // cycle-140 modal-close paths.
                self.reset_blink_phase(ws);
            }
            Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollLineUp
            | Action::ScrollLineDown
            | Action::ScrollToTop
            | Action::ScrollToBottom => {
                if let Some(p) = ws.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    t.scroll_display(match action {
                        Action::ScrollPageUp => Scroll::PageUp,
                        Action::ScrollPageDown => Scroll::PageDown,
                        // `Scroll::Delta(+n)` scrolls *back* (toward older
                        // lines) — same sign convention the mouse-wheel
                        // path uses; line-up = +1, line-down = -1.
                        Action::ScrollLineUp => Scroll::Delta(1),
                        Action::ScrollLineDown => Scroll::Delta(-1),
                        Action::ScrollToTop => Scroll::Top,
                        _ => Scroll::Bottom,
                    });
                }
            }
            Action::JumpPrevPrompt | Action::JumpNextPrompt => {
                let prev = matches!(action, Action::JumpPrevPrompt);
                if let Some(p) = ws.mux.focused() {
                    let marks = p.term.prompt_marks();
                    if let Ok(mut t) = p.term.term.lock() {
                        use kettle_core::Dimensions;
                        let hist = t.grid().history_size() as i64;
                        let off = t.grid().display_offset() as i64;
                        let top = hist - off;
                        let target = if prev {
                            marks.iter().filter(|&&m| m < top).max().copied()
                        } else {
                            marks.iter().filter(|&&m| m > top).min().copied()
                        };
                        if let Some(m) = target {
                            let new_off = (hist - m).clamp(0, hist);
                            let delta = (new_off - off) as i32;
                            if delta != 0 {
                                t.scroll_display(Scroll::Delta(delta));
                            }
                        }
                    }
                }
            }
            Action::OpenSsh => {
                self.close_all_modals(ws);
                ws.ssh_input = Some(String::new());
            }
            Action::CommandPalette => {
                self.close_all_modals(ws);
                ws.palette_input = Some((String::new(), 0));
            }
            // Cycle 756: open the in-app settings overlay (Ctrl+, / right-click
            // → Settings / palette "Open settings").
            Action::OpenSettings => {
                self.close_all_modals(ws);
                // v2.23.0: enumerate GPUs once for the Graphics device picker
                // (cached for the session; a wgpu adapter walk is too heavy to
                // repeat per frame). Re-enumerate only if we never have.
                if self.gpu_choices.is_empty() {
                    self.gpu_choices = kettle_render::detect_gpus()
                        .into_iter()
                        .map(|g| {
                            (
                                format!("{:x}:{:x}:{}", g.vendor, g.device, g.name),
                                format!("{} ({})", g.name, g.kind),
                            )
                        })
                        .collect();
                }
                ws.settings_restart_pending = false;
                ws.settings_nav = Some(crate::settings::SettingsNav::default());
            }
            // Cycle 708 (Terminator parity, layoutlauncher.py):
            // open the runtime layout picker. Empty layouts dir
            // is fine — the modal still opens with a "no
            // matching layout" hint, so the user gets a clear
            // "I have no saved layouts yet; save one with
            // `kettle --save-layout NAME`" affordance.
            Action::OpenLayoutPicker => {
                self.close_all_modals(ws);
                ws.layout_picker_input = Some((String::new(), 0));
            }
            Action::HintMode => {
                let targets = self.collect_hints(ws);
                if !targets.is_empty() {
                    self.close_all_modals(ws);
                    ws.hint_state = Some((targets, String::new()));
                }
            }
            Action::ToggleViMode => {
                // Cycle 298 vi-mode (Alacritty parity), sub-cycle 1
                // of 4. Foundation: toggle entry / exit. Visible
                // block cursor at the focused pane's current cursor
                // position; Esc also exits (handled in keyboard
                // dispatch). h/j/k/l movement + visual selection +
                // yank land in sub-cycles 2-4.
                if ws.vi_mode.is_some() {
                    ws.vi_mode = None;
                } else {
                    self.close_all_modals(ws);
                    // Seed cursor at the focused pane's current
                    // terminal cursor position. h/j/k/l will move
                    // around this in sub-cycle 2.
                    let (row, col) = ws
                        .mux
                        .focused()
                        .and_then(|p| {
                            p.term.term.lock().ok().map(|t| {
                                let cursor = t.grid().cursor.point;
                                (cursor.line.0.max(0) as usize, cursor.column.0)
                            })
                        })
                        .unwrap_or((0, 0));
                    ws.vi_mode = Some(ViState {
                        row,
                        col,
                        visual_anchor: None,
                    });
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::OpenContextMenu => {
                // Keyboard-triggered open: anchor at the current mouse
                // position so the menu lands where the user is looking;
                // falls back to the center of the focused pane when
                // dispatched programmatically (e.g. from the palette).
                let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                self.open_context_menu(ws, px, py);
            }
            Action::UndoCloseTab => {
                let waker = self.waker();
                match ws.mux.undo_close_tab(&self.cfg, cols, rows, cw, ch, waker) {
                    Ok(true) => {
                        self.resize_all(ws);
                        self.save_session(ws);
                    }
                    Ok(false) => {
                        log::debug!("undo_close_tab: ring is empty, nothing to restore");
                    }
                    Err(e) => log::error!("undo_close_tab failed: {e}"),
                }
            }
            Action::DuplicateTab => {
                let waker = self.waker();
                if let Err(e) = ws
                    .mux
                    .duplicate_focused_tab(&self.cfg, cols, rows, cw, ch, waker)
                {
                    log::error!("duplicate_tab failed: {e}");
                } else {
                    self.resize_all(ws);
                    self.save_session(ws);
                }
            }
            Action::DuplicatePane => {
                let waker = self.waker();
                if let Err(e) = ws.mux.duplicate_focused_pane(
                    crate::mux::Dir::Horizontal,
                    &self.cfg,
                    cols,
                    rows,
                    cw,
                    ch,
                    waker,
                ) {
                    log::error!("duplicate_pane failed: {e}");
                } else {
                    self.resize_all(ws);
                }
            }
            Action::NextTheme | Action::PrevTheme => {
                let fwd = matches!(action, Action::NextTheme);
                let name = kettle_config::Theme::cycle(&self.cfg.theme_name, fwd);
                self.cfg.theme_name = name.to_string();
                self.cfg.theme = kettle_config::Theme::by_name(name);
                if !self.persist_pref("theme", name) {
                    // cycle 918: config-governed; cycle 919 (L4): notify on failure
                    fire_notify(
                        "kettle: theme not saved",
                        "Applied for this session — couldn't write it to your config file.",
                    );
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::ToggleLightDark => {
                if let Some(next) = pick_light_dark_target(
                    &self.cfg.theme_name,
                    &self.cfg.light_theme,
                    &self.cfg.dark_theme,
                ) {
                    self.cfg.theme_name = next.clone();
                    self.cfg.theme = kettle_config::Theme::by_name(&next);
                    if !self.persist_pref("theme", &next) {
                        // cycle 918: config-governed; cycle 919 (L4): notify on failure
                        fire_notify(
                            "kettle: theme not saved",
                            "Applied for this session — couldn't write it to your config file.",
                        );
                    }
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                } else {
                    log::warn!(
                        "toggle-light-dark: needs both light-theme and dark-theme in config (current: light={:?} dark={:?})",
                        self.cfg.light_theme,
                        self.cfg.dark_theme,
                    );
                }
            }
            Action::ToggleSessionLog => {
                // Cycle 621 (Terminator parity, `plugins/logger.py`):
                // toggle the focused pane's session log. Pure helper
                // computes the file path; this arm does the I/O.
                // v2.20.0 P7: routed through `Terminal::set_log_file` so the
                // reader thread's lock-skip flag stays in sync.
                if let Some(pane) = ws.mux.focused() {
                    if pane.term.log_enabled() {
                        // Drop the file handle to stop logging.
                        pane.term.set_log_file(None);
                        log::info!("toggle-session-log: stopped");
                    } else {
                        let secs = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let cache = cache_dir_from_env(|k| std::env::var(k).ok());
                        let path = session_log_path(secs, std::process::id(), cache.as_deref());
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        match std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                        {
                            Ok(f) => {
                                log::info!("toggle-session-log: writing to {}", path.display());
                                // Cycle 625: propagate the config's
                                // strip-ANSI choice to the reader
                                // thread's per-Terminal flag BEFORE the
                                // log goes live, so the first logged
                                // read already honors it.
                                if let Ok(mut strip) = pane.term.log_strip_ansi.lock() {
                                    *strip = self.cfg.log_strip_ansi;
                                }
                                pane.term.set_log_file(Some(f));
                            }
                            Err(e) => log::warn!(
                                "toggle-session-log: open {} failed: {e}",
                                path.display()
                            ),
                        }
                    }
                }
            }
            Action::TakeScreenshot => {
                // Cycle 654 + 688 + 689 (terminalshot sub-cycles
                // 3/4/5). Compute the output path, queue the
                // request, then fire a desktop notification so the
                // user knows where to look.
                // Cycle 690 (terminalshot sub-cycle 6): compute
                // the focused pane's rect at dispatch time so the
                // screenshot crops to just that pane. Computed
                // BEFORE the `&mut ws.renderer` borrow to keep
                // the borrow window narrow.
                let area = self.area(ws);
                let active = ws.mux.active;
                let focus_id = ws.mux.tabs.get(active).map(|t| t.focus).unwrap_or(0);
                let crop = ws
                    .mux
                    .layout(active, area)
                    .into_iter()
                    .find(|(id, _)| *id == focus_id)
                    .map(|(_, rect)| rect);
                if let Some(renderer) = ws.renderer.as_mut() {
                    let secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let cache = cache_dir_from_env(|k| std::env::var(k).ok());
                    let path = session_screenshot_path(secs, std::process::id(), cache.as_deref());
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    log::info!("take_screenshot: queued → {}", path.display());
                    let path_str = path.display().to_string();
                    renderer.set_pending_screenshot(kettle_render::ScreenshotRequest {
                        out_path: path,
                        crop,
                        completion: None,
                    });
                    // Cycle 689 (sub-cycle 5): toast notification.
                    // Optimistic — we fire BEFORE the GPU readback
                    // completes (which happens on the next frame).
                    // If the capture fails the notification is a
                    // mild lie, but capture failures are rare
                    // (would require GPU/disk I/O error) and the
                    // log::warn from capture_live_surface surfaces
                    // them in --debug runs.
                    fire_notify("kettle: screenshot queued", &path_str);
                }
            }
            Action::ReloadConfig => self.reload_config(ws),
            Action::MoveTabLeft => {
                ws.mux.move_active_tab(-1);
            }
            Action::MoveTabRight => {
                ws.mux.move_active_tab(1);
            }
            Action::NewTabShell(n) => {
                // Dropdown-parity cycle: Ctrl+Shift+N opens the Nth dropdown
                // entry (Windows Terminal's profile shortcuts). The list is
                // process-cached + prewarmed; out-of-range is a silent no-op,
                // same as GotoTab clamping.
                let shells = kettle_core::term::detect_shells();
                if let Some((_, argv)) = shells.get(n as usize) {
                    let argv = argv.clone();
                    self.open_tab_with_argv(ws, &argv);
                }
            }
            Action::About => self.open_about_panel(ws),
            Action::GotoTab(n) => {
                let i = n as usize;
                if i < ws.mux.tabs.len() {
                    ws.mux.active = i;
                    ws.mux.touch_active_tab_seen();
                }
            }
            // Cycle 809 (audit): keyboard equivalents of clicking the cycle-794
            // update banner — `OpenUpdate` opens the release page + dismisses,
            // `DismissUpdate` just dismisses. Both no-op (debug-logged) when no
            // banner is showing, so a bound key is harmless the rest of the time.
            Action::OpenUpdate => {
                if !self.act_on_update_banner(ws, true) {
                    log::debug!("open_update: no update banner is showing");
                }
            }
            Action::DismissUpdate => {
                if !self.act_on_update_banner(ws, false) {
                    log::debug!("dismiss_update: no update banner is showing");
                }
            }
            // Cycle 345 Terminator-parity behavior wiring (continued
            // from cycle 342's stubs). Each branch implements the
            // Terminator key_<name> behavior in kettle-idiomatic
            // shape.
            //
            // Still stubbed (overlay-required; future sub-cycles):
            //   RotateCw / RotateCcw (split-tree rotation in Mux)
            //   ToggleScrollbar (runtime scrollbar toggle)
            //   EditWindowTitle / EditTabTitle / EditPaneTitle
            //   NextProfile / PrevProfile (runtime profile cycle)
            // Cycle 369 (Terminator parity, replaces cycle-354
            // placeholders): real Edit-title overlay. Each action
            // opens the overlay pre-filled with the current title.
            // Enter applies via the appropriate setter; Esc cancels.
            // Render is a thin bar at the top of the window (similar
            // shape to cycle-X's command palette overlay).
            Action::EditWindowTitle => {
                self.close_all_modals(ws);
                let current = ws.last_title.clone();
                ws.editing_title = Some(TitleEditState {
                    scope: TitleEditScope::Window,
                    input: current,
                    bulk: GroupBulkScope::Single,
                });
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::EditTabTitle => {
                self.close_all_modals(ws);
                let current = ws
                    .mux
                    .tabs
                    .get(ws.mux.active)
                    .and_then(|t| t.title_override.clone())
                    .unwrap_or_default();
                ws.editing_title = Some(TitleEditState {
                    scope: TitleEditScope::Tab,
                    input: current,
                    bulk: GroupBulkScope::Single,
                });
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::EditPaneTitle => {
                self.close_all_modals(ws);
                let current = ws
                    .mux
                    .focused()
                    .map(|p| p.title.clone())
                    .unwrap_or_default();
                ws.editing_title = Some(TitleEditState {
                    scope: TitleEditScope::Pane,
                    input: current,
                    bulk: GroupBulkScope::Single,
                });
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::EditPaneGroup | Action::CreateGroup => {
                // Cycle 407 + cycle 642: edit the focused pane's
                // broadcast-group name. Empty input → clear the
                // group. Same overlay mechanism as cycle-369
                // EditPaneTitle. `CreateGroup` (Terminator name)
                // and `EditPaneGroup` (kettle name) share dispatch.
                self.close_all_modals(ws);
                let current = ws
                    .mux
                    .focused()
                    .and_then(|p| p.group_name.clone())
                    .unwrap_or_default();
                ws.editing_title = Some(TitleEditState {
                    scope: TitleEditScope::Group,
                    input: current,
                    bulk: GroupBulkScope::Single,
                });
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::GroupTab | Action::GroupWindow => {
                // Cycle 680 (named-groups sub-cycle 4): open the
                // title-edit overlay with `bulk` set to Tab/Window
                // so on Apply the typed name writes to every pane
                // in scope.
                self.close_all_modals(ws);
                let bulk = if matches!(action, Action::GroupTab) {
                    GroupBulkScope::Tab
                } else {
                    GroupBulkScope::Window
                };
                ws.editing_title = Some(TitleEditState {
                    scope: TitleEditScope::Group,
                    input: String::new(),
                    bulk,
                });
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::UngroupTab | Action::UngroupWindow => {
                // Cycle 680 (named-groups sub-cycle 4): bulk-
                // clear the group on every pane in scope. No
                // overlay needed — empty input is the "clear"
                // signal, and the action carries the scope.
                let pane_ids: Vec<u64> = if matches!(action, Action::UngroupTab) {
                    ws.mux
                        .tabs
                        .get(ws.mux.active)
                        .map(|t| t.root.leaf_ids())
                        .unwrap_or_default()
                } else {
                    ws.mux.panes.keys().copied().collect()
                };
                for id in pane_ids {
                    if let Some(p) = ws.mux.panes.get_mut(&id) {
                        p.group_name = None;
                    }
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            // Cycle 348 (Terminator parity, terminatorlib/terminal.py:
            // key_next_profile + key_previous_profile): runtime cycle
            // through profile files at <config-dir>/profiles/.
            //
            // Enumerates the profiles directory each dispatch (cheap;
            // typically <10 entries), sorts deterministically, finds
            // the current entry by basename match against the loaded
            // config-path, and loads the next/prev. Falls back to
            // log::info when no profiles directory or no entries.
            Action::NextProfile | Action::PrevProfile => {
                // Cycle 618: delegate listing + name extraction to
                // kettle-config so the same path math has a single
                // home (and drift guards on it). Empty list → no-op
                // with a one-line info nudge.
                let names = kettle_config::Config::list_profiles();
                if names.is_empty() {
                    log::info!(
                        "{action:?}: no profiles in <config-dir>/profiles/ — \
                         create one with `kettle --print-default-config > \
                         ~/.config/kettle/profiles/dev.config`"
                    );
                } else {
                    let current = self
                        .config_path
                        .as_deref()
                        .and_then(kettle_config::Config::profile_name_from_path);
                    let next = pick_next_profile(
                        current.as_deref(),
                        &names,
                        matches!(action, Action::NextProfile),
                    );
                    if let Some(p) = kettle_config::Config::path_for_profile(&next) {
                        self.config_path = Some(p);
                        self.reload_config(ws);
                    }
                }
            }
            // Cycle 347: split-tree rotation. RotateCw flips dir +
            // swaps children (Terminator's clockwise semantics);
            // RotateCcw flips dir without swap. No-op when the
            // focused leaf has no parent (single-pane tab).
            Action::RotateCw => {
                ws.mux.rotate_focused_split(true);
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::RotateCcw => {
                ws.mux.rotate_focused_split(false);
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            // Cycle 346: runtime scrollbar toggle. Cycles
            // ScrollbarMode through Never → Always → Auto → Never.
            // Three-state cycle (vs binary) because Auto is the
            // useful steady state for most users; explicit toggle
            // is for power users with a specific preference. Same
            // shape as cycle-X's NextTheme cycle.
            Action::ToggleScrollbar => {
                use kettle_config::ScrollbarMode::*;
                self.cfg.scrollbar = match self.cfg.scrollbar {
                    Never => Always,
                    Always => Auto,
                    Auto => Never,
                };
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            // Cycle 941 (Terminator parity): toggle the focused pane's read-only
            // state. While on, user input (keystrokes / paste / broadcast) is
            // dropped before it reaches the PTY; the child keeps producing
            // output. A `[RO]` titlebar badge shows the state.
            Action::TogglePaneReadOnly => {
                let _ = ws.mux.toggle_focused_read_only();
                // The `[RO]` titlebar badge reflects the new state.
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            // Cycle 345: broadcast zoom. kettle's font-size is
            // window-wide (not per-pane like VTE's per-terminal
            // scale), so zoom-all has the same effect as the
            // existing single-pane zoom. Compose by reusing the
            // IncreaseFontSize / DecreaseFontSize / ResetFontSize
            // arm — same shape as ResetAndClear.
            Action::ZoomInAll => {
                if let Some(r) = ws.renderer.as_mut() {
                    // Cycle 747: step logical size (see IncreaseFontSize).
                    r.set_font_size(r.font_size() + 1.0);
                }
            }
            Action::ZoomOutAll => {
                if let Some(r) = ws.renderer.as_mut() {
                    r.set_font_size((r.font_size() - 1.0).max(6.0));
                }
            }
            Action::ZoomNormalAll => {
                if let Some(r) = ws.renderer.as_mut() {
                    r.set_font_size(self.cfg.font_size);
                }
            }
            // Cycle 345: insert pane index. Pane index is 1-based
            // (matches Terminator's GotoTab + every user-facing
            // numbering). InsertPanePadded uses 2-digit zero-padded
            // form (Terminator default).
            Action::InsertPaneNumber => {
                let idx = ws
                    .mux
                    .focused_pane_index_in_tab()
                    .map(|i| i + 1)
                    .unwrap_or(1);
                if let Some(p) = ws.mux.focused() {
                    p.feed_input(idx.to_string().as_bytes());
                }
            }
            Action::InsertPanePadded => {
                let idx = ws
                    .mux
                    .focused_pane_index_in_tab()
                    .map(|i| i + 1)
                    .unwrap_or(1);
                if let Some(p) = ws.mux.focused() {
                    p.feed_input(format!("{idx:02}").as_bytes());
                }
            }
            // Cycle 606 Terminator parity (`insert_term_name.py`
            // plugin → `InsertTermName` menu item + keybind). Send
            // the focused pane's title (Pane::title — same string
            // the chrome shows in the per-pane titlebar) to the
            // PTY. Useful for scripts that want to label their
            // output by source pane, or for keyboard workflows
            // that re-type the current title into the command line.
            Action::InsertPaneName => {
                if let Some(p) = ws.mux.focused() {
                    let title = p.title.clone();
                    p.feed_input(title.as_bytes());
                }
            }
            // Cycle 607 Terminator parity (`dir_open.py` plugin →
            // `CurrDirOpen` menu item). Open the focused pane's
            // current working directory in the OS file manager.
            // Builds `file://<cwd>` and routes through `open_url`
            // so the cycle-374 Lua URL-handler dispatch + the
            // cycle-X custom-url-handler config + the
            // `is_safe_url` allowlist (which accepts `file://`
            // without `..`) all apply consistently. Identical
            // shape to clicking a `file://...` hyperlink in pane
            // output — re-uses the safety policy for free.
            Action::OpenCwdInFileManager => {
                match ws.mux.focused().and_then(|p| p.term.current_dir()) {
                    // Cycle 816 (audit): refuse a non-local OSC 7 cwd before
                    // building/opening the URL (it's untrusted PTY input — a
                    // UNC path would trigger an SMB/NTLM leak on Windows).
                    Some(cwd) if cwd_is_local(&cwd) => {
                        self.open_url(&format!("file://{cwd}"));
                    }
                    Some(_) => {
                        log::warn!(
                            "Action::OpenCwdInFileManager: refusing a non-local cwd \
                             reported via OSC 7 (possible UNC/SSRF payload)"
                        );
                    }
                    None => {
                        log::info!(
                            "Action::OpenCwdInFileManager: focused pane has no OSC 7 cwd \
                             — set up shell integration with `kettle --shell-integration bash`"
                        );
                    }
                }
            }
            // Cycle 345: half-page scroll. Same shape as cycle-X's
            // ScrollPageUp/Down handler but with half the row count.
            // Pull the row count from the focused pane's grid
            // dimensions (cycle-X pattern; works for any pane size).
            Action::ScrollPageUpHalf | Action::ScrollPageDownHalf => {
                if let Some(p) = ws.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    use kettle_core::Dimensions;
                    let rows = t.screen_lines() as i32;
                    let half = (rows / 2).max(1);
                    let dir = if matches!(action, Action::ScrollPageUpHalf) {
                        half
                    } else {
                        -half
                    };
                    t.scroll_display(Scroll::Delta(dir));
                }
            }
            // Cycle 755: paste the X11 PRIMARY selection (middle-click). On X11
            // PRIMARY is the last mouse-highlighted text, distinct from the
            // CLIPBOARD; `paste_primary` reads PRIMARY on Linux and falls back
            // to the clipboard on Wayland/macOS/Windows. It shares `paste_text`
            // so the LOCAL_PASTE_MAX clamp, bracketed-paste wrap, and broadcast
            // scoping all match Action::Paste.
            Action::PastePrimary => self.paste_primary(ws),
            // Cycle 345: in-process Quake toggle. Same tri-state
            // logic as cycle-319's --toggle remote command:
            //   hidden → show + focus
            //   visible + focused → hide
            //   visible + !focused → focus (don't hide)
            Action::ToggleWindowVisibility => {
                if let Some(w) = &ws.window {
                    let visible = w.is_visible().unwrap_or(true);
                    let focused = w.has_focus();
                    if !visible {
                        w.set_visible(true);
                        w.focus_window();
                    } else if focused {
                        w.set_visible(false);
                    } else {
                        w.focus_window();
                    }
                }
            }
            // Cycle 384 (Terminator parity, detachable-tabs Bucket-D
            // Wayland-fallback). Spawn a NEW kettle process with the
            // focused pane's cwd as its starting dir, then close the
            // source tab. Running shells in the source tab stay
            // alive in the original window (cross-process PTY
            // transfer needs SCM_RIGHTS — multi-cycle full impl).
            //
            // For now: just open a fresh kettle in the same cwd.
            // This gives the user the "move this work to a new
            // window" UX path Terminator's detachable_tabs ships.
            Action::MoveTabToNewWindow => {
                // C5 (multi-window): LIVE in-process move — the tab's panes
                // (PTYs, scrollback, running programs) transfer untouched to
                // a brand-new window via detach_tab → open_window(AdoptTab).
                // Replaces the cycle-405/410 serialize-and-respawn handoff
                // (SCM_RIGHTS socketpair on Unix / one-shot JSON file
                // elsewhere), which never transferred live PTYs — the target
                // process respawned the shells from argv+cwd, losing running
                // programs. The receive-side `--tab-handoff` parsing stays
                // one release for an upgrade-in-flight old sender.
                if !self.cfg.detachable_tabs {
                    log::info!("move_tab_to_new_window ignored because detachable-tabs = false");
                    return;
                }
                if ws.mux.tabs.len() <= 1 {
                    // Moving a lone tab out of its window is a no-op: you'd
                    // get the same window back.
                    return;
                }
                let closing_idx = ws.mux.active;
                let Some(dt) = ws.mux.detach_tab(closing_idx) else {
                    return;
                };
                match self.open_window(event_loop, WindowOpen::AdoptTab(dt), None, None) {
                    Ok(_) => {
                        // The tab LEFT this window; plugins see the same
                        // close event the process-handoff path fired
                        // (cycle 424).
                        self.fire_tab_close_event(closing_idx);
                        // C8: agents subscribed to the event feed see moves.
                        self.ctl_broadcast(
                            "tab_moved",
                            None,
                            serde_json::json!({
                                "from_window": ws.seq,
                                "to_window": self.focused_seq,
                                "tab": closing_idx,
                            }),
                        );
                        self.resize_all(ws);
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                    }
                    Err(WindowOpen::AdoptTab(dt)) => {
                        // Window creation failed — put the live tab back
                        // exactly where it was; nothing is lost.
                        log::warn!(
                            "MoveTabToNewWindow: open_window failed; tab kept in source window"
                        );
                        ws.mux.attach_tab(dt, Some(closing_idx));
                    }
                    Err(_) => unreachable!("open_window returns the WindowOpen it was given"),
                }
            }
            Action::ResetAndClear => {
                // Cycle 342 Terminator parity (key_reset_clear):
                // Reset (RIS, \ec) + ClearHistory (CSI 3 J) composed
                // into a single keybind. The two byte writes go to
                // the existing PTY-write path; the engine handles
                // them the same as cycle-X's separate Reset +
                // ClearHistory actions.
                // Cycle 942 (audit): feed_input, same read-only rule as the
                // separate Reset + ClearHistory arms.
                if let Some(p) = ws.mux.focused()
                    && p.feed_input(b"\x1bc")
                {
                    p.feed_input(b"\x1b[3J");
                }
            }
        }
        // Cycle 135 (cont.): if focus moved as a result of the action,
        // land the cursor visible on the new pane right away.
        self.note_focus_change(ws, pre_focus);
        self.resize_all(ws);
        self.save_session(ws);
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// C7 (multi-window): snapshot ONE window (its tabs + geometry).
    fn snapshot_window(w: &WindowState) -> crate::session::SWindow {
        let s = w.mux.snapshot();
        let geometry = w.window.as_ref().and_then(|win| {
            let pos = win.outer_position().ok()?;
            let size = win.inner_size();
            Some(crate::session::SGeometry {
                x: pos.x,
                y: pos.y,
                w: size.width,
                h: size.height,
            })
        });
        crate::session::SWindow {
            tabs: s.tabs,
            active: s.active,
            geometry,
        }
    }

    fn save_session(&self, ws: &WindowState) {
        // C7 (multi-window): serialize EVERY live window — the checked-out
        // one plus the map — ordered by seq so window 1 stays first. Windows
        // whose mux is already empty (a close_window in flight) are dropped:
        // nothing to restore there.
        let mut wins: Vec<(u64, crate::session::SWindow)> =
            vec![(ws.seq, Self::snapshot_window(ws))];
        for w in self.windows.values() {
            wins.push((w.seq, Self::snapshot_window(w)));
        }
        wins.sort_by_key(|(seq, _)| *seq);
        let windows: Vec<crate::session::SWindow> = wins
            .into_iter()
            .map(|(_, w)| w)
            .filter(|w| !w.tabs.is_empty())
            .collect();
        // Downgrade-compat dual-write: mirror window 1 into the legacy
        // top-level fields so an older kettle reading this file still
        // restores its first window.
        let mut s = crate::session::Session {
            tabs: windows.first().map(|w| w.tabs.clone()).unwrap_or_default(),
            active: windows.first().map(|w| w.active).unwrap_or(0),
            theme: None,
            windows,
        };
        // Cycle 918: theme is config-governed (persisted to the config file via
        // `persist_pref`), NOT stored in the session. A session-pinned theme used
        // to OVERRIDE the config/compile-time default on restore, so a default
        // change (or a fresh-config user) silently kept the old theme.
        s.theme = None;
        // Cycle 291: when launched with `--layout NAME`, save to the
        // named-layout file instead of the default session.json. Lets
        // the user maintain distinct workspaces ("dev", "ops", "docs")
        // without each one clobbering the others on close.
        // Cycle 919 (audit M1): only write the DEFAULT session.json when this
        // launch is in restore mode — symmetric with the opt-in load gate. A
        // fresh (non-opted-in) window must NOT overwrite the saved layout that
        // `--restore` / `restore-session = true` exists to recover.
        match &self.startup.layout {
            Some(name) => s.save_layout(name),
            None if should_restore_session(self.startup.restore, self.cfg.restore_session) => {
                s.save()
            }
            None => {}
        }
    }

    /// Cycle 666 (sub-cycle 5 of [`TERMINATOR-AUTO-THEME-DESIGN.md`](
    /// ../../../docs/TERMINATOR-AUTO-THEME-DESIGN.md)): poll the
    /// clock-schedule (when `cfg.theme_schedule` is `Some(Clock { … })`)
    /// and flip the theme between `light_theme` and `dark_theme` on
    /// boundary crossings.
    ///
    /// Cheap: a few u32 comparisons via cycle-664's
    /// `schedule_decision_clock`, run from `redraw()` per tick.
    /// State on `App::last_schedule_decision` means a single
    /// boundary fires the swap once; sub-cycles can stretch the
    /// throttling later if needed.
    /// Cycle 703 (Terminator plugin parity, plugin sub-cycle:
    /// `LuaEvent::PaneFocus`). Detect focus boundary crossings —
    /// from any source (keybind, mouse click, new tab, close tab,
    /// remote-control IPC) — and emit a single `PaneFocus` event
    /// per crossing.
    ///
    /// Polled from `redraw()` per tick rather than wiring every
    /// focus-changing call site because (a) there are 6+
    /// such sites today and (b) future cycles will add more.
    /// One diff site = one drift guard, not N.
    ///
    /// First tick after startup emits with `previous = None` so
    /// plugins can seed their state.
    fn poll_focus_event(&mut self, ws: &mut WindowState) {
        let current = ws.mux.active_focus();
        let Some(cur_id) = current else { return };
        if ws.last_emitted_focus == Some(cur_id) {
            return;
        }
        let prev = ws.last_emitted_focus;
        ws.last_emitted_focus = Some(cur_id);
        if let Some(eng) = self.lua_engine.as_ref() {
            eng.fire_event(&crate::LuaEvent::PaneFocus(prev, cur_id));
        }
        // Cycle 930 (agent-first A2): mirror the focus change to ctl subscribers.
        self.ctl_broadcast(
            "pane_focus",
            Some(cur_id),
            serde_json::json!({"previous": prev}),
        );
    }

    /// Cycle 745: reflect the FOCUSED pane's OSC 9;4 progress onto the OS
    /// taskbar button each frame (pwsh 7 / Windows Terminal parity). Reads the
    /// focused pane the same way the cursor-blink poll does; `Taskbar` dedups
    /// internally, so an unchanged value costs nothing. No-op off Windows.
    fn poll_taskbar_progress(&mut self, ws: &mut WindowState) {
        let progress = ws
            .mux
            .active_focus()
            .and_then(|id| ws.mux.panes.get(&id))
            .and_then(|p| p.term.progress());
        if let Some(window) = ws.window.clone() {
            ws.taskbar.apply(&window, progress);
        }
    }

    /// Cycle 704 (Terminator plugin parity, plugin sub-cycle:
    /// `LuaEvent::TitleChanged`). Walk live panes, diff each
    /// title against `ws.last_emitted_titles`, emit on any
    /// boundary cross. One pass site, regardless of how many
    /// title-mutating sites exist in App.
    ///
    /// O(n_panes) per redraw. Even 100 panes is trivial — a
    /// hash lookup + string compare per entry. Future cycles
    /// can add a "dirty-title" bitset on Mux if pane counts
    /// grow into the thousands.
    fn poll_title_event(&mut self, ws: &mut WindowState) {
        // Cycle 930 (agent-first A2): also run when a ctl subscriber is present,
        // so title changes reach agents even without a Lua engine.
        let has_subscribers = self
            .ctl
            .as_ref()
            .map(|c| c.has_subscribers())
            .unwrap_or(false);
        if self.lua_engine.is_none() && !has_subscribers {
            return;
        }
        let mut changes: Vec<(u64, String)> = Vec::new();
        for (id, p) in ws.mux.panes.iter() {
            let last = ws.last_emitted_titles.get(id);
            if last.map(|s| s.as_str()) != Some(p.title.as_str()) {
                changes.push((*id, p.title.clone()));
            }
        }
        for (id, title) in changes {
            ws.last_emitted_titles.insert(id, title.clone());
            if let Some(eng) = self.lua_engine.as_ref() {
                eng.fire_event(&crate::LuaEvent::TitleChanged(id, title.clone()));
            }
            self.ctl_broadcast("title", Some(id), serde_json::json!({"title": title}));
        }
        // Cycle 763: drop title state for panes that have closed so this map
        // can't grow unbounded over a long session of opening/closing panes
        // (it's only ever read for live panes). Covers every close path —
        // keybind, confirm dialog, tab close, reap. The O(1) length guard means
        // the O(n) retain runs only when there are actually stale entries.
        if ws.last_emitted_titles.len() > ws.mux.panes.len() {
            let panes = &ws.mux.panes;
            ws.last_emitted_titles
                .retain(|id, _| panes.contains_key(id));
        }
    }

    fn poll_theme_schedule(&mut self, ws: &mut WindowState) {
        let Some(schedule) = self.cfg.theme_schedule else {
            return;
        };
        // Compute now in local-ish HH:MM (UTC for v1 — same as the
        // cycle-296 status-bar clock; a future cycle could pick up
        // `$TZ` but no extra dep yet).
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let day_secs = secs % 86_400;
        let h = (day_secs / 3600) as u8;
        let m = ((day_secs % 3600) / 60) as u8;
        // Cycle 670: branch on the schedule variant.
        let is_dark = match schedule {
            kettle_config::ThemeSchedule::Clock { .. } => {
                kettle_config::schedule_decision_clock((h, m), schedule)
            }
            kettle_config::ThemeSchedule::SunriseSunset { lat, long } => {
                // Day-of-year approximation: days since unix
                // epoch mod 365. Good enough — sunrise/sunset
                // varies very slowly day-to-day, and we re-poll
                // every redraw tick anyway. Sub-cycle 8 could
                // refine with a real Gregorian-calendar
                // conversion if needed.
                let days_since_epoch = (secs / 86_400) as u32;
                let approx_doy = ((days_since_epoch % 365) + 1) as u16;
                kettle_config::schedule_decision_sunrise(day_secs as u32, approx_doy, lat, long)
            }
        };
        // Seed on first call so we don't flip the theme just for
        // existing on a "now's dark" tick — only boundary
        // crossings fire the swap.
        if self.last_schedule_decision == Some(is_dark) {
            return;
        }
        let was_first = self.last_schedule_decision.is_none();
        self.last_schedule_decision = Some(is_dark);
        if was_first {
            return;
        }
        // Boundary crossing: ask the cycle-649 resolve_theme_for_mode
        // what to switch to. We override the ThemeMode to Light/Dark
        // for the duration of the swap so the helper picks the
        // configured light/dark theme name.
        let target_mode = if is_dark {
            kettle_config::ThemeMode::Dark
        } else {
            kettle_config::ThemeMode::Light
        };
        if let Some(next) = kettle_config::resolve_theme_for_mode(
            target_mode,
            &self.cfg.theme_name,
            &self.cfg.light_theme,
            &self.cfg.dark_theme,
            None,
        ) {
            self.apply_theme_name(ws, "theme-schedule", next);
        }
    }

    fn apply_theme_name(&mut self, ws: &mut WindowState, source: &str, next: String) {
        log::info!("{source}: switching to {next}");
        self.cfg.theme_name = next.clone();
        self.cfg.theme = kettle_config::Theme::by_name(&next);
        self.save_session(ws);
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    fn os_theme_choice(&self, theme: WindowTheme) -> Option<String> {
        if self.cfg.theme_mode != kettle_config::ThemeMode::Auto {
            return None;
        }
        if self.cfg.theme_schedule.is_some() {
            return None;
        }
        let os_dark = matches!(theme, WindowTheme::Dark);
        kettle_config::resolve_theme_for_mode(
            kettle_config::ThemeMode::Auto,
            &self.cfg.theme_name,
            &self.cfg.light_theme,
            &self.cfg.dark_theme,
            Some(os_dark),
        )
    }

    fn apply_initial_os_theme_preference(&mut self, theme: WindowTheme) {
        if let Some(next) = self.os_theme_choice(theme) {
            log::info!("theme-mode=auto: initial OS theme selects {next}");
            self.cfg.theme_name = next.clone();
            self.cfg.theme = kettle_config::Theme::by_name(&next);
        }
    }

    fn apply_os_theme_preference(&mut self, ws: &mut WindowState, theme: WindowTheme) {
        if let Some(next) = self.os_theme_choice(theme) {
            self.apply_theme_name(ws, "theme-mode=auto", next);
        }
    }

    /// Cycle 660 (sub-cycle 5 of [`TERMINATOR-CONFIRM-DIALOG-DESIGN.md`](
    /// ../../../docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md)): dispatch
    /// the `ConfirmAction` after the user accepts the modal. Skips
    /// the `should_prompt` check (we wouldn't be here otherwise) so
    /// the close-family actions run their real bodies.
    fn dispatch_confirm_action(
        &mut self,
        ws: &mut WindowState,
        action: ConfirmAction,
        // C4: closes route through pending_window_close now, so the loop
        // handle is unused — kept so the dispatch signature stays uniform
        // with the other key handlers.
        _event_loop: &winit::event_loop::ActiveEventLoop,
    ) {
        match action {
            ConfirmAction::CloseWindow => {
                ws.mux.close_window();
                self.save_session(ws);
                self.pending_window_close = true;
            }
            ConfirmAction::CloseTab => {
                // CloseTab dispatch (cycle X). Sub-cycle 6 wires
                // ask-before-closing for CloseTab too; this arm is
                // the dispatch target for that future wiring.
                // Cycle 898 (audit): honor close_tab()'s return like the
                // keybind path (app.rs Action::CloseTab) — `true` means that
                // was the LAST tab, so the window must exit now. Pre-fix the
                // return was dropped, so closing the last tab via the confirm
                // dialog deferred exit by a tick and painted an empty frame.
                if ws.mux.close_tab() {
                    self.save_session(ws);
                    self.pending_window_close = true;
                    return;
                }
                self.save_session(ws);
            }
            ConfirmAction::ClosePane => {
                // Cycle 750: capture the pane id before the close so the
                // pane_close hook fires with the right id (mirrors the
                // keybind path).
                let closing_pane = ws.mux.active_focus();
                let was_last = ws.mux.close_focused();
                if let Some(id) = closing_pane {
                    self.fire_pane_close_event(id);
                }
                // Cycle 898 (audit): honor close_focused()'s return like the
                // keybind ClosePane path — `true` means the last pane closed,
                // so exit; otherwise redraw the collapsed layout (the renderer
                // cache + focus id are stale until a frame is scheduled).
                if was_last {
                    self.save_session(ws);
                    self.pending_window_close = true;
                    return;
                }
                self.save_session(ws);
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
        }
    }

    /// Cycle 656 (sub-cycle 6 of [`TERMINATOR-REMOTE-DESIGN.md`](
    /// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): periodic poll of
    /// every pane's process tree to detect SSH / Docker / Podman /
    /// kubectl sessions. Throttled to ~5 Hz so a typical 60 Hz
    /// redraw doesn't refresh sysinfo every frame.
    ///
    /// On a detection change (was-None now-Some, or shape change),
    /// the pane's title is updated to `format_remote_title(...)`.
    /// On the inverse (was-Some now-None — SSH exited), the title
    /// is left alone (the shell that re-shows after `ssh exit` is
    /// already the right OSC-1/2-set title).
    fn poll_remote_contexts(&mut self, ws: &mut WindowState) {
        if self.last_remote_poll.elapsed().as_millis() < 200 {
            return;
        }
        self.last_remote_poll = std::time::Instant::now();
        let pane_ids: Vec<u64> = ws.mux.panes.keys().copied().collect();
        // Cycle 851: refresh the OS process snapshot + parent→children index
        // ONCE per tick, then query every pane against the shared index.
        self.remote_scanner.refresh();
        for id in pane_ids {
            let Some(pane) = ws.mux.panes.get(&id) else {
                continue;
            };
            let Some(pid) = pane.term.child_pid() else {
                continue;
            };
            let detected = self.remote_scanner.detect_root(pid);
            if let Some(pane) = ws.mux.panes.get_mut(&id)
                && detected != pane.remote_context
            {
                if let Some(ctx) = &detected {
                    pane.title = kettle_remote::format_remote_title(ctx);
                }
                pane.remote_context = detected;
            }
        }
    }

    fn reload_config(&mut self, ws: &mut WindowState) {
        let mut new = self
            .config_path
            .as_deref()
            .map(Config::load_from)
            .unwrap_or_else(Config::load);
        if let Some(r) = ws.renderer.as_mut() {
            // Order matters slightly: family first so the cell measurer
            // sees the new family when size changes (the size setter
            // re-measures internally; a stale family would yield wrong
            // cell dims for one frame). Both are no-ops when unchanged,
            // so steady-state reloads (same family / same size) are free.
            r.set_font_family(new.font_family.clone());
            r.set_font_size(new.font_size);
            // Cycle 636: pick up cell-width/cell-height changes too.
            // Setter is a no-op when unchanged.
            r.set_cell_scale(new.cell_width, new.cell_height);
        }
        // Cycle 290: re-compile triggers from the freshly-loaded config
        // BEFORE assigning, while `new` is still owned. Recompile
        // catches added/removed/changed patterns. Throttle stamp
        // resets to "60s ago" so a fresh edit can fire immediately
        // even mid-throttle.
        self.compiled_triggers = compile_triggers(&new.triggers);
        self.last_trigger_fire = std::time::Instant::now() - std::time::Duration::from_secs(60);
        // Cycle 937: the accent seed is a runtime per-window value (not in the
        // config file), so carry it across a reload + re-apply the --accent
        // override (launch-time intent survives a reload).
        new.accent_seed = self.cfg.accent_seed;
        if let Some(rgb) = self.startup.accent_override {
            new.accent_color = Some(rgb);
        }
        // Cycle 942 (audit): the cycle-938 launch-time window flags are the
        // same "launch-time intent" — without re-applying, ANY live reload
        // (including kettle's own theme/settings persistence writes) silently
        // reverted a `-T` pinned title to `window-title-format` (and the
        // -m/-f/-H/-b overrides out of `self.cfg`, which save_session and
        // future window ops read).
        if let Some(wstate) = self.startup.window_state_override {
            new.window_state = wstate;
        }
        if let Some(b) = self.startup.borderless_override {
            new.borderless = b;
        }
        if let Some(title) = self.startup.title_override.clone() {
            new.window_title_format = title;
        }
        self.cfg = new;
        // Cycle 936: a config reload may have changed the font size, so the
        // saved pre-scaled-zoom size is stale — drop it (see the font-size
        // action arm) so a later zoom-out doesn't revert to the old size.
        ws.scaled_zoom_prev_font_size = None;
        self.resize_all(ws);
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// Cycle 290: scan every pane's recent output for configured
    /// trigger patterns. On the first match in this tick, raise the
    /// OS window's attention indicator (taskbar flash / dock bounce
    /// / WM_HINTS urgency) — but only if the window isn't focused,
    /// AND throttle to one fire per 2 seconds so a build script
    /// printing 100 error lines doesn't pulse the taskbar 100×.
    /// Cycle 302: drain pending lines from the remote-command file
    /// and dispatch each. Atomic-truncate after read so a fast-firing
    /// `kettle --remote-send` storm doesn't re-process the same lines
    /// every notify event. v1 commands:
    ///
    ///   send-text TEXT     write TEXT (with `\n` decoded back to
    ///                      newline) to the focused pane's PTY.
    ///
    /// Unknown commands log a `warn!` and continue. Empty file or
    /// missing file is a no-op (notify-watcher can fire on the
    /// initial create event before the writer's content is visible;
    /// next event will catch it).
    /// Cycle 928 (agent-first A2): drain the control-server channel and
    /// dispatch each message on the main thread (the only place `ws.mux` is
    /// touched). Mirrors `drain_remote_commands` but with per-connection
    /// replies + a connection table.
    fn drain_ctl(&mut self, ws: &mut WindowState) {
        use crate::ctl_server::CtlServerMsg;
        // Pull all pending messages first so we don't hold a `&self.ctl` borrow
        // while calling `&mut self` handlers.
        let mut msgs = Vec::new();
        if let Some(ctl) = &self.ctl {
            while let Some(m) = ctl.try_recv() {
                msgs.push(m);
            }
        }
        if msgs.is_empty() {
            return;
        }
        // v2.20.0 (review fix): only repaint when the batch could have
        // changed visible state. `wait_for` probes are pure reads arriving
        // at up to 20/s for the whole wait — repainting an otherwise-idle
        // window for each one burned CPU for nothing.
        let mut needs_redraw = false;
        for msg in msgs {
            match msg {
                CtlServerMsg::NewConn { conn_id, event_tx } => {
                    if let Some(ctl) = &mut self.ctl {
                        ctl.add_conn(conn_id, event_tx);
                    }
                    log::info!("agent-server: connection {conn_id} opened");
                }
                CtlServerMsg::BadRequest { reply, resp, .. } => {
                    let _ = reply.send(resp);
                }
                CtlServerMsg::Request {
                    conn_id,
                    req,
                    reply,
                    internal_probe,
                } => {
                    // Mutations + first-time attaches change visible state
                    // (pane content, agent badge); wait_for's internal
                    // probes never do.
                    if !internal_probe {
                        needs_redraw = true;
                    }
                    self.handle_ctl_request(ws, conn_id, &req, reply, internal_probe);
                }
                CtlServerMsg::Disconnect { conn_id } => {
                    let panes = self
                        .ctl
                        .as_mut()
                        .map(|c| c.remove_conn(conn_id))
                        .unwrap_or_default();
                    // Clear the agent badge for panes no connection holds now.
                    for pane in panes {
                        let still = self
                            .ctl
                            .as_ref()
                            .map(|c| c.pane_is_attached(pane))
                            .unwrap_or(false);
                        if !still {
                            self.set_pane_agent_attached(ws, pane, false);
                        }
                    }
                    // Drop any pending run owned by this connection.
                    self.pending_runs
                        .retain(|_, p: &mut PendingRun| p.conn_id != conn_id);
                    log::info!("agent-server: connection {conn_id} closed");
                    needs_redraw = true; // the agent badge may have cleared
                }
            }
        }
        if needs_redraw && let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// Dispatch one control request against the App and reply over `reply`.
    /// Most methods reply immediately; `run_command` stores `reply` and resolves
    /// it later (OSC-133 completion or the deadline).
    fn handle_ctl_request(
        &mut self,
        ws: &mut WindowState,
        conn_id: u64,
        req: &kettle_ctl::protocol::Request,
        reply: crate::ctl_server::ReplyTx,
        internal_probe: bool,
    ) {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let mode = self
            .ctl
            .as_ref()
            .map(|c| c.mode())
            .unwrap_or(kettle_config::AgentServer::Off);
        // Annotate the dev-record trace with each agent action (cheap; the
        // recorder only exists in dev-record builds with --record).
        // v2.20.0 (review fix): wait_for's internal read_screen probes are
        // NOT annotated — a 300s wait at 50ms polls would land ~6000
        // markers; the wait itself is visible via the client's own calls.
        #[cfg(feature = "dev-record")]
        if !internal_probe && let Some(rec) = self.recorder.as_mut() {
            rec.record_marker(&format!("kettle:agent {} conn={conn_id}", req.method));
        }
        #[cfg(not(feature = "dev-record"))]
        let _ = internal_probe;
        // Single mutation gate (a drift-guard test pins that every mutating
        // method routes through this check).
        let require_full = |id: u64, method: &str| -> Option<Response> {
            (!mode.allows_mutation()).then(|| {
                Response::err(
                    id,
                    ec::READ_ONLY,
                    format!("method '{method}' requires agent-server=full"),
                )
            })
        };
        let resp = match req.method.as_str() {
            "get_state" => Response::ok(req.id, self.ctl_get_state(ws, mode)),
            "list_tabs" => Response::ok(req.id, self.ctl_list_tabs(ws)),
            "list_panes" => Response::ok(req.id, self.ctl_list_panes(ws)),
            "read_screen" => self.ctl_read_screen(ws, req),
            "screenshot" => {
                self.ctl_screenshot(ws, req, reply);
                return;
            }
            "subscribe" => {
                if let Some(c) = &mut self.ctl {
                    c.set_subscribed(conn_id);
                }
                Response::ok(req.id, serde_json::json!({"subscribed": true}))
            }
            "send_text" => require_full(req.id, "send_text")
                .unwrap_or_else(|| self.ctl_send_text(ws, conn_id, req)),
            "send_keys" => require_full(req.id, "send_keys")
                .unwrap_or_else(|| self.ctl_send_keys(ws, conn_id, req)),
            "run_command" => {
                if let Some(deny) = require_full(req.id, "run_command") {
                    deny
                } else {
                    // Deferred: store `reply`; resolved on completion/deadline.
                    self.ctl_run_command(ws, conn_id, req, reply);
                    return;
                }
            }
            other => Response::err(
                req.id,
                ec::UNKNOWN_METHOD,
                format!("unknown method '{other}'"),
            ),
        };
        let _ = reply.send(resp);
    }

    /// `get_state`: version, theme, pid, server mode, focused pane.
    fn ctl_get_state(
        &self,
        ws: &WindowState,
        mode: kettle_config::AgentServer,
    ) -> serde_json::Value {
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "mode": format!("{mode:?}").to_lowercase(),
            "theme": self.cfg.theme_name,
            // `tabs` / `focused_pane` describe the FOCUSED window (back-
            // compat); C8 adds the window dimension alongside.
            "tabs": ws.mux.tabs.len(),
            "focused_pane": ws.mux.tabs.get(ws.mux.active).map(|t| t.focus),
            "windows": 1 + self.windows.len(),
            "focused_window": self.focused_seq,
        })
    }

    /// `list_tabs`: index, title, active flag, pane ids.
    fn ctl_list_tabs(&self, ws: &WindowState) -> serde_json::Value {
        // C8 (multi-window): tabs across EVERY window, ordered by window seq.
        // `index` and `active` are in-window values; `window` disambiguates.
        let mut tabs = Vec::new();
        for w in self.all_windows(ws) {
            let titles = w.mux.tab_titles();
            for (i, t) in w.mux.tabs.iter().enumerate() {
                tabs.push(serde_json::json!({
                    "window": w.seq,
                    "index": i,
                    "title": titles.get(i).cloned().unwrap_or_default(),
                    "active": i == w.mux.active,
                    "focused_pane": t.focus,
                    "panes": t.root.leaf_ids(),
                }));
            }
        }
        serde_json::json!({ "tabs": tabs })
    }

    /// `list_panes`: id, tab, title, cwd, size, focused, argv, child_pid,
    /// agent_attached.
    fn ctl_list_panes(&self, ws: &WindowState) -> serde_json::Value {
        // C8 (multi-window): panes across EVERY window; `tab` is the
        // in-window tab index, `window` the owning window's seq, `focused`
        // means focused WITHIN its window.
        let mut panes = Vec::new();
        for w in self.all_windows(ws) {
            self.ctl_list_panes_of(w, &mut panes);
        }
        serde_json::json!({ "panes": panes })
    }

    fn ctl_list_panes_of(&self, ws: &WindowState, panes: &mut Vec<serde_json::Value>) {
        let focused = ws.mux.tabs.get(ws.mux.active).map(|t| t.focus);
        for (ti, tab) in ws.mux.tabs.iter().enumerate() {
            for id in tab.root.leaf_ids() {
                let Some(pane) = ws.mux.panes.get(&id) else {
                    continue;
                };
                let (cols, rows) = pane
                    .term
                    .term
                    .lock()
                    .ok()
                    .map(|t| {
                        use kettle_core::Dimensions;
                        (t.columns(), t.screen_lines())
                    })
                    .unwrap_or((0, 0));
                let attached = self
                    .ctl
                    .as_ref()
                    .map(|c| c.pane_is_attached(id))
                    .unwrap_or(false);
                panes.push(serde_json::json!({
                    "id": id,
                    "window": ws.seq,
                    "tab": ti,
                    "title": pane.title,
                    "cwd": pane.term.current_dir(),
                    "cols": cols,
                    "rows": rows,
                    "focused": Some(id) == focused,
                    "argv": pane.argv,
                    "child_pid": pane.term.child_pid(),
                    "agent_attached": attached,
                    // Cycle 942: surfaced so an agent can SEE the user's
                    // read-only lock before a send_text/run_command bounces
                    // with the `read_only` error code.
                    "read_only": pane.read_only,
                }));
            }
        }
    }

    /// `read_screen`: a plain-text snapshot of a pane (default: focused).
    fn ctl_read_screen(
        &mut self,
        ws: &mut WindowState,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let pane = match self.ctl_resolve_pane(ws, &req.params) {
            Ok(p) => p,
            Err(e) => return Response::err(req.id, ec::NO_SUCH_PANE, e),
        };
        let scrollback = req
            .params
            .get("scrollback_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let Some(p) = Self::ctl_pane_ref(ws, &self.windows, pane) else {
            return Response::err(req.id, ec::NO_SUCH_PANE, "pane vanished");
        };
        match p.term.screen_text(scrollback) {
            Some(s) => Response::ok(
                req.id,
                serde_json::json!({
                    "pane": pane,
                    "text": s.text,
                    "cols": s.cols,
                    "rows": s.rows,
                    "history_size": s.history_size,
                    "display_offset": s.display_offset,
                    "cursor": [s.cursor.0, s.cursor.1],
                    // v2.20.0 (agent plane): DEC ?25 — vim/fzf/less hide the
                    // cursor; agents must know when `cursor` is meaningless.
                    "cursor_visible": s.cursor_visible,
                }),
            ),
            None => Response::err(req.id, ec::INTERNAL, "could not read the grid"),
        }
    }

    /// `screenshot`: queue a live-surface PNG capture and reply when the next
    /// rendered frame saves it. Defaults to the focused pane crop; pass
    /// `{full_window:true}` to capture the whole window, or `{path:"…"}` to
    /// choose the output file.
    fn ctl_screenshot(
        &mut self,
        ws: &mut WindowState,
        req: &kettle_ctl::protocol::Request,
        reply: crate::ctl_server::ReplyTx,
    ) {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let full_window = req
            .params
            .get("full_window")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let pane = if full_window && req.params.get("pane").is_none() {
            None
        } else {
            match self.ctl_resolve_pane(ws, &req.params) {
                Ok(p) => Some(p),
                Err(e) => {
                    let _ = reply.send(Response::err(req.id, ec::NO_SUCH_PANE, e));
                    return;
                }
            }
        };
        let target_seq = match pane {
            Some(id) if ws.mux.panes.contains_key(&id) => Some(ws.seq),
            Some(id) => self
                .windows
                .values()
                .find(|w| w.mux.panes.contains_key(&id))
                .map(|w| w.seq),
            None => Some(ws.seq),
        };
        let Some(target_seq) = target_seq else {
            let _ = reply.send(Response::err(req.id, ec::NO_SUCH_PANE, "pane vanished"));
            return;
        };
        let crop = if full_window {
            None
        } else {
            let Some(pane_id) = pane else {
                let _ = reply.send(Response::err(req.id, ec::BAD_PARAMS, "missing pane"));
                return;
            };
            let target = if target_seq == ws.seq {
                Some(&*ws)
            } else {
                self.windows.get(&target_seq)
            };
            target.and_then(|target| {
                let area = self.area(target);
                let active = target.mux.active;
                target
                    .mux
                    .layout(active, area)
                    .into_iter()
                    .find(|(id, _)| *id == pane_id)
                    .map(|(_, rect)| rect)
            })
        };
        let path = match req.params.get("path").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => std::path::PathBuf::from(s),
            Some(_) => {
                let _ = reply.send(Response::err(req.id, ec::BAD_PARAMS, "'path' is empty"));
                return;
            }
            None => {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let cache = cache_dir_from_env(|k| std::env::var(k).ok());
                session_screenshot_path(secs, std::process::id(), cache.as_deref())
            }
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            let _ = reply.send(Response::err(
                req.id,
                ec::INTERNAL,
                format!("could not create screenshot directory: {e}"),
            ));
            return;
        }
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let renderer = if target_seq == ws.seq {
            ws.renderer.as_mut()
        } else {
            self.windows
                .get_mut(&target_seq)
                .and_then(|w| w.renderer.as_mut())
        };
        let Some(renderer) = renderer else {
            let _ = reply.send(Response::err(
                req.id,
                ec::INTERNAL,
                "target window has no renderer",
            ));
            return;
        };
        renderer.set_pending_screenshot(kettle_render::ScreenshotRequest {
            out_path: path.clone(),
            crop,
            completion: Some(done_tx),
        });
        if target_seq == ws.seq {
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
        } else if let Some(target) = self.windows.get(&target_seq)
            && let Some(w) = &target.window
        {
            w.request_redraw();
        }
        let request_id = req.id;
        std::thread::spawn(move || {
            let resp = match done_rx.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(Ok(saved)) => Response::ok(
                    request_id,
                    serde_json::json!({
                        "path": saved,
                        "pane": pane,
                        "window": target_seq,
                        "full_window": full_window,
                    }),
                ),
                Ok(Err(e)) => Response::err(request_id, ec::INTERNAL, e),
                Err(_) => Response::err(request_id, ec::INTERNAL, "screenshot capture timed out"),
            };
            let _ = reply.send(resp);
        });
    }

    /// `send_text`: write text to a pane's PTY (default: focused).
    fn ctl_send_text(
        &mut self,
        ws: &mut WindowState,
        conn_id: u64,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let pane = match self.ctl_resolve_pane(ws, &req.params) {
            Ok(p) => p,
            Err(e) => return Response::err(req.id, ec::NO_SUCH_PANE, e),
        };
        let Some(text) = req.params.get("text").and_then(|v| v.as_str()) else {
            return Response::err(req.id, ec::BAD_PARAMS, "missing 'text' string");
        };
        let Some(p) = Self::ctl_pane_ref(ws, &self.windows, pane) else {
            return Response::err(req.id, ec::NO_SUCH_PANE, "pane vanished");
        };
        // Cycle 941: an agent acts as the user — the per-pane read-only
        // toggle (Terminator parity) blocks it like any other input, with an
        // explicit error instead of a silent drop.
        if !p.feed_input(text.as_bytes()) {
            return Response::err(
                req.id,
                ec::READ_ONLY,
                "pane is read-only (user toggled 'Read only')",
            );
        }
        log::info!(
            "agent-server: send_text conn={conn_id} pane={pane} ({} bytes)",
            text.len()
        );
        self.ctl_attach(ws, conn_id, pane);
        Response::ok(
            req.id,
            serde_json::json!({"pane": pane, "bytes": text.len()}),
        )
    }

    /// v2.20.0 (agent plane): `send_keys` — press named keys / chords in a
    /// pane. `send_text` can only type literal characters (CR included), so
    /// an agent could not press Escape, arrows, Ctrl-chords or F-keys — the
    /// keys interactive apps (vim, htop, fzf, tmux) are driven with. Tokens
    /// are encoded through the SAME `input::encode` path as GUI keystrokes,
    /// against the pane's LIVE terminal mode — so arrows honor DECCKM
    /// application-cursor mode exactly like a human's key press.
    ///
    /// Params: `{pane?: u64, keys: ["escape", "ctrl+c", "down", "G", …]}`.
    /// All tokens are parsed BEFORE any byte is written — a typo mid-sequence
    /// must not leave the target app half-keyed.
    fn ctl_send_keys(
        &mut self,
        ws: &mut WindowState,
        conn_id: u64,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let pane = match self.ctl_resolve_pane(ws, &req.params) {
            Ok(p) => p,
            Err(e) => return Response::err(req.id, ec::NO_SUCH_PANE, e),
        };
        let Some(keys) = req.params.get("keys").and_then(|v| v.as_array()) else {
            return Response::err(
                req.id,
                ec::BAD_PARAMS,
                "missing 'keys' array of key tokens (e.g. [\"escape\",\"ctrl+c\"])",
            );
        };
        if keys.is_empty() {
            return Response::err(req.id, ec::BAD_PARAMS, "'keys' is empty");
        }
        let mut parsed = Vec::with_capacity(keys.len());
        for k in keys {
            let Some(tok) = k.as_str() else {
                return Response::err(req.id, ec::BAD_PARAMS, "non-string entry in 'keys'");
            };
            match parse_send_key(tok) {
                Some(p) => parsed.push(p),
                None => {
                    return Response::err(
                        req.id,
                        ec::BAD_PARAMS,
                        format!("unrecognized key token '{tok}'"),
                    );
                }
            }
        }
        let Some(p) = Self::ctl_pane_ref(ws, &self.windows, pane) else {
            return Response::err(req.id, ec::NO_SUCH_PANE, "pane vanished");
        };
        // The live mode decides the byte form (app-cursor arrows etc.).
        let mode = p
            .term
            .term
            .lock()
            .ok()
            .map(|t| *t.mode())
            .unwrap_or_else(kettle_core::TermMode::empty);
        let mut bytes = Vec::new();
        for (mods, key) in &parsed {
            if let Some(b) = crate::input::encode(key, None, *mods, mode) {
                // Review fix: honor the user's backspace-binding /
                // delete-binding remap, exactly like the GUI key path.
                let b = apply_bs_del_binding(&self.cfg, key, *mods, b);
                bytes.extend_from_slice(&b);
            }
        }
        // Cycle 941 semantics: the per-pane read-only toggle blocks agents
        // like any other input, with an explicit error.
        if !p.feed_input(&bytes) {
            return Response::err(
                req.id,
                ec::READ_ONLY,
                "pane is read-only (user toggled 'Read only')",
            );
        }
        log::info!(
            "agent-server: send_keys conn={conn_id} pane={pane} ({} keys, {} bytes)",
            parsed.len(),
            bytes.len()
        );
        self.ctl_attach(ws, conn_id, pane);
        Response::ok(
            req.id,
            serde_json::json!({"pane": pane, "keys": parsed.len(), "bytes": bytes.len()}),
        )
    }

    /// Resolve the `pane` param to a pane id; default to the focused pane.
    /// `Err` with a message when neither resolves.
    fn ctl_resolve_pane(
        &self,
        ws: &WindowState,
        params: &serde_json::Value,
    ) -> Result<u64, String> {
        if let Some(p) = params.get("pane").and_then(|v| v.as_u64()) {
            // C8 (multi-window): an explicit pane id may live in ANY window
            // (pane ids are process-global).
            if ws.mux.panes.contains_key(&p)
                || self.windows.values().any(|w| w.mux.panes.contains_key(&p))
            {
                return Ok(p);
            }
            return Err(format!("no pane with id {p}"));
        }
        ws.mux
            .tabs
            .get(ws.mux.active)
            .map(|t| t.focus)
            .filter(|f| ws.mux.panes.contains_key(f))
            .ok_or_else(|| "no focused pane".to_string())
    }

    /// C8 (multi-window): every window — the checked-out one plus the map —
    /// ordered by seq, for the ctl read paths.
    fn all_windows<'a>(&'a self, ws: &'a WindowState) -> Vec<&'a WindowState> {
        let mut v: Vec<&WindowState> = std::iter::once(ws).chain(self.windows.values()).collect();
        v.sort_by_key(|w| w.seq);
        v
    }

    /// C8 (multi-window): borrow a pane wherever it lives — the checked-out
    /// window or any other in the map (pane ids are process-global).
    fn ctl_pane_ref<'a>(
        ws: &'a WindowState,
        windows: &'a std::collections::BTreeMap<u64, WindowState>,
        id: u64,
    ) -> Option<&'a crate::mux::Pane> {
        ws.mux
            .panes
            .get(&id)
            .or_else(|| windows.values().find_map(|w| w.mux.panes.get(&id)))
    }

    /// Mark `conn_id` attached to `pane` + fire the badge transition once.
    fn ctl_attach(&mut self, ws: &mut WindowState, conn_id: u64, pane: u64) {
        let newly = self
            .ctl
            .as_mut()
            .map(|c| c.attach_pane(conn_id, pane))
            .unwrap_or(false);
        if newly {
            self.set_pane_agent_attached(ws, pane, true);
        }
    }

    /// Cycle 934 (agent-first A4): flip a pane's agent badge + emit the
    /// `agent_attached` event so subscribers + the titlebar update.
    fn set_pane_agent_attached(&mut self, ws: &mut WindowState, pane: u64, attached: bool) {
        // C8 (multi-window): the pane may live in any window.
        let in_ws = ws.mux.panes.contains_key(&pane);
        {
            let p = if in_ws {
                ws.mux.panes.get_mut(&pane)
            } else {
                self.windows
                    .values_mut()
                    .find_map(|w| w.mux.panes.get_mut(&pane))
            };
            let Some(p) = p else { return };
            if p.agent_attached == attached {
                return;
            }
            p.agent_attached = attached;
        }
        self.ctl_broadcast(
            "agent_attached",
            Some(pane),
            serde_json::json!({"attached": attached}),
        );
        // Repaint the owning window so the titlebar badge updates.
        let owner = if in_ws {
            ws.window.as_ref()
        } else {
            self.windows
                .values()
                .find(|w| w.mux.panes.contains_key(&pane))
                .and_then(|w| w.window.as_ref())
        };
        if let Some(w) = owner {
            w.request_redraw();
        }
    }

    /// Cycle 930 (agent-first A2): broadcast an event to subscribed control
    /// connections (no-op when the server is off / nobody subscribed).
    fn ctl_broadcast(&self, kind: &str, pane: Option<u64>, data: serde_json::Value) {
        if let Some(ctl) = &self.ctl
            && ctl.has_subscribers()
        {
            ctl.broadcast(&kettle_ctl::protocol::Event::new(kind, pane, data));
        }
    }

    /// `run_command`: write `cmd\n` to a pane and defer the reply (stored in a
    /// `PendingRun`) until the next OSC-133 `CommandFinished` for that pane, or
    /// the deadline. A bad request replies immediately over `reply`.
    fn ctl_run_command(
        &mut self,
        ws: &mut WindowState,
        conn_id: u64,
        req: &kettle_ctl::protocol::Request,
        reply: crate::ctl_server::ReplyTx,
    ) {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let pane = match self.ctl_resolve_pane(ws, &req.params) {
            Ok(p) => p,
            Err(e) => {
                let _ = reply.send(Response::err(req.id, ec::NO_SUCH_PANE, e));
                return;
            }
        };
        let Some(command) = req.params.get("command").and_then(|v| v.as_str()) else {
            let _ = reply.send(Response::err(
                req.id,
                ec::BAD_PARAMS,
                "missing 'command' string",
            ));
            return;
        };
        if self.pending_runs.contains_key(&pane) {
            let _ = reply.send(Response::err(
                req.id,
                ec::BUSY,
                "a run_command is already pending on this pane",
            ));
            return;
        }
        let timeout_s = req
            .params
            .get("timeout_s")
            .and_then(|v| v.as_f64())
            .unwrap_or(15.0)
            .clamp(0.1, 600.0);
        // Snapshot the output start line as the ABSOLUTE cursor line
        // (history_size + cursor row), not history_size + rows. The command's
        // output usually fits on the visible screen (no scrollback growth), so
        // measuring by `rows` would make `total_now == start_line` and slice out
        // nothing; the cursor advances as output is printed, so the absolute
        // cursor line tracks the real content position whether or not it scrolls.
        let start_line = Self::ctl_pane_ref(ws, &self.windows, pane)
            .and_then(|p| p.term.screen_text(0).map(|s| s.history_size + s.cursor.0))
            .unwrap_or(0);
        if let Some(p) = Self::ctl_pane_ref(ws, &self.windows, pane) {
            let mut line = command.to_string();
            // Submit with a CARRIAGE RETURN, not a line feed: CR is the Enter
            // key a shell's line editor acts on. Under Windows ConPTY only CR is
            // delivered as Enter (a bare LF is typed but never executes); on a
            // Unix PTY the line discipline's ICRNL maps CR→LF, so CR works on
            // both. (A trailing newline already in `command` is left as-is.)
            if !line.ends_with('\r') && !line.ends_with('\n') {
                line.push('\r');
            }
            // Cycle 941: an agent acts as the user — the per-pane read-only
            // toggle (Terminator parity) blocks it, with an explicit error
            // (no PendingRun is registered; nothing was written).
            if !p.feed_input(line.as_bytes()) {
                let _ = reply.send(Response::err(
                    req.id,
                    ec::READ_ONLY,
                    "pane is read-only (user toggled 'Read only')",
                ));
                return;
            }
        }
        log::info!("agent-server: run_command conn={conn_id} pane={pane}: {command:?}");
        self.ctl_attach(ws, conn_id, pane);
        self.pending_runs.insert(
            pane,
            PendingRun {
                conn_id,
                req_id: req.id,
                start_line,
                deadline: std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout_s),
                reply,
            },
        );
    }

    /// Cycle 929: an OSC-133 `CommandFinished` arrived for `pane` — if a
    /// `run_command` is pending there, reply with the exit code, duration, and
    /// the output captured since the command started.
    fn resolve_pending_run(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        ev: &kettle_core::CommandFinished,
    ) {
        let Some(run) = self.pending_runs.remove(&pane) else {
            return;
        };
        let output = self.ctl_capture_output_since(ws, pane, run.start_line);
        let resp = kettle_ctl::protocol::Response::ok(
            run.req_id,
            serde_json::json!({
                "pane": pane,
                "exit_code": ev.exit_code,
                "duration_ms": ev.duration.as_millis() as u64,
                "timed_out": false,
                "output": output,
            }),
        );
        let _ = run.reply.send(resp);
    }

    /// Cycle 929: reply `timed_out` to any pending run whose deadline passed
    /// (no OSC-133 completion — usually the shell lacks shell integration), and
    /// (cycle 936) immediately resolve any pending run whose pane has CLOSED, so
    /// the agent isn't blocked for the full timeout after a pane vanishes.
    /// Called each event-loop tick from the redraw scheduler.
    fn check_pending_run_deadlines(&mut self, ws: &mut WindowState) {
        if self.pending_runs.is_empty() {
            return;
        }
        // A pending run whose pane no longer exists (closed/reaped) is resolved
        // at once with an error, freeing the blocked connection thread.
        let orphaned: Vec<u64> = self
            .pending_runs
            .keys()
            .copied()
            .filter(|pane| !ws.mux.panes.contains_key(pane))
            .collect();
        for pane in orphaned {
            if let Some(run) = self.pending_runs.remove(&pane) {
                let resp = kettle_ctl::protocol::Response::err(
                    run.req_id,
                    kettle_ctl::protocol::error_codes::NO_SUCH_PANE,
                    "pane closed before the command completed",
                );
                let _ = run.reply.send(resp);
            }
        }
        let now = std::time::Instant::now();
        let expired: Vec<u64> = self
            .pending_runs
            .iter()
            .filter(|(_, p)| now >= p.deadline)
            .map(|(&pane, _)| pane)
            .collect();
        for pane in expired {
            let Some(run) = self.pending_runs.remove(&pane) else {
                continue;
            };
            let output = self.ctl_capture_output_since(ws, pane, run.start_line);
            let resp = kettle_ctl::protocol::Response::ok(
                run.req_id,
                serde_json::json!({
                    "pane": pane,
                    "exit_code": serde_json::Value::Null,
                    "timed_out": true,
                    "output": output,
                    "hint": "no OSC 133 command-end seen — enable shell integration \
                             (kettle --shell-integration <shell>) for exit codes",
                }),
            );
            let _ = run.reply.send(resp);
        }
    }

    /// Capture a pane's output produced since absolute line `start_line` (the
    /// lines a single `run_command` added). Best-effort; empty on a vanished
    /// pane.
    ///
    /// Slices the ABSOLUTE line range `[start_line, cursor]` out of the grid —
    /// NOT "the last N grid lines". A fresh shell's content sits at the TOP of
    /// the screen with blank rows below it, so taking the grid tail would return
    /// those trailing blanks; addressing by absolute line lands on the real
    /// content wherever it is. The cursor's absolute line is the end of the
    /// content the command produced.
    fn ctl_capture_output_since(&self, ws: &WindowState, pane: u64, start_line: usize) -> String {
        const CAP_LINES: usize = 10_000;
        // C8: the pane may have moved to another window mid-run.
        let Some(p) = Self::ctl_pane_ref(ws, &self.windows, pane) else {
            return String::new();
        };
        let Some(probe) = p.term.screen_text(0) else {
            return String::new();
        };
        let hist = probe.history_size;
        let cursor_abs = hist + probe.cursor.0; // absolute line of the cursor now
        // History lines we must pull to reach `start_line` (0 when it's already
        // on the active screen, the common case), capped.
        let take = hist.saturating_sub(start_line).min(CAP_LINES);
        let Some(s) = p.term.screen_text(take) else {
            return String::new();
        };
        let lines: Vec<&str> = s.text.lines().collect();
        // The first returned line is absolute `hist - min(take, hist)`.
        let first_abs = hist - take.min(hist);
        let start_idx = start_line.saturating_sub(first_abs);
        // Inclusive of the cursor line; clamp to what we actually have.
        let end_idx = (cursor_abs.saturating_sub(first_abs) + 1).min(lines.len());
        if start_idx >= end_idx {
            return String::new();
        }
        lines[start_idx..end_idx].join("\n")
    }

    fn drain_remote_commands(&mut self, ws: &mut WindowState) {
        let Some(path) = self.startup.remote_file.clone() else {
            return;
        };
        // Cycle 315: cap the read at 1 MB. A legitimate command is
        // dozens of bytes; even a chatty automation pushing 1000
        // commands fits in ~64 KB. 1 MB is 10× safety margin. A
        // larger file likely means a runaway script or an accidental
        // log redirect (`some-cmd >> remote.cmd` instead of
        // `kettle --remote-send "$(some-cmd)"`); silently truncate
        // + warn rather than allocate the whole thing.
        const MAX_REMOTE_BYTES: u64 = 1 << 20; // 1 MB
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size > MAX_REMOTE_BYTES {
            log::warn!(
                "remote-command file at {} is {size} bytes (cap {MAX_REMOTE_BYTES}); \
                 truncating without processing",
                path.display()
            );
            let _ = std::fs::write(&path, "");
            return;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(s) if !s.is_empty() => s,
            _ => return,
        };
        let _ = std::fs::write(&path, "");
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(payload) = line.strip_prefix("send-text ") {
                let decoded = payload.replace("\\n", "\n");
                if let Some(p) = ws.mux.focused() {
                    // Cycle 941: remote.cmd acts as the user — read-only drops it.
                    p.feed_input(decoded.as_bytes());
                }
            } else if line == "toggle-window" {
                // Cycle 303 + 319: tri-state Quake dropdown toggle.
                // The naive binary toggle (hide-when-visible / show-
                // when-hidden) had a real UX problem: when kettle was
                // visible but the user had clicked away to another
                // window, pressing the hotkey HID kettle — but the
                // user usually wanted to bring it BACK INTO FOCUS.
                // The Quake / Yakuake / Tilda tradition is tri-state:
                //
                //   hidden            → show + raise + focus
                //   visible + focused → hide
                //   visible + !focused → raise + focus (don't hide)
                //
                // winit's has_focus / is_visible / focus_window /
                // set_visible all support this; the helper landed in
                // cycle 319.
                if let Some(w) = &ws.window {
                    let visible = w.is_visible().unwrap_or(true);
                    let focused = w.has_focus();
                    if !visible {
                        w.set_visible(true);
                        w.focus_window();
                    } else if focused {
                        w.set_visible(false);
                    } else {
                        // Visible but unfocused — bring to front +
                        // focus, don't hide. The common "I clicked
                        // away" case.
                        w.focus_window();
                    }
                }
            } else if line == "new-tab" {
                // Cycle 419 (Terminator parity, remote-control verb):
                // open a new tab via the remote-control IPC channel.
                // Mirrors the Action::NewTab dispatch (cycle 134) +
                // cycle 423: also fire LuaEvent::TabAdd so plugins
                // listening for tab_add see remote-triggered tabs
                // the same as keyboard ones.
                let (cw, ch) = self.cell_px(ws);
                let area = self.area(ws);
                let (cols, rows) = self.grid_of(ws, area);
                let waker = self.waker();
                if let Err(e) = ws.mux.new_tab(&self.cfg, cols, rows, cw, ch, waker) {
                    log::warn!("remote-control: new-tab failed: {e}");
                } else {
                    self.fire_tab_add_event(ws);
                }
            } else {
                log::warn!("remote command not recognized: {line:?}");
            }
        }
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    fn run_triggers(&mut self, ws: &mut WindowState) {
        if self.compiled_triggers.is_empty() {
            return;
        }
        // Throttle. `last_trigger_fire` is pre-set to "60 seconds ago"
        // at construct/reload time so the first match always fires.
        if self.last_trigger_fire.elapsed().as_millis() < 2000 {
            return;
        }
        // Don't pulse the user's own window when it's already focused.
        if ws.window_focused {
            return;
        }
        // Pull each pane's bottom-of-screen text. Visible viewport
        // only — scanning the whole scrollback every wakeup would
        // burn CPU on a chatty pane. Last 50 rows is the typical
        // "what just happened" window.
        let snapshots: Vec<String> = {
            let mut out = Vec::with_capacity(ws.mux.panes.len());
            for pane in ws.mux.panes.values() {
                if let Ok(t) = pane.term.term.lock() {
                    use kettle_core::Dimensions;
                    let rows = t.screen_lines();
                    let cols = t.columns();
                    let from = rows.saturating_sub(50);
                    let mut s = String::with_capacity((rows - from) * cols);
                    for r in from..rows {
                        for c in 0..cols {
                            let p = kettle_core::Point::new(
                                kettle_core::Line(r as i32),
                                kettle_core::Column(c),
                            );
                            s.push(t.grid()[p].c);
                        }
                        // v2.20.0 (review fix): trim the row's grid padding
                        // BEFORE the newline — a capture group like `(.+)$`
                        // otherwise embedded up-to-a-full-row of trailing
                        // spaces into the spawned command's argv. Safe: the
                        // previous row already ends in '\n', so the trim can
                        // never eat earlier rows.
                        let keep = s.trim_end_matches(' ').len();
                        s.truncate(keep);
                        s.push('\n');
                    }
                    out.push(s);
                }
            }
            out
        };
        for snap in &snapshots {
            if let Some(action) = match_triggers(snap, &self.compiled_triggers) {
                match action {
                    kettle_config::TriggerAction::Urgency => {
                        if let Some(w) = &ws.window {
                            use winit::window::UserAttentionType;
                            w.request_user_attention(Some(UserAttentionType::Critical));
                            ws.attention_active = true;
                        }
                    }
                    kettle_config::TriggerAction::RunCommand(argv) => {
                        // Cycle 622 (Terminator parity, `plugins/run_cmd_on_match.py`):
                        // fire-and-forget spawn. Argv form means no
                        // shell expansion at kettle's layer; the
                        // configured command is treated as data, not
                        // a shell string. Spawn errors are logged
                        // but otherwise ignored so a missing binary
                        // doesn't loop spawn-fail every trigger tick.
                        spawn_trigger_command(&argv);
                    }
                }
                self.last_trigger_fire = std::time::Instant::now();
                break;
            }
        }
    }

    fn search_key(&mut self, ws: &mut WindowState, key: &Key, text: Option<&str>) {
        match key {
            Key::Named(NamedKey::Escape) => {
                // Cycle 140: closing the search overlay reveals the
                // pane's cursor underneath. Reset blink so the
                // cursor is visible immediately — same UX argument
                // as cycles 134/135 (focus + Reset paths).
                ws.mux.search.open = false;
                self.reset_blink_phase(ws);
            }
            Key::Named(NamedKey::Enter) => {
                // Cycle 358 (Terminator parity, terminatorlib/config.py:93
                // `invert_search`): flip the default-direction.
                // - Default: Enter → next match, Shift+Enter → previous.
                // - With invert_search = true: Enter → previous match,
                //   Shift+Enter → next. Matches Terminator's "search
                //   reverse" toggle.
                let go_back = ws.mods.shift_key() ^ self.cfg.invert_search;
                self.search_step_match(ws, go_back);
            }
            // v2.20.0 (`vim-menu-nav`): Ctrl+j/Ctrl+k (+ Ctrl+n/Ctrl+p) step
            // to the next/previous match from the home row — search had no
            // arrow-key nav at all before this. The direction is literal:
            // `invert-search` only flips Enter's *default*, not an explicit
            // directional key.
            Key::Character(s)
                if self.cfg.vim_menu_nav
                    && ws.mods.control_key()
                    && !ws.mods.alt_key()
                    && matches!(s.as_str(), "j" | "k" | "n" | "p") =>
            {
                self.search_step_match(ws, matches!(s.as_str(), "k" | "p"));
            }
            Key::Named(NamedKey::Backspace) => {
                ws.mux.search.query.pop();
            }
            _ => {
                // Cycle 857 (audit): filter control chars like the sibling
                // title / SSH-input handlers do — a stray control byte
                // (Tab, embedded ESC from a paste, etc.) must not land in the
                // search query and corrupt the match.
                if let Some(t) = text
                    && !t.chars().any(|c| c.is_control())
                {
                    ws.mux.search.query.push_str(t);
                }
            }
        }
    }

    /// Advance the search selection one match forward (`go_back = false`) or
    /// backward, honoring `search-wrap` (cycle 940: `false` stops at the ends
    /// instead of cycling). Extracted from `search_key`'s Enter arm in
    /// v2.20.0 so vim-menu-nav's Ctrl+j/Ctrl+k share the exact stepping.
    fn search_step_match(&mut self, ws: &mut WindowState, go_back: bool) {
        let s = &mut ws.mux.search;
        if s.matches.is_empty() {
            return;
        }
        let n = s.matches.len();
        s.index = if self.cfg.search_wrap {
            if go_back {
                (s.index + n - 1) % n
            } else {
                (s.index + 1) % n
            }
        } else if go_back {
            s.index.saturating_sub(1)
        } else {
            (s.index + 1).min(n - 1)
        };
    }

    /// Command-palette key handling: fuzzy-filter as you type, `Tab`/`↑↓`
    /// to move the selection, `Enter` to run it, `Esc` to cancel.
    /// Quick-select hint key handling: type the label of a target to act on
    /// it (open URLs, copy paths/hashes/IPs); `Esc` cancels.
    /// Cycle 299: vi-mode key dispatcher (sub-cycle 2). Handles
    /// h/j/k/l movement, 0/$/g/G/H/M/L jumps, and Esc exit. Other
    /// keys are absorbed (no PTY write) so a stray press doesn't
    /// land bytes in the shell while the user thinks they're
    /// navigating.
    ///
    /// Movement clamps to the focused pane's grid (no negative rows
    /// yet — sub-cycle 3 extends into scrollback).
    fn vi_mode_key(&mut self, ws: &mut WindowState, key: &Key, text: Option<&str>) {
        // Esc exits.
        if matches!(key, Key::Named(NamedKey::Escape)) {
            ws.vi_mode = None;
            return;
        }
        // Grab the focused pane's grid dims to clamp movement.
        let (max_row, max_col) = ws
            .mux
            .focused()
            .and_then(|p| {
                p.term.term.lock().ok().map(|t| {
                    use kettle_core::Dimensions;
                    (
                        t.screen_lines().saturating_sub(1),
                        t.columns().saturating_sub(1),
                    )
                })
            })
            .unwrap_or((23, 79));
        let Some(state) = ws.vi_mode.as_mut() else {
            return;
        };
        // Character-based dispatch — works for both `Key::Character`
        // and `event.text` paths.
        let ch = text.and_then(|s| s.chars().next()).unwrap_or('\0');
        match ch {
            'h' => state.col = state.col.saturating_sub(1),
            'l' => state.col = (state.col + 1).min(max_col),
            'k' => state.row = state.row.saturating_sub(1),
            'j' => state.row = (state.row + 1).min(max_row),
            '0' | '^' => state.col = 0,
            '$' => state.col = max_col,
            'g' => state.row = 0,
            'G' => state.row = max_row,
            'H' => state.row = 0,
            'M' => state.row = max_row / 2,
            'L' => state.row = max_row,
            // Cycle 301 sub-cycle 4: `v` toggles char-visual mode.
            // Setting the anchor at the current cursor position begins
            // a selection; pressing `v` again clears it.
            'v' => {
                state.visual_anchor = match state.visual_anchor {
                    Some(_) => None,
                    None => Some((state.row, state.col)),
                };
            }
            // Cycle 301: `y` yanks the visual selection to clipboard
            // (system + selection) and exits vi-mode — same shape as
            // Alacritty.
            'y' => {
                if let Some(anchor) = state.visual_anchor {
                    let cur = (state.row, state.col);
                    let (start, end) = if anchor <= cur {
                        (anchor, cur)
                    } else {
                        (cur, anchor)
                    };
                    let yanked = self.yank_vi_selection(ws, start, end);
                    if !yanked.is_empty() {
                        // Cycle 316: log a warn when clipboard is None
                        // (e.g. SSH without X11 / Wayland forwarding,
                        // missing DISPLAY, or arboard init failed at
                        // startup). Pre-fix, vi-mode `y` silently
                        // dropped the selection — the user saw the
                        // visual-mode highlight clear + vi-mode exit,
                        // assumed copy worked, then hit paste and
                        // got their previous clipboard contents.
                        if let Some(clip) = self.clipboard.as_mut() {
                            if let Err(e) = clip.set_text(yanked) {
                                log::warn!("vi-mode yank: clipboard set_text failed: {e}");
                            }
                        } else {
                            log::warn!(
                                "vi-mode yank: clipboard unavailable (selection of {} bytes \
                                 not copied — try a kettle window with DISPLAY / Wayland set)",
                                yanked.len()
                            );
                        }
                    }
                }
                ws.vi_mode = None;
            }
            _ => {
                // Arrow keys also navigate.
                match key {
                    Key::Named(NamedKey::ArrowLeft) => {
                        state.col = state.col.saturating_sub(1);
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        state.col = (state.col + 1).min(max_col);
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        state.row = state.row.saturating_sub(1);
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        state.row = (state.row + 1).min(max_row);
                    }
                    _ => {}
                }
            }
        }
    }

    fn hint_key(&mut self, ws: &mut WindowState, key: &Key, text: Option<&str>) {
        match key {
            Key::Named(NamedKey::Escape) => {
                ws.hint_state = None;
                self.reset_blink_phase(ws);
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some((_, typed)) = ws.hint_state.as_mut() {
                    typed.pop();
                }
            }
            _ => {
                let Some(ch) = text
                    .and_then(|t| t.chars().next())
                    .filter(|c| c.is_ascii_alphabetic())
                    .map(|c| c.to_ascii_lowercase())
                else {
                    return;
                };
                // Extend the typed prefix only if it still matches a label.
                let chosen = {
                    let Some((targets, typed)) = ws.hint_state.as_mut() else {
                        return;
                    };
                    let cand = format!("{typed}{ch}");
                    if targets.iter().any(|t| t.label.starts_with(&cand)) {
                        *typed = cand;
                    }
                    let exact: Vec<&HintTarget> =
                        targets.iter().filter(|t| t.label == *typed).collect();
                    (exact.len() == 1).then(|| exact[0].clone())
                };
                if let Some(h) = chosen {
                    ws.hint_state = None;
                    self.act_hint(&h);
                }
            }
        }
    }

    fn act_hint(&mut self, h: &HintTarget) {
        if h.kind == kettle_core::hints::Kind::Url {
            // Cycle 351: route through open_url helper so the
            // hint-mode URL-open path also honors the custom URL
            // handler config.
            self.open_url(&h.text);
        } else if let Some(cb) = self.clipboard.as_mut() {
            // Cycle 777: log instead of silently swallowing.
            if let Err(e) = cb.set_text(h.text.clone()) {
                log::warn!("clipboard set_text failed (hint copy): {e}");
            }
        }
    }

    fn palette_key(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        text: Option<&str>,
        event_loop: &ActiveEventLoop,
    ) {
        let cmds = kettle_config::palette::commands();
        let Some((q, sel)) = ws.palette_input.as_mut() else {
            return;
        };
        match key {
            Key::Named(NamedKey::Escape) => {
                ws.palette_input = None;
                self.reset_blink_phase(ws);
            }
            Key::Named(NamedKey::Backspace) => {
                q.pop();
                *sel = 0;
            }
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => {
                let n = kettle_config::palette::rank(q, &cmds).len();
                if n > 0 {
                    *sel = (*sel + 1) % n;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                let n = kettle_config::palette::rank(q, &cmds).len();
                if n > 0 {
                    *sel = (*sel + n - 1) % n;
                }
            }
            Key::Named(NamedKey::Enter) => {
                let ranked = kettle_config::palette::rank(q, &cmds);
                let action = ranked.get(*sel).map(|&i| cmds[i].1.clone());
                ws.palette_input = None;
                if let Some(a) = action {
                    self.handle_action(ws, a, event_loop);
                }
            }
            // v2.20.0 (`vim-menu-nav`): Ctrl+j/Ctrl+k (plus the telescope/
            // fzf Ctrl+n/Ctrl+p idiom) move the selection — bare letters
            // keep typing into the query. Other Ctrl-chords fall through to
            // the catch-all unchanged.
            Key::Character(s)
                if self.cfg.vim_menu_nav
                    && ws.mods.control_key()
                    && !ws.mods.alt_key()
                    && matches!(s.as_str(), "j" | "k" | "n" | "p") =>
            {
                let n = kettle_config::palette::rank(q, &cmds).len();
                if n > 0 {
                    *sel = match s.as_str() {
                        "j" | "n" => (*sel + 1) % n,
                        _ => (*sel + n - 1) % n,
                    };
                }
            }
            _ => {
                if let Some(t) = text
                    && !t.is_empty()
                    && !t.chars().any(|c| c.is_control())
                {
                    q.push_str(t);
                    *sel = 0;
                }
            }
        }
    }

    /// Cycle 756: keyboard routing while the settings overlay is open.
    /// ↑/↓ move between fields, Tab/Shift+Tab switch category, ←/→ change the
    /// focused field's value, Space/Enter activate (toggle / cycle forward),
    /// Esc closes. Every change persists to the config file via `persist_pref`
    /// and reloads live, so the effect is immediate (matching the right-click
    /// Preferences toggles).
    fn settings_key(&mut self, ws: &mut WindowState, key: &Key, _event_loop: &ActiveEventLoop) {
        let cats = crate::settings::categories(&self.gpu_choices);
        if cats.is_empty() {
            ws.settings_nav = None;
            return;
        }
        let (mut cat, mut fld, capturing) = match &ws.settings_nav {
            Some(n) => (n.category.min(cats.len() - 1), n.field, n.capturing),
            None => return,
        };
        let field_count = cats[cat].fields.len();
        if field_count == 0 {
            return;
        }
        if fld >= field_count {
            fld = field_count - 1;
        }

        // Cycle 766: chord-capture mode — the focused Keybind field is waiting
        // for the user to press a chord. Esc cancels. A bare modifier press maps
        // to `None` via `to_kkey`, so we simply stay in capture until a real key
        // arrives. Any other chord is bound to the action live AND appended to
        // the config file so it survives restart.
        if capturing {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                if let Some(n) = ws.settings_nav.as_mut() {
                    n.capturing = false;
                }
                return;
            }
            if let Some(kk) = to_kkey(key) {
                let mods = to_mods(ws.mods);
                // Cycle 835 (audit): refuse a modifier-less binding to a
                // text/essential key — it would shadow that key in normal typing
                // everywhere, persisted, with no in-overlay unbind. Stay in
                // capture mode and tell the user instead of soft-bricking.
                if !keybind_chord_is_safe(mods, kk) {
                    fire_notify(
                        "kettle: keybind needs a modifier",
                        "Hold Ctrl, Alt, or Shift with the key (or bind an F-key).",
                    );
                    return;
                }
                if let Some(action) = crate::settings::keybind_action(&cats[cat].fields[fld])
                    && let Some(act) = Action::from_name(action)
                {
                    let trig = Trigger::new(mods, kk);
                    let label = trig.label();
                    // Live: this chord now triggers the action.
                    self.cfg.keybinds.insert(trig, act);
                    // Persist: append `keybind = <chord>=<action>`.
                    if let Some(path) = self
                        .config_path
                        .clone()
                        .or_else(kettle_config::Config::default_path)
                        && let Err(e) = kettle_config::append_keybind(&path, &label, action)
                    {
                        log::warn!("append_keybind({label}={action}) failed: {e}");
                    }
                }
                if let Some(n) = ws.settings_nav.as_mut() {
                    n.capturing = false;
                }
            }
            return;
        }

        match key {
            Key::Named(NamedKey::Escape) => {
                ws.settings_nav = None;
                self.reset_blink_phase(ws);
                return;
            }
            Key::Named(NamedKey::ArrowDown) => {
                fld = crate::settings::next_enabled_field(&self.cfg, &cats[cat].fields, fld, 1)
            }
            Key::Named(NamedKey::ArrowUp) => {
                fld = crate::settings::next_enabled_field(&self.cfg, &cats[cat].fields, fld, -1)
            }
            Key::Named(NamedKey::Tab) => {
                let n = cats.len();
                cat = if ws.mods.shift_key() {
                    (cat + n - 1) % n
                } else {
                    (cat + 1) % n
                };
                fld = 0;
            }
            Key::Named(NamedKey::ArrowRight)
            | Key::Named(NamedKey::ArrowLeft)
            | Key::Named(NamedKey::Space)
            | Key::Named(NamedKey::Enter) => {
                let dir = match key {
                    Key::Named(NamedKey::ArrowRight) => 1,
                    Key::Named(NamedKey::ArrowLeft) => -1,
                    _ => 0, // Space / Enter = activate (cycle forward / toggle)
                };
                self.settings_adjust(ws, &cats, cat, fld, dir);
            }
            // v2.20.0 (`vim-menu-nav`): j/k mirror ↓/↑, h/l mirror ←/→,
            // g/G jump to the first/last field, Ctrl+d / Ctrl+u move half a
            // page. Chord-capture mode (handled before this match) already
            // consumed the key, so capturing a chord like bare `j` (refused
            // anyway) or `Ctrl+d` still reaches the capture branch.
            Key::Character(s)
                if self.cfg.vim_menu_nav && !ws.mods.alt_key() && !ws.mods.super_key() =>
            {
                let half = (field_count / 2).max(1);
                // v2.20.0 (review fix): case-fold so CapsLock can't kill the
                // nav layer; g-vs-G derives from the physical Shift state.
                let folded = s.to_ascii_lowercase();
                match (folded.as_str(), ws.mods.control_key()) {
                    ("j", false) => {
                        fld = crate::settings::next_enabled_field(
                            &self.cfg,
                            &cats[cat].fields,
                            fld,
                            1,
                        )
                    }
                    ("k", false) => {
                        fld = crate::settings::next_enabled_field(
                            &self.cfg,
                            &cats[cat].fields,
                            fld,
                            -1,
                        )
                    }
                    ("g", false) => {
                        fld = if ws.mods.shift_key() {
                            field_count - 1
                        } else {
                            0
                        };
                    }
                    ("h", false) => self.settings_adjust(ws, &cats, cat, fld, -1),
                    ("l", false) => self.settings_adjust(ws, &cats, cat, fld, 1),
                    ("d", true) => fld = (fld + half).min(field_count - 1),
                    ("u", true) => fld = fld.saturating_sub(half),
                    _ => {}
                }
            }
            _ => {}
        }
        if let Some(n) = ws.settings_nav.as_mut() {
            n.category = cat;
            n.field = fld;
        }
    }

    /// ←/→ (`dir = ±1`) or Space/Enter (`dir = 0`) on the focused settings
    /// row. Keybind rows enter chord-capture on activate (±1 no-ops); value
    /// rows persist the stepped value, mirror padding, and live-reload.
    /// Extracted from `settings_key`'s arrow arm in v2.20.0 so vim-menu-nav's
    /// `h`/`l` share the exact code path.
    fn settings_adjust(
        &mut self,
        ws: &mut WindowState,
        cats: &[crate::settings::Category],
        cat: usize,
        fld: usize,
        dir: i32,
    ) {
        let field = &cats[cat].fields[fld];
        // v2.24.0: a gated/inapplicable row never changes (it's drawn dimmed).
        if crate::settings::field_disabled(&self.cfg, field.key) {
            return;
        }
        if crate::settings::is_keybind(field) {
            // Activate on a keybind row → enter chord-capture; ←/→ no-op.
            if dir == 0
                && let Some(n) = ws.settings_nav.as_mut()
            {
                n.category = cat;
                n.field = fld;
                n.capturing = true;
            }
            return;
        }
        // v2.24.0: a Text row (the image path) opens an inline prompt on activate
        // (dir 0); ←/→ no-op. A gated/disabled Text row can't be edited.
        if crate::settings::is_text(field) {
            if dir == 0 && !crate::settings::field_disabled(&self.cfg, field.key) {
                if let Some(n) = ws.settings_nav.as_mut() {
                    n.category = cat;
                    n.field = fld;
                }
                self.open_settings_text_edit(ws, field.key);
            }
            return;
        }
        // Scope the cfg borrow so reload_config (&mut self) is free.
        let (key_str, new_val) = {
            (
                field.key,
                crate::settings::next_value(&self.cfg, field, dir),
            )
        };
        // v2.23.0: the GPU device picker is one row but persists THREE keys
        // (vendor/device/name). "auto" clears the pin; "<vendor>:<device>:<name>"
        // sets it. GPU changes apply on the NEXT launch (the wgpu device/surface
        // graph can't hot-swap and every window shares one adapter), so we flag
        // a restart affordance instead of rebuilding the renderer live.
        if key_str == "gpu" {
            if new_val == "auto" {
                self.persist_pref("gpu-vendor-id", "0");
                self.persist_pref("gpu-device-id", "0");
                self.persist_pref("gpu-name", "");
            } else if let Some((v, rest)) = new_val.split_once(':')
                && let Some((d, name)) = rest.split_once(':')
            {
                self.persist_pref("gpu-vendor-id", &format!("0x{v}"));
                self.persist_pref("gpu-device-id", &format!("0x{d}"));
                self.persist_pref("gpu-name", name);
            }
            ws.settings_restart_pending = true;
            self.reload_config(ws);
            return;
        }
        // Cycle 919 (audit L4): notify if the Settings change can't
        // be written — it's live this session but lost on restart.
        if !self.persist_pref(key_str, &new_val) {
            fire_notify(
                "kettle: setting not saved",
                "Applied for this session — couldn't write it to your config file.",
            );
        }
        // Cycle 856 (audit): the single "Window padding" control is
        // meant to set *uniform* padding, but persisted only the X
        // axis — leaving `window-padding-y` at its default produced
        // visibly lopsided padding. Mirror the value to the Y axis.
        if key_str == "window-padding-x" {
            self.persist_pref("window-padding-y", &new_val);
        }
        // v2.23.0: the remaining GPU policy keys (power preference / backend /
        // force-software) also only take effect on restart.
        if matches!(
            key_str,
            "gpu-power-preference" | "gpu-backend" | "gpu-force-software"
        ) {
            ws.settings_restart_pending = true;
        }
        self.reload_config(ws);
        // Cycle 918: the cycle-880 `save_session()`-for-theme band-aid
        // is gone. It existed only to defend against startup's
        // session-theme override (now removed) reverting a Settings
        // pick after an unclean exit. The pick is durably written to
        // the config `theme =` line by `persist_pref` above, which is
        // the single source of truth on restart.
    }

    /// v2.24.0: open the inline text prompt for a [`crate::settings::FieldKind::Text`]
    /// row, pre-filled with the current config value.
    fn open_settings_text_edit(&mut self, ws: &mut WindowState, key: &'static str) {
        let cur = match key {
            "background-image" => self.cfg.background_image.clone(),
            _ => String::new(),
        };
        ws.settings_text_edit = Some(crate::settings::SettingsTextEdit { key, buf: cur });
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// v2.24.0: keyboard routing while the settings inline text prompt is open.
    /// Esc cancels, Enter persists + live-reloads, Backspace deletes, printable
    /// text appends (control chars filtered, like the ssh/palette inputs).
    fn settings_text_key(&mut self, ws: &mut WindowState, key: &Key, text: Option<&str>) {
        match key {
            Key::Named(NamedKey::Escape) => {
                ws.settings_text_edit = None;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(e) = ws.settings_text_edit.as_mut() {
                    e.buf.pop();
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(e) = ws.settings_text_edit.take() {
                    let val = e.buf.trim().to_string();
                    if !self.persist_pref(e.key, &val) {
                        fire_notify(
                            "kettle: setting not saved",
                            "Applied for this session — couldn't write it to your config file.",
                        );
                    }
                    self.reload_config(ws);
                }
            }
            _ => {
                if let Some(t) = text
                    && !t.chars().any(|c| c.is_control())
                    && let Some(e) = ws.settings_text_edit.as_mut()
                {
                    e.buf.push_str(t);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
        }
    }

    /// Cycle 708 (Terminator parity, `layoutlauncher.py`):
    /// keyboard routing while the layout picker overlay is open.
    /// Same shape as `palette_key` but ranks against
    /// `Session::list_layouts()` and dispatches by spawning
    /// `kettle --layout NAME` as a new window.
    fn layout_picker_key(&mut self, ws: &mut WindowState, key: &Key, text: Option<&str>) {
        let layouts = crate::session::Session::list_layouts();
        let Some((q, sel)) = ws.layout_picker_input.as_mut() else {
            return;
        };
        match key {
            Key::Named(NamedKey::Escape) => {
                ws.layout_picker_input = None;
                self.reset_blink_phase(ws);
            }
            Key::Named(NamedKey::Backspace) => {
                q.pop();
                *sel = 0;
            }
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => {
                let n = rank_layouts(q, &layouts).len();
                if n > 0 {
                    *sel = (*sel + 1) % n;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                let n = rank_layouts(q, &layouts).len();
                if n > 0 {
                    *sel = (*sel + n - 1) % n;
                }
            }
            // v2.20.0 (`vim-menu-nav`): Ctrl+j/k (+ Ctrl+n/p) move the
            // selection; bare letters keep typing into the filter.
            Key::Character(s)
                if self.cfg.vim_menu_nav
                    && ws.mods.control_key()
                    && !ws.mods.alt_key()
                    && matches!(s.as_str(), "j" | "k" | "n" | "p") =>
            {
                let n = rank_layouts(q, &layouts).len();
                if n > 0 {
                    *sel = match s.as_str() {
                        "j" | "n" => (*sel + 1) % n,
                        _ => (*sel + n - 1) % n,
                    };
                }
            }
            Key::Named(NamedKey::Enter) => {
                let ranked = rank_layouts(q, &layouts);
                let name = ranked.get(*sel).map(|&i| layouts[i].clone());
                ws.layout_picker_input = None;
                if let Some(name) = name {
                    let exe = std::env::current_exe()
                        .unwrap_or_else(|_| std::path::PathBuf::from("kettle"));
                    if let Err(e) = std::process::Command::new(&exe)
                        .arg("--layout")
                        .arg(&name)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                    {
                        log::warn!("layout picker: spawn `kettle --layout {name}`: {e}");
                    }
                }
            }
            _ => {
                if let Some(t) = text
                    && !t.is_empty()
                    && !t.chars().any(|c| c.is_control())
                {
                    q.push_str(t);
                    *sel = 0;
                }
            }
        }
    }

    /// Esc / `h` semantics: pop a drilled-in submenu back to its parent, or
    /// close the menu when at the top level. Extracted from the Esc arm in
    /// v2.20.0 so vim-menu-nav's `h` shares the exact code path.
    fn context_menu_back(&mut self, ws: &mut WindowState) {
        // Cycle 687 (theme-submenu sub-cycle 3): Esc on
        // a drilled-in submenu pops back to the parent
        // instead of closing the menu entirely. Only
        // when drill_stack is empty does Esc close.
        if let Some(menu) = ws.context_menu.as_mut()
            && let Some(parent) = menu.drill_stack.pop()
        {
            menu.items = parent;
            // Cycle 714: restore the parent level's
            // scroll_offset so popping out of a deep theme
            // list doesn't snap the parent's scroll position
            // to 0.
            menu.scroll_offset = menu.scroll_stack.pop().unwrap_or(0);
            menu.highlight = menu
                .items
                .iter()
                .position(item_is_dispatchable)
                .unwrap_or(0);
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
            return;
        }
        ws.context_menu = None;
        self.reset_blink_phase(ws);
    }

    /// Enter / Space / `l` semantics: dispatch the highlighted row through the
    /// shared mapper. Extracted from the Enter arm in v2.20.0 so
    /// vim-menu-nav's `l` shares the exact code path.
    fn context_menu_activate(&mut self, ws: &mut WindowState, event_loop: &ActiveEventLoop) {
        // Cycle 890 (audit): resolve the highlighted row through the
        // shared mapper so Enter / Space dispatches *every* row type
        // — submenu (drills in), Lua item, config command, theme /
        // profile choice, new-tab ▾ shell — not just `Item`. The
        // mapper + dispatcher also own the close-or-keep decision, so
        // a submenu Enter no longer wrongly closes the menu.
        let chosen = ws.context_menu.as_ref().and_then(|m| {
            m.items
                .get(m.highlight)
                .and_then(|it| item_to_click(it, m.highlight))
        });
        self.reset_blink_phase(ws);
        match chosen {
            Some(click) => self.dispatch_context_menu_click(ws, click, event_loop),
            // Highlight on a non-dispatchable row (shouldn't happen —
            // nav skips them — but close rather than trap the user).
            None => ws.context_menu = None,
        }
    }

    /// v2.20.0 (`vim-menu-nav`): the context menu's vim layer. Returns `true`
    /// when the key was consumed. Called BEFORE the mnemonic/typeahead
    /// catch-all (which eats every bare a–z) so nav letters always navigate;
    /// `assign_mnemonics` reserves the same letters (`VIM_NAV_RESERVED`) so
    /// no row ever points at a key this layer intercepts.
    fn context_menu_vim_key(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        let Key::Character(s) = key else {
            return false;
        };
        if ws.mods.alt_key() || ws.mods.super_key() {
            return false;
        }
        let ctrl = ws.mods.control_key();
        // v2.20.0 (review fix): case-fold so CapsLock can't kill the nav
        // layer (letters arrive uppercased); g-vs-G derives from the
        // PHYSICAL Shift state, not character case, so CapsLock+g still
        // means "first" (the user didn't press Shift).
        let folded = s.to_ascii_lowercase();
        match (folded.as_str(), ctrl) {
            ("j", false) => {
                self.step_context_menu_highlight(ws, 1);
                true
            }
            ("k", false) => {
                self.step_context_menu_highlight(ws, -1);
                true
            }
            ("g", false) => {
                let last = ws.mods.shift_key();
                let next = ws.context_menu.as_ref().and_then(|m| {
                    if last {
                        m.items.iter().rposition(item_is_dispatchable)
                    } else {
                        m.items.iter().position(item_is_dispatchable)
                    }
                });
                if let Some(next) = next {
                    self.set_context_menu_highlight(ws, next);
                }
                true
            }
            ("h", false) => {
                self.context_menu_back(ws);
                true
            }
            ("l", false) => {
                self.context_menu_activate(ws, event_loop);
                true
            }
            ("d", true) | ("u", true) => {
                let dir: isize = if s.as_str() == "d" { 1 } else { -1 };
                let Some(((_, _), (_, panel_h))) = self.context_menu_geometry(ws) else {
                    return true;
                };
                let (_, ch) = self.menu_cell(ws);
                let row_h = ch + kettle_render::menu::ROW_PAD;
                let sep_h = kettle_render::menu::SEP_H;
                let next = ws.context_menu.as_ref().map(|m| {
                    let visible =
                        count_rows_fitting(&m.items, m.scroll_offset, panel_h, row_h, sep_h);
                    half_page_menu_target(&m.items, m.highlight, (visible / 2).max(1), dir)
                });
                if let Some(next) = next {
                    self.set_context_menu_highlight(ws, next);
                }
                true
            }
            _ => false,
        }
    }

    fn context_menu_key(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        text: Option<&str>,
        event_loop: &ActiveEventLoop,
    ) {
        // v2.20.0: vim navigation intercepts BEFORE the mnemonic/typeahead
        // catch-all below — otherwise bare `j`/`k`/… would be eaten as
        // mnemonic/typeahead input. Disabled (`vim-menu-nav = false`)
        // restores the pre-v2.20.0 behavior byte-for-byte.
        // Keyboard routing while the right-click context menu is open:
        // `Esc` closes (or pops a submenu), `↑/↓` step the highlight
        // (skipping separators + disabled rows), `Enter`/`Space` fire the
        // highlighted action, and bare letters dispatch mnemonics /
        // typeahead. Any other key is swallowed so a stray keypress doesn't
        // leak into the focused pane while the menu is expecting nav input.
        if self.cfg.vim_menu_nav && self.context_menu_vim_key(ws, key, event_loop) {
            return;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                self.context_menu_back(ws);
            }
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => {
                self.step_context_menu_highlight(ws, 1);
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.step_context_menu_highlight(ws, -1);
            }
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                self.context_menu_activate(ws, event_loop);
            }
            _ => {
                // Cycle 715 (Terminator menu UX, C6): mnemonics +
                // typeahead. A single A-Z keystroke dispatches a row
                // whose mnemonic char matches; multi-char accumulates
                // into `typeahead_buf` for prefix-match. Buffer
                // clears after 750ms of inactivity.
                let Some(t) = text else { return };
                let mut chars = t.chars();
                let Some(c) = chars.next() else { return };
                if chars.next().is_some() || !c.is_ascii_alphabetic() {
                    // Multi-char text events or non-alpha (digits,
                    // punctuation, IME composition) are ignored.
                    return;
                }
                let lower = c.to_ascii_lowercase();
                let now = std::time::Instant::now();
                // Clear stale typeahead buffer.
                if let Some(menu) = ws.context_menu.as_mut()
                    && menu
                        .typeahead_until
                        .map(|deadline| now > deadline)
                        .unwrap_or(false)
                {
                    menu.typeahead_buf.clear();
                    menu.typeahead_until = None;
                }
                // First key after a long pause: try mnemonic dispatch
                // (single-char). On a hit, dispatch the row + close
                // the menu (matches Win32 / GTK convention). On a
                // miss, accumulate into typeahead.
                // v2.20.0: with vim-menu-nav on, the nav letters are reserved
                // — they were intercepted above and must never match a row.
                let reserved: &[char] = if self.cfg.vim_menu_nav {
                    VIM_NAV_RESERVED
                } else {
                    &[]
                };
                let mnemonic_hit = ws.context_menu.as_ref().and_then(|menu| {
                    if !menu.typeahead_buf.is_empty() {
                        return None;
                    }
                    let mn = assign_mnemonics(&menu.items, reserved);
                    mn.iter().enumerate().find_map(|(idx, slot)| {
                        slot.and_then(|(_, ch)| (ch == lower).then_some(idx))
                    })
                });
                if let Some(idx) = mnemonic_hit {
                    // Cycle 890: dispatch the matched row through the same
                    // shared mapper + sink as mouse clicks and Enter / Space,
                    // so a mnemonic reaches every row type (Lua / config /
                    // ▾ shell included) and the close-or-drill decision stays
                    // in one place.
                    let click = ws
                        .context_menu
                        .as_ref()
                        .and_then(|m| m.items.get(idx).and_then(|it| item_to_click(it, idx)));
                    if let Some(click) = click {
                        self.dispatch_context_menu_click(ws, click, event_loop);
                        return;
                    }
                }
                // Typeahead path: accumulate into the buffer, find
                // the first row whose label has this prefix, advance
                // highlight without dispatching.
                if let Some(menu) = ws.context_menu.as_mut() {
                    menu.typeahead_buf.push(lower);
                    menu.typeahead_until = Some(now + std::time::Duration::from_millis(750));
                    if let Some(idx) = typeahead_match(&menu.items, &menu.typeahead_buf) {
                        menu.highlight = idx;
                    }
                }
            }
        }
    }

    fn ssh_key(&mut self, ws: &mut WindowState, key: &Key, text: Option<&str>) {
        match key {
            Key::Named(NamedKey::Escape) => {
                ws.ssh_input = None;
                self.reset_blink_phase(ws);
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(q) = ws.ssh_input.as_mut() {
                    q.pop();
                }
            }
            Key::Named(NamedKey::Tab) => {
                // Fuzzy-complete to the best-matching configured host name.
                let typed = ws.ssh_input.clone().unwrap_or_default();
                if !typed.is_empty()
                    && let Some((n, _)) =
                        kettle_config::fuzzy::best(&typed, &self.cfg.ssh_hosts, |h| h.0.as_str())
                {
                    ws.ssh_input = Some(n.clone());
                }
            }
            Key::Named(NamedKey::Enter) => {
                let typed = ws.ssh_input.take().unwrap_or_default();
                let target = self
                    .cfg
                    .ssh_hosts
                    .iter()
                    .find(|(n, _)| *n == typed)
                    .map(|(_, t)| t.clone())
                    // No exact name → best fuzzy host match for the query.
                    .or_else(|| {
                        if typed.trim().is_empty() {
                            return None;
                        }
                        kettle_config::fuzzy::best(typed.trim(), &self.cfg.ssh_hosts, |h| {
                            h.0.as_str()
                        })
                        .map(|(_, t)| t.clone())
                    })
                    .or_else(|| {
                        if !typed.trim().is_empty() {
                            Some(typed.trim().to_string())
                        } else {
                            self.cfg.ssh_hosts.first().map(|(_, t)| t.clone())
                        }
                    });
                if let Some(target) = target {
                    let area = self.area(ws);
                    let (cols, rows) = self.grid_of(ws, area);
                    let (cw, ch) = self.cell_px(ws);
                    if let Err(e) =
                        ws.mux
                            .new_ssh_tab(&self.cfg, cols, rows, cw, ch, self.waker(), &target)
                    {
                        log::error!("ssh launch failed: {e}");
                    }
                    self.resize_all(ws);
                    self.save_session(ws);
                }
            }
            _ => {
                // Cycle 863 (audit): filter control chars before appending,
                // like the search / palette / title handlers — the cycle-857
                // comment claimed this handler already did, but it didn't.
                if let Some(t) = text
                    && !t.chars().any(|c| c.is_control())
                    && let Some(q) = ws.ssh_input.as_mut()
                {
                    q.push_str(t);
                }
            }
        }
    }
}

/// Cycle 352 (Terminator parity): remap encoded Backspace/Delete bytes per
/// the user's `backspace-binding`/`delete-binding`. Extracted in v2.20.0
/// (review fix) so `send_keys` honors the same remap as GUI keystrokes —
/// the "same path as a human key press" contract.
fn apply_bs_del_binding(cfg: &Config, key: &Key, mods: ModifiersState, bytes: Vec<u8>) -> Vec<u8> {
    let Key::Named(named) = key else {
        return bytes;
    };
    use kettle_config::{BackspaceBinding, DeleteBinding};
    if *named == NamedKey::Backspace && !mods.control_key() && !mods.alt_key() {
        match cfg.backspace_binding {
            BackspaceBinding::AsciiDel => vec![0x7f],
            BackspaceBinding::ControlH => vec![0x08],
            BackspaceBinding::EscapeSequence => b"\x1b[3~".to_vec(),
            BackspaceBinding::Automatic => bytes,
        }
    } else if *named == NamedKey::Delete {
        match cfg.delete_binding {
            DeleteBinding::AsciiDel => vec![0x7f],
            DeleteBinding::ControlH => vec![0x08],
            DeleteBinding::EscapeSequence => b"\x1b[3~".to_vec(),
            DeleteBinding::Automatic => bytes,
        }
    } else {
        bytes
    }
}

/// v2.20.0 (agent plane): parse one `send_keys` token — `"escape"`,
/// `"ctrl+c"`, `"shift+tab"`, `"f5"`, `"alt+enter"`, a bare character like
/// `"G"` — into the `(mods, key)` pair the GUI's PTY encoder
/// (`input::encode`) consumes. Same `+`-separated grammar and modifier
/// aliases as config keybind triggers (`parse_trigger`), plus the named keys
/// the keybind grammar has no variant for (escape, backspace, delete,
/// insert, space) — those are exactly the keys agents drive TUIs with.
/// Character case is preserved (`"G"` sends `G`, like a human holding
/// Shift). `None` on an unrecognized token, so the caller can name the bad
/// token instead of sending wrong bytes. Pure (unit-tested).
fn parse_send_key(token: &str) -> Option<(ModifiersState, Key)> {
    let parts: Vec<&str> = token.split('+').collect();
    let last = parts.len().checked_sub(1)?;
    let mut mods = ModifiersState::empty();
    let mut key: Option<Key> = None;
    for (i, part) in parts.iter().enumerate() {
        let raw = part.trim();
        let lower = raw.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => mods |= ModifiersState::CONTROL,
            "alt" | "opt" | "option" => mods |= ModifiersState::ALT,
            "shift" => mods |= ModifiersState::SHIFT,
            "super" | "cmd" | "command" | "win" | "windows" | "meta" | "logo" => {
                mods |= ModifiersState::SUPER;
            }
            _ => {
                // Only the LAST slot may be a key (same rule as parse_trigger:
                // a typo'd modifier must fail loudly, not bind a plain key).
                if i != last {
                    return None;
                }
                // Named characters first (review fix: `,` was unreachable
                // from the CLI's comma-split `--keys`, `+` from every client
                // because it's the chord separator). Mirrors `parse_key`'s
                // plus/minus/equal aliases.
                match lower.as_str() {
                    "plus" => {
                        key = Some(Key::Character("+".into()));
                        continue;
                    }
                    "comma" => {
                        key = Some(Key::Character(",".into()));
                        continue;
                    }
                    "minus" => {
                        key = Some(Key::Character("-".into()));
                        continue;
                    }
                    "equal" => {
                        key = Some(Key::Character("=".into()));
                        continue;
                    }
                    _ => {}
                }
                let named = match lower.as_str() {
                    "escape" | "esc" => Some(NamedKey::Escape),
                    "enter" | "return" => Some(NamedKey::Enter),
                    "tab" => Some(NamedKey::Tab),
                    "backspace" | "bs" => Some(NamedKey::Backspace),
                    "delete" | "del" => Some(NamedKey::Delete),
                    "insert" | "ins" => Some(NamedKey::Insert),
                    "space" => Some(NamedKey::Space),
                    "up" => Some(NamedKey::ArrowUp),
                    "down" => Some(NamedKey::ArrowDown),
                    "left" => Some(NamedKey::ArrowLeft),
                    "right" => Some(NamedKey::ArrowRight),
                    "home" => Some(NamedKey::Home),
                    "end" => Some(NamedKey::End),
                    "page_up" | "pageup" | "prior" => Some(NamedKey::PageUp),
                    "page_down" | "pagedown" | "next" => Some(NamedKey::PageDown),
                    "f1" => Some(NamedKey::F1),
                    "f2" => Some(NamedKey::F2),
                    "f3" => Some(NamedKey::F3),
                    "f4" => Some(NamedKey::F4),
                    "f5" => Some(NamedKey::F5),
                    "f6" => Some(NamedKey::F6),
                    "f7" => Some(NamedKey::F7),
                    "f8" => Some(NamedKey::F8),
                    "f9" => Some(NamedKey::F9),
                    "f10" => Some(NamedKey::F10),
                    "f11" => Some(NamedKey::F11),
                    "f12" => Some(NamedKey::F12),
                    _ => None,
                };
                key = Some(match named {
                    Some(n) => Key::Named(n),
                    None => {
                        // A single character, case preserved.
                        let mut ch = raw.chars();
                        let mut c = ch.next()?;
                        if ch.next().is_some() {
                            return None;
                        }
                        // Review fixes: `super+<char>` has NO PTY encoding —
                        // fail loudly rather than silently dropping the
                        // modifier. `shift+<letter>` normalizes to the
                        // uppercase character with SHIFT cleared (exactly
                        // what a human's Shift press delivers; the encoder's
                        // Character arm ignores SHIFT, so `shift+g` would
                        // otherwise silently send lowercase `g`).
                        if mods.contains(ModifiersState::SUPER) {
                            return None;
                        }
                        if mods.contains(ModifiersState::SHIFT) && c.is_ascii_alphabetic() {
                            c = c.to_ascii_uppercase();
                            mods.remove(ModifiersState::SHIFT);
                        }
                        Key::Character(c.to_string().into())
                    }
                });
            }
        }
    }
    key.map(|k| (mods, k))
}

fn to_kkey(key: &Key) -> Option<KKey> {
    Some(match key {
        Key::Character(s) => KKey::Char(s.chars().next()?.to_ascii_lowercase()),
        Key::Named(n) => match n {
            NamedKey::ArrowUp => KKey::Up,
            NamedKey::ArrowDown => KKey::Down,
            NamedKey::ArrowLeft => KKey::Left,
            NamedKey::ArrowRight => KKey::Right,
            NamedKey::PageUp => KKey::PageUp,
            NamedKey::PageDown => KKey::PageDown,
            NamedKey::Home => KKey::Home,
            NamedKey::End => KKey::End,
            NamedKey::Enter => KKey::Enter,
            NamedKey::Tab => KKey::Tab,
            NamedKey::F1 => KKey::F(1),
            NamedKey::F2 => KKey::F(2),
            NamedKey::F3 => KKey::F(3),
            NamedKey::F4 => KKey::F(4),
            NamedKey::F5 => KKey::F(5),
            NamedKey::F6 => KKey::F(6),
            NamedKey::F7 => KKey::F(7),
            NamedKey::F8 => KKey::F(8),
            NamedKey::F9 => KKey::F(9),
            NamedKey::F10 => KKey::F(10),
            NamedKey::F11 => KKey::F(11),
            NamedKey::F12 => KKey::F(12),
            _ => return None,
        },
        _ => return None,
    })
}

/// Whether a captured `(mods, key)` chord is safe to bind from the settings
/// keybind-capture overlay.
///
/// Cycle 835 (audit): a modifier-LESS chord is rejected unless the key is an
/// F-key. Binding e.g. a bare `a` (a mis-press during capture) inserted
/// `Trigger { mods: empty, key: Char('a') }` into the keybinds AND the config
/// file; afterward the global key path matched it before text encoding, so
/// every future `a` fired the action instead of typing — across all panes,
/// persisted across restarts, with no in-overlay unbind. Enter/Tab/arrows are
/// likewise essential unmodified, so only F-keys (which produce no text) may be
/// bound without a modifier.
fn keybind_chord_is_safe(mods: Mods, key: KKey) -> bool {
    !mods.is_empty() || matches!(key, KKey::F(_))
}

fn to_mods(m: ModifiersState) -> Mods {
    let mut out = Mods::empty();
    if m.shift_key() {
        out |= Mods::SHIFT;
    }
    if m.control_key() {
        out |= Mods::CTRL;
    }
    if m.alt_key() {
        out |= Mods::ALT;
    }
    if m.super_key() {
        out |= Mods::SUPER;
    }
    out
}

/// v2.24.0: how the in-settings text-edit buffer is shown in the value column —
/// the whole string when short, else an ellipsized tail so a long path stays
/// readable (the caret/end is what matters while typing) without ballooning the
/// panel width.
fn settings_edit_display(buf: &str) -> String {
    let count = buf.chars().count();
    if count <= 40 {
        return buf.to_string();
    }
    let tail: String = buf.chars().skip(count - 39).collect();
    format!("…{tail}")
}

/// Startup visibility policy. Windows are created hidden while renderer init
/// runs, then visible states are revealed once the wgpu surface is configured.
/// Only `window_state = hidden` remains hidden.
fn should_reveal_after_renderer_init(state: kettle_config::WindowState) -> bool {
    !matches!(state, kettle_config::WindowState::Hidden)
}

/// Cycle 919 (audit M1/M2): is the default last-session (`session.json`) active
/// for THIS launch? Drives BOTH the startup restore gate (whether to `load()`)
/// and the `save_session` gate (whether to `save()` the default session). They
/// MUST agree: cycle 918 made *load* opt-in (fresh windows by default) but left
/// *save* unconditional, so a fresh, non-opted-in window silently overwrote the
/// saved layout that `--restore` exists to recover — data loss against the
/// feature's own contract. Routing both through this one predicate keeps them
/// symmetric. `--layout NAME` is independent (its own file, explicit intent) and
/// always saves/loads regardless.
fn should_restore_session(startup_restore: bool, cfg_restore_session: bool) -> bool {
    startup_restore || cfg_restore_session
}

/// Cycle 812 (audit #10): how long to let the synchronous GPU adapter+device
/// init run before treating it as a hung graphics driver. Real init is ~1.5s;
/// this is deliberately generous so a slow-but-working GPU is never killed,
/// while still bounding the worst case (a wedged driver that never returns)
/// to a clean diagnostic exit instead of an infinite invisible-window hang.
const GPU_INIT_TIMEOUT_SECS: u64 = 30;

/// Watchdog body for the GPU-init guard (cycle 812). Polls `done` every `step`
/// until `timeout` elapses. Returns `true` if the timeout was reached without
/// `done` ever being observed set — i.e. the caller should treat the init as
/// hung — and `false` if `done` was seen in time (init finished, watchdog
/// stands down). Pulled out as a pure-ish helper so the timeout logic is
/// unit-testable without standing up a GPU or calling `process::exit`.
fn gpu_init_timed_out(
    done: &std::sync::atomic::AtomicBool,
    timeout: std::time::Duration,
    step: std::time::Duration,
) -> bool {
    use std::sync::atomic::Ordering;
    let mut waited = std::time::Duration::ZERO;
    while waited < timeout {
        if done.load(Ordering::Acquire) {
            return false;
        }
        std::thread::sleep(step);
        waited += step;
    }
    !done.load(Ordering::Acquire)
}

/// Cycle 786: should an open modal swallow a pointer event (mouse press /
/// wheel) instead of letting it fall through to the tab bar, pane focus, or
/// mouse-tracking *behind* the dialog? True whenever any modal is open —
/// search / palette / ssh / settings / layout-picker / hint / confirm dialog /
/// inline title-edit / vi copy-mode — *except* a lone context menu, which owns
/// its own click/scroll paths above and is re-opened (relocated) by a
/// right-click below, so gating it here would break that. Before this cycle a
/// click switched tabs / focused a pane and a wheel zoomed the font or scrolled
/// the pane while a dialog the user thought was capturing input sat on top.
fn modal_swallows_pointer(any_modal_open: bool, context_menu_open: bool) -> bool {
    any_modal_open && !context_menu_open
}

impl ApplicationHandler<UserEvent> for App {
    // C1-DISPATCH-BEGIN: take-out/put-back window dispatch.
    //
    // Every per-window field lives on `WindowState` (window_state.rs). Each
    // handler removes the addressed window from the map, runs the inner
    // handler with disjoint `&mut self` (globals) + `&mut WindowState`
    // borrows, then hands the entry to `finish_window_dispatch` — which
    // reinserts it, or (C4) drops it when the inner handler flagged the
    // window closed, exiting the loop once no windows remain. A panic
    // mid-handler drops the entry, which is fine: kettle aborts on panic,
    // nothing observes the missing window.
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let seq = self.focused_seq;
        let Some(mut ws) = self.windows.remove(&seq) else {
            return;
        };
        self.resumed_inner(&mut ws, event_loop);
        self.finish_window_dispatch(event_loop, seq, ws);
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            // C4: PTY wakeups carry no pane id and config reloads re-style
            // everything — fan these out to every window. The Wakeup arm
            // gates per window on the panes' output generations, so only
            // windows with fresh output repaint.
            UserEvent::Wakeup | UserEvent::ReloadConfig => {
                // v2.20.0 P4: reopen the wakeup latch BEFORE any generation
                // reads below — a reader that bumps its generation after this
                // store enqueues a fresh Wakeup, so the new output is painted
                // either by this pass (we see the bump) or by the next event
                // (we already woke for it). Clearing on ReloadConfig too is
                // harmless: the worst case is one extra queued event.
                // (swap with AcqRel rather than a plain Release store: the
                // acquire edge pairs with the reader's swap — the textbook
                // consumer side of a wake flag.)
                self.wake_pending
                    .swap(false, std::sync::atomic::Ordering::AcqRel);
                let seqs: Vec<u64> = self.windows.keys().copied().collect();
                for seq in seqs {
                    let Some(mut ws) = self.windows.remove(&seq) else {
                        continue;
                    };
                    self.user_event_inner(&mut ws, el, ev.clone());
                    self.finish_window_dispatch(el, seq, ws);
                }
            }
            // Ctl / remote / update-banner events act on the focused window.
            _ => {
                let seq = self.focused_seq;
                let Some(mut ws) = self.windows.remove(&seq) else {
                    return;
                };
                self.user_event_inner(&mut ws, el, ev);
                self.finish_window_dispatch(el, seq, ws);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // An unknown WindowId (a late event for an already-closed window)
        // is dropped rather than misrouted to the focused window.
        let Some(seq) = self.seq_of_window(id) else {
            return;
        };
        let Some(mut ws) = self.windows.remove(&seq) else {
            return;
        };
        self.window_event_inner(&mut ws, event_loop, event);
        // v2.24.0: single chokepoint for the live theme preview. After every
        // event, make `cfg.theme` reflect the context-menu highlight — apply the
        // hovered `ThemeChoice` ephemerally, or revert to the baseline once the
        // highlight leaves a theme row OR the menu closes without committing.
        self.sync_theme_preview(&mut ws);
        self.finish_window_dispatch(event_loop, seq, ws);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // v2.19.0 (tear-off UX): failsafe — torn-drag tracking that lost its
        // drop signal (an X11 release the WM swallowed, then no further
        // input) is abandoned after 120s of silence. The window is long
        // because a LIVE-but-stationary drag is indistinguishable from
        // stale tracking on X11 (about_to_wait keeps running there while
        // the WM moves the window, and a motionless hover generates no
        // Moved events) — 120s only fires on a hover no real gesture
        // holds, and the cost of waiting is bounded (the next press or
        // release also cleans stale tracking). Windows can't misfire
        // either way: the modal move loop doesn't run about_to_wait.
        if self
            .torn_drag
            .as_ref()
            .is_some_and(|t| t.last_signal.elapsed().as_secs() >= 120)
        {
            self.abandon_torn_drag(None);
        }
        // C4: every window ticks (reap, blink, coalesced-paint deadlines);
        // the per-window wait requests merge to the EARLIEST deadline so one
        // window's animation can't starve another's coalesced output flush.
        let seqs: Vec<u64> = self.windows.keys().copied().collect();
        let mut min_wait: Option<u64> = None;
        for seq in seqs {
            let Some(mut ws) = self.windows.remove(&seq) else {
                continue;
            };
            let wait = self.about_to_wait_inner(&mut ws, event_loop);
            self.finish_window_dispatch(event_loop, seq, ws);
            if let Some(ms) = wait {
                min_wait = Some(min_wait.map_or(ms, |w: u64| w.min(ms)));
            }
        }
        if self.windows.is_empty() {
            return;
        }
        match min_wait {
            Some(ms) => event_loop.set_control_flow(ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(ms),
            )),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
    // C1-DISPATCH-END
}

/// C4 (multi-window): how a new in-process window starts life.
enum WindowOpen {
    /// A fresh window with one shell tab (`Action::NewWindow`).
    Fresh { cwd: Option<String> },
    /// Adopt a tab detached from another window — the live tab move /
    /// tear-off. PTYs keep running; nothing respawns.
    AdoptTab(crate::mux::DetachedTab),
    /// C7: respawn one saved window of a multi-window session (tabs +
    /// geometry) on `--restore` / `restore-session = true` startup.
    Restore(crate::session::SWindow),
}

/// B (Peacock): pure pool-slot picker — start at the seed's slot, advance to
/// the first hue no live window uses; a fully-claimed pool accepts the seed
/// slot (a rare same-color pair beats inventing off-theme colors).
fn pick_accent_slot(
    pool: &[kettle_config::Rgb],
    seed: u64,
    in_use: &[kettle_config::Rgb],
) -> usize {
    if pool.is_empty() {
        return 0;
    }
    let start = (seed % pool.len() as u64) as usize;
    (0..pool.len())
        .map(|i| (start + i) % pool.len())
        .find(|&i| !in_use.contains(&pool[i]))
        .unwrap_or(start)
}

/// B (Peacock): `#rrggbb` for the presence registry's wire format.
fn rgb_hex(c: kettle_config::Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// v2.19.0 (tear-off UX, T5): is this window's backend Wayland? Runtime
/// check — X11 vs Wayland is a runtime choice on Linux, so `cfg!` can't
/// answer it. Wayland gets the tear-at-release fallback (no client-side
/// window positioning, and `xdg_toplevel.move` is compositor-validated
/// against a press serial a just-created surface never saw).
fn window_is_wayland(w: &winit::window::Window) -> bool {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    matches!(
        w.window_handle().map(|h| h.as_raw()),
        Ok(RawWindowHandle::Wayland(_))
    )
}

/// v2.19.0 (tear-off UX, Windows): the LIVE screen cursor. The tear
/// positions the torn window from the CursorMoved event's coordinates,
/// but window creation takes ~50-150ms — by the time `drag_window()`'s
/// posted WM_NCLBUTTONDOWN starts the modal move loop, a fast-moving
/// pointer has slid well past the event position, and DefWindowProc
/// anchors the loop at the CURRENT cursor. Without re-anchoring, the
/// pointer ends up holding the window mid-body instead of at the grab
/// point (measured: ~97px of slide under a scripted drag). Re-positioning
/// from the live cursor immediately before the handoff closes the gap to
/// sub-millisecond.
#[cfg(target_os = "windows")]
fn cursor_screen_pos() -> Option<(f64, f64)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }
        .ok()
        .map(|()| (f64::from(p.x), f64::from(p.y)))
}

/// v2.19.0 (re-dock, Windows): per-window alpha for the dock-hover
/// preview. The torn window rides UNDER the pointer, so at the moment a
/// dock target latches, the full-size torn window is covering the very
/// strip the insertion marker draws in — the user would never see it.
/// Going translucent while a target is latched shows the target strip
/// (and marker) through the dragged window, Chromium-style. Verified
/// live against kettle's wgpu flip-model swapchain (renders fine under
/// WS_EX_LAYERED + LWA_ALPHA). 255 restores and drops the layered bit.
#[cfg(target_os = "windows")]
fn set_window_alpha(hwnd: isize, alpha: u8) {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW,
    };
    let h = HWND(hwnd as *mut core::ffi::c_void);
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        if alpha == 255 {
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex & !(WS_EX_LAYERED_BIT));
        } else {
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_LAYERED_BIT);
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), alpha, LWA_ALPHA);
        }
    }
}

/// `WS_EX_LAYERED` as the isize `GetWindowLongPtrW` works in.
#[cfg(target_os = "windows")]
const WS_EX_LAYERED_BIT: isize = 0x0008_0000;

/// v2.19.0 (re-dock): torn-window alpha while a dock target is latched —
/// translucent enough to read the target strip + insertion marker through
/// the dragged window, opaque enough that the window still reads as "the
/// thing you're holding" (probed live at several values).
#[cfg(target_os = "windows")]
const DOCK_HOVER_ALPHA: u8 = 150;

/// v2.19.0 (tear-off UX, Windows): is the primary mouse button PHYSICALLY
/// held right now? Distinguishes an Esc-CANCELLED modal move loop (button
/// still down at WM_EXITSIZEMOVE) from a real drop (button up) — winit's
/// synthesized release looks identical for both. Honors a swapped-button
/// mouse (`SM_SWAPBUTTON`: GetAsyncKeyState reports PHYSICAL buttons, so
/// the primary button of a left-handed mouse is VK_RBUTTON). Always false
/// off-Windows: only the Windows modal loop produces a release-while-held.
fn primary_button_physically_held() -> bool {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON,
        };
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
        let vk = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 {
            VK_RBUTTON
        } else {
            VK_LBUTTON
        };
        (unsafe { GetAsyncKeyState(i32::from(vk.0)) } as u16) & 0x8000 != 0
    }
    #[cfg(not(target_os = "windows"))]
    false
}

/// v2.19.0 (re-dock): HWND of a winit window for the z-order walk.
/// `None` off-Windows (the `Win32` raw-handle variant exists on every
/// platform, so this compiles everywhere without cfg noise).
fn window_hwnd(w: &winit::window::Window) -> Option<isize> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match w.window_handle().ok().map(|h| h.as_raw()) {
        Some(RawWindowHandle::Win32(h)) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// v2.19.0 (re-dock, Windows): resolve overlapping dock candidates — and
/// foreign windows covering a band — against the REAL z-order. Walks down
/// from the torn window (topmost while dragged); the first visible,
/// uncloaked window containing the cursor decides: one of ours → that's
/// the target; a foreign window → it covers the band, no dock. The cloak
/// check matters: suspended UWP apps park full-screen DWM-cloaked windows
/// in the z-order that would otherwise always read as "covered".
///
/// Known limitation (cycle-943, accepted): always-on-top foreign windows
/// sit ABOVE the torn window and are invisible to a downward walk — a
/// band covered by one still shows the marker (the merge itself is
/// unaffected). GetWindowRect also includes the invisible Win10/11 resize
/// borders, which mirrors real input routing (those borders DO capture
/// clicks), so near-edge "covered" verdicts match what a click would do.
#[cfg(target_os = "windows")]
fn zorder_pick(
    torn_hwnd: isize,
    cursor: (f64, f64),
    candidates: &[(isize, u64, usize)],
) -> Option<(u64, usize)> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows::Win32::UI::WindowsAndMessaging::{
        GW_HWNDNEXT, GetWindow, GetWindowRect, IsWindowVisible,
    };
    let (px, py) = (cursor.0 as i32, cursor.1 as i32);
    let mut hwnd = unsafe { GetWindow(HWND(torn_hwnd as *mut core::ffi::c_void), GW_HWNDNEXT) };
    // Bounded walk: the desktop can hold hundreds of top-level windows but
    // a runaway loop must be impossible.
    for _ in 0..1024 {
        let Ok(h) = hwnd else { break };
        if unsafe { IsWindowVisible(h) }.as_bool() {
            let mut cloaked: u32 = 0;
            let _ = unsafe {
                DwmGetWindowAttribute(
                    h,
                    DWMWA_CLOAKED,
                    (&raw mut cloaked).cast(),
                    std::mem::size_of::<u32>() as u32,
                )
            };
            let mut r = RECT::default();
            if cloaked == 0
                && unsafe { GetWindowRect(h, &mut r) }.is_ok()
                && px >= r.left
                && px < r.right
                && py >= r.top
                && py < r.bottom
            {
                let found = h.0 as isize;
                return candidates
                    .iter()
                    .find(|(ch, _, _)| *ch == found)
                    .map(|&(_, s, i)| (s, i));
            }
        }
        hwnd = unsafe { GetWindow(h, GW_HWNDNEXT) };
    }
    None
}

/// C7: the live monitor rects, for clamping a saved window geometry whose
/// monitor is gone (see `session::clamp_geometry_to_monitors`).
fn monitor_rects(event_loop: &ActiveEventLoop) -> Vec<(i32, i32, u32, u32)> {
    event_loop
        .available_monitors()
        .map(|m| {
            let p = m.position();
            let s = m.size();
            (p.x, p.y, s.width, s.height)
        })
        .collect()
}

impl App {
    /// C4: window attributes shared by window 1 (`resumed_inner`) and
    /// windows 2..N (`open_window`), so a second window honors borderless /
    /// always-on-top / hide-from-taskbar / geometry-hinting / WM_CLASS
    /// exactly like the first. Always returns `visible(false)` while renderer
    /// init runs; callers reveal visible states once the surface is configured.
    fn window_attributes(
        &self,
        state: kettle_config::WindowState,
    ) -> winit::window::WindowAttributes {
        let mut attrs = Window::default_attributes()
            .with_title("kettle")
            // Cycle 752: show kettle's icon in the title bar / taskbar / Alt-Tab
            // for the running window (winit leaves it unset by default).
            .with_window_icon(load_window_icon());
        // Cycle 332 (Terminator parity, terminatorlib/config.py:75 +
        // 78). `borderless` removes OS chrome; `always-on-top` keeps
        // the window above other windows. Best-effort per OS; failure
        // modes degrade silently (e.g. Wayland respects compositor
        // rules over our hint).
        if self.cfg.borderless {
            attrs = attrs.with_decorations(false);
        }
        if self.cfg.always_on_top {
            attrs = attrs.with_window_level(winit::window::WindowLevel::AlwaysOnTop);
        }
        // Cycle 691 (Terminator parity, terminatorlib/config.py:79
        // `hide_from_taskbar`): on Windows, winit 0.30 exposes
        // `WindowAttributesExtWindows::with_skip_taskbar`. Other
        // platforms remain Bucket E — X11/Wayland/macOS need
        // raw-window-handle direct atom writes which the design
        // doc tagged as a follow-up. A user copying a Terminator
        // config that sets `hide_from_taskbar = true` gets the
        // intended behavior on Windows; on other platforms the
        // value parses without effect (no warning since the key
        // is recognized).
        #[cfg(target_os = "windows")]
        if self.cfg.hide_from_taskbar {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = WindowAttributesExtWindows::with_skip_taskbar(attrs, true);
        }
        // Cycle 754: on non-Windows the key parses but there's no winit API to
        // honor it (X11 would need a `_NET_WM_STATE_SKIP_TASKBAR` atom write).
        // Log so a user porting a Terminator config knows it's recognized but
        // not yet applied here — mirrors the `sticky` log on macOS below
        // (silent no-ops are the worst UX: the user can't tell parse-failed
        // from not-implemented).
        #[cfg(not(target_os = "windows"))]
        if self.cfg.hide_from_taskbar {
            log::info!(
                "kettle: `hide_from_taskbar = true` is not yet applied on this \
                 platform (winit 0.30 exposes the API on Windows only); the \
                 window will still appear in the taskbar/dock"
            );
        }
        // Cycle 344 (Terminator parity, terminatorlib/config.py:75
        // `window_state`). Apply initial window state at creation.
        match state {
            kettle_config::WindowState::Normal => {}
            kettle_config::WindowState::Maximise => {
                attrs = attrs.with_maximized(true);
            }
            kettle_config::WindowState::Fullscreen => {
                attrs = attrs.with_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
            }
            kettle_config::WindowState::Hidden => {
                attrs = attrs.with_visible(false);
            }
        }
        // Cycle 359 (Terminator parity, terminatorlib/config.py:74
        // `geometry_hinting`): when true, request that the WM resize
        // the window in font-cell increments (so a drag-resize lands
        // on exact column/row boundaries vs sub-cell sliver). Uses
        // an approximate cell size — actual font metrics aren't
        // available yet at attrs-build time, so we use 8x16 px as a
        // typical Mono baseline. Honored best-effort per OS (X11
        // honors via WM_SIZE_HINTS; Wayland varies; macOS doesn't).
        if self.cfg.geometry_hinting {
            attrs = attrs.with_resize_increments(winit::dpi::LogicalSize::new(8.0_f64, 16.0_f64));
        }
        // Set WM_CLASS / Wayland app_id explicitly so GNOME / KDE
        // task switchers, dock pins, and the `StartupWMClass=kettle`
        // line in `packaging/linux/kettle.desktop` all line up. Without
        // this the X11 WM_CLASS defaults to the cargo target name
        // (still "kettle" for normal builds, but "kettle-bin" or
        // similar for forks / renamed binaries), and Wayland windows
        // show up as generic "Unknown" in the activities overview.
        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
        let attrs = {
            // Both `WindowAttributesExtWayland::with_name` and
            // `WindowAttributesExtX11::with_name` write to the same
            // `platform_specific.name` field — calling either suffices.
            // We pick X11 here and fully-qualify the call so the import
            // doesn't put both methods in scope (which would make
            // `attrs.with_name(…)` ambiguous).
            use winit::platform::x11::WindowAttributesExtX11;
            // Cycle 755: derive WM_CLASS from the running binary's stem
            // (default "kettle") instead of hardcoding, so a fork or renamed
            // binary groups correctly in GNOME/KDE task switchers without
            // editing code. The canonical build is `kettle`, so this matches
            // `StartupWMClass=kettle` in packaging/linux/kettle.desktop exactly.
            let wm_class = std::env::current_exe()
                .ok()
                .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "kettle".to_string());
            WindowAttributesExtX11::with_name(attrs, wm_class.clone(), wm_class)
        };
        attrs.with_visible(false)
    }

    /// C4: post-creation window setup shared by both window-creation paths.
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    fn apply_post_create(&self, window: &Window) {
        // Cycle 694 (Terminator parity, terminatorlib/config.py:81
        // `sticky`): show window on every workspace. macOS exposes
        // this as a Window-level method via `WindowExtMacOS`, so
        // we apply it after construction (unlike Windows
        // `with_skip_taskbar` which is a build-time attribute).
        // X11/Wayland remain Bucket E — winit 0.30 doesn't expose
        // `_NET_WM_STATE_STICKY` on the cross-platform API and
        // would need raw-window-handle direct atom writes (heavy
        // dep for one config key).
        //
        // Cycle 768: macOS `sticky` is now implemented for real. winit 0.30
        // dropped `WindowExtMacOS::set_visible_on_all_workspaces` (cycle 730
        // stubbed it as a log to stop breaking the macOS build), so we reach
        // through the raw NSWindow handle and set
        // `NSWindowCollectionBehavior::CanJoinAllSpaces | Stationary` via
        // objc2 — the same thing the dropped winit method did internally.
        #[cfg(target_os = "macos")]
        if self.cfg.sticky {
            set_visible_on_all_spaces(window);
        }
        // Cycle 754: X11/Wayland sticky needs a `_NET_WM_STATE_STICKY` atom
        // write that winit 0.30 doesn't expose; log (don't silently no-op) so
        // the user knows the key is recognized but not yet applied here.
        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
        if self.cfg.sticky {
            log::info!(
                "kettle: `sticky = true` is not yet applied on X11/Wayland \
                 (winit 0.30 exposes no API for `_NET_WM_STATE_STICKY`); the \
                 window stays on its current workspace"
            );
        }
    }

    /// C4: open another OS window in this process — the multi-window core.
    /// Returns the new window's seq on success; returns the `WindowOpen`
    /// back on failure so an `AdoptTab` caller can re-attach the tab to its
    /// source window instead of losing live PTYs. `size` overrides the
    /// platform-default inner size (v2.19.0 tear-off: the torn window
    /// inherits the source window's dimensions, Windows Terminal parity).
    fn open_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        open: WindowOpen,
        pos: Option<winit::dpi::PhysicalPosition<i32>>,
        size: Option<winit::dpi::PhysicalSize<u32>>,
    ) -> Result<u64, WindowOpen> {
        let Some(gpu) = self.gpu.clone() else {
            log::warn!("open_window: GPU context not ready (window 1 still initializing)");
            return Err(open);
        };
        // A tear-off window opens Normal (it must be visible at the drop
        // point); a fresh window honors the configured window-state except
        // Hidden (an invisible NewWindow helps nobody).
        let state = match (&open, self.cfg.window_state) {
            (WindowOpen::AdoptTab(_), _) | (WindowOpen::Restore(_), _) => {
                kettle_config::WindowState::Normal
            }
            (_, kettle_config::WindowState::Hidden) => kettle_config::WindowState::Normal,
            (_, s) => s,
        };
        let mut attrs = self.window_attributes(state);
        // C7: a restored window lands at its saved geometry, clamped to the
        // live monitor layout (its monitor may be unplugged).
        if let WindowOpen::Restore(sw) = &open
            && let Some(g) = sw.geometry
        {
            let g = crate::session::clamp_geometry_to_monitors(g, &monitor_rects(event_loop));
            attrs = attrs
                .with_position(winit::dpi::PhysicalPosition::new(g.x, g.y))
                .with_inner_size(winit::dpi::PhysicalSize::new(g.w, g.h));
        }
        if let Some(p) = pos {
            attrs = attrs.with_position(p);
        }
        if let Some(s) = size {
            attrs = attrs.with_inner_size(s);
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("open_window: window creation failed: {e}");
                return Err(open);
            }
        };
        self.apply_post_create(&window);
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        // Synchronous renderer init against the shared device — no block_on,
        // no watchdog (those are window-1 costs; see Renderer::new_with_gpu).
        let renderer = match Renderer::new_with_gpu(
            &gpu,
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            scale,
            &self.cfg,
        ) {
            Ok(r) => r,
            Err(e) => {
                log::error!("open_window: renderer init failed: {e}");
                return Err(open);
            }
        };
        let seq = self.next_window_seq;
        self.next_window_seq += 1;
        // Same Mux construction flags as run_with — process-global decisions.
        let mut mux = Mux::new();
        mux.lua_output_subscribed = self.lua_engine.is_some();
        #[cfg(feature = "dev-record")]
        {
            mux.lua_output_subscribed |= self.recorder.is_some();
            mux.record_lossless = self.recorder.is_some();
        }
        let mut ws = WindowState::new(seq, false, mux);
        ws.window_shown = !should_reveal_after_renderer_init(state);
        ws.renderer = Some(renderer);
        ws.window = Some(window);
        if should_reveal_after_renderer_init(state)
            && let Some(w) = &ws.window
        {
            w.set_visible(true);
            ws.window_shown = true;
        }
        let area = self.area(&ws);
        let (cols, rows) = self.grid_of(&ws, area);
        let (cw, ch) = self.cell_px(&ws);
        match open {
            WindowOpen::Fresh { cwd } => {
                if let Err(e) = ws.mux.new_tab_with(
                    &self.cfg,
                    cols,
                    rows,
                    cw,
                    ch,
                    self.waker(),
                    &[],
                    cwd.as_deref(),
                ) {
                    // `ws` (and its OS window) drop here; nothing was adopted.
                    log::error!("open_window: shell spawn failed: {e}");
                    return Err(WindowOpen::Fresh { cwd });
                }
            }
            WindowOpen::AdoptTab(dt) => {
                ws.mux.attach_tab(dt, None);
            }
            WindowOpen::Restore(sw) => {
                let sess = crate::session::Session {
                    tabs: sw.tabs.clone(),
                    active: sw.active,
                    theme: None,
                    windows: Vec::new(),
                };
                // v2.20.0 (review fix): route through the LATCHED waker
                // constructor — this hand-rolled closure bypassed the P4
                // wakeup-dedup latch, so restored panes re-created the
                // one-wakeup-per-read flood the latch eliminates.
                let mk = || self.waker();
                if !ws.mux.restore(&sess, &self.cfg, cw, ch, &mk) {
                    // `ws` (and its OS window) drop here; nothing restored.
                    log::error!("open_window: saved-window restore failed");
                    return Err(WindowOpen::Restore(sw));
                }
            }
        }
        self.resize_all(&mut ws);
        self.focused_seq = seq;
        // First frame painted directly — RedrawRequested is not delivered to
        // a never-shown window on Windows (the cycle-785 reveal dance).
        self.redraw(&mut ws);
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
        self.windows.insert(seq, ws);
        Ok(seq)
    }

    /// v2.19.0 (tear-off UX, T1+T2+T3): Chromium-style tear. The moment the
    /// dragged tab crosses the band threshold, detach it into a live window
    /// under the cursor — inheriting the source dimensions, positioned so
    /// the pointer keeps holding the tab — and hand the drag to the OS via
    /// `drag_window()` (Windows: ReleaseCapture + WM_NCLBUTTONDOWN/HTCAPTION
    /// posted to the torn window, the exact Chromium handoff; X11:
    /// _NET_WM_MOVERESIZE; macOS: performWindowDragWithEvent). Returns true
    /// when a tear happened — the source window no longer owns the gesture.
    fn maybe_tear_off(&mut self, ws: &mut WindowState, event_loop: &ActiveEventLoop) -> bool {
        if !self.cfg.detachable_tabs {
            return false;
        }
        if !matches!(
            ws.detach_drag,
            crate::detach::DragState::DraggingInside { .. }
                | crate::detach::DragState::DraggingOutside { .. }
        ) {
            return false;
        }
        let Some(src) = ws.window.clone() else {
            return false;
        };
        // T5: Wayland can neither position the torn window nor (reliably)
        // start a compositor move for a surface that never saw the press —
        // tearing mid-drag would drop the window at a compositor-chosen
        // spot while the user is still dragging. Wayland keeps the v2.18.0
        // tear-at-release path (see the Released arm).
        if window_is_wayland(&src) {
            return false;
        }
        let (cx, cy) = (ws.cursor.x as f32, ws.cursor.y as f32);
        let Some(band) = self.dock_band(ws) else {
            return false;
        };
        // 1.5× the bar thickness of slop in EVERY direction away from the
        // band (uniform hysteresis): drag along the strip = reorder, drag
        // past the slop = tear. ~36px at the default font.
        let threshold = 1.5 * self.tab_bar_h(ws);
        if !tear_threshold_crossed(cx, cy, band, threshold) {
            return false;
        }
        // Lone tab: the tab IS the window — drag the whole window instead
        // of tearing (Chromium semantics; tearing would just re-create the
        // same window). The torn-drag tracking still attaches, so the
        // full re-dock path applies: this is how a previously torn-off
        // window merges back into a sibling.
        if ws.mux.tabs.len() <= 1 {
            ws.tab_drag_active = false;
            ws.detach_drag = crate::detach::DragState::default();
            ws.drag_press = None;
            // Frame-relative grab = where the pointer is right now
            // (screen cursor − frame origin), so the dock hit-test's
            // cursor approximation tracks the real pointer.
            let grab = match (src.inner_position(), src.outer_position()) {
                (Ok(ip), Ok(op)) => (
                    f64::from(ip.x - op.x) + ws.cursor.x,
                    f64::from(ip.y - op.y) + ws.cursor.y,
                ),
                _ => (40.0, 12.0),
            };
            // Sound on macOS too: this window IS receiving the drag, so
            // NSApp.currentEvent is its own mouseDragged (unlike the torn-
            // window handoff below, where the event belongs to the source).
            let native = match src.drag_window() {
                Ok(()) => true,
                Err(e) => {
                    log::info!(
                        "tab-drag window move: native drag unavailable ({e}); manual follow"
                    );
                    false
                }
            };
            self.torn_drag = Some(TornDrag {
                seq: ws.seq,
                carrier: ws.seq,
                started: std::time::Instant::now(),
                grab,
                dock: None,
                native,
                saw_move: false,
                last_signal: std::time::Instant::now(),
                hwnd: window_hwnd(&src),
            });
            return true;
        }
        // The torn window inherits the source dimensions (WT tear-off
        // parity). A maximized/fullscreen source approximates its restored
        // size at 60% — winit exposes no restore bounds (Chromium uses
        // them; this is the closest available).
        let isz = src.inner_size();
        let scale = if src.is_maximized() || src.fullscreen().is_some() {
            0.6
        } else {
            1.0
        };
        let size = winit::dpi::PhysicalSize::new(
            ((f64::from(isz.width) * scale) as u32).max(400),
            ((f64::from(isz.height) * scale) as u32).max(300),
        );
        // Grab offset: where the pointer holds the torn window, relative to
        // its FRAME top-left (winit positions windows AND reports `Moved`
        // by the frame origin — verified against the vendored 0.30.13
        // WM_WINDOWPOSCHANGED handler). Two parts: the frame→client
        // decoration delta (caption + border, measured on the source —
        // same decoration config — so the pointer lands INSIDE the client,
        // on the strip, not on the native title bar), plus the in-strip
        // hold point in the TORN window's own layout: the strip sits at
        // the torn window's `tab-bar-pos` edge, so a Bottom/Right bar
        // anchors the pointer at the bottom/right of the torn frame —
        // without this the window hangs off the wrong side of the pointer.
        // Along the strip the pointer keeps its offset INTO the dragged
        // segment (WT's stashed drag offset, microsoft/terminal PR #14935),
        // clamped inside the torn frame.
        let ftc = match (src.inner_position(), src.outer_position()) {
            (Ok(ip), Ok(op)) => (f64::from(ip.x - op.x), f64::from(ip.y - op.y)),
            _ => (0.0, 0.0),
        };
        let bar = self.tab_bar(ws);
        let bar_h = f64::from(self.tab_bar_h(ws)).max(8.0);
        let seg = bar.segments.iter().find(|s| s.active).map(|s| s.rect);
        let in_seg_x = seg
            .map(|(sx, _, seg_w, _)| f64::from((cx - sx).clamp(16.0, (seg_w - 16.0).max(16.0))))
            .unwrap_or(40.0)
            .min(f64::from(size.width) - 24.0);
        let in_seg_y = seg
            .map(|(_, sy, _, seg_h)| f64::from((cy - sy).clamp(8.0, (seg_h - 8.0).max(8.0))))
            .unwrap_or(12.0)
            .min(f64::from(size.height) - 24.0);
        let strip_w = f64::from(self.cfg.tab_bar_width).max(8.0);
        let grab = match self.cfg.tab_bar_pos {
            TabBarPos::Top => (ftc.0 + in_seg_x, ftc.1 + bar_h * 0.5),
            TabBarPos::Bottom => (
                ftc.0 + in_seg_x,
                ftc.1 + f64::from(size.height) - bar_h * 0.5,
            ),
            TabBarPos::Left => (ftc.0 + strip_w * 0.5, ftc.1 + in_seg_y),
            TabBarPos::Right => (
                ftc.0 + f64::from(size.width) - strip_w * 0.5,
                ftc.1 + in_seg_y,
            ),
        };
        // Frame origin = screen cursor − grab. `inner_position` is the
        // client origin in screen coords, so client cursor + it = screen
        // cursor exactly; `outer_position` is the (rare) fallback.
        let pos = src
            .inner_position()
            .or_else(|_| src.outer_position())
            .ok()
            .map(|p| {
                winit::dpi::PhysicalPosition::new(
                    (f64::from(p.x) + ws.cursor.x - grab.0) as i32,
                    (f64::from(p.y) + ws.cursor.y - grab.1) as i32,
                )
            });
        // The dragged tab is the ACTIVE tab — the cycle-249 reorder keeps
        // it active while the FSM's armed index can go stale across
        // reorders (same invariant the old at-release tear relied on).
        let closing_idx = ws.mux.active;
        let Some(dt) = ws.mux.detach_tab(closing_idx) else {
            return false;
        };
        // The gesture leaves this window no matter how open_window goes.
        ws.tab_drag_active = false;
        ws.detach_drag = crate::detach::DragState::default();
        ws.drag_press = None;
        match self.open_window(event_loop, WindowOpen::AdoptTab(dt), pos, Some(size)) {
            Ok(torn_seq) => {
                self.fire_tab_close_event(closing_idx);
                self.ctl_broadcast(
                    "tab_moved",
                    None,
                    serde_json::json!({
                        "from_window": ws.seq,
                        "to_window": torn_seq,
                        "tab": closing_idx,
                    }),
                );
                self.resize_all(ws);
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
                // Re-anchor to the LIVE cursor: the pointer kept moving
                // during window creation, and the Windows modal move loop
                // anchors at the cursor's CURRENT position — without this
                // the grab offset drifts by however far the pointer slid
                // (see `cursor_screen_pos`).
                #[cfg(target_os = "windows")]
                if let Some((lx, ly)) = cursor_screen_pos()
                    && let Some(tw) = self.windows.get(&torn_seq).and_then(|t| t.window.as_ref())
                {
                    tw.set_outer_position(winit::dpi::PhysicalPosition::new(
                        (lx - grab.0) as i32,
                        (ly - grab.1) as i32,
                    ));
                }
                // T3: the native handoff. On Err, manual-follow: the source
                // still holds mouse capture, so its CursorMoved stream
                // repositions the torn window until release. macOS goes
                // straight to manual-follow: performWindowDragWithEvent on
                // the TORN window would consume NSApp.currentEvent — a
                // mouseDragged belonging to the SOURCE window — which is
                // unsound for a window that never saw the press (cycle-943
                // review); the lone-tab branch above keeps the native path
                // there since it drags the window that owns the event.
                let native = if cfg!(target_os = "macos") {
                    false
                } else {
                    self.windows
                        .get(&torn_seq)
                        .and_then(|t| t.window.as_ref())
                        .map(|w| match w.drag_window() {
                            Ok(()) => true,
                            Err(e) => {
                                log::info!(
                                    "tear-off: native drag unavailable ({e}); manual follow"
                                );
                                false
                            }
                        })
                        .unwrap_or(false)
                };
                self.torn_drag = Some(TornDrag {
                    seq: torn_seq,
                    carrier: ws.seq,
                    started: std::time::Instant::now(),
                    grab,
                    dock: None,
                    native,
                    saw_move: false,
                    last_signal: std::time::Instant::now(),
                    hwnd: self
                        .windows
                        .get(&torn_seq)
                        .and_then(|t| t.window.as_deref())
                        .and_then(window_hwnd),
                });
            }
            Err(WindowOpen::AdoptTab(dt)) => {
                log::warn!("tear-off: open_window failed; tab kept in source window");
                ws.mux.attach_tab(dt, Some(closing_idx));
            }
            Err(_) => unreachable!("open_window returns the WindowOpen it was given"),
        }
        true
    }

    /// v2.19.0 (re-dock): the client-px rect where a torn window can dock —
    /// the tab band, INCLUDING the would-be band of a hidden single-tab
    /// `auto` bar (the dock preview materializes it on hover). `None` when
    /// `tab-bar = off` (no strip exists to dock onto). Doubles as the tear
    /// threshold's band on the source window, where the bar is always
    /// visible (a tear needs ≥ 2 tabs).
    fn dock_band(&self, ws: &WindowState) -> Option<(f32, f32, f32, f32)> {
        if matches!(self.cfg.tab_bar, TabBarMode::Off) {
            return None;
        }
        let (w, h) = ws
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        let (sw, sh) = (w as f32, h as f32);
        let bh = ws.renderer.as_ref().map(|r| r.cell_h + 8.0).unwrap_or(24.0);
        Some(match self.cfg.tab_bar_pos {
            TabBarPos::Top => (0.0, 0.0, sw, bh),
            TabBarPos::Bottom => (0.0, sh - bh, sw, bh),
            TabBarPos::Left => (0.0, 0.0, self.cfg.tab_bar_width, sh),
            TabBarPos::Right => (sw - self.cfg.tab_bar_width, 0.0, self.cfg.tab_bar_width, sh),
        })
    }

    /// v2.19.0 (re-dock): insertion slot if the (approximated) screen
    /// cursor is over this window's tab band; `None` when it isn't (or
    /// the window can't host a dock right now).
    fn dock_index_at(&self, w: &WindowState, cursor: (f64, f64)) -> Option<usize> {
        let win = w.window.as_ref()?;
        if win.is_minimized().unwrap_or(false) || !win.is_visible().unwrap_or(true) {
            return None;
        }
        let ip = win.inner_position().ok()?;
        let (cx, cy) = (
            (cursor.0 - f64::from(ip.x)) as f32,
            (cursor.1 - f64::from(ip.y)) as f32,
        );
        let band = self.dock_band(w)?;
        if dist_to_rect(cx, cy, band) > 0.0 {
            return None;
        }
        let bar = self.tab_bar(w);
        let vertical = self.cfg.tab_bar_pos.is_vertical();
        if bar.height > 0.0 && !bar.segments.is_empty() {
            let mids: Vec<f32> = bar
                .segments
                .iter()
                .map(|s| {
                    let (x, y, sw_, sh_) = s.rect;
                    if vertical {
                        y + sh_ * 0.5
                    } else {
                        x + sw_ * 0.5
                    }
                })
                .collect();
            Some(dock_insertion_index(&mids, if vertical { cy } else { cx }))
        } else {
            // Hidden bar (single-tab auto, preview not applied yet): use
            // the geometry the bar WILL have once the preview materializes
            // it, so the slot can't flip between the first hit-test and
            // the next one (cycle-943): vertically the lone segment
            // occupies the strip's first bar_h, horizontally it spans the
            // button-trimmed strip — NOT the whole band.
            let bh = w.renderer.as_ref().map(|r| r.cell_h + 8.0).unwrap_or(24.0);
            let (mid, c) = if vertical {
                (bh * 0.5, cy)
            } else {
                let (bx, _, bw, _) = band;
                let arrow_w = if new_tab_dropdown_visible() { bh } else { 0.0 };
                (bx + tab_segment_strip_width(bw, bh, arrow_w) * 0.5, cx)
            };
            Some(if c < mid { 0 } else { 1 })
        }
    }

    /// v2.19.0 (re-dock): which window's tab band is under the screen
    /// cursor, and at which insertion slot. `extra` is the checked-out
    /// source window during manual-follow — it isn't in the map but is a
    /// legal (and common: drag out, change your mind, drop back) target.
    /// On Windows the candidates are resolved against the real z-order;
    /// elsewhere newest-window-first is the best approximation available
    /// (winit exposes no z-order).
    fn dock_hit_test(
        &self,
        cursor: (f64, f64),
        torn_seq: u64,
        torn_hwnd: Option<isize>,
        extra: Option<&WindowState>,
    ) -> Option<(u64, usize)> {
        let mut hits: Vec<(Option<isize>, u64, usize)> = Vec::new();
        for (seq, w) in extra
            .map(|w| (w.seq, w))
            .into_iter()
            .chain(self.windows.iter().rev().map(|(s, w)| (*s, w)))
        {
            if seq == torn_seq {
                continue;
            }
            if let Some(idx) = self.dock_index_at(w, cursor) {
                let hwnd = w.window.as_deref().and_then(window_hwnd);
                hits.push((hwnd, seq, idx));
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(th) = torn_hwnd {
            let cands: Vec<(isize, u64, usize)> = hits
                .iter()
                .filter_map(|&(h, s, i)| h.map(|h| (h, s, i)))
                .collect();
            return zorder_pick(th, cursor, &cands);
        }
        #[cfg(not(target_os = "windows"))]
        let _ = torn_hwnd;
        hits.first().map(|&(_, s, i)| (s, i))
    }

    /// v2.19.0 (re-dock): re-run the hit-test for the live cursor and move
    /// the insertion preview (and the latched dock target) accordingly.
    /// `extra` = the checked-out source window in manual-follow, so the
    /// preview can land on it even though it's out of the map.
    fn update_dock_target(
        &mut self,
        cursor: (f64, f64),
        torn_hwnd: Option<isize>,
        mut extra: Option<&mut WindowState>,
    ) {
        let Some(td) = self.torn_drag.as_ref() else {
            return;
        };
        let torn_seq = td.seq;
        let prev = td.dock;
        let hit = self.dock_hit_test(cursor, torn_seq, torn_hwnd, extra.as_deref());
        if prev == hit {
            return;
        }
        if let Some((ps, _)) = prev
            && hit.map(|(s, _)| s) != Some(ps)
        {
            match extra.as_deref_mut() {
                Some(x) if x.seq == ps => self.apply_dock_preview_ws(x, None),
                _ => self.apply_dock_preview(ps, None),
            }
        }
        if let Some((ns, idx)) = hit {
            match extra {
                Some(x) if x.seq == ns => self.apply_dock_preview_ws(x, Some(idx)),
                _ => self.apply_dock_preview(ns, Some(idx)),
            }
        }
        // Dock-hover translucency (Windows): the full-size torn window is
        // covering the very strip the insertion marker draws in — go
        // see-through while a target is latched so the user can watch the
        // marker under the pointer; restore on unlatch.
        #[cfg(target_os = "windows")]
        if let Some(th) = self.torn_drag.as_ref().and_then(|t| t.hwnd) {
            match (prev.is_some(), hit.is_some()) {
                (false, true) => set_window_alpha(th, DOCK_HOVER_ALPHA),
                (true, false) => set_window_alpha(th, 255),
                _ => {}
            }
        }
        if let Some(td) = self.torn_drag.as_mut() {
            td.dock = hit;
        }
    }

    /// v2.19.0 (re-dock): does the latched dock target still hold for the
    /// torn window's FINAL resting position? A real drop leaves the frame
    /// tracking the pointer, so frame + grab lies on the latched band; a
    /// WM-cancelled move (X11 Esc) snapped the frame back to its origin,
    /// where frame + grab points nowhere near the band — the commit then
    /// becomes an abandon. Bias-safe on X11: the latch was set from the
    /// same frame+grab approximation, so a constant anchor slide cancels
    /// out. `ws` = the torn window (checked out of the map).
    fn revalidate_dock_latch(&self, ws: &WindowState) -> bool {
        let Some(td) = self.torn_drag.as_ref() else {
            return false;
        };
        let Some((latched, _)) = td.dock else {
            // Nothing latched — commit and abandon are equivalent.
            return false;
        };
        let Some(op) = ws.window.as_ref().and_then(|w| w.outer_position().ok()) else {
            return true; // can't tell — trust the latch
        };
        let cursor = (f64::from(op.x) + td.grab.0, f64::from(op.y) + td.grab.1);
        self.dock_hit_test(cursor, td.seq, td.hwnd, None)
            .is_some_and(|(seq, _)| seq == latched)
    }

    /// v2.19.0 (re-dock): set/clear a mapped window's dock preview (the
    /// checked-out variant below does the actual work).
    fn apply_dock_preview(&mut self, seq: u64, idx: Option<usize>) {
        if let Some(mut w) = self.windows.remove(&seq) {
            self.apply_dock_preview_ws(&mut w, idx);
            self.windows.insert(seq, w);
        }
    }

    /// v2.19.0 (re-dock): set/clear a window's dock preview. The preview
    /// that MATERIALIZES a hidden single-tab auto bar is render-only — the
    /// strip overlays the top of the content for the hover's duration and
    /// the PTY grids are NOT resized (cycle-943: hovering across the band
    /// edge would otherwise SIGWINCH-spam the target's shells with
    /// resize/restore pairs; the real resize happens once, in
    /// `dock_tab_into`, when a drop actually merges).
    fn apply_dock_preview_ws(&mut self, w: &mut WindowState, idx: Option<usize>) {
        if w.dock_preview == idx {
            return;
        }
        w.dock_preview = idx;
        if let Some(win) = &w.window {
            win.request_redraw();
        }
    }

    /// v2.19.0 (re-dock): drop torn-drag tracking without merging (stale
    /// tracking, cancel, or focus loss) — clears any latched insertion
    /// preview. `ws` = the checked-out window, in case the preview is on it.
    fn abandon_torn_drag(&mut self, ws: Option<&mut WindowState>) {
        if let Some(td) = self.torn_drag.take()
            && let Some((seq, _)) = td.dock
        {
            #[cfg(target_os = "windows")]
            if let Some(th) = td.hwnd {
                set_window_alpha(th, 255);
            }
            match ws {
                Some(x) if x.seq == seq => self.apply_dock_preview_ws(x, None),
                _ => self.apply_dock_preview(seq, None),
            }
        }
    }

    /// v2.19.0 (re-dock, D4): the drop. Commit the latched dock target —
    /// move the torn window's tab into the target at the insertion slot and
    /// close the emptied torn window — or just clear tracking + preview.
    /// `ws` is whatever window's dispatch observed the drop: the torn
    /// window itself for the native OS loop (winit synthesizes its
    /// left-release when the Windows modal move loop exits), or the
    /// capture-holding source for manual-follow.
    fn finalize_torn_drag(&mut self, ws: &mut WindowState, commit: bool) {
        let committable = self
            .torn_drag
            .as_ref()
            .is_some_and(|td| commit && td.saw_move && td.dock.is_some());
        if !committable {
            self.abandon_torn_drag(Some(ws));
            return;
        }
        let td = self.torn_drag.take().expect("checked committable above");
        let (target_seq, idx) = td.dock.expect("checked committable above");
        // The torn window is about to close (merge) — restoring opacity is
        // moot then, but the defensive multi-tab branch below keeps it
        // alive, and a no-op restore on a dying HWND is harmless.
        #[cfg(target_os = "windows")]
        if let Some(th) = td.hwnd {
            set_window_alpha(th, 255);
        }
        if td.seq == ws.seq {
            // Native loop: `ws` IS the torn window; the target is mapped.
            let donor_active = ws.mux.active;
            let Some(dt) = ws.mux.detach_tab(donor_active) else {
                // Cycle-943: every early return after take() must clear the
                // latched preview, or the marker (and a materialized auto
                // bar) sticks to the target forever.
                self.clear_dock_preview_at(ws, target_seq);
                return;
            };
            let Some(mut target) = self.windows.remove(&target_seq) else {
                // Target vanished between latch and drop — keep the tab.
                ws.mux.attach_tab(dt, Some(donor_active));
                return;
            };
            self.dock_tab_into(&mut target, dt, idx, td.seq);
            self.windows.insert(target_seq, target);
            self.focused_seq = target_seq;
            if ws.mux.tabs.is_empty() {
                // The emptied torn window closes through the normal funnel
                // (finish_window_dispatch drops it; other windows remain,
                // so no event-loop exit). Session-save pairing: every other
                // window-close path saves right before flagging the close.
                self.save_session(ws);
                self.pending_window_close = true;
            }
        } else {
            // Manual-follow: `ws` is the SOURCE (capture holder); the torn
            // window is mapped. The target may be `ws` itself or another
            // mapped window (the hit-test excludes the torn window).
            let Some(mut torn) = self.windows.remove(&td.seq) else {
                // Torn window died mid-drag (reap) — the latch outlived it.
                self.clear_dock_preview_at(ws, target_seq);
                return;
            };
            let donor_active = torn.mux.active;
            let Some(dt) = torn.mux.detach_tab(donor_active) else {
                self.windows.insert(td.seq, torn);
                self.clear_dock_preview_at(ws, target_seq);
                return;
            };
            if target_seq == ws.seq {
                self.dock_tab_into(ws, dt, idx, td.seq);
            } else if let Some(mut target) = self.windows.remove(&target_seq) {
                self.dock_tab_into(&mut target, dt, idx, td.seq);
                self.windows.insert(target_seq, target);
            } else {
                torn.mux.attach_tab(dt, Some(donor_active));
                self.windows.insert(td.seq, torn);
                return;
            }
            self.focused_seq = target_seq;
            if torn.mux.tabs.is_empty() {
                // Dropping outside finish_window_dispatch is safe here: the
                // map keeps the target (and source), so the "exit only when
                // the map is empty" invariant can't trip. Save first so the
                // session reflects the post-merge window set (the close-path
                // pairing every other close keeps).
                self.save_session(ws);
                drop(torn);
            } else {
                self.windows.insert(td.seq, torn);
            }
        }
    }

    /// v2.19.0 (re-dock, cycle-943): clear a latched dock preview wherever
    /// its window lives — the checked-out `ws` or the map.
    fn clear_dock_preview_at(&mut self, ws: &mut WindowState, target_seq: u64) {
        if ws.seq == target_seq {
            self.apply_dock_preview_ws(ws, None);
        } else {
            self.apply_dock_preview(target_seq, None);
        }
    }

    /// v2.19.0 (re-dock): receive a docked tab — attach at the insertion
    /// slot (attach_tab makes it active + seen), clear the preview, resize,
    /// take focus, notify agents.
    fn dock_tab_into(
        &mut self,
        target: &mut WindowState,
        dt: crate::mux::DetachedTab,
        idx: usize,
        from_seq: u64,
    ) {
        let landed = target.mux.attach_tab(dt, Some(idx));
        target.dock_preview = None;
        self.resize_all(target);
        if let Some(w) = &target.window {
            w.focus_window();
            w.request_redraw();
        }
        self.ctl_broadcast(
            "tab_moved",
            None,
            serde_json::json!({
                "from_window": from_seq,
                "to_window": target.seq,
                "tab": landed,
            }),
        );
    }

    /// B (Peacock): resolve + claim this window's accent — walk the theme's
    /// pool from the cwd-seed slot, skipping hues live windows already use:
    /// in-process siblings (authoritative) plus other kettle processes via
    /// the presence registry (best-effort; see kettle-ctl/src/presence.rs).
    fn assign_window_accent(&self, ws: &mut WindowState) {
        let pool = kettle_config::peacock_pool(&self.cfg.theme);
        if pool.is_empty() {
            return;
        }
        let mut in_use: Vec<kettle_config::Rgb> = self
            .windows
            .values()
            .filter(|w| w.seq != ws.seq)
            .filter_map(|w| w.accent.as_ref().map(|a| a.color))
            .collect();
        let dir = kettle_ctl::presence::presence_dir();
        let me = std::process::id();
        for e in kettle_ctl::presence::live_entries(&dir) {
            // Skip only THIS window's own (re-)claim. Own-process siblings
            // are deliberately counted from presence too: during a window
            // open the OPENER is checked out of `self.windows` (the
            // take-out dispatch), so the map alone misses its claim —
            // exactly the bug the first 3-window live test caught (windows
            // 1+2 both mauve). Double-counting an in-map sibling is
            // harmless (`in_use` is a contains-set).
            if e.pid == me && e.win == ws.seq {
                continue;
            }
            if let Some(c) = kettle_config::Rgb::parse(&e.rgb) {
                in_use.push(c);
            }
        }
        let slot = pick_accent_slot(&pool, self.cfg.accent_seed, &in_use);
        let color = pool[slot];
        let presence = kettle_ctl::presence::claim(
            &dir,
            kettle_ctl::presence::PresenceEntry {
                v: 1,
                pid: me,
                win: ws.seq,
                rgb: rgb_hex(color),
                auto: true,
                started_unix: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            },
        );
        if let Some(r) = ws.renderer.as_mut() {
            r.set_accent_override(Some(color));
        }
        ws.accent = Some(crate::window_state::WindowAccent {
            color,
            slot,
            theme_name: self.cfg.theme_name.clone(),
            presence,
        });
    }

    /// B (Peacock): keep this window's accent in sync, called once per frame
    /// from `redraw` — cheap steady state (an Option check + string compare).
    /// Covers: the first frame (claim), a theme switch (re-resolve the SAME
    /// pool slot against the new theme, so every window shifts consistently),
    /// and reconfiguration to a pinned hex / `accent-color = theme` (drops
    /// the claim and clears the renderer override).
    fn sync_window_accent(&self, ws: &mut WindowState) {
        if self.cfg.accent_color.is_some() || !self.cfg.accent_auto {
            if ws.accent.take().is_some()
                && let Some(r) = ws.renderer.as_mut()
            {
                r.set_accent_override(None);
            }
            return;
        }
        match &mut ws.accent {
            Some(acc) if acc.theme_name == self.cfg.theme_name => {}
            Some(acc) => {
                let pool = kettle_config::peacock_pool(&self.cfg.theme);
                let color = pool[acc.slot % pool.len()];
                acc.theme_name = self.cfg.theme_name.clone();
                if color != acc.color {
                    acc.color = color;
                    if let Some(g) = acc.presence.as_mut() {
                        g.set_rgb(&rgb_hex(color));
                    }
                }
                if let Some(r) = ws.renderer.as_mut() {
                    r.set_accent_override(Some(color));
                }
            }
            None => self.assign_window_accent(ws),
        }
    }

    /// C4: does any pane in this window have PTY output newer than the
    /// window's last paint? (`Terminal::output_generation` vs the snapshot
    /// `redraw` records.) Plain output emits no TermEvent, so this counter
    /// is the only reliable per-window "new output" signal for the fan-out
    /// wakeup.
    fn window_has_new_output(&self, ws: &WindowState) -> bool {
        ws.mux.panes.iter().any(|(id, p)| {
            ws.seen_output_gen.get(id).copied().unwrap_or(0) != p.term.output_generation()
        })
    }

    fn resumed_inner(&mut self, ws: &mut WindowState, event_loop: &ActiveEventLoop) {
        if ws.window.is_some() {
            return;
        }
        // Create the window hidden while renderer init runs on the event-loop
        // thread. Reveal once the wgpu surface is configured, then paint
        // immediately below; `window_state = hidden` remains hidden.
        ws.window_shown = !should_reveal_after_renderer_init(self.cfg.window_state);
        let attrs = self.window_attributes(self.cfg.window_state);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
        self.apply_post_create(&window);
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        // Cycle 812 (audit #10): guard the synchronous GPU init against a hung
        // graphics driver. `Renderer::new` block_on's wgpu's adapter+device
        // requests on this (event-loop) thread; a wedged driver or GPU reset
        // can make those never return, leaving kettle stuck on an invisible
        // window (cycle 785 keeps it hidden until the first paint) with no
        // diagnostic — indistinguishable from a crash. A watchdog thread (which
        // only ever touches an `AtomicBool`, so there's no Send/thread-affinity
        // hazard with the GPU objects) turns that infinite hang into a clean,
        // actionable exit. `done` is set the instant init returns — success OR
        // a quick failure — so only a true never-returns hang trips it.
        let gpu_init_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let gpu_init_done = gpu_init_done.clone();
            // If the watchdog thread can't be spawned we simply proceed without
            // it (back to the pre-cycle-812 behavior) rather than failing init.
            let _ = std::thread::Builder::new()
                .name("kettle-gpu-init-watchdog".into())
                .spawn(move || {
                    if gpu_init_timed_out(
                        &gpu_init_done,
                        std::time::Duration::from_secs(GPU_INIT_TIMEOUT_SECS),
                        std::time::Duration::from_millis(100),
                    ) {
                        log::error!(
                            "GPU initialization did not finish within {GPU_INIT_TIMEOUT_SECS}s — \
                             your graphics driver may be hung or unresponsive. Try updating the \
                             GPU driver; on Linux you can force software rendering with \
                             `LIBGL_ALWAYS_SOFTWARE=1` or pick another backend via \
                             `WGPU_BACKEND=gl`. Exiting so kettle doesn't hang invisibly."
                        );
                        std::process::exit(1);
                    }
                });
        }
        let init_result = pollster::block_on(Renderer::new(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            scale,
            &self.cfg,
        ));
        // Disarm the watchdog the moment init returns, on BOTH the success and
        // the quick-failure path, so a clean renderer-init error below never
        // gets a spurious "timed out" report behind it.
        gpu_init_done.store(true, std::sync::atomic::Ordering::Release);
        let renderer = match init_result {
            Ok(r) => r,
            Err(e) => {
                log::error!("renderer init failed: {e}");
                event_loop.exit();
                return;
            }
        };
        // C4: cache the shared GPU context for open_window (windows 2..N).
        self.gpu = Some(renderer.gpu().clone());
        ws.renderer = Some(renderer);
        ws.window = Some(window);
        if let Some(theme) = ws.window.as_ref().and_then(|w| w.theme()) {
            self.apply_initial_os_theme_preference(theme);
        }
        if should_reveal_after_renderer_init(self.cfg.window_state)
            && let Some(w) = &ws.window
        {
            w.set_visible(true);
            ws.window_shown = true;
        }

        let area = self.area(ws);
        let (cols, rows) = self.grid_of(ws, area);
        let (cw, ch) = self.cell_px(ws);

        // CLI `-e cmd` / `-d dir` are consumed ONCE (they seed this window's
        // first tab and must not respawn on restore); take exactly those two
        // fields. The REST of `self.startup` stays intact for the whole
        // session — the explicit-restore gates below (`--tab-handoff` /
        // `--layout` / `--restore`), the save-session layout/restore gating,
        // and reload_config's launch-override re-application (cycle 938) all
        // read it later.
        //
        // C7 regression fix: this used to be a wholesale
        // `mem::take(&mut self.startup)`, which silently DEFAULTED all of
        // those later reads — `--layout` / `--restore` / `--tab-handoff`
        // never loaded at startup (verified live: `--layout` with a 2-tab
        // file opened 1 tab) and live reloads dropped the `-m/-f/-b/-H/-T`
        // overrides. Pinned by `startup_is_not_taken_wholesale` below.
        let cmd_override = self.startup.command.take();
        let cwd_override = self.startup.cwd.take();
        #[cfg(feature = "dev-record")]
        let dev_record = self
            .startup
            .record
            .clone()
            .map(|p| (p, self.startup.record_raw_input));
        // Cycle 928 (agent-first A2): the `--agent-server` override for the
        // control-server start further down (after the first paint).
        let startup_agent_server = self.startup.agent_server;
        let has_override = cmd_override.is_some() || cwd_override.is_some();
        let restored = if has_override {
            let argv = cmd_override.unwrap_or_default();
            let cwd = cwd_override
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned());
            if let Err(e) = ws.mux.new_tab_with(
                &self.cfg,
                cols,
                rows,
                cw,
                ch,
                self.waker(),
                &argv,
                cwd.as_deref(),
            ) {
                log::error!("failed to spawn `-e` command: {e}");
                event_loop.exit();
                return;
            }
            true
        } else {
            // Cycle 291: load the named layout if `--layout NAME` was
            // passed; otherwise fall through to the default session
            // (which is the per-install last-state file).
            //
            // Cycle 404 (Terminator parity, detachable-tabs Bucket-D
            // sub-cycle 8): --tab-handoff PATH wins over both. Used
            // by Action::MoveTabToNewWindow (cycle 384) when spawning
            // the target kettle process; passes the source tab's
            // serialized state via a one-shot JSON file. The handoff
            // file is deleted after read.
            // Cycle 409 (Terminator parity, detachable-tabs Bucket-D
            // sub-cycle 7 target): --tab-handoff-fd FD wins over
            // --tab-handoff PATH. Receives a serialized tab + PTY
            // fds via SCM_RIGHTS over the inherited socket fd. Unix-
            // only; Windows + Wayland use the file-fallback path.
            //
            // Today's commit ships the recv + deserialize half;
            // adopting the received fds as live Pane PTYs needs a
            // Terminal::from_fd constructor — that's the remaining
            // sub-cycle 7 piece. For now: the recv runs + the JSON
            // restores via the existing path; PTY fds get closed on
            // drop. Future sub-cycle replaces the existing PTY-spawn
            // with adoption.
            let loaded = if let Some(fd) = self.startup.tab_handoff_fd {
                #[cfg(unix)]
                {
                    use std::os::unix::io::FromRawFd;
                    // SAFETY: the fd was passed via --tab-handoff-fd
                    // by the source kettle process; the parent
                    // surrendered ownership via fork+exec. Owned
                    // here for the duration of recv_fds.
                    let socket = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
                    let mut payload = vec![0u8; 1024 * 1024];
                    match crate::fd_transport::recv_fds(&socket, &mut payload, 64) {
                        Ok((n, received_fds)) => {
                            log::info!(
                                "tab-handoff-fd: received {n} bytes + {} fds",
                                received_fds.len()
                            );
                            // Future cycle adopts received_fds as
                            // Pane PTYs; for now they leak on drop
                            // (the source process holds the
                            // canonical reference + will close them
                            // when its source tab closes).
                            for fd in received_fds {
                                // SAFETY: each fd is owned; close
                                // via libc::close. Drop-on-leak
                                // would also work but is less explicit.
                                unsafe {
                                    libc::close(fd);
                                }
                            }
                            payload.truncate(n);
                            serde_json::from_slice::<crate::session::Session>(&payload).ok()
                        }
                        Err(e) => {
                            log::warn!("tab-handoff-fd recv_fds: {e}");
                            None
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = fd;
                    log::warn!("--tab-handoff-fd unsupported on non-Unix");
                    None
                }
            } else if let Some(path) = self.startup.tab_handoff.as_deref() {
                crate::session::Session::load_tab_handoff(path)
            } else if let Some(name) = self.startup.layout.as_deref() {
                // `--layout NAME` is an explicit named-workspace restore.
                crate::session::Session::load_layout(name)
            } else if should_restore_session(self.startup.restore, self.cfg.restore_session) {
                // Cycle 918: the default last-session restore is OPT-IN now —
                // fresh windows by default, matching every mainstream terminal
                // (GNOME Terminal, Windows Terminal, kitty, Alacritty, WezTerm,
                // iTerm2). Enable "continue where I left off" with `--restore`
                // (one-shot) or `restore-session = true` (config). The session is
                // still SAVED on exit, so there is state to restore when opted in.
                crate::session::Session::load()
            } else {
                None
            };
            match loaded {
                Some(s) if !s.is_empty() => {
                    // Cycle 918: the theme is NO LONGER applied from the session.
                    // It is config-governed (the config `theme =` line, with the
                    // compile-time default as fallback), persisted via
                    // `persist_pref`. Applying a session-stored theme here used to
                    // OVERRIDE the config/default on every restore, so a user with
                    // any prior session kept the old theme even after the default
                    // changed (the exact "theme didn't update to Catppuccin" bug).
                    // `s.theme` is ignored (kept on the struct only for back-compat
                    // parsing of older session.json files).
                    // v2.20.0 (review fix): the latched waker constructor —
                    // the old hand-rolled closure bypassed the P4 dedup
                    // latch for every session-restored pane.
                    let mk = || self.waker();
                    // C7 (multi-window): window 1 of the session restores into
                    // THIS window; each additional saved window opens via
                    // open_window(Restore) — possible here because the GPU
                    // context was cached just above. A legacy single-window
                    // file normalizes to one entry.
                    let wins = s.windows_normalized();
                    if let Some((first, rest)) = wins.split_first() {
                        let first_session = crate::session::Session {
                            tabs: first.tabs.clone(),
                            active: first.active,
                            theme: None,
                            windows: Vec::new(),
                        };
                        let ok = ws.mux.restore(&first_session, &self.cfg, cw, ch, &mk);
                        if ok {
                            // Window 1's saved geometry, clamped to the live
                            // monitor layout (the saved monitor may be gone).
                            if let (Some(g), Some(win)) = (first.geometry, ws.window.as_ref()) {
                                let g = crate::session::clamp_geometry_to_monitors(
                                    g,
                                    &monitor_rects(event_loop),
                                );
                                win.set_outer_position(winit::dpi::PhysicalPosition::new(g.x, g.y));
                                let _ =
                                    win.request_inner_size(winit::dpi::PhysicalSize::new(g.w, g.h));
                            }
                            for sw in rest {
                                if self
                                    .open_window(
                                        event_loop,
                                        WindowOpen::Restore(sw.clone()),
                                        None,
                                        None,
                                    )
                                    .is_err()
                                {
                                    log::warn!(
                                        "session restore: could not open an additional window"
                                    );
                                }
                            }
                            // Focus comes home to window 1 (open_window moves
                            // focused_seq to each window it opens).
                            self.focused_seq = ws.seq;
                        }
                        ok
                    } else {
                        false
                    }
                }
                _ => false,
            }
        };
        if !restored && let Err(e) = ws.mux.new_tab(&self.cfg, cols, rows, cw, ch, self.waker()) {
            log::error!("failed to spawn shell: {e}");
            event_loop.exit();
            return;
        }
        self.resize_all(ws);
        // Cycle 928 (agent-first A2): start the control server now — right after
        // the first pane exists, BEFORE the first GPU paint (which can take
        // several seconds on a cold shader cache). The server only needs the
        // pid + a live proxy for its waker, so binding it here makes the agent
        // surface available as soon as the window comes up, not after the first
        // frame. `--agent-server` (captured before `startup` was consumed)
        // overrides the `agent-server` config; gated on `is_none()` so a
        // re-resume doesn't double-bind.
        if self.ctl.is_none() {
            let mode = startup_agent_server.unwrap_or(self.cfg.agent_server);
            if mode.is_enabled() {
                let proxy = self.proxy.clone();
                let wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
                    let _ = proxy.send_event(UserEvent::Ctl);
                });
                let started_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                self.ctl = crate::ctl_server::CtlServer::start(
                    mode,
                    std::process::id(),
                    env!("CARGO_PKG_VERSION"),
                    started_unix,
                    wake,
                );
            }
        }
        // Cycle 325 Lua scripting: drain any `kettle.send_text(s)`
        // bytes the startup script queued, into the now-existing
        // focused pane's PTY. The pane is fresh; the shell will
        // see this as the user's first typing.
        if !self.pending_lua_send.is_empty()
            && let Some(p) = ws.mux.focused()
        {
            let bytes = std::mem::take(&mut self.pending_lua_send);
            // Cycle 941: Lua send_text acts as the user — read-only drops it.
            p.feed_input(&bytes);
        }
        // Cycle 326 Lua scripting: drain any `kettle.exec_action(name)`
        // dispatches the startup script queued. Done after the
        // send_text drain so scripts that mix both produce a
        // deterministic order. Actions go through the existing
        // dispatch helper so they hit every cycle-specific hook
        // (focus tracking, palette/menu closing, blink reset).
        if !self.pending_lua_actions.is_empty() {
            let actions = std::mem::take(&mut self.pending_lua_actions);
            for a in actions {
                self.handle_action(ws, a, event_loop);
            }
        }
        // Cycle 366 (Terminator plugin parity, sub-cycle 3): fire
        // LuaEvent::Startup the first time we have an alive window
        // + at least one pane. Subsequent resumed() calls (Wayland
        // can re-emit) get short-circuited by lua_startup_fired.
        // Drains any LuaCommand the callbacks queued so a
        // `kettle.on('startup', function() kettle.send_text(...) end)`
        // takes effect immediately.
        if !self.lua_startup_fired && self.lua_engine.is_some() && ws.mux.focused().is_some() {
            if let Some(eng) = &self.lua_engine {
                eng.fire_event(&crate::LuaEvent::Startup);
            }
            // Cycle 428: route through the same helper as TabAdd /
            // TabClose / Bell / Output so all 5 event hooks share
            // one canonical command-drain path. Inherent methods are
            // callable from a trait impl as long as `self: &mut App`.
            self.drain_lua_hook_commands("startup hook");
            self.lua_startup_fired = true;
        }
        // Paint the first frame *directly* here — not only via
        // `request_redraw` — so visible startup windows receive terminal
        // content immediately after renderer + pane setup. The follow-up
        // `request_redraw` schedules the next normal frame.
        // Cycle 875: start the developer session recorder now that the grid
        // exists (only a `dev-record` build with `--record` / `KETTLE_RECORD`;
        // opts captured above before `startup` was consumed).
        // Cycle 908 (dev-record completeness): start it BEFORE the first
        // `redraw()` below — `redraw()` runs the first `drain_events()`, which
        // tees PTY output into the trace; starting the recorder *after* it
        // dropped the session's opening output (e.g. a fast `-e cmd`'s line).
        #[cfg(feature = "dev-record")]
        if let Some((path, raw)) = dev_record {
            let (cols, rows) = self.grid_of(ws, self.area(ws));
            match crate::dev_record::Recorder::start(&path, cols as u16, rows as u16, raw) {
                Ok(rec) => {
                    log::info!("dev-record: recording this session to {}", path.display());
                    self.recorder = Some(rec);
                }
                Err(e) => log::warn!(
                    "dev-record: could not start recorder at {}: {e}",
                    path.display()
                ),
            }
        }
        self.redraw(ws);
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
        // Cycle 794: kick off the update check on a background thread — AFTER
        // the first paint so it never blocks startup. It's opt-out
        // (`update-check`), skips the very first launch, throttles to once/24h
        // via a cache file, and no-ops in packaged builds; the cache throttle
        // also dedups a Wayland re-resume or multiple windows hitting GitHub.
        if self.cfg.update_check {
            crate::update_check::maybe_spawn_check(self.proxy.clone(), env!("CARGO_PKG_VERSION"));
        }
        // Dropdown-parity cycle: warm the shell-detection cache (wsl.exe /
        // vswhere probes, bounded ~2s each) off the UI thread so the first
        // dropdown open / Ctrl+Shift+N press doesn't pay it.
        kettle_core::term::prewarm_shell_detection();
    }

    fn user_event_inner(&mut self, ws: &mut WindowState, _el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Wakeup => {
                // C4: wakeups fan out to every window; skip windows whose
                // panes produced no output since their last paint (plain
                // text emits no TermEvent — the generation counter is the
                // only reliable per-window signal).
                if !self.window_has_new_output(ws) {
                    return;
                }
                // Cycle 290: run output triggers before the redraw —
                // a match fires window urgency so the user notices the
                // event even if they're focused on another OS window.
                // Cheap when triggers are empty (which is the default).
                self.run_triggers(ws);
                // Cycle 910 (R2): coalesce rapid PTY-output paints to ~one per
                // frame budget so a non-atomic repaint burst settles before we
                // snapshot the grid. When deferred, `about_to_wait` schedules
                // the flush at the budget deadline so the final frame still
                // paints. Input/cursor paints don't come through here, so
                // typing and the cursor stay immediate.
                let now = std::time::Instant::now();
                // PERF (key-repeat stutter fix): keystroke ECHO paints
                // immediately — request_redraw is vsync-coalesced, so this
                // can't outpace the display, and the coalescer's job
                // (letting a NON-atomic output burst settle before the
                // snapshot) doesn't apply to a few echoed cells. Routing
                // echo through the WaitUntil deadline (~16ms timer
                // granularity on Windows, frequently late) made held-key
                // repeat visibly stutter while Terminator's steady GTK
                // frame clock stayed smooth.
                if typed_recently(now, ws.last_typed, TYPING_ECHO_WINDOW) {
                    ws.coalescing_paint = false;
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                } else if should_defer_output_paint(
                    now,
                    ws.last_paint,
                    effective_output_budget(ws.flood_paints),
                ) {
                    ws.coalescing_paint = true;
                } else if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            UserEvent::ReloadConfig => self.reload_config(ws),
            UserEvent::RemoteCommand => self.drain_remote_commands(ws),
            UserEvent::Ctl => self.drain_ctl(ws),
            UserEvent::UpdateAvailable { tag, url } => {
                // Cycle 794: a newer release exists. Show the dismissable
                // bottom-bar banner, fire one desktop toast, and nudge the
                // taskbar/dock so the user notices even if kettle is unfocused.
                // The background thread already filtered out dismissed versions.
                fire_notify(
                    "kettle update available",
                    &format!("{tag} — click the banner in kettle to open the release page"),
                );
                self.update_available = Some((tag, url));
                if let Some(w) = &ws.window {
                    // Cycle 879: only raise OS attention (+ latch the tracker)
                    // when unfocused — mirroring the bell path — so
                    // `attention_active` keeps meaning "a flash is actually
                    // outstanding". FlashWindowEx is a no-op on the foreground
                    // window anyway, and latching the flag while focused would
                    // leave it set until an unrelated defocus/refocus.
                    if !ws.window_focused {
                        w.request_user_attention(Some(UserAttentionType::Informational));
                        ws.attention_active = true;
                    }
                    w.request_redraw();
                }
            }
        }
    }

    fn window_event_inner(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                // Cycle 875/908: tee any in-flight PTY output into the trace,
                // THEN flush, before exit (Drop also flushes). Without the
                // drain, bytes queued in a pane's output_rx when the user clicks
                // X are never recorded — `finish()` only flushes already-written
                // events, it does not pull the output sidechannel.
                #[cfg(feature = "dev-record")]
                {
                    self.flush_recorder_output(ws);
                    // C4: the recorder spans the whole session — finish it
                    // only when the LAST window goes (this one is checked out
                    // of the map, so empty == last).
                    if self.windows.is_empty()
                        && let Some(rec) = self.recorder.as_mut()
                    {
                        rec.finish();
                    }
                }
                self.save_session(ws);
                self.pending_window_close = true;
            }
            WindowEvent::Resized(size) => {
                // Cycle 841 (audit): minimizing a window delivers Resized(0, 0)
                // on Windows. Reconfiguring the surface + `resize_all` to a 0×0
                // area collapsed every PTY to a 1×1 grid (`grid_of`'s `.max(1)`),
                // firing a SIGWINCH storm that reflowed/redrew every TUI in every
                // pane — and the restore event then reflowed them all back. Skip
                // a degenerate size entirely: keep the last good grid so PTYs
                // hold their real dimensions; the genuine restore carries the
                // true non-zero size and reflows once.
                if size.width == 0 || size.height == 0 {
                    return;
                }
                if let Some(r) = ws.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
                self.resize_all(ws);
                // v2.20.0 (Ghostty `resize-overlay` parity): arm the
                // transient size chip. `after-first` (default) skips the
                // initial placement resize that fires at window creation.
                let show_chip = match self.cfg.resize_overlay {
                    kettle_config::ResizeOverlayMode::Never => false,
                    kettle_config::ResizeOverlayMode::Always => true,
                    // Review fix: placement events arrive as a short STORM
                    // at window birth (session restore re-positions,
                    // `window-state = maximised` applies post-create, a
                    // tear-off window materializes mid-drag) — swallow the
                    // whole birth window, not just the literal first event.
                    kettle_config::ResizeOverlayMode::AfterFirst => {
                        ws.seen_first_resize
                            && ws.spawned_at.elapsed() > std::time::Duration::from_millis(1500)
                    }
                };
                ws.seen_first_resize = true;
                if show_chip {
                    let (cols, rows) = self.grid_of(ws, self.area(ws));
                    ws.resize_overlay = Some((cols as u16, rows as u16, std::time::Instant::now()));
                }
                // Cycle 875: record the new grid size into the asciicast trace.
                #[cfg(feature = "dev-record")]
                if self.recorder.is_some() {
                    let (cols, rows) = self.grid_of(ws, self.area(ws));
                    if let Some(rec) = self.recorder.as_mut() {
                        rec.record_resize(cols as u16, rows as u16);
                    }
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Cycle 747: actually apply the new DPI scale. Previously this
                // arm only requested a redraw and dropped the factor, so text
                // stayed at the launch scale — tiny at >100% Windows scaling,
                // and never rescaling when dragged to a different-DPI monitor.
                // set_scale re-derives physical font metrics + cell size; the
                // surface itself is reconfigured by the Resized event winit
                // emits alongside this one. Re-grid so panes reflow to the new
                // cell dimensions.
                if let Some(r) = ws.renderer.as_mut() {
                    r.set_scale(scale_factor as f32);
                }
                self.resize_all(ws);
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::ThemeChanged(theme) => {
                self.apply_os_theme_preference(ws, theme);
            }
            WindowEvent::ModifiersChanged(m) => {
                ws.mods = m.state();
                // Modifier change can flip the URL hover affordance from
                // text-I-beam to pointing-hand without the mouse moving
                // (Ctrl held = "this click would open"). Re-sync the
                // cursor icon so the affordance updates the moment Ctrl
                // is pressed/released over a link.
                self.sync_cursor_icon(ws);
            }
            // Cycle 402 (Terminator parity, detachable-tabs Bucket-D
            // sub-cycle 6): winit CursorLeft/Entered events transition
            // the detach FSM. CursorLeft → DraggingOutside (caller
            // generates a fresh session_id for the future cross-process
            // IPC handshake); CursorEntered → DraggingInside (user
            // brought the cursor back; cancel the cross-window flow).
            //
            // Staging note (cycle 854 audit, updated by C5): the FSM's
            // *entry* transitions are wired by C6 of the multi-window cycle
            // (mouse-down arming on the tab bar). The working cross-window
            // move is the keyboard `Action::MoveTabToNewWindow`, now a LIVE
            // in-process detach_tab → open_window(AdoptTab) (the SCM_RIGHTS
            // process-handoff sender is retired). A no-op transition on
            // `Idle` remains harmless.
            // v2.19.0 (tear-off UX, D2): the torn window streams `Moved`
            // during the OS move loop (Windows: WM_WINDOWPOSCHANGED inside
            // the NC modal loop; X11: ConfigureNotify from the WM move —
            // both verified). Approximate the live cursor as frame origin +
            // grab offset and drive the re-dock hit-test against every
            // sibling window's tab band.
            WindowEvent::Moved(pos) => {
                let Some(td) = self.torn_drag.as_mut() else {
                    return;
                };
                if td.seq != ws.seq {
                    return;
                }
                td.saw_move = true;
                td.last_signal = std::time::Instant::now();
                let grab = td.grab;
                // Frame + grab approximates the pointer, but the move
                // loop's anchor can drift from `grab` by however far the
                // pointer slid between window creation and loop start
                // (measured ~10-35px) — enough to miss a band edge. On
                // Windows the LIVE cursor is one call away; on X11 the
                // approximation (with whatever constant slide the WM's
                // anchor baked in) is all we have — the latch and the
                // commit-time revalidation use the SAME approximation, so
                // the bias is at least self-consistent.
                let approx = (f64::from(pos.x) + grab.0, f64::from(pos.y) + grab.1);
                #[cfg(target_os = "windows")]
                let cursor = cursor_screen_pos().unwrap_or(approx);
                #[cfg(not(target_os = "windows"))]
                let cursor = approx;
                let torn_hwnd = ws.window.as_deref().and_then(window_hwnd);
                self.update_dock_target(cursor, torn_hwnd, None);
            }
            WindowEvent::CursorLeft { .. } => {
                let prev = std::mem::take(&mut ws.detach_drag);
                ws.detach_drag = prev.on_cursor_leave_window();
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorEntered { .. } => {
                let prev = std::mem::take(&mut ws.detach_drag);
                ws.detach_drag = prev.on_cursor_reenter_window();
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                ws.cursor = position;
                // v2.19.0 (tear-off UX): client pointer events do NOT reach
                // the torn window while the OS moves it (Windows: NC modal
                // loop; X11: the WM holds an active pointer grab). The first
                // CursorMoved after movement is therefore the post-drop
                // signal on platforms without Windows' synthesized release
                // (which arrives first there and wins). Guards (cycle-943
                // review): a short post-handoff blackout absorbs a stray
                // client motion racing the WM actually taking the grab, and
                // the latch is REVALIDATED against the torn window's final
                // resting position — a WM-cancelled move (Esc) snaps the
                // window back to its origin, where frame+grab no longer
                // hits the latched band, so the cancel abandons instead of
                // committing. Then fall through — this event is real input
                // for the now-free window.
                if let Some(td) = self.torn_drag.as_ref()
                    && td.seq == ws.seq
                    && td.native
                    && td.saw_move
                    && td.started.elapsed() >= std::time::Duration::from_millis(300)
                {
                    let commit = self.revalidate_dock_latch(ws);
                    self.finalize_torn_drag(ws, commit);
                    // A committed merge empties this window — it is closing;
                    // don't process the event against an empty mux.
                    if self.pending_window_close {
                        return;
                    }
                }
                // v2.19.0 (tear-off UX): self-healing demotion — the
                // drag_window() call succeeded as an API call but the WM
                // never actually took the move (no Moved has arrived) and
                // the capture holder is still streaming motion. An X11 WM
                // that ignores _NET_WM_MOVERESIZE would otherwise leave the
                // torn window frozen mid-air with no path to carry it.
                if let Some(td) = self.torn_drag.as_mut()
                    && td.native
                    && !td.saw_move
                    && td.carrier == ws.seq
                    && td.started.elapsed() >= std::time::Duration::from_millis(400)
                {
                    log::info!("tear-off: WM never took the drag; demoting to manual follow");
                    td.native = false;
                }
                // v2.19.0 (tear-off UX, T3 fallback): manual-follow — the
                // native handoff failed, so the capture holder (the tear's
                // source window, or the dragged window itself for a
                // lone-tab whole-window drag) carries the torn window:
                // reposition it from this cursor stream and drive the
                // re-dock hit-test. Gated on the CARRIER: only the capture
                // holder's stream drives the follow — without the gate,
                // stale tracking would hijack every window's cursor stream
                // and keep refreshing the failsafe forever (cycle-943).
                if let Some(td) = self.torn_drag.as_ref()
                    && !td.native
                    && td.carrier == ws.seq
                {
                    let (grab, torn_seq) = (td.grab, td.seq);
                    if let Some(ip) = ws.window.as_ref().and_then(|w| w.inner_position().ok()) {
                        let cursor = (f64::from(ip.x) + position.x, f64::from(ip.y) + position.y);
                        let torn_win = if torn_seq == ws.seq {
                            ws.window.as_ref()
                        } else {
                            self.windows.get(&torn_seq).and_then(|t| t.window.as_ref())
                        };
                        let torn_hwnd = torn_win.map(|w| w.as_ref()).and_then(window_hwnd);
                        if let Some(tw) = torn_win {
                            tw.set_outer_position(winit::dpi::PhysicalPosition::new(
                                (cursor.0 - grab.0) as i32,
                                (cursor.1 - grab.1) as i32,
                            ));
                        }
                        if let Some(td) = self.torn_drag.as_mut() {
                            td.saw_move = true;
                            td.last_signal = std::time::Instant::now();
                        }
                        // For a self-drag, `ws` IS the torn window — the
                        // hit-test already excludes it by seq, so no extra
                        // candidate is needed.
                        let extra = if torn_seq == ws.seq { None } else { Some(ws) };
                        self.update_dock_target(cursor, torn_hwnd, extra);
                    }
                    return;
                }
                // Cycle 904 (audit): dragging a split divider — recompute the
                // addressed split's ratio from the cursor and apply. The split
                // rect is re-fetched from the live seams so a mid-drag layout
                // change (a pane elsewhere closing) can't desync the math; if
                // the split has vanished, cancel the drag.
                if ws.dragging_split.is_some() {
                    let (path, dir) = {
                        let d = ws.dragging_split.as_ref().unwrap();
                        (d.path.clone(), d.dir)
                    };
                    let area = self.area(ws);
                    let seams = ws.mux.split_seams(ws.mux.active, area);
                    match seams.iter().find(|s| s.path == path).map(|s| s.rect) {
                        Some(rect) => {
                            let ratio = crate::mux::ratio_from_pos(
                                rect,
                                dir,
                                ws.cursor.x as f32,
                                ws.cursor.y as f32,
                            );
                            ws.mux.set_split_ratio(ws.mux.active, &path, ratio);
                            // Cycle 916 (file-by-file audit): the layout-tree
                            // ratio changed, but each pane's PTY grid is resized
                            // only by resize_all (the keyboard resize path reaches
                            // it via handle_action's tail). Without this the child
                            // TUIs keep their old cols/rows — rendering clipped —
                            // until some unrelated event fires a resize.
                            self.resize_all(ws);
                            if let Some(w) = &ws.window {
                                w.set_cursor(Self::resize_cursor_for(dir));
                                w.request_redraw();
                            }
                        }
                        None => ws.dragging_split = None,
                    }
                    return;
                }
                // Any real mouse movement undoes the hide-while-typing
                // state. Sub-pixel movements that winit *might* coalesce
                // are fine to ignore — the next "real" motion will fire.
                self.show_mouse_cursor(ws);
                self.sync_cursor_icon(ws);
                // Cycle 712 (Terminator menu UX, hover-to-highlight):
                // cursor over a context-menu row immediately updates
                // the highlight. Matches GTK/NSMenu/Win32 menu
                // conventions; before this cycle the highlight only
                // moved via keyboard so the menu felt unresponsive to
                // mouse users. Cheap: no-op when the menu is closed.
                if ws.context_menu.is_some() {
                    self.update_menu_highlight_from_cursor(ws);
                }
                // Cycle 360 (Terminator parity, terminatorlib/config.py:73
                // `focus = sloppy`): focus-follows-mouse. The pane
                // under the cursor becomes focused on every cursor
                // movement (vs default `click` mode where click is
                // required). `system` is treated like `click` for
                // kettle — winit doesn't expose the OS-level focus
                // policy.
                if matches!(self.cfg.focus, kettle_config::FocusMode::Sloppy)
                    && !ws.tab_drag_active
                    && !ws.selecting
                    && !ws.dragging_scrollbar
                    // Cycle 786 (audit A4): don't let focus-follows-mouse
                    // reassign pane focus while a modal is open — typing into
                    // search/palette while the cursor drifts over another pane
                    // would otherwise silently steal focus.
                    && !self.any_modal_open(ws)
                {
                    let area = self.area(ws);
                    let pre = self.focus_key(ws);
                    ws.mux
                        .focus_at(area, ws.cursor.x as f32, ws.cursor.y as f32);
                    self.note_focus_change(ws, pre);
                }
                // Cycle 249: drag-to-reorder tabs (kitty / iTerm2 /
                // Ghostty parity). When a left-button press in the tab
                // bar armed `tab_drag_active`, walk the bar geometry,
                // compute the target index under the cursor, and swap
                // the active tab toward it via `move_active_tab`
                // (cycle ~125's pure swap-with-clamp helper).
                // v2.19.0 (cycle-943): x-only — gated off vertical bars,
                // where mapping cursor.x onto a vertically stacked strip
                // produced silent bogus shuffles during any tab drag
                // (vertical drag-reorder remains the deferred sub-cycle 6).
                if ws.tab_drag_active && !self.cfg.tab_bar_pos.is_vertical() {
                    let bar = self.tab_bar(ws);
                    if bar.height > 0.0 && !bar.segments.is_empty() {
                        let (_, _, nw, _) = bar.new_tab;
                        // Cycle 821 (audit): the trailing button area is the
                        // cycle-805 `▾ +` PAIR, not just `+`. Subtract both so
                        // the drag strip matches the width `tab_bar()` actually
                        // tiles segments across (`(sw - plus_w - arrow_w)
                        // .max(plus_w)`); using `sw - plus_w` left the strip one
                        // button too wide, so the reorder target lagged the
                        // cursor near the right edge (`arrow_w` is 0 when the
                        // dropdown is absent, so this is still correct there).
                        let (_, _, aw, _) = bar.new_tab_menu;
                        let (sw, _) = ws
                            .renderer
                            .as_ref()
                            .map(|r| {
                                let (w, h) = r.surface_size();
                                (w as f32, h as f32)
                            })
                            .unwrap_or((800.0, 600.0));
                        let strip_w = tab_segment_strip_width(sw, nw, aw);
                        let target =
                            tab_drag_target_index(ws.cursor.x as f32, bar.segments.len(), strip_w);
                        let delta = target as i32 - ws.mux.active as i32;
                        if delta != 0 && ws.mux.move_active_tab(delta) {
                            ws.mux.touch_active_tab_seen();
                            if let Some(w) = &ws.window {
                                w.request_redraw();
                            }
                        }
                    }
                }
                // C6 (tear-off): drive the detach FSM from the press origin.
                // Distance decides click-vs-drag; WINDOW-BOUNDS CONTAINMENT
                // decides inside/outside — Windows' SetCapture keeps
                // streaming CursorMoved (with out-of-client coordinates)
                // while the button is held but suppresses CursorLeft, so
                // position is the reliable outside signal. The
                // CursorLeft/Entered arms below remain as supplementary
                // signals for platforms that do deliver them mid-drag.
                if let Some((ox, oy)) = ws.drag_press {
                    let (cx, cy) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    let next = std::mem::take(&mut ws.detach_drag).on_mouse_move(cx - ox, cy - oy);
                    let (sw, sh) = ws
                        .renderer
                        .as_ref()
                        .map(|r| {
                            let (w, h) = r.surface_size();
                            (w as f32, h as f32)
                        })
                        .unwrap_or((800.0, 600.0));
                    let outside = cx < 0.0 || cy < 0.0 || cx >= sw || cy >= sh;
                    ws.detach_drag = if outside {
                        next.on_cursor_leave_window()
                    } else {
                        next.on_cursor_reenter_window()
                    };
                    // v2.19.0 (tear-off UX): Chromium-style — tear the
                    // moment the cursor crosses the band threshold, not at
                    // release. The torn window appears live under the
                    // cursor and rides the OS move loop from here.
                    if self.maybe_tear_off(ws, event_loop) {
                        return;
                    }
                }
                if let Some(btn) = ws.mouse_btn {
                    // Drag while a button is held — report motion if tracked.
                    if self.send_mouse(ws, btn, true, true) {
                        return;
                    }
                }
                if ws.dragging_scrollbar {
                    let area = self.area(ws);
                    let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    self.scrollbar_at(ws, area, px, py, false);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                if ws.selecting {
                    let area = self.area(ws);
                    self.update_selection(ws, area);
                }
                if (ws.selecting || !ws.links.is_empty())
                    && let Some(w) = &ws.window
                {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                // v2.19.0 (tear-off UX): a fresh press while torn-drag
                // tracking is live. For a NATIVE drag any client press
                // means the tracking is stale (the OS modal loop / WM grab
                // swallows presses mid-drag) — abandon, never merge on a
                // click. A LEFT press during manual-follow is equally
                // impossible mid-gesture (left is held the whole drag) —
                // stale, abandon. But an OTHER-button press routed to the
                // manual-follow capture holder is live mid-drag input:
                // swallow it (Chromium swallows stray presses during a tab
                // drag) instead of killing the gesture (cycle-943 review).
                if let Some(td) = self.torn_drag.as_ref() {
                    if td.native || button == MouseButton::Left {
                        self.finalize_torn_drag(ws, false);
                    } else if td.carrier == ws.seq {
                        return;
                    }
                }
                // Cycle 810 (audit): forward Back / Forward (buttons 8 / 9) to
                // a mouse-tracking app rather than dropping them. No local UI
                // meaning, so they no-op when tracking is off.
                // Cycle 831 (audit): gate behind an open modal — this forward
                // sat ABOVE the modal check below, so a side-button press leaked
                // SGR into a tracking TUI *behind* a search/palette/settings/…
                // dialog (the exact leak cycle 786 closed for L/M/R + wheel). A
                // lone context menu isn't a modal here.
                if let Some(sgr) = extra_mouse_sgr(button) {
                    // Cycle 897 (audit): a lone context menu must swallow the
                    // side-button press too — dismiss the menu and DON'T forward.
                    // `modal_swallows_pointer` returns false for a lone menu, so
                    // pre-fix a Back/Forward click both leaked SGR to the
                    // tracking app *behind* the menu AND left the menu open
                    // (every other button dismisses it).
                    if ws.context_menu.is_some() {
                        ws.context_menu = None;
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    if !modal_swallows_pointer(self.any_modal_open(ws), ws.context_menu.is_some()) {
                        self.send_mouse(ws, sgr, true, false);
                    }
                    return;
                }
                let bcode = match button {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => return,
                };
                // Context menu (cycle 245): if the menu is open, a left-
                // click either fires the row that was hit or — if the
                // click landed outside the panel — closes the menu (the
                // GNOME / browser convention; right-click on another
                // location is handled as a re-open after this close
                // because the right-click handler runs the open path).
                if ws.context_menu.is_some()
                    && let Some(click) = self.context_menu_click_action(ws, bcode)
                {
                    // Cycle 889: route through the shared sink. It closes the
                    // menu per-leaf and keeps it open for DrillIntoSubmenu —
                    // the old inline `ws.context_menu = None` *before* the
                    // match made the drill arm dead code (submenu clicks just
                    // dismissed the menu).
                    self.dispatch_context_menu_click(ws, click, event_loop);
                    return;
                }
                if ws.context_menu.is_some() && bcode == 0 {
                    // Left-click outside the panel — dismiss without
                    // firing anything (matches every modern menu).
                    ws.context_menu = None;
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // v2.24.0: the settings overlay is mouse-driven (click-to-cycle).
                // Left-click a field cycles its value forward, right-click
                // backward; clicking a category tab switches category; a click
                // outside the panel closes settings. Handled before the generic
                // modal swallow below so the click actually does something.
                if ws.settings_nav.is_some()
                    && (bcode == 0 || bcode == 2)
                    && self.settings_mouse(ws, if bcode == 2 { -1 } else { 1 }, true)
                {
                    return;
                }
                // Cycle 786 (audit A1, critical): with any *other* modal open
                // (search / palette / ssh / settings / layout-picker / hint /
                // confirm dialog / inline title-edit / vi copy-mode) the click
                // must be consumed — otherwise it fell straight through to the
                // tab-bar / pane-focus / mouse-tracking logic below, switching
                // tabs and injecting mouse events into the terminal *behind* a
                // dialog that looked like it had focus. The context menu is
                // excluded (handled + returned above; a right-click below
                // relocates it).
                if modal_swallows_pointer(self.any_modal_open(ws), ws.context_menu.is_some()) {
                    return;
                }
                // Tab-bar interactions (left = switch / close-✕ / new-+;
                // middle = close that tab).
                let bar = self.tab_bar(ws);
                let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                // Cycle 794: the update banner is the bottom bar when shown.
                // Left-click opens the release page (+ records the dismissal so
                // it won't re-nag); right-click dismisses without opening. Only
                // reachable with no modal open (the gate above returned for
                // those) — exactly when the banner is actually on screen.
                if self.update_available.is_some() {
                    let sh = ws
                        .window
                        .as_ref()
                        .map(|w| w.inner_size().height as f32)
                        .unwrap_or(0.0);
                    let banner_h = self.cell_px(ws).1 as f32 + 10.0;
                    // Cycle 808 (audit): the banner stacks ABOVE a bottom-
                    // anchored tab / status bar (matching the renderer through
                    // the shared `update_banner_top`), so hit-test its ACTUAL
                    // rect rather than the whole bottom band — otherwise a
                    // click meant for a bottom tab / status bar got swallowed
                    // by the banner.
                    let bottom_tabbar_h = if matches!(self.cfg.tab_bar_pos, TabBarPos::Bottom) {
                        bar.height
                    } else {
                        0.0
                    };
                    let bottom_status_h =
                        if matches!(self.cfg.status_bar, kettle_config::StatusBarMode::Bottom) {
                            self.status_bar_h(ws)
                        } else {
                            0.0
                        };
                    let banner_top = kettle_render::update_banner_top(
                        sh,
                        banner_h,
                        bottom_tabbar_h,
                        bottom_status_h,
                    );
                    if sh > 0.0
                        && py >= banner_top
                        && py < banner_top + banner_h
                        && (bcode == 0 || bcode == 2)
                    {
                        // Left-click opens + dismisses; right-click only
                        // dismisses. Shared with the keyboard `OpenUpdate` /
                        // `DismissUpdate` actions (cycle 809).
                        self.act_on_update_banner(ws, bcode == 0);
                        return;
                    }
                }
                let in_bar = |r: kettle_render::Rect4, px: f32, py: f32| {
                    px >= r.0 && px < r.0 + r.2 && py >= r.1 && py < r.1 + r.3
                };
                if bar.height > 0.0
                    && py >= bar.y
                    && py < bar.y + bar.height
                    && (bcode == 0 || bcode == 1)
                {
                    if bcode == 0 && bar.new_tab_menu.2 > 0.0 && in_bar(bar.new_tab_menu, px, py) {
                        // Cycle 805: the `▾` dropdown — open the shell chooser
                        // anchored at the arrow's bottom-left. Checked BEFORE the
                        // `+` so the arrow region isn't swallowed by the button.
                        let (ax, ay, _, ah) = bar.new_tab_menu;
                        self.open_new_tab_menu(ws, ax, ay + ah);
                    } else if bcode == 0 && in_bar(bar.new_tab, px, py) {
                        let area = self.area(ws);
                        let (cols, rows) = self.grid_of(ws, area);
                        let (cw, ch) = self.cell_px(ws);
                        // Cycle 802 (audit): log a `+`-button new-tab spawn
                        // failure rather than swallowing it.
                        if let Err(e) = ws.mux.new_tab(&self.cfg, cols, rows, cw, ch, self.waker())
                        {
                            log::warn!("could not open a new tab (+ button): {e}");
                        }
                    } else if let Some(seg) = bar.segments.iter().find(|s| in_bar(s.rect, px, py)) {
                        let close = bcode == 1 || in_bar(seg.close, px, py);
                        if close {
                            // Cycle 144: closing a tab (middle-click or
                            // ✕) can shift focus to a different tab
                            // (cycle 120's `reap_tabs` bookkeeping).
                            // Treat it like any other focus-changing
                            // action so the cursor on the now-active
                            // tab lands visible immediately.
                            let pre = self.focus_key(ws);
                            // Cycle 424: fire TabClose so plugins see
                            // the ✕-click close the same as Action::CloseTab.
                            let closing_idx = seg.idx;
                            if ws.mux.close_tab_at(seg.idx) {
                                // Cycle 157: save the (empty) session
                                // before exit so next launch starts
                                // fresh rather than restoring the
                                // *previous* multi-tab state. Other
                                // exit paths (Action::CloseTab on the
                                // last tab, WindowEvent::CloseRequested)
                                // already save; this one was missed.
                                self.fire_tab_close_event(closing_idx);
                                self.save_session(ws);
                                self.pending_window_close = true;
                                return;
                            }
                            self.fire_tab_close_event(closing_idx);
                            self.note_focus_change(ws, pre);
                        } else {
                            let pre = self.focus_key(ws);
                            ws.mux.active = seg.idx;
                            ws.mux.touch_active_tab_seen();
                            self.note_focus_change(ws, pre);
                            // Cycle 249: arm the drag-to-reorder
                            // handler so a subsequent CursorMoved
                            // event with the left button still held
                            // can swap the active tab toward the
                            // cursor. Cleared in the Released arm
                            // below. Only on bare left-click (bcode 0
                            // == left, not middle / close).
                            if bcode == 0 {
                                ws.tab_drag_active = true;
                                // C6 (tear-off): arm the detach FSM alongside
                                // the in-window reorder. v2.19.0: armed for a
                                // LONE tab too — dragging it past the band
                                // threshold moves the whole window (Chromium
                                // semantics), which is how a torn-off window
                                // re-docks into a sibling.
                                if self.cfg.detachable_tabs {
                                    ws.detach_drag =
                                        crate::detach::DragState::on_mouse_down_on_tab(seg.idx);
                                    ws.drag_press = Some((px, py));
                                } else {
                                    ws.detach_drag = crate::detach::DragState::default();
                                    ws.drag_press = None;
                                }
                            }
                        }
                    }
                    self.resize_all(ws);
                    if let Some(win) = &ws.window {
                        win.request_redraw();
                    }
                    return;
                }
                let area = self.area(ws);
                // Ctrl/Cmd + left-click opens a hyperlink under the cursor.
                //
                // Cycle 350 (Terminator parity, terminatorlib/config.py:120
                // `link_single_click`): when true, single-click (no
                // modifier) is enough to open URLs. Default keeps
                // kettle's Ctrl-click guard so accidental drags don't
                // navigate.
                let url_modifier =
                    self.cfg.link_single_click || ws.mods.control_key() || ws.mods.super_key();
                if bcode == 0
                    && url_modifier
                    && let Some(uri) = self.link_at_cursor(ws).map(|l| l.uri.clone())
                {
                    // Cycle 351: route through helper so custom URL
                    // handler config is honored.
                    self.open_url(&uri);
                    return;
                }
                // Cycle 389 (Terminator parity, titlebar Bucket-D
                // sub-cycle 5): left-click on per-pane titlebar
                // focuses + opens the EditPaneTitle overlay. Two
                // clicks model (focus first, edit second) avoids
                // accidental title edits on focus transitions.
                let (cx, cy) = (ws.cursor.x as f32, ws.cursor.y as f32);
                if bcode == 0
                    && let Some(clicked_pane_id) = self.pane_at_titlebar_click(ws, cx, cy)
                {
                    let already_focused = ws.mux.active_focus() == Some(clicked_pane_id);
                    let pre = self.focus_key(ws);
                    ws.mux.focus_at(area, cx, cy);
                    self.note_focus_change(ws, pre);
                    if already_focused {
                        self.handle_action(ws, Action::EditPaneTitle, event_loop);
                    }
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Cycle 904 (audit): a left-press on a split divider seam starts
                // a drag-to-resize gesture rather than focusing a pane / starting
                // a selection. Seams are hit-tested over the same content `area`
                // the panes lay out in, with a small grab tolerance.
                if bcode == 0
                    && let Some(drag) = self.split_drag_at(ws, area, px, py)
                {
                    let dir = drag.dir;
                    ws.dragging_split = Some(drag);
                    if let Some(w) = &ws.window {
                        w.set_cursor(Self::resize_cursor_for(dir));
                        w.request_redraw();
                    }
                    return;
                }
                let pre = self.focus_key(ws);
                ws.mux
                    .focus_at(area, ws.cursor.x as f32, ws.cursor.y as f32);
                self.note_focus_change(ws, pre);
                if self.send_mouse(ws, bcode, true, false) {
                    ws.mouse_btn = Some(bcode);
                    return;
                }
                // Middle-click in the content area pastes the PRIMARY
                // selection when the platform exposes one; otherwise it falls
                // back to the regular clipboard.
                //
                // Cycle 350 (Terminator parity, terminatorlib/config.py:88
                // `disable_mouse_paste`): when true, middle-click does
                // not paste. Useful for terminal-of-last-resort use
                // cases where accidental middle-clicks shouldn't leak
                // clipboard content into commands.
                if bcode == 1 && !self.cfg.disable_mouse_paste {
                    self.paste_primary(ws);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Cycle 350 (Terminator parity, terminatorlib/config.py:89
                // `putty_paste_style`): right-click pastes (PuTTY/Windows
                // convention) instead of opening the context menu. The
                // companion `putty_paste_style_source_clipboard` decides
                // whether the source is CLIPBOARD or PRIMARY.
                if bcode == 2 && self.cfg.putty_paste_style {
                    match putty_paste_source(self.cfg.putty_paste_style_source_clipboard) {
                        PasteSource::Clipboard => self.paste_clipboard(ws),
                        PasteSource::Primary => self.paste_primary(ws),
                    }
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Click the scrollbar to jump the viewport, then drag it.
                if bcode == 0 && self.scrollbar_jump(ws, area, px, py) {
                    ws.dragging_scrollbar = true;
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Right-click handling — layered:
                //
                // 1. `Shift + right-click` *with* an existing selection
                //    keeps the cycle-49 extend-selection behavior (xterm
                //    convention; muscle memory for kettle's power users
                //    since v1.0).
                // 2. Any other right-click opens the context menu at the
                //    click point (Terminator / GNOME / iTerm2 default).
                //    Before cycle 245 this branch was a silent no-op,
                //    which left first-time users confused.
                //
                // Mouse-tracking already short-circuited above when the
                // focused program is consuming mouse events, so this only
                // fires for the kettle chrome.
                if bcode == 2 {
                    if ws.mods.shift_key() && self.extend_selection_to_cursor(ws, area) {
                        if self.cfg.copy_on_select {
                            self.copy_selection(ws);
                        }
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    self.open_context_menu(ws, px, py);
                    return;
                }
                if bcode == 0 {
                    // Shift+left-click extends an existing selection to the
                    // click point (xterm / Alacritty / iTerm2 / WezTerm
                    // parity). Alt still takes precedence for block-select
                    // so Shift+Alt remains block. If there's no selection
                    // to extend, fall through to the normal new-selection
                    // path so Shift+Click on empty space "just works."
                    if ws.mods.shift_key()
                        && !ws.mods.alt_key()
                        && self.extend_selection_to_cursor(ws, area)
                    {
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    let cell = self.cursor_cell(ws);
                    let clicks = cell.map(|(r, c)| self.click_count(ws, r, c)).unwrap_or(1);
                    let kind = selection_kind(clicks, ws.mods.alt_key());
                    // Cycle 288 smart selection (iTerm2 parity): on a
                    // double-click that lands inside a hint match
                    // (URL / path / IPv4 / git SHA), select the whole
                    // match as a Simple range instead of the alacritty
                    // Semantic word, which usually under- or over-shoots
                    // structured tokens. Falls through to begin_selection
                    // when no hint matches, preserving existing behavior.
                    let mut smart_selected = false;
                    if clicks == 2
                        && !ws.mods.alt_key()
                        && let Some((row, col)) = cell
                        && let Some((start, end)) = self
                            .line_text_for_smart_select(ws, row)
                            .as_deref()
                            .and_then(|line| smart_selection_at(line, col))
                        && self.apply_smart_selection(ws, area, row, start, end)
                    {
                        smart_selected = true;
                    }
                    if !smart_selected {
                        self.begin_selection(ws, area, kind);
                    }
                    // Word/line selections resolve on press; copy them now.
                    // Simple/Block are drags — copied on button release.
                    // Cycle 288: smart-selected count as resolved-on-press
                    // for copy_on_select purposes too — the whole match
                    // is the selection, no drag follow-up expected.
                    if self.cfg.copy_on_select
                        && (smart_selected
                            || matches!(
                                kind,
                                kettle_core::SelectionType::Semantic
                                    | kettle_core::SelectionType::Lines
                            ))
                    {
                        self.copy_selection(ws);
                    }
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button,
                ..
            } => {
                // Cycle 810 (audit): release report for the side buttons, so a
                // tracking app sees the matching button-up after the press.
                // Cycle 831 (audit): gated behind an open modal, matching Pressed.
                if let Some(sgr) = extra_mouse_sgr(button) {
                    // Cycle 897 (audit): symmetry with the Pressed path — a lone
                    // context menu swallows the side-button release too (its
                    // press was swallowed above, so no up-report should leak).
                    if ws.context_menu.is_some() {
                        return;
                    }
                    if !modal_swallows_pointer(self.any_modal_open(ws), ws.context_menu.is_some()) {
                        self.send_mouse(ws, sgr, false, false);
                    }
                    return;
                }
                let bcode = match button {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => return,
                };
                // v2.19.0 (tear-off UX, D4): a left-release while a torn
                // window is tracked is the DROP. Two shapes: (a) the
                // synthesized release winit posts to the TORN window when
                // the Windows modal move loop exits (WM_EXITSIZEMOVE →
                // WM_LBUTTONUP — verified in the vendored 0.30.13 source);
                // (b) the real release on the capture-holding SOURCE in
                // manual-follow. Commit the latched dock either way. A
                // left-release on an unrelated window while tracking is
                // live means the tracking went stale (an X11 drop we never
                // observed) — abandon it and process the release normally.
                //
                // Esc-cancel guard (cycle-943 review, HIGH): WM_EXITSIZEMOVE
                // fires for EVERY modal-loop exit — Esc-cancel included —
                // and the synthesized release is indistinguishable from a
                // drop, while the latch survives the snap-back (the live
                // cursor is still over the band). The tell is PHYSICAL
                // button state: an Esc-cancel exits the loop with the
                // primary button still held; a real drop only exits after
                // it went up. Held ⇒ abandon (the user's real release later
                // arrives with tracking already cleared and flows through
                // normal processing harmlessly).
                if bcode == 0
                    && let Some(td) = self.torn_drag.as_ref()
                {
                    if td.seq == ws.seq || !td.native {
                        let commit = !primary_button_physically_held();
                        self.finalize_torn_drag(ws, commit);
                        return;
                    }
                    self.finalize_torn_drag(ws, false);
                }
                if ws.mouse_btn == Some(bcode) {
                    ws.mouse_btn = None;
                    if self.send_mouse(ws, bcode, false, false) {
                        return;
                    }
                }
                if bcode == 0 {
                    if ws.selecting && self.cfg.copy_on_select {
                        self.copy_selection(ws);
                    }
                    ws.selecting = false;
                    ws.dragging_scrollbar = false;
                    // Cycle 904: end any split-divider drag on left-button up.
                    ws.dragging_split = None;
                    // Cycle 249: end the drag-to-reorder gesture on
                    // left-button release. Any swaps that happened
                    // during the drag are already committed; this just
                    // disarms the CursorMoved handler.
                    ws.tab_drag_active = false;
                    // C6 (tear-off), WAYLAND ONLY since v2.19.0: a release
                    // while the detach FSM is OUTSIDE the window tears the
                    // dragged tab off into a new window. Everywhere else
                    // the tear fired the moment the cursor crossed the band
                    // threshold (`maybe_tear_off`), so the FSM is already
                    // Idle by release — and a release while still inside
                    // the hysteresis slop deliberately does NOT tear
                    // (Chromium's within-slop = click/reorder semantics).
                    // Wayland can't position windows client-side nor hand
                    // off a drag to a surface that never saw the press, so
                    // the v2.18.0 at-release behavior remains its path.
                    let dropped_outside = matches!(
                        ws.detach_drag,
                        crate::detach::DragState::DraggingOutside { .. }
                    );
                    ws.detach_drag = std::mem::take(&mut ws.detach_drag).on_mouse_up();
                    ws.drag_press = None;
                    let wayland = ws.window.as_deref().is_some_and(window_is_wayland);
                    if self.cfg.detachable_tabs
                        && dropped_outside
                        && wayland
                        && ws.mux.tabs.len() > 1
                    {
                        let closing_idx = ws.mux.active;
                        if let Some(dt) = ws.mux.detach_tab(closing_idx) {
                            // `outer_position` errs on Wayland — `None` lets
                            // the compositor place the window.
                            let pos = ws
                                .window
                                .as_ref()
                                .and_then(|w| w.outer_position().ok())
                                .map(|p| {
                                    winit::dpi::PhysicalPosition::new(
                                        p.x + ws.cursor.x as i32,
                                        p.y + ws.cursor.y as i32,
                                    )
                                });
                            match self.open_window(event_loop, WindowOpen::AdoptTab(dt), pos, None)
                            {
                                Ok(torn_seq) => {
                                    self.fire_tab_close_event(closing_idx);
                                    // C8: agents see the tear-off too.
                                    self.ctl_broadcast(
                                        "tab_moved",
                                        None,
                                        serde_json::json!({
                                            "from_window": ws.seq,
                                            "to_window": torn_seq,
                                            "tab": closing_idx,
                                        }),
                                    );
                                    self.resize_all(ws);
                                    if let Some(w) = &ws.window {
                                        w.request_redraw();
                                    }
                                }
                                Err(WindowOpen::AdoptTab(dt)) => {
                                    log::warn!(
                                        "tear-off: open_window failed; tab kept in source window"
                                    );
                                    ws.mux.attach_tab(dt, Some(closing_idx));
                                }
                                Err(_) => {
                                    unreachable!("open_window returns the WindowOpen it was given")
                                }
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = wheel_lines(&delta, self.cfg.scroll_multiplier);
                if lines == 0 {
                    return;
                }
                // Cycle 714 (Terminator menu UX, C5): wheel over an
                // open context menu scrolls its rows (one row per
                // wheel notch). Pre-empts every other wheel dispatch
                // so a 512-entry Theme submenu scrolls cleanly
                // instead of leaking through to the underlying pane
                // / tab bar / font-zoom.
                if ws.context_menu.is_some() {
                    // Wheel up = lines > 0 = scroll up = decrement
                    // offset; wheel down = lines < 0 = scroll down.
                    self.scroll_context_menu(ws, -(lines as isize));
                    return;
                }
                // v2.24.0: wheel over a settings field adjusts it (up = forward,
                // down = backward). A wheel outside the panel is NOT a dismiss —
                // it just falls through to the modal swallow below.
                if ws.settings_nav.is_some()
                    && self.settings_mouse(ws, if lines > 0 { 1 } else { -1 }, false)
                {
                    return;
                }
                // Cycle 786 (audit A2): a non-context-menu modal swallows the
                // wheel too — without this, Ctrl+wheel still zoomed the font
                // and Shift/plain wheel still scrolled the pane / cycled tabs
                // behind an open search / palette / settings / etc. The context
                // menu already consumed its wheel above, so it is `None` here
                // and `modal_swallows_pointer` reduces to "any modal open".
                if modal_swallows_pointer(self.any_modal_open(ws), ws.context_menu.is_some()) {
                    return;
                }
                // Wheel over the tab bar cycles tabs (kitty / iTerm2 /
                // Ghostty parity). Each "click" of the wheel moves one
                // tab regardless of `scroll-multiplier` so the gesture
                // stays predictable — multiple lines from a fast scroll
                // collapse to a single tab change, like the real apps.
                if self.cursor_in_tab_bar(ws) && ws.mux.tabs.len() > 1 {
                    if lines > 0 {
                        ws.mux.prev_tab();
                    } else {
                        ws.mux.next_tab();
                    }
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Cycle 604 (Terminator parity): Ctrl+wheel resizes the
                // font. Fires BEFORE the mouse-tracking pass-through so
                // it works even when a TUI like tmux/htop has mouse
                // tracking on — matches gnome-terminal / Terminator /
                // xterm UX. `cfg.disable_mousewheel_zoom = true`
                // (recognized since cycle 334; previously a no-op
                // because the feature it disables didn't exist) opts
                // out for users who scroll-zoom by accident on a
                // touchpad. Step size matches the existing keyboard
                // IncreaseFontSize / DecreaseFontSize actions for a
                // single source of truth.
                if let Some(sign) = should_zoom_font(
                    ws.mods.control_key(),
                    lines,
                    self.cfg.disable_mousewheel_zoom,
                ) && let Some(r) = ws.renderer.as_mut()
                {
                    // Cycle 747: step logical size, not the now-physical
                    // cell_h (which would double-apply the DPI scale).
                    let new = if sign > 0 {
                        r.font_size() + 1.0
                    } else {
                        (r.font_size() - 1.0).max(6.0)
                    };
                    r.set_font_size(new);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Shift+wheel always scrolls the kettle scrollback even
                // when a TUI has mouse-tracking on (xterm convention).
                // Without this bypass, you can't scroll back through
                // your tmux/htop session — the TUI swallows every wheel
                // notch.
                let (track, _) = input::mouse_tracking(self.focused_mode(ws));
                let track_active = track != input::MouseTracking::Off && !ws.mods.shift_key();
                if track_active {
                    let btn = if lines > 0 { 64 } else { 65 };
                    for _ in 0..lines.abs().min(8) {
                        self.send_mouse(ws, btn, true, false);
                    }
                } else {
                    if let Some(pane) = ws.mux.focused()
                        && let Ok(mut t) = pane.term.term.lock()
                    {
                        t.scroll_display(Scroll::Delta(lines));
                    }
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::DroppedFile(path) => {
                // Cycle 897 (audit): a file dropped while a modal (search /
                // palette / settings / confirm dialog / inline title-edit / vi
                // copy-mode / …) is open must NOT inject its path into the PTY
                // behind the dialog — the same pointer-leak class cycle 786
                // closed for clicks. A lone context menu doesn't count here.
                if self.any_modal_open(ws) {
                    return;
                }
                // Standard modern-terminal affordance: dragging a file
                // onto the window inserts its (shell-quoted) path at the
                // cursor, so the user can drop a config / log / Rust
                // source file and press Enter to act on it without
                // typing the path. iTerm2 / WezTerm / kitty / Ghostty /
                // GNOME Terminal all do this. A trailing space lets
                // `cat ` + drop + Enter Just Work; without it, the
                // user would have to add a space between the previous
                // token and the path.
                //
                // Cycle 182: route through `paste_payload` so a vim /
                // neovim / fzf / mc that has bracketed paste enabled
                // sees the path wrapped in `\e[200~ … \e[201~` and
                // treats it as a paste block (no per-char command
                // interpretation). Without this, dropping a file onto
                // vim caused each char of the path to act as a normal-
                // mode command — chaotic. Clipboard paste already
                // routes through the same helper; this brings drag-
                // drop into line. Honors broadcast (cycle 173/174):
                // when group input is on, the path goes to every pane
                // in the active tab — and each pane gets the *per-
                // pane* BRACKETED_PASTE wrap (cycle 174 invariant),
                // so a broadcast set containing one shell + one vim
                // doesn't break either of them.
                let text = format!("{} ", shell_quote_path(&path));
                if ws.mux.is_broadcast_on() {
                    ws.mux.broadcast_paste(&text);
                } else {
                    // Read the focused pane's BRACKETED_PASTE state first
                    // — `focused_mode` and `mux.focused` both want &mut
                    // self, so they have to run sequentially (not nested).
                    let bracketed = self
                        .focused_mode(ws)
                        .contains(kettle_core::TermMode::BRACKETED_PASTE);
                    let bytes = input::paste_payload(&text, bracketed);
                    if let Some(p) = ws.mux.focused() {
                        // Cycle 941: drag-drop is user input — read-only drops it.
                        p.feed_input(&bytes);
                    }
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Focused(f) => {
                ws.window_focused = f;
                // Cycle 876: non-interactive UI-state marker (OS-driven focus
                // change — a transition the PTY output stream can't show).
                #[cfg(feature = "dev-record")]
                if let Some(rec) = self.recorder.as_mut() {
                    rec.record_marker(if f {
                        "kettle:focus_in"
                    } else {
                        "kettle:focus_out"
                    });
                }
                // Cycle 897 (audit): a focus loss can swallow the button-UP that
                // ends an in-progress drag (the release lands on whatever window
                // took focus), latching `selecting` / `dragging_scrollbar` /
                // `tab_drag_active` / a held `mouse_btn`. The next CursorMoved
                // then kept extending the selection or dragging a tab with no
                // button down. Disarm them on focus loss, committing any pending
                // copy-on-select first (mirrors the left-button-up path).
                if !f {
                    if ws.selecting && self.cfg.copy_on_select {
                        self.copy_selection(ws);
                    }
                    ws.selecting = false;
                    ws.dragging_scrollbar = false;
                    ws.tab_drag_active = false;
                    // Cycle 904: a focus loss also ends any split-divider drag.
                    ws.dragging_split = None;
                    ws.mouse_btn = None;
                    // C6: a focus loss also cancels an in-flight tab tear-off
                    // (the release will land on whatever window took focus).
                    ws.detach_drag = crate::detach::DragState::default();
                    ws.drag_press = None;
                    // v2.19.0 note: torn-drag tracking deliberately survives
                    // this disarm — the tear itself moves OS focus to the
                    // torn window (firing Focused(false) on the source), and
                    // manual-follow rides the source's mouse CAPTURE, which
                    // outlives focus. Stale tracking is cleaned by the
                    // press/release handlers + the about_to_wait failsafe.
                }
                // Cycle 344 (Terminator parity, terminatorlib/config.py:77
                // `hide_on_lose_focus`): Quake-style auto-hide. When
                // the user clicks away to another window, hide the
                // kettle window. Reappears via `kettle --toggle`
                // (cycle 303) or whatever global hotkey the user
                // bound. Honors only on focus-LOSS (f == false).
                if !f
                    && self.cfg.hide_on_lose_focus
                    && let Some(w) = &ws.window
                {
                    w.set_visible(false);
                }
                // Cycle 171: route through the shared helper so all
                // user-driven blink-reset paths share one implementation
                // (cycles 134-141 + 144 + 150 audit). The
                // CursorBlinkingChange handler still inlines the body
                // because it runs inside `ws.mux.panes.values_mut()`
                // and can't borrow `self` again — that one's documented.
                self.reset_blink_phase(ws);
                // Focus-event reporting (DEC private mode ?1004): apps that
                // enabled it expect CSI I on focus-in, CSI O on focus-out.
                if self
                    .focused_mode(ws)
                    .contains(kettle_core::TermMode::FOCUS_IN_OUT)
                    && let Some(p) = ws.mux.focused()
                {
                    p.term.write(if f { b"\x1b[I" } else { b"\x1b[O" });
                }
                // Cycle 869: winit's `request_user_attention(None)` alone does
                // not reliably stop the Win11 taskbar flash once started, so
                // when an attention request is outstanding, clear it directly
                // (FlashWindowEx FLASHW_STOP via Taskbar) on focus-gain and
                // reset the tracker.
                if f && ws.attention_active {
                    ws.attention_active = false;
                    if let Some(w) = &ws.window {
                        w.request_user_attention(None);
                        ws.taskbar.clear_attention(w);
                    }
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Occluded(occluded) => {
                // v2.24.0: freeze the animated background (and any proactive
                // animation wake) while the window is fully hidden behind other
                // windows — an invisible window must cost zero idle, the
                // safety refinement that makes `background-animation = always`
                // (the new default) safe. On un-occlude, repaint at once so the
                // wallpaper catches up to its true time.
                ws.window_occluded = occluded;
                if !occluded && let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                // v2.19.0 (tear-off UX): typing in the torn window right
                // after an X11 drop whose release the WM swallowed — commit
                // the latched dock first (same shape as the CursorMoved
                // post-drop heuristic, including the post-handoff blackout
                // and the final-position latch revalidation; on Windows the
                // synthesized release already cleared the tracking before
                // any key can arrive).
                if let Some(td) = self.torn_drag.as_ref()
                    && td.seq == ws.seq
                    && td.native
                    && td.saw_move
                    && td.started.elapsed() >= std::time::Duration::from_millis(300)
                {
                    let commit = self.revalidate_dock_latch(ws);
                    self.finalize_torn_drag(ws, commit);
                    // A committed merge empties this window — it is closing;
                    // don't process the key against an empty mux.
                    if self.pending_window_close {
                        return;
                    }
                }
                // Cycle 876: record the keystroke (redacted token) BEFORE any
                // modal/early-return path consumes it, so the trace captures
                // every key. Pasted content never reaches here — it's a `paste`
                // marker (see the paste sites), never raw bytes.
                #[cfg(feature = "dev-record")]
                {
                    let mods = ws.mods;
                    if let Some(rec) = self.recorder.as_mut() {
                        dev_record_key(rec, &event.logical_key, mods);
                    }
                }
                // C6 (tear-off): Esc cancels an in-flight tab drag and is
                // consumed — it must not leak to the PTY or close a modal.
                if !matches!(ws.detach_drag, crate::detach::DragState::Idle)
                    && matches!(&event.logical_key, Key::Named(NamedKey::Escape))
                {
                    let (next, _restored) = std::mem::take(&mut ws.detach_drag).cancel();
                    ws.detach_drag = next;
                    ws.drag_press = None;
                    ws.tab_drag_active = false;
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Keep the cursor solid while actively typing (cycle 144).
                // Routes through the shared helper so the eight
                // user-driven blink-reset paths (Reset / focus changes /
                // modal close / typing / tab close / window focus /
                // DEC ?12 toggle) stay in lock-step. Cycle 171.
                self.reset_blink_phase(ws);
                // Hide the OS mouse cursor (configurable; default on, like
                // every modern terminal). Re-shown on the next CursorMoved.
                self.hide_mouse_cursor(ws);
                let text = event.text.as_ref().map(|s| s.as_str());

                if ws.context_menu.is_some() {
                    self.context_menu_key(ws, &event.logical_key, text, event_loop);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // Cycle 299: vi-mode key dispatch (sub-cycle 2). When
                // vi_mode is Some, intercept keys for vi-style
                // navigation before they reach the PTY. h/j/k/l move
                // the vi cursor; 0/$/g/G jump; Esc exits.
                if ws.vi_mode.is_some() {
                    self.vi_mode_key(ws, &event.logical_key, text);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.hint_state.is_some() {
                    self.hint_key(ws, &event.logical_key, text);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.palette_input.is_some() {
                    self.palette_key(ws, &event.logical_key, text, event_loop);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // v2.24.0: while the inline path prompt is open it owns the
                // keyboard (typed text → the buffer; Enter/Esc finish it).
                if ws.settings_nav.is_some() && ws.settings_text_edit.is_some() {
                    self.settings_text_key(ws, &event.logical_key, text);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Cycle 756: settings overlay key handling (exclusive modal).
                if ws.settings_nav.is_some() {
                    self.settings_key(ws, &event.logical_key, event_loop);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.layout_picker_input.is_some() {
                    self.layout_picker_key(ws, &event.logical_key, text);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.ssh_input.is_some() {
                    self.ssh_key(ws, &event.logical_key, text);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // Cycle 660 (sub-cycle 5 of confirm-dialog design):
                // confirm-modal key handler. Tab/Shift+Tab/←→
                // cycle focus, Enter dispatches on_confirm, Esc
                // closes the modal without dispatching. Modal is
                // exclusive — non-nav keys are swallowed.
                if ws.confirm_dialog.is_some() {
                    let key = match &event.logical_key {
                        Key::Named(NamedKey::Escape) => Some(ConfirmKey::Escape),
                        Key::Named(NamedKey::Enter) => Some(ConfirmKey::Enter),
                        Key::Named(NamedKey::Tab) => {
                            if ws.mods.shift_key() {
                                Some(ConfirmKey::ShiftTab)
                            } else {
                                Some(ConfirmKey::Tab)
                            }
                        }
                        Key::Named(NamedKey::ArrowLeft) => Some(ConfirmKey::Left),
                        Key::Named(NamedKey::ArrowRight) => Some(ConfirmKey::Right),
                        // v2.20.0 (`vim-menu-nav`): y/n answer directly;
                        // h/l move button focus like ←/→. The modal swallows
                        // all other keys either way, so disabling the setting
                        // restores the old behavior exactly.
                        Key::Character(s)
                            if self.cfg.vim_menu_nav
                                && !ws.mods.control_key()
                                && !ws.mods.alt_key()
                                && !ws.mods.super_key() =>
                        {
                            // Case-folded so CapsLock can't disable y/n/h/l.
                            match s.to_ascii_lowercase().as_str() {
                                "y" => Some(ConfirmKey::Yes),
                                "n" => Some(ConfirmKey::No),
                                "h" => Some(ConfirmKey::Left),
                                "l" => Some(ConfirmKey::Right),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    if let Some(k) = key
                        && let Some(state) = ws.confirm_dialog.as_ref()
                    {
                        let n = state.buttons.len();
                        let focus = state.focus_idx;
                        let action = &state.on_confirm;
                        let result = confirm_dialog_keypress(focus, n, k);
                        match result {
                            ConfirmKeyResult::Move(idx) => {
                                if let Some(s) = ws.confirm_dialog.as_mut() {
                                    s.focus_idx = idx;
                                }
                            }
                            ConfirmKeyResult::Confirm => {
                                // Inspect on_confirm BEFORE clearing
                                // so the dispatch sees the right action.
                                let to_run = action.clone();
                                ws.confirm_dialog = None;
                                self.dispatch_confirm_action(ws, to_run, event_loop);
                            }
                            ConfirmKeyResult::Cancel => {
                                ws.confirm_dialog = None;
                            }
                            ConfirmKeyResult::Ignore => {}
                        }
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                    }
                    return;
                }
                // Cycle 369: Edit-title overlay key handler. Esc
                // cancels; Enter applies via apply_title_edit;
                // Backspace removes one char; printable text appends.
                if ws.editing_title.is_some() {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            ws.editing_title = None;
                        }
                        Key::Named(NamedKey::Enter) => {
                            self.apply_title_edit(ws);
                        }
                        Key::Named(NamedKey::Backspace) => {
                            if let Some(state) = ws.editing_title.as_mut() {
                                state.input.pop();
                            }
                        }
                        _ => {
                            if let Some(s) = text
                                && let Some(state) = ws.editing_title.as_mut()
                            {
                                for c in s.chars() {
                                    if !c.is_control() {
                                        state.input.push(c);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.mux.search.open {
                    self.search_key(ws, &event.logical_key, text);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if let Some(k) = to_kkey(&event.logical_key) {
                    let trig = Trigger::new(to_mods(ws.mods), k);
                    if let Some(act) = self.cfg.keybinds.get(&trig).cloned() {
                        self.handle_action(ws, act, event_loop);
                        return;
                    }
                }

                let mode = ws
                    .mux
                    .focused()
                    .and_then(|p| p.term.term.lock().ok().map(|t| *t.mode()))
                    .unwrap_or(kettle_core::TermMode::empty());
                // Cycle 828 (audit): application-keypad mode (DECKPAM) — an
                // unmodified numpad key emits its SS3 sequence when the focused
                // app set it. Tried before the normal encoder, which is
                // location-agnostic. `event.location` distinguishes the numpad
                // from the main keyboard row.
                let encoded =
                    input::encode_app_keypad(&event.logical_key, event.location, ws.mods, mode)
                        .or_else(|| input::encode(&event.logical_key, text, ws.mods, mode));
                if let Some(mut bytes) = encoded {
                    // Cycle 352 (Terminator parity, terminatorlib/config.py:107-108
                    // `backspace_binding` + `delete_binding`): remap the
                    // encoded bytes when the user picked a non-default
                    // binding. Same as VTE's per-profile override.
                    // v2.20.0: shared with `send_keys` (review fix) so the
                    // agent plane honors the same remap as GUI keystrokes.
                    bytes = apply_bs_del_binding(&self.cfg, &event.logical_key, ws.mods, bytes);
                    // Any keystroke that produces PTY bytes also dismisses
                    // an active selection — alacritty/iTerm2/WezTerm all do
                    // this so typing after a select doesn't leave a stale
                    // highlight behind.
                    self.clear_selection_on_input(ws);
                    // Cycle 141: typing should land the cursor visible
                    // immediately. Without this, a fast typist hitting
                    // a key right as `blink_on` was false saw a brief
                    // flash of no-cursor before the next half-period.
                    // Alacritty / kitty / iTerm2 / WezTerm all reset
                    // the blink phase on every keystroke. Same shape
                    // as cycles 134-140 (Reset, focus changes, modal
                    // close, mouse focus); typing is the last
                    // user-driven path that still needed it.
                    self.reset_blink_phase(ws);
                    // PERF (key-repeat stutter fix): mark the keystroke so
                    // its PTY echo paints immediately (see the Wakeup arm).
                    ws.last_typed = Some(std::time::Instant::now());
                    if ws.mux.is_broadcast_on() {
                        ws.mux.broadcast_write(&bytes);
                        // `scroll-on-keystroke` (Ghostty / Alacritty
                        // default) snaps the viewport back to the
                        // bottom on every keystroke. With broadcast
                        // *off* (the next branch), only the focused
                        // pane gets typed into and only it snaps —
                        // self-consistent. With broadcast *on*, the
                        // bytes go to every pane in the active tab;
                        // the pre-cycle-173 code only wrote the bytes
                        // and skipped the snap, so a user with
                        // broadcast on AND any pane scrolled back saw
                        // a confusing mismatch: typing reached the
                        // remote shells but the local view stayed
                        // pinned to history. Snap every pane in the
                        // broadcast set, matching the non-broadcast
                        // path's behavior so the config flag is
                        // honored consistently regardless of mode.
                        if self.cfg.scroll_on_keystroke {
                            ws.mux.broadcast_scroll_to_bottom();
                        }
                    } else if let Some(p) = ws.mux.focused() {
                        // Cycle 941: a read-only pane (Terminator parity)
                        // drops the keystroke — and skips the scroll snap,
                        // since nothing was typed.
                        if p.feed_input(&bytes) {
                            // Yank back to the bottom *if* the user wants it
                            // (Ghostty/Alacritty default `scroll-on-keystroke`).
                            // Disabling lets you pin the viewport while typing —
                            // useful for ed-style line editors and code reading
                            // sessions where you're typing search terms while
                            // the screen stays put.
                            if self.cfg.scroll_on_keystroke
                                && let Ok(mut t) = p.term.term.lock()
                            {
                                t.scroll_display(Scroll::Bottom);
                            }
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => self.redraw(ws),
            _ => {}
        }
    }

    /// C4: returns the wake-up this window wants (ms; `None` = wait for
    /// events). The dispatch wrapper merges every window's request to the
    /// earliest deadline and sets the control flow once.
    fn about_to_wait_inner(
        &mut self,
        ws: &mut WindowState,
        _event_loop: &ActiveEventLoop,
    ) -> Option<u64> {
        // Cycle 908: drain trailing recorder output before reap removes a
        // just-exited pane (covers the shell-exit → process-exit close path,
        // e.g. a fast `-e cmd` session).
        #[cfg(feature = "dev-record")]
        self.flush_recorder_output(ws);
        if ws.mux.reap() && ws.window.is_some() {
            self.save_session(ws);
            self.pending_window_close = true;
            return None;
        }
        let now = std::time::Instant::now();
        // Drive cursor blink + visual-bell decay without busy-looping: only
        // schedule wake-ups while something is actually animating.
        let bell_active = ws
            .last_bell
            .map(|t| t.elapsed() < std::time::Duration::from_millis(300))
            .unwrap_or(false);
        // v2.20.0: the resize chip needs repaints until it expires (then one
        // more to erase it); clear the state once it has.
        let resize_chip_active = ws
            .resize_overlay
            .map(|(_, _, t)| t.elapsed() < RESIZE_OVERLAY_DURATION)
            .unwrap_or(false);
        if !resize_chip_active && ws.resize_overlay.is_some() {
            ws.resize_overlay = None;
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
        let blink_active = self.cfg.cursor_blink && ws.window_focused;
        let blink_interval = std::time::Duration::from_millis(self.cfg.cursor_blink_interval);
        let blink_elapsed = now.saturating_duration_since(ws.last_blink);
        let blink_due = blink_active && blink_elapsed >= blink_interval;
        let term_anim = ws
            .mux
            .panes
            .values()
            .any(|p| p.term.has_running_animation());
        // v2.23.1: an animated background (a starfield, or a GIF/APNG/WebP
        // image) wakes at its OWN frame rate, not a fixed 30 fps — at 30 fps an
        // 8 fps GIF repaints the same frame ~22×/s (wasted present()s, the ~55%
        // animated idle). `bg_anim_interval_ms` is the ms to the next frame
        // boundary; `None` unless a bg is animating (and — for `when-focused` —
        // only while focused, so an unfocused window still reaches
        // `ControlFlow::Wait` at zero idle cost, unlike Ghostty's always-on
        // shaders).
        let bg_anim_interval_raw = ws
            .renderer
            .as_ref()
            .and_then(|r| r.bg_anim_interval_ms(&self.cfg, ws.window_focused));
        // v2.24.0 freeze-when-hidden: an occluded or minimized window can't show
        // the animation, so it must cost zero idle — this is what makes the new
        // always-on default safe. Only probe `is_minimized()` (an OS call) when
        // a bg is actually animating, so the common no-wallpaper case pays
        // nothing here.
        let bg_hidden = bg_anim_interval_raw.is_some()
            && (ws.window_occluded
                || ws
                    .window
                    .as_ref()
                    .is_some_and(|w| w.is_minimized().unwrap_or(false)));
        let bg_anim_interval = if bg_hidden {
            None
        } else {
            bg_anim_interval_raw
        };
        // Edge-trigger the bg redraw: request it ONLY when the displayed frame
        // index actually changes, not every loop iteration. (Requesting it every
        // `about_to_wait` made winit redraw continuously — the high animated
        // idle. Mirrors how `blink_due` gates the cursor-blink redraw.)
        let bg_frame = if bg_hidden {
            None
        } else {
            ws.renderer
                .as_ref()
                .and_then(|r| r.bg_current_frame_index(&self.cfg, ws.window_focused))
        };
        let bg_frame_due = bg_frame.is_some() && bg_frame != ws.last_bg_frame;
        if bg_frame_due {
            ws.last_bg_frame = bg_frame;
        }
        if bg_frame.is_none() {
            ws.last_bg_frame = None;
        }
        // Selection-autoscroll runs at the same ~30 fps as bell / image
        // animation — without an active wake-up the loop sits idle waiting
        // for a fresh CursorMoved, so the drag-past-edge case would freeze
        // until the user wiggled the mouse.
        let autoscroll_active = ws.selecting && {
            let area = self.area(ws);
            self.focused_rect(ws, area)
                .map(|r| selection_autoscroll_lines(ws.cursor.y as f32, r.1, r.1 + r.3) != 0)
                .unwrap_or(false)
        };
        // Cycle 910 (R2): a deferred (coalesced) output paint becomes due
        // `OUTPUT_FRAME_BUDGET` after the last frame. Until then it stays
        // pending and we wake at its deadline so the burst paints exactly once.
        let output_budget = effective_output_budget(ws.flood_paints);
        let coalesce_due = ws.coalescing_paint
            && ws
                .last_paint
                .map(|t| now.saturating_duration_since(t) >= output_budget)
                .unwrap_or(true);
        if bell_active
            || blink_due
            || term_anim
            || bg_frame_due
            || autoscroll_active
            || coalesce_due
            || resize_chip_active
        {
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
            if coalesce_due {
                ws.coalescing_paint = false;
            }
        }
        // Pick the earliest wake we still need: ~30 fps for bell / animation /
        // autoscroll / the resize chip, the cursor-blink half-period deadline,
        // or the pending coalesced output paint's deadline.
        let mut wait_ms: Option<u64> =
            if bell_active || term_anim || autoscroll_active || resize_chip_active {
                Some(33)
            } else if let Some(bg_ms) = bg_anim_interval {
                // Animated bg: wake exactly at its next frame boundary (its own
                // fps), not 30 fps — this is the fix for the ~55% animated idle.
                Some(bg_ms.clamp(16, 1000))
            } else if blink_active {
                let remaining = blink_interval.saturating_sub(blink_elapsed);
                Some((remaining.as_millis() as u64).max(1))
            } else {
                None
            };
        if ws.coalescing_paint {
            let remaining = ws
                .last_paint
                .map(|t| output_budget.saturating_sub(now.saturating_duration_since(t)))
                .unwrap_or_default();
            let ms = (remaining.as_millis() as u64).max(1);
            wait_ms = Some(wait_ms.map_or(ms, |w| w.min(ms)));
        }
        // Cycle 929 (agent-first A2): reply `timed_out` to any pending
        // run_command whose deadline has passed, and—while runs are pending—
        // schedule a wake at the soonest deadline so a fully-silent command
        // (no output to wake us) still times out on time.
        self.check_pending_run_deadlines(ws);
        if let Some(soonest) = self.pending_runs.values().map(|p| p.deadline).min() {
            let ms = (soonest.saturating_duration_since(now).as_millis() as u64).clamp(1, 500);
            wait_ms = Some(wait_ms.map_or(ms, |w| w.min(ms)));
        }
        // v2.20.0 (review fix): bound the dev-record staleness in WALL time.
        // The recorder's interval flush is event-driven, so a burst followed
        // by silence left its buffered tail unflushed until the next event;
        // flush here when stale and, while dirty, wake at the deadline.
        #[cfg(feature = "dev-record")]
        if let Some(rec) = self.recorder.as_mut() {
            rec.flush_if_stale();
            if let Some(deadline) = rec.flush_deadline() {
                let ms = (deadline.saturating_duration_since(now).as_millis() as u64).max(1);
                wait_ms = Some(wait_ms.map_or(ms, |w| w.min(ms)));
            }
        }
        wait_ms
    }
}

/// Cycle 754 drift guard. The confirm dialog ("Close pane?", "Quit?") is a
/// modal: opening another overlay over it must clear it (`close_all_modals`),
/// and it must count as a modal for mouse/scroll/cursor gating
/// (`any_modal_open`). A full behavioral test would need a constructed `App`
/// (window + renderer); pin the invariant at the source level instead — the
/// same approach as `kettle-core`'s teardown guard.
#[cfg(test)]
mod modal_discipline_guard {
    #[test]
    fn confirm_dialog_is_tracked_as_a_modal() {
        let src = include_str!("app.rs").replace("\r\n", "\n");
        let body = |name: &str| -> String {
            let start = src
                .find(&format!("fn {name}("))
                .unwrap_or_else(|| panic!("fn {name} not found"));
            let rest = &src[start..];
            let end = rest.find("\n    }").expect("fn end");
            rest[..end].to_string()
        };
        assert!(
            body("close_all_modals").contains("ws.confirm_dialog = None"),
            "close_all_modals must clear confirm_dialog so it can't stack under \
             another overlay"
        );
        assert!(
            body("any_modal_open").contains("ws.confirm_dialog.is_some()"),
            "any_modal_open must count the confirm dialog so input doesn't fall \
             through to the terminal behind it"
        );
    }

    /// Cycle 898 drift guard (audit). The confirm-dialog dispatch must honor
    /// the close return values like the keybind paths: `close_tab()` /
    /// `close_focused()` returning `true` means the last tab / pane closed, so
    /// the window must `event_loop.exit()` immediately instead of deferring a
    /// tick and painting an empty frame. A behavioral test needs an event loop;
    /// pin the honored returns at the source level.
    #[test]
    fn confirm_close_honors_close_returns() {
        let src = include_str!("app.rs").replace("\r\n", "\n");
        let start = src
            .find("fn dispatch_confirm_action(")
            .expect("dispatch_confirm_action not found");
        let rest = &src[start..];
        let end = rest.find("\n    }").expect("fn end");
        let body = &rest[..end];
        assert!(
            body.contains("if ws.mux.close_tab() {")
                && body.contains("self.pending_window_close = true;"),
            "CloseTab dispatch must exit when close_tab() reports the last tab"
        );
        assert!(
            body.contains("let was_last = ws.mux.close_focused();")
                && body.contains("if was_last {"),
            "ClosePane dispatch must exit when close_focused() reports the last pane"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        App, ContextMenuItem, assign_mnemonics, count_rows_fitting, filter_disabled,
        find_menu_row_y, modal_swallows_pointer, rank_layouts, selection_kind,
        should_restore_session, should_reveal_after_renderer_init, typeahead_match,
    };
    use kettle_config::Action;
    use kettle_core::SelectionType;

    /// Startup drift guard: the window is hidden only while renderer init runs,
    /// then revealed for every visible startup state. `window_state = hidden`
    /// still stays hidden.
    #[test]
    fn window_revealed_after_renderer_init_for_visible_states_only() {
        use kettle_config::WindowState;
        assert!(should_reveal_after_renderer_init(WindowState::Normal));
        assert!(should_reveal_after_renderer_init(WindowState::Maximise));
        assert!(should_reveal_after_renderer_init(WindowState::Fullscreen));
        assert!(!should_reveal_after_renderer_init(WindowState::Hidden));
    }

    /// Cycle 919 (audit M1/M2): the default-session restore is opt-in. The same
    /// predicate gates BOTH the startup `load()` and the `save()` so they can't
    /// drift apart (cycle 918 shipped a load-gated/save-unconditional asymmetry
    /// that let a fresh window clobber the saved layout). (F,F) — the default —
    /// means do NOT touch session.json; any opt-in (`--restore` one-shot OR
    /// `restore-session = true`) turns both load and save back on.
    #[test]
    fn restore_session_gate_truth_table() {
        assert!(
            !should_restore_session(false, false),
            "fresh default: no load, no save"
        );
        assert!(should_restore_session(true, false), "--restore one-shot");
        assert!(
            should_restore_session(false, true),
            "restore-session = true"
        );
        assert!(should_restore_session(true, true));
    }

    /// Cycle 919 (audit M2) drift guard: in `resumed()`, the explicit restore
    /// paths (`--tab-handoff-fd`, `--tab-handoff`, `--layout`) must all be
    /// resolved BEFORE the opt-in default-session branch, so an explicit launch
    /// target can never be overridden by a stale `session.json`. A future
    /// re-order of the if/else-if chain would silently break that precedence;
    /// this pins it at the source level.
    #[test]
    fn explicit_restore_paths_precede_default_session() {
        let src = include_str!("app.rs").replace("\r\n", "\n");
        // The LOAD gate specifically (`} else if should_restore_session(...)`) —
        // distinct from the save gate's `None if should_restore_session(...)`,
        // which uses the same args and appears earlier in the file.
        let gate = src
            .find("else if should_restore_session(self.startup.restore")
            .expect(
                "the load gate is `} else if should_restore_session(self.startup.restore, ...)`",
            );
        let before = &src[..gate];
        for marker in [
            "self.startup.tab_handoff_fd",
            "self.startup.tab_handoff.as_deref()",
            "self.startup.layout.as_deref()",
        ] {
            assert!(
                before.contains(marker),
                "{marker} must be resolved BEFORE the opt-in default-session branch \
                 (explicit launch targets outrank a stale session)"
            );
        }
        // And the SAVE side must be gated by the same predicate (no clobber).
        assert!(
            src.contains(
                "None if should_restore_session(self.startup.restore, self.cfg.restore_session)"
            ),
            "save_session must gate the default session.json write behind \
             should_restore_session — symmetric with the load gate (audit M1)"
        );
    }

    /// Cycle 786 drift guard (audit A1/A2): a mouse press / wheel is swallowed
    /// whenever a non-context-menu modal is open, so it can't fall through to
    /// the tab bar / pane focus / mouse-tracking behind the dialog. A lone
    /// context menu is the one exception — it owns its click/scroll paths and a
    /// right-click relocates it — and with nothing open the pointer passes
    /// through as normal.
    #[test]
    fn modal_swallows_pointer_except_lone_context_menu() {
        // A non-context-menu modal (e.g. search/palette/settings) is open.
        assert!(modal_swallows_pointer(true, false));
        // A lone context menu does NOT swallow — its own paths handle it.
        assert!(!modal_swallows_pointer(true, true));
        // Nothing open: the pointer falls through to tabs/panes normally.
        assert!(!modal_swallows_pointer(false, false));
        // Defensive: even if both were somehow set, the context-menu
        // exclusion wins (its dedicated handling ran first).
        assert!(!modal_swallows_pointer(true, true));
    }

    /// Cycle 904 (audit) drift guard. Mouse drag-to-resize of split dividers is
    /// wired across three event handlers (press starts the drag, move applies
    /// the new ratio, up/focus-loss ends it) plus a hover resize-cursor. A
    /// behavioral test needs a window + a real mouse drag (and the geometry math
    /// is already unit-tested in `mux::node_tests`), so pin the wiring at the
    /// source level.
    #[test]
    fn split_divider_drag_is_wired() {
        let src = include_str!("app.rs");
        // Press starts the drag from a seam hit-test.
        assert!(
            src.contains("if let Some(drag) = self.split_drag_at(area, px, py)"),
            "left-press must start a split-divider drag on a seam hit"
        );
        // Move applies the new ratio.
        assert!(
            src.contains("if ws.dragging_split.is_some() {")
                && src.contains("ws.mux.set_split_ratio(ws.mux.active, &path, ratio);"),
            "CursorMoved must apply the dragged split ratio"
        );
        // Up + focus-loss end the drag (distinctive comments at each site, so
        // this guard doesn't self-match its own assertion literals).
        assert!(
            src.contains("Cycle 904: end any split-divider drag on left-button up."),
            "the split drag must be cleared on left-button up"
        );
        assert!(
            src.contains("Cycle 904: a focus loss also ends any split-divider drag."),
            "the split drag must be cleared on focus loss"
        );
        // Hover shows a resize cursor.
        assert!(
            src.contains(".or_else(|| self.split_seam_hover_icon())"),
            "hovering a divider must show the resize cursor"
        );
    }

    /// v2.23.1: the animated background must be EDGE-triggered — a redraw is
    /// requested only when the displayed frame index changes (`bg_frame_due`),
    /// and the loop wakes at the GIF's frame boundary (`bg_anim_interval`), not a
    /// fixed 30 fps. Requesting it every `about_to_wait` (level-triggered) made
    /// winit redraw continuously — the ~55% animated-idle CPU regression.
    #[test]
    fn animated_bg_redraw_is_edge_triggered() {
        let src = include_str!("app.rs").replace("\r\n", "\n");
        assert!(
            src.contains("let bg_frame_due = bg_frame.is_some() && bg_frame != ws.last_bg_frame;"),
            "the bg redraw must edge-trigger on a frame-index change"
        );
        // The redraw block must use the edge (bg_frame_due), not a level anim flag.
        assert!(
            src.contains("|| bg_frame_due\n"),
            "the redraw block must trigger on bg_frame_due, not level anim_active"
        );
        // The wake interval is the GIF's frame boundary, not a fixed 30 fps.
        assert!(
            src.contains("} else if let Some(bg_ms) = bg_anim_interval {"),
            "the wait must use the bg frame interval, not the fixed 33ms tick"
        );
    }

    /// Cycle 908 (dev-record completeness) drift guard. The recorder is fed PTY
    /// output ONLY by `drain_events()` (on redraw), so output that lands after
    /// the last redraw-drain and before a pane is reaped / the window closes
    /// would be dropped with the pane (a fast `-e cmd`'s final line, or bytes in
    /// flight at close). `flush_recorder_output()` must therefore run before
    /// BOTH `mux.reap()` sites and on `CloseRequested`. A behavioral test needs
    /// a full App + window; pin the wiring at the source level via the
    /// distinctive per-site comments (so the guard can't self-match its own
    /// assertion literals). The live close-path tests verify behavior.
    #[cfg(feature = "dev-record")]
    #[test]
    fn recorder_output_flushed_before_reap_and_on_close() {
        let src = include_str!("app.rs");
        assert!(
            src.contains("fn flush_recorder_output(&mut self)"),
            "the recorder-output flush helper must exist"
        );
        for marker in [
            "Cycle 908: capture a just-exited pane's final output before reap drops",
            "Cycle 908: drain trailing recorder output before reap removes a",
            "Cycle 875/908: tee any in-flight PTY output into the trace,",
        ] {
            assert!(
                src.contains(marker),
                "missing recorder-flush wiring at a close/reap site: {marker:?}"
            );
        }
    }

    /// Cycle 897 (audit) drift guards. Three event-state leaks, each needing a
    /// winit event loop (+ modal / tracking PTY / drag gesture) for a behavioral
    /// test, so pinned at the source level like the sibling pointer-gate guards:
    ///   1. A dropped file behind an open modal must not inject into the PTY.
    ///   2. Focus loss must disarm latched drag flags (a swallowed button-up
    ///      otherwise leaves `selecting` / `tab_drag_active` / `mouse_btn` set).
    ///   3. A side-button press/release with a lone context menu open must
    ///      dismiss the menu and NOT forward SGR (the lone menu isn't a
    ///      `modal_swallows_pointer` modal, so the forward leaked before).
    #[test]
    fn event_state_leaks_are_gated() {
        let src = include_str!("app.rs");
        // 1. Dropped-file modal gate, at the top of the arm.
        assert!(
            src.contains("WindowEvent::DroppedFile(path) => {")
                && src.contains("if self.any_modal_open(ws) {\n                    return;\n                }\n                // Standard modern-terminal affordance"),
            "DroppedFile must early-return when a modal is open"
        );
        // 2. Focus-loss drag-flag reset (the block also clears dragging_split
        //    since cycle 904, so check the individual resets, not a contiguous
        //    block).
        assert!(
            src.contains("if ws.selecting && self.cfg.copy_on_select {")
                && src.contains("ws.selecting = false;")
                && src.contains("ws.tab_drag_active = false;\n                    // Cycle 904: a focus loss also ends any split-divider drag.\n                    ws.dragging_split = None;\n                    ws.mouse_btn = None;"),
            "the Focused `!f` arm must disarm the latched drag flags"
        );
        // 3. Side-button dismisses a lone context menu instead of leaking SGR.
        assert!(
            src.contains(
                "if ws.context_menu.is_some() {\n                        ws.context_menu = None;"
            ),
            "a side-button press must dismiss a lone context menu, not forward SGR"
        );
    }

    /// Cycle 831 (audit) drift guard. The cycle-810 side-button (Back/Forward)
    /// forward must be gated behind the modal check — it once sat above it and
    /// leaked SGR into a tracking TUI behind a dialog. A behavioral test needs a
    /// modal + tracking PTY; pin the gated shape at the source level. Both the
    /// Pressed and Released arms must guard `send_mouse(sgr, …)` with
    /// `!modal_swallows_pointer(…)` inside the `extra_mouse_sgr` block.
    /// Cycle 841 (audit) drift guard. A 0×0 `Resized` (window minimize on
    /// Windows) must be ignored — reconfiguring + `resize_all` to 0×0 collapses
    /// every PTY to a 1×1 grid (SIGWINCH storm). A behavioral test needs a winit
    /// event loop; pin the early-return at the source level.
    #[test]
    fn motion_coalesces_to_cell_crossings() {
        // Press/release always report, regardless of the last cell.
        assert!(App::motion_should_report(false, Some((4, 4)), (4, 4)));
        assert!(App::motion_should_report(false, None, (4, 4)));
        // Motion into a NEW cell reports; motion staying in the same cell does not.
        assert!(App::motion_should_report(true, Some((4, 4)), (4, 5)));
        assert!(App::motion_should_report(true, None, (4, 4)));
        assert!(!App::motion_should_report(true, Some((4, 4)), (4, 4)));
    }

    /// Cycle 846 (audit) drift guard. `ScaledZoom` must baseline off the live
    /// renderer size (`r.font_size()`), never `self.cfg.font_size` — the latter
    /// is stale after any manual Increase/DecreaseFontSize (which only touch the
    /// renderer), so scaling from it discards the user's manual zoom on exit.
    /// A behavioral test needs a live renderer + mux; pin it at the source.
    #[test]
    fn scaled_zoom_baselines_off_live_font_size() {
        let src = include_str!("app.rs");
        let arm = src
            .split("Action::ScaledZoom => {")
            .nth(1)
            .and_then(|s| s.split("Action::ToggleFullscreen").next())
            .expect("ScaledZoom arm present");
        assert!(
            arm.contains("let cur = r.font_size();") && arm.contains("cur * 1.5"),
            "ScaledZoom must scale from the live renderer size"
        );
        assert!(
            !arm.contains("self.cfg.font_size * 1.5"),
            "ScaledZoom must not scale from the (stale) config font size"
        );
    }

    /// Cycle 857 (audit) drift guard. `search_key` must filter control chars
    /// before appending to the query (like the title / SSH-input handlers), so a
    /// stray control byte can't corrupt the search. The handler needs full App
    /// state; pin the filter at the source.
    #[test]
    fn search_key_filters_control_chars() {
        let src = include_str!("app.rs");
        // The filtered push lives in search_key's catch-all arm.
        let arm = src
            .split("fn search_key(")
            .nth(1)
            .and_then(|s| s.split("fn ").next())
            .expect("search_key present");
        assert!(
            arm.contains("!t.chars().any(|c| c.is_control())")
                && arm.contains("ws.mux.search.query.push_str(t)"),
            "search_key must filter control chars before appending to the query"
        );
    }

    /// Cycle 863 (audit) drift guard. `ssh_key` must filter control chars
    /// before appending to the host input, like its sibling handlers (the
    /// cycle-857 comment had claimed this was already done). Needs full App
    /// state; pin at the source.
    #[test]
    fn ssh_key_filters_control_chars() {
        let src = include_str!("app.rs");
        let arm = src
            .split("fn ssh_key(")
            .nth(1)
            .and_then(|s| s.split("fn ").next())
            .expect("ssh_key present");
        assert!(
            arm.contains("!t.chars().any(|c| c.is_control())") && arm.contains("q.push_str(t)"),
            "ssh_key must filter control chars before appending to the host input"
        );
    }

    /// v2.20.0 (agent plane): `parse_send_key` is the entire vocabulary an
    /// agent can press — pin the grammar: named keys (incl. the ones the
    /// keybind grammar lacks), chords with every modifier alias, preserved
    /// character case, and loud failure on typos.
    #[test]
    fn parse_send_key_grammar() {
        use super::parse_send_key;
        use winit::keyboard::{Key, ModifiersState, NamedKey};
        let none = ModifiersState::empty();
        // Named keys the keybind grammar has no variant for.
        assert_eq!(
            parse_send_key("escape"),
            Some((none, Key::Named(NamedKey::Escape)))
        );
        assert_eq!(
            parse_send_key("backspace"),
            Some((none, Key::Named(NamedKey::Backspace)))
        );
        assert_eq!(
            parse_send_key("space"),
            Some((none, Key::Named(NamedKey::Space)))
        );
        // Chords, with aliases.
        assert_eq!(
            parse_send_key("ctrl+c"),
            Some((ModifiersState::CONTROL, Key::Character("c".into())))
        );
        assert_eq!(
            parse_send_key("shift+tab"),
            Some((ModifiersState::SHIFT, Key::Named(NamedKey::Tab)))
        );
        assert_eq!(
            parse_send_key("alt+enter"),
            Some((ModifiersState::ALT, Key::Named(NamedKey::Enter)))
        );
        assert_eq!(
            parse_send_key("control+shift+f5"),
            Some((
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                Key::Named(NamedKey::F5)
            ))
        );
        // Character case is PRESERVED — `G` (vim: jump to end) is not `g`.
        assert_eq!(
            parse_send_key("G"),
            Some((none, Key::Character("G".into())))
        );
        assert_eq!(
            parse_send_key(":"),
            Some((none, Key::Character(":".into())))
        );
        // Review fixes: shift+letter normalizes to the uppercase char with
        // SHIFT cleared (what a human's Shift press delivers — the encoder's
        // Character arm ignores SHIFT); super+char has no PTY encoding and
        // fails loudly; the chord/CLI separator characters are reachable
        // via their names.
        assert_eq!(
            parse_send_key("shift+g"),
            Some((none, Key::Character("G".into())))
        );
        assert_eq!(parse_send_key("super+x"), None, "super+char unencodable");
        assert_eq!(
            parse_send_key("plus"),
            Some((none, Key::Character("+".into())))
        );
        assert_eq!(
            parse_send_key("comma"),
            Some((none, Key::Character(",".into())))
        );
        assert_eq!(
            parse_send_key("ctrl+minus"),
            Some((ModifiersState::CONTROL, Key::Character("-".into())))
        );
        // Typos fail loudly instead of degrading.
        assert_eq!(parse_send_key("cttrl+c"), None, "typo'd modifier");
        assert_eq!(parse_send_key("ctrl+"), None, "missing key");
        assert_eq!(parse_send_key("f13"), None, "no such F-key");
        assert_eq!(parse_send_key("escape+x"), None, "named key as modifier");
    }

    /// v2.20.0 (agent plane): the encoded bytes must match what a human
    /// pressing the same keys produces — same encoder, same mode handling.
    #[test]
    fn send_keys_tokens_encode_like_gui_keystrokes() {
        use super::parse_send_key;
        use kettle_core::TermMode;
        let enc = |tok: &str, mode: TermMode| {
            let (mods, key) = parse_send_key(tok).expect(tok);
            crate::input::encode(&key, None, mods, mode)
        };
        let plain = TermMode::empty();
        assert_eq!(enc("escape", plain), Some(vec![0x1b]));
        assert_eq!(enc("ctrl+c", plain), Some(vec![0x03]));
        assert_eq!(enc("enter", plain), Some(vec![b'\r']));
        assert_eq!(enc("up", plain), Some(b"\x1b[A".to_vec()));
        // DECCKM application-cursor mode flips arrows to SS3 — the reason
        // send_keys reads the pane's LIVE mode (vim sets it).
        assert_eq!(enc("up", TermMode::APP_CURSOR), Some(b"\x1bOA".to_vec()));
        assert_eq!(enc("shift+tab", plain), Some(b"\x1b[Z".to_vec()));
        assert_eq!(enc("G", plain), Some(b"G".to_vec()));
    }

    /// v2.20.0 (`vim-menu-nav`) drift guards: (1) the vim layer must run
    /// BEFORE the mnemonic/typeahead catch-all in `context_menu_key` — moved
    /// after it, bare `j`/`k` would be eaten as typeahead input and the nav
    /// would silently die; (2) the catch-all's mnemonic lookup must pass the
    /// reservation set so a row can never claim a nav letter while the
    /// setting is on. Both are ordering/wiring contracts a behavioral test
    /// can't see without a live window; pin them at the source.
    #[test]
    fn vim_menu_nav_intercepts_before_mnemonic_catchall() {
        let src = include_str!("app.rs");
        let body = src
            .split("fn context_menu_key(")
            .nth(1)
            .and_then(|s| s.split("\n    fn ").next())
            .expect("context_menu_key present");
        let vim = body
            .find("self.context_menu_vim_key(ws, key, event_loop)")
            .expect("context_menu_key must consult the vim layer");
        let catchall = body
            .find("assign_mnemonics(&menu.items, reserved)")
            .expect("the mnemonic lookup must pass the reservation set");
        assert!(
            vim < catchall,
            "vim nav must intercept BEFORE the mnemonic/typeahead catch-all"
        );
    }

    /// Cycle 856 (audit) drift guard. The single "Window padding" Settings
    /// control must persist BOTH `window-padding-x` and `-y` so the result is
    /// symmetric — persisting only X leaves Y at its default (lopsided). A
    /// behavioral test needs the live overlay + persist path; pin the
    /// dual-write at the source.
    #[test]
    fn window_padding_setting_writes_both_axes() {
        let src = include_str!("app.rs");
        assert!(
            src.contains("if key_str == \"window-padding-x\" {")
                && src.contains("self.persist_pref(\"window-padding-y\", &new_val);"),
            "the Window-padding control must mirror its value to window-padding-y"
        );
    }

    #[test]
    fn resized_ignores_degenerate_size() {
        let src = include_str!("app.rs");
        let arm = src
            .split("WindowEvent::Resized(size) => {")
            .nth(1)
            .expect("Resized arm present");
        let head = &arm[..arm.len().min(900)];
        assert!(
            head.contains("size.width == 0 || size.height == 0") && head.contains("return"),
            "the Resized handler must early-return on a 0-dimension size"
        );
    }

    /// Dropdown-parity cycle: the `▾` menu is shells → separator → Windows
    /// Terminal's bottom section (Settings / Command palette / About).
    #[test]
    fn new_tab_menu_items_has_wt_bottom_rows() {
        let shells = vec![
            ("PowerShell".to_string(), vec!["pwsh.exe".to_string()]),
            ("Git Bash".to_string(), vec!["bash.exe".to_string()]),
        ];
        let items = App::new_tab_menu_items(&shells);
        assert_eq!(items.len(), 6, "2 shells + separator + 3 bottom rows");
        assert!(matches!(
            &items[0],
            ContextMenuItem::NewTabShell { label, .. } if label == "PowerShell"
        ));
        assert!(matches!(items[2], ContextMenuItem::Separator));
        assert!(matches!(
            items[3],
            ContextMenuItem::Item {
                label: "Settings…",
                action: Action::OpenSettings,
                enabled: true
            }
        ));
        assert!(matches!(
            items[4],
            ContextMenuItem::Item {
                label: "Command palette",
                action: Action::CommandPalette,
                enabled: true
            }
        ));
        assert!(matches!(
            items[5],
            ContextMenuItem::Item {
                label: "About kettle",
                action: Action::About,
                enabled: true
            }
        ));
        // Every dispatchable row gets a distinct mnemonic; the two-pass
        // assignment (cycle 942) must keep working with the new bottom rows.
        let mn = assign_mnemonics(&items, &[]);
        let mut letters: Vec<char> = mn.iter().flatten().map(|(_, c)| *c).collect();
        assert_eq!(letters.len(), 5, "all 5 dispatchable rows get mnemonics");
        letters.sort_unstable();
        letters.dedup();
        assert_eq!(letters.len(), 5, "mnemonics are distinct");
    }

    /// Dropdown-parity cycle: an `Info` row (About panel) is inert — not
    /// dispatchable, no click mapping, survives filter_disabled.
    #[test]
    fn info_rows_are_inert_but_visible() {
        let info = ContextMenuItem::Info {
            label: "kettle 9.9.9".to_string(),
        };
        assert!(!super::item_is_dispatchable(&info));
        assert!(super::item_to_click(&info, 0).is_none());
        let kept = filter_disabled(vec![
            ContextMenuItem::Info {
                label: "x".to_string(),
            },
            ContextMenuItem::Separator,
            item("Copy version info", true),
        ]);
        assert_eq!(kept.len(), 3, "Info + separator + item all survive");
    }

    /// PERF (key-repeat stutter fix) drift guard: output inside the typing
    /// window bypasses the coalescer (paints immediately); output after it
    /// re-enters the frame-budget defer. The user-visible symptom this pins:
    /// holding a key stuttered in kettle but not Terminator, because echo
    /// paints rode the WaitUntil deadline (~16ms Windows timer granularity)
    /// instead of the keystroke cadence.
    #[test]
    fn typed_echo_bypasses_the_output_coalescer() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        // Echo within the window → immediate (bypass).
        assert!(super::typed_recently(
            now,
            Some(now - Duration::from_millis(30)),
            super::TYPING_ECHO_WINDOW
        ));
        // Stale keystroke → back to the coalescer.
        assert!(!super::typed_recently(
            now,
            Some(now - Duration::from_millis(500)),
            super::TYPING_ECHO_WINDOW
        ));
        // Never typed → coalescer.
        assert!(!super::typed_recently(now, None, super::TYPING_ECHO_WINDOW));
        // The window must bridge OS key-repeat intervals (~33ms default) with
        // slack for ConPTY echo latency.
        assert!(super::TYPING_ECHO_WINDOW >= Duration::from_millis(100));
        // The Wakeup arm must consult it (source-level pin: the bypass calls
        // request_redraw and clears any pending coalesce).
        let src = include_str!("app.rs").replace("\r\n", "\n");
        assert!(
            src.contains(
                "if typed_recently(now, ws.last_typed, TYPING_ECHO_WINDOW) {\n                    ws.coalescing_paint = false;"
            ),
            "the Wakeup arm must paint echo immediately"
        );
    }

    /// B (Peacock) drift guard: the live-dedupe pool walk. Same project →
    /// same starting slot; a collision with a live window advances to the
    /// next free hue; a fully-claimed pool accepts the seed slot.
    #[test]
    fn accent_slot_walk_dedupes_live_windows() {
        let pool: Vec<kettle_config::Rgb> =
            (0u8..4).map(|i| kettle_config::Rgb::new(i, i, i)).collect();
        // Empty in-use → the seed's own slot.
        assert_eq!(super::pick_accent_slot(&pool, 1, &[]), 1);
        assert_eq!(
            super::pick_accent_slot(&pool, 5, &[]),
            1,
            "seed wraps modulo"
        );
        // Seed slot taken → next free.
        assert_eq!(super::pick_accent_slot(&pool, 1, &[pool[1]]), 2);
        // Walk wraps past the end.
        assert_eq!(super::pick_accent_slot(&pool, 3, &[pool[3], pool[0]]), 1);
        // Fully claimed → fall back to the seed slot (accept the collision).
        assert_eq!(
            super::pick_accent_slot(&pool, 2, &[pool[0], pool[1], pool[2], pool[3]]),
            2
        );
        // Degenerate empty pool.
        assert_eq!(super::pick_accent_slot(&[], 7, &[]), 0);
        // Presence wire format round-trips through Rgb::parse.
        let c = kettle_config::Rgb::new(0xcb, 0xa6, 0xf7);
        assert_eq!(kettle_config::Rgb::parse(&super::rgb_hex(c)), Some(c));
    }

    /// C7 regression guard. `resumed_inner` must take ONLY the consumed-once
    /// CLI fields (`command`, `cwd`) from `self.startup` — a wholesale
    /// `mem::take(&mut self.startup)` silently defaults every later read:
    /// the `--tab-handoff` / `--layout` / `--restore` startup gates (they
    /// loaded NOTHING — verified live, a 2-tab `--layout` opened 1 tab), the
    /// save-session layout/restore gating, and reload_config's cycle-938
    /// launch-override re-application.
    #[test]
    fn startup_is_not_taken_wholesale() {
        let src = include_str!("app.rs").replace("\r\n", "\n");
        let taken = concat!("std::mem::take", "(&mut self.startup)");
        assert!(
            !src.contains(taken),
            "self.startup must never be wholesale-taken; take the \
             consumed-once fields individually"
        );
        assert!(
            src.contains("let cmd_override = self.startup.command.take();")
                && src.contains("let cwd_override = self.startup.cwd.take();"),
            "the -e/-d overrides are the only consumed-once startup fields"
        );
    }

    /// Drift guard for `theme-mode = auto` / `system` / `follow-system`.
    /// OS appearance following must apply both the initial window theme and
    /// live `WindowEvent::ThemeChanged` updates, while `theme-schedule` remains
    /// the explicit owner when configured.
    #[test]
    fn system_theme_following_is_wired() {
        let src = include_str!("app.rs").replace("\r\n", "\n");
        assert!(
            src.contains(concat!(
                "WindowEvent::ThemeChanged",
                "(theme) => {\n                self.apply_os_theme_preference(ws, theme);"
            )),
            "winit ThemeChanged events must feed theme-mode=auto"
        );
        assert!(
            src.contains(concat!("ws.window.as_ref().and_then", "(|w| w.theme())")),
            "startup must apply the platform's current window theme when winit reports one"
        );
        assert!(
            src.contains(concat!(
                "self.apply_initial_os_theme_preference",
                "(theme);"
            )),
            "startup OS theme application must use the pre-session-save path"
        );
        let initial_body = src
            .split(concat!("fn apply_initial_os_theme_preference", "("))
            .nth(1)
            .and_then(|s| s.split(concat!("fn apply_os_theme_preference", "(")).next())
            .expect("initial OS theme helper exists");
        assert!(
            !initial_body.contains(concat!("save_", "session")),
            "initial OS theme application must not save an empty startup session"
        );
        assert!(
            src.contains(concat!(
                "if self.cfg.theme_schedule.is_some",
                "() {\n            return None;\n        }"
            )),
            "theme-schedule must not fight OS appearance following"
        );
    }

    /// C4 (multi-window) drift guard. A window close must NOT exit the event
    /// loop directly — it sets `pending_window_close` and the single funnel
    /// (`finish_window_dispatch`) drops the window, exiting only when the
    /// windows map is empty. The only legitimate direct exits are the funnel
    /// itself (close + quit arms) and `resumed_inner`'s window-1 startup
    /// failures (window create / renderer init / `-e` spawn / shell spawn),
    /// where no other window can exist yet. A new `event_loop.exit()`
    /// anywhere else reintroduces "closing one window kills them all".
    #[test]
    fn event_loop_exit_sites_are_allowlisted() {
        let src = include_str!("app.rs").replace("\r\n", "\n");
        // concat! so this test's own literals don't self-match the scan.
        let exit_needle = concat!("event_loop", ".exit();");
        let n_exits = src.matches(exit_needle).count();
        assert_eq!(
            n_exits, 6,
            "expected exactly 6 event_loop.exit() sites (2 in \
             finish_window_dispatch + 4 resumed_inner startup failures); a \
             new one must route through pending_window_close instead"
        );
        // The close paths all flag instead of exiting: keybind CloseTab /
        // ClosePane / CloseWindow, the three confirm-dialog arms, the OS
        // close button, the tab-bar ✕ on the last tab, and the reap path.
        let flag_needle = concat!("self.pending_window_close", " = true;");
        let n_flags = src.matches(flag_needle).count();
        assert!(
            n_flags >= 9,
            "expected the 9 window-close paths to set pending_window_close \
             (found {n_flags})"
        );
    }

    #[test]
    fn side_button_forward_is_modal_gated() {
        // Cycle 885: normalize CRLF before this multi-line `\n` scan. The GitHub
        // Windows runner checks out with autocrlf, turning the LF-committed file
        // into CRLF so the exact `{\n   self.send_mouse(sgr,` literal matched 0
        // (the `build (windows-latest)` job was red from v2.8.0). The new
        // `.gitattributes eol=lf` fixes checkout; this keeps the test robust
        // even on a CRLF working tree. (`\r` removal doesn't touch the escaped
        // `\n` in this literal, so the test's own source can't self-match.)
        let src = include_str!("app.rs").replace('\r', "");
        let gated = src
            .matches("if !modal_swallows_pointer(self.any_modal_open(ws), ws.context_menu.is_some()) {\n                        self.send_mouse(ws, sgr,")
            .count();
        assert!(
            gated >= 2,
            "both Pressed and Released side-button forwards must be modal-gated (found {gated})"
        );
    }

    fn item(label: &'static str, enabled: bool) -> ContextMenuItem {
        // Any concrete Action works — the filter only looks at the
        // `enabled` flag + variant kind, not the action payload.
        ContextMenuItem::Item {
            label,
            action: Action::Paste,
            enabled,
        }
    }

    /// Cycle 713 drift guard. Disabled `Item`s are removed entirely
    /// (Terminator-style "only show what you can click") and
    /// orphaned/leading/trailing separators collapse so the menu
    /// never has visual gaps that lead nowhere.
    #[test]
    fn disabled_items_are_hidden_and_separators_collapse() {
        // Mixed: Copy disabled, Paste enabled, separator, Split disabled,
        // separator, NewTab enabled, separator (trailing).
        let menu = vec![
            item("Copy", false),
            item("Paste", true),
            ContextMenuItem::Separator,
            item("Split Right", false),
            ContextMenuItem::Separator,
            item("New Tab", true),
            ContextMenuItem::Separator,
        ];
        let got = filter_disabled(menu);
        // Expected: Paste, separator, New Tab.
        assert_eq!(got.len(), 3);
        assert!(matches!(&got[0], ContextMenuItem::Item { label, .. } if *label == "Paste"));
        assert!(matches!(&got[1], ContextMenuItem::Separator));
        assert!(matches!(&got[2], ContextMenuItem::Item { label, .. } if *label == "New Tab"));
    }

    /// Cycle 713: runs of separators collapse to a single one; a
    /// leading separator (everything-disabled above it) gets dropped.
    #[test]
    fn consecutive_separators_collapse_and_leading_is_dropped() {
        let menu = vec![
            ContextMenuItem::Separator,
            ContextMenuItem::Separator,
            item("OK", true),
            ContextMenuItem::Separator,
            ContextMenuItem::Separator,
            ContextMenuItem::Separator,
            item("Cancel", true),
        ];
        let got = filter_disabled(menu);
        assert_eq!(got.len(), 3);
        assert!(matches!(&got[0], ContextMenuItem::Item { label, .. } if *label == "OK"));
        assert!(matches!(&got[1], ContextMenuItem::Separator));
        assert!(matches!(&got[2], ContextMenuItem::Item { label, .. } if *label == "Cancel"));
    }

    /// Cycle 713: filter is identity (modulo trailing separator) when
    /// nothing is disabled.
    #[test]
    fn filter_disabled_is_near_identity_when_all_enabled() {
        let menu = vec![item("A", true), ContextMenuItem::Separator, item("B", true)];
        let got = filter_disabled(menu);
        assert_eq!(got.len(), 3);
    }

    /// Cycle 713: empty menu stays empty (defensive).
    #[test]
    fn filter_disabled_handles_empty() {
        let got = filter_disabled(vec![]);
        assert!(got.is_empty());
    }

    /// Cycle 713: all-disabled menu collapses to empty (no rows, no
    /// orphan separators).
    #[test]
    fn filter_disabled_collapses_all_disabled_to_empty() {
        let menu = vec![
            item("X", false),
            ContextMenuItem::Separator,
            item("Y", false),
        ];
        let got = filter_disabled(menu);
        assert!(got.is_empty());
    }

    /// Cycle 717 drift guard. The Preferences ▸ submenu builder
    /// must surface every runtime-mutable toggle the spec promises:
    ///   - 3 scrollbar radio rows
    ///   - 3 boolean toggles (cursor blink, copy on select, mouse-hide)
    ///   - 4 bell radio rows
    ///   - 2 font-size +/− rows
    ///   - 1 Advanced… (EditConfig) escape hatch
    ///
    /// Total: 13 actionable rows + 4 separators = 17 items. If the
    /// count drifts (someone adds a row without updating this guard
    /// or removes one in a refactor), the test fails so the
    /// regression is caught at PR time.
    #[test]
    fn preferences_submenu_contains_all_user_facing_toggles() {
        // We can't directly call append_preferences_submenu_items
        // without an App; instead pin the Action variants this cycle
        // ships so the keybinds-side palette wiring stays in sync.
        let expected_actions: &[Action] = &[
            Action::SetScrollbarAlways,
            Action::SetScrollbarAuto,
            Action::SetScrollbarNever,
            Action::ToggleCursorBlink,
            Action::ToggleCopyOnSelect,
            Action::ToggleMouseHide,
            Action::SetBellOff,
            Action::SetBellVisual,
            Action::SetBellAttention,
            Action::SetBellBoth,
            Action::IncreaseFontSize,
            Action::DecreaseFontSize,
            Action::EditConfig,
        ];
        // Each action parses from its name (cycle-104 from_name
        // surface) so the keybind grammar accepts them. Catches the
        // case where someone adds a variant but forgets the
        // from_name arm.
        for a in expected_actions {
            // We need a known name string for each. Use the canonical
            // forms documented in palette.rs / keybinds.rs.
            let name = match a {
                Action::SetScrollbarAlways => "set_scrollbar_always",
                Action::SetScrollbarAuto => "set_scrollbar_auto",
                Action::SetScrollbarNever => "set_scrollbar_never",
                Action::ToggleCursorBlink => "toggle_cursor_blink",
                Action::ToggleCopyOnSelect => "toggle_copy_on_select",
                Action::ToggleMouseHide => "toggle_mouse_hide",
                Action::SetBellOff => "set_bell_off",
                Action::SetBellVisual => "set_bell_visual",
                Action::SetBellAttention => "set_bell_attention",
                Action::SetBellBoth => "set_bell_both",
                Action::IncreaseFontSize => "increase_font_size",
                Action::DecreaseFontSize => "decrease_font_size",
                Action::EditConfig => "edit_config",
                _ => unreachable!(),
            };
            let parsed = Action::from_name(name)
                .unwrap_or_else(|| panic!("Action::from_name({name:?}) returned None"));
            assert_eq!(
                std::mem::discriminant(&parsed),
                std::mem::discriminant(a),
                "Action::from_name({name:?}) returned the wrong variant"
            );
        }
    }

    /// Cycle 715 drift guard. `assign_mnemonics` returns the first
    /// A-Z char per row; on collision the second row's first letter
    /// is taken, so the second row falls through to its next A-Z.
    /// Pinning the contract: Copy=C, Close Pane=l (C taken),
    /// Cancel=a (C taken).
    #[test]
    fn mnemonics_assign_unique_chars_with_fallback() {
        let menu = vec![
            item("Copy", true),       // 'C'
            item("Close Pane", true), // 'C' taken → 'l'
            item("Cancel", true),     // 'C' + 'l' taken → 'a'
            ContextMenuItem::Separator,
            item("Theme", true), // 'T'
            item("Tab", true),   // 'T' taken → 'a' taken → 'b'
            item("12345", true), // no A-Z → None
        ];
        let mn = assign_mnemonics(&menu, &[]);
        assert_eq!(mn[0], Some((0, 'c')));
        // "Close Pane": C taken, so next alphabetic is 'l' at byte 1.
        assert_eq!(mn[1], Some((1, 'l')));
        // "Cancel": C + l taken, so next is 'a' at byte 1.
        assert_eq!(mn[2], Some((1, 'a')));
        // Separator: no label, no mnemonic.
        assert_eq!(mn[3], None);
        // "Theme": T at byte 0.
        assert_eq!(mn[4], Some((0, 't')));
        // "Tab": T taken, a taken, so 'b' at byte 2.
        assert_eq!(mn[5], Some((2, 'b')));
        // "12345": no A-Z, None.
        assert_eq!(mn[6], None);
    }

    /// Cycle 942 (audit): the URL-aware leading rows claim their mnemonics
    /// AFTER the stable core rows. Without the two-round pass, "Open Link" /
    /// "Copy Link Address" leading the menu stole 'o'/'c', silently remapping
    /// muscle-memory mnemonics whenever the right-click landed on a link
    /// ('p' fired Copy instead of Paste).
    #[test]
    fn mnemonics_url_rows_claim_letters_last() {
        let url = |label: &'static str, copy: bool| ContextMenuItem::UrlItem {
            label,
            url: "https://example.com".into(),
            copy,
        };
        let menu = vec![
            url("Open Link", false),
            url("Copy Link Address", true),
            ContextMenuItem::Separator,
            item("Copy", true),
            item("Paste", true),
        ];
        let mn = assign_mnemonics(&menu, &[]);
        // Core rows keep their stable letters…
        assert_eq!(mn[3], Some((0, 'c'))); // Copy = c (not stolen)
        assert_eq!(mn[4], Some((0, 'p'))); // Paste = p (not stolen)
        // …and the URL rows pick from what's left.
        assert_eq!(mn[0], Some((0, 'o'))); // Open Link = o (free)
        // "Copy Link Address": c, o, p taken → 'y' (byte 3).
        assert_eq!(mn[1], Some((3, 'y')));
        assert_eq!(mn[2], None); // separator
    }

    /// v2.20.0 (`vim-menu-nav`) drift guard: while the setting is on, the
    /// vim navigation letters must never be assigned as mnemonics — the nav
    /// layer intercepts them first, so a row keyed on one would silently
    /// lose its hotkey. Rows fall through to their next free letter instead.
    #[test]
    fn vim_nav_letters_are_excluded_from_mnemonics() {
        let menu = vec![
            item("Jump", true),      // 'j' reserved → 'u'
            item("Kill Pane", true), // 'k' reserved → 'i'
            item("Hold", true),      // 'h' reserved → 'o'
            item("List", true),      // 'l' reserved → 'i' taken → 's'
            item("Go", true),        // 'g' reserved → 'o' taken → None
        ];
        let mn = assign_mnemonics(&menu, super::VIM_NAV_RESERVED);
        for (i, slot) in mn.iter().enumerate() {
            if let Some((_, c)) = slot {
                assert!(
                    !super::VIM_NAV_RESERVED.contains(c),
                    "row {i} was assigned reserved nav letter {c:?}"
                );
            }
        }
        assert_eq!(mn[0], Some((1, 'u'))); // Jump → 'u'
        assert_eq!(mn[1], Some((1, 'i'))); // Kill Pane → 'i'
        assert_eq!(mn[2], Some((1, 'o'))); // Hold → 'o'
        assert_eq!(mn[3], Some((2, 's'))); // List → 's'
        // "Go": both letters reserved/taken → no mnemonic at all.
        assert_eq!(mn[4], None);
        // And with the reservation off, first letters win as before.
        let plain = assign_mnemonics(&menu, &[]);
        assert_eq!(plain[0], Some((0, 'j')));
        assert_eq!(plain[1], Some((0, 'k')));
    }

    /// v2.20.0 (`vim-menu-nav`): `Ctrl+d`/`Ctrl+u` clamp at the list ends
    /// (no wrap — vim half-page semantics) and snap off separators in the
    /// direction of travel.
    #[test]
    fn half_page_menu_target_clamps_and_snaps() {
        let menu = vec![
            item("A", true),            // 0
            item("B", true),            // 1
            ContextMenuItem::Separator, // 2
            item("C", true),            // 3
            item("D", true),            // 4
        ];
        // Down by 2 from row 0 lands on the separator → snaps DOWN to 3.
        assert_eq!(super::half_page_menu_target(&menu, 0, 2, 1), 3);
        // Down by 10 from row 0 clamps to the last row.
        assert_eq!(super::half_page_menu_target(&menu, 0, 10, 1), 4);
        // Up by 2 from row 4 lands on the separator → snaps UP to 1.
        assert_eq!(super::half_page_menu_target(&menu, 4, 2, -1), 1);
        // Up by 10 clamps to the first row.
        assert_eq!(super::half_page_menu_target(&menu, 4, 10, -1), 0);
        // Empty list: target stays put.
        assert_eq!(super::half_page_menu_target(&[], 0, 3, 1), 0);
    }

    /// Cycle 715 drift guard. Typeahead prefix-match is case-
    /// insensitive and stops at the first dispatchable hit.
    #[test]
    fn typeahead_th_highlights_theme_first() {
        let menu = vec![
            item("Copy", true),
            item("Theme", true),
            item("Toggle Broadcast", true),
            item("Profile", true),
        ];
        // Single-char "t" hits Theme (first label starting with t).
        assert_eq!(typeahead_match(&menu, "t"), Some(1));
        // "th" still Theme.
        assert_eq!(typeahead_match(&menu, "th"), Some(1));
        // "to" → Toggle Broadcast.
        assert_eq!(typeahead_match(&menu, "to"), Some(2));
        // Uppercase = lowercase (case-insensitive).
        assert_eq!(typeahead_match(&menu, "TH"), Some(1));
        // No match.
        assert_eq!(typeahead_match(&menu, "xyz"), None);
        // Empty buffer.
        assert_eq!(typeahead_match(&menu, ""), None);
    }

    /// Cycle 715 drift guard. Disabled rows aren't typeahead targets
    /// (would be confusing to highlight a row that can't dispatch).
    #[test]
    fn typeahead_skips_disabled_rows() {
        let menu = vec![item("Theme", false), item("Theme Plus", true)];
        // First Theme is disabled; second matches.
        assert_eq!(typeahead_match(&menu, "th"), Some(1));
    }

    /// Cycle 714 drift guard. `count_rows_fitting` walks rows from
    /// `start` forward and sums heights until the next row would
    /// exceed `panel_h`. Pinning the arithmetic so scroll math
    /// can't silently drift if the row-height constants change.
    #[test]
    fn count_rows_fitting_respects_panel_height_and_separator_height() {
        let menu = vec![
            item("A", true),            // row_h=24
            item("B", true),            // row_h=24
            ContextMenuItem::Separator, // sep_h=8
            item("C", true),            // row_h=24
            item("D", true),            // row_h=24
            item("E", true),            // row_h=24
        ];
        let row_h = 24.0;
        let sep_h = 8.0;
        // panel_h = 0 -> nothing fits.
        assert_eq!(count_rows_fitting(&menu, 0, 0.0, row_h, sep_h), 0);
        // panel_h = 24 -> A only (B would push us to 48 > 24).
        assert_eq!(count_rows_fitting(&menu, 0, 24.0, row_h, sep_h), 1);
        // panel_h = 48 -> A + B.
        assert_eq!(count_rows_fitting(&menu, 0, 48.0, row_h, sep_h), 2);
        // panel_h = 56 -> A + B + separator (24 + 24 + 8 = 56).
        assert_eq!(count_rows_fitting(&menu, 0, 56.0, row_h, sep_h), 3);
        // panel_h = 1000 -> all 6 rows.
        assert_eq!(count_rows_fitting(&menu, 0, 1000.0, row_h, sep_h), 6);
        // Start past the separator: skip the first 3, fit C+D in 48.
        assert_eq!(count_rows_fitting(&menu, 3, 48.0, row_h, sep_h), 2);
        // Start at last row, plenty of height — just 1 fits.
        assert_eq!(count_rows_fitting(&menu, 5, 100.0, row_h, sep_h), 1);
        // Start past the end -> 0.
        assert_eq!(count_rows_fitting(&menu, 99, 1000.0, row_h, sep_h), 0);
    }

    /// Cycle 714 drift guard. With a 512-entry submenu and a real
    /// surface-bound panel height of ~580px (surface_h=660 - 80px
    /// chrome breathing room), `count_rows_fitting` reports a tiny
    /// fraction of the total — the rest scrolls into view. The
    /// "menu doesn't grow off-screen" invariant.
    #[test]
    fn theme_submenu_with_512_entries_clamps_panel_to_surface_height() {
        let big: Vec<ContextMenuItem> = (0..512)
            .map(|i| ContextMenuItem::Item {
                label: Box::leak(format!("Theme {i}").into_boxed_str()),
                action: Action::Paste,
                enabled: true,
            })
            .collect();
        let row_h = 24.0;
        let sep_h = 8.0;
        // 580px / 24px ≈ 24 rows visible.
        let visible = count_rows_fitting(&big, 0, 580.0, row_h, sep_h);
        assert!(
            (20..30).contains(&visible),
            "expected ~24 visible rows at 580px panel; got {visible}"
        );
        // All 512 rows shouldn't possibly fit in 580px.
        assert!(visible < big.len(), "panel must clamp; visible={visible}");
    }

    /// Cycle 712 drift guard. Hover-to-highlight walks `find_menu_row_y`
    /// on every `CursorMoved`; pin the contract so a render-layout
    /// change can't silently drop separator handling, off-by-one the
    /// last row, or break the out-of-bounds contract.
    #[test]
    fn hover_updates_menu_highlight_skipping_separators() {
        // Menu layout: 4 rows with a separator between row 1 (Item)
        // and row 3 (Item):
        //   row 0 (Item)      [anchor + 0   .. anchor + row_h]
        //   row 1 (Item)      [anchor + 24  .. anchor + 48]
        //   row 2 (Separator) [anchor + 48  .. anchor + 56]
        //   row 3 (Item)      [anchor + 56  .. anchor + 80]
        let anchor_y = 100.0;
        let row_h = 24.0;
        let sep_h = 8.0;
        let kinds = [false, false, true, false];
        // Cursor squarely inside row 0.
        assert_eq!(
            find_menu_row_y(anchor_y + 10.0, anchor_y, row_h, sep_h, &kinds),
            Some(0)
        );
        // Cursor on the edge between row 0 and row 1: lands on row 1
        // (half-open interval [y, y+h)).
        assert_eq!(
            find_menu_row_y(anchor_y + row_h, anchor_y, row_h, sep_h, &kinds),
            Some(1)
        );
        // Cursor inside row 1.
        assert_eq!(
            find_menu_row_y(anchor_y + 30.0, anchor_y, row_h, sep_h, &kinds),
            Some(1)
        );
        // Cursor inside the separator — None (don't highlight a
        // visual-only gap).
        assert_eq!(
            find_menu_row_y(anchor_y + 50.0, anchor_y, row_h, sep_h, &kinds),
            None
        );
        // Cursor inside row 3 (after the separator).
        assert_eq!(
            find_menu_row_y(anchor_y + 60.0, anchor_y, row_h, sep_h, &kinds),
            Some(3)
        );
        // Cursor above the panel.
        assert_eq!(
            find_menu_row_y(anchor_y - 1.0, anchor_y, row_h, sep_h, &kinds),
            None
        );
        // Cursor below the last row.
        assert_eq!(
            find_menu_row_y(anchor_y + 1000.0, anchor_y, row_h, sep_h, &kinds),
            None
        );
        // Empty menu: always None.
        assert_eq!(
            find_menu_row_y(anchor_y + 10.0, anchor_y, row_h, sep_h, &[]),
            None
        );
    }

    /// Cycle 708 drift guard. `rank_layouts` filters layout names by
    /// every lower-cased query token; empty query returns identity.
    #[test]
    fn rank_layouts_filters_by_tokens_case_insensitive() {
        let layouts: Vec<String> = ["dev", "work", "dev-rust", "Notes", "ssh prod"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Empty query — identity, every layout shows.
        assert_eq!(rank_layouts("", &layouts), vec![0, 1, 2, 3, 4]);
        // Whitespace-only query — still identity (trim).
        assert_eq!(rank_layouts("   ", &layouts), vec![0, 1, 2, 3, 4]);
        // Single token, case-insensitive.
        assert_eq!(rank_layouts("dev", &layouts), vec![0, 2]);
        assert_eq!(rank_layouts("DEV", &layouts), vec![0, 2]);
        // Multi-token AND (every token must match).
        assert_eq!(rank_layouts("dev rust", &layouts), vec![2]);
        assert_eq!(rank_layouts("ssh prod", &layouts), vec![4]);
        // Lower-cased target matches mixed-case layouts.
        assert_eq!(rank_layouts("notes", &layouts), vec![3]);
        // No matches → empty (not a panic).
        assert_eq!(rank_layouts("xyz", &layouts), Vec::<usize>::new());
        // Empty layouts → empty (defensive).
        assert_eq!(rank_layouts("dev", &[]), Vec::<usize>::new());
    }

    #[test]
    fn cap_title_for_status_bar_truncates_at_char_budget_with_ellipsis() {
        // Cycle 308 drift guard. The status-bar strip is 1 cell tall;
        // a long title would wrap past the visible region without
        // this cap. Pins the contract:
        //   - under-budget: returned as-is, no `…`.
        //   - over-budget: truncated to `max` chars + `…`.
        //   - UTF-8 multibyte: char-count not byte-count (so a
        //     "🦀🦀🦀…" title isn't mis-truncated in the middle of
        //     a surrogate pair).
        use super::cap_title_for_status_bar;
        assert_eq!(cap_title_for_status_bar("short", 60), "short");
        assert_eq!(cap_title_for_status_bar("", 60), "");
        let exactly_60 = "x".repeat(60);
        assert_eq!(cap_title_for_status_bar(&exactly_60, 60), exactly_60);
        let long = "x".repeat(80);
        let capped = cap_title_for_status_bar(&long, 60);
        assert_eq!(capped.chars().count(), 61); // 60 + the `…`
        assert!(capped.ends_with('…'));
        // UTF-8 multibyte at the boundary — must split on a char
        // boundary, not a byte boundary.
        let crab_run = "🦀".repeat(80);
        let capped_crab = cap_title_for_status_bar(&crab_run, 60);
        assert_eq!(capped_crab.chars().count(), 61);
        // Every char before the `…` should still be a full crab.
        assert!(capped_crab.chars().take(60).all(|c| c == '🦀'));
    }

    /// Cycle 934 (agent-first A4) + cycle 941 (read-only): the per-pane
    /// titlebar badges, composed by `compose_pane_title`.
    #[test]
    fn pane_title_badges_compose_from_state() {
        use super::compose_pane_title;
        // No badges: title unchanged regardless of the configured badge text.
        assert_eq!(compose_pane_title("[agent] ", false, false, "bash"), "bash");
        // Agent attached: badge prefixed.
        assert_eq!(
            compose_pane_title("[agent] ", true, false, "bash"),
            "[agent] bash"
        );
        // Empty badge disables the agent prefix even when attached.
        assert_eq!(compose_pane_title("", true, false, "bash"), "bash");
        // Custom badge glyph.
        assert_eq!(compose_pane_title("🤖 ", true, false, "vim"), "🤖 vim");
        // Read-only: `[RO] ` prefixed.
        assert_eq!(
            compose_pane_title("[agent] ", false, true, "bash"),
            "[RO] bash"
        );
        // Both: read-only leads, then the agent badge.
        assert_eq!(
            compose_pane_title("[agent] ", true, true, "bash"),
            "[RO] [agent] bash"
        );
        // Read-only with an empty agent badge still shows `[RO]`.
        assert_eq!(compose_pane_title("", true, true, "bash"), "[RO] bash");
    }

    #[test]
    fn shell_quote_path_handles_spaces_quotes_and_multibyte() {
        use super::shell_quote_path;
        use std::path::Path;
        // Plain path — still wrapped (always-quote keeps the rule simple
        // and the output predictable across every special-char list).
        assert_eq!(
            shell_quote_path(Path::new("/foo/bar.txt")),
            "'/foo/bar.txt'",
        );
        // Spaces in a path — quoting is the *whole point*; cat 'a b.txt'
        // works, cat a b.txt would be two arguments.
        assert_eq!(
            shell_quote_path(Path::new("/foo bar/baz qux.txt")),
            "'/foo bar/baz qux.txt'",
        );
        // Embedded apostrophe — POSIX form is close-quote, escape, reopen.
        // `/foo'bar` becomes `'/foo'\''bar'`. bash/zsh/fish all accept this
        // identically.
        assert_eq!(
            shell_quote_path(Path::new("/foo'bar.txt")),
            r"'/foo'\''bar.txt'",
        );
        // Multiple apostrophes — each gets the same treatment.
        assert_eq!(shell_quote_path(Path::new("'a'b'")), r"''\''a'\''b'\'''",);
        // Multibyte (Japanese path component) — passes through verbatim
        // inside the quotes; no special handling needed since UTF-8 is
        // shell-safe.
        let p = Path::new("/路径/file.txt");
        assert_eq!(shell_quote_path(p), "'/路径/file.txt'");
        // Empty path — empty quotes (harmless on shell, the user will
        // see '' and notice).
        assert_eq!(shell_quote_path(Path::new("")), "''");
    }

    #[test]
    fn wheel_lines_scales_by_multiplier() {
        use super::wheel_lines;
        use winit::dpi::PhysicalPosition;
        use winit::event::MouseScrollDelta;

        // One LineDelta notch at default mult (1.0) = 3 lines; doubles at 2x.
        let one = MouseScrollDelta::LineDelta(0.0, 1.0);
        assert_eq!(wheel_lines(&one, 1.0), 3);
        assert_eq!(wheel_lines(&one, 2.0), 6);
        // Negative notch → negative lines (scroll the other way).
        let down = MouseScrollDelta::LineDelta(0.0, -2.0);
        assert_eq!(wheel_lines(&down, 1.0), -6);
        // PixelDelta: ~3 lines per ~60 px notch (60/20=3) at default mult.
        let pix = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 60.0));
        assert_eq!(wheel_lines(&pix, 1.0), 3);
        // Multiplier clamps at 0 to avoid backwards-scroll on bad config.
        assert_eq!(wheel_lines(&one, -5.0), 0);
    }

    /// Cycle 609 drift guard: pin the `copy_clipboard_decision`
    /// policy. The four cases enumerate every (selection ×
    /// smart_copy) combination so a future refactor that inverts
    /// the smart_copy semantics or drops the empty-clobber case
    /// fires this test before the regression ships.
    #[test]
    fn copy_clipboard_decision_smart_vs_clobber() {
        use super::copy_clipboard_decision;
        // Selection present + smart_copy = true: copy the selection.
        assert_eq!(
            copy_clipboard_decision(Some("hello"), true).as_deref(),
            Some("hello")
        );
        // Selection present + smart_copy = false: still copy
        // (smart_copy only affects the no-selection branch).
        assert_eq!(
            copy_clipboard_decision(Some("hello"), false).as_deref(),
            Some("hello")
        );
        // No selection + smart_copy = true (kettle default + Terminator
        // default): preserve existing clipboard — return None so the
        // caller skips the set_text call.
        assert_eq!(copy_clipboard_decision(None, true), None);
        // No selection + smart_copy = false: clobber clipboard with
        // empty string. Terminator's deliberate-UX-choice mode.
        assert_eq!(copy_clipboard_decision(None, false).as_deref(), Some(""));
    }

    #[test]
    fn putty_paste_source_honors_clipboard_toggle() {
        use super::{PasteSource, putty_paste_source};
        assert_eq!(putty_paste_source(false), PasteSource::Primary);
        assert_eq!(putty_paste_source(true), PasteSource::Clipboard);
    }

    #[test]
    fn mouse_paste_routes_primary_and_putty_source() {
        let src = include_str!("app.rs");
        assert!(
            src.contains("if bcode == 1 && !self.cfg.disable_mouse_paste {\n                    self.paste_primary(ws);"),
            "middle-click paste must use paste_primary so X11 PRIMARY works"
        );
        assert!(
            src.contains("match putty_paste_source(self.cfg.putty_paste_style_source_clipboard)")
                && src.contains("PasteSource::Clipboard => self.paste_clipboard(ws)")
                && src.contains("PasteSource::Primary => self.paste_primary(ws)"),
            "PuTTY right-click paste must honor putty_paste_style_source_clipboard"
        );
    }

    /// Cycle 604 drift guard: pin the `should_zoom_font` policy. The
    /// in-wheel-handler call relies on this returning Some only when
    /// Ctrl is held AND the user hasn't opted out via
    /// `disable-mousewheel-zoom`. If a future refactor accidentally
    /// inverts the disable check or drops the Ctrl gate, the test
    /// fires before the regression ships.
    #[test]
    fn should_zoom_font_gates_on_ctrl_and_disable_flag() {
        use super::should_zoom_font;
        // Ctrl + scroll up + not disabled → +1 (grow font).
        assert_eq!(should_zoom_font(true, 3, false), Some(1));
        // Ctrl + scroll down + not disabled → -1 (shrink font).
        assert_eq!(should_zoom_font(true, -3, false), Some(-1));
        // Ctrl + scroll up + DISABLED → None (opt-out honored).
        assert_eq!(should_zoom_font(true, 3, true), None);
        // No Ctrl → None even when not disabled (regular scrollback
        // scroll path takes over).
        assert_eq!(should_zoom_font(false, 3, false), None);
        // No Ctrl AND disabled → still None.
        assert_eq!(should_zoom_font(false, 3, true), None);
        // Zero lines → None (a stale wheel event with no actual
        // motion shouldn't trigger a font change).
        assert_eq!(should_zoom_font(true, 0, false), None);
    }

    #[test]
    fn selection_autoscroll_rate_scales_with_overshoot() {
        use super::selection_autoscroll_lines;
        // Inside the pane → no scroll.
        assert_eq!(selection_autoscroll_lines(150.0, 100.0, 200.0), 0);
        assert_eq!(selection_autoscroll_lines(100.0, 100.0, 200.0), 0);
        assert_eq!(selection_autoscroll_lines(200.0, 100.0, 200.0), 0);
        // Just past the top → 1 line/frame up into history (positive).
        assert_eq!(selection_autoscroll_lines(95.0, 100.0, 200.0), 1);
        // Moderate overshoot (10..40 px) → 2 lines/frame.
        assert_eq!(selection_autoscroll_lines(80.0, 100.0, 200.0), 2);
        // Big overshoot (≥40 px) → 3 lines/frame.
        assert_eq!(selection_autoscroll_lines(50.0, 100.0, 200.0), 3);
        // Past the bottom → negative (toward the present).
        assert_eq!(selection_autoscroll_lines(205.0, 100.0, 200.0), -1);
        assert_eq!(selection_autoscroll_lines(220.0, 100.0, 200.0), -2);
        assert_eq!(selection_autoscroll_lines(280.0, 100.0, 200.0), -3);
    }

    #[test]
    fn pane_titlebar_hit_geometry() {
        use super::pane_titlebar_hit;
        // 800x600 surface with 2 panes side-by-side, 24px titlebar.
        // Left pane:  (0, 0, 400, 600)
        // Right pane: (400, 0, 400, 600)
        let rects = vec![
            (1u64, (0.0_f32, 0.0_f32, 400.0_f32, 600.0_f32)),
            (2u64, (400.0_f32, 0.0_f32, 400.0_f32, 600.0_f32)),
        ];
        // Top titlebar: y-band [1, 25).
        // Click inside left pane's titlebar:
        assert_eq!(pane_titlebar_hit(50.0, 12.0, &rects, false, 24.0), Some(1));
        // Click inside right pane's titlebar:
        assert_eq!(pane_titlebar_hit(500.0, 12.0, &rects, false, 24.0), Some(2));
        // Click below the titlebar (in cell content) → no hit.
        assert_eq!(pane_titlebar_hit(50.0, 100.0, &rects, false, 24.0), None);
        // Click ABOVE the bar (y < 1) → no hit.
        assert_eq!(pane_titlebar_hit(50.0, 0.5, &rects, false, 24.0), None);
        // Bottom titlebar: y-band [576, 600).
        assert_eq!(pane_titlebar_hit(50.0, 580.0, &rects, true, 24.0), Some(1));
        assert_eq!(pane_titlebar_hit(500.0, 580.0, &rects, true, 24.0), Some(2));
        // Click at top-of-pane with title_at_bottom=true → no hit.
        assert_eq!(pane_titlebar_hit(50.0, 12.0, &rects, true, 24.0), None);
        // Click in cell content with title_at_bottom=true → no hit.
        assert_eq!(pane_titlebar_hit(50.0, 300.0, &rects, true, 24.0), None);
    }

    #[test]
    fn gpu_init_watchdog_fires_only_on_a_real_hang() {
        use super::gpu_init_timed_out;
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;
        // Cycle 812 drift guard. `done` never set within the window → reports a
        // hang (true), so the real watchdog would log + exit.
        let stuck = AtomicBool::new(false);
        assert!(gpu_init_timed_out(
            &stuck,
            Duration::from_millis(60),
            Duration::from_millis(10)
        ));
        // `done` already set (init finished) → stands down immediately (false),
        // even with a long nominal timeout, so a fast init/failure is never
        // killed.
        let finished = AtomicBool::new(true);
        assert!(!gpu_init_timed_out(
            &finished,
            Duration::from_secs(30),
            Duration::from_millis(10)
        ));
    }

    #[test]
    fn viewport_point_to_grid_applies_display_offset() {
        use super::viewport_point_to_grid;
        use kettle_core::{Column, Line, Point};
        // At the bottom (display_offset 0): viewport == grid-absolute.
        let p = viewport_point_to_grid(Point::new(Line(5), Column(3)), 0);
        assert_eq!(p.line, Line(5));
        assert_eq!(p.column, Column(3));
        // Scrolled back by 3: absolute = viewport − offset (the R1 bug was the
        // missing subtraction, so a scrolled selection read the wrong row).
        assert_eq!(
            viewport_point_to_grid(Point::new(Line(5), Column(0)), 3).line,
            Line(2)
        );
        // Scrolled far enough that the visible row maps into history (negative
        // absolute line — alacritty's grid indexes history with negative lines).
        assert_eq!(
            viewport_point_to_grid(Point::new(Line(2), Column(0)), 10).line,
            Line(-8)
        );
    }

    #[test]
    fn output_paint_coalesces_within_frame_budget() {
        use super::{OUTPUT_FRAME_BUDGET, should_defer_output_paint};
        use std::time::{Duration, Instant};
        let now = Instant::now();
        // No prior paint → paint immediately (never defer the first frame).
        assert!(!should_defer_output_paint(now, None, OUTPUT_FRAME_BUDGET));
        // Painted just now → defer so a same-burst wakeup coalesces.
        assert!(should_defer_output_paint(
            now,
            Some(now),
            OUTPUT_FRAME_BUDGET
        ));
        // A frame's worth later → the budget elapsed, paint immediately.
        let later = now + OUTPUT_FRAME_BUDGET + Duration::from_millis(5);
        assert!(!should_defer_output_paint(
            later,
            Some(now),
            OUTPUT_FRAME_BUDGET
        ));
    }

    /// v2.21.1 (throughput): the output-paint budget must GROW under a sustained
    /// flood so fewer per-frame snapshots are taken under the `Term` lock the
    /// PTY reader needs — but a brief burst must stay at the responsive 60 fps
    /// budget so keystroke echo and short bursts don't get throttled.
    #[test]
    fn effective_output_budget_grows_under_sustained_flood() {
        use super::{OUTPUT_FRAME_BUDGET, effective_output_budget};
        use std::time::Duration;
        // A brief burst stays at the responsive 60 fps budget.
        assert_eq!(effective_output_budget(0), OUTPUT_FRAME_BUDGET);
        assert_eq!(effective_output_budget(3), OUTPUT_FRAME_BUDGET);
        // A sustained flood steps down to ~30 fps, then ~20 fps.
        assert_eq!(effective_output_budget(4), Duration::from_millis(33));
        assert_eq!(effective_output_budget(15), Duration::from_millis(33));
        assert_eq!(effective_output_budget(16), Duration::from_millis(50));
        assert_eq!(effective_output_budget(10_000), Duration::from_millis(50));
        // Monotonic non-decreasing: deeper flood never paints MORE often.
        let mut prev = Duration::ZERO;
        for n in 0..40u32 {
            let b = effective_output_budget(n);
            assert!(b >= prev, "budget must not shrink as flood deepens");
            prev = b;
        }
        // The throttle is bounded (never starves the settled frame indefinitely).
        assert!(effective_output_budget(u32::MAX) <= Duration::from_millis(50));
    }

    /// Cycle 912 (audit): the `about_to_wait` coalescer must FLUSH a due deferred
    /// frame (clear `coalescing_paint`) BEFORE it computes the WaitUntil clamp,
    /// or a still-pending paint could re-schedule a ~1 ms wake every tick
    /// (busy-spin). A behavioral test needs a live winit event loop; pin the
    /// ordering at the source level (the `modal_discipline_guard` pattern).
    #[test]
    fn output_coalescer_flushes_before_the_wait_clamp() {
        let src = include_str!("app.rs").replace("\r\n", "\n");
        let start = src
            .find("fn about_to_wait_inner(")
            .expect("about_to_wait_inner present");
        let rest = &src[start..];
        // Bound the scan to the about_to_wait_inner body (it is the last method
        // in the impl, so the next column-0 `}` closes the impl).
        let end = rest.find("\n}\n").map(|i| i + 2).unwrap_or(rest.len());
        let body = &rest[..end];
        let flush = body
            .find("ws.coalescing_paint = false")
            .expect("the coalesce_due flush must exist in about_to_wait");
        let clamp = body
            .find(".max(1)")
            .expect("the wait-ms clamp must exist in about_to_wait");
        assert!(
            flush < clamp,
            "coalescing_paint flush must precede the .max(1) wait clamp (anti-busy-spin)"
        );
    }

    /// Idle blink must wake at the configured half-period deadline, not at a
    /// fixed sub-interval. The old 120 ms poll requested four mostly-no-op
    /// redraws before each default 530 ms blink toggle.
    #[test]
    fn cursor_blink_waits_until_the_actual_deadline() {
        let src = include_str!("app.rs").replace("\r\n", "\n");
        let start = src
            .find("fn about_to_wait_inner(")
            .expect("about_to_wait_inner present");
        let rest = &src[start..];
        let end = rest.find("\n}\n").map(|i| i + 2).unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(
            body.contains("let blink_due = blink_active && blink_elapsed >= blink_interval"),
            "about_to_wait must request a blink redraw only when the half-period elapsed"
        );
        assert!(
            body.contains("blink_interval.saturating_sub(blink_elapsed)")
                && !body.contains("Some(120)"),
            "blink wait time must be the remaining configured interval, not a fixed poll"
        );
    }

    #[test]
    fn titlebar_inset_realigns_hit_test_and_grid() {
        use super::{grid_dims_px, px_to_cell};
        let rect = (10.0, 20.0, 800.0, 600.0);
        let (cw, ch, pad) = (8.0_f32, 16.0_f32, 4.0_f32);
        let tb = ch + 6.0; // renderer's pane_titlebar_h

        // No titlebar (single-pane tab): content origin = ry + pad = 24.
        let (_, line0, _) = px_to_cell(100.0, 24.0, rect, (cw, ch), (pad, pad), 0.0);
        assert_eq!(line0, 0, "py at content origin → row 0 (no titlebar)");

        // With a titlebar: content origin shifts down by `tb` to 24 + 22 = 46.
        let (_, line_origin, _) = px_to_cell(100.0, 24.0 + tb, rect, (cw, ch), (pad, pad), tb);
        assert_eq!(
            line_origin, 0,
            "py at the titlebar'd content origin → row 0"
        );
        // One cell below the titlebar'd origin → row 1 (was ~row 2 before the
        // fix: that off-by-one is exactly what the audit caught).
        let (_, line1, _) = px_to_cell(100.0, 24.0 + tb + ch, rect, (cw, ch), (pad, pad), tb);
        assert_eq!(line1, 1);
        // A click up in the titlebar band clamps to row 0 (never negative).
        let (_, clamped, _) = px_to_cell(100.0, 24.0, rect, (cw, ch), (pad, pad), tb);
        assert_eq!(clamped, 0);

        // grid_of: the titlebar steals height, so fewer rows are reported.
        let (cols_no, rows_no) = grid_dims_px((800.0, 600.0), (cw, ch), (pad, pad), 0.0);
        let (cols_tb, rows_tb) = grid_dims_px((800.0, 600.0), (cw, ch), (pad, pad), tb);
        assert_eq!(cols_no, cols_tb, "titlebar doesn't change column count");
        assert!(
            rows_tb < rows_no,
            "titlebar must reduce reported rows ({rows_tb} < {rows_no})"
        );
    }

    #[test]
    fn px_to_cell_reports_sub_cell_side() {
        use super::px_to_cell;
        use kettle_core::Side;
        let rect = (10.0, 20.0, 800.0, 600.0);
        let (cw, ch, pad) = (8.0_f32, 16.0_f32, 4.0_f32);
        // Content origin x = rx + pad = 14.0; each cell is `cw` wide.
        let ox = 10.0 + pad; // 14.0
        let y = 30.0; // any in-grid row

        // Left half of cell 0 → Left.
        let (c0, _, s0) = px_to_cell(ox + 1.0, y, rect, (cw, ch), (pad, pad), 0.0);
        assert_eq!(c0, 0);
        assert_eq!(s0, Side::Left, "left half of the cell → Left");

        // Right half of cell 0 → Right.
        let (_, _, s1) = px_to_cell(ox + 6.0, y, rect, (cw, ch), (pad, pad), 0.0);
        assert_eq!(s1, Side::Right, "right half of the cell → Right");

        // The exact midpoint counts as the right half (matches alacritty's
        // `< half ⇒ Left` boundary — anything ≥ half is Right).
        let (_, _, s2) = px_to_cell(ox + cw / 2.0, y, rect, (cw, ch), (pad, pad), 0.0);
        assert_eq!(s2, Side::Right, "exact midpoint → Right");

        // The side is a within-cell property: the same sub-cell offset in a later
        // column yields the same side (no drift across the row).
        let (c3, _, s3) = px_to_cell(ox + cw * 5.0 + 1.0, y, rect, (cw, ch), (pad, pad), 0.0);
        assert_eq!(c3, 5);
        assert_eq!(s3, Side::Left, "side is independent of the column index");

        // A pointer in the LEFT PADDING (left of column 0) clamps to col 0 AND
        // Side::Left, so a drag starting there still INCLUDES the first cell.
        // (Audit v2.25.0: deriving the side from the raw negative offset via
        // `rem_euclid` wrapped it into the right half and dropped column 0.)
        let (c4, _, s4) = px_to_cell(ox - 3.0, y, rect, (cw, ch), (pad, pad), 0.0);
        assert_eq!(c4, 0, "left of origin clamps to column 0");
        assert_eq!(
            s4,
            Side::Left,
            "left of origin → Side::Left (first cell kept)"
        );
    }

    #[test]
    fn keybind_chord_is_safe_rejects_modless_text_keys() {
        use super::keybind_chord_is_safe;
        use kettle_config::{Key as KKey, Mods};
        // Modifier-less text/essential keys are REFUSED (the soft-brick).
        assert!(!keybind_chord_is_safe(Mods::empty(), KKey::Char('a')));
        assert!(!keybind_chord_is_safe(Mods::empty(), KKey::Enter));
        assert!(!keybind_chord_is_safe(Mods::empty(), KKey::Tab));
        assert!(!keybind_chord_is_safe(Mods::empty(), KKey::Up));
        // A modifier-less F-key is fine (produces no text — F1=help etc.).
        assert!(keybind_chord_is_safe(Mods::empty(), KKey::F(1)));
        // Any modifier makes the chord safe.
        assert!(keybind_chord_is_safe(Mods::CTRL, KKey::Char('a')));
        assert!(keybind_chord_is_safe(Mods::ALT, KKey::Enter));
        assert!(keybind_chord_is_safe(Mods::SHIFT, KKey::Char('z')));
    }

    #[test]
    fn cwd_is_local_rejects_unc_and_traversal() {
        use super::cwd_is_local;
        // Cycle 816 drift guard. Ordinary absolute cwds (the OSC 7 path form,
        // which uses forward slashes even on Windows: /C:/Users/x) are fine.
        assert!(cwd_is_local("/home/user"));
        assert!(cwd_is_local("/C:/Users/me/Repos"));
        // UNC / authority forms and traversal are refused (the SMB/NTLM vector).
        assert!(!cwd_is_local("//evil.host/share"));
        assert!(!cwd_is_local("\\\\evil\\share"));
        assert!(!cwd_is_local("/x/../../etc/passwd"));
        assert!(!cwd_is_local(""));
    }

    #[test]
    fn extra_mouse_sgr_maps_side_buttons() {
        use super::extra_mouse_sgr;
        use winit::event::MouseButton;
        // Cycle 810 drift guard. Back = XBUTTON1 = SGR 128, Forward =
        // XBUTTON2 = SGR 129; the primary three + anything else are handled
        // locally (or dropped), so they return None here.
        assert_eq!(extra_mouse_sgr(MouseButton::Back), Some(128));
        assert_eq!(extra_mouse_sgr(MouseButton::Forward), Some(129));
        assert_eq!(extra_mouse_sgr(MouseButton::Left), None);
        assert_eq!(extra_mouse_sgr(MouseButton::Middle), None);
        assert_eq!(extra_mouse_sgr(MouseButton::Right), None);
        assert_eq!(extra_mouse_sgr(MouseButton::Other(5)), None);
    }

    #[test]
    fn cursor_in_tab_bar_band_geometry() {
        use super::{TabBarPos, cursor_in_tab_bar_band};
        // 600 px window, 24 px tab bar. Top: [0, 24).
        assert!(cursor_in_tab_bar_band(0.0, 24.0, 600.0, TabBarPos::Top));
        assert!(cursor_in_tab_bar_band(23.0, 24.0, 600.0, TabBarPos::Top));
        assert!(!cursor_in_tab_bar_band(24.0, 24.0, 600.0, TabBarPos::Top));
        assert!(!cursor_in_tab_bar_band(300.0, 24.0, 600.0, TabBarPos::Top));
        // Bottom: [576, 600].
        assert!(cursor_in_tab_bar_band(
            576.0,
            24.0,
            600.0,
            TabBarPos::Bottom
        ));
        assert!(cursor_in_tab_bar_band(
            600.0,
            24.0,
            600.0,
            TabBarPos::Bottom
        ));
        assert!(!cursor_in_tab_bar_band(
            575.0,
            24.0,
            600.0,
            TabBarPos::Bottom
        ));
        assert!(!cursor_in_tab_bar_band(0.0, 24.0, 600.0, TabBarPos::Bottom));
        // Hidden tab bar (`tab-bar = off` or single-tab `auto`) → always
        // out of band so the wheel falls through to scrollback.
        assert!(!cursor_in_tab_bar_band(0.0, 0.0, 600.0, TabBarPos::Top));
        assert!(!cursor_in_tab_bar_band(0.0, 0.0, 600.0, TabBarPos::Bottom));
    }

    #[test]
    fn cursor_in_status_bar_band_geometry() {
        // Cycle 321 drift guard for the cycle-320 status-bar
        // cursor-icon fix. Same shape as the cycle-264 tab-bar
        // drift guard above. Pins:
        //   - Off mode → always false (status bar invisible).
        //   - Top mode → [0, bar_h).
        //   - Bottom mode → [surface - bar_h, surface].
        //   - bar_h == 0 → false regardless of mode (cycle-296
        //     `status_bar_h()` returns 0 on the Off branch even
        //     before the mode check; this is the same defensive
        //     contract).
        use super::cursor_in_status_bar_band;
        use kettle_config::StatusBarMode;
        // Off → always false.
        assert!(!cursor_in_status_bar_band(
            0.0,
            22.0,
            600.0,
            StatusBarMode::Off
        ));
        assert!(!cursor_in_status_bar_band(
            300.0,
            22.0,
            600.0,
            StatusBarMode::Off
        ));
        // Top: [0, 22).
        assert!(cursor_in_status_bar_band(
            0.0,
            22.0,
            600.0,
            StatusBarMode::Top
        ));
        assert!(cursor_in_status_bar_band(
            21.0,
            22.0,
            600.0,
            StatusBarMode::Top
        ));
        assert!(!cursor_in_status_bar_band(
            22.0,
            22.0,
            600.0,
            StatusBarMode::Top
        ));
        assert!(!cursor_in_status_bar_band(
            300.0,
            22.0,
            600.0,
            StatusBarMode::Top
        ));
        // Bottom: [578, 600].
        assert!(cursor_in_status_bar_band(
            578.0,
            22.0,
            600.0,
            StatusBarMode::Bottom
        ));
        assert!(cursor_in_status_bar_band(
            600.0,
            22.0,
            600.0,
            StatusBarMode::Bottom
        ));
        assert!(!cursor_in_status_bar_band(
            577.0,
            22.0,
            600.0,
            StatusBarMode::Bottom
        ));
        assert!(!cursor_in_status_bar_band(
            0.0,
            22.0,
            600.0,
            StatusBarMode::Bottom
        ));
        // bar_h == 0 → out of band regardless of mode.
        assert!(!cursor_in_status_bar_band(
            0.0,
            0.0,
            600.0,
            StatusBarMode::Top
        ));
        assert!(!cursor_in_status_bar_band(
            0.0,
            0.0,
            600.0,
            StatusBarMode::Bottom
        ));
    }

    #[test]
    fn chrome_cursor_icon_overrides_only_for_chrome() {
        use super::chrome_cursor_icon;
        use winit::window::CursorIcon;
        // Content area, no modals → caller picks (None means "use content
        // logic" — Text by default, Pointer over a Ctrl-held link).
        assert_eq!(chrome_cursor_icon(false, false), None);
        // Tab bar → Default arrow (clickable tabs are not selectable text).
        assert_eq!(chrome_cursor_icon(true, false), Some(CursorIcon::Default));
        // Modal up (search / palette / hints / SSH launcher) → Default arrow
        // regardless of where the pointer is, so the I-beam doesn't bleed
        // through onto the overlay.
        assert_eq!(chrome_cursor_icon(false, true), Some(CursorIcon::Default));
        // Both at once (modal opened while pointer happened to be over the
        // tab bar) → still Default.
        assert_eq!(chrome_cursor_icon(true, true), Some(CursorIcon::Default));
    }

    #[test]
    fn hovered_close_button_finds_only_the_close_rect_hits() {
        use super::hovered_close_button;
        use kettle_render::TabSeg;
        // Two tab segments side-by-side, each 100×24 with a trailing
        // 24-px close-button zone at x=76..100 (segment 0) and
        // x=176..200 (segment 1) — mirrors the cycle-241 tab_bar()
        // builder's `(x + seg_w - height)` formula.
        let segs = vec![
            TabSeg {
                idx: 0,
                rect: (0.0, 0.0, 100.0, 24.0),
                close: (76.0, 0.0, 24.0, 24.0),
                title: "one".into(),
                active: true,
                activity: kettle_render::TabActivity::Normal,
            },
            TabSeg {
                idx: 1,
                rect: (100.0, 0.0, 100.0, 24.0),
                close: (176.0, 0.0, 24.0, 24.0),
                title: "two".into(),
                active: false,
                activity: kettle_render::TabActivity::Normal,
            },
        ];
        // Cursor over title area of segment 0 → no close hit.
        assert_eq!(hovered_close_button(&segs, 20.0, 12.0), None);
        // Cursor over segment 0's close button → hit on idx 0.
        assert_eq!(hovered_close_button(&segs, 88.0, 12.0), Some(0));
        // Cursor over segment 1's close button → hit on idx 1.
        assert_eq!(hovered_close_button(&segs, 188.0, 12.0), Some(1));
        // Cursor outside the bar entirely → no hit.
        assert_eq!(hovered_close_button(&segs, 88.0, 100.0), None);
        // Edge: cursor exactly at the bar bottom (24.0) is outside
        // because `py < ry + rh` is strict.
        assert_eq!(hovered_close_button(&segs, 88.0, 24.0), None);
        // Empty bar (single-pane, tabs hidden) → no hit ever.
        assert_eq!(hovered_close_button(&[], 50.0, 10.0), None);
    }

    #[test]
    fn tab_drag_target_index_clamps_to_strip() {
        use super::tab_drag_target_index;
        // 3 tabs, 300-px strip → 100 px per segment. Cursor at 50 →
        // tab 0; 150 → tab 1; 250 → tab 2.
        assert_eq!(tab_drag_target_index(50.0, 3, 300.0), 0);
        assert_eq!(tab_drag_target_index(150.0, 3, 300.0), 1);
        assert_eq!(tab_drag_target_index(250.0, 3, 300.0), 2);
        // Right at the boundary: 100 → tab 1 (floor); 200 → tab 2.
        assert_eq!(tab_drag_target_index(100.0, 3, 300.0), 1);
        assert_eq!(tab_drag_target_index(200.0, 3, 300.0), 2);
        // Negative cursor (past the left edge) → clamps to 0.
        assert_eq!(tab_drag_target_index(-50.0, 3, 300.0), 0);
        // Past the right edge → clamps to last segment, not n.
        assert_eq!(tab_drag_target_index(900.0, 3, 300.0), 2);
        assert_eq!(tab_drag_target_index(f32::MAX, 3, 300.0), 2);
        // Empty bar or zero strip → 0 (defensive no-op).
        assert_eq!(tab_drag_target_index(50.0, 0, 300.0), 0);
        assert_eq!(tab_drag_target_index(50.0, 3, 0.0), 0);
    }

    /// v2.19.0 (tear-off UX): the tear decision is uniform hysteresis in
    /// every direction away from the tab band, for every `tab-bar-pos`.
    #[test]
    fn tear_threshold_crossed_per_orientation() {
        use super::{dist_to_rect, tear_threshold_crossed};
        // Top band (0,0,800,24), threshold 36 (= 1.5 × 24).
        let top = (0.0, 0.0, 800.0, 24.0);
        // Inside the band, anywhere along the strip: reorder, never tear.
        assert!(!tear_threshold_crossed(5.0, 12.0, top, 36.0));
        assert!(!tear_threshold_crossed(790.0, 12.0, top, 36.0));
        // Below the band but within the slop: still no tear.
        assert!(!tear_threshold_crossed(400.0, 59.0, top, 36.0));
        // Past the slop downward (into the terminal content): tear.
        assert!(tear_threshold_crossed(400.0, 60.0, top, 36.0));
        // Above the window (band edge == window edge): same hysteresis.
        assert!(!tear_threshold_crossed(400.0, -35.0, top, 36.0));
        assert!(tear_threshold_crossed(400.0, -36.0, top, 36.0));
        // Past the strip's right end horizontally: distance is measured
        // from the band edge, so the slop applies there too.
        assert!(!tear_threshold_crossed(820.0, 12.0, top, 36.0));
        assert!(tear_threshold_crossed(836.0, 12.0, top, 36.0));
        // Diagonal: Euclidean, not Manhattan — 30² + 30² > 36².
        assert!(tear_threshold_crossed(830.0, 54.0, top, 36.0));
        // Bottom band on a 600-tall surface.
        let bottom = (0.0, 576.0, 800.0, 24.0);
        assert!(!tear_threshold_crossed(400.0, 580.0, bottom, 36.0));
        assert!(tear_threshold_crossed(400.0, 540.0, bottom, 36.0));
        // Left vertical band (width = tab_bar_width 200): tearing happens
        // rightward past the slop.
        let left = (0.0, 0.0, 200.0, 600.0);
        assert!(!tear_threshold_crossed(100.0, 300.0, left, 36.0));
        assert!(!tear_threshold_crossed(235.0, 300.0, left, 36.0));
        assert!(tear_threshold_crossed(236.0, 300.0, left, 36.0));
        // Right vertical band: leftward.
        let right = (600.0, 0.0, 200.0, 600.0);
        assert!(tear_threshold_crossed(560.0, 300.0, right, 36.0));
        // A zero threshold can never tear (defensive: hidden bar).
        assert!(!tear_threshold_crossed(400.0, 500.0, top, 0.0));
        // dist_to_rect itself: inside = 0, outside = Euclidean.
        assert_eq!(dist_to_rect(10.0, 10.0, top), 0.0);
        assert_eq!(dist_to_rect(400.0, 124.0, top), 100.0);
        assert_eq!(dist_to_rect(803.0, 28.0, top), 5.0); // 3-4-5 corner
    }

    /// v2.19.0 (re-dock): docking inserts BETWEEN segments — n tabs have
    /// n+1 slots, decided by segment midpoints.
    #[test]
    fn dock_insertion_index_slots_between_segments() {
        use super::dock_insertion_index;
        // Three segments at mids 50 / 150 / 250.
        let mids = [50.0, 150.0, 250.0];
        assert_eq!(dock_insertion_index(&mids, 0.0), 0); // before first
        assert_eq!(dock_insertion_index(&mids, 49.0), 0);
        assert_eq!(dock_insertion_index(&mids, 51.0), 1); // past mid 0
        assert_eq!(dock_insertion_index(&mids, 149.0), 1);
        assert_eq!(dock_insertion_index(&mids, 200.0), 2);
        assert_eq!(dock_insertion_index(&mids, 251.0), 3); // append
        assert_eq!(dock_insertion_index(&mids, f32::MAX), 3);
        // No segments (hidden-bar fallback handles this elsewhere): 0.
        assert_eq!(dock_insertion_index(&[], 100.0), 0);
    }

    /// v2.19.0 drift guard: the tear-at-threshold call sites and the native
    /// drag handoff stay wired the way the platform analysis verified them.
    /// Source-level needles (the cycle-916 concat style) because the flow
    /// spans winit's event dispatch and can't run headless.
    #[test]
    fn tear_off_flow_stays_wired() {
        let src = include_str!("app.rs");
        // 1. The CursorMoved FSM block tears at the threshold (Chromium
        //    model), not at release.
        assert!(
            src.contains("if self.maybe_tear_off(ws, event_loop) {"),
            "CursorMoved must drive the threshold tear"
        );
        // 2. The torn window inherits the source size and is positioned by
        //    the grab offset (open_window's size override).
        assert!(
            src.contains("self.open_window(event_loop, WindowOpen::AdoptTab(dt), pos, Some(size))"),
            "the tear must open the torn window at the source size"
        );
        // 3. The native handoff happens right after the insert.
        assert!(
            src.contains(".map(|w| match w.drag_window() {"),
            "the torn window must be handed to the OS move loop"
        );
        // 4. The at-release tear is Wayland-only now.
        assert!(
            src.contains(
                "if self.cfg.detachable_tabs\n                        && dropped_outside\n                        && wayland\n                        && ws.mux.tabs.len() > 1\n                    {"
            ),
            "the Released-arm tear must be gated to Wayland and detachable-tabs"
        );
        // 5. The drop commits the latched dock from the left-release —
        //    GATED on the primary button being physically up (cycle-943
        //    HIGH: Windows' synthesized release fires for an Esc-CANCELLED
        //    modal loop too, with the button still held; committing there
        //    performed the exact merge the user was cancelling).
        assert!(
            src.contains("let commit = !primary_button_physically_held();"),
            "the release-drop must distinguish Esc-cancel via physical button state"
        );
        // 6. The X11/macOS pointer-event commits revalidate the latch
        //    against the torn window's FINAL position (a WM-cancelled move
        //    snapped it back to its origin, off the band).
        assert!(
            src.contains("let commit = self.revalidate_dock_latch(ws);"),
            "heuristic commits must revalidate the latch"
        );
        // 7. Manual-follow listens only to the capture holder (cycle-943:
        //    stale tracking must not hijack every window's cursor stream).
        assert!(
            src.contains("&& td.carrier == ws.seq\n                {"),
            "manual-follow must be carrier-gated"
        );
        // 8. A window that dies mid-drag takes its tracking with it.
        assert!(
            src.contains(".is_some_and(|t| t.seq == seq || t.carrier == seq)"),
            "finish_window_dispatch must abandon tracking for a dying torn/carrier window"
        );
    }

    /// Cycle 959 drift guard: the Terminator-parity detachable-tabs setting is
    /// a real runtime switch, not just a parsed compatibility key.
    #[test]
    fn detachable_tabs_config_gates_all_detach_paths() {
        let src = include_str!("app.rs");
        assert!(
            src.contains("if !self.cfg.detachable_tabs {\n                    log::info!(\"move_tab_to_new_window ignored because detachable-tabs = false\");"),
            "keyboard/palette move_tab_to_new_window must honor detachable-tabs = false"
        );
        assert!(
            src.contains(
                "fn maybe_tear_off(&mut self, ws: &mut WindowState, event_loop: &ActiveEventLoop) -> bool {\n        if !self.cfg.detachable_tabs {"
            ),
            "mouse threshold tear-off must honor detachable-tabs = false"
        );
        assert!(
            src.contains("if self.cfg.detachable_tabs {\n                                    ws.detach_drag ="),
            "tab press must not arm the cross-window detach FSM when disabled"
        );
        assert!(
            src.contains(
                "if self.cfg.detachable_tabs\n                        && dropped_outside\n                        && wayland\n                        && ws.mux.tabs.len() > 1\n                    {"
            ),
            "Wayland release-only tear-off must honor detachable-tabs = false"
        );
    }

    /// Cycle 821 drift guard: the drag strip and `tab_bar()`'s segment layout
    /// share this width, so a tab can be dragged all the way to the last slot.
    #[test]
    fn tab_segment_strip_width_excludes_both_buttons() {
        use super::tab_segment_strip_width;
        // plus_w = arrow_w = height = 24, surface 800 → strip excludes BOTH
        // buttons (the `▾ +` pair), 800 - 48 = 752 (was 776 before the fix).
        assert_eq!(tab_segment_strip_width(800.0, 24.0, 24.0), 752.0);
        // No dropdown (vertical bar): arrow_w = 0 → only `+` excluded.
        assert_eq!(tab_segment_strip_width(800.0, 24.0, 0.0), 776.0);
        // Degenerate narrow bar floors at plus_w so `+` always has room.
        assert_eq!(tab_segment_strip_width(20.0, 24.0, 24.0), 24.0);
    }

    #[test]
    fn next_context_menu_highlight_skips_separators_and_disabled() {
        use super::{ContextMenuItem, next_context_menu_highlight};
        use kettle_config::Action;
        // Layout: [Copy(disabled), Paste, Separator, SplitRight,
        //          ClosePane(disabled), Separator, NewTab]
        // Indices:  0               1      2          3
        //           4                       5          6
        // Enabled-only walk should land 1 → 3 → 6 → 1 (wrap).
        let items = vec![
            ContextMenuItem::Item {
                label: "Copy",
                action: Action::Copy,
                enabled: false,
            },
            ContextMenuItem::Item {
                label: "Paste",
                action: Action::Paste,
                enabled: true,
            },
            ContextMenuItem::Separator,
            ContextMenuItem::Item {
                label: "Split Right",
                action: Action::SplitRight,
                enabled: true,
            },
            ContextMenuItem::Item {
                label: "Close Pane",
                action: Action::ClosePane,
                enabled: false,
            },
            ContextMenuItem::Separator,
            ContextMenuItem::Item {
                label: "New Tab",
                action: Action::NewTab,
                enabled: true,
            },
        ];
        // ↓ from Paste (1) skips Separator(2) → SplitRight(3).
        assert_eq!(next_context_menu_highlight(&items, 1, 1), 3);
        // ↓ from SplitRight(3) skips disabled ClosePane(4) + Separator
        // (5) → NewTab(6).
        assert_eq!(next_context_menu_highlight(&items, 3, 1), 6);
        // ↓ from NewTab(6) wraps past disabled Copy(0) → Paste(1).
        assert_eq!(next_context_menu_highlight(&items, 6, 1), 1);
        // ↑ from Paste(1) wraps past disabled Copy(0) → NewTab(6).
        assert_eq!(next_context_menu_highlight(&items, 1, -1), 6);
        // No enabled items at all — return `current` unchanged so the
        // caller doesn't crash. (Defensive — caller shouldn't open an
        // empty menu, but the guard exists.)
        let all_disabled = vec![ContextMenuItem::Separator];
        assert_eq!(next_context_menu_highlight(&all_disabled, 0, 1), 0);
    }

    /// Cycle 889/890 (audit): the shared row→click mapper must recognise
    /// EVERY dispatchable row type, so the keyboard Enter / Space +
    /// mnemonic paths reach the same set of rows the mouse hit-test does.
    /// Before the fix, Enter handled only `Item`, and the mnemonic path
    /// handled only Item / Submenu / theme / profile — Lua items, config
    /// commands and the new-tab ▾ dropdown were keyboard dead-ends.
    #[test]
    fn item_to_click_maps_every_dispatchable_row_type() {
        use super::{ContextMenuClick, ContextMenuItem, item_to_click};
        use kettle_config::Action;

        // Enabled Item / DynamicItem → Action.
        assert!(matches!(
            item_to_click(
                &ContextMenuItem::Item {
                    label: "Copy",
                    action: Action::Copy,
                    enabled: true
                },
                0
            ),
            Some(ContextMenuClick::Action(_))
        ));
        assert!(matches!(
            item_to_click(
                &ContextMenuItem::DynamicItem {
                    label: "Bold".into(),
                    action: Action::Copy,
                    enabled: true
                },
                0
            ),
            Some(ContextMenuClick::Action(_))
        ));
        // Submenu → DrillIntoSubmenu(idx) — carries the row index so the
        // dispatcher knows which level to descend into.
        assert!(matches!(
            item_to_click(
                &ContextMenuItem::Submenu {
                    label: "Theme".into(),
                    items: vec![]
                },
                7
            ),
            Some(ContextMenuClick::DrillIntoSubmenu(7))
        ));
        // Lua item → LuaMenuItem (was a keyboard dead-end before 890).
        assert!(matches!(
            item_to_click(
                &ContextMenuItem::LuaItem {
                    label: "Plugin".into(),
                    lua_idx: 3
                },
                0
            ),
            Some(ContextMenuClick::LuaMenuItem(3))
        ));
        // Config command → ConfigCommand (was a keyboard dead-end).
        assert!(matches!(
            item_to_click(
                &ContextMenuItem::ConfigItem {
                    label: "Clear".into(),
                    command: "clear".into()
                },
                0
            ),
            Some(ContextMenuClick::ConfigCommand(_))
        ));
        // Theme / profile choices → SetTheme / SetProfile.
        assert!(matches!(
            item_to_click(
                &ContextMenuItem::ThemeChoice {
                    label: "Dracula".into(),
                    theme: "Dracula".into()
                },
                0
            ),
            Some(ContextMenuClick::SetTheme(_))
        ));
        assert!(matches!(
            item_to_click(
                &ContextMenuItem::ProfileChoice {
                    label: "work".into(),
                    profile: "work".into()
                },
                0
            ),
            Some(ContextMenuClick::SetProfile(_))
        ));
        // New-tab ▾ shell choice → NewTabWithArgv (was a keyboard dead-end).
        assert!(matches!(
            item_to_click(
                &ContextMenuItem::NewTabShell {
                    label: "bash".into(),
                    argv: vec!["bash".into()]
                },
                0
            ),
            Some(ContextMenuClick::NewTabWithArgv(_))
        ));
        // Cycle 941: URL-aware rows → Url { copy } carrying the captured
        // address, for both the Open and Copy flavors.
        assert!(matches!(
            item_to_click(
                &ContextMenuItem::UrlItem {
                    label: "Open Link",
                    url: "https://example.com".into(),
                    copy: false
                },
                0
            ),
            Some(ContextMenuClick::Url { copy: false, .. })
        ));
        assert!(matches!(
            item_to_click(
                &ContextMenuItem::UrlItem {
                    label: "Copy Link Address",
                    url: "https://example.com".into(),
                    copy: true
                },
                0
            ),
            Some(ContextMenuClick::Url { copy: true, .. })
        ));
        // Non-dispatchable rows → None (disabled item + separator).
        assert!(
            item_to_click(
                &ContextMenuItem::Item {
                    label: "Copy",
                    action: Action::Copy,
                    enabled: false
                },
                0
            )
            .is_none()
        );
        assert!(item_to_click(&ContextMenuItem::Separator, 0).is_none());
    }

    /// Cycle 890 (audit): theme + profile choice leaves must be keyboard-
    /// navigable, otherwise drilling into a Theme ▸ / Profile ▸ submenu
    /// leaves ↑/↓ unable to land on any row.
    #[test]
    fn theme_and_profile_choices_are_keyboard_navigable() {
        use super::{ContextMenuItem, item_is_dispatchable};
        assert!(item_is_dispatchable(&ContextMenuItem::ThemeChoice {
            label: "Nord".into(),
            theme: "Nord".into()
        }));
        assert!(item_is_dispatchable(&ContextMenuItem::ProfileChoice {
            label: "work".into(),
            profile: "work".into()
        }));
    }

    #[test]
    fn clamp_context_menu_anchor_keeps_panel_on_screen() {
        use super::clamp_context_menu_anchor;
        // 800x600 surface, 200x300 panel — anchors inside the safe
        // region pass through unchanged.
        assert_eq!(
            clamp_context_menu_anchor((100.0, 100.0), (200.0, 300.0), (800.0, 600.0)),
            (100.0, 100.0)
        );
        // Right-click near the bottom-right corner gets clamped so the
        // panel still fits with the 4-px margin (max_x = 800 - 200 - 4
        // = 596, max_y = 600 - 300 - 4 = 296).
        assert_eq!(
            clamp_context_menu_anchor((780.0, 580.0), (200.0, 300.0), (800.0, 600.0)),
            (596.0, 296.0)
        );
        // Right-click in the top-left corner clamps up against the
        // 4-px screen-edge margin so the panel doesn't glue to the
        // bezel.
        assert_eq!(
            clamp_context_menu_anchor((0.0, 0.0), (200.0, 300.0), (800.0, 600.0)),
            (4.0, 4.0)
        );
        // Pathological: panel larger than the surface — anchor clamps
        // to the 4-px margin without producing a NaN / negative.
        let (x, y) = clamp_context_menu_anchor((100.0, 100.0), (2000.0, 2000.0), (800.0, 600.0));
        assert!(x >= 4.0 && y >= 4.0);
    }

    #[test]
    fn tab_close_hover_icon_overrides_chrome_default() {
        use super::{chrome_cursor_icon, tab_close_hover_icon};
        use winit::window::CursorIcon;
        // Not over a close button → no override; chrome decision wins.
        assert_eq!(tab_close_hover_icon(false), None);
        let chrome = chrome_cursor_icon(true, false);
        // Hover a close ✕ → Pointer (the browser-tab convention).
        assert_eq!(tab_close_hover_icon(true), Some(CursorIcon::Pointer));
        // Compose: close-hover wins over the chrome Default — once
        // the cursor lands on a clickable affordance we want the
        // hand, not the arrow.
        assert_eq!(
            tab_close_hover_icon(true).or(chrome),
            Some(CursorIcon::Pointer)
        );
    }

    #[test]
    fn clamp_osc52_bounds_and_keeps_char_boundary() {
        use super::clamp_osc52;
        assert_eq!(clamp_osc52("hello", 1024), "hello"); // under cap → as-is
        assert_eq!(clamp_osc52("abcdef", 3), "abc"); // truncated
        // Never splits a multibyte char: "é" is 2 bytes; cap 1 → empty.
        assert_eq!(clamp_osc52("é", 1), "");
        assert_eq!(clamp_osc52("aé", 2), "a");
        assert_eq!(clamp_osc52("", 0), "");
        // Result always within the byte cap.
        let big = "x".repeat(5000);
        assert!(clamp_osc52(&big, 1000).len() <= 1000);
    }

    #[test]
    fn window_title_formats_and_falls_back() {
        use super::window_title;
        let dflt = "{title} — kettle";
        assert_eq!(
            window_title(dflt, "vim README.md", "", 1),
            "vim README.md — kettle"
        );
        assert_eq!(window_title(dflt, "  spaced  ", "", 1), "spaced — kettle");
        // Empty / placeholder titles with no cwd → just the app name (the
        // template never produces a stub like " — kettle").
        assert_eq!(window_title(dflt, "", "", 1), "kettle");
        assert_eq!(window_title(dflt, "   ", "", 1), "kettle");
        assert_eq!(window_title(dflt, "kettle", "", 1), "kettle");
        // Empty / placeholder title *with* cwd → cwd basename fills in
        // (cycle-89 tab-title fallback, same shape for the window title).
        assert_eq!(
            window_title(dflt, "", "/home/k/Repos/kettle", 1),
            "kettle — kettle",
            "cwd basename `kettle` fills the {{title}} slot pre-OSC 2"
        );
        assert_eq!(
            window_title(dflt, "kettle", "/home/k/Documents", 1),
            "Documents — kettle"
        );
        // Custom templates can use {tab} and {cwd}.
        let t = "[{tab}] {title} ({cwd})";
        assert_eq!(
            window_title(t, "vim", "/home/k/Repos/kettle", 2),
            "[2] vim (/home/k/Repos/kettle)"
        );
    }

    #[test]
    fn selection_kind_maps_clicks_and_alt() {
        // Double/triple click → word/line regardless of Alt.
        assert!(matches!(selection_kind(2, false), SelectionType::Semantic));
        assert!(matches!(selection_kind(2, true), SelectionType::Semantic));
        assert!(matches!(selection_kind(3, false), SelectionType::Lines));
        // Single click: plain drag, or Alt → rectangular block.
        assert!(matches!(selection_kind(1, false), SelectionType::Simple));
        assert!(matches!(selection_kind(1, true), SelectionType::Block));
        // A 0 click-count (no cell) still behaves like a single click.
        assert!(matches!(selection_kind(0, false), SelectionType::Simple));
        assert!(matches!(selection_kind(0, true), SelectionType::Block));
    }

    #[test]
    fn match_triggers_finds_pattern_anywhere_in_text() {
        // Cycle 290 drift guard. The matching engine should fire on
        // the first regex hit, return its action, and silently no-op
        // when nothing matches. Anchors (`^` / `$`) work too because
        // we scan multi-line viewport snapshots; the trigger uses
        // `regex::Regex::is_match` which doesn't auto-anchor.
        use super::{compile_triggers, match_triggers};
        use kettle_config::{OutputTrigger, TriggerAction};
        let cfg = vec![
            OutputTrigger {
                pattern: r"error.*panic".into(),
                action: TriggerAction::Urgency,
            },
            OutputTrigger {
                pattern: r"(BUILD SUCCESSFUL|FAILED)".into(),
                action: TriggerAction::Urgency,
            },
        ];
        let compiled = compile_triggers(&cfg);
        assert_eq!(compiled.len(), 2);

        // Bare match anywhere in the snapshot.
        assert!(match_triggers("thread 'main' panicked: error panic", &compiled).is_some());
        // Alternation pattern still matches both branches.
        assert!(match_triggers("BUILD SUCCESSFUL in 1m23s", &compiled).is_some());
        assert!(match_triggers("BUILD FAILED with 3 errors", &compiled).is_some());
        // Non-matching text: no fire.
        assert!(match_triggers("just normal output, nothing here", &compiled).is_none());
        assert!(match_triggers("", &compiled).is_none());

        // Empty trigger set never fires.
        assert!(match_triggers("error.*panic any text", &[]).is_none());

        // Invalid regex is dropped at compile time (warn-logged); the
        // remaining valid ones still work.
        let cfg_mixed = vec![
            OutputTrigger {
                pattern: r"valid_pattern".into(),
                action: TriggerAction::Urgency,
            },
            OutputTrigger {
                pattern: r"unclosed[".into(), // invalid regex
                action: TriggerAction::Urgency,
            },
        ];
        let compiled_mixed = compile_triggers(&cfg_mixed);
        assert_eq!(
            compiled_mixed.len(),
            1,
            "invalid regex should be dropped at compile time"
        );
        assert!(match_triggers("here is the valid_pattern token", &compiled_mixed).is_some());
    }

    /// v2.20.0 (Terminator `run_cmd_on_match.py` parity completion): a
    /// trigger's capture groups substitute into the spawned argv — `{0}` is
    /// the whole match, `{1}`… numbered groups; an out-of-range reference
    /// stays LITERAL (the typo stays visible); argv stays argv (substitution
    /// can change a VALUE, never add arguments).
    #[test]
    fn trigger_capture_groups_substitute_into_argv() {
        use super::{compile_triggers, match_triggers};
        use kettle_config::{OutputTrigger, TriggerAction};
        let cfg = vec![OutputTrigger {
            pattern: r"ERROR in ([\w./-]+) line (\d+)".into(),
            action: TriggerAction::RunCommand(vec![
                "notify-send".into(),
                "build error".into(),
                "{1}:{2}".into(),
                "match={0}".into(),
                "{9}".into(), // out of range → stays literal
            ]),
        }];
        let compiled = compile_triggers(&cfg);
        let action =
            match_triggers("xx ERROR in src/main.rs line 42 yy", &compiled).expect("trigger fires");
        let TriggerAction::RunCommand(argv) = action else {
            panic!("expected RunCommand");
        };
        assert_eq!(
            argv,
            vec![
                "notify-send".to_string(),
                "build error".to_string(),
                "src/main.rs:42".to_string(),
                "match=ERROR in src/main.rs line 42".to_string(),
                "{9}".to_string(),
            ]
        );
        // Argv arity is unchanged — a match can never ADD arguments.
        assert_eq!(argv.len(), 5);
        // Review fix: a capture whose MATCHED TEXT contains a placeholder
        // must not expand again (single template pass, no re-scan of
        // substituted output).
        let nested = vec![OutputTrigger {
            pattern: r"E:(\S+):(\S+)".into(),
            action: TriggerAction::RunCommand(vec!["log".into(), "{1}".into()]),
        }];
        let action = match_triggers("E:{2}:secret", &compile_triggers(&nested))
            .expect("nested trigger fires");
        let TriggerAction::RunCommand(argv) = action else {
            panic!("expected RunCommand");
        };
        assert_eq!(
            argv[1], "{2}",
            "substituted text must be emitted verbatim, never re-expanded"
        );
        // Urgency triggers are untouched by capture plumbing.
        let urgency = vec![OutputTrigger {
            pattern: r"(panic)".into(),
            action: TriggerAction::Urgency,
        }];
        assert!(matches!(
            match_triggers("a panic b", &compile_triggers(&urgency)),
            Some(TriggerAction::Urgency)
        ));
    }

    #[test]
    fn smart_selection_at_returns_full_token_range() {
        use super::smart_selection_at;
        // Cycle 288 drift guard. The function should pick up every hint
        // kettle-core::hints::detect knows about (URL / path / IPv4 /
        // git SHA), return the inclusive `[start, end]` of the match
        // when the cursor lands inside it, and None otherwise.
        let url_line = "see https://example.com/path?x=1 for more";
        // cursor anywhere inside the URL gets the whole URL.
        let (s, e) = smart_selection_at(url_line, 10).unwrap();
        assert_eq!(&url_line[s..=e], "https://example.com/path?x=1");
        let (s, e) = smart_selection_at(url_line, 31).unwrap();
        assert_eq!(&url_line[s..=e], "https://example.com/path?x=1");
        // Cursor outside the URL returns None — the caller falls back to
        // the alacritty Semantic word.
        assert!(smart_selection_at(url_line, 0).is_none());
        assert!(smart_selection_at(url_line, 39).is_none());

        // IPv4 — dots aren't word chars so the alacritty Semantic word
        // would under-select; smart selection grabs the whole thing.
        let ip_line = "connect to 192.168.1.100 now";
        let (s, e) = smart_selection_at(ip_line, 15).unwrap();
        assert_eq!(&ip_line[s..=e], "192.168.1.100");

        // git SHA.
        let sha_line = "commit a1b2c3d4e5f6 landed";
        let (s, e) = smart_selection_at(sha_line, 10).unwrap();
        assert_eq!(&sha_line[s..=e], "a1b2c3d4e5f6");

        // No hint at all — None.
        assert!(smart_selection_at("plain prose with nothing structured", 5).is_none());
    }

    /// Cycle 662 drift guard. `count_leaves` is the pure helper
    /// behind `Action::CloseTab`'s scope_count for the confirm-
    /// dialog. Walks a tiny synthetic tree to verify the recursion.
    #[test]
    fn count_leaves_for_nested_splits() {
        use super::count_leaves;
        use crate::mux::{Dir, Node};
        // Single leaf.
        let leaf = Node::Leaf(1);
        assert_eq!(count_leaves(&leaf), 1);
        // Two-way split.
        let split = Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Leaf(1)),
            b: Box::new(Node::Leaf(2)),
        };
        assert_eq!(count_leaves(&split), 2);
        // Three-way nested split (a is a split, b is a leaf).
        let nested = Node::Split {
            dir: Dir::Vertical,
            ratio: 0.5,
            a: Box::new(Node::Split {
                dir: Dir::Horizontal,
                ratio: 0.5,
                a: Box::new(Node::Leaf(1)),
                b: Box::new(Node::Leaf(2)),
            }),
            b: Box::new(Node::Leaf(3)),
        };
        assert_eq!(count_leaves(&nested), 3);
        // Four-way (both a and b are splits).
        let four = Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Split {
                dir: Dir::Vertical,
                ratio: 0.5,
                a: Box::new(Node::Leaf(1)),
                b: Box::new(Node::Leaf(2)),
            }),
            b: Box::new(Node::Split {
                dir: Dir::Vertical,
                ratio: 0.5,
                a: Box::new(Node::Leaf(3)),
                b: Box::new(Node::Leaf(4)),
            }),
        };
        assert_eq!(count_leaves(&four), 4);
    }

    /// Cycle 652 drift guard. `confirm_dialog_keypress` is the pure
    /// state machine for the confirm dialog's keyboard handler.
    /// Sub-cycle 4 of confirm-dialog design.
    #[test]
    fn confirm_dialog_keypress_walks_state_machine() {
        use super::{ConfirmKey, ConfirmKeyResult, confirm_dialog_keypress};
        // Escape always cancels.
        assert_eq!(
            confirm_dialog_keypress(0, 2, ConfirmKey::Escape),
            ConfirmKeyResult::Cancel
        );
        assert_eq!(
            confirm_dialog_keypress(1, 2, ConfirmKey::Escape),
            ConfirmKeyResult::Cancel
        );
        // Cycle 861: Enter activates the FOCUSED button (buttons are
        // `[Cancel, Confirm]`). Focused on Cancel (idx 0) → Cancel; focused on
        // Confirm (idx 1, the last) → Confirm. This must match the highlighted
        // button so Enter on the safe default doesn't fire the destructive action.
        assert_eq!(
            confirm_dialog_keypress(0, 2, ConfirmKey::Enter),
            ConfirmKeyResult::Cancel,
            "Enter on the highlighted Cancel button must cancel, not confirm"
        );
        assert_eq!(
            confirm_dialog_keypress(1, 2, ConfirmKey::Enter),
            ConfirmKeyResult::Confirm,
            "Enter on the Confirm button confirms"
        );
        // Tab cycles forward with wrap.
        assert_eq!(
            confirm_dialog_keypress(0, 2, ConfirmKey::Tab),
            ConfirmKeyResult::Move(1)
        );
        assert_eq!(
            confirm_dialog_keypress(1, 2, ConfirmKey::Tab),
            ConfirmKeyResult::Move(0),
            "Tab wraps at end"
        );
        // Shift+Tab cycles backward with wrap.
        assert_eq!(
            confirm_dialog_keypress(0, 2, ConfirmKey::ShiftTab),
            ConfirmKeyResult::Move(1),
            "Shift+Tab wraps at start"
        );
        assert_eq!(
            confirm_dialog_keypress(1, 2, ConfirmKey::ShiftTab),
            ConfirmKeyResult::Move(0)
        );
        // Left at idx 0 is a no-op (Ignore — caller suppresses).
        assert_eq!(
            confirm_dialog_keypress(0, 2, ConfirmKey::Left),
            ConfirmKeyResult::Ignore
        );
        // Left at idx 1 moves to 0.
        assert_eq!(
            confirm_dialog_keypress(1, 2, ConfirmKey::Left),
            ConfirmKeyResult::Move(0)
        );
        // Right at last idx is a no-op.
        assert_eq!(
            confirm_dialog_keypress(1, 2, ConfirmKey::Right),
            ConfirmKeyResult::Ignore
        );
        // Right at idx 0 with 2 buttons moves to 1.
        assert_eq!(
            confirm_dialog_keypress(0, 2, ConfirmKey::Right),
            ConfirmKeyResult::Move(1)
        );
        // Defensive: 0 buttons → any key cancels.
        assert_eq!(
            confirm_dialog_keypress(0, 0, ConfirmKey::Enter),
            ConfirmKeyResult::Cancel
        );
        // Single-button dialog: Tab cycles to itself (no-op move).
        assert_eq!(
            confirm_dialog_keypress(0, 1, ConfirmKey::Tab),
            ConfirmKeyResult::Move(0)
        );
    }

    /// v2.20.0 (`vim-menu-nav`): `y` confirms regardless of focus (it
    /// answers the QUESTION, unlike Enter which fires the focused button —
    /// cycle 861), `n` cancels regardless of focus.
    #[test]
    fn confirm_dialog_y_and_n_answer_directly() {
        use super::{ConfirmKey, ConfirmKeyResult, confirm_dialog_keypress};
        assert_eq!(
            confirm_dialog_keypress(0, 2, ConfirmKey::Yes),
            ConfirmKeyResult::Confirm,
            "y must confirm even with Cancel focused"
        );
        assert_eq!(
            confirm_dialog_keypress(1, 2, ConfirmKey::Yes),
            ConfirmKeyResult::Confirm
        );
        assert_eq!(
            confirm_dialog_keypress(1, 2, ConfirmKey::No),
            ConfirmKeyResult::Cancel,
            "n must cancel even with Confirm focused"
        );
        // Degenerate zero-button dialog still cancels safely.
        assert_eq!(
            confirm_dialog_keypress(0, 0, ConfirmKey::Yes),
            ConfirmKeyResult::Cancel
        );
    }

    /// Cycle 651 drift guard. `content_rect_for` is the pure helper
    /// behind `App::area`. Sub-cycle 2 of vertical-tabs design.
    /// Walks the 4 (tab_pos)×3 (status_pos) cases.
    #[test]
    fn content_rect_for_carves_out_tab_and_status_bands() {
        use super::content_rect_for;
        use kettle_config::{StatusBarMode, TabBarPos};
        // Top + status-off: content starts at y=tab_h, full width.
        let r = content_rect_for((800, 600), 24.0, 16.0, TabBarPos::Top, StatusBarMode::Off);
        assert_eq!(r, (0.0, 24.0, 800.0, 576.0));
        // Bottom + status-off: content at y=0, height shrinks by tab_h.
        let r = content_rect_for(
            (800, 600),
            24.0,
            16.0,
            TabBarPos::Bottom,
            StatusBarMode::Off,
        );
        assert_eq!(r, (0.0, 0.0, 800.0, 576.0));
        // Top + status-top: both claim from top.
        let r = content_rect_for((800, 600), 24.0, 16.0, TabBarPos::Top, StatusBarMode::Top);
        assert_eq!(r, (0.0, 40.0, 800.0, 560.0));
        // Top + status-bottom: status claims from bottom.
        let r = content_rect_for(
            (800, 600),
            24.0,
            16.0,
            TabBarPos::Top,
            StatusBarMode::Bottom,
        );
        assert_eq!(r, (0.0, 24.0, 800.0, 560.0));
        // Bottom + status-bottom: both claim from bottom.
        let r = content_rect_for(
            (800, 600),
            24.0,
            16.0,
            TabBarPos::Bottom,
            StatusBarMode::Bottom,
        );
        assert_eq!(r, (0.0, 0.0, 800.0, 560.0));
        // Cycle 665: Left vertical strip — content carves out
        // `VERTICAL_TAB_STRIP_W` (180 px) from the LEFT side.
        let r = content_rect_for((800, 600), 24.0, 0.0, TabBarPos::Left, StatusBarMode::Off);
        assert_eq!(r, (180.0, 0.0, 620.0, 600.0));
        // Right: 180 px carved from the RIGHT side.
        let r = content_rect_for((800, 600), 24.0, 0.0, TabBarPos::Right, StatusBarMode::Off);
        assert_eq!(r, (0.0, 0.0, 620.0, 600.0));
        // Vertical + status-bar: status still claims y-band
        // (status is always horizontal in v1); strip claims x-band.
        let r = content_rect_for((800, 600), 24.0, 16.0, TabBarPos::Left, StatusBarMode::Top);
        assert_eq!(r, (180.0, 16.0, 620.0, 584.0));
        let r = content_rect_for(
            (800, 600),
            24.0,
            16.0,
            TabBarPos::Right,
            StatusBarMode::Bottom,
        );
        assert_eq!(r, (0.0, 0.0, 620.0, 584.0));
        // Defensive: content_h + content_w clamped to >= 1.0 so a
        // tiny window doesn't degenerate to a zero/negative rect.
        let r = content_rect_for((100, 30), 24.0, 16.0, TabBarPos::Top, StatusBarMode::Bottom);
        assert!(r.3 >= 1.0);
        let r = content_rect_for((100, 600), 0.0, 0.0, TabBarPos::Left, StatusBarMode::Off);
        assert!(
            r.2 >= 1.0,
            "narrow window with vertical strip clamps content_w"
        );
    }

    /// Cycle 650 drift guard. `session_screenshot_path` is the
    /// pure helper behind `Action::TakeScreenshot`. Mirrors the
    /// cycle-621 `session_log_path` shape.
    #[test]
    fn session_screenshot_path_under_cache_kettle_shots() {
        use super::session_screenshot_path;
        let cache = std::path::Path::new("/home/u/.cache");
        let p = session_screenshot_path(1_716_422_400, 1234, Some(cache));
        assert_eq!(
            p,
            std::path::PathBuf::from("/home/u/.cache/kettle/shots/kettle-1716422400-1234.png")
        );
        // Relative fallback when no cache dir resolves.
        let p = session_screenshot_path(1_716_422_400, 1234, None);
        assert_eq!(
            p,
            std::path::PathBuf::from("kettle-shots/kettle-1716422400-1234.png")
        );
        // .png extension is fixed (vs the cycle-621 .log shape).
        assert_eq!(p.extension().and_then(|s| s.to_str()), Some("png"));
    }

    /// Cycle 621 drift guard. `session_log_path` is the pure helper
    /// behind `Action::ToggleSessionLog`. Verify:
    ///   - lives under `<cache>/kettle/logs/`
    ///   - filename includes both the unix-secs (for sort) and pid
    ///     (for collision-resistance across kettle windows)
    ///   - falls back to a relative dir when no cache dir resolves
    #[test]
    fn session_log_path_under_cache_kettle_logs() {
        use super::session_log_path;
        let cache = std::path::Path::new("/home/u/.cache");
        let p = session_log_path(1_716_422_400, 9876, Some(cache));
        assert_eq!(
            p,
            std::path::PathBuf::from("/home/u/.cache/kettle/logs/kettle-1716422400-9876.log")
        );
        // No cache dir resolved → relative fallback (still gets the
        // log written somewhere instead of erroring out).
        let p = session_log_path(1_716_422_400, 9876, None);
        assert_eq!(
            p,
            std::path::PathBuf::from("kettle-logs/kettle-1716422400-9876.log")
        );
    }

    /// Cycle 621 drift guard. `cache_dir_from_env` probes the XDG /
    /// HOME / Windows-ish envs in order. Empty values are treated as
    /// unset so a stripped CI container with `XDG_CACHE_HOME=""`
    /// falls through to the next probe instead of returning `""`.
    #[test]
    fn cache_dir_from_env_probes_in_order() {
        use super::cache_dir_from_env;
        // XDG wins when set.
        let f = |k: &str| match k {
            "XDG_CACHE_HOME" => Some("/x/cache".to_string()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(
            cache_dir_from_env(f).as_deref(),
            Some(std::path::Path::new("/x/cache"))
        );
        // Cycle 919 (audit L1): the non-XDG fallback is per-OS, mirroring the
        // config-dir resolver. On Unix, empty XDG falls through to HOME/.cache.
        #[cfg(not(windows))]
        {
            let f = |k: &str| match k {
                "XDG_CACHE_HOME" => Some(String::new()),
                "HOME" => Some("/h".to_string()),
                _ => None,
            };
            assert_eq!(
                cache_dir_from_env(f).as_deref(),
                Some(std::path::Path::new("/h/.cache"))
            );
        }
        // On Windows the cache dir is %LOCALAPPDATA%, and a stray HOME is IGNORED
        // (the config-dir split-brain class — a shell launch must not disagree
        // with a Start-menu launch).
        #[cfg(windows)]
        {
            // HOME set but no LOCALAPPDATA → None (HOME must not be used).
            let f = |k: &str| match k {
                "XDG_CACHE_HOME" => Some(String::new()),
                "HOME" => Some("/h".to_string()),
                _ => None,
            };
            assert_eq!(
                cache_dir_from_env(f),
                None,
                "Windows must not fall back to HOME/.cache"
            );
            // HOME AND LOCALAPPDATA set → LOCALAPPDATA wins, HOME ignored.
            let f = |k: &str| match k {
                "HOME" => Some(r"C:\Users\u".to_string()),
                "LOCALAPPDATA" => Some(r"C:\Users\u\AppData\Local".to_string()),
                _ => None,
            };
            assert_eq!(
                cache_dir_from_env(f).as_deref(),
                Some(std::path::Path::new(r"C:\Users\u\AppData\Local"))
            );
        }
        // None of the env vars set → None (every OS).
        assert!(cache_dir_from_env(|_| None).is_none());
    }

    /// Cycle 620 drift guard. `compute_tab_segment_widths` is the
    /// pure layout helper behind the tab bar. Verify:
    ///   - homogeneous = true divides the strip evenly (kettle default)
    ///   - homogeneous = false sizes per title length when there's room
    ///   - sum > strip falls back to homogeneous (no truncation)
    ///   - empty title list yields one safe-width segment (no div/0)
    ///   - one-char titles still satisfy a minimum (close-btn-affordance)
    #[test]
    fn compute_tab_segment_widths_homogeneous_and_natural() {
        use super::compute_tab_segment_widths;
        let cell_w = 10.0;
        let tab_h = 24.0;
        // Homogeneous: every width is strip/n.
        let widths =
            compute_tab_segment_widths(["a", "bb", "ccc"].into_iter(), 300.0, cell_w, tab_h, true);
        assert_eq!(widths, vec![100.0, 100.0, 100.0]);
        // Non-homogeneous with plenty of room: each width is
        // chars*cell_w + 2*chrome + close_w (= 1*10 + 24 + 24 = 58
        // for "a", but min clamp = close_w * 1.5 = 36, so 58 wins).
        let widths = compute_tab_segment_widths(
            ["a", "bb", "ccc"].into_iter(),
            1_000.0,
            cell_w,
            tab_h,
            false,
        );
        assert_eq!(widths.len(), 3);
        // Wider title ⇒ wider segment.
        assert!(widths[2] > widths[1]);
        assert!(widths[1] > widths[0]);
        // Min affordance: a single-char title is at least 1.5*tab_h.
        assert!(widths[0] >= tab_h * 1.5);
        // Overflow falls back to homogeneous: sum > strip.
        let titles: Vec<String> = (0..20).map(|i| format!("tab-{i}-with-padding")).collect();
        let widths = compute_tab_segment_widths(
            titles.iter().map(|s| s.as_str()),
            200.0,
            cell_w,
            tab_h,
            false,
        );
        // All equal because we fell back to homogeneous.
        assert!(widths.windows(2).all(|w| (w[0] - w[1]).abs() < 0.01));
        // Empty list ⇒ one safe segment (no div/0). The bar code
        // never actually sees this (renders nothing when there are
        // no tabs), but the helper still has to be panic-safe.
        let widths: Vec<f32> =
            compute_tab_segment_widths(std::iter::empty::<&str>(), 100.0, cell_w, tab_h, true);
        assert_eq!(widths, vec![100.0]);
    }

    /// Cycle 805 drift guard. `split_new_tab_button` splits the trailing
    /// button into the `▾` arrow (left) and `+` (right) hit-rects; they must
    /// abut with no gap/overlap, and a degenerate `arrow_w` can't yield a
    /// negative `+` width.
    #[test]
    fn split_new_tab_button_places_arrow_left_of_plus() {
        use super::split_new_tab_button;
        // button at x=200, 52 wide (two 26-px halves), arrow 26 wide.
        let (arrow, plus) = split_new_tab_button((200.0, 0.0, 52.0, 26.0), 26.0);
        assert_eq!(arrow, (200.0, 0.0, 26.0, 26.0)); // arrow on the LEFT
        assert_eq!(plus, (226.0, 0.0, 26.0, 26.0)); // plus on the RIGHT
        // They abut exactly (no gap, no overlap).
        assert_eq!(arrow.0 + arrow.2, plus.0);
        // Degenerate: arrow wider than the button → clamped, plus width >= 0.
        let (a2, p2) = split_new_tab_button((0.0, 0.0, 20.0, 10.0), 999.0);
        assert_eq!(a2.2, 20.0);
        assert_eq!(p2.2, 0.0);
        assert!(p2.2 >= 0.0);
    }

    /// Cycle 618 drift guard. `pick_next_profile` is the pure
    /// helper behind `Action::NextProfile`/`PrevProfile`. Cycles
    /// the sorted profile list with wrap-around; starts at index 0
    /// when current isn't a known profile (e.g. --config FILE or
    /// default config).
    #[test]
    fn pick_next_profile_wraps_and_starts_at_index_0() {
        use super::pick_next_profile;
        let names: Vec<String> = vec!["dark".into(), "dev".into(), "light".into()];
        // Forward through the list, wrapping at the end.
        assert_eq!(pick_next_profile(Some("dark"), &names, true), "dev");
        assert_eq!(pick_next_profile(Some("dev"), &names, true), "light");
        assert_eq!(pick_next_profile(Some("light"), &names, true), "dark");
        // Backward, wrapping at the start.
        assert_eq!(pick_next_profile(Some("dark"), &names, false), "light");
        assert_eq!(pick_next_profile(Some("dev"), &names, false), "dark");
        assert_eq!(pick_next_profile(Some("light"), &names, false), "dev");
        // Current is None or an unknown name: start the cycle at idx 0.
        // Forward → idx 1 (the second name); backward → last name.
        assert_eq!(pick_next_profile(None, &names, true), "dev");
        assert_eq!(pick_next_profile(None, &names, false), "light");
        assert_eq!(pick_next_profile(Some("missing"), &names, true), "dev");
        // Single profile: always returns itself (n=1, both arms ⇒ idx 0).
        let single = vec!["only".to_string()];
        assert_eq!(pick_next_profile(Some("only"), &single, true), "only");
        assert_eq!(pick_next_profile(Some("only"), &single, false), "only");
    }

    /// Cycle 616 drift guard. `pick_light_dark_target` is the
    /// pure helper behind `Action::ToggleLightDark` — the policy
    /// must round-trip current ↔ {light, dark} cleanly, default
    /// to `dark` on a third-party current theme, and silently
    /// no-op when neither config key is set.
    #[test]
    fn pick_light_dark_target_round_trips() {
        use super::pick_light_dark_target;
        // Round-trip: current==dark → switch to light.
        assert_eq!(
            pick_light_dark_target("TokyoNight Night", "TokyoNight Day", "TokyoNight Night")
                .as_deref(),
            Some("TokyoNight Day"),
        );
        // Round-trip: current==light → switch to dark.
        assert_eq!(
            pick_light_dark_target("TokyoNight Day", "TokyoNight Day", "TokyoNight Night")
                .as_deref(),
            Some("TokyoNight Night"),
        );
        // Case-insensitive match on `current`.
        assert_eq!(
            pick_light_dark_target("tokyonight night", "TokyoNight Day", "TokyoNight Night")
                .as_deref(),
            Some("TokyoNight Day"),
        );
        // Current is a third-party theme: default to dark.
        assert_eq!(
            pick_light_dark_target("Catppuccin Mocha", "TokyoNight Day", "TokyoNight Night")
                .as_deref(),
            Some("TokyoNight Night"),
        );
        // Only light set: one-way switch to light.
        assert_eq!(
            pick_light_dark_target("Catppuccin Mocha", "TokyoNight Day", "").as_deref(),
            Some("TokyoNight Day"),
        );
        // Only dark set: one-way switch to dark.
        assert_eq!(
            pick_light_dark_target("Catppuccin Latte", "", "TokyoNight Night").as_deref(),
            Some("TokyoNight Night"),
        );
        // Neither set: no-op.
        assert_eq!(pick_light_dark_target("anything", "", ""), None);
    }
}
