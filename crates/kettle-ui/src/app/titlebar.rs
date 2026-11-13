//! Shared titlebar measurements and gestures. All layout values are physical pixels.
use super::*;

#[cfg(any(test, target_os = "macos"))]
pub(super) fn macos_view_frame_size(
    frame: winit::dpi::LogicalSize<f64>,
    backing_scale: f64,
) -> winit::dpi::PhysicalSize<u32> {
    frame.to_physical(backing_scale)
}

#[cfg(target_os = "macos")]
pub(super) fn macos_view_size(window: &Window) -> Option<winit::dpi::PhysicalSize<u32>> {
    native::view_size(window)
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TitlebarBand {
    pub height: f32,
    pub left: f32,
}

pub(super) fn titlebar_band_metrics(
    scale: f32,
    height: Option<f32>,
    buttons_right: Option<f32>,
    fullscreen: bool,
) -> TitlebarBand {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    TitlebarBand {
        height: height
            .filter(|h| h.is_finite() && (20.0..=128.0).contains(h))
            .unwrap_or(28.0)
            .max(28.0)
            * scale,
        left: if fullscreen {
            0.0
        } else {
            buttons_right
                .filter(|x| x.is_finite() && (0.0..=256.0).contains(x))
                .map_or(78.0, |x| x + 8.0)
                * scale
        },
    }
}

pub(super) fn inset_titlebar_band(band: Rect, titlebar: Option<TitlebarBand>) -> Rect {
    titlebar.map_or(band, |t| {
        let left = t.left.min(band.2);
        (band.0 + left, band.1, (band.2 - left).max(0.0), t.height)
    })
}

pub(super) fn empty_titlebar_space(bar: &TabBar, x: f32, y: f32) -> bool {
    rect_contains(bar.band, x, y)
        && window_control_at(bar, x, y).is_none()
        && !bar.segments.iter().any(|s| rect_contains(s.rect, x, y))
        && ![
            bar.new_tab,
            bar.new_tab_menu,
            bar.settings,
            bar.scroll_left,
            bar.scroll_right,
        ]
        .into_iter()
        .any(|r| rect_contains(r, x, y))
}

impl App {
    /// Reconcile estimated startup chrome while the new window is still hidden.
    pub(super) fn correct_startup_titlebar_size(
        &self,
        window: &Window,
        estimated: Option<TitlebarBand>,
        monitor: Option<StartupMonitor>,
    ) {
        let size = window.inner_size();
        #[cfg(target_os = "macos")]
        let corrected = estimated.map(|estimated| {
            let measured = native::measure(window);
            startup_surface_with_measured_titlebar(
                &self.cfg,
                (size.width, size.height),
                estimated.height,
                measured.height,
                monitor,
            )
        });
        #[cfg(target_os = "linux")]
        let corrected = estimated
            .filter(|_| linux_custom_titlebar(&self.cfg, window))
            .map(|estimated| {
                linux_startup_surface_with_header(
                    &self.cfg,
                    (size.width, size.height),
                    estimated.height,
                    window.scale_factor() as f32,
                    monitor,
                )
            });
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let corrected: Option<(u32, u32)> = {
            let _ = (estimated, monitor);
            None
        };
        if let Some(corrected) = corrected
            && corrected != (size.width, size.height)
        {
            // AppKit and Wayland update winit's size before renderer creation.
            let _ =
                window.request_inner_size(winit::dpi::PhysicalSize::new(corrected.0, corrected.1));
        }
    }

    pub(super) fn sync_macos_titlebar(&self, ws: &mut WindowState) {
        #[cfg(target_os = "macos")]
        if let Some(window) = &ws.window {
            let tabs = window.is_decorated()
                && macos_titlebar_policy(&self.cfg) == MacosTitlebarPolicy::Tabs;
            native::set_tabs(window, tabs);
            ws.macos_titlebar_tabs = tabs;
        }
        #[cfg(target_os = "linux")]
        if let Some(window) = &ws.window {
            let custom = linux_custom_titlebar(&self.cfg, window);
            // Kettle's header replaces winit's frame; any other case keeps
            // winit's or the compositor's frame, including X11 sessions that
            // inherited WAYLAND_DISPLAY and config reloads either way.
            if !self.cfg.borderless && window.is_decorated() == custom {
                window.set_decorations(!custom);
            }
            ws.linux_titlebar = custom;
            ws.macos_titlebar_tabs = custom;
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = ws;
    }

    pub(super) fn macos_titlebar_band(ws: &WindowState) -> Option<TitlebarBand> {
        if !ws.macos_titlebar_tabs {
            return None;
        }
        let scale = ws
            .window
            .as_ref()
            .map_or(ws.startup_scale, |w| w.scale_factor() as f32);
        if ws.linux_titlebar {
            return Some(TitlebarBand {
                height: LINUX_HEADER_HEIGHT * scale,
                left: 0.0,
            });
        }
        #[cfg(target_os = "macos")]
        if let Some(window) = &ws.window {
            return Some(native::measure(window));
        }
        Some(titlebar_band_metrics(scale, None, None, false))
    }

    pub(super) fn tab_strip_height(&self, ws: &WindowState) -> f32 {
        Self::macos_titlebar_band(ws).map_or_else(
            || {
                let scale = ws.window.as_ref().map_or(1.0, |w| w.scale_factor() as f32);
                tab_bar_height(
                    Self::cell_size(ws).map_or(16.0, |(_, ch)| ch),
                    self.cfg.tab_bar_pos,
                )
                .max(
                    if self.cfg.tab_bar_pos.is_vertical() {
                        20.0
                    } else {
                        28.0
                    } * scale,
                )
            },
            |band| band.height,
        )
    }

    pub(super) fn press_titlebar_space(
        &self,
        ws: &mut WindowState,
        bar: &TabBar,
        x: f32,
        y: f32,
    ) -> bool {
        if !ws.macos_titlebar_tabs || !empty_titlebar_space(bar, x, y) {
            ws.titlebar_click = None;
            return false;
        }
        #[cfg(target_os = "macos")]
        if let Some(window) = &ws.window {
            let now = std::time::Instant::now();
            let interval = native::double_click_interval();
            let scale = window.scale_factor() as f32;
            let double = ws.titlebar_click.is_some_and(|(t, px, py)| {
                now.duration_since(t).as_secs_f64() <= interval
                    && (x - px).abs() <= 4.0 * scale
                    && (y - py).abs() <= 4.0 * scale
            });
            ws.titlebar_click = if double { None } else { Some((now, x, y)) };
            if double {
                native::double_click(window);
            } else if let Err(error) = window.drag_window() {
                log::debug!("titlebar window drag unavailable: {error}");
            }
        }
        true
    }
}

#[cfg(any(test, target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TitlebarDoubleClick {
    Zoom,
    Minimize,
    None,
}

#[cfg(any(test, target_os = "macos"))]
pub(super) fn titlebar_double_click_action(value: Option<&str>) -> TitlebarDoubleClick {
    match value {
        Some(v) if v.eq_ignore_ascii_case("Minimize") => TitlebarDoubleClick::Minimize,
        Some(v) if v.eq_ignore_ascii_case("None") => TitlebarDoubleClick::None,
        _ => TitlebarDoubleClick::Zoom,
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use objc2_app_kit::{NSView, NSWindow, NSWindowStyleMask, NSWindowTitleVisibility};
    use objc2_foundation::{MainThreadMarker, NSString, NSUserDefaults};

    fn with_window<R>(window: &Window, f: impl FnOnce(&NSWindow, &NSView) -> R) -> Option<R> {
        use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
        let _mtm = MainThreadMarker::new()?;
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            return None;
        };
        // SAFETY: winit keeps this content view alive and the marker proves main-thread access.
        let view = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
        let native = view.window()?;
        Some(f(&native, view))
    }

    pub(super) fn view_size(window: &Window) -> Option<winit::dpi::PhysicalSize<u32>> {
        with_window(window, |native, view| {
            let frame = view.frame();
            macos_view_frame_size(
                winit::dpi::LogicalSize::new(frame.size.width, frame.size.height),
                native.backingScaleFactor(),
            )
        })
    }

    pub(super) fn set_tabs(window: &Window, tabs: bool) {
        with_window(window, |native, view| {
            let mut mask = native.styleMask();
            let visibility = if tabs {
                NSWindowTitleVisibility::NSWindowTitleHidden
            } else {
                NSWindowTitleVisibility::NSWindowTitleVisible
            };
            // SAFETY: This read-only query uses a live NSWindow on the main thread.
            let current_visibility = unsafe { native.titleVisibility() };
            if mask.contains(NSWindowStyleMask::FullSizeContentView) == tabs
                && current_visibility == visibility
            {
                return;
            }
            mask.set(NSWindowStyleMask::FullSizeContentView, tabs);
            native.setStyleMask(mask);
            native.setTitlebarAppearsTransparent(tabs);
            native.setTitleVisibility(if tabs {
                NSWindowTitleVisibility::NSWindowTitleHidden
            } else {
                NSWindowTitleVisibility::NSWindowTitleVisible
            });
            native.setMovableByWindowBackground(false);
            // Changing the style mask can drop the content view's keyboard focus.
            // Match winit's style setter by restoring its first responder.
            let _ = native.makeFirstResponder(Some(view));
        });
    }

    pub(super) fn measure(window: &Window) -> TitlebarBand {
        let fullscreen = window.fullscreen().is_some();
        let measurements = with_window(window, |native, view| {
            // SAFETY: AppKit geometry queries on live views on the main thread. The zoom button
            // is an NSButton (an NSView subclass), borrowed for the duration of this query.
            unsafe {
                let layout = native.contentLayoutRect();
                let height = native.frame().size.height - layout.origin.y - layout.size.height;
                let button: *const NSView = objc2::msg_send![native, standardWindowButton: 2usize];
                let right = button.as_ref().and_then(|button| {
                    button.superview().map(|parent| {
                        let frame = parent.convertRect_toView(button.frame(), Some(view));
                        (frame.origin.x + frame.size.width) as f32
                    })
                });
                (Some(height as f32), right)
            }
        })
        .unwrap_or((None, None));
        titlebar_band_metrics(
            window.scale_factor() as f32,
            measurements.0,
            measurements.1,
            fullscreen,
        )
    }

    pub(super) fn double_click_interval() -> f64 {
        // SAFETY: This is a read-only AppKit query on the event loop's main thread.
        unsafe { objc2_app_kit::NSEvent::doubleClickInterval() }
    }

    pub(super) fn double_click(window: &Window) {
        with_window(window, |native, _| {
            let key = NSString::from_str("AppleActionOnDoubleClick");
            // SAFETY: Read a property-list string from the user's defaults on the main thread.
            let value = unsafe { NSUserDefaults::standardUserDefaults().stringForKey(&key) };
            match titlebar_double_click_action(value.as_ref().map(|v| v.to_string()).as_deref()) {
                TitlebarDoubleClick::Minimize => native.miniaturize(None),
                TitlebarDoubleClick::Zoom => {
                    // SAFETY: Standard AppKit zoom action on a live main-thread window.
                    unsafe {
                        native.performZoom(None);
                    }
                }
                TitlebarDoubleClick::None => (),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titlebar_band_matches_appkit_or_scaled_fallback_and_clears_buttons() {
        for scale in [1.0, 1.5, 2.0, 3.0] {
            for (height, right, expected) in [
                (None, None, (28.0, 78.0)),
                (Some(32.0), Some(72.0), (32.0, 80.0)),
                (Some(22.0), Some(72.0), (28.0, 80.0)),
                (Some(f32::NAN), Some(-1.0), (28.0, 78.0)),
            ] {
                let titlebar = titlebar_band_metrics(scale, height, right, false);
                let surface = ((800.0 * scale) as u32, (600.0 * scale) as u32);
                let band = inset_titlebar_band(
                    tab_band_rect(
                        surface,
                        TabBarPos::Top,
                        titlebar.height,
                        180.0,
                        0.0,
                        0.0,
                        0.0,
                    ),
                    Some(titlebar),
                );
                assert_eq!(band.0, expected.1 * scale);
                assert_eq!(band.1, 0.0);
                assert_eq!(band.3, expected.0 * scale);
                assert_eq!(band.0 + band.2, surface.0 as f32);
                let content = content_rect_for_with_strip(
                    surface,
                    ContentBands {
                        tab_bar_h: titlebar.height,
                        ..Default::default()
                    },
                    TabBarPos::Top,
                    StatusBarMode::Off,
                    180.0,
                );
                assert_eq!(content.1, band.3);
                assert!(!rect_contains(band, band.0 - 1.0, band.3 * 0.5));
                assert!(!rect_contains(band, band.0 + 10.0, content.1));
                let full = titlebar_band_metrics(scale, height, right, true);
                assert_eq!(full.left, 0.0);
                assert_eq!(full.height, titlebar.height);
            }
        }
    }

    #[test]
    fn titlebar_single_tab_stays_visible_and_centres_title_with_hover_controls() {
        assert!(tab_strip_visible(TabBarMode::Auto, 1, false, false, true));
        assert!(!tab_strip_visible(TabBarMode::Auto, 1, false, false, false));
        assert!(!tab_strip_visible(TabBarMode::Off, 1, false, false, false));
        for scale in [1.0, 2.0] {
            for left in [78.0, 90.0] {
                let width = 800.0 * scale;
                let left = left * scale;
                let h = 28.0 * scale;
                let strip = width - left - 2.0 * h;
                let layout = titlebar_tab_layout(1, 0, (strip, 2.0 * h, left), h, 80.0, true);
                let rect = (left + layout.xs[0].unwrap(), 0.0, layout.width, h);
                for hovered in [false, true] {
                    let content = kettle_render::horizontal_tab_content_layout(
                        rect,
                        hovered,
                        true,
                        false,
                        "shell",
                        Some("Cmd+1"),
                        8.0 * scale,
                    );
                    let title = centered_title_lane(content.title, width * 0.5);
                    assert!((title.0 + title.2 * 0.5 - width * 0.5).abs() < 0.01);
                    assert!(title.0 >= left);
                    assert_eq!(content.hint.is_some(), hovered);
                    if let Some(hint) = content.hint {
                        assert!(title.0 + title.2 <= hint.0);
                        assert!(content.close.2 >= 20.0 * scale && content.close.3 >= 20.0 * scale);
                    } else {
                        assert_eq!(content.close.2, 0.0);
                        assert_eq!(content.close.3, 0.0);
                    }
                }
                assert!(
                    rect.0 + rect.2 < left + strip,
                    "room remains for native window drag"
                );
                let (arrow, plus) = split_new_tab_button((left + strip, 0.0, 2.0 * h, h), h);
                for target in [arrow, plus] {
                    assert!(target.2 / scale >= 20.0 && target.3 / scale >= 20.0);
                }
                let narrow = kettle_render::horizontal_tab_content_layout(
                    (left, 0.0, h, h),
                    true,
                    true,
                    false,
                    "shell",
                    Some("Cmd+1"),
                    8.0 * scale,
                );
                assert!(narrow.hint.is_none());
            }
        }
    }

    #[test]
    fn titlebar_empty_space_excludes_tabs_controls_and_traffic_lights() {
        let mut bar = TabBar::hidden();
        bar.height = 28.0;
        bar.band = (78.0, 0.0, 722.0, 28.0);
        bar.new_tab = (772.0, 0.0, 28.0, 28.0);
        bar.new_tab_menu = (744.0, 0.0, 28.0, 28.0);
        bar.segments.push(kettle_render::TabSeg {
            idx: 0,
            rect: (240.0, 0.0, 320.0, 28.0),
            title_rect: (250.0, 0.0, 280.0, 28.0),
            close: (532.0, 0.0, 28.0, 28.0),
            title: "shell".into(),
            path: None,
            hint: None,
            active: true,
            activity: RenderTabActivity::Normal,
        });
        for (x, expected) in [
            (40.0, false),
            (100.0, true),
            (300.0, false),
            (540.0, false),
            (600.0, true),
            (750.0, false),
            (780.0, false),
        ] {
            assert_eq!(empty_titlebar_space(&bar, x, 14.0), expected, "x={x}");
        }
        assert!(!empty_titlebar_space(&bar, 600.0, 28.0));
        assert_eq!(
            titlebar_double_click_action(None),
            TitlebarDoubleClick::Zoom
        );
        assert_eq!(
            titlebar_double_click_action(Some("Maximize")),
            TitlebarDoubleClick::Zoom
        );
        assert_eq!(
            titlebar_double_click_action(Some("Minimize")),
            TitlebarDoubleClick::Minimize
        );
        assert_eq!(
            titlebar_double_click_action(Some("None")),
            TitlebarDoubleClick::None
        );
    }
}
