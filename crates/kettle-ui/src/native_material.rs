//! Native window material behind Kettle's transparent render surface.

use std::sync::Arc;
use winit::window::Window;

#[cfg(any(target_os = "macos", test))]
fn macos_material_policy(
    window_blur: bool,
    background_opacity: f32,
    reduce_transparency: bool,
) -> (bool, bool) {
    let effect_visible = window_blur && !reduce_transparency;
    let translucent = !reduce_transparency && (window_blur || background_opacity < 1.0);
    (effect_visible, !translucent)
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
    last_opacity_bits: std::cell::Cell<Option<u32>>,
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
            let (notification_center, notification_observer, accessibility_display_dirty) =
                observe_macos_accessibility_display(window);
            let material = Self {
                last_blur: std::cell::Cell::new(None),
                effect: install_macos_effect(window),
                last_reduce_transparency: std::cell::Cell::new(None),
                last_background: std::cell::Cell::new(None),
                last_opacity_bits: std::cell::Cell::new(None),
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
        let now = std::time::Instant::now();
        let accessibility_changed = self
            .accessibility_display_dirty
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        let due =
            now.duration_since(self.checked_at.get()) >= std::time::Duration::from_millis(500);
        let blur_changed = self.last_blur.get() != Some(config.window_blur);
        let background_changed = self.last_background.get() != Some(config.theme.background);
        let opacity_changed =
            self.last_opacity_bits.get() != Some(config.background_opacity.to_bits());
        if !accessibility_changed
            && !due
            && !blur_changed
            && !background_changed
            && !opacity_changed
        {
            return;
        }
        self.checked_at.set(now);
        let reduce = macos_reduce_transparency();
        if !blur_changed
            && !background_changed
            && !opacity_changed
            && self.last_reduce_transparency.get() == Some(reduce)
        {
            return;
        }
        self.last_blur.set(Some(config.window_blur));
        self.last_background.set(Some(config.theme.background));
        self.last_opacity_bits
            .set(Some(config.background_opacity.to_bits()));
        self.last_reduce_transparency.set(Some(reduce));
        let (active, opaque) =
            macos_material_policy(config.window_blur, config.background_opacity, reduce);
        if let Some(effect) = &self.effect {
            effect.setHidden(!active);
        }
        set_macos_opaque_background(window, config.theme.background, opaque);
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
    let content: &NSView = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
    content.window()?;
    // SAFETY: winit's live content view is attached to AppKit's frame view
    // while the window exists, and this runs on the main thread during setup.
    let frame_view = unsafe { content.superview()? };
    let frame = content.frame();
    let main_thread = objc2_foundation::MainThreadMarker::new()?;
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
fn macos_reduce_transparency() -> bool {
    use objc2_app_kit::NSWorkspace;
    unsafe { NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency() }
}

#[cfg(target_os = "macos")]
fn set_macos_opaque_background(window: &Window, color: kettle_config::Rgb, opaque: bool) {
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
    use super::macos_material_policy;

    #[test]
    fn macos_material_distinguishes_blur_alpha_and_accessibility_fallback() {
        assert_eq!(macos_material_policy(false, 1.0, false), (false, true));
        assert_eq!(macos_material_policy(false, 0.7, false), (false, false));
        assert_eq!(macos_material_policy(true, 1.0, false), (true, false));
        assert_eq!(
            macos_material_policy(true, 0.7, true),
            (false, true),
            "Reduce Transparency must disable both blur and translucency"
        );
    }
}
