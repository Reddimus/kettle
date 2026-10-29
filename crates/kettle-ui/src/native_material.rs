//! Native window material behind Kettle's transparent render surface.

use std::sync::Arc;
use winit::window::Window;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackdropStatus {
    Disabled,
    Active,
    Unavailable,
}

#[cfg(any(target_os = "windows", test))]
fn blend_channel(source: u8, target: u8, source_percent: u16) -> u8 {
    let source_percent = source_percent.min(100);
    let target_percent = 100 - source_percent;
    ((u16::from(source) * source_percent + u16::from(target) * target_percent + 50) / 100) as u8
}

#[cfg(any(target_os = "windows", test))]
fn caption_colors(theme: &kettle_config::Theme) -> (kettle_config::Rgb, kettle_config::Rgb) {
    let surface = kettle_config::Rgb::new(
        blend_channel(theme.foreground.r, theme.background.r, 6),
        blend_channel(theme.foreground.g, theme.background.g, 6),
        blend_channel(theme.foreground.b, theme.background.b, 6),
    );
    let foreground = if contrast_ratio(theme.foreground, surface) >= 4.5 {
        theme.foreground
    } else {
        let black = kettle_config::Rgb::new(0, 0, 0);
        let white = kettle_config::Rgb::new(255, 255, 255);
        if contrast_ratio(white, surface) >= contrast_ratio(black, surface) {
            white
        } else {
            black
        }
    };
    (surface, foreground)
}

