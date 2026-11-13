/// Drift guard. `ScaledZoom` must baseline off the live
/// renderer size (`r.font_size()`), never `self.cfg.font_size` — the latter
/// is stale after any manual Increase/DecreaseFontSize (which only touch the
/// renderer), so scaling from it discards the user's manual zoom on exit.
/// A behavioral test needs a live renderer + mux; pin it at the source.
///
/// The scaling logic lives in the shared `toggle_zoom_with_scale` helper
/// (both `ToggleZoom` and `ScaledZoom` call it), so the guard checks the
/// helper body rather than the `Action::ScaledZoom` arm itself.
#[test]
fn scaled_zoom_baselines_off_live_font_size() {
    let src = production_source();
    let helper = src
        .split("fn toggle_zoom_with_scale(")
        .nth(1)
        .and_then(|s| s.split("\n    }\n").next())
        .expect("toggle_zoom_with_scale body present");
    // Strip line comments before the checks: the rationale prose in the
    // helper necessarily NAMES the `cfg.font_size` anti-pattern it warns
    // against, which would otherwise trip the negative assertion below.
    let code: String = helper
        .lines()
        .map(|line| line.split("//").next().unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        code.contains("let cur = r.font_size();") && code.contains("cur * factor"),
        "ScaledZoom must scale from the live renderer size"
    );
    assert!(
        !code.contains("cfg.font_size"),
        "ScaledZoom must not scale from the (stale) config font size"
    );
}

/// Zoom-all changes the window-wide font size like Ctrl+Plus, so it must
/// also drop a stale scaled-zoom baseline. Otherwise leaving scaled zoom
/// restores the size from before the zoom-all.
#[test]
fn zoom_all_shares_the_font_size_arm() {
    let src = production_source();
    let arm = src
        .split_once("| Action::ZoomNormalAll => {")
        .expect("zoom-all joins the font-size arm")
        .1
        .split_once("Action::StartSearch =>")
        .expect("end of the font-size arm")
        .0;
    assert!(arm.contains("ws.scaled_zoom_prev_font_size = None;"));
    assert!(
        !src.contains("Action::ZoomInAll => {"),
        "no separate zoom-all arm"
    );
}

/// Reset font size (Cmd/Ctrl+0) and zoom-normal-all return to the config's
/// `font-size`, which is the platform default when the config sets none:
/// 13 px on macOS and 14 px elsewhere, never a fixed 13. Both platforms'
/// defaults are checked on every host.
#[test]
fn font_reset_returns_to_the_platform_default() {
    use super::font_size_after;
    use kettle_config::{Config, DEFAULT_FONT_SIZE, default_font_size};
    for (macos, expected) in [(true, 13.0), (false, 14.0)] {
        for reset in [Action::ResetFontSize, Action::ZoomNormalAll] {
            assert_eq!(
                font_size_after(&reset, 24.0, default_font_size(macos)),
                expected,
                "{reset:?} with macos={macos}"
            );
        }
    }
    let unset = Config::parse_text("");
    assert_eq!(
        font_size_after(&Action::ResetFontSize, 24.0, unset.font_size),
        DEFAULT_FONT_SIZE
    );
    let set = Config::parse_text("font-size = 18\n");
    assert_eq!(
        font_size_after(&Action::ResetFontSize, 24.0, set.font_size),
        18.0
    );
    for (step, expected) in [
        (Action::IncreaseFontSize, 25.0),
        (Action::ZoomInAll, 25.0),
        (Action::DecreaseFontSize, 23.0),
        (Action::ZoomOutAll, 23.0),
        (Action::Copy, 24.0),
    ] {
        assert_eq!(font_size_after(&step, 24.0, 15.0), expected, "{step:?}");
    }

    // The window's font-size arm resets to the config, through this helper.
    let src = production_source();
    let arm = src
        .split_once("| Action::ZoomNormalAll => {")
        .expect("font-size arm")
        .1
        .split_once("Action::StartSearch =>")
        .expect("end of the font-size arm")
        .0;
    assert!(arm.contains("font_size_after(&action, r.font_size(), self.cfg.font_size)"));
}

