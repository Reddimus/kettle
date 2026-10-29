//! Preserve logical client geometry across native fullscreen transitions.

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq)]
struct Frame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[cfg(any(target_os = "macos", test))]
impl Frame {
    fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .into_iter()
            .all(f64::is_finite)
            && self.width > 0.0
            && self.height > 0.0
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default)]
struct Geometry {
    windowed: Option<Frame>,
    suspended: bool,
    exit_pending: bool,
    exit_generation: Option<u64>,
    generation: u64,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq)]
enum Restore {
    Ignore,
    Resume(Option<Frame>),
}

#[cfg(any(target_os = "macos", test))]
impl Geometry {
    fn new(frame: Frame, fullscreen: bool) -> Self {
        Self {
            windowed: (!fullscreen && frame.valid()).then_some(frame),
            suspended: fullscreen,
            exit_pending: false,
            exit_generation: None,
            generation: 0,
        }
    }

    fn observe(&mut self, frame: Frame, fullscreen: bool) {
        if !self.suspended && !fullscreen && frame.valid() {
            self.windowed = Some(frame);
        }
    }

    fn enter(&mut self, frame: Option<Frame>) {
        // A failed native entry has no exit notification. The next entry
        // captures the current windowed frame instead of an earlier attempt.
        // A queued reentry must retain the frame across its unfinished exit.
        if self.windowed.is_some()
            && !self.exit_pending
            && let Some(frame) = frame.filter(|frame| frame.valid())
        {
            self.windowed = Some(frame);
        }
        self.suspended = true;
        self.exit_pending = false;
        self.generation = self.generation.wrapping_add(1);
    }

    fn holds_resize(&self, fullscreen: bool) -> bool {
        self.exit_pending && !fullscreen
    }

    fn will_exit(&mut self) {
        if self.suspended {
            self.exit_pending = true;
            self.exit_generation = Some(self.generation);
        }
    }

    fn exited(&mut self) -> u64 {
        let generation = self.exit_generation.take().unwrap_or(self.generation);
        if self.suspended && generation == self.generation {
            self.exit_pending = true;
        }
        generation
    }

    fn finish(&mut self, generation: u64, fullscreen: bool) -> Restore {
        if !self.suspended || !self.exit_pending || generation != self.generation || fullscreen {
            return Restore::Ignore;
        }
        self.suspended = false;
        self.exit_pending = false;
        Restore::Resume(self.windowed)
    }
}

#[derive(Default)]
pub(crate) struct NativeFullscreen {
    #[cfg(target_os = "macos")]
    geometry: std::rc::Rc<std::cell::RefCell<Geometry>>,
    #[cfg(target_os = "macos")]
    native: Option<objc2::rc::Retained<objc2_app_kit::NSWindow>>,
    #[cfg(target_os = "macos")]
    center: Option<objc2::rc::Retained<objc2_foundation::NSNotificationCenter>>,
    #[cfg(target_os = "macos")]
    observers: Vec<objc2::rc::Retained<objc2_foundation::NSObject>>,
}

// Fullscreen gives borderless windows a temporary caption. Store client
// geometry so those extra pixels cannot survive the restored style.
#[cfg(target_os = "macos")]
fn frame(native: &objc2_app_kit::NSWindow) -> Frame {
    let rect = native.contentRectForFrameRect(native.frame());
    Frame {
        x: rect.origin.x,
        y: rect.origin.y,
        width: rect.size.width,
        height: rect.size.height,
    }
}

#[cfg(target_os = "macos")]
fn fullscreen(native: &objc2_app_kit::NSWindow) -> bool {
    native
        .styleMask()
        .contains(objc2_app_kit::NSWindowStyleMask::FullScreen)
}

