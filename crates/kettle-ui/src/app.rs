//! winit application: window lifecycle, input routing, the tiled multiplexer,
//! the search overlay, clipboard, and live config reload.

use std::sync::Arc;

use anyhow::Result;
use kettle_config::{Action, Config, Key as KKey, Mods, Trigger};
use kettle_core::{Scroll, TermEvent};
use kettle_render::{HighlightRect, Overlay, PaneView, Renderer};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::input;
use crate::mux::{Dir, Mux, Rect};

#[derive(Debug, Clone)]
pub enum UserEvent {
    Wakeup,
    ReloadConfig,
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
    _watcher: Option<notify::RecommendedWatcher>,
}

impl App {
    pub fn run() -> Result<()> {
        let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();

        let mut watcher = None;
        if let Some(path) = Config::default_path()
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
            cfg: Config::load(),
            window: None,
            renderer: None,
            mux: Mux::new(),
            mods: ModifiersState::empty(),
            proxy,
            clipboard: arboard::Clipboard::new().ok(),
            fullscreen: false,
            cursor: PhysicalPosition::new(0.0, 0.0),
            selecting: false,
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
        if self.mux.tabs.len() > 1 {
            self.renderer
                .as_ref()
                .map(|r| r.cell_h + 8.0)
                .unwrap_or(24.0)
        } else {
            0.0
        }
    }

    /// Content area below the tab bar, in physical pixels.
    fn area(&self) -> Rect {
        let (w, h) = self
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        let tb = self.tab_bar_h();
        (0.0, tb, w as f32, (h as f32 - tb).max(1.0))
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

    fn begin_selection(&mut self, area: Rect) {
        self.selecting = true;
        if let Some(rect) = self.focused_rect(area) {
            let p = self.px_to_point(rect, self.cursor.x as f32, self.cursor.y as f32);
            if let Some(pane) = self.mux.focused()
                && let Ok(mut t) = pane.term.term.lock()
            {
                t.selection = Some(kettle_core::Selection::new(
                    kettle_core::SelectionType::Simple,
                    p,
                    kettle_core::Side::Left,
                ));
            }
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
        let mut title: Option<String> = None;
        let focus = self.mux.active_focus();
        for (id, pane) in self.mux.panes.iter_mut() {
            while let Ok(ev) = pane.rx.try_recv() {
                match ev {
                    TermEvent::Title(t) => {
                        pane.title = t.clone();
                        if Some(*id) == focus {
                            title = Some(t);
                        }
                    }
                    TermEvent::ResetTitle => pane.title = "kettle".into(),
                    TermEvent::PtyWrite(s) => pane.term.write(s.as_bytes()),
                    TermEvent::ClipboardStore(_, s) => {
                        if let Some(cb) = &mut self.clipboard {
                            let _ = cb.set_text(s);
                        }
                    }
                    TermEvent::ClipboardLoad(_, fmt) => {
                        let text = self
                            .clipboard
                            .as_mut()
                            .and_then(|c| c.get_text().ok())
                            .unwrap_or_default();
                        pane.term.write(fmt(&text).as_bytes());
                    }
                    TermEvent::Exit | TermEvent::ChildExit(_) => pane.closed = true,
                    _ => {}
                }
            }
        }
        if let Some(t) = title
            && let Some(w) = &self.window
        {
            w.set_title(&format!("{t} — kettle"));
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
        let s = &mut self.mux.search;
        s.matches = matches;
        if s.index >= s.matches.len() {
            s.index = 0;
        }
    }

    fn overlay(&self) -> Overlay {
        let s = &self.mux.search;
        if !s.open {
            return Overlay::default();
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
        }
    }

    fn redraw(&mut self) {
        self.drain_events();
        if self.mux.reap() {
            return;
        }
        self.update_search();
        let overlay = self.overlay();
        let area = self.area();
        let tab_bar_h = self.tab_bar_h();
        let active = self.mux.active;
        let titles = self.mux.tab_titles();
        let layout = self.mux.layout(active, area);
        let focus = self.mux.active_focus();

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        // Lock every visible pane, then hand references to the renderer.
        let mut guards = Vec::with_capacity(layout.len());
        for (id, r) in &layout {
            if let Some(p) = self.mux.panes.get(id)
                && let Ok(g) = p.term.term.lock()
            {
                guards.push((*r, g, Some(*id) == focus));
            }
        }
        let panes: Vec<PaneView> = guards
            .iter()
            .map(|(r, g, f)| PaneView {
                rect: *r,
                term: g,
                focused: *f,
            })
            .collect();
        if let Err(e) =
            renderer.render_frame(&panes, &titles, active, tab_bar_h, &self.cfg, &overlay)
        {
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
            Action::Paste => {
                let text = self
                    .clipboard
                    .as_mut()
                    .and_then(|c| c.get_text().ok())
                    .unwrap_or_default();
                if let Some(p) = self.mux.focused() {
                    p.term.write(text.as_bytes());
                }
            }
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
            }
            Action::ToggleBroadcastAll => self.mux.broadcast = true,
            Action::ToggleBroadcastOff => self.mux.broadcast = false,
            Action::ToggleZoom => {}
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
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn reload_config(&mut self) {
        let new = Config::load();
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
        if let Err(e) = self
            .mux
            .new_tab(&self.cfg, cols, rows, cw, ch, self.waker())
        {
            log::error!("failed to spawn shell: {e}");
            event_loop.exit();
            return;
        }
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
            WindowEvent::CloseRequested => event_loop.exit(),
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
                if self.selecting {
                    let area = self.area();
                    self.update_selection(area);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let area = self.area();
                self.mux
                    .focus_at(area, self.cursor.x as f32, self.cursor.y as f32);
                self.begin_selection(area);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.selecting = false,
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y.round() as i32 * 3,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y / 20.0) as i32,
                };
                if lines != 0 {
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
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                let text = event.text.as_ref().map(|s| s.as_str());

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
            event_loop.exit();
        }
    }
}