/// PTY rows, columns, and CSI 14 t pixels must be recomputed from the font
/// metrics selected by the zoom transition, not the metrics that preceded
/// it. A live renderer is too heavyweight for this unit suite, so pin the
/// mutation order at the shared helper boundary.
#[test]
fn scaled_zoom_resizes_after_changing_font_metrics() {
    let src = production_source();
    let helper = src
        .split("fn toggle_zoom_with_scale(")
        .nth(1)
        .and_then(|s| s.split("\n    }\n").next())
        .expect("toggle_zoom_with_scale body present");
    let resize = helper
        .rfind("self.resize_all(ws);")
        .expect("scaled zoom resizes PTYs");
    for change in ["r.set_font_size(new_size);", "r.set_font_size(prev);"] {
        assert!(
            helper.find(change).is_some_and(|offset| offset < resize),
            "{change} must precede the PTY geometry refresh"
        );
    }
}

/// Drift guard. `ToggleZoom` and `ScaledZoom` must route through the SAME
/// `toggle_zoom_with_scale` transition helper, which owns
/// `scaled_zoom_prev_font_size`. Otherwise a plain zoom-out after a
/// `ScaledZoom` zoom-in leaves the font enlarged and the saved baseline
/// stale, and a later `ScaledZoom` compounds from the wrong size. A
/// behavioral test needs a live renderer + mux; pin the shared call at the
/// source.
#[test]
fn toggle_zoom_and_scaled_zoom_share_the_transition_helper() {
    let src = production_source();
    let toggle_arm = src
        .split("Action::ToggleZoom => {")
        .nth(1)
        .and_then(|s| s.split("Action::EqualizeSplits").next())
        .expect("ToggleZoom arm present");
    assert!(
        toggle_arm.contains("self.toggle_zoom_with_scale(ws, None);"),
        "plain ToggleZoom must route through toggle_zoom_with_scale so \
             zoom-out restores/clears any pending scaled-zoom font baseline"
    );
    let scaled_arm = src
        .split("Action::ScaledZoom => {")
        .nth(1)
        .and_then(|s| s.split("Action::ToggleFullscreen").next())
        .expect("ScaledZoom arm present");
    assert!(
        scaled_arm.contains("self.toggle_zoom_with_scale(ws, Some(1.5));"),
        "ScaledZoom must route through the same shared helper as ToggleZoom"
    );
}

/// A plain zoom toggle on a single-pane tab does nothing rather than set a
/// hidden zoom flag, while leaving zoom always works and ScaledZoom keeps
/// its font step. A behavioral test needs a live renderer + mux; the live
/// `zoomed-layout-keys` smoke drives it end to end.
#[test]
fn plain_zoom_toggle_needs_a_pane_to_expand() {
    let src = production_source();
    let toggle_arm = src
        .split("Action::ToggleZoom => {")
        .nth(1)
        .and_then(|s| s.split("Action::EqualizeSplits").next())
        .expect("ToggleZoom arm present");
    assert!(
        toggle_arm.contains("if ws.mux.is_zoomed() || ws.mux.active_pane_count() > 1 {"),
        "ToggleZoom must leave a single-pane tab alone unless it is already zoomed"
    );
    let scaled_arm = src
        .split("Action::ScaledZoom => {")
        .nth(1)
        .and_then(|s| s.split("Action::ToggleFullscreen").next())
        .expect("ScaledZoom arm present");
    assert!(
        !scaled_arm.contains("active_pane_count"),
        "ScaledZoom keeps its font step on a single pane"
    );
}

#[test]
fn resized_ignores_degenerate_size() {
    let src = production_source();
    let arm = src
        .split("WindowEvent::Resized(size) => {")
        .nth(1)
        .expect("Resized arm present");
    let head = &arm[..arm.len().min(900)];
    assert!(
        head.contains("size.width == 0 || size.height == 0") && head.contains("return"),
        "the Resized handler must early-return on a 0-dimension size"
    );
}