impl NativeFullscreen {
    pub(crate) fn install(
        window: &winit::window::Window,
        window_seq: u64,
        proxy: winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
    ) -> Self {
        #[cfg(target_os = "macos")]
        {
            use block2::RcBlock;
            use objc2_app_kit::{
                NSView, NSWindowDidExitFullScreenNotification, NSWindowDidMoveNotification,
                NSWindowDidResizeNotification, NSWindowWillEnterFullScreenNotification,
                NSWindowWillExitFullScreenNotification,
            };
            use objc2_foundation::{MainThreadMarker, NSNotification, NSNotificationCenter};
            use std::ptr::NonNull;
            use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

            let mut state = Self::default();
            let Some(_mtm) = MainThreadMarker::new() else {
                return state;
            };
            let Ok(handle) = window.window_handle() else {
                return state;
            };
            let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
                return state;
            };
            // SAFETY: the live winit view is accessed on its main thread.
            let view = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
            let Some(native) = view.window() else {
                return state;
            };
            *state.geometry.borrow_mut() = Geometry::new(
                frame(&native),
                window.fullscreen().is_some() || fullscreen(&native),
            );
            let geometry = state.geometry.clone();
            let observed = native.clone();
            let changed = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                geometry
                    .borrow_mut()
                    .observe(frame(&observed), fullscreen(&observed));
            });
            let geometry = state.geometry.clone();
            let entering_native = native.clone();
            let entering = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                let windowed = (!fullscreen(&entering_native)).then(|| frame(&entering_native));
                geometry.borrow_mut().enter(windowed);
            });
            let geometry = state.geometry.clone();
            let leaving = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                geometry.borrow_mut().will_exit();
            });
            let geometry = state.geometry.clone();
            let exited = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                // Winit restores its style and zoom state in the delegate. Run
                // after that callback so it cannot overwrite our saved frame.
                let generation = geometry.borrow_mut().exited();
                let _ = proxy.send_event(crate::app::UserEvent::NativeFullscreenExited {
                    window_seq,
                    generation,
                });
            });
            // SAFETY: The default notification center is queried on the AppKit main thread.
            let center = unsafe { NSNotificationCenter::defaultCenter() };
            // SAFETY: These AppKit notification names are immutable, process-lifetime framework
            // globals.
            for name in unsafe { [NSWindowDidMoveNotification, NSWindowDidResizeNotification] } {
                // SAFETY: This window posts on the main thread; the center copies changed and
                // the token is removed on drop.
                state.observers.push(unsafe {
                    center.addObserverForName_object_queue_usingBlock(
                        Some(name),
                        Some(&native),
                        None,
                        &changed,
                    )
                });
            }
            // SAFETY: This window posts on the main thread; the center copies entering and the
            // token is removed on drop.
            state.observers.push(unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWindowWillEnterFullScreenNotification),
                    Some(&native),
                    None,
                    &entering,
                )
            });
            // SAFETY: This window posts on the main thread; the center copies exited and the
            // token is removed on drop.
            state.observers.push(unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWindowDidExitFullScreenNotification),
                    Some(&native),
                    None,
                    &exited,
                )
            });
            // SAFETY: This window posts on the main thread; the center copies leaving and the
            // token is removed on drop.
            state.observers.push(unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWindowWillExitFullScreenNotification),
                    Some(&native),
                    None,
                    &leaving,
                )
            });
            state.center = Some(center);
            state.native = Some(native);
            state
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (window, window_seq, proxy);
            Self::default()
        }
    }

    pub(crate) fn pending(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.native
                .as_ref()
                .is_some_and(|native| self.geometry.borrow().holds_resize(fullscreen(native)))
        }
        #[cfg(not(target_os = "macos"))]
        false
    }

    pub(crate) fn finish(&self, generation: u64, fullscreen_now: bool) -> bool {
        #[cfg(target_os = "macos")]
        {
            use objc2_foundation::{NSPoint, NSRect, NSSize};
            let Some(native) = &self.native else {
                return false;
            };
            let restore = self
                .geometry
                .borrow_mut()
                .finish(generation, fullscreen_now);
            let Restore::Resume(saved) = restore else {
                return false;
            };
            if let Some(saved) = saved
                && saved != frame(native)
            {
                let content = NSRect::new(
                    NSPoint::new(saved.x, saved.y),
                    NSSize::new(saved.width, saved.height),
                );
                // SAFETY: the retained window and validated logical rectangle
                // are accessed on the main thread after its native transition.
                let outer = unsafe { native.frameRectForContentRect(content) };
                native.setFrame_display(outer, true);
            }
            self.geometry
                .borrow_mut()
                .observe(frame(native), fullscreen_now);
            true
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (generation, fullscreen_now);
            false
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for NativeFullscreen {
    fn drop(&mut self) {
        if let Some(center) = &self.center {
            for observer in &self.observers {
                // SAFETY: these are the retained tokens installed above.
                unsafe { center.removeObserver(observer) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: f64, height: f64) -> Frame {
        Frame {
            x: 222.0,
            y: 84.0,
            width,
            height,
        }
    }

    #[test]
    fn a_native_exit_keeps_the_last_resized_frame_not_the_startup_frame() {
        let mut geometry = Geometry::new(frame(579.0, 431.0), false);
        geometry.observe(frame(740.0, 480.0), false);
        geometry.enter(None);
        geometry.observe(frame(1024.0, 768.0), true);
        geometry.observe(frame(579.0, 431.0), false);
        geometry.exited();
        assert_eq!(
            geometry.finish(1, false),
            Restore::Resume(Some(frame(740.0, 480.0)))
        );
        geometry.exited();
        assert_eq!(
            geometry.finish(1, false),
            Restore::Ignore,
            "restore is consumed once"
        );
        assert!(!geometry.holds_resize(false));
    }

    #[test]
    fn fullscreen_startup_leaves_the_native_restored_frame_alone() {
        let mut geometry = Geometry::new(frame(1024.0, 768.0), true);
        // A startup attribute requests fullscreen before AppKit sets its style bit.
        geometry.enter(Some(frame(579.0, 431.0)));
        geometry.exited();
        assert_eq!(geometry.finish(1, false), Restore::Resume(None));
        geometry.observe(frame(900.0, 700.0), false);
        geometry.enter(None);
        geometry.exited();
        assert_eq!(
            geometry.finish(2, false),
            Restore::Resume(Some(frame(900.0, 700.0)))
        );
    }

    #[test]
    fn an_old_exit_cannot_override_a_new_fullscreen_transition() {
        let mut geometry = Geometry::new(frame(740.0, 480.0), false);
        geometry.enter(None);
        geometry.enter(None);
        geometry.exited();
        assert_eq!(geometry.finish(1, false), Restore::Ignore);
        assert_eq!(geometry.finish(2, true), Restore::Ignore);
        assert!(geometry.suspended);
        geometry.exited();
        assert_eq!(
            geometry.finish(2, false),
            Restore::Resume(Some(frame(740.0, 480.0)))
        );
    }

    #[test]
    fn failed_entry_does_not_hold_resizes_or_restore_an_old_attempt() {
        let mut geometry = Geometry::new(frame(579.0, 431.0), false);
        geometry.enter(None);
        assert!(!geometry.exit_pending);
        // AppKit can reject entry without posting DidExitFullScreen.
        let resized = frame(740.0, 480.0);
        geometry.enter(Some(resized));
        let generation = geometry.exited();
        assert_eq!(
            geometry.finish(generation, false),
            Restore::Resume(Some(resized))
        );
    }

    #[test]
    fn entry_preserves_a_moved_or_maximized_logical_frame_without_rounding() {
        let mut geometry = Geometry::new(frame(579.0, 431.0), false);
        let wanted = Frame {
            x: -903.5,
            y: 67.25,
            width: 1120.5,
            height: 810.25,
        };
        geometry.enter(Some(wanted));
        geometry.observe(frame(1920.0, 1080.0), true);
        let generation = geometry.exited();
        assert_eq!(
            geometry.finish(generation, false),
            Restore::Resume(Some(wanted))
        );
    }

    #[test]
    fn exit_preparation_holds_the_stale_frame_before_completion() {
        let wanted = frame(740.0, 480.0);
        let mut geometry = Geometry::new(wanted, false);
        geometry.enter(None);
        geometry.will_exit();
        assert!(geometry.holds_resize(false));
        assert!(
            !geometry.holds_resize(true),
            "a rejected exit retains native fullscreen"
        );
        geometry.observe(frame(579.0, 431.0), false);
        let generation = geometry.exited();
        assert_eq!(
            geometry.finish(generation, false),
            Restore::Resume(Some(wanted))
        );
    }

    #[test]
    fn a_nested_entry_cannot_relabel_the_previous_exit_or_save_its_stale_frame() {
        let wanted = frame(740.0, 480.0);
        let mut geometry = Geometry::new(wanted, false);
        geometry.enter(None);
        geometry.will_exit();
        // Winit can start a queued entry in its DidExit delegate, before
        // notification-center observers receive that previous exit.
        geometry.enter(Some(frame(579.0, 431.0)));
        let old_exit = geometry.exited();
        assert_eq!(old_exit, 1);
        assert!(!geometry.exit_pending);
        assert_eq!(geometry.finish(old_exit, false), Restore::Ignore);
        geometry.will_exit();
        let new_exit = geometry.exited();
        assert_eq!(new_exit, 2);
        assert_eq!(
            geometry.finish(new_exit, false),
            Restore::Resume(Some(wanted))
        );
    }

    #[test]
    fn duplicate_exit_notifications_cannot_hold_future_windowed_resizes() {
        let wanted = frame(740.0, 480.0);
        let mut geometry = Geometry::new(wanted, false);
        geometry.enter(None);
        geometry.will_exit();
        let generation = geometry.exited();
        assert_eq!(
            geometry.finish(generation, false),
            Restore::Resume(Some(wanted))
        );
        geometry.will_exit();
        assert!(!geometry.holds_resize(false));
        let duplicate = geometry.exited();
        assert_eq!(geometry.finish(duplicate, false), Restore::Ignore);
        assert!(!geometry.holds_resize(false));
        let resized = frame(820.0, 530.0);
        geometry.observe(resized, false);
        geometry.enter(None);
        geometry.will_exit();
        let generation = geometry.exited();
        assert_eq!(
            geometry.finish(generation, false),
            Restore::Resume(Some(resized))
        );
    }

    #[test]
    fn degenerate_and_nonfinite_resizes_do_not_replace_the_windowed_frame() {
        let wanted = frame(740.0, 480.0);
        let mut geometry = Geometry::new(wanted, false);
        for invalid in [
            frame(0.0, 0.0),
            frame(f64::INFINITY, 512.0),
            Frame {
                x: f64::NAN,
                ..wanted
            },
        ] {
            geometry.observe(invalid, false);
        }
        geometry.enter(None);
        geometry.exited();
        assert_eq!(geometry.finish(1, false), Restore::Resume(Some(wanted)));
    }
}
