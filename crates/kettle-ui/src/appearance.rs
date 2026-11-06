//! System appearance is independent of a window's forced palette/material.

#[cfg(any(target_os = "linux", test))]
pub(crate) fn portal_dark(value: Option<u32>) -> bool {
    // XDG: 0 has no preference, 1 prefers dark, 2 prefers light.
    value != Some(2)
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn portal_setting_matches(namespace: &str, changed_key: &str, key: &str) -> bool {
    namespace == "org.freedesktop.appearance" && changed_key == key
}

pub(crate) struct SystemAppearance {
    #[cfg(target_os = "linux")]
    watch: crate::portal_settings::Watch,
    #[cfg(target_os = "macos")]
    observer: Option<macos::Observer>,
}

impl SystemAppearance {
    pub(crate) fn new(wake: kettle_core::Waker) -> Self {
        #[cfg(target_os = "linux")]
        {
            Self {
                watch: crate::portal_settings::Watch::start(wake, None, "color-scheme"),
            }
        }
        #[cfg(target_os = "macos")]
        {
            Self {
                observer: macos::Observer::new(wake),
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = wake;
            Self {}
        }
    }

    pub(crate) fn dark(&self) -> Option<bool> {
        #[cfg(target_os = "linux")]
        {
            Some(portal_dark(self.watch.value()))
        }
        #[cfg(target_os = "macos")]
        {
            self.observer
                .as_ref()
                .map_or_else(macos::system_dark, macos::Observer::dark)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            None
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol};
    use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability, sel};
    use objc2_foundation::{
        MainThreadMarker, NSDistributedNotificationCenter, NSNotification,
        NSNotificationSuspensionBehavior, ns_string,
    };

    struct ObserverState {
        dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
        wake: kettle_core::Waker,
    }

    declare_class!(
        struct AppearanceTarget;
        // SAFETY: NSObject layout and main-thread-only access come from declare_class.
        unsafe impl ClassType for AppearanceTarget {
            type Super = NSObject;
            type Mutability = mutability::MainThreadOnly;
            const NAME: &'static str = "KettleSystemAppearanceTarget";
        }
        impl DeclaredClass for AppearanceTarget { type Ivars = ObserverState; }
        // SAFETY: NSObject's protocol implementation is inherited unchanged.
        unsafe impl NSObjectProtocol for AppearanceTarget {}
        // SAFETY: The notification callback has AppKit's void(id) signature.
        unsafe impl AppearanceTarget {
            #[method(kettleAppearanceChanged:)]
            fn changed(&self, _notification: &NSNotification) {
                self.ivars().dirty.store(true, std::sync::atomic::Ordering::Release);
                (self.ivars().wake)();
            }
        }
    );

    pub(super) struct Observer {
        center: Retained<NSDistributedNotificationCenter>,
        token: Retained<AppearanceTarget>,
        dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
        cached: std::cell::Cell<Option<bool>>,
    }

    impl Observer {
        pub(super) fn new(wake: kettle_core::Waker) -> Option<Self> {
            let mtm = MainThreadMarker::new()?;
            let dirty = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            let this = mtm.alloc::<AppearanceTarget>().set_ivars(ObserverState {
                dirty: dirty.clone(),
                wake,
            });
            // SAFETY: This newly allocated target has initialized ivars; NSObject init consumes it.
            let token: Retained<AppearanceTarget> = unsafe { msg_send_id![super(this), init] };
            // SAFETY: Register on the main thread; the callback only invalidates the cached
            // preference and wakes the event loop. The retained target is removed in Drop.
            let center = unsafe {
                let center = NSDistributedNotificationCenter::defaultCenter();
                center.addObserver_selector_name_object_suspensionBehavior(
                    &token,
                    sel!(kettleAppearanceChanged:),
                    Some(ns_string!("AppleInterfaceThemeChangedNotification")),
                    None,
                    NSNotificationSuspensionBehavior::DeliverImmediately,
                );
                center
            };
            Some(Self {
                center,
                token,
                dirty,
                cached: std::cell::Cell::new(None),
            })
        }

        pub(super) fn dark(&self) -> Option<bool> {
            if self.dirty.swap(false, std::sync::atomic::Ordering::AcqRel) {
                self.cached.set(system_dark());
            }
            self.cached.get()
        }
    }

    impl Drop for Observer {
        fn drop(&mut self) {
            // SAFETY: These retained objects are the registration made by new.
            unsafe { self.center.removeObserver(&self.token) };
        }
    }

    pub(super) fn system_dark() -> Option<bool> {
        use objc2_app_kit::NSApplication;
        use objc2_foundation::NSArray;
        let mtm = MainThreadMarker::new()?;
        // Read the application's inherited appearance independently of NSWindow's forced palette.
        let appearance = NSApplication::sharedApplication(mtm).effectiveAppearance();
        let names = NSArray::from_id_slice(&[
            ns_string!("NSAppearanceNameAqua").to_owned(),
            ns_string!("NSAppearanceNameDarkAqua").to_owned(),
        ]);
        appearance
            .bestMatchFromAppearancesWithNames(&names)
            .map(|name| *name == *ns_string!("NSAppearanceNameDarkAqua"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_system_observer_keeps_inactive_windows_in_sync() {
        let source = kettle_test_support::production_source(include_str!("appearance.rs"));
        assert!(source.contains("NSNotificationSuspensionBehavior::DeliverImmediately"));
        assert!(source.contains("NSApplication::sharedApplication(mtm).effectiveAppearance()"));
        assert!(source.contains("self.center.removeObserver(&self.token)"));
        assert!(!source.contains("window.theme()"));
    }

    #[test]
    fn color_scheme_subscription_filters_namespace_and_key() {
        assert!(portal_setting_matches(
            "org.freedesktop.appearance",
            "color-scheme",
            "color-scheme"
        ));
        assert!(!portal_setting_matches(
            "org.freedesktop.appearance",
            "reduced-motion",
            "color-scheme"
        ));
        assert!(!portal_setting_matches(
            "org.example.appearance",
            "color-scheme",
            "color-scheme"
        ));
        let source = include_str!("portal_settings.rs");
        assert!(source.contains(
            ".receive_signal_with_args(\"SettingChanged\", &[(0, NAMESPACE), (1, key)])"
        ));
        assert!(
            source.find(".receive_signal_with_args(").unwrap()
                < source.find(".call_method(").unwrap()
        );
        assert!(source.contains(".max_queued(1)"));
        assert!(source.contains(".method_timeout(TIMEOUT)"));
    }

    #[test]
    fn portal_color_scheme_mapping_falls_back_to_dark() {
        assert!(!portal_dark(Some(2)));
        for value in [None, Some(0), Some(1), Some(3), Some(u32::MAX)] {
            assert!(portal_dark(value), "{value:?}");
        }
    }
}
