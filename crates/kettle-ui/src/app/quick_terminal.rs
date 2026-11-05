//! Event-loop integration for the dedicated quick terminal.
use super::*;
use crate::quick_terminal::{self as quick, Effect, Rect};

impl App {
    pub(super) fn quick_window_attributes(
        &self,
        attrs: winit::window::WindowAttributes,
        el: &ActiveEventLoop,
    ) -> winit::window::WindowAttributes {
        let mut attrs = attrs
            .with_decorations(false)
            .with_maximized(false)
            .with_fullscreen(None)
            .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
            .with_visible(false);
        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::WindowAttributesExtWindows as _;
            attrs = attrs.with_skip_taskbar(true);
        }
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS as _;
            attrs = attrs
                .with_titlebar_hidden(true)
                .with_titlebar_buttons_hidden(true)
                .with_fullsize_content_view(true);
        }
        if let Some(rect) = self.quick_geometry(el) {
            attrs = attrs
                .with_position(quick::window_position(rect.x, rect.y))
                .with_inner_size(quick::window_size(rect));
        }
        attrs
    }

    fn quick_geometry(&self, el: &ActiveEventLoop) -> Option<Rect> {
        let screen = quick::native::active_screen().or_else(|| {
            let pointer = (!self.quick_terminal.wayland)
                .then(windowing::cursor_screen_pos)
                .flatten();
            let monitors: Vec<_> = el.available_monitors().collect();
            let monitor = pointer
                .and_then(|(x, y)| {
                    monitors.iter().find(|m| {
                        let p = m.position();
                        let s = m.size();
                        x >= f64::from(p.x)
                            && y >= f64::from(p.y)
                            && x < f64::from(p.x) + f64::from(s.width)
                            && y < f64::from(p.y) + f64::from(s.height)
                    })
                })
                .cloned()
                .or_else(|| el.primary_monitor())
                .or_else(|| monitors.into_iter().next())?;
            let p = monitor.position();
            let s = monitor.size();
            #[cfg(target_os = "macos")]
            let (p, s) = (
                p.to_logical::<i32>(monitor.scale_factor()),
                s.to_logical::<u32>(monitor.scale_factor()),
            );
            Some(Rect {
                x: p.x,
                y: p.y,
                width: s.width,
                height: s.height,
            })
        })?;
        Some(quick::geometry(
            screen,
            self.cfg.quick_terminal_position,
            self.cfg.quick_terminal_size,
        ))
    }

    pub(super) fn reconcile_quick_hotkey(&mut self) {
        let trigger = self.cfg.quick_terminal_hotkey;
        if self.quick_terminal.attempted_hotkey == Some(trigger) {
            return;
        }
        self.quick_terminal.hotkey = None;
        self.quick_terminal.attempted_hotkey = Some(trigger);
        let Some(trigger) = trigger else { return };
        let wayland = self.quick_terminal.wayland;
        match quick::hotkey::register(trigger, self.proxy.clone(), wayland) {
            Ok(registration) => self.quick_terminal.hotkey = registration,
            Err(error) => {
                log::warn!("quick terminal hotkey registration failed: {error}");
                fire_notify(
                    self.ui_text
                        .text(kettle_i18n::Text::SettingsCategoryQuickTerminal),
                    self.ui_text
                        .text(kettle_i18n::Text::QuickTerminalHotkeyFailed),
                );
            }
        }
    }

    fn capture_quick_focus(&mut self) {
        if !std::mem::take(&mut self.quick_terminal.initial_focus) {
            self.quick_terminal.previous = if self.quick_terminal.wayland {
                Default::default()
            } else {
                quick::native::PreviousApp::capture()
            };
        }
        self.quick_terminal.previous_window = self
            .windows
            .values()
            .find(|ws| {
                Some(ws.seq) != self.quick_terminal.model.window
                    && ws.window.as_ref().is_some_and(|w| w.has_focus())
            })
            .map(|ws| ws.seq);
    }

    pub(super) fn reveal_window(&mut self, ws: &mut WindowState) {
        if self.quick_terminal.model.window == Some(ws.seq) {
            if !self.quick_terminal.model.visible {
                self.capture_quick_focus();
            }
            let can_return = self.quick_terminal.previous.available()
                || self.quick_terminal.previous_window.is_some();
            self.quick_terminal.model.reveal(can_return);
            self.quick_terminal.focus_check = None;
        }
        if let Some(window) = &ws.window {
            window.set_visible(true);
            window.set_minimized(false);
            window.focus_window();
            window.request_redraw();
            ws.window_shown = true;
        }
    }

    pub(super) fn toggle_window_visibility(&mut self, ws: &mut WindowState) {
        let Some(window) = &ws.window else { return };
        let visible = if self.quick_terminal.model.window == Some(ws.seq) {
            self.quick_terminal.model.visible
        } else {
            window.is_visible().unwrap_or(true)
        };
        if visible && window.has_focus() {
            window.set_visible(false);
            if self.quick_terminal.model.window == Some(ws.seq)
                && let Effect::Hide { return_focus } = self.quick_terminal.model.toggle(false)
            {
                self.hide_quick_terminal(return_focus);
            }
        } else {
            self.reveal_window(ws);
        }
    }

    pub(super) fn toggle_quick_terminal(&mut self, el: &ActiveEventLoop) -> bool {
        if self.gpu.is_none() {
            return false;
        }
        if !self.quick_terminal.model.visible {
            self.capture_quick_focus();
        }
        let seq = match self.quick_terminal.model.window {
            Some(seq) if self.windows.contains_key(&seq) => seq,
            _ => {
                let Ok(seq) = self.open_window(el, WindowOpen::Quick, None, None) else {
                    return false;
                };
                self.quick_terminal.model.window = Some(seq);
                seq
            }
        };
        let can_return = self.quick_terminal.previous.available()
            || self.quick_terminal.previous_window.is_some();
        match self.quick_terminal.model.toggle(can_return) {
            Effect::Show => {
                let rect = self.quick_geometry(el);
                let animate = self.quick_system_motion_allowed();
                self.quick_terminal.slide = rect.filter(|_| animate).map(|rect| {
                    quick::Slide::new(
                        seq,
                        rect,
                        self.cfg.quick_terminal_position,
                        std::time::Instant::now(),
                    )
                });
                self.quick_terminal.focus_check = None;
                if let Some(mut ws) = self.windows.remove(&seq) {
                    if let Some(window) = ws.window.as_ref() {
                        if let Some(rect) = rect {
                            let (x, y) = self
                                .quick_terminal
                                .slide
                                .as_ref()
                                .map_or((rect.x, rect.y), |slide| {
                                    slide.position(std::time::Instant::now()).0
                                });
                            quick::place_window(window.as_ref(), rect, (x, y));
                        }
                        window.set_minimized(false);
                        window.set_visible(true);
                        window.focus_window();
                        ws.window_shown = true;
                        window.request_redraw();
                    }
                    self.focused_seq = seq;
                    self.start_window_frames(&mut ws);
                    self.finish_window_dispatch(el, seq, ws);
                    if self.quick_terminal.wayland {
                        self.quick_terminal.activation_until =
                            Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
                    }
                    self.activate_quick_wayland();
                }
            }
            Effect::Hide { return_focus } => self.hide_quick_terminal(return_focus),
            Effect::None => {}
        }
        true
    }

    // The system's reduced-motion preference always wins for this animation,
    // even when the general reduce-motion config is explicitly off.
    fn quick_system_motion_allowed(&self) -> bool {
        quick::animation_allowed(
            self.cfg.quick_terminal_animation,
            self.cfg.reduce_motion,
            || {
                self.system_motion
                    .get_or_init(|| crate::motion::SystemMotion::new(self.waker()))
                    .allows()
            },
        )
    }

    fn hide_quick_terminal(&mut self, return_focus: bool) {
        self.quick_terminal.slide = None;
        self.quick_terminal.focus_check = None;
        self.quick_terminal.activation_token = None;
        self.quick_terminal.activation_until = None;
        if let Some(ws) = self
            .quick_terminal
            .model
            .window
            .and_then(|seq| self.windows.get(&seq))
            && let Some(window) = &ws.window
        {
            window.set_visible(false);
            if self.quick_terminal.wayland {
                window.set_minimized(true);
            }
        }
        if return_focus {
            if let Some(window) = self
                .quick_terminal
                .previous_window
                .take()
                .and_then(|seq| self.windows.get(&seq))
                .and_then(|ws| ws.window.as_ref())
            {
                window.set_visible(true);
                window.set_minimized(false);
                window.focus_window();
            } else {
                self.quick_terminal.previous.restore();
            }
        }
    }

    fn activate_quick_wayland(&mut self) {
        if !self.quick_terminal.model.visible
            || !self.quick_terminal.wayland
            || self
                .quick_terminal
                .activation_until
                .is_some_and(|until| until <= std::time::Instant::now())
        {
            self.quick_terminal.activation_token = None;
            self.quick_terminal.activation_until = None;
        }
        #[cfg(target_os = "linux")]
        if self.quick_terminal.activation_until.is_some()
            && let Some(target_seq) = self.quick_terminal.model.window
            && let Some(target) = self
                .windows
                .get(&target_seq)
                .and_then(|ws| ws.window.clone())
            && let Some(bridge) = self.wayland.as_mut()
        {
            if let Some(token) = self.quick_terminal.activation_token.as_deref() {
                if bridge.activate_external(token, &target) {
                    self.quick_terminal.activation_token = None;
                    self.quick_terminal.activation_until = None;
                }
            } else if let Some(source_seq) = self.quick_terminal.previous_window
                && let Some(source) = self
                    .windows
                    .get(&source_seq)
                    .and_then(|ws| ws.window.clone())
                && bridge.activate(source_seq, source, target_seq, target)
            {
                self.quick_terminal.activation_until = None;
            }
        }
    }

    pub(super) fn quick_terminal_tick(
        &mut self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        if self
            .quick_terminal
            .focus_check
            .is_some_and(|deadline| deadline <= now)
        {
            self.quick_terminal.focus_check = None;
            let other_app = quick::native::other_app_active()
                && !self
                    .windows
                    .values()
                    .any(|ws| ws.window.as_ref().is_some_and(|w| w.has_focus()));
            if let Effect::Hide { return_focus } = self
                .quick_terminal
                .model
                .focus_lost(other_app, self.cfg.quick_terminal_autohide)
            {
                self.hide_quick_terminal(return_focus);
            }
        }
        let mut next = self.quick_terminal.focus_check;
        if self.quick_terminal.activation_until.is_some() {
            self.activate_quick_wayland();
            if self.quick_terminal.activation_until.is_some() {
                next = Some(next.map_or(now + quick::FRAME, |n| n.min(now + quick::FRAME)));
            }
        }
        if let Some(slide) = &self.quick_terminal.slide {
            let ((x, y), done) = slide.position(now);
            if let Some(window) = self
                .windows
                .get(&slide.window)
                .and_then(|ws| ws.window.as_ref())
            {
                window.set_outer_position(quick::window_position(x, y));
            }
            if done {
                self.quick_terminal.slide = None;
            } else {
                next = Some(next.map_or(now + quick::FRAME, |n| n.min(now + quick::FRAME)));
            }
        }
        next
    }
}
