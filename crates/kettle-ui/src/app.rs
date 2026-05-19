//! winit application: window lifecycle, input routing, the multiplexer, the
//! search overlay, clipboard, and live config reload.

use std::sync::Arc;

use anyhow::Result;
use kettle_config::{Action, Config, Key as KKey, Mods, Trigger};
use kettle_core::{Scroll, TermEvent};
use kettle_render::{HighlightRect, Overlay, Renderer};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::input;
use crate::mux::Mux;

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
    _watcher: Option<notify::RecommendedWatcher>,
}

impl App {
    pub fn run() -> Result<()> {
        let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();

        // Live config reload via a filesystem watcher.
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
            .map(|r| (r.cell_w as u16, r.cell_h as u16))
            .unwrap_or((8, 16))
    }

    fn grid(&self) -> (usize, usize) {
        self.renderer
            .as_ref()
            .map(|r| r.grid_size(self.cfg.padding_x, self.cfg.padding_y))
            .unwrap_or((80, 24))
    }

    fn resize_all(&mut self) {
        let (cols, rows) = self.grid();
        let (cw, ch) = self.cell_px();
        for tab in &mut self.mux.tabs {
            for p in &mut tab.panes {
                p.term.resize(cols, rows, cw, ch);
            }
        }
    }

    fn drain_events(&mut self) {
        let mut title: Option<String> = None;
        for tab in &mut self.mux.tabs {
            for pane in &mut tab.panes {
                while let Ok(ev) = pane.rx.try_recv() {
                    match ev {
                        TermEvent::Title(t) => {
                            pane.title = t.clone();
                            title = Some(t);
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
            if let Ok(t) = p.term.term.lock() {
                kettle_core::search(&t, &query)
            } else {
                Vec::new()
            }
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
        let (Some(renderer), Some(_w)) = (self.renderer.as_mut(), self.window.as_ref()) else {
            return;
        };
        let a = self.mux.active;
        if let Some(tab) = self.mux.tabs.get_mut(a)
            && let Some(pane) = tab.panes.get_mut(tab.focus)
            && let Ok(term) = pane.term.term.lock()
            && let Err(e) = renderer.render(&term, &self.cfg, &overlay)
        {
            log::warn!("render error: {e}");
        }
    }

    fn handle_action(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        let (cols, rows) = self.grid();
        let (cw, ch) = self.cell_px();
        let waker = self.waker();
        match action {
            Action::NewTab => {
                let _ = self.mux.new_tab(&self.cfg, cols, rows, cw, ch, waker);
            }
            Action::SplitRight | Action::SplitDown | Action::SplitAuto => {
                let _ = self.mux.split(&self.cfg, cols, rows, cw, ch, waker);
            }
            Action::ClosePane => {
                if self.mux.close_focused() {
                    event_loop.exit();
                }
            }
            Action::CloseWindow | Action::CloseTab => {
                let a = self.mux.active;
                if a < self.mux.tabs.len() {
                    self.mux.tabs.remove(a);
                    if self.mux.active >= self.mux.tabs.len() && self.mux.active > 0 {
                        self.mux.active -= 1;
                    }
                }
                if self.mux.tabs.is_empty() {
                    event_loop.exit();
                }
            }
            Action::NextTab => self.mux.next_tab(),
            Action::PrevTab => self.mux.prev_tab(),
            Action::FocusNext | Action::FocusRight | Action::FocusDown => {
                self.mux.focus_next_pane()
            }
            Action::FocusPrev | Action::FocusLeft | Action::FocusUp => self.mux.focus_prev_pane(),
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
                    let s = match action {
                        Action::IncreaseFontSize => r.cell_h + 1.0,
                        Action::DecreaseFontSize => (r.cell_h - 1.0).max(6.0),
                        _ => self.cfg.font_size * 1.25,
                    };
                    // cell_h tracks line height; derive font size back out.
                    let new_size = match action {
                        Action::ResetFontSize => self.cfg.font_size,
                        Action::IncreaseFontSize => (s / 1.25) + 0.0,
                        _ => s / 1.25,
                    };
                    r.set_font_size(new_size);
                }
                self.resize_all();
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
                    let s = match action {
                        Action::ScrollPageUp => Scroll::PageUp,
                        Action::ScrollPageDown => Scroll::PageDown,
                        Action::ScrollToTop => Scroll::Top,
                        _ => Scroll::Bottom,
                    };
                    t.scroll_display(s);
                }
            }
            Action::ReloadConfig => self.reload_config(),
            Action::ResizeUp | Action::ResizeDown | Action::ResizeLeft | Action::ResizeRight => {}
            Action::NewWindow => {
                let _ = self
                    .mux
                    .new_tab(&self.cfg, cols, rows, cw, ch, self.waker());
            }
            Action::MoveTabLeft | Action::MoveTabRight => {}
            Action::GotoTab(n) => {
                let i = n as usize;
                if i < self.mux.tabs.len() {
                    self.mux.active = i;
                }
            }
        }
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
            Key::Named(NamedKey::Escape) => {
                self.mux.search.open = false;
            }
            Key::Named(NamedKey::Enter) => {
                let s = &mut self.mux.search;
                if !s.matches.is_empty() {
                    if self.mods.shift_key() {
                        s.index = (s.index + s.matches.len() - 1) % s.matches.len();
                    } else {
                        s.index = (s.index + 1) % s.matches.len();
                    }
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

        let (cols, rows) = self.grid();
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

                // Keybindings first.
                if let Some(k) = to_kkey(&event.logical_key) {
                    let trig = Trigger::new(to_mods(self.mods), k);
                    if let Some(act) = self.cfg.keybinds.get(&trig).cloned() {
                        self.handle_action(act, event_loop);
                        return;
                    }
                }

                // Otherwise feed the focused pane (and broadcast if enabled).
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