#[test]
fn queued_macos_startup_resizes_preserve_the_corrected_grid() {
    use super::window_events::macos_resize_event_is_current;
    use super::{
        ContentBands, StartupMonitor, content_rect_for_with_strip, grid_dims_px, pty_pixel_size,
        startup_surface_for, startup_surface_with_measured_titlebar,
    };
    use winit::dpi::PhysicalSize;

    let cfg = kettle_config::Config {
        window_width: Some(100),
        window_height: Some(30),
        ..Default::default()
    };
    for scale in [1.0, 1.5, 2.0] {
        let cell = (7.8 * scale as f32, 16.25 * scale as f32);
        let monitor = Some(StartupMonitor::from_display_points(1920, 1080, scale));
        let estimated = 28.0 * scale as f32;
        let measured = 32.0 * scale as f32;
        let before = startup_surface_for(
            &cfg,
            cell,
            ContentBands {
                tab_bar_h: estimated,
                ..Default::default()
            },
            monitor,
        );
        let after =
            startup_surface_with_measured_titlebar(&cfg, before, estimated, measured, monitor);
        let geometry = |size: (u32, u32)| {
            let content = content_rect_for_with_strip(
                size,
                ContentBands {
                    tab_bar_h: measured,
                    ..Default::default()
                },
                cfg.tab_bar_pos,
                cfg.status_bar,
                cfg.tab_bar_width,
            );
            let grid = grid_dims_px((content.2, content.3), cell, (cfg.padding_x, cfg.padding_y));
            (grid, pty_pixel_size(cell, grid))
        };
        let initial = geometry(after);
        assert_eq!(initial.0, (100, 30));
        assert_eq!(geometry(before).0, (100, 29));
        if scale == 1.0 {
            assert_eq!((before, after), ((805, 540), (805, 544)));
            assert_eq!(initial.1, (780, 488));
            assert_eq!(geometry(before).1, (780, 471));
        }
        // Renderer creation already read the corrected native size. AppKit's
        // two queued frame notifications arrive afterwards, in their old order.
        let current = PhysicalSize::new(after.0, after.1);
        let mut changes = vec![initial];
        for size in [before, after] {
            if macos_resize_event_is_current(PhysicalSize::new(size.0, size.1), current) {
                let next = geometry(size);
                if changes.last() != Some(&next) {
                    changes.push(next);
                }
            }
        }
        assert_eq!(
            changes,
            vec![initial],
            "scale={scale}: no transient PTY reflow"
        );
    }
}

#[test]
fn macos_resize_filter_accepts_live_changes_and_precedes_layout() {
    use super::window_events::macos_resize_event_is_current;
    use winit::dpi::PhysicalSize;

    for (width, height) in [(805, 544), (900, 600), (640, 480), (1610, 1088)] {
        let size = PhysicalSize::new(width, height);
        assert!(macos_resize_event_is_current(size, size));
        assert!(!macos_resize_event_is_current(
            PhysicalSize::new(width, height - 4),
            size,
        ));
        assert!(!macos_resize_event_is_current(
            PhysicalSize::new(width + 4, height),
            size,
        ));
    }
    let source = production_source();
    let resized = source
        .split_once("WindowEvent::Resized(size) => {")
        .unwrap()
        .1
        .split_once("WindowEvent::ScaleFactorChanged")
        .unwrap()
        .0;
    let filter = resized
        .find("!macos_resize_event_is_current(size, current)")
        .unwrap();
    assert!(resized[..filter].contains("#[cfg(target_os = \"macos\")]"));
    assert!(resized[..filter].contains("size.width == 0 || size.height == 0"));
    assert!(
        resized[filter..]
            .split_once("ws.dpi_resize_surface_suspended = false;")
            .unwrap()
            .0
            .contains("return;")
    );
    assert!(filter < resized.find("ws.dpi_resize.on_event(").unwrap());
    assert!(filter < resized.find("self.commit_window_resize(").unwrap());
}

