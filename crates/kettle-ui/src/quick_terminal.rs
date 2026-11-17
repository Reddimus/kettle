//! Dedicated quick-window policy, independent of the desktop and renderer.
use kettle_config::QuickTerminalPosition as Position;
use std::time::{Duration, Instant};

pub(crate) mod hotkey;
pub(crate) mod native;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod portal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// AppKit points on macOS; physical pixels on other platforms.
pub(crate) struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub(crate) fn window_position(x: i32, y: i32) -> winit::dpi::Position {
    if cfg!(target_os = "macos") {
        winit::dpi::LogicalPosition::new(f64::from(x), f64::from(y)).into()
    } else {
        winit::dpi::PhysicalPosition::new(x, y).into()
    }
}

pub(crate) fn window_size(rect: Rect) -> winit::dpi::Size {
    if cfg!(target_os = "macos") {
        winit::dpi::LogicalSize::new(f64::from(rect.width), f64::from(rect.height)).into()
    } else {
        winit::dpi::PhysicalSize::new(rect.width, rect.height).into()
    }
}

pub(crate) trait GeometryWindow {
    fn move_to(&self, position: winit::dpi::Position);
    fn resize_to(&self, size: winit::dpi::Size);
}

impl GeometryWindow for winit::window::Window {
    fn move_to(&self, position: winit::dpi::Position) {
        self.set_outer_position(position);
    }
    fn resize_to(&self, size: winit::dpi::Size) {
        let _ = self.request_inner_size(size);
    }
}

pub(crate) fn place_window(window: &impl GeometryWindow, rect: Rect, position: (i32, i32)) {
    // AppKit must move to the destination screen before changing content size.
    window.move_to(window_position(position.0, position.1));
    window.resize_to(window_size(rect));
}

pub(crate) fn geometry(screen: Rect, position: Position, percent: u8) -> Rect {
    let fraction = |length: u32| {
        ((u64::from(length) * u64::from(percent.clamp(1, 100))) / 100)
            .max(1)
            .min(u64::from(length)) as u32
    };
    let (width, height) = match position {
        Position::Top | Position::Bottom => (screen.width, fraction(screen.height)),
        Position::Left | Position::Right => (fraction(screen.width), screen.height),
        Position::Center => (fraction(screen.width), fraction(screen.height)),
    };
    let dx = match position {
        Position::Right => screen.width - width,
        Position::Center => (screen.width - width) / 2,
        _ => 0,
    };
    let dy = match position {
        Position::Bottom => screen.height - height,
        Position::Center => (screen.height - height) / 2,
        _ => 0,
    };
    Rect {
        x: screen.x.saturating_add_unsigned(dx),
        y: screen.y.saturating_add_unsigned(dy),
        width,
        height,
    }
}

pub(crate) fn session_geometry(
    screen: Rect,
    position: Position,
    percent: u8,
    wayland: bool,
) -> Rect {
    geometry(
        screen,
        if wayland { Position::Center } else { position },
        percent,
    )
}
pub(crate) fn session_maximized(wayland: bool, percent: u8) -> Option<bool> {
    wayland.then_some(percent == 100)
}

pub(crate) trait SizeWindow {
    fn is_maximized(&self) -> bool;
    fn set_maximized(&self, maximized: bool);
    fn request_size(&self, rect: Rect);
}
impl SizeWindow for winit::window::Window {
    fn is_maximized(&self) -> bool {
        self.is_maximized()
    }
    fn set_maximized(&self, maximized: bool) {
        self.set_maximized(maximized);
    }
    fn request_size(&self, rect: Rect) {
        let _ = self.request_inner_size(window_size(rect));
    }
}

