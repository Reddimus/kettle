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
    effect_installed: bool,
) -> BackdropStatus {
    if window_blur && policy.effect_visible && effect_installed {
        BackdropStatus::Active
    } else {
        // Reduce Transparency deliberately replaces material with an opaque
        // surface, while borderless windows deliberately retain sharp alpha.
        // Neither is a compositor failure and neither needs Linux's fallback.
        BackdropStatus::Disabled
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_native_effect_supported(borderless: bool) -> bool {
    // A sibling NSVisualEffectView works with AppKit's decorated frame view.
    // In a borderless NSWindow it composites over Winit's CAMetalLayer and
    // hides the terminal, so preserve visible sharp alpha instead of blur.
    !borderless
}

fn live_opacity_floor_for_status(status: BackdropStatus, linux_fallback: bool) -> Option<f32> {
    (linux_fallback && status == BackdropStatus::Unavailable).then_some(0.99)
}

pub(crate) struct NativeMaterial {
    last_blur: std::cell::Cell<Option<bool>>,
    status: std::cell::Cell<BackdropStatus>,
    #[cfg(not(target_os = "macos"))]
    reported_unavailable: std::cell::Cell<bool>,
    #[cfg(target_os = "macos")]
    effect: Option<objc2::rc::Retained<objc2_app_kit::NSVisualEffectView>>,
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
    linux_blur_supported: bool,
}

impl NativeMaterial {
    pub(crate) fn install(window: &Arc<Window>, config: &kettle_config::Config) -> Self {
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
                effect: if macos_native_effect_supported(config.borderless) {
                    install_macos_effect(window)
                } else {
                    None
                },
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
                linux_blur_supported: linux_blur_supported(window),
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
        if self.last_blur.replace(Some(config.window_blur)) != Some(config.window_blur) {
            let active = config.window_blur && self.linux_blur_supported;
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
        if let Some(effect) = &self.effect {
            effect.setHidden(!policy.effect_visible);
        }
        sync_macos_window_background(
            window,
            config.theme.background,
            policy.window_opaque,
            &main_thread,
        );
        self.status.set(macos_backdrop_status(
            config.window_blur,
            policy,
            self.effect.is_some(),
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
fn install_macos_effect(
    window: &Window,
) -> Option<objc2::rc::Retained<objc2_app_kit::NSVisualEffectView>> {
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
        NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode,
    };
    use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    let main_thread = objc2_foundation::MainThreadMarker::new()?;
    let content: &NSView = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
    content.window()?;
    // SAFETY: winit's live content view is attached to AppKit's frame view
    // while the window exists, and this runs on the main thread during setup.
    let frame_view = unsafe { content.superview()? };
    // Material belongs to terminal content, not the caption. The initial frame
    // prevents a one-frame flash; constraints below track Winit's content rect
    // through resize, titlebar auto-hide, and fullscreen transitions.
    let frame = content.frame();
    let effect = unsafe {
        NSVisualEffectView::initWithFrame(main_thread.alloc::<NSVisualEffectView>(), frame)
    };
    unsafe {
        effect.setMaterial(NSVisualEffectMaterial::UnderWindowBackground);
        effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        effect.setState(NSVisualEffectState::FollowsWindowActiveState);
        effect.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.setAutoresizingMask(
            NSAutoresizingMaskOptions::NSViewWidthSizable
                | NSAutoresizingMaskOptions::NSViewHeightSizable,
        );
        // Keep winit's private WinitView as the NSWindow content view. Winit
        // retrieves that view later and casts it back to its declared class;
        // replacing it makes ordinary calls such as `set_cursor` abort. The
        // effect also cannot be a child of WinitView: AppKit composites child
        // views above the parent's Metal layer. Put it in the native frame
        // view as a sibling immediately behind WinitView instead.
        frame_view.addSubview_positioned_relativeTo(
            &effect,
            NSWindowOrderingMode::NSWindowBelow,
            Some(content),
        );
        for constraint in [
            effect
                .leadingAnchor()
                .constraintEqualToAnchor(&content.leadingAnchor()),
            effect
                .trailingAnchor()
                .constraintEqualToAnchor(&content.trailingAnchor()),
            effect
                .topAnchor()
                .constraintEqualToAnchor(&content.topAnchor()),
            effect
                .bottomAnchor()
                .constraintEqualToAnchor(&content.bottomAnchor()),
        ] {
            constraint.setActive(true);
        }
    }
    Some(effect)
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
fn linux_blur_supported(window: &Window) -> bool {
    use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return false;
    };
    if !matches!(handle.as_raw(), RawWindowHandle::Wayland(_)) {
        return false;
    }
    linux_blur_policy(true, wayland_advertises_kwin_blur())
}

#[cfg(target_os = "linux")]
fn wayland_advertises_kwin_blur() -> bool {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(probe_wayland_kwin_blur)
}

#[cfg(target_os = "linux")]
fn probe_wayland_kwin_blur() -> bool {
    use wayland_client::{
        Connection, Dispatch, QueueHandle,
        globals::{GlobalListContents, registry_queue_init},
        protocol::wl_registry,
    };

    struct RegistryProbe;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for RegistryProbe {
        fn event(
            _state: &mut Self,
            _proxy: &wl_registry::WlRegistry,
            _event: wl_registry::Event,
            _data: &GlobalListContents,
            _connection: &Connection,
            _queue: &QueueHandle<Self>,
        ) {
        }
    }

    let Ok(connection) = Connection::connect_to_env() else {
        return false;
    };
    let Ok((globals, _queue)) = registry_queue_init::<RegistryProbe>(&connection) else {
        return false;
    };
    globals.contents().with_list(|list| {
        list.iter()
            .any(|global| global.interface == "org_kde_kwin_blur_manager")
    })
}

#[cfg(test)]
mod tests {
    use super::{
        BackdropStatus, MacosMaterialPolicy, caption_colors, colorref, contrast_ratio,
        linux_blur_policy, live_opacity_floor_for_status, macos_backdrop_status,
        macos_material_policy, macos_native_effect_supported,
    };
    use kettle_config::{BackgroundType, Config};

    #[test]
    fn macos_material_stops_at_the_native_titlebar() {
        let source = kettle_test_support::production_source(include_str!("native_material.rs"));
        let install = source
            .split("fn install_macos_effect(")
            .nth(1)
            .and_then(|body| body.split("fn macos_reduce_transparency").next())
            .expect("install_macos_effect body");
        let normalized = install.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            install.contains("let frame = content.frame();"),
            "the effect must use content geometry so AppKit can paint the titlebar"
        );
        assert!(
            !install.contains("let frame = frame_view.bounds();"),
            "frame bounds expose the desktop through the complete native caption"
        );
        assert!(
            normalized.contains(
                "NSVisualEffectView::initWithFrame(main_thread.alloc::<NSVisualEffectView>(), frame)"
            ),
            "the frame-view bounds must be the rectangle passed to the material view"
        );
        assert!(
            normalized.contains("effect.setTranslatesAutoresizingMaskIntoConstraints(false);"),
            "the material must use explicit content-relative constraints"
        );
        assert!(
            normalized.contains(
                "frame_view.addSubview_positioned_relativeTo( &effect, NSWindowOrderingMode::NSWindowBelow, Some(content),"
            ),
            "the material must stay below the Metal content and native controls"
        );
        for edge in ["leading", "trailing", "top", "bottom"] {
            assert!(
                normalized.contains(&format!(
                    "effect .{edge}Anchor() .constraintEqualToAnchor(&content.{edge}Anchor())"
                )),
                "the material's {edge} edge must follow the content view"
            );
        }
        for setter in [
            "effect.setFrame(",
            "effect.setFrameSize(",
            "effect.setFrameOrigin(",
            "effect.setBounds(",
            "effect.setBoundsSize(",
            "effect.setBoundsOrigin(",
        ] {
            assert!(
                !source.contains(setter),
                "a later {setter} call could silently expand material back into the caption"
            );
        }
        assert!(
            source.contains("ns_window.setTitlebarAppearsTransparent(false);"),
            "AppKit must draw an opaque native caption instead of exposing the desktop"
        );
        let sync = source
            .split("fn sync_macos(")
            .nth(1)
            .and_then(|body| body.split("fn observe_macos_accessibility_display").next())
            .expect("sync_macos body");
        assert!(
            sync.contains("config.theme.background"),
            "the exact terminal background must back live resize"
        );
        assert!(
            !sync.contains("caption_colors(&config.theme)"),
            "AppKit owns native caption color; its backing surface must not invent a raised strip"
        );
    }

    #[test]
    fn borderless_macos_windows_keep_visible_alpha_instead_of_covering_metal() {
        assert!(macos_native_effect_supported(false));
        assert!(
            !macos_native_effect_supported(true),
            "borderless AppKit composition must fall back instead of hiding Winit's Metal view"
        );
        let source = kettle_test_support::production_source(include_str!("native_material.rs"));
        assert!(
            source.contains("if macos_native_effect_supported(config.borderless)"),
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
    fn wayland_registry_probe_is_cached_per_process() {
        let source = kettle_test_support::production_source(include_str!("native_material.rs"));
        let probe = source
            .split("fn wayland_advertises_kwin_blur()")
            .nth(1)
            .and_then(|body| body.split("fn probe_wayland_kwin_blur()").next())
            .expect("Wayland blur cache body");
        assert!(probe.contains("std::sync::OnceLock<bool>"));
        assert!(probe.contains("SUPPORTED.get_or_init(probe_wayland_kwin_blur)"));
    }
}