#[test]
fn macos_native_tab_resize_matches_live_view_instead_of_window_rectangle() {
    use super::titlebar::macos_view_frame_size;
    use super::window_events::macos_resize_event_is_current;
    use winit::dpi::{LogicalSize, PhysicalSize};

    // AppKit's window-derived content rectangle includes the native tab band;
    // winit emits the smaller content view frame, including its pixel rounding.
    let window_rectangle = LogicalSize::new(805.25, 544.25);
    let live_view_frame = LogicalSize::new(805.25, 516.25);
    let stale_view_frame = LogicalSize::new(805.25, 512.25);
    for (scale, expected) in [
        (1.0, (805, 516)),
        (1.25, (1007, 645)),
        (1.5, (1208, 774)),
        (2.0, (1611, 1033)),
    ] {
        let final_event = PhysicalSize::new(expected.0, expected.1);
        let window_size = window_rectangle.to_physical::<u32>(scale);
        let current = macos_view_frame_size(live_view_frame, scale);
        let stale_event = stale_view_frame.to_physical::<u32>(scale);
        assert_eq!(current, final_event, "scale={scale}");
        assert_ne!(current, window_size, "native tab height remains different");
        assert!(!macos_resize_event_is_current(final_event, window_size));
        assert!(macos_resize_event_is_current(final_event, current));
        assert!(!macos_resize_event_is_current(stale_event, current));
        assert!(!macos_resize_event_is_current(window_size, current));
    }

    // Keep the native query connected to the portable conversion above. Native
    // windows cannot be created by this portable libtest regression.
    let events = include_str!("window_events.rs");
    let resized = events
        .split_once("WindowEvent::Resized(size) => {")
        .unwrap()
        .1
        .split_once("WindowEvent::ScaleFactorChanged")
        .unwrap()
        .0;
    assert!(
        resized.contains(".and_then(|window| titlebar::macos_view_size(window))"),
        "resize acceptance must query the live view, not the window-derived rectangle"
    );
    assert!(!resized.contains("window.inner_size()"));
    let titlebar = include_str!("titlebar.rs");
    let query = titlebar
        .split_once("pub(super) fn view_size(")
        .unwrap()
        .1
        .split_once("pub(super) fn set_tabs(")
        .unwrap()
        .0;
    for required in [
        "with_window(window, |native, view|",
        "view.frame()",
        "macos_view_frame_size(",
        "frame.size.width, frame.size.height",
        "native.backingScaleFactor()",
    ] {
        assert!(query.contains(required), "native view query requires {required}");
    }
    assert!(!query.contains("unsafe"));
    assert!(!query.contains("window_handle"));
}

/// Drift guard: pin the `should_zoom_font` policy. The
/// in-wheel-handler call relies on this returning Some only when
/// Ctrl is held AND the user hasn't opted out via
/// `disable-mousewheel-zoom`. If a future refactor accidentally
/// inverts the disable check or drops the Ctrl gate, the test
/// fires before the regression ships.
#[test]
fn should_zoom_font_gates_on_ctrl_and_disable_flag() {
    use super::should_zoom_font;
    // Ctrl + scroll up + not disabled → +1 (grow font).
    assert_eq!(should_zoom_font(true, 3, false), Some(1));
    // Ctrl + scroll down + not disabled → -1 (shrink font).
    assert_eq!(should_zoom_font(true, -3, false), Some(-1));
    // Ctrl + scroll up + DISABLED → None (opt-out honored).
    assert_eq!(should_zoom_font(true, 3, true), None);
    // No Ctrl → None even when not disabled (regular scrollback
    // scroll path takes over).
    assert_eq!(should_zoom_font(false, 3, false), None);
    // No Ctrl AND disabled → still None.
    assert_eq!(should_zoom_font(false, 3, true), None);
    // Zero lines → None (a stale wheel event with no actual
    // motion shouldn't trigger a font change).
    assert_eq!(should_zoom_font(true, 0, false), None);
}

