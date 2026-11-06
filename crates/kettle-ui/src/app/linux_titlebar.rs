//! Linux client decorations, shared control geometry and compositor gestures.
use super::*;

pub(super) const LINUX_HEADER_HEIGHT: f32 = 46.0;
const LINUX_RESIZE_GRIP: f32 = 6.0;

#[cfg(any(test, target_os = "linux"))]
pub(super) fn linux_titlebar_policy(cfg: &Config) -> bool {
    cfg.linux_titlebar_style == kettle_config::LinuxTitlebarStyle::Tabs
        && !cfg.borderless
        && cfg.tab_bar_pos == TabBarPos::Top
        && cfg.tab_bar != TabBarMode::Off
}

/// Desktops whose compositor offers no server-side decorations (Mutter and
/// its relatives), so a Wayland client draws its own frame there. Elsewhere
/// (KWin, wlroots compositors) winit negotiates the compositor's frame.
#[cfg(any(test, target_os = "linux"))]
pub(super) fn desktop_lacks_server_decorations(desktop: Option<&str>) -> bool {
    desktop.is_some_and(|desktop| {
        desktop.split(':').any(|part| {
            matches!(
                part.trim().to_ascii_lowercase().as_str(),
                "gnome" | "gnome-classic" | "gnome-flashback" | "pantheon" | "unity" | "budgie"
            )
        })
    })
}

