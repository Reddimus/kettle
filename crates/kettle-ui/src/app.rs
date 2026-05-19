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
use winit::window::{Fullscreen, UserAttentionType, Window, WindowId};

use crate::input;
use crate::mux::{Dir, Mux, Rect};

#[derive(Debug, Clone)]
pub enum UserEvent {
    Wakeup,
    ReloadConfig,
}

/// The OS window title for the active pane's title: `"<t> — kettle"`, or
/// just `"kettle"` when the pane has no meaningful title.
fn window_title(pane_title: &str) -> String {
    let t = pane_title.trim();
    if t.is_empty() || t == "kettle" {
        "kettle".to_string()
    } else {
        format!("{t} — kettle")
    }
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
        let mut watcher = None;
        if let Some(path) = startup.config.clone().or_else(Config::default_path)
            && let Some(dir) = path.parent().map(|p| p.to_path_buf())
        {
            let p = proxy.clone();
            use notify::Watcher;
            if let Ok(mut w) = notify::recommended_watcher(move |_| {
                let _ = p.send_event(UserEvent::ReloadConfig);
            }) {
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
        let bracketed = self
            .focused_mode()
            .contains(kettle_core::TermMode::BRACKETED_PASTE);
        let bytes = input::paste_payload(&text, bracketed);
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
                            let _ = cb.set_text(s);
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
        let active = self
            .mux
            .active_focus()
            .and_then(|id| self.mux.panes.get(&id))
            .map(|p| p.title.as_str())
            .unwrap_or("kettle");
        let want = window_title(active);
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
        let cursor_visible = if !self.cfg.cursor_blink
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
        // Advance the cursor blink phase (configurable half-period).
        if self.cfg.cursor_blink
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
        self.update_search();
        self.update_links();
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

    fn handle_action(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        let area = self.area();
        let (cols, rows) = self.grid_of(area);
        let (cw, ch) = self.cell_px();
        let waker = self.waker();
        match action {
            Action::NewTab | Action::NewWindow => {
                let _ = self.mux.new_tab(&self.cfg, cols, rows, cw, ch, waker);
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
            Action::CloseWindow | Action::CloseTab => {
                if self.mux.close_tab() {
                    event_loop.exit();
                }
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
            Action::Reset => {
                if let Some(p) = self.mux.focused() {
                    p.term.write(b"\x1bc");
                }
            }
            Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollToTop
            | Action::ScrollToBottom => {
                if let Some(p) = self.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    t.scroll_display(match action {
                        Action::ScrollPageUp => Scroll::PageUp,
                        Action::ScrollPageDown => Scroll::PageDown,
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
                self.ssh_input = Some(String::new());
            }
            Action::CommandPalette => {
                self.palette_input = Some((String::new(), 0));
            }
            Action::HintMode => {
                let targets = self.collect_hints();
                if !targets.is_empty() {
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
            Action::MoveTabLeft | Action::MoveTabRight => {}
            Action::GotoTab(n) => {
                let i = n as usize;
                if i < self.mux.tabs.len() {
                    self.mux.active = i;
                }
            }
        }
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
            Key::Named(NamedKey::Escape) => self.mux.search.open = false,
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
            Key::Named(NamedKey::Escape) => self.hint_state = None,
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
            Key::Named(NamedKey::Escape) => self.palette_input = None,
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
            Key::Named(NamedKey::Escape) => self.ssh_input = None,
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
                        && name != self.cfg.theme_name
                        && kettle_config::Theme::list().contains(&name)
                    {
                        self.cfg.theme_name = name.to_string();
                        self.cfg.theme = kettle_config::Theme::by_name(name);
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
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
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
                            if self.mux.close_tab_at(seg.idx) {
                                event_loop.exit();
                                return;
                            }
                        } else {
                            self.mux.active = seg.idx;
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
                self.mux
                    .focus_at(area, self.cursor.x as f32, self.cursor.y as f32);
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
                if bcode == 0 {
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
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y.round() as i32 * 3,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y / 20.0) as i32,
                };
                if lines == 0 {
                    return;
                }
                let (track, _) = input::mouse_tracking(self.focused_mode());
                if track != input::MouseTracking::Off {
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
            WindowEvent::Focused(f) => {
                self.window_focused = f;
                self.blink_on = true;
                self.last_blink = std::time::Instant::now();
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
                // Keep the cursor solid while actively typing.
                self.blink_on = true;
                self.last_blink = std::time::Instant::now();
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
                    if self.mux.broadcast {
                        self.mux.broadcast_write(&bytes);
                    } else if let Some(p) = self.mux.focused() {
                        p.term.write(&bytes);
                        if let Ok(mut t) = p.term.term.lock() {
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
        if bell_active || blink_active || anim_active {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            // Bell decay and animation playback both want a ~30 fps tick;
            // cursor blink alone can coast at a coarse 120 ms.
            let wait = if bell_active || anim_active { 33 } else { 120 };
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
    fn window_title_formats_and_falls_back() {
        use super::window_title;
        assert_eq!(window_title("vim README.md"), "vim README.md — kettle");
        assert_eq!(window_title("  spaced  "), "spaced — kettle");
        // Empty / placeholder titles → just the app name.
        assert_eq!(window_title(""), "kettle");
        assert_eq!(window_title("   "), "kettle");
        assert_eq!(window_title("kettle"), "kettle");
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
