//! System motion preferences and explicit user overrides.

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Preference {
    #[default]
    Unknown,
    Normal,
    Reduced,
}

#[cfg(any(target_os = "linux", test))]
impl Preference {
    fn from_portal(value: u32) -> Self {
        if value == 1 {
            Self::Reduced
        } else {
            Self::Normal
        }
    }

    fn allows(self) -> bool {
        self == Self::Normal
    }
}

pub(crate) fn allowed(mode: kettle_config::ReduceMotion, system: impl FnOnce() -> bool) -> bool {
    match mode {
        kettle_config::ReduceMotion::On => false,
        kettle_config::ReduceMotion::Off => true,
        kettle_config::ReduceMotion::Auto => system(),
    }
}

pub(crate) struct SystemMotion {
    #[cfg(target_os = "linux")]
    watch: linux::Watch,
}

impl SystemMotion {
    pub(crate) fn new(wake: kettle_core::Waker) -> Self {
        #[cfg(target_os = "linux")]
        {
            Self {
                watch: linux::Watch::start(wake, None),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = wake;
            Self {}
        }
    }

    pub(crate) fn allows(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            use objc2_app_kit::NSWorkspace;
            if objc2_foundation::MainThreadMarker::new().is_some() {
                // SAFETY: read the shared workspace only on its main thread.
                return !unsafe {
                    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
                };
            }
            false
        }
        #[cfg(target_os = "linux")]
        {
            self.watch.preference().allows()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            false
        }
    }
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_values_follow_the_standard_motion_preference() {
        assert_eq!(Preference::from_portal(0), Preference::Normal);
        assert_eq!(Preference::from_portal(1), Preference::Reduced);
        for value in [2, 3, u32::MAX] {
            assert_eq!(Preference::from_portal(value), Preference::Normal);
        }
    }

    #[test]
    fn an_unavailable_system_does_not_start_motion() {
        assert!(!Preference::default().allows());
        assert!(allowed(kettle_config::ReduceMotion::Auto, || {
            Preference::from_portal(0).allows()
        }));
        assert!(!allowed(kettle_config::ReduceMotion::Auto, || {
            Preference::from_portal(1).allows()
        }));
    }

    #[test]
    fn explicit_overrides_do_not_query_the_system() {
        assert!(!allowed(kettle_config::ReduceMotion::On, || panic!(
            "queried"
        )));
        assert!(allowed(kettle_config::ReduceMotion::Off, || panic!(
            "queried"
        )));
    }
}
