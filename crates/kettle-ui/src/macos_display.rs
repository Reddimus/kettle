//! Read the same display size and scales winit uses, before `run_app`.

use crate::app::StartupMonitor;
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSScreen;
use objc2_core_graphics_03::{CGDisplayPixelsHigh, CGDisplayPixelsWide, CGMainDisplayID};
use objc2_foundation::{MainThreadMarker, NSNumber, ns_string};

pub(crate) fn startup_display() -> Option<StartupMonitor> {
    let mtm = MainThreadMarker::new()?;
    autoreleasepool(|_| {
        let display = CGMainDisplayID();
        let width = CGDisplayPixelsWide(display);
        let height = CGDisplayPixelsHigh(display);
        if display == 0 || width == 0 || height == 0 {
            return None;
        }
        let screen = NSScreen::screens(mtm).into_iter().find(|screen| {
            let description = screen.deviceDescription();
            let Some(number) = description.get(ns_string!("NSScreenNumber")) else {
                return false;
            };
            // SAFETY: AppKit defines NSScreenNumber as an NSNumber.
            let number = unsafe { &*(number as *const _ as *const NSNumber) };
            number.as_u32() == display
        });
        let scale = screen.map(|screen| screen.backingScaleFactor());
        let main_scale = NSScreen::mainScreen(mtm)?.backingScaleFactor();
        matching_display(width, height, scale, main_scale)
    })
}

// Absence of the CGMainDisplayID screen is not evidence of a 1x display.
fn matching_display(
    width: usize,
    height: usize,
    scale: Option<f64>,
    main_scale: f64,
) -> Option<StartupMonitor> {
    let scale = scale?;
    if !scale.is_finite() || scale <= 0.0 || scale != main_scale {
        return None;
    }
    Some(StartupMonitor::from_display_points(width, height, scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_primary_screen_declines_pre_launch() {
        assert!(matching_display(1920, 1080, None, 1.0).is_none());
        assert!(matching_display(1920, 1080, Some(1.0), 1.0).is_some());
        assert!(matching_display(1920, 1080, Some(2.0), 1.0).is_none());
    }
}
