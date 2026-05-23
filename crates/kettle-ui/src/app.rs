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
#[allow(dead_code)] // sub-cycle 5 wires the consumer.
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
#[allow(dead_code)] // sub-cycle 5 consumes this.
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
#[allow(dead_code)] // sub-cycle 5 wires the consumer.
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

/// Cycle 651 (sub-cycle 2 of [`TERMINATOR-VERTICAL-TABS-DESIGN.md`](
/// ../../../docs/TERMINATOR-VERTICAL-TABS-DESIGN.md)): pure helper
/// that computes the pane-content rect from the inputs that govern
/// it — surface size, tab-bar height, status-bar height, and the
/// edge each occupies.
///
/// Returns `(x, y, width, height)` in pixel coordinates.
///
/// Cycle-651 v1 of this helper treats vertical-strip positions
/// (`Left` / `Right`) the same as `Top` (the strip claims the
/// same per-edge slice). Sub-cycle 4 of the vertical-tabs design
/// will branch on the orientation + carve out a per-strip width
/// instead of a per-edge height.
///
/// Pure — no `&self`, no renderer, no winit. Drives the `App::area`
/// method (which now wraps this helper) so cycle-651 + future
/// vertical-strip wiring can test the layout math without
/// constructing a full App.
fn content_rect_for(
    surface: (u32, u32),
    tab_bar_h: f32,
    status_bar_h: f32,
    tab_bar_pos: kettle_config::TabBarPos,
    status_bar_mode: kettle_config::StatusBarMode,
) -> Rect {
    let (sw, sh) = (surface.0 as f32, surface.1 as f32);
    let tb_on_top = matches!(
        tab_bar_pos,
        kettle_config::TabBarPos::Top
            | kettle_config::TabBarPos::Left
            | kettle_config::TabBarPos::Right
    );
    let tb_on_bottom = matches!(tab_bar_pos, kettle_config::TabBarPos::Bottom);
    let sb_on_top = matches!(status_bar_mode, kettle_config::StatusBarMode::Top);
    let sb_on_bottom = matches!(status_bar_mode, kettle_config::StatusBarMode::Bottom);

    let top_offset =
        (if tb_on_top { tab_bar_h } else { 0.0 }) + (if sb_on_top { status_bar_h } else { 0.0 });
    let bot_offset = (if tb_on_bottom { tab_bar_h } else { 0.0 })
        + (if sb_on_bottom { status_bar_h } else { 0.0 });
    let content_h = (sh - top_offset - bot_offset).max(1.0);
    (0.0, top_offset, sw, content_h)
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
#[allow(dead_code)] // sub-cycle 4 wires the dispatch usage.
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
    // Cycle 647: Left/Right (vertical strip) don't use the y-band
    // check at all — hit-testing is x-axis-based. Until the render
    // layer fully wires the vertical strip (sub-cycles 2-6 of
    // [`TERMINATOR-VERTICAL-TABS-DESIGN.md`](
    // ../../../docs/TERMINATOR-VERTICAL-TABS-DESIGN.md)) those
    // values fall through to "no hit on the y-band" so the rest
    // of the y-band-dependent code stays correct.
    match pos {
        TabBarPos::Top => y >= 0.0 && y < bar_h,
        TabBarPos::Bottom => y >= (surface_h - bar_h) && y <= surface_h,
        TabBarPos::Left | TabBarPos::Right => false,
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
}

/// and click dispatch; `Item` rows carry the action to fire.
#[derive(Clone)]
enum ContextMenuItem {
    Item {
        label: &'static str,
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

#[derive(Debug, Clone)]
pub struct TitleEditState {
    pub scope: TitleEditScope,
    /// Current text the user has typed. Pre-filled with the existing
    /// title so the user can edit in place vs starting blank.
    pub input: String,
}

/// Cycle 648 (sub-cycle 2 of [`TERMINATOR-CONFIRM-DIALOG-DESIGN.md`](
/// ../../../docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md)):
/// the action a confirmed modal will dispatch when the user accepts.
///
/// First user is the cycle-637 `ask_before_closing` flow
/// (`Action::CloseWindow` / `CloseTab` / `ClosePane`). Future cycles
/// add `KillProcess`, `DiscardLayout`, `ResetConfig` etc. — the
/// enum is intentionally extensible.
#[allow(dead_code)] // sub-cycle 5 wires the dispatch usage.
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
#[allow(dead_code)] // sub-cycle 3/4 wire renderer + keyboard nav.
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
#[allow(dead_code)] // sub-cycle 3-5 wire the consumers.
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

/// Pure: walk the menu item list to find the next enabled, non-
/// separator row index, given a `delta` (±1) and a wrap-around at the
/// list ends. Used by both `↑` and `↓` keyboard nav. Returns `current`
/// unchanged if no enabled rows exist at all (defensive — the menu
/// shouldn't have been opened with zero actionable rows).
fn item_is_dispatchable(item: &ContextMenuItem) -> bool {
    matches!(
        item,
        ContextMenuItem::Item { enabled: true, .. }
            | ContextMenuItem::LuaItem { .. }
            | ContextMenuItem::ConfigItem { .. }
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
    #[allow(dead_code)] // wiring lands in sub-cycle 3-5.
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
            renderer: None,
            mux: {
                let mut m = Mux::new();
                m.lua_output_subscribed = lua_output_subscribed;
                m
            },
            mods: ModifiersState::empty(),
            proxy,
            clipboard: arboard::Clipboard::new().ok(),
            fullscreen: false,
            cursor: PhysicalPosition::new(0.0, 0.0),
            selecting: false,
            dragging_scrollbar: false,
            search_revealed: None,
            mouse_btn: None,
            links: Vec::new(),
            ssh_input: None,
            palette_input: None,
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
        let (_, sh) = self
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        cursor_in_tab_bar_band(self.cursor.y as f32, h, sh as f32, self.cfg.tab_bar_pos)
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
        // Cycle 651: delegate to the pure helper so layout math is
        // unit-testable without constructing a full App.
        content_rect_for(
            surface,
            self.tab_bar_h(),
            self.status_bar_h(),
            self.cfg.tab_bar_pos,
            self.cfg.status_bar,
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
        // Cycle 647: vertical strip rendering (Left / Right) is
        // designed in [`TERMINATOR-VERTICAL-TABS-DESIGN.md`](
        // ../../../docs/TERMINATOR-VERTICAL-TABS-DESIGN.md). Until
        // those sub-cycles land, Left + Right route to the same
        // y = 0.0 as Top (the user's strip configuration parses +
        // stores correctly; only the layout doesn't honor it yet).
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
            broadcast: self.mux.broadcast,
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
        if self.mux.broadcast {
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
        let s = &self.mux.search;
        if !s.open {
            return Overlay {
                links,
                ssh_query,
                ssh_hint,
                palette_query,
                palette_hint,
                edit_title,
                hint_labels,
                window_focused,
                cursor_visible,
                bell,
                context_menu,
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
            edit_title,
            hint_labels,
            window_focused,
            cursor_visible,
            bell,
            context_menu,
            vi_cursor: self.vi_mode.map(|v| (v.row, v.col)),
            vi_visual_anchor: self.vi_mode.and_then(|v| v.visual_anchor),
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
        self.hint_state = None;
        self.ssh_input = None;
        self.context_menu = None;
        self.editing_title = None;
        // Cycle 298 vi-mode behaves like a modal — Esc exits it,
        // close_all_modals exits it. Sub-cycle 1.
        self.vi_mode = None;
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
                    if let Some(p) = self.mux.focused() {
                        p.group_name = if value.is_empty() { None } else { Some(value) };
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
            || self.hint_state.is_some()
            || self.ssh_input.is_some()
            || self.context_menu.is_some()
            || self.editing_title.is_some()
            || self.vi_mode.is_some()
    }

    /// Build the right-click context-menu item list. Copy is enabled
    /// only when the focused pane has a non-empty selection (matches
    /// Terminator / GNOME Terminal: the row stays visible but greyed
    /// out, so the user sees the option exists without it being
    /// actionable when nothing is selected). All other items are
    /// always enabled.
    fn context_menu_items(&mut self) -> Vec<ContextMenuItem> {
        let has_selection = self
            .mux
            .focused()
            .and_then(|p| p.term.term.lock().ok().map(|t| t.selection.is_some()))
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
        // Highlight the first enabled non-separator item.
        let highlight = items.iter().position(item_is_dispatchable).unwrap_or(0);
        let (cw, ch) = self.cell_px();
        let (cw, ch) = (cw as f32, ch as f32);
        let row_h = ch + 12.0;
        let sep_h = 8.0_f32;
        let panel_h: f32 = items
            .iter()
            .map(|it| match it {
                ContextMenuItem::Separator => sep_h,
                ContextMenuItem::Item { .. }
                | ContextMenuItem::LuaItem { .. }
                | ContextMenuItem::ConfigItem { .. } => row_h,
            })
            .sum();
        let max_chars = items
            .iter()
            .filter_map(|it| match it {
                ContextMenuItem::Item { label, .. } => Some(label.chars().count()),
                ContextMenuItem::LuaItem { label, .. } => Some(label.chars().count()),
                ContextMenuItem::ConfigItem { label, .. } => Some(label.chars().count()),
                _ => None,
            })
            .max()
            .unwrap_or(0) as f32;
        let panel_w = (max_chars * cw + 40.0).max(180.0);
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
        let Some(menu) = self.context_menu.as_mut() else {
            return;
        };
        let next = next_context_menu_highlight(&menu.items, menu.highlight, delta);
        menu.highlight = next;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Resolve a mouse-button press into a context-menu action, if any.
    /// Only a *left*-click (bcode 0) inside the panel can fire a row
    /// — right and middle clicks are ignored so right-click re-anchor
    /// still feels distinct from "select this menu item." Returns
    /// `None` if the click missed the panel, hit a separator, or hit a
    /// disabled row; the caller then either dismisses (left-click
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
        let row_h = ch as f32 + 12.0;
        let sep_h = 8.0_f32;
        let mut row_y = ay;
        for item in &menu.items {
            let h = match item {
                ContextMenuItem::Separator => sep_h,
                ContextMenuItem::Item { .. }
                | ContextMenuItem::LuaItem { .. }
                | ContextMenuItem::ConfigItem { .. } => row_h,
            };
            if py >= row_y && py < row_y + h {
                match item {
                    ContextMenuItem::Item {
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
        let row_h = ch + 12.0;
        let sep_h = 8.0_f32;
        let panel_h: f32 = menu
            .items
            .iter()
            .map(|it| match it {
                ContextMenuItem::Separator => sep_h,
                ContextMenuItem::Item { .. }
                | ContextMenuItem::LuaItem { .. }
                | ContextMenuItem::ConfigItem { .. } => row_h,
            })
            .sum();
        let max_chars = menu
            .items
            .iter()
            .filter_map(|it| match it {
                ContextMenuItem::Item { label, .. } => Some(label.chars().count()),
                ContextMenuItem::LuaItem { label, .. } => Some(label.chars().count()),
                ContextMenuItem::ConfigItem { label, .. } => Some(label.chars().count()),
                _ => None,
            })
            .max()
            .unwrap_or(0) as f32;
        let panel_w = (max_chars * cw + 40.0).max(180.0);
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
            })
            .collect();
        Some(ContextMenu {
            anchor: menu.anchor,
            rows,
            highlight: menu.highlight,
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
                if self.mux.close_focused() {
                    event_loop.exit();
                }
            }
            Action::CloseTab => {
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
                    let new = match action {
                        Action::IncreaseFontSize => r.cell_h / 1.25 + 1.0,
                        Action::DecreaseFontSize => (r.cell_h / 1.25 - 1.0).max(6.0),
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
            Action::ToggleBroadcastAll => self.mux.broadcast = true,
            Action::ToggleBroadcastOff => self.mux.broadcast = false,
            Action::ToggleZoom => {
                self.mux.toggle_zoom();
                self.resize_all();
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
                if self.mux.broadcast {
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
                // Cycle 654 (sub-cycle 3 of terminalshot design).
                // Computes the output path via cycle-650
                // session_screenshot_path + queues a request on
                // the renderer's pending_screenshot slot.
                // Sub-cycle 4 wires the actual wgpu readback +
                // PNG encode + write to out_path; for now the
                // request sits unread until that sub-cycle lands.
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
                    renderer.set_pending_screenshot(kettle_render::ScreenshotRequest {
                        out_path: path,
                        crop: None,
                    });
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
                });
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Action::GroupTab | Action::GroupWindow => {
                // Cycle 642 (named-groups sub-cycle 1, action
                // surface only). Bulk-assign every pane in scope
                // to a named group — same overlay as CreateGroup,
                // but on Apply the input writes to every pane in
                // scope instead of just the focused one. Wired
                // in sub-cycle 4 of
                // [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](
                // ../../../docs/TERMINATOR-NAMED-GROUPS-DESIGN.md).
                log::info!(
                    "{action:?}: action surface only — bulk-assign wiring lands in named-groups sub-cycle 4"
                );
            }
            Action::UngroupTab | Action::UngroupWindow => {
                // Cycle 642 (named-groups sub-cycle 1, action
                // surface only). Bulk-clear the group on every
                // pane in scope. Wired in sub-cycle 5.
                log::info!(
                    "{action:?}: action surface only — bulk-clear wiring lands in named-groups sub-cycle 5"
                );
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
                    r.set_font_size(r.cell_h / 1.25 + 1.0);
                }
            }
            Action::ZoomOutAll => {
                if let Some(r) = self.renderer.as_mut() {
                    r.set_font_size((r.cell_h / 1.25 - 1.0).max(6.0));
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
            // Cycle 345: paste primary selection (X11 primary clipboard).
            // arboard's get_text() reads the regular clipboard; macOS / Windows
            // / Wayland don't have a separate primary selection, so X11 primary
            // and the regular clipboard are equivalent through arboard's
            // current surface — funnel through `paste_clipboard` so the
            // primary path gets the same LOCAL_PASTE_MAX clamp, bracketed-
            // paste wrap, and broadcast scoping as Action::Paste (cycle 574:
            // the inline impl was bypassing all three).
            Action::PastePrimary => self.paste_clipboard(),
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

    /// Keyboard routing while the right-click context menu is open.
    /// `Esc` closes, `↑/↓` step the highlight (skipping separators +
    /// disabled rows via `next_context_menu_highlight`), `Enter` fires
    /// the highlighted action. Any other key is swallowed so a stray
    /// keypress doesn't leak into the focused pane while the menu is
    /// expecting nav input.
    fn context_menu_key(&mut self, key: &Key, event_loop: &ActiveEventLoop) {
        match key {
            Key::Named(NamedKey::Escape) => {
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
                // Swallow other keys — the user is in menu-nav mode.
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
        let mut attrs = Window::default_attributes().with_title("kettle");
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
            WindowAttributesExtX11::with_name(attrs, "kettle", "kettle")
        };
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
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
            WindowEvent::ScaleFactorChanged { .. } => {
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
                    let new = if sign > 0 {
                        r.cell_h / 1.25 + 1.0
                    } else {
                        (r.cell_h / 1.25 - 1.0).max(6.0)
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
                if self.mux.broadcast {
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
                    self.context_menu_key(&event.logical_key, event_loop);
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

                if self.ssh_input.is_some() {
                    self.ssh_key(&event.logical_key, text);
                    if let Some(w) = &self.window {
                        w.request_redraw();
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
                    if self.mux.broadcast {
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

#[cfg(test)]
mod tests {
    use super::selection_kind;
    use kettle_core::SelectionType;

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
        // Left (cycle 647 vertical, sub-cycle 1 only): treated as
        // Top by the v1 helper. Sub-cycle 4 will branch on
        // orientation + carve a width slice instead of a height.
        let r = content_rect_for((800, 600), 24.0, 0.0, TabBarPos::Left, StatusBarMode::Off);
        assert_eq!(r, (0.0, 24.0, 800.0, 576.0));
        // Right: same fallback as Left for now.
        let r = content_rect_for((800, 600), 24.0, 0.0, TabBarPos::Right, StatusBarMode::Off);
        assert_eq!(r, (0.0, 24.0, 800.0, 576.0));
        // Defensive: content_h clamped to >= 1.0 so a tiny window
        // doesn't degenerate to a zero/negative rect.
        let r = content_rect_for((100, 30), 24.0, 16.0, TabBarPos::Top, StatusBarMode::Bottom);
        assert!(r.3 >= 1.0);
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