#[cfg(any(target_os = "windows", test))]
fn contrast_ratio(a: kettle_config::Rgb, b: kettle_config::Rgb) -> f64 {
    fn luminance(color: kettle_config::Rgb) -> f64 {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
    }
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MacosMaterialPolicy {
    effect_visible: bool,
    window_opaque: bool,
}

#[cfg(any(target_os = "macos", test))]
fn macos_material_policy(
    window_blur: bool,
    alpha_surface_required: bool,
    reduce_transparency: bool,
) -> MacosMaterialPolicy {
    let translucent = !reduce_transparency && alpha_surface_required;
    let effect_visible = window_blur && translucent;
    MacosMaterialPolicy {
        effect_visible,
        window_opaque: !translucent,
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_backdrop_status(
    window_blur: bool,
    policy: MacosMaterialPolicy,
    blur_allowed: bool,
) -> BackdropStatus {
    if window_blur && policy.effect_visible && blur_allowed {
        BackdropStatus::Active
    } else {
        // Reduce Transparency deliberately replaces material with an opaque
        // surface, while borderless windows deliberately retain sharp alpha.
        // Neither is a compositor failure and neither needs Linux's fallback.
        BackdropStatus::Disabled
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_native_blur_allowed(borderless: bool) -> bool {
    // Preserve the documented sharp-alpha fallback for borderless windows.
    !borderless
}

fn live_opacity_floor_for_status(status: BackdropStatus, linux_fallback: bool) -> Option<f32> {
    (linux_fallback && status == BackdropStatus::Unavailable).then_some(0.99)
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Default)]
struct BlurDiscovery(Arc<std::sync::Mutex<(u64, u8)>>);

#[cfg(any(target_os = "linux", test))]
impl BlurDiscovery {
    fn supported(&self) -> bool {
        self.0.lock().is_ok_and(|state| state.1 == 3)
    }

    fn reset(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.0 = state.0.wrapping_add(1);
            state.1 = 0;
        }
    }

    fn request(
        &self,
        enabled: bool,
        source: impl FnOnce() -> bool + Send + 'static,
        timeout: std::time::Duration,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) {
        if !enabled {
            return;
        }
        let epoch = {
            let Ok(mut state) = self.0.lock() else { return };
            if state.1 != 0 {
                return;
            }
            state.1 = 1;
            state.0
        };
        let state = self.0.clone();
        let started = std::thread::Builder::new()
            .name("kettle-blur-deadline".into())
            .spawn(move || {
                let supported = bounded_blur_probe(source, timeout);
                if let Ok(mut state) = state.lock()
                    && state.0 == epoch
                {
                    state.1 = if supported { 3 } else { 2 };
                }
                wake();
            });
        if started.is_err()
            && let Ok(mut state) = self.0.lock()
            && state.0 == epoch
        {
            state.1 = 2;
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn bounded_blur_probe(
    source: impl FnOnce() -> bool + Send + 'static,
    timeout: std::time::Duration,
) -> bool {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return false;
    }
    struct ProbeSlot;
    impl Drop for ProbeSlot {
        fn drop(&mut self) {
            RUNNING.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    let slot = ProbeSlot;
    let (answer, result) = std::sync::mpsc::sync_channel(1);
    let started = std::thread::Builder::new()
        .name("kettle-blur-registry".into())
        .spawn(move || {
            let _slot = slot;
            let _ = answer.send(source());
        });
    started.is_ok() && result.recv_timeout(timeout).unwrap_or(false)
}

pub(crate) struct NativeMaterial {
    last_blur: std::cell::Cell<Option<bool>>,
    status: std::cell::Cell<BackdropStatus>,
    #[cfg(not(target_os = "macos"))]
    reported_unavailable: std::cell::Cell<bool>,
    #[cfg(target_os = "macos")]
    native_blur_allowed: bool,
    #[cfg(target_os = "macos")]
    last_reduce_transparency: std::cell::Cell<Option<bool>>,
    #[cfg(target_os = "macos")]
    last_background: std::cell::Cell<Option<kettle_config::Rgb>>,
    #[cfg(target_os = "macos")]
    alpha_surface_required: bool,
    #[cfg(target_os = "macos")]
    checked_at: std::cell::Cell<std::time::Instant>,
    #[cfg(target_os = "macos")]
    accessibility_display_dirty: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(target_os = "macos")]
    notification_center: Option<objc2::rc::Retained<objc2_foundation::NSNotificationCenter>>,
    #[cfg(target_os = "macos")]
    notification_observer: Option<objc2::rc::Retained<objc2_foundation::NSObject>>,
    #[cfg(target_os = "windows")]
    last_caption: std::cell::Cell<Option<(kettle_config::Rgb, kettle_config::Rgb)>>,
    #[cfg(target_os = "linux")]
    linux_wayland: bool,
    #[cfg(target_os = "linux")]
    blur_discovery: BlurDiscovery,
    #[cfg(target_os = "linux")]
    blur_wake: Arc<dyn Fn() + Send + Sync>,
}

impl NativeMaterial {
    pub(crate) fn install(
        window: &Arc<Window>,
        config: &kettle_config::Config,
        proxy: winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
    ) -> Self {
        #[cfg(not(target_os = "linux"))]
        let _ = proxy;
        #[cfg(target_os = "macos")]
        {
            // The window and wgpu surface choose their transparency at creation.
            // Keep the native titlebar on that same decision until a new window
            // is opened, even if a live config reload changes opacity later.
            let alpha_surface_required = kettle_render::window_requires_alpha_surface(config);
            let (notification_center, notification_observer, accessibility_display_dirty) =
                observe_macos_accessibility_display(window);
            let material = Self {
                last_blur: std::cell::Cell::new(None),
                status: std::cell::Cell::new(BackdropStatus::Disabled),
                native_blur_allowed: macos_native_blur_allowed(config.borderless),
                last_reduce_transparency: std::cell::Cell::new(None),
                last_background: std::cell::Cell::new(None),
                alpha_surface_required,
                checked_at: std::cell::Cell::new(
                    std::time::Instant::now() - std::time::Duration::from_secs(1),
                ),
                accessibility_display_dirty,
                notification_center,
                notification_observer,
            };
            material.sync(window, config);
            material
        }
        #[cfg(not(target_os = "macos"))]
        {
            let material = Self {
                last_blur: std::cell::Cell::new(None),
                status: std::cell::Cell::new(BackdropStatus::Disabled),
                reported_unavailable: std::cell::Cell::new(false),
                #[cfg(target_os = "windows")]
                last_caption: std::cell::Cell::new(None),
                #[cfg(target_os = "linux")]
                linux_wayland: linux_is_wayland(window),
                #[cfg(target_os = "linux")]
                blur_discovery: {
                    static DISCOVERY: std::sync::OnceLock<BlurDiscovery> =
                        std::sync::OnceLock::new();
                    DISCOVERY.get_or_init(BlurDiscovery::default).clone()
                },
                #[cfg(target_os = "linux")]
                blur_wake: Arc::new(move || {
                    let _ = proxy.send_event(crate::app::UserEvent::BlurDiscovered);
                }),
            };
            material.sync(window, config);
            material
        }
    }

    pub(crate) fn sync(&self, window: &Window, config: &kettle_config::Config) {
        #[cfg(target_os = "macos")]
        self.sync_macos(window, config);

        #[cfg(target_os = "windows")]
        {
            let caption = caption_colors(&config.theme);
            if self.last_blur.get() != Some(config.window_blur)
                || self.last_caption.get() != Some(caption)
            {
                self.last_blur.set(Some(config.window_blur));
                self.last_caption.set(Some(caption));
                let active = sync_windows_backdrop(window, config.window_blur, caption);
                self.set_status(
                    config.window_blur,
                    if active {
                        BackdropStatus::Active
                    } else {
                        BackdropStatus::Unavailable
                    },
                );
            }
        }

        #[cfg(target_os = "linux")]
        {
            let changed =
                self.last_blur.replace(Some(config.window_blur)) != Some(config.window_blur);
            if changed && !config.window_blur {
                self.blur_discovery.reset();
            }
            self.blur_discovery.request(
                config.window_blur && self.linux_wayland,
                probe_wayland_kwin_blur,
                std::time::Duration::from_millis(500),
                self.blur_wake.clone(),
            );
            let active = config.window_blur
                && linux_blur_policy(self.linux_wayland, self.blur_discovery.supported());
            if changed || (self.status.get() == BackdropStatus::Active) != active {
                window.set_blur(active);
                self.set_status(
                    config.window_blur,
                    if active {
                        BackdropStatus::Active
                    } else {
                        BackdropStatus::Unavailable
                    },
                );
            }
        }

        #[cfg(any(target_os = "freebsd", target_os = "openbsd"))]
        if self.last_blur.replace(Some(config.window_blur)) != Some(config.window_blur) {
            window.set_blur(config.window_blur);
            self.set_status(config.window_blur, BackdropStatus::Active);
        }

        #[cfg(not(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux",
            target_os = "freebsd",
            target_os = "openbsd"
        )))]
        let _ = (window, config);
    }

    #[cfg(not(target_os = "macos"))]
    fn set_status(&self, enabled: bool, enabled_status: BackdropStatus) {
        let status = if enabled {
            enabled_status
        } else {
            BackdropStatus::Disabled
        };
        self.status.set(status);
        if status == BackdropStatus::Unavailable && !self.reported_unavailable.replace(true) {
            #[cfg(target_os = "linux")]
            log::warn!("native window blur is unavailable; using Kettle's 99% opacity fallback");
            #[cfg(not(target_os = "linux"))]
            log::warn!("native window backdrop is unavailable; continuing without native blur");
        }
    }

    pub(crate) fn live_opacity_floor(&self) -> Option<f32> {
        live_opacity_floor_for_status(self.status.get(), cfg!(target_os = "linux"))
    }

    #[cfg(target_os = "macos")]
    fn sync_macos(&self, window: &Window, config: &kettle_config::Config) {
        let Some(main_thread) = objc2_foundation::MainThreadMarker::new() else {
            return;
        };
        let now = std::time::Instant::now();
        let accessibility_changed = self
            .accessibility_display_dirty
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        let due =
            now.duration_since(self.checked_at.get()) >= std::time::Duration::from_millis(500);
        let blur_changed = self.last_blur.get() != Some(config.window_blur);
        let background_changed = self.last_background.get() != Some(config.theme.background);
        if !accessibility_changed && !due && !blur_changed && !background_changed {
            return;
        }
        self.checked_at.set(now);
        let reduce = macos_reduce_transparency(&main_thread);
        if !blur_changed
            && !background_changed
            && self.last_reduce_transparency.get() == Some(reduce)
        {
            return;
        }
        self.last_blur.set(Some(config.window_blur));
        self.last_background.set(Some(config.theme.background));
        self.last_reduce_transparency.set(Some(reduce));
        let policy = macos_material_policy(config.window_blur, self.alpha_surface_required, reduce);
        // Frame subviews can remove AppKit's light caption on macOS 27.
        // Winit applies blur to the window without changing that hierarchy.
        window.set_blur(policy.effect_visible && self.native_blur_allowed);
        sync_macos_window_background(
            window,
            config.theme.background,
            policy.window_opaque,
            &main_thread,
        );
        self.status.set(macos_backdrop_status(
            config.window_blur,
            policy,
            self.native_blur_allowed,
        ));
        window.request_redraw();
    }
}

#[cfg(target_os = "macos")]
impl Drop for NativeMaterial {
    fn drop(&mut self) {
        if let (Some(center), Some(observer)) =
            (&self.notification_center, &self.notification_observer)
        {
            // SAFETY: both objects are retained by this material and AppKit's
            // notification API permits removing an observer more than once.
            unsafe { center.removeObserver(observer) };
        }
    }
}

#[cfg(target_os = "macos")]
fn observe_macos_accessibility_display(
    window: &Arc<Window>,
) -> (
    Option<objc2::rc::Retained<objc2_foundation::NSNotificationCenter>>,
    Option<objc2::rc::Retained<objc2_foundation::NSObject>>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    use block2::RcBlock;
    use objc2_app_kit::{NSWorkspace, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification};
    use objc2_foundation::NSNotification;
    use std::ptr::NonNull;

    let dirty = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let Some(_main_thread) = objc2_foundation::MainThreadMarker::new() else {
        return (None, None, dirty);
    };
    let dirty_for_observer = dirty.clone();
    let weak_window = Arc::downgrade(window);
    let workspace = unsafe { NSWorkspace::sharedWorkspace() };
    let center = unsafe { workspace.notificationCenter() };
    let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
        dirty_for_observer.store(true, std::sync::atomic::Ordering::Release);
        if let Some(window) = weak_window.upgrade() {
            window.request_redraw();
        }
    });
    // SAFETY: registration runs on AppKit's main thread. Passing no operation
    // queue delivers the workspace notification on its posting thread, and the
    // retained token is removed before `NativeMaterial` is dropped.
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
            None,
            None,
            &block,
        )
    };
    (Some(center), Some(observer), dirty)
}

