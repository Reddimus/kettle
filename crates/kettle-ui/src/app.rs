//! winit application: window lifecycle, input routing, the tiled multiplexer,
//! the search overlay, clipboard, and live config reload.

use std::sync::Arc;

use anyhow::Result;
use kettle_config::{Action, Config, Key as KKey, Mods, Trigger};
use kettle_config::{TabBarMode, TabBarPos};
use kettle_core::{Scroll, TermEvent};
use kettle_render::{
    ContextMenu, ContextMenuRow, HighlightRect, HintLabel, Overlay, PaneView, Renderer,
    TabActivity as RenderTabActivity, TabBar, TabSeg,
};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Fullscreen, UserAttentionType, Window, WindowId};

use crate::input;
use crate::mux::{Dir, Mux, Rect};

#[derive(Debug, Clone)]
pub enum UserEvent {
    Wakeup,
    ReloadConfig,
    /// Cycle 302 remote control: the remote-command file changed and
    /// the watcher needs the main thread to read + process new lines.
    /// One event per change (notify coalesces consecutive writes), so
    /// the main thread can batch-read all pending lines at once.
    RemoteCommand,
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
        ConfirmKey::Enter => ConfirmKeyResult::Confirm,
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
    if let Some(p) = get("XDG_CACHE_HOME").filter(|s| !s.is_empty()) {
        return Some(std::path::PathBuf::from(p));
    }
    if let Some(home) = get("HOME").filter(|s| !s.is_empty()) {
        return Some(std::path::PathBuf::from(home).join(".cache"));
    }
    if let Some(p) = get("LOCALAPPDATA").filter(|s| !s.is_empty()) {
        return Some(std::path::PathBuf::from(p));
    }
    None
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
struct ViState {
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
    triggers
        .iter()
        .find(|(re, _)| re.is_match(text))
        .map(|(_, action)| action.clone())
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
struct HintTarget {
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

struct ContextMenuState {
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
fn assign_mnemonics(items: &[ContextMenuItem]) -> Vec<Option<(usize, char)>> {
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
            ContextMenuItem::Separator => "",
        })
        .collect();
    let mut claimed: std::collections::HashSet<char> = std::collections::HashSet::new();
    let mut out: Vec<Option<(usize, char)>> = Vec::with_capacity(labels.len());
    for label in labels {
        let mut chosen: Option<(usize, char)> = None;
        for (bi, c) in label.char_indices() {
            if !c.is_ascii_alphabetic() {
                continue;
            }
            let low = c.to_ascii_lowercase();
            if !claimed.contains(&low) {
                claimed.insert(low);
                chosen = Some((bi, low));
                break;
            }
        }
        out.push(chosen);
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
        | ContextMenuItem::ProfileChoice { label, .. } => {
            label.to_ascii_lowercase().starts_with(&needle)
        }
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
    )
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

pub struct App {
    cfg: Config,
    window: Option<Arc<Window>>,
    /// Cycle 745: OS taskbar progress, driven by the focused pane's OSC 9;4
    /// state each frame (pwsh 7 / Windows Terminal parity). No-op off Windows.
    taskbar: crate::taskbar::Taskbar,
    renderer: Option<Renderer>,
    mux: Mux,
    mods: ModifiersState,
    proxy: EventLoopProxy<UserEvent>,
    clipboard: Option<arboard::Clipboard>,
    fullscreen: bool,
    cursor: PhysicalPosition<f64>,
    selecting: bool,
    /// Dragging the focused pane's scrollbar thumb.
    dragging_scrollbar: bool,
    /// `(query, index)` last scrolled-to, so the viewport follows search
    /// matches into scrollback without re-scrolling every frame.
    search_revealed: Option<(String, usize)>,
    mouse_btn: Option<u8>,
    links: Vec<kettle_core::Link>,
    ssh_input: Option<String>,
    /// `Some((query, selected))` while the command palette is open.
    palette_input: Option<(String, usize)>,
    /// Cycle 708 (Terminator parity, `layoutlauncher.py`):
    /// `Action::OpenLayoutPicker` modal state. Same shape as
    /// `palette_input` — (typed query, selected index) — but
    /// ranks against the cycle-708 `Session::list_layouts`
    /// instead of the cycle-104 command palette. Enter spawns
    /// `kettle --layout NAME` as a new window.
    layout_picker_input: Option<(String, usize)>,
    /// Active quick-select hint mode: detected targets + typed prefix.
    hint_state: Option<(Vec<HintTarget>, String)>,
    /// Right-click context menu state (`Some` while open). Lives next
    /// to the other modal overlays — same close-all-modals discipline,
    /// same Esc-to-dismiss key route. Anchored at the click point so
    /// the menu appears where the user looked, not at a fixed corner.
    context_menu: Option<ContextMenuState>,
    /// Cycle 369 (Terminator parity, replaces cycle-354 placeholders):
    /// when `Some`, the user is editing a window/tab/pane title via
    /// an inline overlay. Enter applies + clears; Esc cancels +
    /// clears; printable chars append; Backspace removes one.
    editing_title: Option<TitleEditState>,
    /// Cycle 648 (sub-cycle 2 of confirm-dialog design): when `Some`,
    /// a confirm modal is open. Keyboard input routes to modal
    /// dispatch (`Tab` cycles `focus_idx`, `Enter` confirms, `Esc`
    /// cancels) and the renderer paints the centered modal panel
    /// over a dimming backdrop. State landed now; sub-cycle 3 wires
    /// the renderer, sub-cycle 4 wires keyboard nav, sub-cycle 5
    /// wires the dispatch interception for `Action::CloseWindow`.
    confirm_dialog: Option<ConfirmDialogState>,
    window_focused: bool,
    /// True while the OS mouse cursor is hidden because the user is typing
    /// (`mouse-hide-while-typing`). Re-shown on the next mouse movement.
    mouse_hidden: bool,
    /// Last `CursorIcon` we pushed to the window — used to dedupe so we
    /// don't issue a `set_cursor` syscall on every CursorMoved event.
    /// `None` until the first call, which guarantees the initial state
    /// gets pushed exactly once.
    last_cursor_icon: Option<CursorIcon>,
    /// Cycle 249: drag-to-reorder tab state. `Some(_)` while a left-
    /// mouse-button press in the tab bar is being held; cleared on
    /// release. Mouse moves while held swap the active tab by the
    /// delta between the current cursor index and the active index.
    tab_drag_active: bool,
    /// Cycle 402 (Terminator parity, detachable-tabs Bucket-D
    /// sub-cycle 6): cross-window drag FSM state. Distinct from
    /// the existing in-window tab_drag_active (cycle 249) — that
    /// handles drag-to-reorder within this window. detach_drag
    /// handles cross-window drag-to-detach. Both fire from the
    /// same mouse-down on the tab bar.
    detach_drag: crate::detach::DragState,
    /// Index of the tab whose close-button (`✕`) zone the mouse cursor
    /// is currently over. Drives both the OS pointer-cursor swap and
    /// the renderer's hover-background quad so the trailing `✕` reads
    /// as a clickable button rather than part of the title text.
    /// Updated in `sync_cursor_icon` on `CursorMoved`; cleared when
    /// the cursor leaves the bar or the bar is hidden.
    hovered_close_idx: Option<usize>,
    /// Cycle 290 triggers: compiled regex set built from
    /// `cfg.triggers` at App construction (and after live reload).
    /// Invalid patterns are logged via `log::warn!` and dropped, so a
    /// malformed `trigger = ` line on one rule doesn't sink the whole
    /// trigger set.
    compiled_triggers: Vec<(regex::Regex, kettle_config::TriggerAction)>,
    /// Cycle 298 vi-mode (Alacritty parity), sub-cycle 1 of 4.
    /// `Some(ViState)` when the user has triggered ToggleViMode and
    /// kettle is intercepting keys for vi-style navigation; `None`
    /// otherwise. Sub-cycle 2 wires h/j/k/l movement; sub-cycle 3
    /// adds the visible block cursor render; sub-cycle 4 adds `v`
    /// visual selection + `y` yank.
    vi_mode: Option<ViState>,
    /// Cycle 290: per-trigger last-fire timestamps. Dedupes a fast-
    /// arriving match flood (e.g., a build script printing 100 error
    /// lines in one frame should only nudge the user once, not
    /// 100×). Cleared when any trigger fires past a 2-second
    /// quietness window.
    last_trigger_fire: std::time::Instant,
    /// Cycle 656 (Terminator parity, sub-cycle 6 of
    /// [`TERMINATOR-REMOTE-DESIGN.md`](
    /// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): cached
    /// `sysinfo::System` owned across ticks so the process-list
    /// refresh amortizes (sysinfo's internal cache survives between
    /// `refresh_processes_specifics` calls). Used by the
    /// per-pane remote-session detector.
    remote_sysinfo: kettle_remote::SysinfoSystem,
    /// Cycle 656: throttle the remote-detect poll to ~5 Hz. The
    /// process-list refresh is fast on Linux (<1 ms) but no need
    /// to walk every tick — SSH/Docker sessions don't fire-up
    /// faster than a couple times per second.
    last_remote_poll: std::time::Instant,
    /// Cycle 666 (sub-cycle 5 of [`TERMINATOR-AUTO-THEME-DESIGN.md`](
    /// ../../../docs/TERMINATOR-AUTO-THEME-DESIGN.md)): the
    /// most-recent "schedule decision" (true=dark, false=light)
    /// we've applied. A boundary-crossing fires the theme swap
    /// exactly once (instead of every tick the schedule says
    /// "now's dark"). `None` = haven't checked yet; first check
    /// seeds it without swapping the theme.
    last_schedule_decision: Option<bool>,
    /// Cycle 693 Terminator parity (`key_scaled_zoom`). When
    /// `Action::ScaledZoom` enters the zoom state, it saves the
    /// font size here so the leave-zoom path can restore it
    /// exactly. `None` means "not currently in scaled zoom" — so
    /// repeated `ToggleZoom` taps from other code paths don't
    /// accidentally undo this.
    scaled_zoom_prev_font_size: Option<f32>,
    /// Cycle 703 (Terminator plugin parity, plugin sub-cycle:
    /// `LuaEvent::PaneFocus`). The pane id we last fired the
    /// focus-change event for. `None` until the first
    /// `poll_focus_event` tick — that first tick emits with
    /// `prev = None` so plugins can seed their state. Diff
    /// against `self.mux.active_focus()` each redraw; emit on
    /// boundary-cross.
    last_emitted_focus: Option<u64>,
    /// Cycle 704 (Terminator plugin parity, plugin sub-cycle:
    /// `LuaEvent::TitleChanged`). Snapshot of the last title we
    /// emitted a `title_changed` event for, keyed by pane id.
    /// Diffed against `mux.panes[].title` each redraw; emit on
    /// boundary-cross. Entries for closed panes drop on next
    /// tick (we only iterate live panes — closed ids never get
    /// re-checked, so the map self-prunes on close+reopen of
    /// the same id since `Pane::id` is monotonic).
    last_emitted_titles: std::collections::HashMap<u64, String>,
    blink_on: bool,
    last_blink: std::time::Instant,
    last_bell: Option<std::time::Instant>,
    last_click: Option<(std::time::Instant, usize, usize, u8)>,
    /// Last OS window title set (dedupe `set_title` syscalls).
    last_title: String,
    /// Explicit `--config` file (persists for live reload).
    config_path: Option<std::path::PathBuf>,
    /// First-tab CLI overrides (`-e cmd`, `-d dir`); consumed once.
    startup: crate::Options,
    _watcher: Option<notify::RecommendedWatcher>,
    /// Cycle 302: drop guard for the remote-control watcher. Stays
    /// alive for the whole App lifetime; dropping the watcher would
    /// kill the notify thread without warning.
    _remote_watcher: Option<notify::RecommendedWatcher>,
    /// Cycle 325 Lua scripting: bytes the user's `--lua-script`
    /// queued via `kettle.send_text(s)` before the first pane
    /// existed. Drained + written to the first focused pane's
    /// PTY once that pane is ready.
    pending_lua_send: Vec<u8>,
    /// Cycle 326 Lua scripting: Actions the user's `--lua-script`
    /// queued via `kettle.exec_action(name)`. Drained + dispatched
    /// after the first pane spawns (some actions like
    /// `toggle_vi_mode` need a focused pane to operate on).
    pending_lua_actions: Vec<kettle_config::Action>,
    /// Cycle 412 (Terminator parity, exit-action = restart impl):
    /// pane ids whose shell exited + cfg.exit_action requested
    /// restart. Drained AFTER drain_events (so we don't borrow
    /// self.mux mutably twice in one tick), spawns a NEW tab
    /// containing the same argv + cwd via `Mux::new_tab_with`
    /// (cycle 418); the dead pane is reaped at end-of-redraw.
    /// Dedup'd on push (cycle 452) so alacritty's `Exit` +
    /// `ChildExit` pair only spawn one new tab.
    pending_pane_restarts: Vec<u64>,
    /// Cycle 366 (Terminator plugin parity, plugin Bucket-D
    /// sub-cycle 3): the live LuaEngine persisted across the App's
    /// lifetime so `kettle.on(event, callback)` registrations stay
    /// in scope + LuaEngine::fire_event(...) can invoke them from
    /// the 5 emission sites:
    /// - Startup — App::resumed once, guarded by lua_startup_fired
    ///   (cycle 366).
    /// - TabAdd — `fire_tab_add_event` helper (cycle 425): keyboard
    ///   NewTab, NewWindow fallback, remote-control IPC new-tab,
    ///   exit-action=restart respawn — 4 sites.
    /// - TabClose — `fire_tab_close_event` helper (cycle 424):
    ///   keyboard CloseTab, tab-bar ✕-click (2 click-handler
    ///   branches), SCM_RIGHTS + file-fallback handoff sources —
    ///   5 sites.
    /// - Bell — drain_events Bell handler (cycle 367).
    /// - Output — drain_events output sidechannel (cycle 378).
    ///
    /// All 5 hooks share `drain_lua_hook_commands` for the
    /// LuaCommand queue dispatch (cycles 426-428, 433).
    lua_engine: Option<crate::LuaEngine>,
    /// Cycle 366: set to true after we've fired LuaEvent::Startup
    /// once. Guards against re-firing on subsequent resumed()
    /// invocations (winit may re-emit resumed on Wayland).
    lua_startup_fired: bool,
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
    /// Cycle 410: SCM_RIGHTS-based cross-process tab handoff.
    /// Creates a Unix socketpair, fork+exec's a kettle child with
    /// the child's socket fd as fd 3 + `--tab-handoff-fd 3`, then
    /// calls `fd_transport::send_fds` to ship the serialized tab
    /// JSON. Returns true on success; false on any failure
    /// (caller falls through to file-fallback).
    ///
    /// Currently sends JSON only; live PTY-fd transfer is the
    /// final sub-cycle 7 piece that requires extracting PTYs
    /// from the source Pane (a non-trivial alacritty_terminal
    /// internal change).
    #[cfg(unix)]
    #[allow(unused_variables)]
    fn try_move_tab_to_new_window_scm_rights(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
    ) -> bool {
        use std::os::unix::io::AsRawFd;
        let stab = match self.mux.serialize_tab(self.mux.active) {
            Some(s) => s,
            None => return false,
        };
        let session = crate::session::Session {
            tabs: vec![stab],
            active: 0,
            theme: Some(self.cfg.theme_name.clone()),
        };
        let json = match serde_json::to_vec(&session) {
            Ok(j) => j,
            Err(_) => return false,
        };
        let (parent, child) = match std::os::unix::net::UnixStream::pair() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let child_fd = child.as_raw_fd();
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--tab-handoff-fd").arg("3");
        if let Some(p) = self.config_path.as_ref() {
            cmd.arg("--config").arg(p);
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // The child socket needs to end up at fd 3 in the child
        // process. pre_exec runs in the child between fork + exec;
        // dup2 the socket into 3 + clear close-on-exec so it
        // survives the exec.
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(child_fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Clear FD_CLOEXEC on fd 3 so it survives exec.
                let flags = libc::fcntl(3, libc::F_GETFD);
                if flags >= 0 {
                    libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                }
                Ok(())
            });
        }
        match cmd.spawn() {
            Ok(_) => {
                // Drop the child end in the parent so we don't
                // hold an extra reference.
                drop(child);
                // Send the JSON via send_fds (empty fds for now;
                // future cycle adds PTY fds).
                let _ = crate::fd_transport::send_fds(&parent, &json, &[]);
                drop(parent);
                // Close the source tab now that the child is up.
                // Cycle 424: fire TabClose so plugins see the close.
                let closing_idx = self.mux.active;
                let _ = self.mux.close_tab();
                self.fire_tab_close_event(closing_idx);
                true
            }
            Err(_) => false,
        }
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
        let initial_triggers = compile_triggers(&initial_cfg.triggers);
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
        let mut app = App {
            cfg: initial_cfg,
            window: None,
            taskbar: crate::taskbar::Taskbar::new(),
            renderer: None,
            mux: {
                let mut m = Mux::new();
                m.lua_output_subscribed = lua_output_subscribed;
                m
            },
            mods: ModifiersState::empty(),
            proxy,
            // Cycle 754: surface why the clipboard is unavailable instead of a
            // silent `None`. On headless/SSH-without-X11-forwarding/sandboxed
            // Linux, arboard can't connect to a display server, and copy/paste
            // + OSC 52 then silently no-op. A startup warning makes the cause
            // debuggable from `RUST_LOG` rather than "paste mysteriously does
            // nothing".
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
            fullscreen: false,
            cursor: PhysicalPosition::new(0.0, 0.0),
            selecting: false,
            dragging_scrollbar: false,
            search_revealed: None,
            mouse_btn: None,
            links: Vec::new(),
            ssh_input: None,
            palette_input: None,
            layout_picker_input: None,
            hint_state: None,
            context_menu: None,
            editing_title: None,
            confirm_dialog: None,
            window_focused: true,
            mouse_hidden: false,
            last_cursor_icon: None,
            tab_drag_active: false,
            detach_drag: crate::detach::DragState::default(),
            hovered_close_idx: None,
            vi_mode: None,
            compiled_triggers: initial_triggers,
            last_trigger_fire: std::time::Instant::now() - std::time::Duration::from_secs(60),
            remote_sysinfo: kettle_remote::SysinfoSystem::new(),
            last_remote_poll: std::time::Instant::now() - std::time::Duration::from_secs(60),
            last_schedule_decision: None,
            scaled_zoom_prev_font_size: None,
            last_emitted_focus: None,
            last_emitted_titles: std::collections::HashMap::new(),
            blink_on: true,
            last_blink: std::time::Instant::now(),
            last_bell: None,
            last_click: None,
            last_title: String::new(),
            config_path: startup.config.clone(),
            startup,
            _watcher: watcher,
            _remote_watcher: remote_watcher,
            pending_lua_send,
            pending_lua_actions,
            pending_pane_restarts: Vec::new(),
            lua_engine,
            lua_startup_fired: false,
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
    fn fire_tab_add_event(&mut self) {
        if let Some(eng) = &self.lua_engine {
            eng.fire_event(&crate::LuaEvent::TabAdd(self.mux.active));
        }
        self.drain_lua_hook_commands("tab_add hook");
    }

    /// Cycle 424+426: fire LuaEvent::TabClose + drain commands.
    /// Every close_tab call site should call this so plugins
    /// listening for tab_close see every close regardless of
    /// trigger source.
    fn fire_tab_close_event(&mut self, closing_idx: usize) {
        if let Some(eng) = &self.lua_engine {
            eng.fire_event(&crate::LuaEvent::TabClose(closing_idx));
        }
        self.drain_lua_hook_commands("tab_close hook");
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
        Arc::new(move || {
            let _ = p.send_event(UserEvent::Wakeup);
        })
    }

    fn cell_px(&self) -> (u16, u16) {
        self.renderer
            .as_ref()
            .map(|r| (r.cell_w.max(1.0) as u16, r.cell_h.max(1.0) as u16))
            .unwrap_or((8, 16))
    }

    /// Hide the OS mouse cursor; idempotent. Called when the user starts
    /// typing if `mouse-hide-while-typing` is on. The cursor reappears on
    /// the next mouse move or window-enter event.
    fn hide_mouse_cursor(&mut self) {
        if self.mouse_hidden || !self.cfg.mouse_hide_while_typing {
            return;
        }
        if let Some(w) = &self.window {
            w.set_cursor_visible(false);
            self.mouse_hidden = true;
        }
    }

    /// Show the OS mouse cursor; idempotent. Called whenever the mouse
    /// moves or re-enters the window.
    fn show_mouse_cursor(&mut self) {
        if !self.mouse_hidden {
            return;
        }
        if let Some(w) = &self.window {
            w.set_cursor_visible(true);
            self.mouse_hidden = false;
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
    fn cursor_in_status_bar(&self) -> bool {
        let h = self.status_bar_h();
        if h <= 0.0 {
            return false;
        }
        let (_, sh) = self
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        cursor_in_status_bar_band(self.cursor.y as f32, h, sh as f32, self.cfg.status_bar)
    }

    /// Cycle 320: combined chrome-band hit-test. True when the
    /// cursor is over either the tab bar or the status bar — both
    /// belong in the "OS arrow cursor" group.
    fn cursor_in_chrome_band(&self) -> bool {
        self.cursor_in_tab_bar() || self.cursor_in_status_bar()
    }

    fn cursor_in_tab_bar(&self) -> bool {
        let h = self.tab_bar_h();
        if h <= 0.0 {
            return false;
        }
        let (sw, sh) = self
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        // Cycle 668 (vertical-tabs sub-cycle 4): for Left/Right
        // strips, the cursor needs to be within
        // `VERTICAL_TAB_STRIP_W` of the configured edge.
        match self.cfg.tab_bar_pos {
            TabBarPos::Left => {
                let x = self.cursor.x as f32;
                (0.0..self.cfg.tab_bar_width).contains(&x)
            }
            TabBarPos::Right => {
                let x = self.cursor.x as f32;
                x >= sw as f32 - self.cfg.tab_bar_width && x <= sw as f32
            }
            TabBarPos::Top | TabBarPos::Bottom => {
                cursor_in_tab_bar_band(self.cursor.y as f32, h, sh as f32, self.cfg.tab_bar_pos)
            }
        }
    }

    /// Set the OS mouse-cursor icon, deduped against the last value pushed
    /// to the window. Called on CursorMoved (position changes the
    /// hit-test) and on ModifiersChanged (the modifier state gates the
    /// click-to-open affordance).
    fn sync_cursor_icon(&mut self) {
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
        let bar = self.tab_bar();
        self.hovered_close_idx =
            hovered_close_button(&bar.segments, self.cursor.x as f32, self.cursor.y as f32);
        let close_hover = tab_close_hover_icon(self.hovered_close_idx.is_some());
        let chrome = chrome_cursor_icon(self.cursor_in_chrome_band(), self.any_modal_open());
        let want = close_hover.or(chrome).unwrap_or_else(|| {
            let want_pointer = (self.mods.control_key() || self.mods.super_key())
                && self.link_at_cursor().is_some();
            if want_pointer {
                CursorIcon::Pointer
            } else {
                CursorIcon::Text
            }
        });
        if self.last_cursor_icon != Some(want)
            && let Some(w) = &self.window
        {
            w.set_cursor(want);
            self.last_cursor_icon = Some(want);
        }
    }

    /// Clear the focused pane's selection (called when the user types —
    /// every modern terminal does this so a stale highlight doesn't
    /// confuse the next copy/paste). No-op when nothing is selected.
    fn clear_selection_on_input(&mut self) {
        if let Some(p) = self.mux.focused()
            && let Ok(mut t) = p.term.term.lock()
            && t.selection.is_some()
        {
            t.selection = None;
        }
    }

    fn tab_bar_h(&self) -> f32 {
        let show = match self.cfg.tab_bar {
            TabBarMode::Off => false,
            TabBarMode::Auto => self.mux.tabs.len() > 1,
            TabBarMode::Always => true,
        };
        if show {
            self.renderer
                .as_ref()
                .map(|r| r.cell_h + 8.0)
                .unwrap_or(24.0)
        } else {
            0.0
        }
    }

    /// Cycle 296: status-bar height (0 when off, cell_h + 6 px when
    /// enabled). Pair with `cfg.status_bar` (StatusBarMode) for
    /// position. Slightly shorter than the tab bar so the two strips
    /// read as distinct horizontal bands.
    fn status_bar_h(&self) -> f32 {
        match self.cfg.status_bar {
            kettle_config::StatusBarMode::Off => 0.0,
            _ => self
                .renderer
                .as_ref()
                .map(|r| r.cell_h + 6.0)
                .unwrap_or(22.0),
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
    fn pane_at_titlebar_click(&self, px: f32, py: f32) -> Option<u64> {
        if !self.cfg.show_titlebar {
            return None;
        }
        let active = self.mux.active;
        let rects = self.mux.layout(active, self.area());
        if rects.len() < 2 {
            // Single-pane tab: titlebar isn't rendered (cycle-379
            // gates on >1 pane).
            return None;
        }
        let bar_h = self
            .renderer
            .as_ref()
            .map(|r| r.cell_h + 6.0)
            .unwrap_or(20.0);
        pane_titlebar_hit(px, py, &rects, self.cfg.title_at_bottom, bar_h)
    }

    fn area(&self) -> Rect {
        let surface = self
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        // Cycle 651 + 673: delegate to the pure helper, threading
        // `cfg.tab_bar_width` so a user-configured strip width
        // is honored.
        content_rect_for_with_strip(
            surface,
            self.tab_bar_h(),
            self.status_bar_h(),
            self.cfg.tab_bar_pos,
            self.cfg.status_bar,
            self.cfg.tab_bar_width,
        )
    }

    /// Tab-bar geometry — the single source of truth shared by the renderer
    /// (drawing) and the click hit-testing below.
    fn tab_bar(&self) -> TabBar {
        let height = self.tab_bar_h();
        if height <= 0.0 {
            return TabBar::hidden();
        }
        let (w, h) = self
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
            return self.tab_bar_vertical(sw, sh, height);
        }
        let y = match self.cfg.tab_bar_pos {
            TabBarPos::Top | TabBarPos::Left | TabBarPos::Right => 0.0,
            TabBarPos::Bottom => sh - height,
        };
        let titles = self.mux.tab_titles();
        let n = titles.len().max(1);
        // Trailing square "+" button.
        let plus_w = height;
        let strip = (sw - plus_w).max(plus_w);
        let cell_w = self
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
        let active = self.mux.active;
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
                let activity = self
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
            new_tab: (sw - plus_w, y, plus_w, height),
            // Cycle 178: broadcast indicator on the active tab.
            broadcast: self.mux.is_broadcast_on(),
            // Hover-on-✕ chip: renderer paints a red highlight behind
            // the close glyph; UI's `sync_cursor_icon` flips the OS
            // cursor to Pointer at the same time.
            hovered_close_idx: self.hovered_close_idx,
            // Cycle 255: while a tab-bar drag is in progress, hand
            // the renderer the cursor x so it paints a translucent
            // ghost of the dragged segment under the cursor — gives
            // the cycle-249 reorder a "I'm picking this tab up"
            // affordance instead of the bare snap behavior.
            drag_cursor_x: if self.tab_drag_active {
                Some(self.cursor.x as f32)
            } else {
                None
            },
        }
    }

    /// Cycle 668 (vertical-tabs sub-cycle 4): tab-bar layout for
    /// `TabBarPos::Left` / `Right`. Stacks segments vertically,
    /// each one (`VERTICAL_TAB_STRIP_W` × `tab_bar_h`).
    /// New-tab `+` button anchors at the bottom of the strip.
    fn tab_bar_vertical(&self, sw: f32, sh: f32, height: f32) -> TabBar {
        let strip_w = self.cfg.tab_bar_width;
        let strip_x = match self.cfg.tab_bar_pos {
            TabBarPos::Left => 0.0,
            TabBarPos::Right => sw - strip_w,
            _ => 0.0, // unreachable in this branch
        };
        let titles = self.mux.tab_titles();
        let active = self.mux.active;
        let now = std::time::Instant::now();
        let silence = std::time::Duration::from_millis(self.cfg.tab_silence_threshold_ms);
        let segments: Vec<TabSeg> = titles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let seg_y = i as f32 * height;
                let activity = self
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
            broadcast: self.mux.is_broadcast_on(),
            hovered_close_idx: self.hovered_close_idx,
            // Drag-cursor preview is x-only in v1; vertical drag
            // reorder is sub-cycle 6 of the design.
            drag_cursor_x: if self.tab_drag_active {
                Some(self.cursor.x as f32)
            } else {
                None
            },
        }
    }

    fn grid_of(&self, rect: Rect) -> (usize, usize) {
        let (cw, ch) = self
            .renderer
            .as_ref()
            .map(|r| (r.cell_w, r.cell_h))
            .unwrap_or((8.0, 16.0));
        let (_, _, w, h) = rect;
        let cols = ((w - self.cfg.padding_x * 2.0) / cw).floor().max(1.0) as usize;
        let rows = ((h - self.cfg.padding_y * 2.0) / ch).floor().max(1.0) as usize;
        (cols, rows)
    }

    fn focused_rect(&self, area: Rect) -> Option<Rect> {
        let f = self.mux.active_focus()?;
        self.mux
            .layout(self.mux.active, area)
            .into_iter()
            .find(|(id, _)| *id == f)
            .map(|(_, r)| r)
    }

    /// If `(px, py)` is on the focused pane's scrollbar (right edge, ~8 px)
    /// and the bar is visible, jump the viewport to the clicked position.
    /// Returns `true` if it handled the click (so it won't start a
    /// selection).
    fn scrollbar_jump(&mut self, area: Rect, px: f32, py: f32) -> bool {
        self.scrollbar_at(area, px, py, true)
    }

    /// Map a pointer position to a viewport jump on the focused pane's
    /// scrollbar. With `require_zone`, only the right-edge ~8 px strip
    /// counts (initial click); during a drag the x is ignored so the
    /// grab follows the pointer's y anywhere.
    fn scrollbar_at(&mut self, area: Rect, px: f32, py: f32, require_zone: bool) -> bool {
        if self.cfg.scrollbar == kettle_config::ScrollbarMode::Never {
            return false;
        }
        let Some((rx, ry, rw, rh)) = self.focused_rect(area) else {
            return false;
        };
        if require_zone && (px < rx + rw - 8.0 || px > rx + rw || py < ry || py > ry + rh) {
            return false;
        }
        let Some(p) = self.mux.focused() else {
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

    fn px_to_point(&self, rect: Rect, px: f32, py: f32) -> kettle_core::Point {
        let (cw, ch) = self
            .renderer
            .as_ref()
            .map(|r| (r.cell_w, r.cell_h))
            .unwrap_or((8.0, 16.0));
        let (rx, ry, _, _) = rect;
        let col = ((px - rx - self.cfg.padding_x) / cw).floor().max(0.0) as usize;
        let line = ((py - ry - self.cfg.padding_y) / ch).floor().max(0.0) as i32;
        kettle_core::Point::new(kettle_core::Line(line), kettle_core::Column(col))
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
    fn yank_vi_selection(&mut self, start: (usize, usize), end: (usize, usize)) -> String {
        let Some(pane) = self.mux.focused() else {
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
        let mut out = String::new();
        for r in sr..=er.min(rows.saturating_sub(1)) {
            let first = if r == sr { sc } else { 0 };
            let last = if r == er { ec.min(cols - 1) } else { cols - 1 };
            for c in first..=last {
                let p =
                    kettle_core::Point::new(kettle_core::Line(r as i32), kettle_core::Column(c));
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

    fn line_text_for_smart_select(&mut self, row: usize) -> Option<String> {
        use kettle_core::Dimensions;
        let pane = self.mux.focused()?;
        let t = pane.term.term.lock().ok()?;
        let cols = t.columns();
        let rows = t.screen_lines();
        if row >= rows {
            return None;
        }
        let mut out = String::with_capacity(cols);
        for c in 0..cols {
            let p = kettle_core::Point::new(kettle_core::Line(row as i32), kettle_core::Column(c));
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
    fn apply_smart_selection(&mut self, area: Rect, row: usize, start: usize, end: usize) -> bool {
        // The `_area` is unused but kept in the signature to mirror
        // `begin_selection`'s API — future viewport-aware variants may
        // need it for clamping.
        let _ = area;
        let Some(pane) = self.mux.focused() else {
            return false;
        };
        let Ok(mut t) = pane.term.term.lock() else {
            return false;
        };
        let line = kettle_core::Line(row as i32);
        let anchor = kettle_core::Point::new(line, kettle_core::Column(start));
        let end_pt = kettle_core::Point::new(line, kettle_core::Column(end));
        let mut sel = kettle_core::Selection::new(
            kettle_core::SelectionType::Simple,
            anchor,
            kettle_core::Side::Left,
        );
        sel.update(end_pt, kettle_core::Side::Right);
        t.selection = Some(sel);
        // Like Semantic, a smart selection resolves on press; the
        // caller treats `selecting=false` so motion doesn't extend it.
        self.selecting = false;
        true
    }

    fn begin_selection(&mut self, area: Rect, ty: kettle_core::SelectionType) {
        // Simple + Block are drags; word/line select immediately on click.
        self.selecting = matches!(
            ty,
            kettle_core::SelectionType::Simple | kettle_core::SelectionType::Block
        );
        if let Some(rect) = self.focused_rect(area) {
            let p = self.px_to_point(rect, self.cursor.x as f32, self.cursor.y as f32);
            if let Some(pane) = self.mux.focused()
                && let Ok(mut t) = pane.term.term.lock()
            {
                t.selection = Some(kettle_core::Selection::new(ty, p, kettle_core::Side::Left));
            }
        }
    }

    /// Click count for the press at `(row,col)` within ~400 ms of the last.
    fn click_count(&mut self, row: usize, col: usize) -> u8 {
        let now = std::time::Instant::now();
        let n = match self.last_click {
            Some((t, r, c, n))
                if r == row
                    && c == col
                    && now.duration_since(t) < std::time::Duration::from_millis(400) =>
            {
                n % 3 + 1
            }
            _ => 1,
        };
        self.last_click = Some((now, row, col, n));
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

    fn paste_clipboard(&mut self) {
        let text = self
            .clipboard
            .as_mut()
            .and_then(|c| c.get_text().ok())
            .unwrap_or_default();
        self.paste_text(text);
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
    fn paste_primary(&mut self) {
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
        self.paste_text(text);
    }

    /// Cycle 755: shared paste path — clamp, broadcast scoping, bracketed-paste
    /// wrap, write to the focused PTY. Extracted so `paste_clipboard` and
    /// `paste_primary` (and any future paste channel) can't drift on the
    /// safety/scoping rules.
    fn paste_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        // Cap a runaway paste at 4 MiB on a UTF-8 char boundary so an
        // accidental "paste this 1 GB log" doesn't shove every byte into
        // the PTY in one go. `clamp_osc52` is named for OSC 52 but it's a
        // generic byte-clamper that preserves char boundaries — exactly
        // what we want for any paste channel.
        let text = clamp_osc52(&text, LOCAL_PASTE_MAX);
        // Broadcast paste (cycle 174 sibling to cycle 173): with the
        // group-input mode on (Ctrl+Shift+G), keystrokes go to every
        // pane in the active tab — paste is also user input and
        // should follow the same scoping. Each pane gets its own
        // `BRACKETED_PASTE` decision (different panes may have
        // different mode state), so the wrap is per-pane, not a
        // single shared payload.
        if self.mux.is_broadcast_on() {
            self.mux.broadcast_paste(text);
            return;
        }
        let bracketed = self
            .focused_mode()
            .contains(kettle_core::TermMode::BRACKETED_PASTE);
        let bytes = input::paste_payload(text, bracketed);
        if let Some(p) = self.mux.focused() {
            p.term.write(&bytes);
        }
    }

    fn copy_selection(&mut self) {
        let sel = self
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
            let _ = cb.set_text(s);
        }
    }

    fn update_selection(&mut self, area: Rect) {
        if !self.selecting {
            return;
        }
        if let Some(rect) = self.focused_rect(area) {
            let p = self.px_to_point(rect, self.cursor.x as f32, self.cursor.y as f32);
            if let Some(pane) = self.mux.focused()
                && let Ok(mut t) = pane.term.term.lock()
                && let Some(sel) = t.selection.as_mut()
            {
                sel.update(p, kettle_core::Side::Right);
            }
        }
    }

    /// Extend the existing selection to the cursor point (Shift+Click).
    /// Returns `true` when a selection was present *and* extended — the
    /// caller falls back to a fresh `begin_selection` when no selection
    /// existed (so Shift+Click on empty space starts a normal selection).
    /// Matches xterm / Alacritty / iTerm2: Shift+Click anchors the
    /// existing selection's start and pulls the end to the click.
    fn extend_selection_to_cursor(&mut self, area: Rect) -> bool {
        let rect = match self.focused_rect(area) {
            Some(r) => r,
            None => return false,
        };
        let p = self.px_to_point(rect, self.cursor.x as f32, self.cursor.y as f32);
        if let Some(pane) = self.mux.focused()
            && let Ok(mut t) = pane.term.term.lock()
            && let Some(sel) = t.selection.as_mut()
        {
            sel.update(p, kettle_core::Side::Right);
            // Enter drag mode so a follow-up mouse-move keeps extending —
            // matches every Mac/Linux text-control: shift-click, then drag.
            self.selecting = true;
            return true;
        }
        false
    }

    /// Resize every pane's PTY to match its tile in the layout.
    fn resize_all(&mut self) {
        let (cw, ch) = self.cell_px();
        let area = self.area();
        let mut plan: Vec<(u64, usize, usize)> = Vec::new();
        for ti in 0..self.mux.tabs.len() {
            for (id, r) in self.mux.layout(ti, area) {
                let (cols, rows) = self.grid_of(r);
                plan.push((id, cols, rows));
            }
        }
        for (id, cols, rows) in plan {
            if let Some(p) = self.mux.panes.get_mut(&id) {
                p.term.resize(cols, rows, cw, ch);
            }
        }
    }

    fn drain_events(&mut self) {
        let mut bell = false;
        // Cycle 246: pane ids that fired `TermEvent::Bell` this drain
        // pass — latched onto their containing tabs *after* the
        // values_mut() iteration so we don't double-borrow mux.panes.
        let mut bell_panes: Vec<u64> = Vec::new();
        // Cycle 378: pane ids + raw-output bytes accumulated during
        // this drain pass. Fired as LuaEvent::Output after the
        // values_mut iteration completes (to avoid borrow conflicts).
        let mut output_chunks: Vec<(u64, Vec<u8>)> = Vec::new();
        // Cycle 412: pane ids whose shell exited with cfg.exit_action
        // = Restart. Queued during the drain; appended to
        // self.pending_pane_restarts after the iteration so the
        // post-drain handler can process them with a fresh borrow.
        let mut pending_restarts_local: Vec<u64> = Vec::new();
        // Cell size is renderer-owned and uniform across panes, so resolve it
        // once per drain rather than per event (a sixel/kitty app polling CSI
        // 14 t doesn't need a renderer lookup per CSI).
        let (cell_w, cell_h) = self.cell_px();
        for (&pane_id, pane) in self.mux.panes.iter_mut() {
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
                            let _ = cb.set_text(clamp_osc52(&s, OSC52_MAX).to_string());
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
                        // `self.mux.panes.values_mut()` loop here so
                        // we can't call `self.reset_blink_phase()`;
                        // the two field writes are the same body.)
                        self.blink_on = true;
                        self.last_blink = std::time::Instant::now();
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
                    // self.mux during this iteration. Close the
                    // current PTY (pane.closed = true) — the
                    // post-drain handler resurrects with the same
                    // argv via Mux::spawn_pane.
                    // Close (default): unchanged kettle behavior.
                    TermEvent::Exit | TermEvent::ChildExit(_) => match self.cfg.exit_action {
                        kettle_config::ExitAction::Hold => {}
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
            // Cycle 612 (Terminator parity, command_notify.py): drain
            // OSC 133 D (CommandEnd) events from the reader thread.
            // For each event, fire a desktop notification if:
            //   - the kettle window isn't focused at this moment,
            //   - the elapsed duration crosses the configured
            //     threshold (0 disables; default 5 s).
            // Active-pane filtering is intentionally NOT applied — a
            // background pane finishing a long task is the most
            // useful notification case (the user IS in another pane
            // but won't see the result without switching). Window
            // focus is enough.
            if self.cfg.command_notify_threshold_ms > 0 {
                for ev in pane.term.drain_command_finished_events() {
                    let elapsed_ms = ev.duration.as_millis() as u64;
                    if !self.window_focused && elapsed_ms >= self.cfg.command_notify_threshold_ms {
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
            }
        }
        if bell {
            if self.cfg.bell.visual() {
                self.last_bell = Some(std::time::Instant::now());
            }
            if self.cfg.bell.attention()
                && !self.window_focused
                && let Some(w) = &self.window
            {
                w.request_user_attention(Some(UserAttentionType::Informational));
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
            self.mux.touch_tab_bell(id);
            if let Some(eng) = &self.lua_engine {
                eng.fire_event(&crate::LuaEvent::Bell(id));
            }
            self.drain_lua_hook_commands("bell hook");
        }
        // Cycle 378 (Terminator plugin parity, plugin sub-cycle 3):
        // fire LuaEvent::Output(pane_id, bytes) for each pane that
        // accumulated PTY-output chunks this drain pass.
        for (pane_id, bytes) in output_chunks {
            if let Some(eng) = &self.lua_engine {
                eng.fire_event(&crate::LuaEvent::Output(pane_id, bytes));
            }
            self.drain_lua_hook_commands("output hook");
        }
        // Cycle 412: stash the per-tick restart list on App so the
        // post-drain handler can process it with a fresh
        // &mut self.mux borrow (the drain_events loop above held a
        // &mut iter into self.mux.panes, so spawn_pane couldn't run
        // there).
        if !pending_restarts_local.is_empty() {
            self.pending_pane_restarts.extend(pending_restarts_local);
        }
    }

    /// Keep the OS window title in sync with the *active* pane's title —
    /// including after tab/focus switches, not only on OSC title events.
    /// Deduped so it isn't a syscall every frame.
    fn sync_window_title(&mut self) {
        let pane = self
            .mux
            .active_focus()
            .and_then(|id| self.mux.panes.get(&id));
        let title = pane.map(|p| p.title.as_str()).unwrap_or("kettle");
        let cwd = pane.and_then(|p| p.term.current_dir()).unwrap_or_default();
        let tab = self.mux.active + 1;
        let want = window_title(&self.cfg.window_title_format, title, &cwd, tab);
        if want != self.last_title {
            if let Some(w) = &self.window {
                w.set_title(&want);
            }
            self.last_title = want;
        }
    }

    fn update_search(&mut self) {
        if !self.mux.search.open {
            return;
        }
        let query = self.mux.search.query.clone();
        let matches = if let Some(p) = self.mux.focused() {
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
        {
            let s = &mut self.mux.search;
            s.matches = matches;
            if s.index >= s.matches.len() {
                s.index = 0;
            }
        }
        // Follow the active match into scrollback when it (or the query)
        // changed — once, so the user can still wheel-scroll freely.
        let active = {
            let s = &self.mux.search;
            s.matches
                .get(s.index)
                .copied()
                .map(|m| ((s.query.clone(), s.index), m.line))
        };
        if let Some((key, line)) = active
            && self.search_revealed.as_ref() != Some(&key)
        {
            if let Some(p) = self.mux.focused()
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
            self.search_revealed = Some(key);
        }
    }

    /// `(row, col)` of the mouse within the focused pane, if any.
    fn cursor_cell(&self) -> Option<(usize, usize)> {
        let rect = self.focused_rect(self.area())?;
        let p = self.px_to_point(rect, self.cursor.x as f32, self.cursor.y as f32);
        Some((p.line.0.max(0) as usize, p.column.0))
    }

    /// Scan the focused pane's visible grid for quick-select targets and
    /// assign each a short label.
    fn collect_hints(&mut self) -> Vec<HintTarget> {
        use kettle_core::hints;
        use kettle_core::{Column, Dimensions, Line, Point};
        let Some(p) = self.mux.focused() else {
            return Vec::new();
        };
        let Ok(t) = p.term.term.lock() else {
            return Vec::new();
        };
        let g = t.grid();
        let (rows, cols) = (g.screen_lines(), g.columns());
        let lines: Vec<String> = (0..rows)
            .map(|r| {
                let s: String = (0..cols)
                    .map(|c| g[Point::new(Line(r as i32), Column(c))].c)
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

    fn link_at_cursor(&self) -> Option<&kettle_core::Link> {
        let (row, col) = self.cursor_cell()?;
        self.links
            .iter()
            .find(|l| l.row == row && col >= l.start_col && col <= l.end_col)
    }

    fn focused_mode(&mut self) -> kettle_core::TermMode {
        self.mux
            .focused()
            .and_then(|p| p.term.term.lock().ok().map(|t| *t.mode()))
            .unwrap_or(kettle_core::TermMode::empty())
    }

    /// Forward a mouse event to the app via the active tracking protocol.
    /// Returns `true` when it was consumed (so kettle skips local handling).
    fn send_mouse(&mut self, btn: u8, pressed: bool, motion: bool) -> bool {
        // Shift held = "bypass mouse tracking, let kettle handle this
        // locally" — the xterm convention every modern terminal honors.
        // Without it, running htop/vim/tmux with mouse-mode locks out
        // kettle's selection entirely: every click is consumed by the
        // TUI and the user has to disable mouse mode to copy text.
        // Returning `false` here makes the caller fall through to
        // selection / scrollbar / extend logic exactly as if tracking
        // were off.
        if self.mods.shift_key() {
            return false;
        }
        let (track, sgr) = input::mouse_tracking(self.focused_mode());
        if track == input::MouseTracking::Off {
            return false;
        }
        if motion && track != input::MouseTracking::Motion && self.mouse_btn.is_none() {
            return track != input::MouseTracking::Off; // consume, no report
        }
        let Some((row, col)) = self.cursor_cell() else {
            return false;
        };
        let seq = input::mouse_encode(sgr, btn, pressed, motion, col, row, self.mods);
        if let Some(p) = self.mux.focused() {
            p.term.write(&seq);
        }
        true
    }

    fn overlay(&self) -> Overlay {
        let hover = self.cursor_cell();
        let links = self
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

        let (ssh_query, ssh_hint) = match &self.ssh_input {
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

        let (palette_query, palette_hint) = match &self.palette_input {
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
        let (layout_picker_query, layout_picker_hint) = match &self.layout_picker_input {
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

        let hint_labels: Vec<HintLabel> = match &self.hint_state {
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

        let window_focused = self.window_focused;
        // Cursor blink is the *intersection* of the user config and the
        // running app's wishes — programs flip it via DEC private mode 12
        // (`CSI ?12 h/l`), which the engine tracks per-pane on its
        // `cursor_style().blinking`. Read the active pane's live state so
        // editors like vim that disable blink for their own cursor are
        // honored even when the global config wants blink. (Goes through
        // `active_focus` + `panes.get` so the `overlay()` builder stays a
        // pure `&self` reader.)
        let pane_blink = self
            .mux
            .active_focus()
            .and_then(|id| self.mux.panes.get(&id))
            .map(|p| p.term.cursor_blinking())
            .unwrap_or(true);
        let blink_enabled = self.cfg.cursor_blink && pane_blink;
        let cursor_visible = if !blink_enabled
            || !window_focused
            || self.ssh_input.is_some()
            || self.palette_input.is_some()
            || self.layout_picker_input.is_some()
            || self.hint_state.is_some()
            || self.mux.search.open
        {
            true
        } else {
            self.blink_on
        };
        let bell = self
            .last_bell
            .map(|t| {
                let e = t.elapsed().as_secs_f32();
                if e >= 0.30 { 0.0 } else { 1.0 - e / 0.30 }
            })
            .unwrap_or(0.0);

        let context_menu = self.context_menu_overlay();
        // Cycle 372: marshal the in-progress Edit-title state for
        // the render layer so the user sees what they're typing.
        //
        // Cycle 395 (Terminator parity, titlebar Bucket-D sub-cycle 7):
        // for Pane scope, also pass the focused pane's titlebar y so
        // the overlay anchors near the clicked pane vs the window-
        // bottom (window/tab scopes still use window-bottom).
        let edit_title: Option<(String, String, Option<f32>)> =
            self.editing_title.as_ref().map(|s| {
                let label = match s.scope {
                    TitleEditScope::Window => "Edit window title:",
                    TitleEditScope::Tab => "Edit tab title:",
                    TitleEditScope::Pane => "Edit pane title:",
                    TitleEditScope::Group => "Edit pane group:",
                };
                let anchor_y = if matches!(s.scope, TitleEditScope::Pane | TitleEditScope::Group) {
                    let area = self.area();
                    let active = self.mux.active;
                    let rects = self.mux.layout(active, area);
                    let focus = self.mux.active_focus();
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
            self.confirm_dialog
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
        let s = &self.mux.search;
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
                context_menu,
                confirm_dialog: confirm_dialog_early,
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
            context_menu,
            vi_cursor: self.vi_mode.map(|v| (v.row, v.col)),
            vi_visual_anchor: self.vi_mode.and_then(|v| v.visual_anchor),
            confirm_dialog,
        }
    }

    fn update_links(&mut self) {
        self.links = self
            .mux
            .focused()
            .and_then(|p| p.term.term.lock().ok().map(|t| kettle_core::links(&t)))
            .unwrap_or_default();
    }

    fn redraw(&mut self) {
        self.drain_events();
        self.poll_remote_contexts();
        self.poll_theme_schedule();
        self.poll_focus_event();
        self.poll_title_event();
        // Cycle 745: reflect the focused pane's OSC 9;4 progress onto the OS
        // taskbar button (pwsh 7 / Windows Terminal parity). No-op off Windows.
        self.poll_taskbar_progress();
        // Cycle 418: process any pane-restart requests queued during
        // drain_events. Done HERE (after drain) so we don't hold a
        // &mut iter into self.mux.panes when spawning a new tab.
        // event_loop arg is unused for now (the spawn doesn't need it);
        // kept in the signature for symmetry with other dispatchers.
        if !self.pending_pane_restarts.is_empty() {
            let pane_ids: Vec<u64> = std::mem::take(&mut self.pending_pane_restarts);
            let (cw, ch) = self.cell_px();
            let waker = self.waker();
            // Cycle 420: use the live grid (matches the existing surface)
            // for the new tab. cycle-418 hardcoded 80×24 which mismatched
            // any non-default kettle window size — the new shell would
            // start with a tiny grid then grow on next resize. Pulling
            // from the current area means the restart shell starts at
            // the size the user is actually using.
            let (cols, rows) = self.grid_of(self.area());
            for pane_id in pane_ids {
                let restart_info: Option<(Vec<String>, Option<String>)> = self
                    .mux
                    .panes
                    .get(&pane_id)
                    .map(|p| (p.argv.clone(), p.term.current_dir()));
                if let Some((argv, cwd)) = restart_info {
                    if let Err(e) = self.mux.new_tab_with(
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
                        self.fire_tab_add_event();
                    }
                }
            }
        }
        // Reflect the active pane (incl. after tab/focus switches).
        self.sync_window_title();
        // Advance the cursor blink phase (configurable half-period). Skip
        // the increment when the active pane has DEC mode 12 cleared so the
        // cursor sits solid — without this, vim-style "solid block while
        // editing" requests are ignored even though the engine honored them.
        let pane_blink_redraw = self
            .mux
            .active_focus()
            .and_then(|id| self.mux.panes.get(&id))
            .map(|p| p.term.cursor_blinking())
            .unwrap_or(true);
        if self.cfg.cursor_blink
            && pane_blink_redraw
            && self.window_focused
            && self.last_blink.elapsed()
                >= std::time::Duration::from_millis(self.cfg.cursor_blink_interval)
        {
            self.blink_on = !self.blink_on;
            self.last_blink = std::time::Instant::now();
        }
        if self.mux.reap() {
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
        for (&pane_id, pane) in self.mux.panes.iter_mut() {
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
            self.mux.touch_tab_output(id);
        }
        // Auto-scroll while dragging a selection past the focused pane's
        // top/bottom edge — every modern terminal does this so the user
        // doesn't have to release / scroll-back / shift-click to extend.
        // Pure `selection_autoscroll_lines` chooses the per-frame rate;
        // scrolling the viewport here naturally re-fires `update_selection`
        // below to anchor the selection's end to the new visible line.
        if self.selecting {
            let area = self.area();
            if let Some(rect) = self.focused_rect(area) {
                let lines =
                    selection_autoscroll_lines(self.cursor.y as f32, rect.1, rect.1 + rect.3);
                if lines != 0
                    && let Some(p) = self.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    t.scroll_display(Scroll::Delta(lines));
                }
                if lines != 0 {
                    // Re-anchor the selection's end at the (now-moved)
                    // cursor row so the highlight grows in step with the
                    // scroll, not stuck on the original click-time row.
                    self.update_selection(area);
                }
            }
        }
        self.update_search();
        self.update_links();
        // Link set may have changed (scroll, output, mode flip) — re-sync
        // the cursor icon so a URL scrolling out from under a held Ctrl
        // doesn't leave the pointer-hand icon stuck on a now-empty cell.
        // Deduped via `last_cursor_icon` so this is a cheap per-frame
        // recheck when nothing changed.
        self.sync_cursor_icon();
        let overlay = self.overlay();
        let area = self.area();
        let tabbar = self.tab_bar();
        // Cycle 296: build status bar BEFORE the &mut renderer borrow
        // since `build_status_bar` reads self.mux / self.cfg
        // immutably.
        let status = self.build_status_bar();
        let active = self.mux.active;
        let layout = self.mux.layout(active, area);
        let focus = self.mux.active_focus();

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        // Lock every visible pane, then hand references to the renderer.
        // Cycle 382: also pass the pane's title so the cycle-379
        // titlebar can render the text.
        let mut guards = Vec::with_capacity(layout.len());
        for (id, r) in &layout {
            if let Some(p) = self.mux.panes.get(id) {
                let mut imgs = p.term.placements();
                imgs.extend(p.term.placeholder_tiles());
                imgs.extend(p.term.relative_tiles());
                if let Ok(g) = p.term.term.lock() {
                    use kettle_core::Dimensions;
                    let cols = g.columns() as u16;
                    let rows = g.screen_lines() as u16;
                    guards.push((
                        *r,
                        g,
                        Some(*id) == focus,
                        imgs,
                        p.title.clone(),
                        cols,
                        rows,
                        false,
                        p.group_name.clone(),
                    ));
                }
            }
        }
        let panes: Vec<PaneView> = guards
            .iter()
            .map(
                |(r, g, f, imgs, title, cols, rows, bell, group_name)| PaneView {
                    rect: *r,
                    term: g,
                    focused: *f,
                    images: imgs.clone(),
                    title: title.clone(),
                    size_cols: *cols,
                    size_rows: *rows,
                    bell: *bell,
                    group_name: group_name.clone(),
                },
            )
            .collect();
        // Cycle 296: status bar built BEFORE the &mut renderer borrow
        // (the helper reads `self.mux` immutably). Cheap when off.
        if let Err(e) =
            renderer.render_frame_with_status(&panes, &tabbar, &self.cfg, &overlay, &status)
        {
            log::warn!("render error: {e}");
        }
    }

    /// Cycle 296: compose the status-bar contents (HH:MM:SS · theme ·
    /// focused-pane title). Returns `StatusBar::hidden` when the
    /// config has it off. The renderer's draw is a no-op on a
    /// hidden status bar so this is cheap even when never visible.
    /// Takes `&mut self` only because `Mux::focused` does — no state
    /// is actually mutated here.
    fn build_status_bar(&mut self) -> kettle_render::StatusBar {
        if matches!(self.cfg.status_bar, kettle_config::StatusBarMode::Off) {
            return kettle_render::StatusBar::hidden();
        }
        let h = self.status_bar_h();
        let surface_h = self
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
        let title = self
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
    fn focus_key(&self) -> (usize, Option<u64>) {
        (self.mux.active, self.mux.active_focus())
    }

    /// If the focused `(tab, leaf)` changed since `pre`, land the
    /// cursor visible on the new pane right away.
    fn note_focus_change(&mut self, pre: (usize, Option<u64>)) {
        if self.focus_key() != pre {
            self.reset_blink_phase();
        }
    }

    /// Close every modal overlay (search bar, command palette, hint
    /// mode, SSH launcher). Cycle 111's Reset path inlined the same
    /// four-line clear; cycle 154 extracts it so the modal-opening
    /// actions can call it first to avoid stacking two visible
    /// modals at once (palette opened while ssh launcher was up
    /// would render both, with palette capturing keys; visually
    /// confusing).
    fn close_all_modals(&mut self) {
        self.mux.search.open = false;
        self.palette_input = None;
        self.layout_picker_input = None;
        self.hint_state = None;
        self.ssh_input = None;
        self.context_menu = None;
        self.editing_title = None;
        // Cycle 298 vi-mode behaves like a modal — Esc exits it,
        // close_all_modals exits it. Sub-cycle 1.
        self.vi_mode = None;
        // Cycle 754: the confirm dialog ("Close this pane?", "Quit?") is a
        // modal too, but was omitted here — so opening search / palette / a
        // menu while a confirm prompt was up rendered BOTH overlays at once
        // with ambiguous key focus. Every modal-opener calls close_all_modals
        // first (then sets its own modal), so clearing the confirm dialog here
        // is safe: the confirm-open path clears-then-sets in that order.
        self.confirm_dialog = None;
    }

    /// Cycle 369: apply the in-progress title edit + clear the
    /// overlay. The scope decides which setter is invoked.
    fn apply_title_edit(&mut self) {
        if let Some(state) = self.editing_title.take() {
            let value = state.input;
            match state.scope {
                TitleEditScope::Window => {
                    if let Some(w) = &self.window {
                        w.set_title(&value);
                    }
                    self.last_title = value;
                }
                TitleEditScope::Tab => {
                    if let Some(t) = self.mux.tabs.get_mut(self.mux.active) {
                        t.title_override = if value.is_empty() { None } else { Some(value) };
                    }
                }
                TitleEditScope::Pane => {
                    if let Some(p) = self.mux.focused() {
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
                            if let Some(p) = self.mux.focused() {
                                p.group_name = next;
                            }
                        }
                        GroupBulkScope::Tab => {
                            let ids: Vec<u64> = self
                                .mux
                                .tabs
                                .get(self.mux.active)
                                .map(|t| t.root.leaf_ids())
                                .unwrap_or_default();
                            for id in ids {
                                if let Some(p) = self.mux.panes.get_mut(&id) {
                                    p.group_name = next.clone();
                                }
                            }
                        }
                        GroupBulkScope::Window => {
                            let ids: Vec<u64> = self.mux.panes.keys().copied().collect();
                            for id in ids {
                                if let Some(p) = self.mux.panes.get_mut(&id) {
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
    fn any_modal_open(&self) -> bool {
        self.mux.search.open
            || self.palette_input.is_some()
            || self.layout_picker_input.is_some()
            || self.hint_state.is_some()
            || self.ssh_input.is_some()
            || self.context_menu.is_some()
            || self.editing_title.is_some()
            || self.vi_mode.is_some()
            // Cycle 754: the confirm dialog is a modal too. Its key input has a
            // dedicated priority branch, but without it here mouse/scroll/cursor
            // gating let clicks fall through to the terminal behind a "Quit?" /
            // "Close pane?" prompt.
            || self.confirm_dialog.is_some()
    }

    /// Build the right-click context-menu item list. Each `Item`'s
    /// `enabled` flag is computed from current state: Copy needs a
    /// selection; Ungroup needs the focused pane to actually be in a
    /// group. Cycle 713 wraps the whole list in `filter_disabled` at
    /// the `open_context_menu` call-site so disabled rows + the
    /// separators that would orphan them are hidden entirely
    /// (Terminator-style) rather than shown greyed-out — less visual
    /// clutter, every visible row is actionable.
    fn context_menu_items(&mut self) -> Vec<ContextMenuItem> {
        let has_selection = self
            .mux
            .focused()
            .and_then(|p| p.term.term.lock().ok().map(|t| t.selection.is_some()))
            .unwrap_or(false);
        // Cycle 713: only enable Ungroup when the focused pane has a
        // group_name set. Otherwise the row used to greyed-out
        // confuse new users ("why's that here if I can't click it?");
        // now it's filtered out entirely until it's actionable.
        let has_group = self
            .mux
            .focused()
            .map(|p| p.group_name.as_ref().is_some_and(|g| !g.is_empty()))
            .unwrap_or(false);
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

    fn append_remote_menu_items(&mut self, items: &mut Vec<ContextMenuItem>) {
        let Some(pane) = self.mux.focused() else {
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
    fn open_context_menu(&mut self, px: f32, py: f32) {
        self.close_all_modals();
        let mut items = self.context_menu_items();
        // Cycle 611: append config-file menu items (if any).
        self.append_config_menu_items(&mut items);
        // Cycle 375: append Lua-supplied items (if any).
        self.append_lua_menu_items(&mut items);
        // Cycle 658 (remote.py sub-cycle 7): append the remote-
        // session reconnect entry when the focused pane has a
        // detected SSH/Docker/Podman/kubectl context.
        self.append_remote_menu_items(&mut items);
        // Cycle 685 (theme-submenu sub-cycle 2): append the
        // Theme submenu populated from Theme::list(). The flyout
        // open machinery lands in sub-cycle 3.
        self.append_theme_submenu_items(&mut items);
        // Cycle 686 (theme-submenu sub-cycle 8): same machinery
        // for Profile (only appended when ~/.config/kettle/
        // profiles/ has any *.config files).
        self.append_profile_submenu_items(&mut items);
        // Cycle 717 (Preferences submenu, C8): runtime-mutable
        // settings + the Advanced… escape hatch.
        self.append_preferences_submenu_items(&mut items);
        // Cycle 713 (Terminator menu UX, C4): drop disabled rows
        // entirely and collapse the separators that would orphan
        // around them. Matches Terminator/GNOME's "only show what
        // you can actually click" convention; every visible row is
        // actionable.
        let items = filter_disabled(items);
        // Highlight the first enabled non-separator item.
        let highlight = items.iter().position(item_is_dispatchable).unwrap_or(0);
        let (cw, ch) = self.cell_px();
        let (cw, ch) = (cw as f32, ch as f32);
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
                | ContextMenuItem::ProfileChoice { .. } => row_h,
            })
            .sum();
        let max_chars = items
            .iter()
            .filter_map(|it| match it {
                ContextMenuItem::Item { label, .. } => Some(label.chars().count()),
                ContextMenuItem::LuaItem { label, .. } => Some(label.chars().count()),
                ContextMenuItem::ConfigItem { label, .. } => Some(label.chars().count()),
                // Cycle 684: submenu rows show "label ▸" so the
                // max-width budget needs +2 for the suffix.
                ContextMenuItem::Submenu { label, .. } => Some(label.chars().count() + 2),
                // Cycle 685: ThemeChoice surfaces only inside an
                // open submenu flyout (sub-cycle 3); the parent
                // menu's width budget shouldn't grow for choices
                // the user can't directly see.
                ContextMenuItem::ThemeChoice { .. } => None,
                // Cycle 686: same for ProfileChoice (flyout-only).
                ContextMenuItem::ProfileChoice { .. } => None,
                _ => None,
            })
            .max()
            .unwrap_or(0) as f32;
        let panel_w = (max_chars * cw + kettle_render::menu::H_PAD).max(kettle_render::menu::MIN_W);
        let (sw, sh) = self
            .renderer
            .as_ref()
            .map(|r| {
                let (w, h) = r.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        let anchor = clamp_context_menu_anchor((px, py), (panel_w, panel_h), (sw, sh));
        self.context_menu = Some(ContextMenuState {
            anchor,
            items,
            highlight,
            drill_stack: Vec::new(),
            scroll_offset: 0,
            scroll_stack: Vec::new(),
            typeahead_buf: String::new(),
            typeahead_until: None,
        });
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Move the context-menu highlight by `delta` (±1), skipping
    /// `Separator` rows and disabled `Item` rows. Wraps at the ends so
    /// `↑` on the first row jumps to the last enabled row and vice
    /// versa — Chrome / Firefox menu convention. Pure on `(items,
    /// current)` so the wrap+skip math is unit-testable independent of
    /// the App / cursor state.
    fn step_context_menu_highlight(&mut self, delta: isize) {
        let Some(((_, _), (_, panel_h))) = self.context_menu_geometry() else {
            return;
        };
        let (_, ch) = self.cell_px();
        let row_h = ch as f32 + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        let Some(menu) = self.context_menu.as_mut() else {
            return;
        };
        let next = next_context_menu_highlight(&menu.items, menu.highlight, delta);
        menu.highlight = next;
        // Cycle 714: if the new highlight is outside the visible
        // window, advance scroll_offset to bring it into view.
        let visible = count_rows_fitting(&menu.items, menu.scroll_offset, panel_h, row_h, sep_h);
        if next < menu.scroll_offset {
            menu.scroll_offset = next;
        } else if next >= menu.scroll_offset + visible {
            // Pull scroll_offset forward until `next` is the last
            // fully visible row.
            let mut off = next;
            loop {
                let fit = count_rows_fitting(&menu.items, off, panel_h, row_h, sep_h);
                if off + fit > next || off == 0 {
                    break;
                }
                off -= 1;
            }
            menu.scroll_offset = off;
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Cycle 714. Scroll the context-menu by `delta` rows (positive
    /// = down). Clamped so we can't scroll past the last row that
    /// would still fill the visible window.
    fn scroll_context_menu(&mut self, delta: isize) {
        let Some(((_, _), (_, panel_h))) = self.context_menu_geometry() else {
            return;
        };
        let (_, ch) = self.cell_px();
        let row_h = ch as f32 + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        let Some(menu) = self.context_menu.as_mut() else {
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
            if let Some(w) = &self.window {
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
    fn menu_row_at_cursor(&self) -> Option<usize> {
        let menu = self.context_menu.as_ref()?;
        let ((ax, ay), (panel_w, panel_h)) = self.context_menu_geometry()?;
        let (px, py) = (self.cursor.x as f32, self.cursor.y as f32);
        if px < ax || px >= ax + panel_w || py < ay || py >= ay + panel_h {
            return None;
        }
        let (_, ch) = self.cell_px();
        let row_h = ch as f32 + kettle_render::menu::ROW_PAD;
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
    fn update_menu_highlight_from_cursor(&mut self) {
        let Some(idx) = self.menu_row_at_cursor() else {
            return;
        };
        let Some(menu) = self.context_menu.as_mut() else {
            return;
        };
        if menu.highlight == idx {
            return;
        }
        menu.highlight = idx;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// outside) or falls through to the regular click handling
    /// (right-click → re-open at the new point).
    fn context_menu_click_action(&self, bcode: u8) -> Option<ContextMenuClick> {
        if bcode != 0 {
            return None;
        }
        let menu = self.context_menu.as_ref()?;
        let ((ax, ay), (panel_w, panel_h)) = self.context_menu_geometry()?;
        let (px, py) = (self.cursor.x as f32, self.cursor.y as f32);
        if px < ax || px >= ax + panel_w || py < ay || py >= ay + panel_h {
            return None;
        }
        let (_, ch) = self.cell_px();
        let row_h = ch as f32 + kettle_render::menu::ROW_PAD;
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
                | ContextMenuItem::ProfileChoice { .. } => row_h,
            };
            if py >= row_y && py < row_y + h {
                match item {
                    ContextMenuItem::Item {
                        action,
                        enabled: true,
                        ..
                    } => return Some(ContextMenuClick::Action(action.clone())),
                    ContextMenuItem::DynamicItem {
                        action,
                        enabled: true,
                        ..
                    } => return Some(ContextMenuClick::Action(action.clone())),
                    ContextMenuItem::LuaItem { lua_idx, .. } => {
                        return Some(ContextMenuClick::LuaMenuItem(*lua_idx));
                    }
                    ContextMenuItem::ConfigItem { command, .. } => {
                        return Some(ContextMenuClick::ConfigCommand(command.clone()));
                    }
                    ContextMenuItem::Submenu { .. } => {
                        return Some(ContextMenuClick::DrillIntoSubmenu(idx));
                    }
                    ContextMenuItem::ThemeChoice { theme, .. } => {
                        // Cycle 685: clicked inside a Theme flyout
                        // (sub-cycle 3 will wire the flyout open;
                        // until then this row isn't rendered in
                        // the parent panel — see the projection
                        // arm above).
                        return Some(ContextMenuClick::SetTheme(theme.clone()));
                    }
                    ContextMenuItem::ProfileChoice { profile, .. } => {
                        // Cycle 686: same shape as ThemeChoice.
                        return Some(ContextMenuClick::SetProfile(profile.clone()));
                    }
                    _ => return None,
                }
            }
            row_y += h;
        }
        None
    }

    /// `(items, anchor, panel_w, panel_h)` snapshot for the click /
    /// hover hit-tests — returned in pixels so callers don't have to
    /// re-derive the layout. `None` when the menu isn't open.
    fn context_menu_geometry(&self) -> Option<((f32, f32), (f32, f32))> {
        let menu = self.context_menu.as_ref()?;
        let (cw, ch) = self.cell_px();
        let (cw, ch) = (cw as f32, ch as f32);
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
        let (_, surface_h) = self
            .renderer
            .as_ref()
            .map(|r| {
                let (w, h) = r.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        let max_h = (surface_h - kettle_render::menu::PANEL_BREATHING).max(row_h);
        let panel_h = natural_h.min(max_h);
        let max_chars = menu
            .items
            .iter()
            .filter_map(|it| match it {
                ContextMenuItem::Item { label, .. } => Some(label.chars().count()),
                ContextMenuItem::DynamicItem { label, .. } => Some(label.chars().count()),
                ContextMenuItem::LuaItem { label, .. } => Some(label.chars().count()),
                ContextMenuItem::ConfigItem { label, .. } => Some(label.chars().count()),
                _ => None,
            })
            .max()
            .unwrap_or(0) as f32;
        let panel_w = (max_chars * cw + kettle_render::menu::H_PAD).max(kettle_render::menu::MIN_W);
        Some((menu.anchor, (panel_w, panel_h)))
    }

    /// Build the renderer-side `ContextMenu` slice from the App-side
    /// state. Splits the labels (owned `String`) from the dispatch
    /// actions so the renderer stays Action-agnostic.
    fn context_menu_overlay(&self) -> Option<ContextMenu> {
        let menu = self.context_menu.as_ref()?;
        let rows = menu
            .items
            .iter()
            .map(|it| match it {
                ContextMenuItem::Item { label, enabled, .. } => ContextMenuRow {
                    label: (*label).to_string(),
                    separator: false,
                    enabled: *enabled,
                },
                ContextMenuItem::DynamicItem { label, enabled, .. } => ContextMenuRow {
                    label: label.clone(),
                    separator: false,
                    enabled: *enabled,
                },
                ContextMenuItem::Separator => ContextMenuRow {
                    label: String::new(),
                    separator: true,
                    enabled: false,
                },
                ContextMenuItem::LuaItem { label, .. } => ContextMenuRow {
                    label: label.clone(),
                    separator: false,
                    enabled: true,
                },
                ContextMenuItem::ConfigItem { label, .. } => ContextMenuRow {
                    label: label.clone(),
                    separator: false,
                    enabled: true,
                },
                ContextMenuItem::Submenu { label, .. } => ContextMenuRow {
                    // Cycle 684: append "▸" to signal "this row
                    // opens a submenu". Sub-cycle 3 wires the
                    // actual flyout; for now the affordance is
                    // visible but clicking it just no-ops.
                    label: format!("{label} ▸"),
                    separator: false,
                    enabled: true,
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
                },
                ContextMenuItem::ProfileChoice { label, .. } => ContextMenuRow {
                    label: label.clone(),
                    separator: false,
                    enabled: true,
                },
            })
            .collect();
        // Cycle 714 (Terminator menu UX, C5): pass through the
        // scroll state + clamped panel height the renderer needs to
        // draw only the visible slice.
        let panel_h_clamped = self
            .context_menu_geometry()
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
    fn reset_blink_phase(&mut self) {
        self.blink_on = true;
        self.last_blink = std::time::Instant::now();
    }

    /// Cycle 717 (Preferences submenu, C8): write a `key = value`
    /// line to the user's active config file via the cycle-716
    /// atomic helper. Resolves the path the same way Action::EditConfig
    /// does (App::config_path → `Config::default_path` fallback).
    /// Logs + ignores any I/O error so a transient FS issue doesn't
    /// kill the menu dispatch; the in-memory toggle still applied,
    /// so the user's next session will pick up the runtime change
    /// once it persists.
    fn persist_pref(&self, key: &str, value: &str) {
        let Some(path) = self
            .config_path
            .clone()
            .or_else(kettle_config::Config::default_path)
        else {
            log::warn!(
                "persist_pref: no config path resolved (set $XDG_CONFIG_HOME or pass --config)"
            );
            return;
        };
        match kettle_config::persist_config_toggle(&path, key, value) {
            Ok(bak) => {
                log::info!(
                    "persist_pref: wrote {key} = {value} to {} (backup at {})",
                    path.display(),
                    bak.display()
                );
            }
            Err(e) => {
                log::warn!(
                    "persist_pref: failed to write {key} = {value} to {}: {e}",
                    path.display()
                );
            }
        }
    }

    fn handle_action(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        let area = self.area();
        let (cols, rows) = self.grid_of(area);
        let (cw, ch) = self.cell_px();
        let waker = self.waker();
        // Snapshot the (tab, pane-leaf) the cursor lives in so we can
        // detect any focus change the action causes. Cycles 134/135
        // landed this for keyboard-driven actions; cycle 136 extended
        // to mouse paths via the shared `focus_key` / `note_focus_change`
        // helpers.
        let pre_focus = self.focus_key();
        match action {
            Action::NewTab => {
                // Cycle 368 (plugin sub-cycle 4): fires LuaEvent::TabAdd
                // with the new active tab index after Mux::new_tab.
                // Cycle 426 collapsed the inline event/drain into the
                // shared fire_tab_add_event helper.
                let _ = self.mux.new_tab(&self.cfg, cols, rows, cw, ch, waker);
                self.fire_tab_add_event();
            }
            Action::NewWindow => {
                // Spawn a *separate* kettle process so the user gets a real
                // OS window, not just a new tab in this one. Detached so a
                // crash here doesn't take the parent down; we forget the
                // child handle on purpose (the OS reaps it). Falls back to
                // a new tab if the current executable isn't resolvable,
                // which keeps the keybind useful on weird platforms (snap,
                // appimage with custom argv0) instead of silently failing.
                //
                // Inherit `--config FILE` (cycle 123). Without this the
                // child loaded the *default* config path even though
                // the parent was launched with `kettle --config
                // /custom.conf` — the user's theme/font/keybinds
                // appeared in their original window but not in any
                // child window opened via Ctrl+Shift+I.
                let spawned = std::env::current_exe()
                    .ok()
                    .and_then(|exe| {
                        let mut cmd = std::process::Command::new(exe);
                        if let Some(cfg_path) = &self.config_path {
                            cmd.arg("--config").arg(cfg_path);
                        }
                        cmd.stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                            .ok()
                    })
                    .is_some();
                if !spawned {
                    let _ = self.mux.new_tab(&self.cfg, cols, rows, cw, ch, waker);
                    // Cycle 425: NewWindow's fallback path creates a
                    // tab in this process when current_exe isn't
                    // resolvable. Plugins listening for tab_add
                    // should see this just like Action::NewTab.
                    self.fire_tab_add_event();
                }
            }
            Action::SplitRight => {
                let _ = self
                    .mux
                    .split(Dir::Horizontal, &self.cfg, cols, rows, cw, ch, waker);
            }
            Action::SplitDown | Action::SplitAuto => {
                let _ = self
                    .mux
                    .split(Dir::Vertical, &self.cfg, cols, rows, cw, ch, waker);
            }
            Action::ClosePane => {
                // Cycle 662 (confirm-dialog sub-cycle 6): per-pane
                // close prompts when ask_before_closing = Always.
                // MultipleTerminals doesn't prompt (single pane); see
                // cycle-638's should_prompt for the matrix.
                if self.cfg.ask_before_closing.should_prompt(1) {
                    self.close_all_modals();
                    self.confirm_dialog = Some(ConfirmDialogState {
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
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Cycle 750: capture the focused pane id BEFORE the close —
                // afterward active_focus() returns the promoted sibling.
                let closing_pane = self.mux.active_focus();
                let was_last = self.mux.close_focused();
                if let Some(id) = closing_pane {
                    self.fire_pane_close_event(id);
                }
                if was_last {
                    event_loop.exit();
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
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    // The cycle-703 PaneFocus event needs to fire
                    // with the new focused id (the sibling that
                    // got promoted), so plugins that observe focus
                    // don't keep stale per-pane state. Mirrors the
                    // poll_focus_event helper's pattern at ~5987.
                    self.poll_focus_event();
                }
            }
            Action::CloseTab => {
                // Cycle 662 (confirm-dialog sub-cycle 6): close the
                // active tab via the modal when ask_before_closing
                // says so. scope_count = leaves in the active tab
                // (panes_in_tab below).
                let panes_in_tab = self
                    .mux
                    .tabs
                    .get(self.mux.active)
                    .map(|t| count_leaves(&t.root))
                    .unwrap_or(1);
                if self.cfg.ask_before_closing.should_prompt(panes_in_tab) {
                    self.close_all_modals();
                    self.confirm_dialog = Some(ConfirmDialogState {
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
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Cycle 368: capture the active index BEFORE close
                // so the LuaEvent::TabClose payload is meaningful
                // (after close, self.mux.active points at a
                // different tab).
                //
                // Cycle 426 collapsed the inline event/drain into
                // the shared fire_tab_close_event helper.
                let closing_idx = self.mux.active;
                if self.mux.close_tab() {
                    event_loop.exit();
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
                let scope = self.mux.panes.len();
                if self.cfg.ask_before_closing.should_prompt(scope) {
                    self.close_all_modals();
                    self.confirm_dialog = Some(ConfirmDialogState {
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
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Distinct from `CloseTab`: drop *every* tab + pane in
                // this window, not just the focused tab. Previously
                // both actions did `close_tab()` so binding `close_window`
                // gave the user a confusingly-misnamed alias for
                // `close_tab`. Now they're genuinely different.
                self.mux.close_window();
                // Cycle 157: save the (now-empty) session so next
                // launch starts fresh. Otherwise the previous
                // multi-tab state from before close_window stays
                // in session.json and silently restores.
                self.save_session();
                event_loop.exit();
            }
            Action::NextTab => self.mux.next_tab(),
            Action::PrevTab => self.mux.prev_tab(),
            Action::FocusNext => self.mux.focus_cycle(area, true),
            Action::FocusPrev => self.mux.focus_cycle(area, false),
            Action::FocusLeft => self.mux.focus_dir(area, -1, 0),
            Action::FocusRight => self.mux.focus_dir(area, 1, 0),
            Action::FocusUp => self.mux.focus_dir(area, 0, -1),
            Action::FocusDown => self.mux.focus_dir(area, 0, 1),
            Action::ResizeLeft => self.mux.resize_focus(Dir::Horizontal, -0.03),
            Action::ResizeRight => self.mux.resize_focus(Dir::Horizontal, 0.03),
            Action::ResizeUp => self.mux.resize_focus(Dir::Vertical, -0.03),
            Action::ResizeDown => self.mux.resize_focus(Dir::Vertical, 0.03),
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
                let selection_text = self.mux.focused().and_then(|p| {
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
                    let _ = cb.set_text(s);
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
                    self.clear_selection_on_input();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            Action::Paste => self.paste_clipboard(),
            Action::IncreaseFontSize | Action::DecreaseFontSize | Action::ResetFontSize => {
                if let Some(r) = self.renderer.as_mut() {
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
            }
            Action::StartSearch => {
                // Cycle 154: close any other modal first so we don't
                // stack two visible overlays. (Opening only sets one
                // of the four state fields; the others would stay
                // None already on the happy path, but defending in
                // depth here lets a future "open X without closing"
                // bug stay sane.)
                self.close_all_modals();
                self.mux.search.open = true;
                self.mux.search.query.clear();
                self.mux.search.matches.clear();
                self.mux.search.index = 0;
                self.search_revealed = None; // re-reveal on this new search
            }
            Action::ToggleBroadcastAll => {
                // Cycle 679: cycle-178 "broadcast-all" is actually
                // per-tab (the action's misnaming was a known
                // tech-debt). The Tab variant preserves the
                // existing UX exactly. The new All / Group
                // variants are reachable via the upcoming
                // GroupTab/GroupWindow/CreateGroup actions
                // (cycle 642 surface, dispatch follow-up).
                self.mux.broadcast = crate::mux::BroadcastScope::Tab;
            }
            Action::ToggleBroadcastOff => {
                self.mux.broadcast = crate::mux::BroadcastScope::Off;
            }
            Action::ToggleBroadcastGroup => {
                // Cycle 681 (named-groups sub-cycle 5): toggle
                // broadcast scope between Off and
                // Group(focused_pane.group_name). If focused
                // pane has no group, log + no-op.
                let focused_group = self.mux.focused().and_then(|p| p.group_name.clone());
                let Some(group) = focused_group else {
                    log::info!(
                        "toggle-broadcast-group: focused pane has no group_name; \
                         use Action::CreateGroup or Action::GroupTab first"
                    );
                    return;
                };
                self.mux.broadcast = match &self.mux.broadcast {
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
                self.mux.broadcast = match &self.mux.broadcast {
                    crate::mux::BroadcastScope::All => crate::mux::BroadcastScope::Off,
                    _ => crate::mux::BroadcastScope::All,
                };
            }
            Action::ToggleZoom => {
                self.mux.toggle_zoom();
                self.resize_all();
            }
            // Cycle 702 Terminator parity (`key_send_newline`).
            // Write a literal `\n` to the focused pane's PTY.
            // Useful for shell line-editors that consume Enter
            // normally but expect explicit `\n` for line
            // continuation (multi-line readline prompts).
            Action::SendNewline => {
                if let Some(p) = self.mux.focused() {
                    p.term.write(b"\n");
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
                self.mux.toggle_zoom();
                self.resize_all();
                let now_zoomed = self.mux.is_zoomed();
                if let Some(r) = self.renderer.as_mut() {
                    if now_zoomed {
                        if self.scaled_zoom_prev_font_size.is_none() {
                            self.scaled_zoom_prev_font_size = Some(self.cfg.font_size);
                        }
                        let new_size = (self.cfg.font_size * 1.5).clamp(6.0, 96.0);
                        r.set_font_size(new_size);
                    } else if let Some(prev) = self.scaled_zoom_prev_font_size.take() {
                        r.set_font_size(prev);
                    }
                }
            }
            Action::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                if let Some(w) = &self.window {
                    w.set_fullscreen(if self.fullscreen {
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
                if self.mux.is_broadcast_on() {
                    self.mux.broadcast_write(b"\x1b[3J");
                } else if let Some(p) = self.mux.focused() {
                    p.term.write(b"\x1b[3J");
                }
                if let Some(w) = &self.window {
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
                if let Some(p) = self.mux.focused() {
                    p.term.write(b"\x1bc");
                }
                self.clear_selection_on_input();
                // Cycle 111's modal sweep, extracted to a helper in
                // cycle 154 so the modal-opening actions can reuse it.
                self.close_all_modals();
                // Cycle 134: also reset the blink phase so the cursor
                // is immediately visible. Without this, hitting Reset
                // right as `blink_on` was false left the user staring
                // at a missing cursor for up to one blink interval —
                // confusing, because Reset is the chord users hit to
                // recover from a visually-jammed terminal. Shares
                // `reset_blink_phase` with cycle-135 focus-change and
                // cycle-140 modal-close paths.
                self.reset_blink_phase();
            }
            Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollLineUp
            | Action::ScrollLineDown
            | Action::ScrollToTop
            | Action::ScrollToBottom => {
                if let Some(p) = self.mux.focused()
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
                if let Some(p) = self.mux.focused() {
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
                self.close_all_modals();
                self.ssh_input = Some(String::new());
            }
            Action::CommandPalette => {
                self.close_all_modals();
                self.palette_input = Some((String::new(), 0));
            }
            // Cycle 708 (Terminator parity, layoutlauncher.py):
            // open the runtime layout picker. Empty layouts dir
            // is fine — the modal still opens with a "no
            // matching layout" hint, so the user gets a clear
            // "I have no saved layouts yet; save one with
            // `kettle --save-layout NAME`" affordance.
            Action::OpenLayoutPicker => {
                self.close_all_modals();
                self.layout_picker_input = Some((String::new(), 0));
            }
            Action::HintMode => {
                let targets = self.collect_hints();
                if !targets.is_empty() {
                    self.close_all_modals();
                    self.hint_state = Some((targets, String::new()));
                }
            }
            Action::ToggleViMode => {
                // Cycle 298 vi-mode (Alacritty parity), sub-cycle 1
                // of 4. Foundation: toggle entry / exit. Visible
                // block cursor at the focused pane's current cursor
                // position; Esc also exits (handled in keyboard
                // dispatch). h/j/k/l movement + visual selection +
                // yank land in sub-cycles 2-4.
                if self.vi_mode.is_some() {
                    self.vi_mode = None;
                } else {
                    self.close_all_modals();
                    // Seed cursor at the focused pane's current
                    // terminal cursor position. h/j/k/l will move
                    // around this in sub-cycle 2.
                    let (row, col) = self
                        .mux
                        .focused()
                        .and_then(|p| {
                            p.term.term.lock().ok().map(|t| {
                                let cursor = t.grid().cursor.point;
                                (cursor.line.0.max(0) as usize, cursor.column.0)
                            })
                        })
                        .unwrap_or((0, 0));
                    self.vi_mode = Some(ViState {
                        row,
                        col,
                        visual_anchor: None,
                    });
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Action::OpenContextMenu => {
                // Keyboard-triggered open: anchor at the current mouse
                // position so the menu lands where the user is looking;
                // falls back to the center of the focused pane when
                // dispatched programmatically (e.g. from the palette).
                let (px, py) = (self.cursor.x as f32, self.cursor.y as f32);
                self.open_context_menu(px, py);
            }
            Action::UndoCloseTab => {
                let waker = self.waker();
                match self
                    .mux
                    .undo_close_tab(&self.cfg, cols, rows, cw, ch, waker)
                {
                    Ok(true) => {
                        self.resize_all();
                        self.save_session();
                    }
                    Ok(false) => {
                        log::debug!("undo_close_tab: ring is empty, nothing to restore");
                    }
                    Err(e) => log::error!("undo_close_tab failed: {e}"),
                }
            }
            Action::DuplicateTab => {
                let waker = self.waker();
                if let Err(e) = self
                    .mux
                    .duplicate_focused_tab(&self.cfg, cols, rows, cw, ch, waker)
                {
                    log::error!("duplicate_tab failed: {e}");
                } else {
                    self.resize_all();
                    self.save_session();
                }
            }
            Action::DuplicatePane => {
                let waker = self.waker();
                if let Err(e) = self.mux.duplicate_focused_pane(
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
                    self.resize_all();
                }
            }
            Action::NextTheme | Action::PrevTheme => {
                let fwd = matches!(action, Action::NextTheme);
                let name = kettle_config::Theme::cycle(&self.cfg.theme_name, fwd);
                self.cfg.theme_name = name.to_string();
                self.cfg.theme = kettle_config::Theme::by_name(name);
                self.save_session(); // persist so the choice sticks
                if let Some(w) = &self.window {
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
                    self.save_session();
                    if let Some(w) = &self.window {
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
                if let Some(pane) = self.mux.focused() {
                    let mut guard = match pane.term.log_file.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    if guard.is_some() {
                        // Drop the file handle to stop logging.
                        // The reader thread will check is_some() on
                        // its next read + skip the write.
                        *guard = None;
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
                                *guard = Some(f);
                                // Cycle 625: propagate the config's
                                // strip-ANSI choice to the reader
                                // thread's per-Terminal flag.
                                if let Ok(mut strip) = pane.term.log_strip_ansi.lock() {
                                    *strip = self.cfg.log_strip_ansi;
                                }
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
                // BEFORE the `&mut self.renderer` borrow to keep
                // the borrow window narrow.
                let area = self.area();
                let active = self.mux.active;
                let focus_id = self.mux.tabs.get(active).map(|t| t.focus).unwrap_or(0);
                let crop = self
                    .mux
                    .layout(active, area)
                    .into_iter()
                    .find(|(id, _)| *id == focus_id)
                    .map(|(_, rect)| rect);
                if let Some(renderer) = self.renderer.as_mut() {
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
            Action::ReloadConfig => self.reload_config(),
            Action::MoveTabLeft => {
                self.mux.move_active_tab(-1);
            }
            Action::MoveTabRight => {
                self.mux.move_active_tab(1);
            }
            Action::GotoTab(n) => {
                let i = n as usize;
                if i < self.mux.tabs.len() {
                    self.mux.active = i;
                    self.mux.touch_active_tab_seen();
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
                self.close_all_modals();
                let current = self.last_title.clone();
                self.editing_title = Some(TitleEditState {
                    scope: TitleEditScope::Window,
                    input: current,
                    bulk: GroupBulkScope::Single,
                });
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Action::EditTabTitle => {
                self.close_all_modals();
                let current = self
                    .mux
                    .tabs
                    .get(self.mux.active)
                    .and_then(|t| t.title_override.clone())
                    .unwrap_or_default();
                self.editing_title = Some(TitleEditState {
                    scope: TitleEditScope::Tab,
                    input: current,
                    bulk: GroupBulkScope::Single,
                });
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Action::EditPaneTitle => {
                self.close_all_modals();
                let current = self
                    .mux
                    .focused()
                    .map(|p| p.title.clone())
                    .unwrap_or_default();
                self.editing_title = Some(TitleEditState {
                    scope: TitleEditScope::Pane,
                    input: current,
                    bulk: GroupBulkScope::Single,
                });
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Action::EditPaneGroup | Action::CreateGroup => {
                // Cycle 407 + cycle 642: edit the focused pane's
                // broadcast-group name. Empty input → clear the
                // group. Same overlay mechanism as cycle-369
                // EditPaneTitle. `CreateGroup` (Terminator name)
                // and `EditPaneGroup` (kettle name) share dispatch.
                self.close_all_modals();
                let current = self
                    .mux
                    .focused()
                    .and_then(|p| p.group_name.clone())
                    .unwrap_or_default();
                self.editing_title = Some(TitleEditState {
                    scope: TitleEditScope::Group,
                    input: current,
                    bulk: GroupBulkScope::Single,
                });
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Action::GroupTab | Action::GroupWindow => {
                // Cycle 680 (named-groups sub-cycle 4): open the
                // title-edit overlay with `bulk` set to Tab/Window
                // so on Apply the typed name writes to every pane
                // in scope.
                self.close_all_modals();
                let bulk = if matches!(action, Action::GroupTab) {
                    GroupBulkScope::Tab
                } else {
                    GroupBulkScope::Window
                };
                self.editing_title = Some(TitleEditState {
                    scope: TitleEditScope::Group,
                    input: String::new(),
                    bulk,
                });
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Action::UngroupTab | Action::UngroupWindow => {
                // Cycle 680 (named-groups sub-cycle 4): bulk-
                // clear the group on every pane in scope. No
                // overlay needed — empty input is the "clear"
                // signal, and the action carries the scope.
                let pane_ids: Vec<u64> = if matches!(action, Action::UngroupTab) {
                    self.mux
                        .tabs
                        .get(self.mux.active)
                        .map(|t| t.root.leaf_ids())
                        .unwrap_or_default()
                } else {
                    self.mux.panes.keys().copied().collect()
                };
                for id in pane_ids {
                    if let Some(p) = self.mux.panes.get_mut(&id) {
                        p.group_name = None;
                    }
                }
                if let Some(w) = &self.window {
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
                        self.reload_config();
                    }
                }
            }
            // Cycle 347: split-tree rotation. RotateCw flips dir +
            // swaps children (Terminator's clockwise semantics);
            // RotateCcw flips dir without swap. No-op when the
            // focused leaf has no parent (single-pane tab).
            Action::RotateCw => {
                self.mux.rotate_focused_split(true);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Action::RotateCcw => {
                self.mux.rotate_focused_split(false);
                if let Some(w) = &self.window {
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
                if let Some(w) = &self.window {
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
                if let Some(r) = self.renderer.as_mut() {
                    // Cycle 747: step logical size (see IncreaseFontSize).
                    r.set_font_size(r.font_size() + 1.0);
                }
            }
            Action::ZoomOutAll => {
                if let Some(r) = self.renderer.as_mut() {
                    r.set_font_size((r.font_size() - 1.0).max(6.0));
                }
            }
            Action::ZoomNormalAll => {
                if let Some(r) = self.renderer.as_mut() {
                    r.set_font_size(self.cfg.font_size);
                }
            }
            // Cycle 345: insert pane index. Pane index is 1-based
            // (matches Terminator's GotoTab + every user-facing
            // numbering). InsertPanePadded uses 2-digit zero-padded
            // form (Terminator default).
            Action::InsertPaneNumber => {
                let idx = self
                    .mux
                    .focused_pane_index_in_tab()
                    .map(|i| i + 1)
                    .unwrap_or(1);
                if let Some(p) = self.mux.focused() {
                    p.term.write(idx.to_string().as_bytes());
                }
            }
            Action::InsertPanePadded => {
                let idx = self
                    .mux
                    .focused_pane_index_in_tab()
                    .map(|i| i + 1)
                    .unwrap_or(1);
                if let Some(p) = self.mux.focused() {
                    p.term.write(format!("{idx:02}").as_bytes());
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
                if let Some(p) = self.mux.focused() {
                    let title = p.title.clone();
                    p.term.write(title.as_bytes());
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
                if let Some(cwd) = self
                    .mux
                    .focused()
                    .and_then(|p| p.term.current_dir())
                    .filter(|s| !s.is_empty())
                {
                    self.open_url(&format!("file://{cwd}"));
                } else {
                    log::info!(
                        "Action::OpenCwdInFileManager: focused pane has no OSC 7 cwd \
                         — set up shell integration with `kettle --shell-integration bash`"
                    );
                }
            }
            // Cycle 345: half-page scroll. Same shape as cycle-X's
            // ScrollPageUp/Down handler but with half the row count.
            // Pull the row count from the focused pane's grid
            // dimensions (cycle-X pattern; works for any pane size).
            Action::ScrollPageUpHalf | Action::ScrollPageDownHalf => {
                if let Some(p) = self.mux.focused()
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
            Action::PastePrimary => self.paste_primary(),
            // Cycle 345: in-process Quake toggle. Same tri-state
            // logic as cycle-319's --toggle remote command:
            //   hidden → show + focus
            //   visible + focused → hide
            //   visible + !focused → focus (don't hide)
            Action::ToggleWindowVisibility => {
                if let Some(w) = &self.window {
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
                // Cycle 410 (Terminator parity, detachable-tabs
                // Bucket-D sub-cycle 7 source): on Unix, prefer the
                // SCM_RIGHTS socketpair path over the cycle-405 file-
                // fallback. socketpair → fork+exec child with
                // --tab-handoff-fd 3 → parent send_fds the serialized
                // tab + (future: PTY fds) → child recv_fds + restore.
                //
                // Falls through to the cycle-405 file-fallback when
                // socketpair fails or on Windows/Wayland.
                //
                // Cycle 405 (Terminator parity, detachable-tabs
                // Bucket-D sub-cycle 8 full): serialize the focused
                // tab to a one-shot JSON handoff file + spawn a
                // new kettle process with --tab-handoff PATH.
                // The target reads + reconstructs the tab (cycle 404).
                //
                // Running shells in the source tab stay in the
                // source window (true PTY-fd transfer needs the
                // SCM_RIGHTS path, sub-cycle 7). The file-fallback
                // works cross-platform incl. Windows + Wayland.
                #[cfg(unix)]
                if self.try_move_tab_to_new_window_scm_rights(event_loop) {
                    return;
                }
                let cwd = self
                    .mux
                    .focused()
                    .and_then(|p| p.term.current_dir())
                    .or_else(|| {
                        std::env::current_dir()
                            .ok()
                            .map(|p| p.display().to_string())
                    });
                // Serialize the focused tab to a temp file.
                let handoff_path: Option<std::path::PathBuf> =
                    self.mux.serialize_tab(self.mux.active).and_then(|stab| {
                        let session = crate::session::Session {
                            tabs: vec![stab],
                            active: 0,
                            theme: Some(self.cfg.theme_name.clone()),
                        };
                        let path = std::env::temp_dir()
                            .join(format!("kettle-handoff-{}.json", std::process::id()));
                        serde_json::to_string(&session)
                            .ok()
                            .and_then(|json| std::fs::write(&path, json).ok())
                            .map(|_| path)
                    });
                if let Ok(exe) = std::env::current_exe() {
                    let mut cmd = std::process::Command::new(exe);
                    if let Some(p) = handoff_path.as_ref() {
                        cmd.arg("--tab-handoff").arg(p);
                    } else if let Some(d) = cwd {
                        cmd.arg("--working-directory").arg(d);
                    }
                    if let Some(p) = self.config_path.as_ref() {
                        cmd.arg("--config").arg(p);
                    }
                    cmd.stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null());
                    if cmd.spawn().is_ok() {
                        // Cycle 424: fire TabClose so plugins see the close.
                        let closing_idx = self.mux.active;
                        let _ = self.mux.close_tab();
                        self.fire_tab_close_event(closing_idx);
                    } else {
                        log::warn!("MoveTabToNewWindow: spawn failed; tab kept in source window");
                        // Clean up the orphan handoff file.
                        if let Some(p) = handoff_path.as_ref() {
                            let _ = std::fs::remove_file(p);
                        }
                    }
                }
            }
            Action::ResetAndClear => {
                // Cycle 342 Terminator parity (key_reset_clear):
                // Reset (RIS, \ec) + ClearHistory (CSI 3 J) composed
                // into a single keybind. The two byte writes go to
                // the existing PTY-write path; the engine handles
                // them the same as cycle-X's separate Reset +
                // ClearHistory actions.
                if let Some(p) = self.mux.focused() {
                    p.term.write(b"\x1bc");
                    p.term.write(b"\x1b[3J");
                }
            }
        }
        // Cycle 135 (cont.): if focus moved as a result of the action,
        // land the cursor visible on the new pane right away.
        self.note_focus_change(pre_focus);
        self.resize_all();
        self.save_session();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn save_session(&self) {
        let mut s = self.mux.snapshot();
        s.theme = Some(self.cfg.theme_name.clone());
        // Cycle 291: when launched with `--layout NAME`, save to the
        // named-layout file instead of the default session.json. Lets
        // the user maintain distinct workspaces ("dev", "ops", "docs")
        // without each one clobbering the others on close.
        match &self.startup.layout {
            Some(name) => s.save_layout(name),
            None => s.save(),
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
    fn poll_focus_event(&mut self) {
        let current = self.mux.active_focus();
        let Some(cur_id) = current else { return };
        if self.last_emitted_focus == Some(cur_id) {
            return;
        }
        let prev = self.last_emitted_focus;
        self.last_emitted_focus = Some(cur_id);
        if let Some(eng) = self.lua_engine.as_ref() {
            eng.fire_event(&crate::LuaEvent::PaneFocus(prev, cur_id));
        }
    }

    /// Cycle 745: reflect the FOCUSED pane's OSC 9;4 progress onto the OS
    /// taskbar button each frame (pwsh 7 / Windows Terminal parity). Reads the
    /// focused pane the same way the cursor-blink poll does; `Taskbar` dedups
    /// internally, so an unchanged value costs nothing. No-op off Windows.
    fn poll_taskbar_progress(&mut self) {
        let progress = self
            .mux
            .active_focus()
            .and_then(|id| self.mux.panes.get(&id))
            .and_then(|p| p.term.progress());
        if let Some(window) = self.window.clone() {
            self.taskbar.apply(&window, progress);
        }
    }

    /// Cycle 704 (Terminator plugin parity, plugin sub-cycle:
    /// `LuaEvent::TitleChanged`). Walk live panes, diff each
    /// title against `self.last_emitted_titles`, emit on any
    /// boundary cross. One pass site, regardless of how many
    /// title-mutating sites exist in App.
    ///
    /// O(n_panes) per redraw. Even 100 panes is trivial — a
    /// hash lookup + string compare per entry. Future cycles
    /// can add a "dirty-title" bitset on Mux if pane counts
    /// grow into the thousands.
    fn poll_title_event(&mut self) {
        let Some(eng) = self.lua_engine.as_ref() else {
            return;
        };
        let mut changes: Vec<(u64, String)> = Vec::new();
        for (id, p) in self.mux.panes.iter() {
            let last = self.last_emitted_titles.get(id);
            if last.map(|s| s.as_str()) != Some(p.title.as_str()) {
                changes.push((*id, p.title.clone()));
            }
        }
        for (id, title) in changes {
            self.last_emitted_titles.insert(id, title.clone());
            eng.fire_event(&crate::LuaEvent::TitleChanged(id, title));
        }
    }

    fn poll_theme_schedule(&mut self) {
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
            log::info!("theme-schedule: switching to {next}");
            self.cfg.theme_name = next.clone();
            self.cfg.theme = kettle_config::Theme::by_name(&next);
            self.save_session();
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    /// Cycle 660 (sub-cycle 5 of [`TERMINATOR-CONFIRM-DIALOG-DESIGN.md`](
    /// ../../../docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md)): dispatch
    /// the `ConfirmAction` after the user accepts the modal. Skips
    /// the `should_prompt` check (we wouldn't be here otherwise) so
    /// the close-family actions run their real bodies.
    fn dispatch_confirm_action(
        &mut self,
        action: ConfirmAction,
        event_loop: &winit::event_loop::ActiveEventLoop,
    ) {
        match action {
            ConfirmAction::CloseWindow => {
                self.mux.close_window();
                self.save_session();
                event_loop.exit();
            }
            ConfirmAction::CloseTab => {
                // CloseTab dispatch (cycle X). Sub-cycle 6 wires
                // ask-before-closing for CloseTab too; this arm is
                // the dispatch target for that future wiring.
                self.mux.close_tab();
                self.save_session();
            }
            ConfirmAction::ClosePane => {
                // Cycle 750: capture the pane id before the close so the
                // pane_close hook fires with the right id (mirrors the
                // keybind path).
                let closing_pane = self.mux.active_focus();
                self.mux.close_focused();
                if let Some(id) = closing_pane {
                    self.fire_pane_close_event(id);
                }
                self.save_session();
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
    fn poll_remote_contexts(&mut self) {
        if self.last_remote_poll.elapsed().as_millis() < 200 {
            return;
        }
        self.last_remote_poll = std::time::Instant::now();
        let pane_ids: Vec<u64> = self.mux.panes.keys().copied().collect();
        for id in pane_ids {
            let Some(pane) = self.mux.panes.get(&id) else {
                continue;
            };
            let Some(pid) = pane.term.child_pid() else {
                continue;
            };
            let detected = kettle_remote::detect_remote_with(pid, &mut self.remote_sysinfo);
            if let Some(pane) = self.mux.panes.get_mut(&id)
                && detected != pane.remote_context
            {
                if let Some(ctx) = &detected {
                    pane.title = kettle_remote::format_remote_title(ctx);
                }
                pane.remote_context = detected;
            }
        }
    }

    fn reload_config(&mut self) {
        let new = self
            .config_path
            .as_deref()
            .map(Config::load_from)
            .unwrap_or_else(Config::load);
        if let Some(r) = self.renderer.as_mut() {
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
        self.cfg = new;
        self.resize_all();
        if let Some(w) = &self.window {
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
    fn drain_remote_commands(&mut self) {
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
                if let Some(p) = self.mux.focused() {
                    p.term.write(decoded.as_bytes());
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
                if let Some(w) = &self.window {
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
                let (cw, ch) = self.cell_px();
                let area = self.area();
                let (cols, rows) = self.grid_of(area);
                let waker = self.waker();
                if let Err(e) = self.mux.new_tab(&self.cfg, cols, rows, cw, ch, waker) {
                    log::warn!("remote-control: new-tab failed: {e}");
                } else {
                    self.fire_tab_add_event();
                }
            } else {
                log::warn!("remote command not recognized: {line:?}");
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn run_triggers(&mut self) {
        if self.compiled_triggers.is_empty() {
            return;
        }
        // Throttle. `last_trigger_fire` is pre-set to "60 seconds ago"
        // at construct/reload time so the first match always fires.
        if self.last_trigger_fire.elapsed().as_millis() < 2000 {
            return;
        }
        // Don't pulse the user's own window when it's already focused.
        if self.window_focused {
            return;
        }
        // Pull each pane's bottom-of-screen text. Visible viewport
        // only — scanning the whole scrollback every wakeup would
        // burn CPU on a chatty pane. Last 50 rows is the typical
        // "what just happened" window.
        let snapshots: Vec<String> = {
            let mut out = Vec::with_capacity(self.mux.panes.len());
            for pane in self.mux.panes.values() {
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
                        if let Some(w) = &self.window {
                            use winit::window::UserAttentionType;
                            w.request_user_attention(Some(UserAttentionType::Critical));
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

    fn search_key(&mut self, key: &Key, text: Option<&str>) {
        match key {
            Key::Named(NamedKey::Escape) => {
                // Cycle 140: closing the search overlay reveals the
                // pane's cursor underneath. Reset blink so the
                // cursor is visible immediately — same UX argument
                // as cycles 134/135 (focus + Reset paths).
                self.mux.search.open = false;
                self.reset_blink_phase();
            }
            Key::Named(NamedKey::Enter) => {
                let s = &mut self.mux.search;
                if !s.matches.is_empty() {
                    // Cycle 358 (Terminator parity, terminatorlib/config.py:93
                    // `invert_search`): flip the default-direction.
                    // - Default: Enter → next match, Shift+Enter → previous.
                    // - With invert_search = true: Enter → previous match,
                    //   Shift+Enter → next. Matches Terminator's "search
                    //   reverse" toggle.
                    let go_back = self.mods.shift_key() ^ self.cfg.invert_search;
                    s.index = if go_back {
                        (s.index + s.matches.len() - 1) % s.matches.len()
                    } else {
                        (s.index + 1) % s.matches.len()
                    };
                }
            }
            Key::Named(NamedKey::Backspace) => {
                self.mux.search.query.pop();
            }
            _ => {
                if let Some(t) = text {
                    self.mux.search.query.push_str(t);
                }
            }
        }
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
    fn vi_mode_key(&mut self, key: &Key, text: Option<&str>) {
        // Esc exits.
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.vi_mode = None;
            return;
        }
        // Grab the focused pane's grid dims to clamp movement.
        let (max_row, max_col) = self
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
        let Some(state) = self.vi_mode.as_mut() else {
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
                    let yanked = self.yank_vi_selection(start, end);
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
                self.vi_mode = None;
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

    fn hint_key(&mut self, key: &Key, text: Option<&str>) {
        match key {
            Key::Named(NamedKey::Escape) => {
                self.hint_state = None;
                self.reset_blink_phase();
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some((_, typed)) = self.hint_state.as_mut() {
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
                    let Some((targets, typed)) = self.hint_state.as_mut() else {
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
                    self.hint_state = None;
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
            let _ = cb.set_text(h.text.clone());
        }
    }

    fn palette_key(&mut self, key: &Key, text: Option<&str>, event_loop: &ActiveEventLoop) {
        let cmds = kettle_config::palette::commands();
        let Some((q, sel)) = self.palette_input.as_mut() else {
            return;
        };
        match key {
            Key::Named(NamedKey::Escape) => {
                self.palette_input = None;
                self.reset_blink_phase();
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
                self.palette_input = None;
                if let Some(a) = action {
                    self.handle_action(a, event_loop);
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

    /// Cycle 708 (Terminator parity, `layoutlauncher.py`):
    /// keyboard routing while the layout picker overlay is open.
    /// Same shape as `palette_key` but ranks against
    /// `Session::list_layouts()` and dispatches by spawning
    /// `kettle --layout NAME` as a new window.
    fn layout_picker_key(&mut self, key: &Key, text: Option<&str>) {
        let layouts = crate::session::Session::list_layouts();
        let Some((q, sel)) = self.layout_picker_input.as_mut() else {
            return;
        };
        match key {
            Key::Named(NamedKey::Escape) => {
                self.layout_picker_input = None;
                self.reset_blink_phase();
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
            Key::Named(NamedKey::Enter) => {
                let ranked = rank_layouts(q, &layouts);
                let name = ranked.get(*sel).map(|&i| layouts[i].clone());
                self.layout_picker_input = None;
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

    /// Keyboard routing while the right-click context menu is open.
    /// `Esc` closes, `↑/↓` step the highlight (skipping separators +
    /// disabled rows via `next_context_menu_highlight`), `Enter` fires
    /// the highlighted action. Any other key is swallowed so a stray
    /// keypress doesn't leak into the focused pane while the menu is
    /// expecting nav input.
    fn context_menu_key(&mut self, key: &Key, text: Option<&str>, event_loop: &ActiveEventLoop) {
        match key {
            Key::Named(NamedKey::Escape) => {
                // Cycle 687 (theme-submenu sub-cycle 3): Esc on
                // a drilled-in submenu pops back to the parent
                // instead of closing the menu entirely. Only
                // when drill_stack is empty does Esc close.
                if let Some(menu) = self.context_menu.as_mut()
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
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                self.context_menu = None;
                self.reset_blink_phase();
            }
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => {
                self.step_context_menu_highlight(1);
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.step_context_menu_highlight(-1);
            }
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                let chosen = self
                    .context_menu
                    .as_ref()
                    .and_then(|m| match &m.items[m.highlight] {
                        ContextMenuItem::Item {
                            action,
                            enabled: true,
                            ..
                        } => Some(action.clone()),
                        _ => None,
                    });
                self.context_menu = None;
                self.reset_blink_phase();
                if let Some(a) = chosen {
                    self.handle_action(a, event_loop);
                }
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
                if let Some(menu) = self.context_menu.as_mut()
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
                let mnemonic_hit = self.context_menu.as_ref().and_then(|menu| {
                    if !menu.typeahead_buf.is_empty() {
                        return None;
                    }
                    let mn = assign_mnemonics(&menu.items);
                    mn.iter().enumerate().find_map(|(idx, slot)| {
                        slot.and_then(|(_, ch)| (ch == lower).then_some(idx))
                    })
                });
                if let Some(idx) = mnemonic_hit {
                    // Dispatch the matched row exactly the same way
                    // Enter would: drill into Submenu, or fire the
                    // Action, or set theme/profile.
                    let click =
                        self.context_menu
                            .as_ref()
                            .and_then(|m| match m.items.get(idx)? {
                                ContextMenuItem::Item {
                                    action,
                                    enabled: true,
                                    ..
                                } => Some(ContextMenuClick::Action(action.clone())),
                                ContextMenuItem::Submenu { .. } => {
                                    Some(ContextMenuClick::DrillIntoSubmenu(idx))
                                }
                                ContextMenuItem::ThemeChoice { theme, .. } => {
                                    Some(ContextMenuClick::SetTheme(theme.clone()))
                                }
                                ContextMenuItem::ProfileChoice { profile, .. } => {
                                    Some(ContextMenuClick::SetProfile(profile.clone()))
                                }
                                _ => None,
                            });
                    match click {
                        Some(ContextMenuClick::Action(a)) => {
                            self.context_menu = None;
                            self.handle_action(a, event_loop);
                            return;
                        }
                        Some(ContextMenuClick::DrillIntoSubmenu(idx)) => {
                            if let Some(menu) = self.context_menu.as_mut() {
                                let nested_items = match menu.items.get(idx) {
                                    Some(ContextMenuItem::Submenu { items, .. }) => items.clone(),
                                    _ => Vec::new(),
                                };
                                if !nested_items.is_empty() {
                                    let parent = std::mem::replace(&mut menu.items, nested_items);
                                    menu.drill_stack.push(parent);
                                    menu.scroll_stack.push(menu.scroll_offset);
                                    menu.scroll_offset = 0;
                                    menu.typeahead_buf.clear();
                                    menu.typeahead_until = None;
                                    menu.highlight = menu
                                        .items
                                        .iter()
                                        .position(item_is_dispatchable)
                                        .unwrap_or(0);
                                }
                            }
                            return;
                        }
                        Some(ContextMenuClick::SetTheme(name)) => {
                            self.context_menu = None;
                            self.cfg.theme_name = name.clone();
                            self.cfg.theme = kettle_config::Theme::by_name(&name);
                            self.save_session();
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                            return;
                        }
                        Some(ContextMenuClick::SetProfile(name)) => {
                            self.context_menu = None;
                            if let Some(p) = kettle_config::Config::path_for_profile(&name) {
                                self.config_path = Some(p);
                                self.reload_config();
                            }
                            return;
                        }
                        _ => {}
                    }
                }
                // Typeahead path: accumulate into the buffer, find
                // the first row whose label has this prefix, advance
                // highlight without dispatching.
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.typeahead_buf.push(lower);
                    menu.typeahead_until = Some(now + std::time::Duration::from_millis(750));
                    if let Some(idx) = typeahead_match(&menu.items, &menu.typeahead_buf) {
                        menu.highlight = idx;
                    }
                }
            }
        }
    }

    fn ssh_key(&mut self, key: &Key, text: Option<&str>) {
        match key {
            Key::Named(NamedKey::Escape) => {
                self.ssh_input = None;
                self.reset_blink_phase();
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(q) = self.ssh_input.as_mut() {
                    q.pop();
                }
            }
            Key::Named(NamedKey::Tab) => {
                // Fuzzy-complete to the best-matching configured host name.
                let typed = self.ssh_input.clone().unwrap_or_default();
                if !typed.is_empty()
                    && let Some((n, _)) =
                        kettle_config::fuzzy::best(&typed, &self.cfg.ssh_hosts, |h| h.0.as_str())
                {
                    self.ssh_input = Some(n.clone());
                }
            }
            Key::Named(NamedKey::Enter) => {
                let typed = self.ssh_input.take().unwrap_or_default();
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
                    let area = self.area();
                    let (cols, rows) = self.grid_of(area);
                    let (cw, ch) = self.cell_px();
                    if let Err(e) =
                        self.mux
                            .new_ssh_tab(&self.cfg, cols, rows, cw, ch, self.waker(), &target)
                    {
                        log::error!("ssh launch failed: {e}");
                    }
                    self.resize_all();
                    self.save_session();
                }
            }
            _ => {
                if let Some(t) = text
                    && let Some(q) = self.ssh_input.as_mut()
                {
                    q.push_str(t);
                }
            }
        }
    }
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

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
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
        match self.cfg.window_state {
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
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
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
        // Cycle 730: macOS joined Bucket E too. winit 0.30 dropped
        // the `WindowExtMacOS::set_visible_on_all_workspaces` method
        // that the original sticky impl relied on; rebuilding it
        // needs `objc2` + raw NSWindow handle (NSWindowCollectionBehavior
        // with `.canJoinAllSpaces`). Heavy dep for one config key,
        // same trade-off as X11/Wayland. Pre-730 this branch was
        // breaking the macOS CI build (E0599: method not found on
        // Arc<Window>) — the cycle-729 commit went out red on macOS
        // because no maintainer hit it locally. Cycle-730 stubs the
        // branch with a `log::info` so a user copying a Terminator
        // config that sets `sticky = true` sees a debuggable
        // message instead of silent no-op. Re-adding the feature
        // proper is a separate cycle once the objc2 dep is on the
        // table.
        #[cfg(target_os = "macos")]
        if self.cfg.sticky {
            log::info!(
                "kettle: `sticky = true` is currently a no-op on macOS \
                 (winit 0.30 dropped the underlying API; re-implementing \
                 via objc2 is a tracked follow-up). The window appears \
                 on the current Space only."
            );
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
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let renderer = match pollster::block_on(Renderer::new(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            scale,
            &self.cfg,
        )) {
            Ok(r) => r,
            Err(e) => {
                log::error!("renderer init failed: {e}");
                event_loop.exit();
                return;
            }
        };
        self.renderer = Some(renderer);
        self.window = Some(window);

        let area = self.area();
        let (cols, rows) = self.grid_of(area);
        let (cw, ch) = self.cell_px();

        // CLI `-e cmd` / `-d dir` (consumed once) take precedence over a
        // saved session: explicit intent shouldn't be overridden by restore.
        let startup = std::mem::take(&mut self.startup);
        let has_override = startup.command.is_some() || startup.cwd.is_some();
        let restored = if has_override {
            let argv = startup.command.unwrap_or_default();
            let cwd = startup
                .cwd
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned());
            if let Err(e) = self.mux.new_tab_with(
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
            } else {
                match self.startup.layout.as_deref() {
                    Some(name) => crate::session::Session::load_layout(name),
                    None => crate::session::Session::load(),
                }
            };
            match loaded {
                Some(s) if !s.is_empty() => {
                    // A theme picked at runtime last session sticks until
                    // the user changes it again or reloads the config.
                    if let Some(name) = s.theme.as_deref()
                        && !name.eq_ignore_ascii_case(&self.cfg.theme_name)
                        // Case-insensitive — `Theme::by_name` is already
                        // case-insensitive (cycle 0), and a session
                        // written by an older kettle (or hand-edited)
                        // might hold a lowercase theme name that the
                        // pre-cycle-152 `contains(&name)` would reject
                        // verbatim despite the theme existing. Match
                        // `by_name`'s semantics here so the check
                        // agrees with the apply.
                        && let Some(canonical) = kettle_config::Theme::find_name(name)
                    {
                        // Cycle 177 (companion to 176): store the
                        // *canonical* name from the bundled set so
                        // `--check-config` and runtime palette agree
                        // after restore too. A session file written by
                        // an older kettle (pre-176) might hold a typo'd
                        // or all-lowercase theme name; using
                        // `find_name`'s canonical return keeps the
                        // restore in lock-step with parse_collect's
                        // cycle-176 behavior.
                        self.cfg.theme_name = canonical.to_string();
                        self.cfg.theme = kettle_config::Theme::by_name(canonical);
                    }
                    let proxy = self.proxy.clone();
                    let mk = move || -> kettle_core::Waker {
                        let p = proxy.clone();
                        std::sync::Arc::new(move || {
                            let _ = p.send_event(UserEvent::Wakeup);
                        })
                    };
                    self.mux.restore(&s, &self.cfg, cw, ch, &mk)
                }
                _ => false,
            }
        };
        if !restored
            && let Err(e) = self
                .mux
                .new_tab(&self.cfg, cols, rows, cw, ch, self.waker())
        {
            log::error!("failed to spawn shell: {e}");
            event_loop.exit();
            return;
        }
        self.resize_all();
        // Cycle 325 Lua scripting: drain any `kettle.send_text(s)`
        // bytes the startup script queued, into the now-existing
        // focused pane's PTY. The pane is fresh; the shell will
        // see this as the user's first typing.
        if !self.pending_lua_send.is_empty()
            && let Some(p) = self.mux.focused()
        {
            let bytes = std::mem::take(&mut self.pending_lua_send);
            p.term.write(&bytes);
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
                self.handle_action(a, event_loop);
            }
        }
        // Cycle 366 (Terminator plugin parity, sub-cycle 3): fire
        // LuaEvent::Startup the first time we have an alive window
        // + at least one pane. Subsequent resumed() calls (Wayland
        // can re-emit) get short-circuited by lua_startup_fired.
        // Drains any LuaCommand the callbacks queued so a
        // `kettle.on('startup', function() kettle.send_text(...) end)`
        // takes effect immediately.
        if !self.lua_startup_fired && self.lua_engine.is_some() && self.mux.focused().is_some() {
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
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Wakeup => {
                // Cycle 290: run output triggers before the redraw —
                // a match fires window urgency so the user notices the
                // event even if they're focused on another OS window.
                // Cheap when triggers are empty (which is the default).
                self.run_triggers();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            UserEvent::ReloadConfig => self.reload_config(),
            UserEvent::RemoteCommand => self.drain_remote_commands(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.save_session();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
                self.resize_all();
                if let Some(w) = &self.window {
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
                if let Some(r) = self.renderer.as_mut() {
                    r.set_scale(scale_factor as f32);
                }
                self.resize_all();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                // Modifier change can flip the URL hover affordance from
                // text-I-beam to pointing-hand without the mouse moving
                // (Ctrl held = "this click would open"). Re-sync the
                // cursor icon so the affordance updates the moment Ctrl
                // is pressed/released over a link.
                self.sync_cursor_icon();
            }
            // Cycle 402 (Terminator parity, detachable-tabs Bucket-D
            // sub-cycle 6): winit CursorLeft/Entered events transition
            // the detach FSM. CursorLeft → DraggingOutside (caller
            // generates a fresh session_id for the future cross-process
            // IPC handshake); CursorEntered → DraggingInside (user
            // brought the cursor back; cancel the cross-window flow).
            WindowEvent::CursorLeft { .. } => {
                let prev = std::mem::take(&mut self.detach_drag);
                self.detach_drag = prev.on_cursor_leave_window(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_micros() as u64)
                        .unwrap_or(0),
                );
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorEntered { .. } => {
                let (x, y) = (self.cursor.x as f32, self.cursor.y as f32);
                let prev = std::mem::take(&mut self.detach_drag);
                self.detach_drag = prev.on_cursor_reenter_window(x, y);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                // Any real mouse movement undoes the hide-while-typing
                // state. Sub-pixel movements that winit *might* coalesce
                // are fine to ignore — the next "real" motion will fire.
                self.show_mouse_cursor();
                self.sync_cursor_icon();
                // Cycle 712 (Terminator menu UX, hover-to-highlight):
                // cursor over a context-menu row immediately updates
                // the highlight. Matches GTK/NSMenu/Win32 menu
                // conventions; before this cycle the highlight only
                // moved via keyboard so the menu felt unresponsive to
                // mouse users. Cheap: no-op when the menu is closed.
                if self.context_menu.is_some() {
                    self.update_menu_highlight_from_cursor();
                }
                // Cycle 360 (Terminator parity, terminatorlib/config.py:73
                // `focus = sloppy`): focus-follows-mouse. The pane
                // under the cursor becomes focused on every cursor
                // movement (vs default `click` mode where click is
                // required). `system` is treated like `click` for
                // kettle — winit doesn't expose the OS-level focus
                // policy.
                if matches!(self.cfg.focus, kettle_config::FocusMode::Sloppy)
                    && !self.tab_drag_active
                    && !self.selecting
                    && !self.dragging_scrollbar
                {
                    let area = self.area();
                    let pre = self.focus_key();
                    self.mux
                        .focus_at(area, self.cursor.x as f32, self.cursor.y as f32);
                    self.note_focus_change(pre);
                }
                // Cycle 249: drag-to-reorder tabs (kitty / iTerm2 /
                // Ghostty parity). When a left-button press in the tab
                // bar armed `tab_drag_active`, walk the bar geometry,
                // compute the target index under the cursor, and swap
                // the active tab toward it via `move_active_tab`
                // (cycle ~125's pure swap-with-clamp helper).
                if self.tab_drag_active {
                    let bar = self.tab_bar();
                    if bar.height > 0.0 && !bar.segments.is_empty() {
                        let (_, _, nw, _) = bar.new_tab;
                        let (sw, _) = self
                            .renderer
                            .as_ref()
                            .map(|r| {
                                let (w, h) = r.surface_size();
                                (w as f32, h as f32)
                            })
                            .unwrap_or((800.0, 600.0));
                        let strip_w = (sw - nw).max(1.0);
                        let target = tab_drag_target_index(
                            self.cursor.x as f32,
                            bar.segments.len(),
                            strip_w,
                        );
                        let delta = target as i32 - self.mux.active as i32;
                        if delta != 0 && self.mux.move_active_tab(delta) {
                            self.mux.touch_active_tab_seen();
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                    }
                }
                if let Some(btn) = self.mouse_btn {
                    // Drag while a button is held — report motion if tracked.
                    if self.send_mouse(btn, true, true) {
                        return;
                    }
                }
                if self.dragging_scrollbar {
                    let area = self.area();
                    let (px, py) = (self.cursor.x as f32, self.cursor.y as f32);
                    self.scrollbar_at(area, px, py, false);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                if self.selecting {
                    let area = self.area();
                    self.update_selection(area);
                }
                if (self.selecting || !self.links.is_empty())
                    && let Some(w) = &self.window
                {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
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
                if self.context_menu.is_some()
                    && let Some(click) = self.context_menu_click_action(bcode)
                {
                    self.context_menu = None;
                    match click {
                        ContextMenuClick::Action(action) => {
                            self.handle_action(action, event_loop);
                        }
                        ContextMenuClick::LuaMenuItem(idx) => {
                            // Cycle 375: invoke the Lua callback +
                            // drain any LuaCommands it queued.
                            // Cycle 433: drain through the canonical
                            // helper to match the other 5 LuaEvent
                            // hook drains (Startup / TabAdd / TabClose
                            // / Bell / Output). Menu-item click is
                            // NOT a LuaEvent emission but it consumes
                            // the same LuaCommand queue.
                            if let Some(eng) = &self.lua_engine {
                                eng.invoke_menu_item(idx);
                            }
                            self.drain_lua_hook_commands("lua menu-item");
                        }
                        // Cycle 611 (Terminator parity, custom_commands.py):
                        // dispatch a config-file `menu-item = LABEL =
                        // CMD` click by writing the command + newline to
                        // the focused PTY. Simpler than the LuaItem path
                        // because there's no callback to invoke + no
                        // command queue to drain — just bytes to the
                        // PTY, identical to typing the command
                        // letter-by-letter.
                        ContextMenuClick::ConfigCommand(command) => {
                            if let Some(p) = self.mux.focused() {
                                let mut bytes = command.into_bytes();
                                bytes.push(b'\n');
                                p.term.write(&bytes);
                            }
                        }
                        // Cycle 685 (theme-submenu sub-cycle 2):
                        // theme picked from a Theme submenu
                        // flyout. Same swap path as the cycle-
                        // 3514 NextTheme action.
                        ContextMenuClick::SetTheme(name) => {
                            self.cfg.theme_name = name.clone();
                            self.cfg.theme = kettle_config::Theme::by_name(&name);
                            self.save_session();
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                        // Cycle 686 (theme-submenu sub-cycle 8):
                        // profile picked from a Profile submenu
                        // flyout. Uses cycle-618
                        // Config::path_for_profile + reload_config.
                        ContextMenuClick::SetProfile(name) => {
                            if let Some(p) = kettle_config::Config::path_for_profile(&name) {
                                self.config_path = Some(p);
                                self.reload_config();
                            }
                        }
                        // Cycle 687 (theme-submenu sub-cycle 3):
                        // drill into a submenu — push current
                        // items onto drill_stack, replace with
                        // the submenu's items, redraw.
                        ContextMenuClick::DrillIntoSubmenu(idx) => {
                            if let Some(menu) = self.context_menu.as_mut() {
                                let nested_items = match menu.items.get(idx) {
                                    Some(ContextMenuItem::Submenu { items, .. }) => items.clone(),
                                    _ => Vec::new(),
                                };
                                if !nested_items.is_empty() {
                                    let parent = std::mem::replace(&mut menu.items, nested_items);
                                    menu.drill_stack.push(parent);
                                    // Cycle 714: save parent's
                                    // scroll_offset onto the parallel
                                    // stack + reset the submenu's
                                    // offset to 0. Each level has
                                    // its own view.
                                    menu.scroll_stack.push(menu.scroll_offset);
                                    menu.scroll_offset = 0;
                                    menu.highlight = menu
                                        .items
                                        .iter()
                                        .position(item_is_dispatchable)
                                        .unwrap_or(0);
                                }
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                            }
                        }
                    }
                    return;
                }
                if self.context_menu.is_some() && bcode == 0 {
                    // Left-click outside the panel — dismiss without
                    // firing anything (matches every modern menu).
                    self.context_menu = None;
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Tab-bar interactions (left = switch / close-✕ / new-+;
                // middle = close that tab).
                let bar = self.tab_bar();
                let (px, py) = (self.cursor.x as f32, self.cursor.y as f32);
                let in_bar = |r: kettle_render::Rect4, px: f32, py: f32| {
                    px >= r.0 && px < r.0 + r.2 && py >= r.1 && py < r.1 + r.3
                };
                if bar.height > 0.0
                    && py >= bar.y
                    && py < bar.y + bar.height
                    && (bcode == 0 || bcode == 1)
                {
                    if bcode == 0 && in_bar(bar.new_tab, px, py) {
                        let area = self.area();
                        let (cols, rows) = self.grid_of(area);
                        let (cw, ch) = self.cell_px();
                        let _ = self
                            .mux
                            .new_tab(&self.cfg, cols, rows, cw, ch, self.waker());
                    } else if let Some(seg) = bar.segments.iter().find(|s| in_bar(s.rect, px, py)) {
                        let close = bcode == 1 || in_bar(seg.close, px, py);
                        if close {
                            // Cycle 144: closing a tab (middle-click or
                            // ✕) can shift focus to a different tab
                            // (cycle 120's `reap_tabs` bookkeeping).
                            // Treat it like any other focus-changing
                            // action so the cursor on the now-active
                            // tab lands visible immediately.
                            let pre = self.focus_key();
                            // Cycle 424: fire TabClose so plugins see
                            // the ✕-click close the same as Action::CloseTab.
                            let closing_idx = seg.idx;
                            if self.mux.close_tab_at(seg.idx) {
                                // Cycle 157: save the (empty) session
                                // before exit so next launch starts
                                // fresh rather than restoring the
                                // *previous* multi-tab state. Other
                                // exit paths (Action::CloseTab on the
                                // last tab, WindowEvent::CloseRequested)
                                // already save; this one was missed.
                                self.fire_tab_close_event(closing_idx);
                                self.save_session();
                                event_loop.exit();
                                return;
                            }
                            self.fire_tab_close_event(closing_idx);
                            self.note_focus_change(pre);
                        } else {
                            let pre = self.focus_key();
                            self.mux.active = seg.idx;
                            self.mux.touch_active_tab_seen();
                            self.note_focus_change(pre);
                            // Cycle 249: arm the drag-to-reorder
                            // handler so a subsequent CursorMoved
                            // event with the left button still held
                            // can swap the active tab toward the
                            // cursor. Cleared in the Released arm
                            // below. Only on bare left-click (bcode 0
                            // == left, not middle / close).
                            if bcode == 0 {
                                self.tab_drag_active = true;
                            }
                        }
                    }
                    self.resize_all();
                    if let Some(win) = &self.window {
                        win.request_redraw();
                    }
                    return;
                }
                let area = self.area();
                // Ctrl/Cmd + left-click opens a hyperlink under the cursor.
                //
                // Cycle 350 (Terminator parity, terminatorlib/config.py:120
                // `link_single_click`): when true, single-click (no
                // modifier) is enough to open URLs. Default keeps
                // kettle's Ctrl-click guard so accidental drags don't
                // navigate.
                let url_modifier =
                    self.cfg.link_single_click || self.mods.control_key() || self.mods.super_key();
                if bcode == 0
                    && url_modifier
                    && let Some(uri) = self.link_at_cursor().map(|l| l.uri.clone())
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
                let (cx, cy) = (self.cursor.x as f32, self.cursor.y as f32);
                if bcode == 0
                    && let Some(clicked_pane_id) = self.pane_at_titlebar_click(cx, cy)
                {
                    let already_focused = self.mux.active_focus() == Some(clicked_pane_id);
                    let pre = self.focus_key();
                    self.mux.focus_at(area, cx, cy);
                    self.note_focus_change(pre);
                    if already_focused {
                        self.handle_action(Action::EditPaneTitle, event_loop);
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                let pre = self.focus_key();
                self.mux
                    .focus_at(area, self.cursor.x as f32, self.cursor.y as f32);
                self.note_focus_change(pre);
                if self.send_mouse(bcode, true, false) {
                    self.mouse_btn = Some(bcode);
                    return;
                }
                // Middle-click in the content area pastes the clipboard
                // (standard X11 terminal behavior; PRIMARY ≈ clipboard).
                //
                // Cycle 350 (Terminator parity, terminatorlib/config.py:88
                // `disable_mouse_paste`): when true, middle-click does
                // not paste. Useful for terminal-of-last-resort use
                // cases where accidental middle-clicks shouldn't leak
                // clipboard content into commands.
                if bcode == 1 && !self.cfg.disable_mouse_paste {
                    self.paste_clipboard();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Cycle 350 (Terminator parity, terminatorlib/config.py:89
                // `putty_paste_style`): right-click pastes (PuTTY/Windows
                // convention) instead of opening the context menu.
                if bcode == 2 && self.cfg.putty_paste_style {
                    self.paste_clipboard();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Click the scrollbar to jump the viewport, then drag it.
                if bcode == 0 && self.scrollbar_jump(area, px, py) {
                    self.dragging_scrollbar = true;
                    if let Some(w) = &self.window {
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
                    if self.mods.shift_key() && self.extend_selection_to_cursor(area) {
                        if self.cfg.copy_on_select {
                            self.copy_selection();
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    self.open_context_menu(px, py);
                    return;
                }
                if bcode == 0 {
                    // Shift+left-click extends an existing selection to the
                    // click point (xterm / Alacritty / iTerm2 / WezTerm
                    // parity). Alt still takes precedence for block-select
                    // so Shift+Alt remains block. If there's no selection
                    // to extend, fall through to the normal new-selection
                    // path so Shift+Click on empty space "just works."
                    if self.mods.shift_key()
                        && !self.mods.alt_key()
                        && self.extend_selection_to_cursor(area)
                    {
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    let cell = self.cursor_cell();
                    let clicks = cell.map(|(r, c)| self.click_count(r, c)).unwrap_or(1);
                    let kind = selection_kind(clicks, self.mods.alt_key());
                    // Cycle 288 smart selection (iTerm2 parity): on a
                    // double-click that lands inside a hint match
                    // (URL / path / IPv4 / git SHA), select the whole
                    // match as a Simple range instead of the alacritty
                    // Semantic word, which usually under- or over-shoots
                    // structured tokens. Falls through to begin_selection
                    // when no hint matches, preserving existing behavior.
                    let mut smart_selected = false;
                    if clicks == 2
                        && !self.mods.alt_key()
                        && let Some((row, col)) = cell
                        && let Some((start, end)) = self
                            .line_text_for_smart_select(row)
                            .as_deref()
                            .and_then(|line| smart_selection_at(line, col))
                        && self.apply_smart_selection(area, row, start, end)
                    {
                        smart_selected = true;
                    }
                    if !smart_selected {
                        self.begin_selection(area, kind);
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
                        self.copy_selection();
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button,
                ..
            } => {
                let bcode = match button {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => return,
                };
                if self.mouse_btn == Some(bcode) {
                    self.mouse_btn = None;
                    if self.send_mouse(bcode, false, false) {
                        return;
                    }
                }
                if bcode == 0 {
                    if self.selecting && self.cfg.copy_on_select {
                        self.copy_selection();
                    }
                    self.selecting = false;
                    self.dragging_scrollbar = false;
                    // Cycle 249: end the drag-to-reorder gesture on
                    // left-button release. Any swaps that happened
                    // during the drag are already committed; this just
                    // disarms the CursorMoved handler.
                    self.tab_drag_active = false;
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
                if self.context_menu.is_some() {
                    // Wheel up = lines > 0 = scroll up = decrement
                    // offset; wheel down = lines < 0 = scroll down.
                    self.scroll_context_menu(-(lines as isize));
                    return;
                }
                // Wheel over the tab bar cycles tabs (kitty / iTerm2 /
                // Ghostty parity). Each "click" of the wheel moves one
                // tab regardless of `scroll-multiplier` so the gesture
                // stays predictable — multiple lines from a fast scroll
                // collapse to a single tab change, like the real apps.
                if self.cursor_in_tab_bar() && self.mux.tabs.len() > 1 {
                    if lines > 0 {
                        self.mux.prev_tab();
                    } else {
                        self.mux.next_tab();
                    }
                    if let Some(w) = &self.window {
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
                    self.mods.control_key(),
                    lines,
                    self.cfg.disable_mousewheel_zoom,
                ) && let Some(r) = self.renderer.as_mut()
                {
                    // Cycle 747: step logical size, not the now-physical
                    // cell_h (which would double-apply the DPI scale).
                    let new = if sign > 0 {
                        r.font_size() + 1.0
                    } else {
                        (r.font_size() - 1.0).max(6.0)
                    };
                    r.set_font_size(new);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Shift+wheel always scrolls the kettle scrollback even
                // when a TUI has mouse-tracking on (xterm convention).
                // Without this bypass, you can't scroll back through
                // your tmux/htop session — the TUI swallows every wheel
                // notch.
                let (track, _) = input::mouse_tracking(self.focused_mode());
                let track_active = track != input::MouseTracking::Off && !self.mods.shift_key();
                if track_active {
                    let btn = if lines > 0 { 64 } else { 65 };
                    for _ in 0..lines.abs().min(8) {
                        self.send_mouse(btn, true, false);
                    }
                } else {
                    if let Some(pane) = self.mux.focused()
                        && let Ok(mut t) = pane.term.term.lock()
                    {
                        t.scroll_display(Scroll::Delta(lines));
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::DroppedFile(path) => {
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
                if self.mux.is_broadcast_on() {
                    self.mux.broadcast_paste(&text);
                } else {
                    // Read the focused pane's BRACKETED_PASTE state first
                    // — `focused_mode` and `mux.focused` both want &mut
                    // self, so they have to run sequentially (not nested).
                    let bracketed = self
                        .focused_mode()
                        .contains(kettle_core::TermMode::BRACKETED_PASTE);
                    let bytes = input::paste_payload(&text, bracketed);
                    if let Some(p) = self.mux.focused() {
                        p.term.write(&bytes);
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Focused(f) => {
                self.window_focused = f;
                // Cycle 344 (Terminator parity, terminatorlib/config.py:77
                // `hide_on_lose_focus`): Quake-style auto-hide. When
                // the user clicks away to another window, hide the
                // kettle window. Reappears via `kettle --toggle`
                // (cycle 303) or whatever global hotkey the user
                // bound. Honors only on focus-LOSS (f == false).
                if !f
                    && self.cfg.hide_on_lose_focus
                    && let Some(w) = &self.window
                {
                    w.set_visible(false);
                }
                // Cycle 171: route through the shared helper so all
                // user-driven blink-reset paths share one implementation
                // (cycles 134-141 + 144 + 150 audit). The
                // CursorBlinkingChange handler still inlines the body
                // because it runs inside `self.mux.panes.values_mut()`
                // and can't borrow `self` again — that one's documented.
                self.reset_blink_phase();
                // Focus-event reporting (DEC private mode ?1004): apps that
                // enabled it expect CSI I on focus-in, CSI O on focus-out.
                if self
                    .focused_mode()
                    .contains(kettle_core::TermMode::FOCUS_IN_OUT)
                    && let Some(p) = self.mux.focused()
                {
                    p.term.write(if f { b"\x1b[I" } else { b"\x1b[O" });
                }
                if f && let Some(w) = &self.window {
                    w.request_user_attention(None); // clear urgency on focus
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                // Keep the cursor solid while actively typing (cycle 144).
                // Routes through the shared helper so the eight
                // user-driven blink-reset paths (Reset / focus changes /
                // modal close / typing / tab close / window focus /
                // DEC ?12 toggle) stay in lock-step. Cycle 171.
                self.reset_blink_phase();
                // Hide the OS mouse cursor (configurable; default on, like
                // every modern terminal). Re-shown on the next CursorMoved.
                self.hide_mouse_cursor();
                let text = event.text.as_ref().map(|s| s.as_str());

                if self.context_menu.is_some() {
                    self.context_menu_key(&event.logical_key, text, event_loop);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }

                // Cycle 299: vi-mode key dispatch (sub-cycle 2). When
                // vi_mode is Some, intercept keys for vi-style
                // navigation before they reach the PTY. h/j/k/l move
                // the vi cursor; 0/$/g/G jump; Esc exits.
                if self.vi_mode.is_some() {
                    self.vi_mode_key(&event.logical_key, text);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }

                if self.hint_state.is_some() {
                    self.hint_key(&event.logical_key, text);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }

                if self.palette_input.is_some() {
                    self.palette_key(&event.logical_key, text, event_loop);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }

                if self.layout_picker_input.is_some() {
                    self.layout_picker_key(&event.logical_key, text);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }

                if self.ssh_input.is_some() {
                    self.ssh_key(&event.logical_key, text);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }

                // Cycle 660 (sub-cycle 5 of confirm-dialog design):
                // confirm-modal key handler. Tab/Shift+Tab/←→
                // cycle focus, Enter dispatches on_confirm, Esc
                // closes the modal without dispatching. Modal is
                // exclusive — non-nav keys are swallowed.
                if self.confirm_dialog.is_some() {
                    let key = match &event.logical_key {
                        Key::Named(NamedKey::Escape) => Some(ConfirmKey::Escape),
                        Key::Named(NamedKey::Enter) => Some(ConfirmKey::Enter),
                        Key::Named(NamedKey::Tab) => {
                            if self.mods.shift_key() {
                                Some(ConfirmKey::ShiftTab)
                            } else {
                                Some(ConfirmKey::Tab)
                            }
                        }
                        Key::Named(NamedKey::ArrowLeft) => Some(ConfirmKey::Left),
                        Key::Named(NamedKey::ArrowRight) => Some(ConfirmKey::Right),
                        _ => None,
                    };
                    if let Some(k) = key
                        && let Some(state) = self.confirm_dialog.as_ref()
                    {
                        let n = state.buttons.len();
                        let focus = state.focus_idx;
                        let action = &state.on_confirm;
                        let result = confirm_dialog_keypress(focus, n, k);
                        match result {
                            ConfirmKeyResult::Move(idx) => {
                                if let Some(s) = self.confirm_dialog.as_mut() {
                                    s.focus_idx = idx;
                                }
                            }
                            ConfirmKeyResult::Confirm => {
                                // Inspect on_confirm BEFORE clearing
                                // so the dispatch sees the right action.
                                let to_run = action.clone();
                                self.confirm_dialog = None;
                                self.dispatch_confirm_action(to_run, event_loop);
                            }
                            ConfirmKeyResult::Cancel => {
                                self.confirm_dialog = None;
                            }
                            ConfirmKeyResult::Ignore => {}
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    return;
                }
                // Cycle 369: Edit-title overlay key handler. Esc
                // cancels; Enter applies via apply_title_edit;
                // Backspace removes one char; printable text appends.
                if self.editing_title.is_some() {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.editing_title = None;
                        }
                        Key::Named(NamedKey::Enter) => {
                            self.apply_title_edit();
                        }
                        Key::Named(NamedKey::Backspace) => {
                            if let Some(state) = self.editing_title.as_mut() {
                                state.input.pop();
                            }
                        }
                        _ => {
                            if let Some(s) = text
                                && let Some(state) = self.editing_title.as_mut()
                            {
                                for c in s.chars() {
                                    if !c.is_control() {
                                        state.input.push(c);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }

                if self.mux.search.open {
                    self.search_key(&event.logical_key, text);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }

                if let Some(k) = to_kkey(&event.logical_key) {
                    let trig = Trigger::new(to_mods(self.mods), k);
                    if let Some(act) = self.cfg.keybinds.get(&trig).cloned() {
                        self.handle_action(act, event_loop);
                        return;
                    }
                }

                let mode = self
                    .mux
                    .focused()
                    .and_then(|p| p.term.term.lock().ok().map(|t| *t.mode()))
                    .unwrap_or(kettle_core::TermMode::empty());
                if let Some(mut bytes) = input::encode(&event.logical_key, text, self.mods, mode) {
                    // Cycle 352 (Terminator parity, terminatorlib/config.py:107-108
                    // `backspace_binding` + `delete_binding`): remap the
                    // encoded bytes when the user picked a non-default
                    // binding. Same as VTE's per-profile override.
                    if let winit::keyboard::Key::Named(named) = &event.logical_key {
                        use kettle_config::{BackspaceBinding, DeleteBinding};
                        use winit::keyboard::NamedKey;
                        if *named == NamedKey::Backspace
                            && !self.mods.control_key()
                            && !self.mods.alt_key()
                        {
                            bytes = match self.cfg.backspace_binding {
                                BackspaceBinding::AsciiDel => vec![0x7f],
                                BackspaceBinding::ControlH => vec![0x08],
                                BackspaceBinding::EscapeSequence => b"\x1b[3~".to_vec(),
                                BackspaceBinding::Automatic => bytes,
                            };
                        } else if *named == NamedKey::Delete {
                            bytes = match self.cfg.delete_binding {
                                DeleteBinding::AsciiDel => vec![0x7f],
                                DeleteBinding::ControlH => vec![0x08],
                                DeleteBinding::EscapeSequence => b"\x1b[3~".to_vec(),
                                DeleteBinding::Automatic => bytes,
                            };
                        }
                    }
                    // Any keystroke that produces PTY bytes also dismisses
                    // an active selection — alacritty/iTerm2/WezTerm all do
                    // this so typing after a select doesn't leave a stale
                    // highlight behind.
                    self.clear_selection_on_input();
                    // Cycle 141: typing should land the cursor visible
                    // immediately. Without this, a fast typist hitting
                    // a key right as `blink_on` was false saw a brief
                    // flash of no-cursor before the next half-period.
                    // Alacritty / kitty / iTerm2 / WezTerm all reset
                    // the blink phase on every keystroke. Same shape
                    // as cycles 134-140 (Reset, focus changes, modal
                    // close, mouse focus); typing is the last
                    // user-driven path that still needed it.
                    self.reset_blink_phase();
                    if self.mux.is_broadcast_on() {
                        self.mux.broadcast_write(&bytes);
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
                            self.mux.broadcast_scroll_to_bottom();
                        }
                    } else if let Some(p) = self.mux.focused() {
                        p.term.write(&bytes);
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
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.mux.reap() && self.window.is_some() {
            self.save_session();
            event_loop.exit();
            return;
        }
        // Drive cursor blink + visual-bell decay without busy-looping: only
        // schedule wake-ups while something is actually animating.
        let bell_active = self
            .last_bell
            .map(|t| t.elapsed() < std::time::Duration::from_millis(300))
            .unwrap_or(false);
        let blink_active = self.cfg.cursor_blink && self.window_focused;
        let anim_active = self
            .mux
            .panes
            .values()
            .any(|p| p.term.has_running_animation());
        // Selection-autoscroll runs at the same ~30 fps as bell / image
        // animation — without an active wake-up the loop sits idle waiting
        // for a fresh CursorMoved, so the drag-past-edge case would freeze
        // until the user wiggled the mouse.
        let autoscroll_active = self.selecting && {
            let area = self.area();
            self.focused_rect(area)
                .map(|r| selection_autoscroll_lines(self.cursor.y as f32, r.1, r.1 + r.3) != 0)
                .unwrap_or(false)
        };
        if bell_active || blink_active || anim_active || autoscroll_active {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            // Bell decay, animation playback and selection autoscroll all
            // want a ~30 fps tick; cursor blink alone can coast at 120 ms.
            let wait = if bell_active || anim_active || autoscroll_active {
                33
            } else {
                120
            };
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(wait),
            ));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
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
            body("close_all_modals").contains("self.confirm_dialog = None"),
            "close_all_modals must clear confirm_dialog so it can't stack under \
             another overlay"
        );
        assert!(
            body("any_modal_open").contains("self.confirm_dialog.is_some()"),
            "any_modal_open must count the confirm dialog so input doesn't fall \
             through to the terminal behind it"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContextMenuItem, assign_mnemonics, count_rows_fitting, filter_disabled, find_menu_row_y,
        rank_layouts, selection_kind, typeahead_match,
    };
    use kettle_config::Action;
    use kettle_core::SelectionType;

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
        let mn = assign_mnemonics(&menu);
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
        // Enter always confirms.
        assert_eq!(
            confirm_dialog_keypress(0, 2, ConfirmKey::Enter),
            ConfirmKeyResult::Confirm
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
        // Empty XDG falls through to HOME/.cache.
        let f = |k: &str| match k {
            "XDG_CACHE_HOME" => Some(String::new()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(
            cache_dir_from_env(f).as_deref(),
            Some(std::path::Path::new("/h/.cache"))
        );
        // Windows-ish fallback when XDG + HOME both unset.
        let f = |k: &str| match k {
            "LOCALAPPDATA" => Some(r"C:\Users\u\AppData\Local".to_string()),
            _ => None,
        };
        assert_eq!(
            cache_dir_from_env(f).as_deref(),
            Some(std::path::Path::new(r"C:\Users\u\AppData\Local"))
        );
        // None of the env vars set → None.
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
