//! Event-loop integration for the dedicated quick terminal.
use super::*;
use crate::quick_terminal::{self as quick, Effect, Rect};

trait VisibilityWindow {
    fn is_visible(&self) -> Option<bool>;
    fn has_focus(&self) -> bool;
    fn set_visible(&self, visible: bool);
    fn set_minimized(&self, minimized: bool);
}

impl VisibilityWindow for winit::window::Window {
    fn is_visible(&self) -> Option<bool> {
        self.is_visible()
    }
    fn has_focus(&self) -> bool {
        self.has_focus()
    }
    fn set_visible(&self, visible: bool) {
        self.set_visible(visible);
    }
    fn set_minimized(&self, minimized: bool) {
        self.set_minimized(minimized);
    }
}

fn dispatch_visibility_toggle(
    model: &mut quick::Model,
    seq: u64,
    window: &impl VisibilityWindow,
    wayland: bool,
) -> Effect {
    let quick = model.window == Some(seq);
    let visible = if quick {
        model.visible
    } else {
        window.is_visible().unwrap_or(true)
    };
    if visible && window.has_focus() {
        if quick {
            // Dispatch has checked this window out of App::windows.
            hide_native_window(window, wayland);
            model.toggle(false)
        } else {
            window.set_visible(false);
            Effect::None
        }
    } else {
        Effect::Show
    }
}

fn hide_native_window(window: &impl VisibilityWindow, wayland: bool) {
    window.set_visible(false);
    if wayland {
        window.set_minimized(true);
    }
}

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
                .or_else(|| {
                    self.quick_terminal
                        .model
                        .window
                        .and_then(|seq| self.windows.get(&seq))
                        .and_then(|ws| ws.window.as_ref())
                        .and_then(|window| window.current_monitor())
                })
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
        Some(quick::session_geometry(
            screen,
            self.cfg.quick_terminal_position,
            self.cfg.quick_terminal_size,
            self.quick_terminal.wayland,
        ))
    }

    pub(super) fn reconcile_quick_hotkey(&mut self) {
        #[cfg(target_os = "linux")]
        if self.quick_terminal.wayland {
            if self.activation.is_some() {
                if self.quick_terminal.portal.is_none() {
                    let proxy = self.proxy.clone();
                    let pending = self.quick_terminal.portal_wake_pending.clone();
                    self.quick_terminal.portal = quick::portal::Watch::start(
                        self.ui_text
                            .text(kettle_i18n::Text::QuickTerminalToggle)
                            .into(),
                        Arc::new(move || {
                            if !pending.swap(true, std::sync::atomic::Ordering::AcqRel) {
                                let _ = proxy.send_event(UserEvent::GlobalShortcut);
                            }
                        }),
                    );
                }
                if let Some(portal) = &self.quick_terminal.portal {
                    portal.set_trigger(self.cfg.quick_terminal_hotkey);
                }
            }
            return;
        }
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

    pub(super) fn quick_portal_status(&self) -> Option<quick::portal::Status> {
        if !self.quick_terminal.wayland {
            return None;
        }
        if self.cfg.quick_terminal_hotkey.is_none() {
            return Some(quick::portal::Status::Disabled);
        }
        #[cfg(target_os = "linux")]
        if let Some(portal) = &self.quick_terminal.portal {
            return Some(portal.status());
        }
        Some(if self.activation.is_some() {
            quick::portal::Status::Unavailable
        } else {
            quick::portal::Status::OtherOwner
        })
    }

    pub(super) fn drain_global_shortcuts(&mut self, el: &ActiveEventLoop) {
        self.quick_terminal
            .portal_wake_pending
            .store(false, std::sync::atomic::Ordering::Release);
        if self.gpu.is_none() {
            return;
        }
        #[cfg(target_os = "linux")]
        if let Some(portal) = &self.quick_terminal.portal {
            let activations = portal.drain();
            for token in activations {
                self.quick_terminal.activation_token = token;
                self.toggle_quick_terminal(el);
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = el;
        for ws in self.windows.values_mut() {
            ws.accessibility_pending = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
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
        match dispatch_visibility_toggle(
            &mut self.quick_terminal.model,
            ws.seq,
            window.as_ref(),
            self.quick_terminal.wayland,
        ) {
            Effect::Hide { return_focus } => self.hide_quick_terminal(return_focus),
            Effect::Show => self.reveal_window(ws),
            Effect::None => {}
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
                let animate = quick::can_slide(self.quick_terminal.wayland, || {
                    self.quick_system_motion_allowed()
                });
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
                        if let Some(maximized) = quick::session_maximized(
                            self.quick_terminal.wayland,
                            self.cfg.quick_terminal_size,
                        ) {
                            self.quick_terminal
                                .size
                                .show(seq, window.as_ref(), rect, maximized);
                        } else if let Some(rect) = rect {
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
                    .get_or_init(|| crate::motion::SystemMotion::new(self.system_settings_waker()))
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
            hide_native_window(window.as_ref(), self.quick_terminal.wayland);
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
            let previous = self.quick_terminal.previous_window.and_then(|seq| {
                self.windows
                    .get(&seq)
                    .and_then(|ws| ws.window.clone())
                    .map(|window| (seq, window))
            });
            if quick::portal::activate_target(
                bridge,
                self.quick_terminal.activation_token.as_deref(),
                previous,
                target_seq,
                target,
            ) {
                self.quick_terminal.activation_token = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordedWindow(RefCell<Vec<(&'static str, bool)>>);
    impl VisibilityWindow for RecordedWindow {
        fn is_visible(&self) -> Option<bool> {
            // Wayland cannot report visibility reliably.
            None
        }
        fn has_focus(&self) -> bool {
            true
        }
        fn set_visible(&self, visible: bool) {
            self.0.borrow_mut().push(("visible", visible));
        }
        fn set_minimized(&self, minimized: bool) {
            self.0.borrow_mut().push(("minimized", minimized));
        }
    }

    #[test]
    fn review_s4_dispatch_hides_checked_out_quick_window() {
        for wayland in [false, true] {
            let mut windows = std::collections::BTreeMap::from([(1, RecordedWindow::default())]);
            let mut model = quick::Model::for_startup(true, 1);
            model.reveal(true);
            // Keyboard and remote dispatch remove their target before delivering
            // ToggleVisibility. The native operation must use that target.
            let window = windows.remove(&1).unwrap();
            assert!(windows.is_empty());
            assert_eq!(
                dispatch_visibility_toggle(&mut model, 1, &window, wayland),
                Effect::Hide { return_focus: true }
            );
            assert!(!model.visible);
            let mut expected = vec![("visible", false)];
            if wayland {
                expected.push(("minimized", true));
            }
            assert_eq!(*window.0.borrow(), expected);
            windows.insert(1, window);
        }
    }
}