#[test]
fn fractional_cell_geometry_rounds_after_the_full_grid() {
    use super::{pty_pixel_extent, pty_pixel_size};

    assert_eq!(pty_pixel_extent(9.6, 100), 960);
    assert_eq!(pty_pixel_extent(19.2, 40), 768);
    assert_eq!(pty_pixel_size((9.6, 19.2), (100, 40)), (960, 768));

    // Both cell metrics round to the same integer cell width, but their
    // complete PTY geometry remains distinguishable. This is the
    // pixel-only resize a fractional monitor transition must not miss.
    assert_eq!(9.1_f32.round() as u16, 9.4_f32.round() as u16);
    assert_eq!(pty_pixel_extent(9.1, 100), 910);
    assert_eq!(pty_pixel_extent(9.4, 100), 940);

    assert_eq!(pty_pixel_extent(f32::NAN, 80), 80);
    assert_eq!(pty_pixel_extent(f32::INFINITY, 80), 80);
    assert_eq!(pty_pixel_extent(1000.0, usize::MAX), u16::MAX);
}

/// Regression coverage for content geometry at every tab-bar position.
/// Walks the 4 (tab_pos)×3 (status_pos) cases.
#[test]
fn content_rect_for_carves_out_tab_and_status_bands() {
    use super::content_rect_for;
    use kettle_config::{StatusBarMode, TabBarPos};
    // Top + status-off: content starts at y=tab_h, full width.
    let r = content_rect_for((800, 600), 24.0, 16.0, TabBarPos::Top, StatusBarMode::Off);
    assert_eq!(r, (0.0, 24.0, 800.0, 576.0));
    // Bottom + status-off: content at y=0, height shrinks by tab_h.
    let r = content_rect_for(
        (800, 600),
        24.0,
        16.0,
        TabBarPos::Bottom,
        StatusBarMode::Off,
    );
    assert_eq!(r, (0.0, 0.0, 800.0, 576.0));
    // Top + status-top: both claim from top.
    let r = content_rect_for((800, 600), 24.0, 16.0, TabBarPos::Top, StatusBarMode::Top);
    assert_eq!(r, (0.0, 40.0, 800.0, 560.0));
    // Top + status-bottom: status claims from bottom.
    let r = content_rect_for(
        (800, 600),
        24.0,
        16.0,
        TabBarPos::Top,
        StatusBarMode::Bottom,
    );
    assert_eq!(r, (0.0, 24.0, 800.0, 560.0));
    // Bottom + status-bottom: both claim from bottom.
    let r = content_rect_for(
        (800, 600),
        24.0,
        16.0,
        TabBarPos::Bottom,
        StatusBarMode::Bottom,
    );
    assert_eq!(r, (0.0, 0.0, 800.0, 560.0));
    // Left vertical strip — content carves out
    // `VERTICAL_TAB_STRIP_W` (180 px) from the LEFT side.
    let r = content_rect_for((800, 600), 24.0, 0.0, TabBarPos::Left, StatusBarMode::Off);
    assert_eq!(r, (180.0, 0.0, 620.0, 600.0));
    // Right: 180 px carved from the RIGHT side.
    let r = content_rect_for((800, 600), 24.0, 0.0, TabBarPos::Right, StatusBarMode::Off);
    assert_eq!(r, (0.0, 0.0, 620.0, 600.0));
    // Vertical + status-bar: status still claims y-band
    // (status is always horizontal in v1); strip claims x-band.
    let r = content_rect_for((800, 600), 24.0, 16.0, TabBarPos::Left, StatusBarMode::Top);
    assert_eq!(r, (180.0, 16.0, 620.0, 584.0));
    let r = content_rect_for(
        (800, 600),
        24.0,
        16.0,
        TabBarPos::Right,
        StatusBarMode::Bottom,
    );
    assert_eq!(r, (0.0, 0.0, 620.0, 584.0));
    // Defensive: content_h + content_w clamped to >= 1.0 so a
    // tiny window doesn't degenerate to a zero/negative rect.
    let r = content_rect_for((100, 30), 24.0, 16.0, TabBarPos::Top, StatusBarMode::Bottom);
    assert!(r.3 >= 1.0);
    let r = content_rect_for((100, 600), 0.0, 0.0, TabBarPos::Left, StatusBarMode::Off);
    assert!(
        r.2 >= 1.0,
        "narrow window with vertical strip clamps content_w"
    );
}