#[cfg(target_os = "macos")]
fn macos_reduce_transparency(_main_thread: &objc2_foundation::MainThreadMarker) -> bool {
    use objc2_app_kit::NSWorkspace;
    unsafe { NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency() }
}

#[cfg(target_os = "macos")]
fn sync_macos_window_background(
    window: &Window,
    color: kettle_config::Rgb,
    opaque: bool,
    _main_thread: &objc2_foundation::MainThreadMarker,
) {
    use objc2_app_kit::{NSColor, NSView};
    use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let view: &NSView = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
    let Some(ns_window) = view.window() else {
        return;
    };
    let background = unsafe {
        NSColor::colorWithSRGBRed_green_blue_alpha(
            f64::from(color.r) / 255.0,
            f64::from(color.g) / 255.0,
            f64::from(color.b) / 255.0,
            if opaque { 1.0 } else { 0.0 },
        )
    };
    ns_window.setOpaque(opaque);
    ns_window.setBackgroundColor(Some(&background));
    ns_window.setTitlebarAppearsTransparent(false);
}

#[cfg(target_os = "windows")]
fn sync_windows_backdrop(
    window: &Window,
    enabled: bool,
    caption: (kettle_config::Rgb, kettle_config::Rgb),
) -> bool {
    use windows::Win32::Graphics::Dwm::{
        DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW, DWMWA_CAPTION_COLOR, DWMWA_SYSTEMBACKDROP_TYPE,
        DWMWA_TEXT_COLOR, DwmSetWindowAttribute,
    };
    use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return false;
    };
    let value = if enabled {
        DWMSBT_TRANSIENTWINDOW
    } else {
        DWMSBT_NONE
    };
    let hwnd = windows::Win32::Foundation::HWND(win32.hwnd.get() as *mut core::ffi::c_void);
    let backdrop = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&raw const value).cast(),
            std::mem::size_of_val(&value) as u32,
        )
    };
    if let Err(error) = &backdrop {
        log::debug!("native window backdrop unavailable: {error}");
    }
    for (attribute, color, name) in [
        (DWMWA_CAPTION_COLOR, colorref(caption.0), "caption"),
        (DWMWA_TEXT_COLOR, colorref(caption.1), "caption text"),
    ] {
        if let Err(error) = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attribute,
                (&raw const color).cast(),
                std::mem::size_of_val(&color) as u32,
            )
        } {
            log::debug!("native {name} color unavailable: {error}");
        }
    }
    !enabled || backdrop.is_ok()
}

