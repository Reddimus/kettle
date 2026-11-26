//! System motion preferences and explicit user overrides.

/// A desktop setting as the Settings portal last reported it.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reading<T> {
    /// No answer yet: the watcher is still connecting or reading.
    Pending,
    /// No answer to be had: no portal, a portal without the key, a value of
    /// the wrong type, or a portal that went away.
    Unavailable,
    Value(T),
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Preference {
    /// The desktop has not answered yet, and motion waits for it. Each
    /// portal call gives up after two seconds.
    #[default]
    Pending,
    /// The desktop has no motion preference, so motion is allowed.
    Unknown,
    Normal,
    Reduced,
}

#[cfg(any(target_os = "linux", test))]
impl Preference {
    pub(crate) fn from_portal(value: u32) -> Self {
        if value == 1 {
            Self::Reduced
        } else {
            Self::Normal
        }
    }

    /// The portal's `reduced-motion`, or where the portal has none, GNOME's
    /// `enable-animations`, which older GNOME portals do not translate.
    pub(crate) fn resolve(portal: Reading<u32>, gnome: impl FnOnce() -> Reading<bool>) -> Self {
        match portal {
            Reading::Value(value) => Self::from_portal(value),
            Reading::Pending => Self::Pending,
            Reading::Unavailable => match gnome() {
                Reading::Value(true) => Self::Normal,
                Reading::Value(false) => Self::Reduced,
                Reading::Pending => Self::Pending,
                Reading::Unavailable => Self::Unknown,
            },
        }
    }

    fn allows(self) -> bool {
        matches!(self, Self::Normal | Self::Unknown)
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
    watch: crate::portal_settings::Watch,
    #[cfg(target_os = "linux")]
    buttons: crate::portal_settings::ButtonWatch,
    /// GNOME's `enable-animations`, watched once the portal turns out to
    /// have no `reduced-motion`.
    #[cfg(target_os = "linux")]
    animations: std::sync::OnceLock<crate::portal_settings::AnimationsWatch>,
    #[cfg(target_os = "linux")]
    wake: kettle_core::Waker,
}

impl SystemMotion {
    pub(crate) fn new(wake: kettle_core::Waker) -> Self {
        #[cfg(target_os = "linux")]
        {
            Self {
                watch: crate::portal_settings::Watch::start(wake.clone(), None, "reduced-motion"),
                buttons: crate::portal_settings::ButtonWatch::start(
                    wake.clone(),
                    None,
                    "button-layout",
                ),
                animations: std::sync::OnceLock::new(),
                wake,
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = wake;
            Self {}
        }
    }

    /// Start watching what `allows` will read: GNOME's setting, as soon as
    /// the portal has said it has no `reduced-motion`, so the first motion
    /// after that does not wait for GNOME's answer.
    pub(crate) fn prepare(&self) {
        #[cfg(target_os = "linux")]
        if self.watch.reading() == Reading::Unavailable {
            self.animations();
        }
    }

    #[cfg(target_os = "linux")]
    fn animations(&self) -> &crate::portal_settings::AnimationsWatch {
        self.animations.get_or_init(|| {
            crate::portal_settings::AnimationsWatch::start(
                self.wake.clone(),
                None,
                "enable-animations",
            )
        })
    }

    pub(crate) fn button_layout(&self) -> crate::window_buttons::ButtonLayout {
        #[cfg(target_os = "linux")]
        {
            self.buttons.button_layout()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Default::default()
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
            Preference::resolve(self.watch.reading(), || self.animations().reading()).allows()
        }
        #[cfg(windows)]
        {
            use windows::Win32::UI::WindowsAndMessaging::{
                SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
                SystemParametersInfoW,
            };
            let mut enabled = windows::core::BOOL(0);
            // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL into this live output buffer.
            unsafe {
                SystemParametersInfoW(
                    SPI_GETCLIENTAREAANIMATION,
                    0,
                    Some((&mut enabled as *mut windows::core::BOOL).cast()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
            }
            .is_ok()
                && enabled.as_bool()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            false
        }
    }
}

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

    /// A desktop that has no motion preference allows motion. Only an
    /// answer still on its way holds motion back.
    #[test]
    fn a_desktop_without_a_motion_preference_allows_motion() {
        assert!(Preference::Unknown.allows());
        assert!(Preference::Normal.allows());
        assert!(!Preference::Reduced.allows());
        assert!(!Preference::Pending.allows());
        assert_eq!(Preference::default(), Preference::Pending);
        let none = || Reading::Unavailable;
        assert_eq!(
            Preference::resolve(Reading::Unavailable, none),
            Preference::Unknown
        );
        assert!(allowed(kettle_config::ReduceMotion::Auto, || {
            Preference::resolve(Reading::Unavailable, none).allows()
        }));
        assert!(allowed(kettle_config::ReduceMotion::Auto, || {
            Preference::from_portal(0).allows()
        }));
        assert!(!allowed(kettle_config::ReduceMotion::Auto, || {
            Preference::from_portal(1).allows()
        }));
    }

    /// The portal's `reduced-motion` decides when it answers; where it has
    /// none, GNOME's `enable-animations` does, and GNOME is not asked first.
    #[test]
    fn gnome_animations_stand_in_for_a_portal_without_reduced_motion() {
        let unasked = || -> Reading<bool> { panic!("GNOME was asked") };
        assert_eq!(
            Preference::resolve(Reading::Value(1), unasked),
            Preference::Reduced
        );
        assert_eq!(
            Preference::resolve(Reading::Value(0), unasked),
            Preference::Normal
        );
        assert_eq!(
            Preference::resolve(Reading::Pending, unasked),
            Preference::Pending
        );
        for (gnome, expected) in [
            (Reading::Value(false), Preference::Reduced),
            (Reading::Value(true), Preference::Normal),
            (Reading::Pending, Preference::Pending),
            (Reading::Unavailable, Preference::Unknown),
        ] {
            assert_eq!(
                Preference::resolve(Reading::Unavailable, || gnome),
                expected,
                "{gnome:?}"
            );
        }
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