/// #576: update notices never reserve or resize terminal content.
#[test]
fn no_update_state_changes_content_rect() {
    let src = production_source();
    let body = |name: &str| {
        src.split(name)
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .unwrap_or_else(|| panic!("{name} exists"))
            .to_string()
    };
    for name in [
        "fn apply_update_facts(",
        "fn dismiss_update_notice(",
        "fn open_update_release(",
        "fn request_update_facts(",
    ] {
        let body = body(name);
        for call in ["pending_resize", ".resize(", "queue_window_resizes"] {
            assert!(!body.contains(call), "{name} must not resize ({call})");
        }
    }
    assert!(!body("fn tab_bar_h(").contains("update"));
    assert!(!src.contains("update_banner_h"));
}


#[test]
fn closing_scaled_zoom_owner_restores_baseline_with_another_tab_remaining() {
    let tab = |pane, owner| crate::mux::Tab {
        root: crate::mux::Node::Leaf(pane),
        focus: pane,
        title_override: None,
        zoomed: owner,
        last_output_at: None,
        last_seen_at: None,
        bell: false,
        generated_group_name: None,
        scaled_zoom_owner: owner,
        lanes: Default::default(),
    };
    let mut ws = WindowState::new(1, Mux::new());
    ws.mux.tabs = vec![tab(1, true), tab(2, false), tab(3, false)];
    ws.scaled_zoom_prev_font_size = Some(12.0);
    ws.mux.active = 2;
    assert!(!ws.mux.close_tab());
    assert_eq!(
        super::take_orphaned_scaled_zoom_font(&mut ws),
        None,
        "closing another tab must preserve the owner's zoom"
    );
    ws.mux.active = 0;
    assert!(ws.mux.move_active_tab(1));
    assert!(!ws.mux.close_tab());
    assert_eq!(ws.mux.tabs.len(), 1);
    assert_eq!(
        super::take_orphaned_scaled_zoom_font(&mut ws),
        Some(12.0),
        "closing the scaled-zoom owner must restore the font"
    );
    assert!(ws.scaled_zoom_prev_font_size.is_none());
    let source = production_source();
    assert!(
        source.contains(".filter(|_| dt.tab.scaled_zoom_owner)"),
        "tearing another tab must not transfer the owner's baseline"
    );
    let finish = source.split("fn finish_window_dispatch(").nth(1).unwrap();
    assert!(
        finish.find("set_font_size(previous)").unwrap()
            < finish.find("self.resize_all(&mut ws)").unwrap()
    );
}

/// Making the font larger, smaller or resetting it shows the size chip with
/// the new size and the grid it leaves, in the user's language, and reset
/// shows the configured size. Whole sizes have no decimal point and others
/// no float noise.
#[test]
fn font_size_changes_show_the_size_and_grid_on_the_chip() {
    use super::{font_size_after, font_size_chip_label};
    let en = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let es = kettle_i18n::Translator::new(kettle_i18n::Language::Es);
    for (action, size) in [
        (Action::IncreaseFontSize, "14"),
        (Action::ZoomInAll, "14"),
        (Action::DecreaseFontSize, "12"),
        (Action::ZoomOutAll, "12"),
        (Action::ResetFontSize, "15"),
        (Action::ZoomNormalAll, "15"),
    ] {
        let after = font_size_after(&action, 13.0, 15.0);
        assert_eq!(
            font_size_chip_label(&en, after, (80, 24)),
            format!("Font size {size} · 80×24"),
            "{action:?}"
        );
        assert_eq!(
            font_size_chip_label(&es, after, (80, 24)),
            format!("Tamaño de fuente {size} · 80×24"),
            "{action:?}"
        );
    }
    assert_eq!(
        font_size_chip_label(&en, 13.5, (120, 40)),
        "Font size 13.5 · 120×40"
    );
    assert_eq!(
        font_size_chip_label(&en, 13.1_f32 + 1.0, (1, 1)),
        "Font size 14.1 · 1×1"
    );
}