#[derive(Default)]
pub(crate) struct WaylandSize {
    pending: Option<(u64, Rect)>,
}
impl WaylandSize {
    pub(crate) fn show(
        &mut self,
        seq: u64,
        window: &impl SizeWindow,
        rect: Option<Rect>,
        maximized: bool,
    ) {
        self.pending = if maximized {
            None
        } else {
            rect.map(|rect| (seq, rect))
        };
        window.set_maximized(maximized);
        if !maximized
            && !window.is_maximized()
            && let Some(rect) = rect
        {
            window.request_size(rect);
        }
    }
    pub(crate) fn configured(&mut self, seq: u64, window: &impl SizeWindow, size: (u32, u32)) {
        let Some((pending_seq, rect)) = self.pending else {
            return;
        };
        if pending_seq != seq || window.is_maximized() || size.0 == 0 || size.1 == 0 {
            return;
        }
        // Wayland ignores a resize until the unmaximized configure arrives.
        self.pending = None;
        if size != (rect.width, rect.height) {
            window.request_size(rect);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    Show,
    Hide { return_focus: bool },
    None,
}

#[derive(Default)]
pub(crate) struct Model {
    pub window: Option<u64>,
    pub visible: bool,
    can_return_focus: bool,
}

impl Model {
    pub fn for_startup(quick: bool, window: u64) -> Self {
        // Identity must precede every startup action that can save a session.
        Self {
            window: quick.then_some(window),
            ..Self::default()
        }
    }

    pub fn reveal(&mut self, can_return_focus: bool) -> Effect {
        if self.visible {
            Effect::None
        } else {
            self.visible = true;
            self.can_return_focus = can_return_focus;
            Effect::Show
        }
    }

    pub fn toggle(&mut self, can_return_focus: bool) -> Effect {
        if self.visible {
            self.visible = false;
            Effect::Hide {
                return_focus: std::mem::take(&mut self.can_return_focus),
            }
        } else {
            self.reveal(can_return_focus)
        }
    }

    pub fn focus_lost(&mut self, other_app: bool, autohide: bool) -> Effect {
        if self.visible && other_app && autohide {
            self.visible = false;
            self.can_return_focus = false;
            // The user chose another app. Hiding must not activate an older app.
            Effect::Hide {
                return_focus: false,
            }
        } else {
            Effect::None
        }
    }
}

pub(crate) struct Slide {
    pub window: u64,
    start: Instant,
    from: (i32, i32),
    to: (i32, i32),
}
impl Slide {
    pub fn new(window: u64, target: Rect, edge: Position, now: Instant) -> Self {
        let (x, y) = (target.x, target.y);
        let from = match edge {
            Position::Top => (x, y.saturating_sub_unsigned(target.height)),
            Position::Bottom => (x, y.saturating_add_unsigned(target.height)),
            Position::Left => (x.saturating_sub_unsigned(target.width), y),
            Position::Right => (x.saturating_add_unsigned(target.width), y),
            Position::Center => (x, y.saturating_sub(24)),
        };
        Self {
            window,
            start: now,
            from,
            to: (x, y),
        }
    }
    pub fn position(&self, now: Instant) -> ((i32, i32), bool) {
        let t = (now.saturating_duration_since(self.start).as_secs_f64() / 0.15).min(1.0);
        let ease = 1.0 - (1.0 - t).powi(3);
        let lerp = |a, b| (f64::from(a) + (f64::from(b) - f64::from(a)) * ease).round() as i32;
        (
            (lerp(self.from.0, self.to.0), lerp(self.from.1, self.to.1)),
            t == 1.0,
        )
    }
}

#[derive(Default)]
pub(crate) struct Runtime {
    pub model: Model,
    pub previous: native::PreviousApp,
    pub previous_window: Option<u64>,
    pub initial_focus: bool,
    pub activation_token: Option<String>,
    pub activation_until: Option<Instant>,
    pub wayland: bool,
    pub slide: Option<Slide>,
    pub size: WaylandSize,
    pub focus_check: Option<Instant>,
    pub hotkey: Option<hotkey::Registration>,
    #[cfg(target_os = "linux")]
    pub portal: Option<portal::Watch>,
    pub portal_wake_pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub attempted_hotkey: Option<Option<kettle_config::keybinds::Trigger>>,
}

pub(crate) fn animation_allowed(
    enabled: bool,
    mode: kettle_config::ReduceMotion,
    system: impl FnOnce() -> bool,
) -> bool {
    enabled && mode != kettle_config::ReduceMotion::On && system()
}

pub(crate) fn can_slide(wayland: bool, allowed: impl FnOnce() -> bool) -> bool {
    !wayland && allowed()
}

pub(crate) const FRAME: Duration = Duration::from_millis(10);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wayland_size_waits_for_unmaximized_configure() {
        use std::cell::{Cell, RefCell};
        struct Window {
            maximized: Cell<bool>,
            size: Cell<(u32, u32)>,
            requests: RefCell<Vec<(u32, u32)>>,
        }
        impl SizeWindow for Window {
            fn is_maximized(&self) -> bool {
                self.maximized.get()
            }
            fn set_maximized(&self, _: bool) {} // A request does not change the last configure.
            fn request_size(&self, rect: Rect) {
                self.requests.borrow_mut().push((rect.width, rect.height));
                if !self.maximized.get() {
                    self.size.set((rect.width, rect.height));
                }
            }
        }
        let window = Window {
            maximized: Cell::new(true),
            size: Cell::new((2000, 1000)),
            requests: Default::default(),
        };
        let mut model = WaylandSize::default();
        let rect = Rect {
            x: 0,
            y: 0,
            width: 800,
            height: 400,
        };
        model.show(7, &window, Some(rect), false);
        assert_eq!(window.size.get(), (2000, 1000));
        model.configured(7, &window, (2000, 1000)); // An older maximized configure.
        window.maximized.set(false);
        window.size.set((1000, 700)); // The compositor restores the pre-maximized size.
        model.configured(8, &window, (1000, 700)); // Another window must not consume it.
        model.configured(7, &window, (1000, 700));
        assert_eq!(window.size.get(), (800, 400));
        let count = window.requests.borrow().len();
        model.configured(7, &window, (800, 400));
        assert_eq!(window.requests.borrow().len(), count);
        window.maximized.set(true);
        model.show(7, &window, Some(rect), false);
        model.show(7, &window, Some(rect), true); // A newer maximize cancels the small size.
        window.maximized.set(false);
        model.configured(7, &window, (1000, 700));
        assert!(model.pending.is_none());
    }

    #[test]
    fn review_quick_cross_dpi_placement_moves_before_resize_without_animation() {
        use std::cell::{Cell, RefCell};
        use winit::dpi::{LogicalPosition, LogicalSize, Position, Size};
        struct Window {
            scale: Cell<f64>,
            calls: RefCell<Vec<&'static str>>,
            position: Cell<LogicalPosition<f64>>,
            size: Cell<LogicalSize<f64>>,
        }
        impl GeometryWindow for Window {
            fn move_to(&self, position: Position) {
                self.calls.borrow_mut().push("move");
                self.position.set(position.to_logical(self.scale.get()));
                self.scale.set(2.0);
            }
            fn resize_to(&self, size: Size) {
                self.calls.borrow_mut().push("resize");
                self.size.set(size.to_logical(self.scale.get()));
            }
        }
        let window = Window {
            scale: Cell::new(1.0),
            calls: RefCell::new(Vec::new()),
            position: Cell::new(LogicalPosition::new(0.0, 0.0)),
            size: Cell::new(LogicalSize::new(1.0, 1.0)),
        };
        let rect = Rect {
            x: 1440,
            y: -100,
            width: 1512,
            height: 392,
        };
        assert!(!animation_allowed(
            false,
            kettle_config::ReduceMotion::Off,
            || true
        ));
        place_window(&window, rect, (rect.x, rect.y));
        assert_eq!(*window.calls.borrow(), ["move", "resize"]);
        #[cfg(target_os = "macos")]
        {
            assert_eq!(window.position.get(), LogicalPosition::new(1440.0, -100.0));
            assert_eq!(window.size.get(), LogicalSize::new(1512.0, 392.0));
        }
        #[cfg(not(target_os = "macos"))]
        assert_eq!(window.size.get(), LogicalSize::new(756.0, 196.0));
    }
    #[test]
    fn every_edge_and_center_fit_small_large_and_offset_screens() {
        for (x, y, width, height) in [
            (0, 0, 1920, 1080),
            (-1280, 120, 1280, 720),
            (0, -2160, 3840, 2160),
            (20, 40, 101, 71),
        ] {
            let screen = Rect {
                x,
                y,
                width,
                height,
            };
            for percent in [1, 40, 75, 100] {
                let w = (width * u32::from(percent) / 100).max(1);
                let h = (height * u32::from(percent) / 100).max(1);
                for (edge, expected) in [
                    (
                        Position::Top,
                        Rect {
                            x,
                            y,
                            width,
                            height: h,
                        },
                    ),
                    (
                        Position::Bottom,
                        Rect {
                            x,
                            y: y + (height - h) as i32,
                            width,
                            height: h,
                        },
                    ),
                    (
                        Position::Left,
                        Rect {
                            x,
                            y,
                            width: w,
                            height,
                        },
                    ),
                    (
                        Position::Right,
                        Rect {
                            x: x + (width - w) as i32,
                            y,
                            width: w,
                            height,
                        },
                    ),
                    (
                        Position::Center,
                        Rect {
                            x: x + ((width - w) / 2) as i32,
                            y: y + ((height - h) / 2) as i32,
                            width: w,
                            height: h,
                        },
                    ),
                ] {
                    assert_eq!(geometry(screen, edge, percent), expected);
                }
            }
        }
    }
    #[test]
    fn toggle_keeps_one_window_and_returns_focus_only_on_explicit_hide() {
        let mut model = Model {
            window: Some(7),
            ..Default::default()
        };
        assert_eq!(model.toggle(true), Effect::Show);
        assert_eq!(model.focus_lost(false, true), Effect::None);
        assert_eq!(model.focus_lost(true, false), Effect::None);
        assert_eq!(model.toggle(false), Effect::Hide { return_focus: true });
        assert_eq!(model.window, Some(7));
        assert_eq!(model.toggle(true), Effect::Show);
        assert_eq!(
            model.focus_lost(true, true),
            Effect::Hide {
                return_focus: false
            }
        );
        assert_eq!(model.focus_lost(true, true), Effect::None);
        assert!(!model.visible);
        assert_eq!(model.toggle(false), Effect::Show);
        assert_eq!(
            model.toggle(false),
            Effect::Hide {
                return_focus: false
            }
        );
    }
    #[test]
    fn reduced_motion_always_wins_over_animation_and_general_override() {
        use kettle_config::ReduceMotion as R;
        assert!(animation_allowed(true, R::Auto, || true));
        assert!(!animation_allowed(true, R::Auto, || false));
        assert!(!animation_allowed(true, R::Off, || false));
        assert!(!animation_allowed(true, R::On, || panic!("system queried")));
        assert!(!animation_allowed(false, R::Auto, || panic!(
            "system queried"
        )));
    }

    #[test]
    fn slide_reaches_exact_target_and_stays_there() {
        let now = Instant::now();
        let rect = Rect {
            x: -100,
            y: 20,
            width: 1000,
            height: 400,
        };
        let slide = Slide::new(1, rect, Position::Top, now);
        assert_eq!(slide.position(now), ((-100, -380), false));
        assert_eq!(
            slide.position(now + Duration::from_millis(150)),
            ((-100, 20), true)
        );
        assert_eq!(
            slide.position(now + Duration::from_secs(1)),
            ((-100, 20), true)
        );
    }
}
