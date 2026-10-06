//! Observe native caption drags without replacing AppKit's window delegate.

pub(crate) fn may_dock_caption(
    detachable: bool,
    tab_bar_enabled: bool,
    tabs: usize,
    pointer_modal_open: bool,
) -> bool {
    detachable && tab_bar_enabled && tabs == 1 && !pointer_modal_open
}

#[cfg(any(target_os = "macos", test))]
fn is_caption_press(
    same_window: bool,
    primary_held: bool,
    single_click: bool,
    pointer_event: bool,
    point: (f64, f64),
    caption: (f64, f64, f64),
) -> bool {
    let (width, content_top, frame_top) = caption;
    same_window
        && primary_held
        && single_click
        && pointer_event
        && point.0 >= 0.0
        && point.0 < width
        && point.1 >= content_top
        && point.1 < frame_top
}

#[derive(Default)]
pub(crate) struct NativeCaptionDrag {
    #[cfg(target_os = "macos")]
    pending: std::rc::Rc<std::cell::Cell<Option<std::time::Instant>>>,
    #[cfg(target_os = "macos")]
    cancelled: std::rc::Rc<std::cell::Cell<bool>>,
    #[cfg(target_os = "macos")]
    center: Option<objc2::rc::Retained<objc2_foundation::NSNotificationCenter>>,
    #[cfg(target_os = "macos")]
    observers: Vec<objc2::rc::Retained<objc2_foundation::NSObject>>,
}

impl NativeCaptionDrag {
    pub(crate) fn install(window: &winit::window::Window) -> Self {
        #[cfg(target_os = "macos")]
        {
            use block2::RcBlock;
            use objc2_app_kit::{
                NSApplication, NSEvent, NSEventType, NSView, NSWindowDidMoveNotification,
                NSWindowWillMoveNotification,
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
            // SAFETY: the live winit content view is accessed on its main thread.
            let view = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
            let Some(native) = view.window() else {
                return state;
            };
            let pending = state.pending.clone();
            let cancelled = state.cancelled.clone();
            let native_for_start = native.clone();
            let start = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                let Some(mtm) = MainThreadMarker::new() else {
                    return;
                };
                let Some(event) = NSApplication::sharedApplication(mtm).currentEvent() else {
                    return;
                };
                // SAFETY: AppKit posts this window-scoped notification on the
                // main thread; the window and current event are retained here.
                let qualifies = unsafe {
                    let point = event.locationInWindow();
                    let content = native_for_start.contentLayoutRect();
                    let frame = native_for_start.frame();
                    is_caption_press(
                        event.windowNumber() == native_for_start.windowNumber(),
                        NSEvent::pressedMouseButtons() & 1 != 0,
                        event.clickCount() <= 1,
                        matches!(
                            event.r#type(),
                            NSEventType::LeftMouseDown | NSEventType::LeftMouseDragged
                        ),
                        (point.x, point.y),
                        (
                            frame.size.width,
                            content.origin.y + content.size.height,
                            frame.size.height,
                        ),
                    )
                };
                if qualifies {
                    cancelled.set(false);
                    pending.set(Some(std::time::Instant::now()));
                }
            });
            let cancelled = state.cancelled.clone();
            let moved = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                let Some(mtm) = MainThreadMarker::new() else {
                    return;
                };
                if let Some(event) = NSApplication::sharedApplication(mtm).currentEvent() {
                    // AppKit can consume Escape inside its move loop before
                    // winit delivers the restored-position Moved event.
                    if unsafe { event.r#type() == NSEventType::KeyDown && event.keyCode() == 53 } {
                        cancelled.set(true);
                    }
                }
            });
            // SAFETY: no operation queue means main-thread delivery here. The
            // observers are scoped to this window and removed before it drops.
            let center = unsafe { NSNotificationCenter::defaultCenter() };
            state.observers.push(unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWindowWillMoveNotification),
                    Some(&native),
                    None,
                    &start,
                )
            });
            state.observers.push(unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWindowDidMoveNotification),
                    Some(&native),
                    None,
                    &moved,
                )
            });
            state.center = Some(center);
            state
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window;
            Self::default()
        }
    }

    pub(crate) fn take_start(&self) -> Option<std::time::Instant> {
        #[cfg(target_os = "macos")]
        {
            let start = self.pending.take();
            start.filter(|_| !self.cancelled.get())
        }
        #[cfg(not(target_os = "macos"))]
        None
    }

    pub(crate) fn cancelled(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            use objc2_core_graphics_03::{CGEventSource, CGEventSourceStateID};
            self.cancelled.get()
                || CGEventSource::key_state(CGEventSourceStateID::CombinedSessionState, 53)
        }
        #[cfg(not(target_os = "macos"))]
        false
    }
}

#[cfg(target_os = "macos")]
impl Drop for NativeCaptionDrag {
    fn drop(&mut self) {
        if let Some(center) = &self.center {
            for observer in &self.observers {
                // SAFETY: these are the retained tokens installed above.
                unsafe { center.removeObserver(observer) };
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn cursor_position() -> Option<(f64, f64)> {
    use objc2_core_graphics_03::CGEvent;
    let event = CGEvent::new(None)?;
    let point = CGEvent::location(Some(&event));
    Some((point.x, point.y))
}

#[cfg(target_os = "macos")]
pub(crate) fn primary_held() -> bool {
    use objc2_core_graphics_03::{CGEventSource, CGEventSourceStateID, CGMouseButton};
    CGEventSource::button_state(
        CGEventSourceStateID::CombinedSessionState,
        CGMouseButton::Left,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_drag_only_docks_a_single_tab_with_detaching_enabled() {
        assert!(may_dock_caption(true, true, 1, false));
        for input in [
            (false, true, 1, false),
            (true, false, 1, false),
            (true, true, 0, false),
            (true, true, 2, false),
            (true, true, 1, true),
        ] {
            assert!(!may_dock_caption(input.0, input.1, input.2, input.3));
        }
    }

    #[test]
    fn genuine_single_caption_press_qualifies() {
        assert!(is_caption_press(
            true,
            true,
            true,
            true,
            (120.0, 610.0),
            (800.0, 600.0, 628.0)
        ));
    }

    #[test]
    fn programmatic_move_other_window_or_released_pointer_does_not_qualify() {
        for input in [
            (false, true, true, true),
            (true, false, true, true),
            (true, true, true, false),
            (true, true, false, true),
        ] {
            assert!(!is_caption_press(
                input.0,
                input.1,
                input.2,
                input.3,
                (120.0, 610.0),
                (800.0, 600.0, 628.0)
            ));
        }
    }

    #[test]
    fn client_resize_edge_and_outside_caption_do_not_qualify() {
        for point in [
            (120.0, 599.0),
            (120.0, 628.0),
            (-1.0, 610.0),
            (800.0, 610.0),
            (f64::NAN, 610.0),
        ] {
            assert!(!is_caption_press(
                true,
                true,
                true,
                true,
                point,
                (800.0, 600.0, 628.0)
            ));
        }
    }
}