/// `resize-overlay = never` keeps the chip off for font changes too; the
/// other modes show it, `after-first` included, since a font change is
/// never the window's first placement.
#[test]
fn only_resize_overlay_never_hides_the_font_size_chip() {
    use kettle_config::ResizeOverlayMode as Mode;
    assert!(!super::font_size_chip_shown(Mode::Never));
    assert!(super::font_size_chip_shown(Mode::AfterFirst));
    assert!(super::font_size_chip_shown(Mode::Always));
}

/// Screen readers hear the chip's own text from a polite live region.
#[test]
fn the_font_size_chip_is_announced_politely() {
    let label = "Tamaño de fuente 14 · 80×24";
    let (_, node) = super::font_size_chip_node(label);
    assert_eq!(node.role(), accesskit::Role::Status);
    assert_eq!(node.label(), Some(label));
    assert_eq!(node.live(), Some(accesskit::Live::Polite));
}

/// A font change the user asks for fills the size chip with the new size
/// and grid, in the user's language, and has its live region announced;
/// `resize-overlay = never` leaves the chip empty, as does a window with no
/// renderer to size.
#[test]
fn a_font_size_change_fills_the_size_chip_unless_it_is_turned_off() {
    use kettle_config::ResizeOverlayMode as Mode;
    let now = std::time::Instant::now();
    for (language, caption) in [
        (kettle_i18n::Language::En, "Font size 15 · 74×22"),
        (kettle_i18n::Language::Es, "Tamaño de fuente 15 · 74×22"),
    ] {
        let tr = kettle_i18n::Translator::new(language);
        for mode in [Mode::AfterFirst, Mode::Always] {
            let mut ws = WindowState::new(1, Mux::new());
            super::font_size_chip(&mut ws, (mode, &tr), Some((15.0, (74, 22))), now);
            assert_eq!(ws.resize_overlay, Some((74, 22, now)), "{mode:?}");
            assert_eq!(ws.resize_overlay_label.as_deref(), Some(caption));
            assert!(ws.accessibility_pending, "the live region waits");
        }
        let mut ws = WindowState::new(1, Mux::new());
        super::font_size_chip(&mut ws, (Mode::Never, &tr), Some((15.0, (74, 22))), now);
        assert_eq!((ws.resize_overlay, ws.resize_overlay_label), (None, None));
        assert!(!ws.accessibility_pending);
        let mut ws = WindowState::new(1, Mux::new());
        super::font_size_chip(&mut ws, (Mode::Always, &tr), None, now);
        assert_eq!((ws.resize_overlay, ws.resize_overlay_label), (None, None));
    }
}

/// The accessibility tree rebuilds when the size chip's caption comes, goes
/// or changes, though nothing else in the tree does.
#[test]
fn the_size_chip_caption_is_part_of_the_accessibility_key() {
    use kettle_render::Overlay;
    let key = |text: Option<&str>| {
        super::overlay_live_region_key(&Overlay {
            resize_overlay: Some((80, 24)),
            resize_overlay_text: text.map(str::to_owned),
            ..Overlay::default()
        })
    };
    let none = key(None);
    let fourteen = key(Some("Font size 14 · 80×24"));
    let fifteen = key(Some("Font size 15 · 74×22"));
    assert_ne!(none, fourteen);
    assert_ne!(fourteen, fifteen);
    assert_eq!(fourteen, key(Some("Font size 14 · 80×24")));
}
