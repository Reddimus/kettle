//! Title-bar tab strips, on macOS and in the GNOME header: an even split of
//! the lane, the drag lane, the minimum width and scrolling, the hold after a
//! close with the pointer, and hit tests that follow the painted tabs.

use super::*;

const SCALES: [f32; 3] = [1.0, 1.5, 2.0];

/// A macOS title bar `width` points wide: its lane starts past the traffic
/// lights, or at the window's edge in fullscreen.
fn mac_lane(width: f32, scale: f32, fullscreen: bool) -> Rect {
    let left = if fullscreen { 0.0 } else { 78.0 * scale };
    (left, 0.0, width * scale - left, 32.0 * scale)
}

/// A GNOME header `width` points wide, its window buttons on the right.
fn linux_lane(width: f32, scale: f32) -> Rect {
    let band = (0.0, 0.0, width * scale, LINUX_HEADER_HEIGHT * scale);
    let buttons = crate::window_buttons::ButtonLayout::default();
    horizontal_tab_lane(band, buttons.reservations(band.2, band.3))
}

/// The lanes every sweep covers: macOS windowed and fullscreen, and the
/// GNOME header, with the header flags `horizontal_tab_geometry` takes.
fn lanes(width: f32, scale: f32) -> [(&'static str, Rect, (bool, bool)); 3] {
    [
        ("macOS", mac_lane(width, scale, false), (false, true)),
        (
            "macOS fullscreen",
            mac_lane(width, scale, true),
            (false, true),
        ),
        ("GNOME", linux_lane(width, scale), (true, true)),
    ]
}

fn tab_width(layout: &TabStripLayout, i: usize) -> f32 {
    if i + 1 == layout.xs.len() {
        layout.last_width
    } else {
        layout.width
    }
}

/// Where the trailing controls start: the gear or `+` on macOS, Find or `≡`
/// in the GNOME header (whose `+` leads), else the lane's end.
fn trailing_controls(lane: Rect, g: &HorizontalTabGeometry) -> f32 {
    let linux = g.lead > 0.0;
    let controls = if linux {
        [g.search_rect, g.arrow_rect]
    } else {
        [g.settings_rect, g.plus_rect]
    };
    controls
        .into_iter()
        .filter(|r| r.2 > 0.0)
        .map(|r| r.0)
        .fold(lane.0 + lane.2, f32::min)
}

/// The end of what the strip paints before its drag lane: the last tab
/// shown, or `›`.
fn painted_end(lane: Rect, g: &HorizontalTabGeometry) -> f32 {
    let start = lane.0 + g.lead;
    let tabs = g
        .layout
        .xs
        .iter()
        .enumerate()
        .filter_map(|(i, x)| x.map(|x| start + x + tab_width(&g.layout, i)))
        .fold(start, f32::max);
    g.layout
        .arrow_right
        .map_or(tabs, |x| tabs.max(start + x + g.layout.arrow_width))
}

/// The strip a window paints from `g`, as `tab_bar_at_height` builds it.
fn painted_bar(lane: Rect, header: (bool, bool), g: &HorizontalTabGeometry) -> TabBar {
    let mut bar = TabBar::hidden();
    bar.height = lane.3;
    bar.band = lane;
    bar.in_titlebar = header.1;
    bar.linux_header = header.0;
    bar.new_tab = g.plus_rect;
    bar.new_tab_menu = g.arrow_rect;
    bar.settings = g.settings_rect;
    bar.search = g.search_rect;
    let x0 = lane.0 + g.lead;
    for (idx, x) in g.layout.xs.iter().enumerate() {
        let rect = (
            x.map_or(PARKED_TAB, |x| x0 + x),
            lane.1,
            tab_width(&g.layout, idx),
            lane.3,
        );
        // Every tab shows its close button here, as a hovered one does.
        let content =
            kettle_render::horizontal_tab_content_layout(rect, true, true, 0.0, "shell", None, 8.0);
        bar.segments.push(TabSeg {
            idx,
            rect,
            title_rect: content.title,
            close: content.close,
            hint: None,
            title: format!("tab {idx}"),
            path: None,
            active: idx == 0,
            activity: RenderTabActivity::Normal,
        });
    }
    let arrow = |x: Option<f32>| {
        x.map_or((0.0, 0.0, 0.0, 0.0), |x| {
            (x0 + x, lane.1, g.layout.arrow_width, lane.3)
        })
    };
    bar.scroll_left = arrow(g.layout.arrow_left);
    bar.scroll_right = arrow(g.layout.arrow_right);
    bar
}

/// Tabs divide what the lane leaves after its controls and the drag lane
/// evenly, from the strip's start, on macOS and in the GNOME header. They
/// were capped at 240 points (scaled with the bar) and left-packed, leaving
/// the rest of the bar empty.
#[test]
fn titlebar_tabs_divide_the_lane_before_the_drag_lane_evenly() {
    let cfg = Config::default();
    for scale in SCALES {
        for width in [800.0, 1000.0, 1280.0, 1920.0] {
            for (name, lane, header) in lanes(width, scale) {
                for n in [2_usize, 3, 4] {
                    for active in [0, n - 1] {
                        let g = horizontal_tab_geometry(lane, scale, header, (n, active), &cfg);
                        let case = format!("{name} {width}pt @{scale}x, {n} tabs");
                        let room = g.strip - TITLEBAR_DRAG_LANE * scale;
                        assert!(room / n as f32 >= cfg.tab_min_width * scale, "{case}");
                        let even = room / n as f32;
                        assert!(
                            (g.layout.width - even).abs() < 0.01,
                            "{case}: {}",
                            g.layout.width
                        );
                        assert_eq!(g.layout.last_width, g.layout.width, "{case}");
                        for (i, x) in g.layout.xs.iter().enumerate() {
                            let x = x.unwrap_or_else(|| panic!("{case}: tab {i} hidden"));
                            assert!((x - i as f32 * even).abs() < 0.01, "{case}: tab {i} at {x}");
                        }
                        assert!(g.layout.arrow_left.is_none() && g.layout.arrow_right.is_none());
                        // The last tab ends where the drag lane starts.
                        let gap = trailing_controls(lane, &g) - painted_end(lane, &g);
                        let drag = TITLEBAR_DRAG_LANE * scale;
                        assert!(gap >= drag - 0.01, "{case}: {gap} before the controls");
                        // Past the drag lane, only the controls' own margin.
                        assert!(gap <= drag + 12.0 * scale + 0.01, "{case}: {gap} left over");
                    }
                }
            }
        }
    }
}

/// On macOS a lone tab is laid out like any tab: from the lane's start
/// (past the traffic lights, or the window's edge in fullscreen) across the
/// lane to the drag lane. It was sized to its title and centred on the
/// window. The torn-off window's lone tab, which a tear's grip is measured
/// into, is the same tab.
#[test]
fn a_lone_macos_titlebar_tab_fills_the_lane_from_its_start() {
    let cfg = Config::default();
    let buttons = crate::window_buttons::ButtonLayout::default();
    for scale in SCALES {
        for width in [480.0, 640.0, 1000.0, 1280.0, 2560.0] {
            for fullscreen in [false, true] {
                let lane = mac_lane(width, scale, fullscreen);
                let g = horizontal_tab_geometry(lane, scale, (false, true), (1, 0), &cfg);
                let case = format!("{width}pt @{scale}x fullscreen={fullscreen}");
                assert_eq!(g.layout.xs, vec![Some(0.0)], "{case}");
                let w = g.layout.last_width;
                assert_eq!(w, g.strip - TITLEBAR_DRAG_LANE * scale, "{case}");
                assert!(w >= cfg.tab_min_width * scale, "{case}: {w}");
                let tab = tear_off_destination_tab(&cfg, lane, (false, true), scale, &buttons);
                assert_eq!(tab, (lane.0, lane.1, w, lane.3), "{case}");
                for in_tab in [0.0_f64, 40.0, 4000.0] {
                    let grab = titlebar_tear_off_grab_x(
                        &cfg,
                        lane,
                        (false, true),
                        scale,
                        &buttons,
                        in_tab,
                    );
                    let margin = f64::from(8.0 * scale);
                    let expected = f64::from(lane.0) + in_tab.clamp(margin, f64::from(w) - margin);
                    assert!(
                        (grab - expected).abs() < 0.01,
                        "{case}: {grab} for {in_tab}"
                    );
                }
            }
        }
    }
    // Its title sits in the middle of the tab, like any tab's: only the GNOME
    // header's lone title is centred on the window.
    let source: String = include_str!("chrome.rs").split_whitespace().collect();
    let body = source
        .split_once("fntab_bar_at_height(")
        .unwrap()
        .1
        .split_once("fntitle_edit_rect(")
        .unwrap()
        .0;
    assert_eq!(body.matches("centered_title_lane(").count(), 1);
    assert!(body.contains(
        "iflone_title{letwindow_width=band.0+band.2;content.title=centered_title_lane(content.title,window_width*0.5);}"
    ));
    assert!(body.contains("lethint=ifws.macos_titlebar_tabs&&n==1{None}"));
}

/// Every title-bar strip leaves empty space for moving the window between
/// what it paints (the last tab, or `›` while it scrolls) and its trailing
/// controls: the whole drag lane while a minimum-width tab fits beside it,
/// and never less than the lane's floor. Overflowing strips once ran `›`
/// up against the gear.
#[test]
fn titlebar_strips_always_keep_a_drag_lane() {
    for scroll in [true, false] {
        let cfg = Config {
            scroll_tabbar: scroll,
            ..Config::default()
        };
        for scale in SCALES {
            for width in (240..=2000).step_by(40).map(|w| w as f32) {
                for (name, lane, header) in lanes(width, scale) {
                    for n in [1_usize, 2, 3, 7, 12, 40] {
                        for active in [0, n / 2, n - 1] {
                            let g = horizontal_tab_geometry(lane, scale, header, (n, active), &cfg);
                            let case = format!(
                                "{name} {width}pt @{scale}x, {n} tabs, active {active}, scroll={scroll}"
                            );
                            if g.strip < 2.0 {
                                continue;
                            }
                            let gap = trailing_controls(lane, &g) - painted_end(lane, &g);
                            let min = cfg.tab_min_width * scale;
                            let drag = TITLEBAR_DRAG_LANE * scale;
                            let floor = (TITLEBAR_DRAG_LANE_FLOOR * scale).min(g.strip);
                            assert!(gap >= floor - 0.01, "{case}: {gap} before the controls");
                            // Whole while the tabs' least room at the minimum
                            // (one between the arrows once they scroll) fits.
                            let arrow = if header.0 {
                                (LINUX_HEADER_BUTTON * scale).min(lane.3)
                            } else {
                                lane.3
                            };
                            if g.strip >= titlebar_tab_room(n, min, arrow, scroll) + drag {
                                assert!(gap >= drag - 0.01, "{case}: {gap} before the controls");
                            }
                            // Nothing reaches under the traffic lights or `+`.
                            let start = lane.0 + g.lead;
                            for x in g.layout.xs.iter().flatten() {
                                assert!(start + x >= lane.0 - 0.01, "{case}");
                            }
                            if let Some(x) = g.layout.arrow_left {
                                assert!(start + x >= lane.0 - 0.01, "{case}");
                                assert!(
                                    start + x >= g.plus_rect.0 + g.plus_rect.2 - 0.01 || !header.0
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Tabs never narrow below `tab-min-width`, which is in logical pixels like
/// every other measure of the strip: past it the strip scrolls, with `‹` at
/// its start and `›` right after the tabs shown, and the drag lane after
/// that. At 2x the minimum was half what was set.
#[test]
fn titlebar_tabs_keep_the_minimum_and_scroll_between_arrows() {
    let cfg = Config::default();
    for scale in SCALES {
        for width in [480.0, 800.0, 1280.0] {
            for (name, lane, header) in lanes(width, scale) {
                for n in [20_usize, 40] {
                    for active in [0, 7, n - 1] {
                        let g = horizontal_tab_geometry(lane, scale, header, (n, active), &cfg);
                        let case = format!("{name} {width}pt @{scale}x, {n} tabs, active {active}");
                        let min = cfg.tab_min_width * scale;
                        let l = &g.layout;
                        let arrow = l.arrow_width;
                        assert!(arrow > 0.0, "{case}: no arrows");
                        assert_eq!(l.arrow_left, Some(0.0), "{case}");
                        // As many whole tabs as fit beside both arrows and
                        // the drag lane, which narrows toward its floor first
                        // to keep one at the minimum; past that the active
                        // tab shows alone, narrowed to the room.
                        let drag = expected_drag(g.strip, n, min, arrow, scale);
                        let room = g.strip - drag - 2.0 * arrow;
                        if room >= min {
                            assert_eq!(l.width, min, "{case}");
                        } else {
                            assert!((drag - TITLEBAR_DRAG_LANE_FLOOR * scale).abs() < 0.01);
                        }
                        let min = min.min(room);
                        assert_eq!((l.width, l.last_width), (min, min), "{case}");
                        let shown: Vec<usize> = (0..n).filter(|i| l.xs[*i].is_some()).collect();
                        assert!(shown.contains(&active), "{case}: active hidden");
                        assert!(shown.len() < n, "{case}");
                        for (slot, i) in shown.iter().enumerate() {
                            assert_eq!(l.xs[*i], Some(arrow + slot as f32 * min), "{case}");
                        }
                        assert_eq!(
                            l.arrow_right,
                            Some(arrow + shown.len() as f32 * min),
                            "{case}: `›` follows the tabs shown"
                        );
                        assert_eq!(shown.len(), (room / min).floor() as usize, "{case}");
                    }
                }
            }
            // Plain strips measure the minimum the same way.
            let plain = (0.0, 0.0, width * scale, 28.0 * scale);
            let g = horizontal_tab_geometry(plain, scale, (false, false), (40, 0), &cfg);
            assert_eq!(
                g.layout.width,
                cfg.tab_min_width * scale,
                "plain {width} @{scale}x"
            );
        }
    }
    // With `scroll-tabbar = false` tabs keep shrinking instead, still short
    // of the drag lane.
    let cfg = Config {
        scroll_tabbar: false,
        ..Config::default()
    };
    for scale in SCALES {
        for (name, lane, header) in lanes(800.0, scale) {
            let g = horizontal_tab_geometry(lane, scale, header, (40, 39), &cfg);
            let room = g.strip - TITLEBAR_DRAG_LANE * scale;
            assert!(
                g.layout.arrow_left.is_none() && g.layout.arrow_right.is_none(),
                "{name}"
            );
            assert!((g.layout.width - room / 40.0).abs() < 0.01, "{name}");
            assert!(g.layout.width < cfg.tab_min_width * scale, "{name}");
            assert!(g.layout.xs.iter().all(Option::is_some), "{name}");
        }
    }
}

/// The drag lane a strip `strip` wide keeps for `n` tabs: whole while the
/// least room the tabs need at the minimum (one between both arrows once
/// they scroll) fits beside it, then narrowing to its floor.
fn expected_drag(strip: f32, n: usize, min: f32, arrow: f32, scale: f32) -> f32 {
    let need = if n > 1 {
        (n as f32 * min).min(min + 2.0 * arrow)
    } else {
        min
    };
    (strip - need)
        .min(TITLEBAR_DRAG_LANE * scale)
        .max(TITLEBAR_DRAG_LANE_FLOOR * scale)
        .min(strip)
}

/// However many tabs a narrowing title bar holds, it gives up the gear
/// first, then narrows the drag lane to its floor, and only then a tab below
/// the minimum. With 20 tabs a 400-point macOS window once kept the gear and
/// the whole drag lane and showed one 70-point tab between the arrows.
#[test]
fn a_narrowing_title_bar_gives_way_in_order_for_any_tab_count() {
    let cfg = Config::default();
    for scale in SCALES {
        let (min, drag, floor) = (
            cfg.tab_min_width * scale,
            TITLEBAR_DRAG_LANE * scale,
            TITLEBAR_DRAG_LANE_FLOOR * scale,
        );
        for n in [1_usize, 2, 3, 20] {
            for width in (150..=900).step_by(5).map(|w| w as f32) {
                for (name, lane, header) in lanes(width, scale) {
                    let g = horizontal_tab_geometry(lane, scale, header, (n, n - 1), &cfg);
                    if g.strip < 2.0 {
                        continue;
                    }
                    let case = format!("{name} {width}pt @{scale}x, {n} tabs");
                    let start = lane.0 + g.lead;
                    let narrowest = (0..n)
                        .filter(|i| g.layout.xs[*i].is_some())
                        .map(|i| tab_width(&g.layout, i))
                        .fold(f32::INFINITY, f32::min);
                    let lane_left = start + g.strip - painted_end(lane, &g);
                    let gear = g.settings_rect.2 > 0.0;
                    if gear {
                        assert!(
                            narrowest >= min - 0.01,
                            "{case}: a {narrowest} tab beside the gear"
                        );
                        assert!(
                            lane_left >= drag - 0.01,
                            "{case}: a {lane_left} lane beside the gear"
                        );
                    }
                    if narrowest < min - 0.01 {
                        assert!(!gear, "{case}");
                        assert!(
                            (lane_left - floor.min(g.strip)).abs() < 0.01,
                            "{case}: a {narrowest} tab beside a {lane_left} lane"
                        );
                    }
                }
            }
        }
    }
}

/// As a macOS window narrows, the gear goes first, while a minimum-width
/// tab and the whole drag lane still fit beside the other controls; then the
/// drag lane narrows to its floor; only then does a lone tab narrow below
/// the minimum, filling what is left.
#[test]
fn a_narrowing_title_bar_drops_the_gear_then_the_drag_lane_then_the_minimum() {
    let cfg = Config::default();
    for scale in [1.0_f32, 2.0] {
        let (min, drag, floor) = (
            cfg.tab_min_width * scale,
            TITLEBAR_DRAG_LANE * scale,
            TITLEBAR_DRAG_LANE_FLOOR * scale,
        );
        let mut seen_gear = (false, false);
        for width in (150..=700).rev().map(|w| w as f32) {
            let lane = mac_lane(width, scale, false);
            let g = horizontal_tab_geometry(lane, scale, (false, true), (1, 0), &cfg);
            let case = format!("{width}pt @{scale}x");
            let (_, plus, reserve) = super::chrome::titlebar_new_tab_controls(lane, scale);
            let gear_room = lane.2 - reserve - (plus.2 + 8.0 * scale);
            let gear = g.settings_rect.2 > 0.0;
            assert_eq!(gear, gear_room >= min + drag, "{case}");
            if gear {
                seen_gear.0 = true;
            } else {
                seen_gear.1 = true;
            }
            if g.strip < 2.0 {
                continue;
            }
            let tab = g.layout.last_width;
            let lane_left = g.strip - tab;
            if g.strip >= min + drag {
                assert_eq!(tab, g.strip - drag, "{case}");
                assert!((lane_left - drag).abs() < 0.01, "{case}: {lane_left}");
            } else if g.strip >= min + floor {
                assert!(
                    (tab - min).abs() < 0.01,
                    "{case}: the drag lane narrows first"
                );
                assert!(
                    lane_left < drag + 0.01 && lane_left >= floor - 0.01,
                    "{case}"
                );
            } else {
                assert!(
                    (lane_left - floor.min(g.strip)).abs() < 0.01,
                    "{case}: {lane_left}"
                );
                assert!(tab < min, "{case}");
            }
            assert_eq!(g.layout.xs, vec![Some(0.0)], "{case}");
        }
        assert_eq!(seen_gear, (true, true), "@{scale}x");
    }
}

/// The layout `horizontal_tab_geometry` gives a window's strip at `strip`
/// wide, for driving its state by hand.
fn lane_of(strip: f32, scale: f32) -> TitlebarLane {
    TitlebarLane {
        strip,
        arrow: 32.0 * scale,
        min: 120.0 * scale,
        drag: TITLEBAR_DRAG_LANE * scale,
        floor: TITLEBAR_DRAG_LANE_FLOOR * scale,
        scroll: true,
    }
}

fn shown(layout: &TabStripLayout) -> Vec<usize> {
    (0..layout.xs.len())
        .filter(|i| layout.xs[*i].is_some())
        .collect()
}

/// An overflowing strip keeps its active tab in view: a new tab, a tab
/// chosen from the keyboard (the active one too, once scrolled away),
/// next and previous, a close, a resize and a display-scale change each
/// scroll just far enough to show it. Choosing a tab already shown, by
/// pointer or keyboard, leaves the strip where it is, and the arrows and the
/// wheel scroll it without changing the active tab.
#[test]
fn an_overflowing_titlebar_strip_keeps_the_active_tab_in_view() {
    let lane = lane_of(1000.0, 1.0);
    let mut state = TitlebarStrip::default();
    let layout = |n: usize, active: usize, lane: &TitlebarLane, state: &mut TitlebarStrip| {
        let l = titlebar_tab_layout(n, active, lane, state, false);
        assert!(l.xs[active].is_some(), "{n} tabs: active {active} hidden");
        l
    };
    // New tabs open at the end and become active: the strip follows.
    for n in 1..=20 {
        layout(n, n - 1, &lane, &mut state);
    }
    let slots = state.slots;
    assert!(slots > 1 && slots < 20, "{slots}");
    assert_eq!(state.first, 20 - slots);
    // A click on a tab shown makes it active without moving the strip.
    let l = layout(20, 20 - slots, &lane, &mut state);
    assert_eq!(state.first, 20 - slots);
    assert_eq!(l.xs[20 - slots], Some(lane.arrow));
    // The wheel and the arrows scroll the strip, not the selection: the
    // active tab can leave the view.
    assert!(state.scroll(-5, 20));
    let l = layout(20, 20 - slots, &lane, &mut state);
    assert_eq!(shown(&l).first(), Some(&(15 - slots)));
    assert!(state.scroll(-50, 20));
    assert!(!state.scroll(-1, 20), "the start stops it");
    let l = titlebar_tab_layout(20, 19, &lane, &mut { state }, false);
    assert!(l.xs[19].is_some(), "a new active tab is revealed");
    let l = titlebar_tab_layout(20, 20 - slots, &lane, &mut state, false);
    assert_eq!(shown(&l).first(), Some(&0), "scrolled to the start");
    assert!(l.xs[20 - slots].is_none(), "the active tab scrolled away");
    // Choosing that same tab from the keyboard brings it back.
    let mut chosen = state;
    chosen.seen = None;
    let l = titlebar_tab_layout(20, 20 - slots, &lane, &mut chosen, false);
    assert!(l.xs[20 - slots].is_some());
    // Cmd+3, then next tab wrapping from the last to the first.
    layout(20, 2, &lane, &mut state);
    layout(20, 19, &lane, &mut state);
    assert_eq!(state.first, 20 - slots);
    layout(20, 0, &lane, &mut state);
    assert_eq!(state.first, 0);
    layout(20, 19, &lane, &mut state);
    // A close: one tab fewer, the active one moved down.
    layout(19, 18, &lane, &mut state);
    assert_eq!(state.first, 19 - slots);
    // A resize narrower and wider, and a display-scale change.
    for (strip, scale) in [(600.0, 1.0), (1600.0, 1.0), (2000.0, 2.0), (700.0, 2.0)] {
        let lane = lane_of(strip, scale);
        for active in [0, 9, 18] {
            layout(19, active, &lane, &mut state);
            assert!(state.first + state.slots <= 19);
        }
    }
    // An arrow at either end does nothing, and a strip that fits has no
    // scroll to take.
    let mut state = TitlebarStrip::default();
    layout(3, 0, &lane, &mut state);
    assert!(!state.scroll(1, 3) && !state.scroll(-1, 3));
}

/// Scrolled back from the active last tab, a close with the pointer on a
/// tab shown before it holds the strip; a new tab opened then (Cmd+T or `+`,
/// at the end or after the current tab) must still scroll into view. The
/// held layouts once left the strip's record of the active tab and tab count
/// behind, so the new tab matched it and stayed scrolled away.
#[test]
fn a_new_tab_after_a_held_close_opens_in_view() {
    let lane = lane_of(1000.0, 1.0);
    let mut state = TitlebarStrip::default();
    for n in 1..=20 {
        titlebar_tab_layout(n, n - 1, &lane, &mut state, true);
    }
    // `‹` once: the active last tab scrolls out of view.
    assert!(state.scroll(-1, 20));
    let l = titlebar_tab_layout(20, 19, &lane, &mut state, true);
    assert!(l.xs[19].is_none() && l.xs[14].is_some());
    // ✕ on tab 14: the active tab is the 18th now, and the strip holds.
    let before = state;
    state.held = before.held_after_close(19);
    let l = titlebar_tab_layout(19, 18, &lane, &mut state, true);
    assert!(l.xs[18].is_some() && state.held.is_some());
    // A new tab ends the hold and becomes active: tab 19, whether it opens
    // at the end or after the current (last) tab.
    state.held = None;
    let l = titlebar_tab_layout(20, 19, &lane, &mut state, true);
    assert!(l.xs[19].is_some(), "the new tab opened out of view");
}

/// A small deterministic generator for the event sequences below.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

/// Random sequences of what happens to a title-bar strip, with the tab list
/// kept the way the mux keeps it: after every new tab, close (from the
/// keyboard or with the pointer, held or not), Go to Tab, next or previous
/// tab, click, resize or scale change, the active tab is on screen. Arrows,
/// the wheel and the pointer entering or leaving may leave it scrolled away.
#[test]
fn every_event_that_moves_the_selection_keeps_the_active_tab_in_view() {
    let mut rng = Lcg(0x5eed_7ab5);
    let strips = [420.0_f32, 700.0, 1000.0, 1600.0];
    for sequence in 0..4000 {
        let mut scale = [1.0_f32, 2.0][rng.below(2)];
        let mut strip = strips[rng.below(strips.len())] * scale;
        let after_current = rng.below(2) == 1;
        let mut n = 1 + rng.below(24);
        let mut active = rng.below(n);
        let mut hold = false;
        let mut state = TitlebarStrip::default();
        let mut log = Vec::new();
        let close = |n: &mut usize, active: &mut usize, idx: usize| {
            *n -= 1;
            if (*active >= *n || *active > idx) && *active > 0 {
                *active -= 1;
            }
        };
        titlebar_tab_layout(n, active, &lane_of(strip, scale), &mut state, hold);
        for _ in 0..60 {
            let event = rng.below(11);
            log.push(event);
            let lane = lane_of(strip, scale);
            let check = match event {
                0 => {
                    let idx = if after_current { active + 1 } else { n };
                    n += 1;
                    active = idx;
                    state.held = None;
                    true
                }
                1 if n > 1 => {
                    let idx = active;
                    close(&mut n, &mut active, idx);
                    state.held = None;
                    true
                }
                2 if n > 1 => {
                    hold = true;
                    let l = titlebar_tab_layout(n, active, &lane, &mut state, hold);
                    let shown: Vec<usize> = (0..n).filter(|i| l.xs[*i].is_some()).collect();
                    let idx = shown[rng.below(shown.len())];
                    let before = state;
                    close(&mut n, &mut active, idx);
                    state.held = before.held_after_close(n);
                    true
                }
                3 => {
                    active = rng.below(n.min(9));
                    state.seen = None;
                    true
                }
                4 => {
                    active = if rng.below(2) == 0 {
                        (active + 1) % n
                    } else {
                        (active + n - 1) % n
                    };
                    true
                }
                5 => {
                    let step = if rng.below(2) == 0 { -1 } else { 1 };
                    state.scroll(step, n);
                    false
                }
                6 => {
                    // A resize, maybe to the same width.
                    strip = strips[rng.below(strips.len())] * scale;
                    state.held = None;
                    state.seen = None;
                    true
                }
                7 => {
                    let old = scale;
                    scale = if scale == 1.0 { 2.0 } else { 1.0 };
                    strip = strip / old * scale;
                    state.held = None;
                    state.seen = None;
                    true
                }
                8 => {
                    hold = !hold;
                    false
                }
                9 => {
                    let l = titlebar_tab_layout(n, active, &lane, &mut state, hold);
                    let shown: Vec<usize> = (0..n).filter(|i| l.xs[*i].is_some()).collect();
                    active = shown[rng.below(shown.len())];
                    true
                }
                _ => false,
            };
            let l = titlebar_tab_layout(n, active, &lane_of(strip, scale), &mut state, hold);
            if check {
                assert!(
                    l.xs[active].is_some(),
                    "sequence {sequence} ({after_current}): events {log:?} left tab {active} of {n} hidden"
                );
            }
        }
    }
}

/// After a close with the pointer the remaining tabs keep their width and
/// place while the pointer stays on the strip, so the next tab's close
/// button comes to rest where the closed one's was; leaving the strip
/// divides it evenly again. A scrolled strip keeps where it is scrolled to,
/// its `›` included.
#[test]
fn a_close_with_the_pointer_holds_the_tabs_until_the_pointer_leaves() {
    let cfg = Config::default();
    for scale in SCALES {
        for (name, lane, header) in lanes(1280.0, scale) {
            for (n, closed) in [(4_usize, 1_usize), (4, 3), (3, 0), (2, 0)] {
                let case = format!("{name} @{scale}x, closing {closed} of {n}");
                let mut state = TitlebarStrip::default();
                let before = horizontal_tab_geometry_in(
                    lane,
                    scale,
                    header,
                    (n, closed),
                    &cfg,
                    (&mut state, true),
                );
                let close_x = |g: &HorizontalTabGeometry, i: usize| {
                    let bar = painted_bar(lane, header, g);
                    bar.segments[i].close.0
                };
                let x = close_x(&before, closed);
                let held = state.held_after_close(n - 1);
                state.held = held;
                let active = closed.min(n - 2);
                let after = horizontal_tab_geometry_in(
                    lane,
                    scale,
                    header,
                    (n - 1, active),
                    &cfg,
                    (&mut state, true),
                );
                assert_eq!(after.layout.width, before.layout.width, "{case}");
                if closed < n - 1 {
                    assert_eq!(close_x(&after, closed), x, "{case}: the next close button");
                }
                // Held again on the next frame, while the pointer stays.
                let again = horizontal_tab_geometry_in(
                    lane,
                    scale,
                    header,
                    (n - 1, active),
                    &cfg,
                    (&mut state, true),
                );
                assert_eq!(again.layout.xs, after.layout.xs, "{case}");
                // The pointer leaves: an even split of the lane again.
                let left = horizontal_tab_geometry_in(
                    lane,
                    scale,
                    header,
                    (n - 1, active),
                    &cfg,
                    (&mut state, false),
                );
                let even = (left.strip - TITLEBAR_DRAG_LANE * scale) / (n - 1) as f32;
                assert!((left.layout.width - even).abs() < 0.01, "{case}");
                assert!(state.held.is_none(), "{case}");
            }
        }
    }
    // Scrolled to the end, a close keeps the tabs where they are and leaves
    // the last slot empty rather than shifting every tab under the pointer.
    let lane = lane_of(1000.0, 1.0);
    let mut state = TitlebarStrip::default();
    let before = titlebar_tab_layout(20, 19, &lane, &mut state, true);
    let first = state.first;
    // Tab 17 closes; the active 19th tab is the 18th now.
    state.held = state.held_after_close(19);
    let after = titlebar_tab_layout(19, 18, &lane, &mut state, true);
    assert_eq!(after.xs[17], before.xs[17], "the next tab moved up");
    assert_eq!(after.xs[18], before.xs[18]);
    assert_eq!(after.xs[first], before.xs[first]);
    assert_eq!(after.arrow_right, before.arrow_right);
    let released = titlebar_tab_layout(19, 18, &lane, &mut state, false);
    assert_eq!(state.first, 19 - state.slots);
    assert_ne!(released.xs[first], before.xs[first]);
}

/// Whichever caller's layout lets a hold go (moving onto the traffic
/// lights first lays the strip out for the window's hold on AppKit), it asks
/// for a frame, so the even split the hit tests now use is also painted.
#[test]
fn letting_go_of_a_hold_asks_for_a_frame() {
    let source: String = include_str!("chrome.rs").split_whitespace().collect();
    let body = source
        .split_once("fntab_bar_at_height(")
        .unwrap()
        .1
        .split_once("fntitle_edit_rect(")
        .unwrap()
        .0;
    assert!(body.contains("letheld=strip_state.held.is_some();letHorizontalTabGeometry{"));
    assert!(body.contains(
        "ws.titlebar_strip.set(strip_state);ifheld&&strip_state.held.is_none()&&letSome(window)=&ws.window{window.request_redraw();}"
    ));
}

/// A double-click counts only when both presses land on the same spot of
/// the screen. A press on the title bar that drags the window keeps the
/// pointer at the same spot of the window, so a window move (and the
/// pointer leaving, as a compositor's move grab reports) forgets the press.
#[test]
fn moving_the_window_forgets_a_title_bar_press() {
    let source: String = include_str!("window_events.rs")
        .split_whitespace()
        .collect();
    for arm in [
        "WindowEvent::Moved(pos)=>{",
        "WindowEvent::CursorLeft{..}=>{",
    ] {
        let body = source
            .split_once(arm)
            .unwrap_or_else(|| panic!("no {arm}"))
            .1
            .split_once("WindowEvent::")
            .unwrap()
            .0;
        assert!(body.contains("ws.titlebar_click=None;"), "{arm}");
    }
}

/// A hold lasts only while the tabs and the strip are the ones it was made
/// for: a new tab, a close from the keyboard (or by a pane exiting), a
/// resize and a display-scale change all end it, and so does scrolling.
#[test]
fn a_hold_ends_with_a_new_tab_another_close_a_resize_or_a_scroll() {
    let lane = lane_of(1280.0, 1.0);
    let held_layout = |state: &TitlebarStrip, n: usize, lane: &TitlebarLane| {
        let mut held = *state;
        held.held = held.held_after_close(n);
        titlebar_tab_layout(n, 0, lane, &mut held, true);
        held
    };
    let mut state = TitlebarStrip::default();
    titlebar_tab_layout(4, 0, &lane, &mut state, true);
    let held = held_layout(&state, 3, &lane);
    assert!(held.held.is_some());
    for (n, strip) in [(4, 1280.0), (2, 1280.0), (3, 1000.0), (3, 2560.0)] {
        let mut s = held;
        let l = titlebar_tab_layout(n, 0, &lane_of(strip, strip / 1280.0), &mut s, true);
        assert!(s.held.is_none(), "{n} tabs in {strip}");
        let even = (strip - TITLEBAR_DRAG_LANE * strip / 1280.0) / n as f32;
        assert!((l.width - even).abs() < 0.01, "{n} tabs in {strip}");
    }
    let mut scrolled = TitlebarStrip::default();
    let lane = lane_of(1000.0, 1.0);
    titlebar_tab_layout(20, 19, &lane, &mut scrolled, true);
    scrolled.held = scrolled.held_after_close(20);
    assert!(scrolled.scroll(-1, 20) && scrolled.held.is_none());
    // The events that end a hold.
    let source: String = super::production_source().split_whitespace().collect();
    for (event, expected) in [
        (
            "WindowEvent::Resized(size)=>{",
            "ws.release_tab_hold();//Anyresize,aheight-onlyonetoo,bringstheactivetab//backintoascrolledtitle-barstrip.ws.reveal_active_tab();",
        ),
        (
            "WindowEvent::ScaleFactorChanged{scale_factor,..}=>{",
            "ws.release_tab_hold();ws.reveal_active_tab();",
        ),
        (
            "fnfire_tab_add_event(&mutself,ws:&mutWindowState){",
            "//Anewtabendsanyholdaclosewiththepointerleft.ws.release_tab_hold();",
        ),
        (
            "fnfire_tab_close_event(&mutself,ws:&mutWindowState,closing_idx:usize){",
            "//Everycloseendsahold;aclosewiththepointersetsanewone//afterthis(`hold_tabs_after_pointer_close`).ws.release_tab_hold();",
        ),
    ] {
        let after = source
            .split_once(event)
            .unwrap_or_else(|| panic!("no {event}"))
            .1;
        assert!(after.starts_with(expected), "{event}");
    }
    let state: String = include_str!("../window_state.rs")
        .split_whitespace()
        .collect();
    assert!(state.contains("pub(crate)fnleave_tab_bar(&mutself){self.release_tab_hold();"));
}

/// Native and ctl presses on an arrow go to one place, which scrolls a
/// title-bar strip and moves a plain strip's selection; both close a tab
/// with the pointer the same way and then hold the strip; the wheel and a
/// horizontal swipe scroll an overflowing title-bar strip before cycling
/// tabs; and choosing a tab from the keyboard reveals it.
#[test]
fn pointer_and_keyboard_paths_scroll_hold_and_reveal_the_titlebar_strip() {
    let source: String = super::production_source().split_whitespace().collect();
    assert_eq!(
        source
            .matches("self.press_tab_scroll_arrow(ws,&bar,false);")
            .count(),
        2,
        "native and ctl `‹`"
    );
    assert_eq!(
        source
            .matches("self.press_tab_scroll_arrow(ws,&bar,true);")
            .count(),
        2,
        "native and ctl `›`"
    );
    assert_eq!(
        source
            .matches("letbefore=ws.titlebar_strip.get();ifws.mux.close_tab_at(seg.idx){")
            .count(),
        2
    );
    assert_eq!(
        source
            .matches("self.fire_tab_close_event(ws,closing_idx);self.note_focus_change(ws,pre);hold_tabs_after_pointer_close(ws,&bar,before);")
            .count(),
        2
    );
    assert!(source.contains("ifstep!=0&&self.scroll_titlebar_strip(ws,ifstep>0{-1}else{1}){"));
    assert!(source.contains(
        "ws.mux.active=i;ws.mux.touch_active_tab_seen();//Eventhetabalreadyactive,scrolledoutofa//title-barstrip,comesbackintoview.ws.reveal_active_tab();"
    ));
    // Only an overflowing title-bar strip takes the wheel from the selection.
    let mut bar = TabBar::hidden();
    bar.in_titlebar = true;
    assert!(!wheel_scrolls_strip(&bar));
    bar.scroll_left = (78.0, 0.0, 32.0, 32.0);
    assert!(wheel_scrolls_strip(&bar));
    bar.in_titlebar = false;
    assert!(!wheel_scrolls_strip(&bar));
}

/// Hit tests read the painted strip: each tab shown is a control where it is
/// painted (a press there reorders or tears it off), the arrows are controls,
/// and the drag lane is empty title bar that moves the window, also while
/// the strip scrolls. Reorder targets, dock insertion and the insert marker
/// follow the tabs on screen, and tabs scrolled away stay in the
/// accessibility tree without bounds.
#[test]
fn titlebar_hit_tests_follow_the_painted_tabs() {
    use super::titlebar::{empty_titlebar_space, titlebar_control_at};
    let cfg = Config::default();
    let tr = kettle_i18n::Translator::default();
    for scale in [1.0_f32, 2.0] {
        for (name, lane, header) in lanes(1000.0, scale) {
            for n in [2_usize, 5, 30] {
                let active = n - 1;
                let g = horizontal_tab_geometry(lane, scale, header, (n, active), &cfg);
                let bar = painted_bar(lane, header, &g);
                let case = format!("{name} @{scale}x, {n} tabs");
                let y = lane.1 + lane.3 * 0.5;
                let shown: Vec<&TabSeg> = bar.segments.iter().filter(|s| tab_shown(s)).collect();
                for seg in &shown {
                    let mid = seg.rect.0 + seg.rect.2 * 0.5;
                    assert!(titlebar_control_at(&bar, mid, y), "{case}: tab {}", seg.idx);
                    assert_eq!(
                        tab_drag_target_index(mid, y, &bar.segments),
                        seg.idx,
                        "{case}"
                    );
                    assert_eq!(
                        dock_insertion_tab(&bar.segments, false, seg.rect.0 + 1.0),
                        Some(seg.idx),
                        "{case}"
                    );
                    assert_eq!(
                        g.layout.xs[seg.idx].map(|x| lane.0 + g.lead + x),
                        Some(
                            lane.0 + g.lead + horizontal_tab_insert_x(&g.layout, seg.idx, g.strip)
                        ),
                        "{case}: insert marker before tab {}",
                        seg.idx
                    );
                }
                for arrow in [bar.scroll_left, bar.scroll_right] {
                    if arrow.2 > 0.0 {
                        assert!(
                            titlebar_control_at(&bar, arrow.0 + arrow.2 * 0.5, y),
                            "{case}"
                        );
                    }
                }
                // The drag lane, whole while the strip scrolls too, is
                // empty title bar.
                let end = painted_end(lane, &g);
                let controls = trailing_controls(lane, &g);
                assert!(
                    controls - end >= TITLEBAR_DRAG_LANE * scale - 0.01,
                    "{case}: {} before the controls",
                    controls - end
                );
                let lane_mid = (end + controls) * 0.5;
                assert!(
                    empty_titlebar_space(&bar, lane_mid, y),
                    "{case}: {lane_mid}"
                );
                assert!(!titlebar_control_at(&bar, lane_mid, y), "{case}");
                // A drop past the last tab shown lands after it.
                let last = shown.last().unwrap();
                assert_eq!(
                    dock_insertion_tab(&bar.segments, false, lane_mid),
                    Some(last.idx + 1),
                    "{case}"
                );
                let anchors: Vec<u64> = (0..n as u64).map(|i| 100 + i).collect();
                let nodes = tab_accessibility_projection(&bar, &anchors, false, &tr);
                let tabs: Vec<_> = nodes
                    .iter()
                    .filter(|(_, node)| node.role() == accesskit::Role::Tab)
                    .collect();
                assert_eq!(tabs.len(), n, "{case}: every tab is in the tree");
                assert_eq!(
                    tabs.iter()
                        .filter(|(_, node)| node.bounds().is_some())
                        .count(),
                    shown.len(),
                    "{case}"
                );
            }
        }
    }
}

/// On macOS a lone tab still stands for the window's title: its body is not
/// a control, so dragging it moves the window, and a second press there
/// within the double-click interval runs the title bar's double-click
/// action. Its close button, which shows on hover, is a control. The GNOME
/// header's lone title is title space of its own, and two tabs are tabs.
#[test]
fn a_lone_macos_titlebar_tab_stands_for_the_window_title() {
    use super::titlebar::{lone_titlebar_tab_at, titlebar_control_at, titlebar_double_click};
    let cfg = Config::default();
    let lane = mac_lane(1280.0, 2.0, false);
    let g = horizontal_tab_geometry(lane, 2.0, (false, true), (1, 0), &cfg);
    let mut bar = painted_bar(lane, (false, true), &g);
    let tab = bar.segments[0].rect;
    let y = tab.1 + tab.3 * 0.5;
    // The idle tab hides its close button; hovered, it shows one.
    let hovered =
        kettle_render::horizontal_tab_content_layout(tab, true, true, 0.0, "shell", None, 16.0);
    bar.segments[0].close = hovered.close;
    let close = hovered.close;
    assert!(close.2 > 0.0);
    for x in [tab.0 + 2.0, tab.0 + tab.2 * 0.5, close.0 - 1.0] {
        assert!(lone_titlebar_tab_at(&bar, x, y), "{x}");
        assert!(!titlebar_control_at(&bar, x, y), "{x}");
    }
    let (cx, cy) = (close.0 + close.2 * 0.5, close.1 + close.3 * 0.5);
    assert!(!lone_titlebar_tab_at(&bar, cx, cy));
    assert!(titlebar_control_at(&bar, cx, cy));
    // Past the tab, in the drag lane: empty title bar, not the tab.
    assert!(!lone_titlebar_tab_at(&bar, tab.0 + tab.2 + 4.0, y));
    let mut header = painted_bar(lane, (false, true), &g);
    header.linux_header = true;
    assert!(!lone_titlebar_tab_at(&header, tab.0 + 2.0, y));
    let two = painted_bar(
        lane,
        (false, true),
        &horizontal_tab_geometry(lane, 2.0, (false, true), (2, 0), &cfg),
    );
    assert!(!lone_titlebar_tab_at(&two, two.segments[0].rect.0 + 2.0, y));
    // Two presses within the interval and 4 points of each other.
    let t = std::time::Instant::now();
    let interval = std::time::Duration::from_millis(500);
    let first = Some((t, 300.0, 30.0));
    let later = |ms| t + std::time::Duration::from_millis(ms);
    assert!(titlebar_double_click(
        first,
        later(300),
        (304.0, 26.0),
        interval,
        4.0
    ));
    assert!(!titlebar_double_click(
        first,
        later(600),
        (300.0, 30.0),
        interval,
        4.0
    ));
    assert!(!titlebar_double_click(
        first,
        later(100),
        (309.0, 30.0),
        interval,
        4.0
    ));
    assert!(!titlebar_double_click(
        None,
        later(100),
        (300.0, 30.0),
        interval,
        4.0
    ));
    // The press path asks before the tab takes the press.
    let source: String = include_str!("titlebar.rs").split_whitespace().collect();
    assert!(source.contains(
        "letempty=ws.macos_titlebar_tabs&&empty_titlebar_space(bar,x,y);letlone=ws.macos_titlebar_tabs&&lone_titlebar_tab_at(bar,x,y);"
    ));
    assert!(
        source.contains("ifdouble{native::double_click(window);returntrue;}iflone{returnfalse;}")
    );
}
