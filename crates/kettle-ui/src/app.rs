//! winit application: window lifecycle, input routing, the tiled multiplexer,
//! the search overlay, clipboard, and live config reload.

use std::sync::Arc;

use anyhow::Result;
use kettle_config::{Action, Config, Key as KKey, Mods, Trigger};
use kettle_config::{TabBarMode, TabBarPos};
use kettle_core::{Scroll, TermEvent};
use kettle_render::{HighlightRect, HintLabel, Overlay, PaneView, Renderer, TabBar, TabSeg};
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
fn chrome_cursor_icon(in_tab_bar: bool, modal_open: bool) -> Option<CursorIcon> {
    if in_tab_bar || modal_open {
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
fn cursor_in_tab_bar_band(y: f32, bar_h: f32, surface_h: f32, pos: TabBarPos) -> bool {
    if bar_h <= 0.0 {
        return false;
    }
    match pos {
        TabBarPos::Top => y >= 0.0 && y < bar_h,
        TabBarPos::Bottom => y >= (surface_h - bar_h) && y <= surface_h,
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

/// One on-screen quick-select target: where its label sits and what it is.
#[derive(Clone)]
struct HintTarget {
    row: usize,
    col: usize,
    label: String,
    kind: kettle_core::hints::Kind,
    text: String,
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
    window_focused: bool,
    /// True while the OS mouse cursor is hidden because the user is typing
    /// (`mouse-hide-while-typing`). Re-shown on the next mouse movement.
    mouse_hidden: bool,
    /// Last `CursorIcon` we pushed to the window — used to dedupe so we
    /// don't issue a `set_cursor` syscall on every CursorMoved event.
    /// `None` until the first call, which guarantees the initial state
    /// gets pushed exactly once.
    last_cursor_icon: Option<CursorIcon>,
    /// Index of the tab whose close-button (`✕`) zone the mouse cursor
    /// is currently over. Drives both the OS pointer-cursor swap and
    /// the renderer's hover-background quad so the trailing `✕` reads
    /// as a clickable button rather than part of the title text.
    /// Updated in `sync_cursor_icon` on `CursorMoved`; cleared when
    /// the cursor leaves the bar or the bar is hidden.
    hovered_close_idx: Option<usize>,
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
}

impl App {
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

        let mut app = App {
            cfg: startup
                .config
                .as_deref()
                .map(Config::load_from)
                .unwrap_or_else(Config::load),
            window: None,
            renderer: None,
            mux: Mux::new(),
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
            window_focused: true,
            mouse_hidden: false,
            last_cursor_icon: None,
            hovered_close_idx: None,
            blink_on: true,
            last_blink: std::time::Instant::now(),
            last_bell: None,
            last_click: None,
            last_title: String::new(),
            config_path: startup.config.clone(),
            startup,
            _watcher: watcher,
        };
        event_loop.run_app(&mut app)?;
        Ok(())
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
        let chrome = chrome_cursor_icon(self.cursor_in_tab_bar(), self.any_modal_open());
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

    /// Content area for panes (excludes the tab bar), in physical pixels.
    fn area(&self) -> Rect {
        let (w, h) = self
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        let tb = self.tab_bar_h();
        match self.cfg.tab_bar_pos {
            TabBarPos::Top => (0.0, tb, w as f32, (h as f32 - tb).max(1.0)),
            TabBarPos::Bottom => (0.0, 0.0, w as f32, (h as f32 - tb).max(1.0)),
        }
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
        let y = match self.cfg.tab_bar_pos {
            TabBarPos::Top => 0.0,
            TabBarPos::Bottom => sh - height,
        };
        let titles = self.mux.tab_titles();
        let n = titles.len().max(1);
        // Trailing square "+" button.
        let plus_w = height;
        let strip = (sw - plus_w).max(plus_w);
        let seg_w = strip / n as f32;
        let segments = titles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let x = i as f32 * seg_w;
                TabSeg {
                    idx: i,
                    rect: (x, y, seg_w, height),
                    // ✕ hit zone = the trailing `height`-wide square.
                    close: (x + seg_w - height, y, height, height),
                    title: t.clone(),
                    active: i == self.mux.active,
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
        // Cell size is renderer-owned and uniform across panes, so resolve it
        // once per drain rather than per event (a sixel/kitty app polling CSI
        // 14 t doesn't need a renderer lookup per CSI).
        let (cell_w, cell_h) = self.cell_px();
        for pane in self.mux.panes.values_mut() {
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
                    TermEvent::Bell => bell = true,
                    TermEvent::Exit | TermEvent::ChildExit(_) => pane.closed = true,
                    _ => {}
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
                .map(|t| kettle_core::search(&t, &query))
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

        let s = &self.mux.search;
        if !s.open {
            return Overlay {
                links,
                ssh_query,
                ssh_hint,
                palette_query,
                palette_hint,
                hint_labels,
                window_focused,
                cursor_visible,
                bell,
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
            hint_labels,
            window_focused,
            cursor_visible,
            bell,
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
        for pane in self.mux.panes.values_mut() {
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
            if kettle_core::scrollbar::should_scroll_on_output(want_sob, pane.last_history, now)
                && let Ok(mut t) = pane.term.term.lock()
            {
                t.scroll_display(Scroll::Bottom);
            }
            pane.last_history = Some(now);
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
        let active = self.mux.active;
        let layout = self.mux.layout(active, area);
        let focus = self.mux.active_focus();

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        // Lock every visible pane, then hand references to the renderer.
        let mut guards = Vec::with_capacity(layout.len());
        for (id, r) in &layout {
            if let Some(p) = self.mux.panes.get(id) {
                let mut imgs = p.term.placements();
                imgs.extend(p.term.placeholder_tiles());
                imgs.extend(p.term.relative_tiles());
                if let Ok(g) = p.term.term.lock() {
                    guards.push((*r, g, Some(*id) == focus, imgs));
                }
            }
        }
        let panes: Vec<PaneView> = guards
            .iter()
            .map(|(r, g, f, imgs)| PaneView {
                rect: *r,
                term: g,
                focused: *f,
                images: imgs.clone(),
            })
            .collect();
        if let Err(e) = renderer.render_frame(&panes, &tabbar, &self.cfg, &overlay) {
            log::warn!("render error: {e}");
        }
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
    }

    /// `true` while any modal overlay (search bar, command palette, hint
    /// mode, SSH launcher) is up. Mirrors `close_all_modals` so the two
    /// stay in lock-step — extracted in cycle 161 to drive the cursor-icon
    /// override (the OS arrow, not the I-beam, belongs over modal chrome).
    fn any_modal_open(&self) -> bool {
        self.mux.search.open
            || self.palette_input.is_some()
            || self.hint_state.is_some()
            || self.ssh_input.is_some()
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
                let _ = self.mux.new_tab(&self.cfg, cols, rows, cw, ch, waker);
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
                if self.mux.close_tab() {
                    event_loop.exit();
                }
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
                if let Some(p) = self.mux.focused()
                    && let Ok(t) = p.term.term.lock()
                    && let Some(sel) = t.selection_to_string()
                    && let Some(cb) = &mut self.clipboard
                {
                    let _ = cb.set_text(sel);
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
        s.save();
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
        }
        self.cfg = new;
        self.resize_all();
        if let Some(w) = &self.window {
            w.request_redraw();
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
                    s.index = if self.mods.shift_key() {
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
            if !kettle_core::links::is_safe_url(&h.text) {
                log::warn!("refused to open unsafe URL: {}", h.text);
            } else if let Err(e) = open::that_detached(&h.text) {
                log::warn!("failed to open {}: {e}", h.text);
            }
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
        let attrs = Window::default_attributes().with_title("kettle");
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
            match crate::session::Session::load() {
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
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Wakeup => {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            UserEvent::ReloadConfig => self.reload_config(),
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
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                // Any real mouse movement undoes the hide-while-typing
                // state. Sub-pixel movements that winit *might* coalesce
                // are fine to ignore — the next "real" motion will fire.
                self.show_mouse_cursor();
                self.sync_cursor_icon();
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
                            if self.mux.close_tab_at(seg.idx) {
                                // Cycle 157: save the (empty) session
                                // before exit so next launch starts
                                // fresh rather than restoring the
                                // *previous* multi-tab state. Other
                                // exit paths (Action::CloseTab on the
                                // last tab, WindowEvent::CloseRequested)
                                // already save; this one was missed.
                                self.save_session();
                                event_loop.exit();
                                return;
                            }
                            self.note_focus_change(pre);
                        } else {
                            let pre = self.focus_key();
                            self.mux.active = seg.idx;
                            self.note_focus_change(pre);
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
                if bcode == 0
                    && (self.mods.control_key() || self.mods.super_key())
                    && let Some(uri) = self.link_at_cursor().map(|l| l.uri.clone())
                {
                    if !kettle_core::links::is_safe_url(&uri) {
                        log::warn!("refused to open unsafe URL: {uri}");
                    } else if let Err(e) = open::that_detached(&uri) {
                        log::warn!("failed to open {uri}: {e}");
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
                if bcode == 1 {
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
                // Right-click extends an existing selection to the click
                // (xterm convention). Mouse-tracking already short-circuited
                // above when active, so this only fires for "chrome" use;
                // and we only extend an *existing* selection so a bare
                // right-click on empty space is still a no-op (avoiding a
                // surprising selection that the user didn't ask for).
                if bcode == 2 && self.extend_selection_to_cursor(area) {
                    if self.cfg.copy_on_select {
                        self.copy_selection();
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
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
                    let clicks = self
                        .cursor_cell()
                        .map(|(r, c)| self.click_count(r, c))
                        .unwrap_or(1);
                    let kind = selection_kind(clicks, self.mods.alt_key());
                    self.begin_selection(area, kind);
                    // Word/line selections resolve on press; copy them now.
                    // Simple/Block are drags — copied on button release.
                    if self.cfg.copy_on_select
                        && matches!(
                            kind,
                            kettle_core::SelectionType::Semantic
                                | kettle_core::SelectionType::Lines
                        )
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
                if let Some(bytes) = input::encode(&event.logical_key, text, self.mods, mode) {
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
            },
            TabSeg {
                idx: 1,
                rect: (100.0, 0.0, 100.0, 24.0),
                close: (176.0, 0.0, 24.0, 24.0),
                title: "two".into(),
                active: false,
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
}
