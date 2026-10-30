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