#[cfg(any(target_os = "windows", test))]
fn colorref(color: kettle_config::Rgb) -> u32 {
    u32::from(color.r) | (u32::from(color.g) << 8) | (u32::from(color.b) << 16)
}

#[cfg(any(target_os = "linux", test))]
fn linux_blur_policy(is_wayland: bool, kwin_blur_advertised: bool) -> bool {
    is_wayland && kwin_blur_advertised
}

#[cfg(target_os = "linux")]
fn linux_is_wayland(window: &Window) -> bool {
    use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return false;
    };
    if !matches!(handle.as_raw(), RawWindowHandle::Wayland(_)) {
        return false;
    }
    true
}

#[cfg(target_os = "linux")]
fn probe_wayland_kwin_blur() -> bool {
    use std::os::fd::{AsFd as _, AsRawFd as _};
    use wayland_client::{
        Connection, Dispatch, QueueHandle,
        protocol::{wl_callback, wl_registry},
    };

    #[derive(Default)]
    struct RegistryProbe {
        supported: bool,
        done: bool,
    }
    impl Dispatch<wl_registry::WlRegistry, ()> for RegistryProbe {
        fn event(
            state: &mut Self,
            _: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global { interface, .. } = event
                && interface == "org_kde_kwin_blur_manager"
            {
                state.supported = true;
            }
        }
    }
    impl Dispatch<wl_callback::WlCallback, ()> for RegistryProbe {
        fn event(
            state: &mut Self,
            _: &wl_callback::WlCallback,
            _: wl_callback::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            state.done = true;
        }
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    // A private probe must not consume WAYLAND_SOCKET or mutate the process environment.
    let Some(display) = std::env::var_os("WAYLAND_DISPLAY").map(std::path::PathBuf::from) else {
        return false;
    };
    let path = if display.is_absolute() {
        display
    } else {
        let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from)
        else {
            return false;
        };
        if !runtime.is_absolute() {
            return false;
        }
        runtime.join(display)
    };
    let Ok(socket) = std::os::unix::net::UnixStream::connect(path) else {
        return false;
    };
    let Ok(connection) = Connection::from_socket(socket) else {
        return false;
    };
    let mut queue = connection.new_event_queue::<RegistryProbe>();
    let handle = queue.handle();
    let _registry = connection.display().get_registry(&handle, ());
    let _sync = connection.display().sync(&handle, ());
    let mut state = RegistryProbe::default();
    loop {
        if std::time::Instant::now() >= deadline || queue.dispatch_pending(&mut state).is_err() {
            return false;
        }
        if state.done {
            return state.supported;
        }
        if connection.flush().is_err() {
            return false;
        }
        let Some(read) = queue.prepare_read() else {
            continue;
        };
        let mut poll = libc::pollfd {
            fd: connection.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let millis = remaining.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: poll points to one initialized descriptor for this helper's connection.
        let ready = unsafe { libc::poll(&mut poll, 1, millis) };
        if ready <= 0 || read.read().is_err() {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BackdropStatus, MacosMaterialPolicy, caption_colors, colorref, contrast_ratio,
        linux_blur_policy, live_opacity_floor_for_status, macos_backdrop_status,
        macos_material_policy, macos_native_blur_allowed,
    };
    use kettle_config::{BackgroundType, Config};

    #[test]
    fn stalled_blur_discovery_keeps_install_responsive_and_fallback_active() {
        let discovery = super::BlurDiscovery::default();
        discovery.request(
            false,
            || panic!("disabled blur must not probe"),
            std::time::Duration::from_millis(20),
            std::sync::Arc::new(|| {}),
        );
        let (release, waiting) = std::sync::mpsc::channel();
        let (entered, started) = std::sync::mpsc::channel();
        let (returned, received) = std::sync::mpsc::channel();
        let (wake, woken) = std::sync::mpsc::channel();
        let checked = discovery.clone();
        let ui = std::thread::spawn(move || {
            checked.request(
                true,
                move || {
                    entered.send(()).unwrap();
                    let _ = waiting.recv();
                    false
                },
                std::time::Duration::from_millis(20),
                std::sync::Arc::new(move || {
                    let _ = wake.send(());
                }),
            );
            returned.send(()).unwrap();
        });
        started
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        let responsive = received
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_ok();
        assert!(!discovery.supported());
        woken
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        discovery.request(
            true,
            || panic!("a timeout must not retry before configuration changes"),
            std::time::Duration::from_millis(20),
            std::sync::Arc::new(|| {}),
        );
        release.send(()).unwrap();
        ui.join().unwrap();
        assert!(
            responsive,
            "Wayland discovery must not block window installation"
        );
        discovery.reset();
        assert!(!discovery.supported());
    }

    #[test]
    fn borderless_macos_windows_keep_visible_alpha_instead_of_covering_metal() {
        assert!(macos_native_blur_allowed(false));
        assert!(
            !macos_native_blur_allowed(true),
            "existing borderless windows keep the sharp-alpha fallback"
        );
        let source = kettle_test_support::production_source(include_str!("native_material.rs"));
        assert!(
            source.contains("native_blur_allowed: macos_native_blur_allowed(config.borderless)"),
            "NativeMaterial::install must apply the borderless safety policy"
        );
    }

    #[test]
    fn macos_material_keeps_the_window_creation_alpha_decision() {
        let source = kettle_test_support::production_source(include_str!("native_material.rs"));
        assert_eq!(
            source
                .matches("kettle_render::window_requires_alpha_surface(config)")
                .count(),
            1,
            "surface transparency must be captured once when the window is installed"
        );
        let sync = source
            .split("fn sync_macos(")
            .nth(1)
            .and_then(|body| body.split("\n    }").next())
            .expect("sync_macos body");
        let normalized = sync.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains(
                "macos_material_policy(config.window_blur, self.alpha_surface_required, reduce)"
            ),
            "live sync must use the alpha decision made with the native window and surface"
        );
        assert!(
            !sync.contains("window_requires_alpha_surface"),
            "an opacity reload cannot change an existing window's surface capabilities"
        );
    }

    #[test]
    fn macos_material_distinguishes_blur_alpha_and_accessibility_fallback() {
        assert_eq!(
            macos_material_policy(false, false, false),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: true,
            }
        );
        assert_eq!(
            macos_material_policy(false, true, false),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: false,
            },
            "plain alpha keeps a transparent content surface"
        );
        assert_eq!(
            macos_material_policy(true, false, false),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: true,
            },
            "blur cannot show through a surface whose renderer is fully opaque"
        );
        assert_eq!(
            macos_material_policy(true, true, false),
            MacosMaterialPolicy {
                effect_visible: true,
                window_opaque: false,
            },
            "the documented blur-plus-alpha path needs material behind the content"
        );
        assert_eq!(
            macos_material_policy(false, true, true),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: true,
            },
            "Reduce Transparency restores the theme-colored opaque titlebar without blur"
        );
        assert_eq!(
            macos_material_policy(true, true, true),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: true,
            },
            "Reduce Transparency must disable both blur and translucency"
        );

        let visible = macos_material_policy(true, true, false);
        assert_eq!(
            macos_backdrop_status(true, visible, true),
            BackdropStatus::Active
        );
        assert_eq!(
            macos_backdrop_status(true, visible, false),
            BackdropStatus::Disabled,
            "a borderless window deliberately keeps sharp alpha"
        );
        assert_eq!(
            macos_backdrop_status(true, macos_material_policy(true, true, true), true),
            BackdropStatus::Disabled,
            "Reduce Transparency is a user policy, not a compositor failure"
        );
    }

    #[test]
    fn transparent_background_uses_the_shared_surface_alpha_policy() {
        let config = Config {
            background_type: BackgroundType::Transparent,
            background_opacity: 1.0,
            background_darkness: 0.85,
            ..Config::default()
        };
        let alpha_required = kettle_render::window_requires_alpha_surface(&config);
        assert!(alpha_required);
        assert_eq!(
            macos_material_policy(false, alpha_required, false),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: false,
            }
        );
    }

    #[test]
    fn starfield_stays_opaque_without_a_titlebar_only_material_seam() {
        let config = Config {
            background_type: BackgroundType::Starfield,
            background_opacity: 0.4,
            background_darkness: 0.2,
            ..Config::default()
        };
        let alpha_required = kettle_render::window_requires_alpha_surface(&config);
        assert!(!alpha_required, "the starfield shader fills the surface");
        assert_eq!(
            macos_material_policy(false, alpha_required, false),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: true,
            }
        );
        assert_eq!(
            macos_material_policy(true, alpha_required, false),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: true,
            },
            "an opaque scene must not expose material only in its native titlebar"
        );
    }

    #[test]
    fn caption_surface_is_opaque_theme_chrome_with_readable_text() {
        for theme in [Config::default().theme, {
            let mut theme = Config::default().theme;
            theme.background = kettle_config::Rgb::new(242, 244, 248);
            theme.foreground = kettle_config::Rgb::new(36, 41, 52);
            theme
        }] {
            let (surface, text) = caption_colors(&theme);
            assert_ne!(surface, theme.background, "caption must be visibly raised");
            assert!(
                contrast_ratio(text, surface) >= 4.5,
                "caption text must remain readable against {surface:?}"
            );
        }
    }

    #[test]
    fn windows_colorref_uses_the_dwm_byte_order() {
        assert_eq!(
            colorref(kettle_config::Rgb::new(0x12, 0x34, 0x56)),
            0x563412
        );
    }

    #[test]
    fn linux_blur_requires_wayland_and_the_kwin_protocol() {
        assert!(linux_blur_policy(true, true));
        assert!(!linux_blur_policy(true, false));
        assert!(!linux_blur_policy(false, true));
    }

    #[test]
    fn near_opaque_fallback_is_linux_only() {
        assert_eq!(
            live_opacity_floor_for_status(BackdropStatus::Unavailable, true),
            Some(0.99)
        );
        assert_eq!(
            live_opacity_floor_for_status(BackdropStatus::Unavailable, false),
            None,
            "Windows must retain ordinary alpha when its optional backdrop is unavailable"
        );
        assert_eq!(
            live_opacity_floor_for_status(BackdropStatus::Active, true),
            None
        );
    }

    #[test]
    fn wayland_registry_discovery_is_shared_and_never_round_trips_on_install() {
        let source = kettle_test_support::production_source(include_str!("native_material.rs"));
        assert!(source.contains("std::sync::OnceLock<BlurDiscovery>"));
        assert!(!source.contains("registry_queue_init"));
        assert!(!source.contains(".roundtrip("));
        let install = source
            .split("fn install(")
            .nth(1)
            .unwrap()
            .split("fn sync(")
            .next()
            .unwrap();
        assert!(!install.contains("probe_wayland_kwin_blur"));
        assert!(source.contains("UserEvent::BlurDiscovered"));
    }
}
