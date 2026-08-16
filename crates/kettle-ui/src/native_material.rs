//! Native window material behind Kettle's transparent render surface.

use std::sync::Arc;
use winit::window::Window;

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MacosMaterialPolicy {
    effect_visible: bool,
    window_opaque: bool,
    titlebar_transparent: bool,
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
        // A transparent native titlebar needs either the theme-colored window
        // background or the frame-wide material behind it. With plain alpha
        // transparency it would otherwise become a fully clear desktop strip
        // above a tinted terminal, so let AppKit draw its standard backdrop.
        titlebar_transparent: !alpha_surface_required || window_blur || reduce_transparency,
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_native_effect_supported(borderless: bool) -> bool {
    // A sibling NSVisualEffectView works with AppKit's decorated frame view.
    // In a borderless NSWindow it composites over Winit's CAMetalLayer and
    // hides the terminal, so preserve visible sharp alpha instead of blur.
    !borderless
}

pub(crate) struct NativeMaterial {
    last_blur: std::cell::Cell<Option<bool>>,
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
            };
            material.sync(window, config);
            material
        }
    }

    pub(crate) fn sync(&self, window: &Window, config: &kettle_config::Config) {
        #[cfg(target_os = "macos")]
        self.sync_macos(window, config);

        #[cfg(target_os = "windows")]
        if self.last_blur.replace(Some(config.window_blur)) != Some(config.window_blur) {
            sync_windows_backdrop(window, config.window_blur);
        }

        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
        if self.last_blur.replace(Some(config.window_blur)) != Some(config.window_blur) {
            window.set_blur(config.window_blur);
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
            policy.titlebar_transparent,
            &main_thread,
        );
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
    // The frame view includes macOS's native title bar; WinitView's frame does
    // not. Sizing the material from the content view therefore left the area
    // behind the traffic lights completely clear while the terminal below was
    // blurred. Fill the frame view and keep WinitView above it so one material
    // backs both regions without covering the Metal surface or title controls.
    let frame = frame_view.bounds();
    let effect = unsafe {
        NSVisualEffectView::initWithFrame(main_thread.alloc::<NSVisualEffectView>(), frame)
    };
    unsafe {
        effect.setMaterial(NSVisualEffectMaterial::UnderWindowBackground);
        effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        effect.setState(NSVisualEffectState::FollowsWindowActiveState);
        effect.setAutoresizingMask(
            NSAutoresizingMaskOptions::NSViewWidthSizable
                | NSAutoresizingMaskOptions::NSViewHeightSizable,
        );
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
    titlebar_transparent: bool,
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
    ns_window.setTitlebarAppearsTransparent(titlebar_transparent);
}

#[cfg(target_os = "windows")]
fn sync_windows_backdrop(window: &Window, enabled: bool) {
    use windows::Win32::Graphics::Dwm::{
        DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DwmSetWindowAttribute,
    };
    use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };
    let value = if enabled {
        DWMSBT_TRANSIENTWINDOW
    } else {
        DWMSBT_NONE
    };
    let hwnd = windows::Win32::Foundation::HWND(win32.hwnd.get() as *mut core::ffi::c_void);
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&raw const value).cast(),
            std::mem::size_of_val(&value) as u32,
        )
    };
    if let Err(error) = result {
        log::debug!("native window backdrop unavailable: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::{MacosMaterialPolicy, macos_material_policy, macos_native_effect_supported};
    use kettle_config::{BackgroundType, Config};

    #[test]
    fn macos_material_covers_the_native_titlebar() {
        let source = kettle_test_support::production_source(include_str!("native_material.rs"));
        let install = source
            .split("fn install_macos_effect(")
            .nth(1)
            .and_then(|body| body.split("fn macos_reduce_transparency").next())
            .expect("install_macos_effect body");
        let normalized = install.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            install.contains("let frame = frame_view.bounds();"),
            "the effect must use native frame bounds so it continues behind the titlebar"
        );
        assert!(
            !install.contains("content.frame()"),
            "content bounds exclude the native titlebar and make it fully transparent"
        );
        assert!(
            normalized.contains(
                "NSVisualEffectView::initWithFrame(main_thread.alloc::<NSVisualEffectView>(), frame)"
            ),
            "the frame-view bounds must be the rectangle passed to the material view"
        );
        assert!(
            normalized.contains(
                "effect.setAutoresizingMask( NSAutoresizingMaskOptions::NSViewWidthSizable | NSAutoresizingMaskOptions::NSViewHeightSizable, );"
            ),
            "the material must keep covering the titlebar after a window resize"
        );
        assert!(
            normalized.contains(
                "frame_view.addSubview_positioned_relativeTo( &effect, NSWindowOrderingMode::NSWindowBelow, Some(content),"
            ),
            "the material must stay below the Metal content and native controls"
        );
        assert_eq!(
            source.matches("effect.setAutoresizingMask(").count(),
            1,
            "the material's autoresizing policy must be assigned exactly once"
        );
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
                "a later {setter} call could silently restore the transparent strip"
            );
        }
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
                titlebar_transparent: true,
            }
        );
        assert_eq!(
            macos_material_policy(false, true, false),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: false,
                titlebar_transparent: false,
            },
            "plain alpha uses AppKit's backdrop instead of a fully clear titlebar"
        );
        assert_eq!(
            macos_material_policy(true, false, false),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: true,
                titlebar_transparent: true,
            },
            "blur cannot show through a surface whose renderer is fully opaque"
        );
        assert_eq!(
            macos_material_policy(true, true, false),
            MacosMaterialPolicy {
                effect_visible: true,
                window_opaque: false,
                titlebar_transparent: true,
            },
            "the documented blur-plus-alpha path needs the material behind a transparent titlebar"
        );
        assert_eq!(
            macos_material_policy(false, true, true),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: true,
                titlebar_transparent: true,
            },
            "Reduce Transparency restores the theme-colored opaque titlebar without blur"
        );
        assert_eq!(
            macos_material_policy(true, true, true),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: true,
                titlebar_transparent: true,
            },
            "Reduce Transparency must disable both blur and translucency"
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
                titlebar_transparent: false,
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
                titlebar_transparent: true,
            }
        );
        assert_eq!(
            macos_material_policy(true, alpha_required, false),
            MacosMaterialPolicy {
                effect_visible: false,
                window_opaque: true,
                titlebar_transparent: true,
            },
            "an opaque scene must not expose material only in its native titlebar"
        );
    }
}