/// Whether Kettle draws the Linux header bar: the policy allows it, the
/// window is on Wayland, and the desktop has no server-side decorations.
#[cfg(target_os = "linux")]
pub(super) fn linux_custom_titlebar(cfg: &Config, window: &Window) -> bool {
    use winit::raw_window_handle::{HasDisplayHandle as _, RawDisplayHandle};
    let wayland = window
        .display_handle()
        .is_ok_and(|handle| matches!(handle.as_raw(), RawDisplayHandle::Wayland(_)));
    wayland
        && linux_titlebar_policy(cfg)
        && desktop_lacks_server_decorations(std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LinuxTitlebarGesture {
    Drag,
    ToggleMaximize,
    WindowMenu,
    Ignore,
}

pub(super) fn linux_titlebar_gesture(button: u8, double: bool) -> LinuxTitlebarGesture {
    match (button, double) {
        (0, true) => LinuxTitlebarGesture::ToggleMaximize,
        (0, false) => LinuxTitlebarGesture::Drag,
        (2, _) => LinuxTitlebarGesture::WindowMenu,
        _ => LinuxTitlebarGesture::Ignore,
    }
}

pub(super) fn linux_resize_edge(
    size: (u32, u32),
    scale: f32,
    constrained: bool,
    x: f32,
    y: f32,
) -> Option<winit::window::ResizeDirection> {
    use winit::window::ResizeDirection as R;
    let (w, h) = (size.0 as f32, size.1 as f32);
    if constrained || x < 0.0 || y < 0.0 || x >= w || y >= h {
        return None;
    }
    let grip = LINUX_RESIZE_GRIP * scale;
    match (x < grip, x >= w - grip, y < grip, y >= h - grip) {
        (true, _, true, _) => Some(R::NorthWest),
        (_, true, true, _) => Some(R::NorthEast),
        (true, _, _, true) => Some(R::SouthWest),
        (_, true, _, true) => Some(R::SouthEast),
        (true, _, _, _) => Some(R::West),
        (_, true, _, _) => Some(R::East),
        (_, _, true, _) => Some(R::North),
        (_, _, _, true) => Some(R::South),
        _ => None,
    }
}

pub(super) fn linux_resize_cursor(edge: winit::window::ResizeDirection) -> CursorIcon {
    use winit::window::ResizeDirection as R;
    match edge {
        R::North => CursorIcon::NResize,
        R::South => CursorIcon::SResize,
        R::East => CursorIcon::EResize,
        R::West => CursorIcon::WResize,
        R::NorthEast => CursorIcon::NeResize,
        R::NorthWest => CursorIcon::NwResize,
        R::SouthEast => CursorIcon::SeResize,
        R::SouthWest => CursorIcon::SwResize,
    }
}

pub(super) fn window_control_at(
    bar: &TabBar,
    x: f32,
    y: f32,
) -> Option<kettle_render::WindowControl> {
    bar.window_controls
        .iter()
        .find(|b| rect_contains(b.rect, x, y))
        .map(|b| b.action)
}

pub(super) fn window_control_text(action: kettle_render::WindowControl) -> kettle_i18n::Text {
    use kettle_i18n::Text as T;
    use kettle_render::WindowControl as C;
    match action {
        C::Minimize => T::A11yWindowMinimize,
        C::Maximize => T::A11yWindowMaximize,
        C::Restore => T::A11yWindowRestore,
        C::Close => T::A11yWindowClose,
    }
}

impl App {
    pub(super) fn linux_button_layout(
        &self,
        ws: &WindowState,
    ) -> crate::window_buttons::ButtonLayout {
        if ws.linux_titlebar {
            self.system_motion
                .get_or_init(|| crate::motion::SystemMotion::new(self.system_settings_waker()))
                .button_layout()
        } else {
            crate::window_buttons::ButtonLayout {
                left: Vec::new(),
                right: Vec::new(),
            }
        }
    }

    pub(super) fn linux_window_controls(
        &self,
        ws: &WindowState,
        band: Rect,
        preferences: &crate::window_buttons::ButtonLayout,
    ) -> Vec<kettle_render::WindowControlButton> {
        if !ws.linux_titlebar {
            return Vec::new();
        }
        linux_window_control_layout_with_preferences(
            band,
            ws.window.as_ref().is_some_and(|w| w.is_maximized()),
            ws.native_pointer.map(|p| (p.x as f32, p.y as f32)),
            ws.pressed_window_control,
            &self.ui_text,
            preferences,
        )
    }

    pub(super) fn linux_resize_at(
        &self,
        ws: &WindowState,
        x: f32,
        y: f32,
    ) -> Option<winit::window::ResizeDirection> {
        if !ws.linux_titlebar {
            return None;
        }
        let window = ws.window.as_ref()?;
        let size = window.inner_size();
        linux_resize_edge(
            (size.width, size.height),
            window.scale_factor() as f32,
            window.is_maximized() || window.fullscreen().is_some() || !window.is_resizable(),
            x,
            y,
        )
    }

    pub(super) fn activate_window_control(
        &mut self,
        ws: &mut WindowState,
        action: kettle_render::WindowControl,
        event_loop: &ActiveEventLoop,
    ) {
        use kettle_render::WindowControl as C;
        if action == C::Close {
            // Match the native close button's opt-in confirmation policy.
            self.window_event_inner(ws, event_loop, WindowEvent::CloseRequested);
        } else if let Some(window) = &ws.window {
            match action {
                C::Minimize => window.set_minimized(true),
                C::Maximize => window.set_maximized(true),
                C::Restore => window.set_maximized(false),
                C::Close => (),
            }
        }
    }

    pub(super) fn press_linux_chrome(
        &self,
        ws: &mut WindowState,
        bar: &TabBar,
        x: f32,
        y: f32,
        button: u8,
    ) -> bool {
        if !ws.linux_titlebar {
            return false;
        }
        if let Some(edge) = self.linux_resize_at(ws, x, y) {
            if button == 0
                && let Some(window) = &ws.window
            {
                let _ = window.drag_resize_window(edge);
            }
            ws.titlebar_click = None;
            return true;
        }
        if let Some(action) = window_control_at(bar, x, y) {
            ws.pressed_window_control = (button == 0).then_some(action);
            ws.titlebar_click = None;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
            return true;
        }
        if !empty_titlebar_space(bar, x, y) {
            return false;
        }
        let Some(window) = &ws.window else {
            return true;
        };
        let now = std::time::Instant::now();
        let tolerance = 4.0 * window.scale_factor() as f32;
        let double = ws.titlebar_click.is_some_and(|(t, px, py)| {
            now.duration_since(t).as_millis() <= 400
                && (x - px).abs() <= tolerance
                && (y - py).abs() <= tolerance
        });
        ws.titlebar_click = if button == 0 && !double {
            Some((now, x, y))
        } else {
            None
        };
        match linux_titlebar_gesture(button, double) {
            LinuxTitlebarGesture::Drag => {
                let _ = window.drag_window();
            }
            LinuxTitlebarGesture::ToggleMaximize => window.set_maximized(!window.is_maximized()),
            LinuxTitlebarGesture::WindowMenu => {
                window.show_window_menu(winit::dpi::PhysicalPosition::new(x as i32, y as i32))
            }
            LinuxTitlebarGesture::Ignore => (),
        }
        true
    }
}

pub(super) fn window_control_tooltip_label(
    controls: &[kettle_render::WindowControlButton],
) -> Option<&str> {
    controls
        .iter()
        .find(|b| b.hovered && !b.pressed)
        .map(|b| b.label.as_str())
}

pub(super) fn linux_window_control_layout_with_preferences(
    band: Rect,
    maximized: bool,
    pointer: Option<(f32, f32)>,
    pressed: Option<kettle_render::WindowControl>,
    tr: &kettle_i18n::Translator,
    preferences: &crate::window_buttons::ButtonLayout,
) -> Vec<kettle_render::WindowControlButton> {
    use kettle_render::{WindowControl as C, WindowControlButton};
    let h = band.3;
    let scale = h / LINUX_HEADER_HEIGHT;
    let count = preferences.left.len() + preferences.right.len();
    let slot = h.min(band.2 / count.max(1) as f32);
    preferences
        .left
        .iter()
        .enumerate()
        .map(|(i, c)| (band.0 + i as f32 * slot, *c))
        .chain(preferences.right.iter().enumerate().map(|(i, c)| {
            (
                band.0 + band.2 - (preferences.right.len() - i) as f32 * slot,
                *c,
            )
        }))
        .map(|(left, action)| {
            let action = if maximized && action == C::Maximize {
                C::Restore
            } else {
                action
            };
            let size = (28.0 * scale).min(slot).max(0.0);
            let rect = (
                left + (slot - size) * 0.5,
                band.1 + (h - size) * 0.5,
                size,
                size.min(h),
            );
            let hovered = pointer.is_some_and(|(x, y)| rect_contains(rect, x, y));
            WindowControlButton {
                action,
                rect,
                label: tr.text(window_control_text(action)).to_owned(),
                hovered,
                pressed: hovered && pressed == Some(action),
            }
        })
        .collect()
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn linux_startup_surface_with_header(
    cfg: &Config,
    surface: (u32, u32),
    previous_height: f32,
    scale: f32,
    monitor: Option<StartupMonitor>,
) -> (u32, u32) {
    let height = (surface.1 as f32 + LINUX_HEADER_HEIGHT * scale - previous_height)
        .round()
        .max(1.0);
    let height = if cfg.window_width.is_none() && cfg.window_height.is_none() {
        monitor.map_or(height, |m| {
            height.min((m.logical.1 * STARTUP_MONITOR_HEIGHT_FRACTION * m.scale).floor() as f32)
        })
    } else {
        height
    };
    (surface.0, height.max(1.0) as u32)
}

#[cfg(test)]
pub(super) fn linux_window_control_layout(
    band: Rect,
    maximized: bool,
    pointer: Option<(f32, f32)>,
    pressed: Option<kettle_render::WindowControl>,
    tr: &kettle_i18n::Translator,
) -> Vec<kettle_render::WindowControlButton> {
    linux_window_control_layout_with_preferences(
        band,
        maximized,
        pointer,
        pressed,
        tr,
        &Default::default(),
    )
}

impl App {
    pub(super) fn system_settings_waker(&self) -> kettle_core::Waker {
        let proxy = self.proxy.clone();
        let pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
        Arc::new(move || {
            if !pending.swap(true, std::sync::atomic::Ordering::AcqRel) {
                let _ = proxy.send_event(UserEvent::SystemSettingsChanged(pending.clone()));
            }
        })
    }
}

pub(super) fn linux_header_tab_controls(
    band: Rect,
    scale: f32,
    dropdown: bool,
) -> (Rect, Rect, f32) {
    let height = (28.0 * scale).min(band.3);
    let lane = (band.0, band.1 + (band.3 - height) * 0.5, band.2, height);
    let (menu, plus, reserve) = horizontal_new_tab_controls(lane, scale, dropdown);
    let clip = |r: Rect| {
        let left = r.0.clamp(band.0, band.0 + band.2);
        let right = (r.0 + r.2).clamp(left, band.0 + band.2);
        (left, r.1, right - left, r.3)
    };
    (clip(menu), clip(plus), reserve.min(band.2))
}
