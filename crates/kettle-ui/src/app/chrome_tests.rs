#[test]
fn horizontal_single_tab_controls_use_count_and_physical_hover() {
    let source = include_str!("chrome.rs");
    let horizontal = source
        .split_once("fn tab_bar_at_height(")
        .unwrap()
        .1
        .split_once("pub(super) fn title_edit_rect(")
        .unwrap()
        .0;
    assert!(horizontal.contains("let show_controls = n > 1 || hovered;"));
    assert!(horizontal.contains("rect,\n                    show_controls,"));
    assert!(horizontal.contains("rect_contains(rect, p.x as f32, p.y as f32)"));
}

#[test]
fn horizontal_single_tab_controls_match_hit_tests_and_accesskit() {
    for in_titlebar in [false, true] {
        for bottom in [false, true] {
            for count in [1, 2] {
                for hovered in [false, true] {
                    for close_enabled in [false, true] {
                        let mut bar = kettle_render::TabBar::hidden();
                        bar.height = 28.0;
                        bar.in_titlebar = in_titlebar;
                        for idx in 0..count {
                            let rect = (
                                78.0 + idx as f32 * 320.0,
                                if bottom { 572.0 } else { 0.0 },
                                320.0,
                                28.0,
                            );
                            let hint = format!("⌘{}", idx + 1);
                            let content = kettle_render::horizontal_tab_content_layout(
                                rect,
                                count > 1 || hovered,
                                close_enabled,
                                false,
                                "shell",
                                Some(&hint),
                                8.0,
                            );
                            bar.segments.push(kettle_render::TabSeg {
                                idx,
                                rect,
                                title_rect: content.title,
                                hint: content.hint.map(|r| (r, hint)),
                                close: content.close,
                                title: "shell".into(),
                                path: None,
                                active: idx == 0,
                                activity: kettle_render::TabActivity::Normal,
                            });
                        }
                        let nodes = super::tab_accessibility_projection(
                            &bar,
                            &[10, 20],
                            false,
                            &kettle_i18n::Translator::default(),
                        );
                        for seg in &bar.segments {
                            let visible = count > 1 || hovered;
                            assert_eq!(seg.hint.is_some(), visible);
                            let closes = visible && close_enabled;
                            assert_eq!(
                                super::hovered_close_button(
                                    &bar.segments,
                                    seg.rect.0 + 306.0,
                                    seg.rect.1 + 14.0,
                                ),
                                closes.then_some(seg.idx)
                            );
                            let close_id = super::accessibility_tab_id([10, 20][seg.idx], true);
                            assert_eq!(nodes.iter().any(|(id, _)| *id == close_id), closes);
                        }
                    }
                }
            }
        }
    }
    let ctl = include_str!("ctl.rs");
    for field in [
        "\"close\": rect_json(seg.close)",
        "\"hint\": seg.hint.as_ref()",
        "\"hovered\": bar.hovered_tab_idx == Some(seg.idx)",
    ] {
        assert!(
            ctl.contains(field),
            "ui_geometry must report the shared tab state"
        );
    }
}

#[test]
fn shortcut_hint_layout_uses_natural_chrome_text_advance() {
    let src = include_str!("chrome.rs");
    let horizontal = src
        .split_once("fn tab_bar_at_height(")
        .unwrap().1
        .split_once("pub(super) fn title_edit_rect(")
        .unwrap().0;
    assert!(
        horizontal.contains("let cell_w = self.overlay_text_cell_width(ws);"),
        "Cmd+1 is shaped at natural font metrics, so its lane must use the chrome text advance"
    );
    assert!(!horizontal.contains("let cell_w = Self::cell_size(ws)"));
}

#[test]
fn about_never_claims_current_without_a_verified_result() {
    // No record and no install facts: About says the status is unknown
    // rather than up to date.
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    assert!(
        crate::update_notice::fact_rows(&crate::update_notice::UpdateFacts::default(), &tr)
            .is_empty()
    );
    assert_eq!(
        tr.text(kettle_i18n::Text::AboutUpdateUnknown),
        "Update status unknown"
    );
}

/// Plugins keep per-pane state until `pane_close`, so every pane the mux
/// destroys itself is reported for one, exactly once.
#[test]
fn reaped_and_tab_closed_panes_are_reported_for_pane_close() {
    let (mut mux, target, sibling) = title_test_split_mux();
    assert!(mux.take_closed_panes().is_empty());
    mux.panes.get_mut(&target).unwrap().closed = true;
    assert!(!mux.reap().mux_empty);
    assert_eq!(mux.take_closed_panes(), [target]);
    assert!(
        mux.take_closed_panes().is_empty(),
        "each pane is reported once"
    );
    assert!(mux.close_tab(), "closing the only tab empties the mux");
    assert_eq!(mux.take_closed_panes(), [sibling]);
}

#[test]
fn cursor_in_tab_bar_band_geometry() {
    use super::{TabBarPos, cursor_in_tab_bar_band};
    // 600 px window, 24 px tab bar. Top: [0, 24).
    assert!(cursor_in_tab_bar_band(0.0, 24.0, 600.0, TabBarPos::Top));
    assert!(cursor_in_tab_bar_band(23.0, 24.0, 600.0, TabBarPos::Top));
    assert!(!cursor_in_tab_bar_band(24.0, 24.0, 600.0, TabBarPos::Top));
    assert!(!cursor_in_tab_bar_band(300.0, 24.0, 600.0, TabBarPos::Top));
    // Bottom: [576, 600].
    assert!(cursor_in_tab_bar_band(
        576.0,
        24.0,
        600.0,
        TabBarPos::Bottom
    ));
    assert!(cursor_in_tab_bar_band(
        600.0,
        24.0,
        600.0,
        TabBarPos::Bottom
    ));
    assert!(!cursor_in_tab_bar_band(
        575.0,
        24.0,
        600.0,
        TabBarPos::Bottom
    ));
    assert!(!cursor_in_tab_bar_band(0.0, 24.0, 600.0, TabBarPos::Bottom));
    // Hidden tab bar (`tab-bar = off` or single-tab `auto`) → always
    // out of band so the wheel falls through to scrollback.
    assert!(!cursor_in_tab_bar_band(0.0, 0.0, 600.0, TabBarPos::Top));
    assert!(!cursor_in_tab_bar_band(0.0, 0.0, 600.0, TabBarPos::Bottom));
}

#[test]
fn cursor_in_status_bar_band_geometry() {
    // Drift guard for the status-bar cursor icon, same shape as the
    // `cursor_in_tab_bar_band_geometry` drift guard above. Pins:
    //   - Off mode → always false (status bar invisible).
    //   - Top mode → [0, bar_h).
    //   - Bottom mode → [surface - bar_h, surface].
    //   - bar_h == 0 → false regardless of mode, checked before the mode
    //     (`status_bar_h()` returns 0 for Off).
    use super::cursor_in_status_bar_band;
    use kettle_config::StatusBarMode;
    // Off → always false.
    assert!(!cursor_in_status_bar_band(
        0.0,
        22.0,
        600.0,
        StatusBarMode::Off,
        0.0,
        0.0
    ));
    assert!(!cursor_in_status_bar_band(
        300.0,
        22.0,
        600.0,
        StatusBarMode::Off,
        0.0,
        0.0
    ));
    // Top: [0, 22).
    assert!(cursor_in_status_bar_band(
        0.0,
        22.0,
        600.0,
        StatusBarMode::Top,
        0.0,
        0.0
    ));
    assert!(cursor_in_status_bar_band(
        21.0,
        22.0,
        600.0,
        StatusBarMode::Top,
        0.0,
        0.0
    ));
    assert!(!cursor_in_status_bar_band(
        22.0,
        22.0,
        600.0,
        StatusBarMode::Top,
        0.0,
        0.0
    ));
    assert!(!cursor_in_status_bar_band(
        300.0,
        22.0,
        600.0,
        StatusBarMode::Top,
        0.0,
        0.0
    ));
    // Bottom: [578, 600].
    assert!(cursor_in_status_bar_band(
        578.0,
        22.0,
        600.0,
        StatusBarMode::Bottom,
        0.0,
        0.0
    ));
    assert!(cursor_in_status_bar_band(
        600.0,
        22.0,
        600.0,
        StatusBarMode::Bottom,
        0.0,
        0.0
    ));
    assert!(!cursor_in_status_bar_band(
        577.0,
        22.0,
        600.0,
        StatusBarMode::Bottom,
        0.0,
        0.0
    ));
    assert!(!cursor_in_status_bar_band(
        0.0,
        22.0,
        600.0,
        StatusBarMode::Bottom,
        0.0,
        0.0
    ));
    // bar_h == 0 → out of band regardless of mode.
    assert!(!cursor_in_status_bar_band(
        0.0,
        0.0,
        600.0,
        StatusBarMode::Top,
        0.0,
        0.0
    ));
    assert!(!cursor_in_status_bar_band(
        0.0,
        0.0,
        600.0,
        StatusBarMode::Bottom,
        0.0,
        0.0
    ));
}

#[test]
fn chrome_cursor_icon_overrides_only_for_chrome() {
    use super::chrome_cursor_icon;
    use winit::window::CursorIcon;
    // Content area, no modals → caller picks (None means "use content
    // logic" — Text by default, Pointer over a Ctrl-held link).
    assert_eq!(chrome_cursor_icon(false, false), None);
    // Tab bar → Default arrow (clickable tabs are not selectable text).
    assert_eq!(chrome_cursor_icon(true, false), Some(CursorIcon::Default));
    // Modal up (search / palette / hints / SSH launcher) → Default arrow
    // regardless of where the pointer is, so the I-beam doesn't bleed
    // through onto the overlay.
    assert_eq!(chrome_cursor_icon(false, true), Some(CursorIcon::Default));
    // Both at once (modal opened while pointer happened to be over the
    // tab bar) → still Default.
    assert_eq!(chrome_cursor_icon(true, true), Some(CursorIcon::Default));
}

#[test]
fn hovered_close_button_finds_only_the_close_rect_hits() {
    use super::hovered_close_button;
    use kettle_render::TabSeg;
    // Two tab segments side-by-side, each 100×24 with a trailing
    // 24-px close-button zone at x=76..100 (segment 0) and
    // x=176..200 (segment 1) — mirrors the tab_bar()
    // builder's `(x + seg_w - height)` formula.
    let segs = vec![
        TabSeg {
            idx: 0,
            rect: (0.0, 0.0, 100.0, 24.0),
            title_rect: (8.0, 0.0, 68.0, 24.0),
            hint: None,
            close: (76.0, 0.0, 24.0, 24.0),
            title: "one".into(),
            path: None,
            active: true,
            activity: kettle_render::TabActivity::Normal,
        },
        TabSeg {
            idx: 1,
            rect: (100.0, 0.0, 100.0, 24.0),
            title_rect: (108.0, 0.0, 68.0, 24.0),
            hint: None,
            close: (176.0, 0.0, 24.0, 24.0),
            title: "two".into(),
            path: None,
            active: false,
            activity: kettle_render::TabActivity::Normal,
        },
    ];
    // Cursor over title area of segment 0 → no close hit.
    assert_eq!(hovered_close_button(&segs, 20.0, 12.0), None);
    // Cursor over segment 0's close button → hit on idx 0.
    assert_eq!(hovered_close_button(&segs, 88.0, 12.0), Some(0));
    // Cursor over segment 1's close button → hit on idx 1.
    assert_eq!(hovered_close_button(&segs, 188.0, 12.0), Some(1));
    // Cursor outside the bar entirely → no hit.
    assert_eq!(hovered_close_button(&segs, 88.0, 100.0), None);
    // Edge: cursor exactly at the bar bottom (24.0) is outside
    // because `py < ry + rh` is strict.
    assert_eq!(hovered_close_button(&segs, 88.0, 24.0), None);
    // Empty bar (single-pane, tabs hidden) → no hit ever.
    assert_eq!(hovered_close_button(&[], 50.0, 10.0), None);
}

/// `tab-position = left` / `right` stacks the segments down a shared
/// column, so every one of them contains any x inside the strip. Targeting
/// by the cursor's x would always match the first segment, and dragging a
/// tab anywhere below the top of the strip would move tab 0 instead.
#[test]
fn a_vertical_tab_strip_reorders_by_the_axis_it_is_actually_stacked_on() {
    use super::tab_drag_target_index;
    let seg = |idx, y, h| {
        let rect = (0.0, y, 160.0, h);
        let close = (136.0, y, 24.0, 24.0);
        kettle_render::TabSeg {
            idx,
            rect,
            title_rect: super::tab_title_rect(rect, close, true),
            hint: None,
            close,
            title: format!("tab-{idx}"),
            path: None,
            active: idx == 0,
            activity: kettle_render::TabActivity::Normal,
        }
    };
    let stacked = [seg(0, 0.0, 28.0), seg(1, 28.0, 28.0), seg(2, 56.0, 28.0)];

    // Precondition: every segment spans the same x, which is exactly why
    // an x-axis test could not tell them apart.
    let x_mid = 80.0;
    assert!(
        stacked
            .iter()
            .all(|s| x_mid >= s.rect.0 && x_mid < s.rect.0 + s.rect.2),
        "fixture must share one column, or the old bug can't be reproduced"
    );

    assert_eq!(tab_drag_target_index(x_mid, 14.0, &stacked), 0);
    assert_eq!(tab_drag_target_index(x_mid, 42.0, &stacked), 1);
    assert_eq!(tab_drag_target_index(x_mid, 70.0, &stacked), 2);
    // Boundaries follow the rendered rects, same rule as horizontal.
    assert_eq!(tab_drag_target_index(x_mid, 28.0, &stacked), 1);
    assert_eq!(tab_drag_target_index(x_mid, 56.0, &stacked), 2);
    // Past either end clamps to the nearest segment rather than running off.
    assert_eq!(tab_drag_target_index(x_mid, -40.0, &stacked), 0);
    assert_eq!(tab_drag_target_index(x_mid, 9_000.0, &stacked), 2);
    // The x coordinate is now irrelevant on a vertical strip — including
    // one far outside it, which a torn-drag produces.
    assert_eq!(tab_drag_target_index(-999.0, 42.0, &stacked), 1);
}

/// Drift guard: the drag strip and `tab_bar()`'s segment layout
/// share this width, so a tab can be dragged all the way to the last slot.
#[test]
fn tab_segment_strip_width_excludes_both_buttons() {
    use super::tab_segment_strip_width;
    // plus_w = arrow_w = height = 24, surface 800 → strip excludes BOTH
    // buttons (the `▾ +` pair), 800 - 48 = 752, not 800 - 24 = 776.
    assert_eq!(tab_segment_strip_width(800.0, 24.0, 24.0), 752.0);
    // No dropdown (vertical bar): arrow_w = 0 → only `+` excluded.
    assert_eq!(tab_segment_strip_width(800.0, 24.0, 0.0), 776.0);
    // A degenerate strip has no room for tabs; controls keep their reservation.
    assert_eq!(tab_segment_strip_width(20.0, 24.0, 24.0), 0.0);
}

#[test]
fn tab_close_hover_icon_overrides_chrome_default() {
    use super::{chrome_cursor_icon, tab_close_hover_icon};
    use winit::window::CursorIcon;
    // Not over a close button → no override; chrome decision wins.
    assert_eq!(tab_close_hover_icon(false), None);
    let chrome = chrome_cursor_icon(true, false);
    // Hover a close ✕ → Pointer (the browser-tab convention).
    assert_eq!(tab_close_hover_icon(true), Some(CursorIcon::Pointer));
    // Compose: close-hover wins over the chrome Default — once
    // the cursor lands on a clickable affordance we want the
    // hand, not the arrow.
    assert_eq!(
        tab_close_hover_icon(true).or(chrome),
        Some(CursorIcon::Pointer)
    );
}

/// Drift guard for `tab_strip_layout`: tabs ALWAYS fill the bar (even
/// division, no max cap), then floor at min-width + overflow-scroll, and
/// the active tab stays visible.
#[test]
fn tab_strip_layout_fills_and_overflows() {
    use super::tab_strip_layout;
    // (No trailing button reserve here so this drift guard keeps exercising
    // the fill/overflow behavior directly; the full-width-aligned path with
    // a real reserve is covered by the alignment test below.)
    // Few tabs on a wide strip FILL the bar — 2 tabs each take half (no max
    // cap), left-packed, no arrows.
    let l = tab_strip_layout(2, 0, 1920.0, 0.0, 24.0, 120.0, true);
    assert_eq!(l.width, 960.0);
    assert_eq!(l.xs, vec![Some(0.0), Some(960.0)]);
    assert!(l.arrow_left.is_none() && l.arrow_right.is_none());
    // Even division also fills for more tabs while above the min floor.
    assert_eq!(
        tab_strip_layout(4, 0, 800.0, 0.0, 24.0, 120.0, true).width,
        200.0
    );
    // Many tabs: overflow → min width, arrows, only whole tabs shown.
    let l = tab_strip_layout(20, 0, 800.0, 0.0, 24.0, 120.0, true);
    assert_eq!(l.width, 120.0);
    assert!(l.arrow_left.is_some() && l.arrow_right.is_some());
    let visible = l.xs.iter().filter(|x| x.is_some()).count();
    assert!(
        (1..20).contains(&visible),
        "some tabs scroll off: {visible}"
    );
    assert_eq!(l.xs[0], Some(24.0), "active tab 0 visible at the left");
    // Active near the end is pinned into view; early tabs scroll off.
    let l = tab_strip_layout(20, 19, 800.0, 0.0, 24.0, 120.0, true);
    assert!(l.xs[19].is_some(), "active tab 19 must be visible");
    assert!(l.xs[0].is_none(), "early tabs scrolled off");
    // Scrolling disabled: no arrows; width may fall below min (just shrinks).
    let l = tab_strip_layout(20, 0, 800.0, 0.0, 24.0, 120.0, false);
    assert!(l.arrow_left.is_none() && l.arrow_right.is_none() && l.width < 120.0);
    // Degenerate (no tabs): panic-safe, one slot.
    assert_eq!(
        tab_strip_layout(0, 0, 100.0, 0.0, 24.0, 120.0, true)
            .xs
            .len(),
        1
    );
}

/// A side tab bar shows the active tab and `+` however many tabs there are.
/// It once stacked every tab from the top and clamped `+` to the bottom row,
/// so past a screenful the active tab was drawn below the window and `+`
/// covered a tab.
#[test]
fn side_tab_rows_keep_the_active_tab_and_plus_in_view() {
    use super::{SideTabRows, side_tab_rows};
    let fits = |n| SideTabRows {
        first: 0,
        shown: n,
        top: 0,
        plus: n,
        arrows: false,
    };
    // Every tab and `+` fit: tabs from the top, `+` right below the last.
    assert_eq!(side_tab_rows(3, 2, 13), fits(3));
    assert_eq!(side_tab_rows(12, 11, 13), fits(12));
    // One more and the strip scrolls: `⌃`, ten tabs, `⌄`, then `+` on the
    // last row, with the window ending at the active tab past page one.
    let scrolled = |first| SideTabRows {
        first,
        shown: 10,
        top: 1,
        plus: 12,
        arrows: true,
    };
    assert_eq!(side_tab_rows(13, 0, 13), scrolled(0));
    assert_eq!(side_tab_rows(13, 9, 13), scrolled(0));
    assert_eq!(side_tab_rows(13, 10, 13), scrolled(1));
    assert_eq!(side_tab_rows(13, 12, 13), scrolled(3));
    // Too short for the arrows: the active tab and `+`.
    assert_eq!(
        side_tab_rows(5, 4, 2),
        SideTabRows {
            first: 4,
            shown: 1,
            top: 0,
            plus: 1,
            arrows: false,
        }
    );
    for n in 1..48 {
        for rows in 0..20 {
            for active in 0..n {
                let l = side_tab_rows(n, active, rows);
                assert!(
                    (l.first..l.first + l.shown).contains(&active) && l.first + l.shown <= n,
                    "{n} tabs, {rows} rows, active {active}: {l:?}"
                );
                if rows < 2 {
                    // One row or less: the active tab alone, `+` left out.
                    assert!(
                        l.shown == 1 && l.plus >= rows,
                        "{n} tabs, {rows} rows: {l:?}"
                    );
                    continue;
                }
                assert!(l.plus < rows, "{n} tabs, {rows} rows: + below the strip");
                let arrow_rows = usize::from(l.arrows);
                assert_eq!(l.top, arrow_rows);
                assert!(
                    l.top + l.shown + arrow_rows <= l.plus,
                    "{n} tabs, {rows} rows: + or ⌄ covers a tab: {l:?}"
                );
            }
        }
    }
}

/// The side bar lays its rows out through `side_tab_rows` and parks the tabs
/// it does not show, rather than stacking every tab from the top.
#[test]
fn the_side_tab_bar_places_rows_through_side_tab_rows() {
    let src = production_source();
    let body = src
        .split("fn tab_bar_vertical(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("tab_bar_vertical body");
    assert!(body.contains("let layout = side_tab_rows(labels.len(), active, rows);"));
    assert!(body.contains("row_y(layout.top + i - layout.first)"));
    assert!(!body.contains("i as f32 * height"));
    // A title editor takes the top row of the window, strip included, so the
    // rows start below it rather than under it.
    assert!(body.contains("let lead = self.vertical_title_edit_h(ws);"));
    assert!(body.contains("let row_y = |row: usize| strip_y + lead + row as f32 * height;"));
    assert!(body.contains("let strip_rows = ((sh - lead) / height).floor()"));
    // A strip with no room keeps its plus button out of view.
    assert!(body.contains("let plus_y = if layout.plus < rows {"));
    assert!(body.contains("let rows = strip_rows;"));
    // A narrowed horizontal tab has no close button reaching past it.
    let horizontal = src
        .split("fn tab_bar_at_height(&self, ws: &WindowState, height: f32) -> TabBar {")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("tab_bar body");
    assert!(horizontal.contains("kettle_render::horizontal_tab_content_layout("));
}

/// A strip with less than `tab-min-width` between its scroll arrows still
/// keeps the active tab between them. It was once drawn at full width over
/// `›`, `▾` and `+` in a window about 120 px wide.
#[test]
fn an_overflowing_tab_strip_keeps_its_tabs_between_the_arrows() {
    use super::tab_strip_layout;
    for strip in [12.0_f32, 30.0, 71.5, 100.0, 167.0, 400.0, 1000.0] {
        for n in [2_usize, 5, 40] {
            for active in [0, n / 2, n - 1] {
                let l = tab_strip_layout(n, active, strip, 48.5, 24.25, 120.0, true);
                let Some(right) = l.arrow_right else {
                    continue;
                };
                let arrow_w = strip - right;
                assert!(l.xs[active].is_some(), "{strip} px, {n} tabs: active shown");
                for (i, x) in l.xs.iter().enumerate() {
                    let Some(x) = *x else { continue };
                    let w = if i + 1 == n { l.last_width } else { l.width };
                    assert!(
                        x >= arrow_w - 0.01 && x + w <= right + 0.01,
                        "{strip} px, {n} tabs, active {active}: tab {i} at {x}..{} leaves {arrow_w}..{right}",
                        x + w
                    );
                }
            }
        }
    }
}

/// The 2-tab boundary must line up with a centered vertical pane split so
/// the split divider visually continues the tab boundary. With the trailing
/// `▾`/`+` reservation, tabs divide the FULL bar width and only the last tab
/// is shortened.
#[test]
fn tab_strip_layout_aligns_boundary_with_split_seam() {
    use super::{tab_segment_strip_width, tab_strip_layout};
    let w = 1000.0_f32; // window / content width
    let h = 24.0_f32; // tab-bar height == each button's side
    let reserve = 2.0 * h; // `▾` + `+`
    let strip = tab_segment_strip_width(w, h, h); // button-trimmed strip
    let l = tab_strip_layout(2, 0, strip, reserve, h, 120.0, true);
    // tab1/tab2 boundary at the window centre …
    assert_eq!(l.xs, vec![Some(0.0), Some(w / 2.0)]);
    // … which is exactly where a 50/50 vertical split's divider sits
    // (`mux::Node::layout`: `x + round(content_w * ratio)`, content_w == w).
    assert_eq!(l.xs[1], Some((w * 0.5).round()));
    // tab 1 is a full half; tab 2 yields the button reservation.
    assert_eq!(l.width, w / 2.0);
    assert_eq!(l.last_width, w / 2.0 - reserve);
    // The last tab's right edge stops exactly at the button strip.
    assert_eq!(l.xs[1].unwrap() + l.last_width, strip);
    // A window too narrow for the shortened last tab to clear min_w falls
    // back to plain strip packing (no zero/negative last tab).
    let narrow = tab_strip_layout(2, 0, 200.0, reserve, h, 120.0, true);
    assert!(narrow.last_width > 0.0 && (narrow.last_width - narrow.width).abs() < 0.01);
}

/// Drift guard. `split_new_tab_button` splits the trailing
/// button into the `▾` arrow (left) and `+` (right) hit-rects; they must
/// abut with no gap/overlap, and a degenerate `arrow_w` can't yield a
/// negative `+` width.
#[test]
fn split_new_tab_button_places_arrow_left_of_plus() {
    use super::{
        NewTabHoverTarget, new_tab_hover_target, split_new_tab_button,
        tab_bar_pointer_region_contains,
    };
    // button at x=200, 52 wide (two 26-px halves), arrow 26 wide.
    let (arrow, plus) = split_new_tab_button((200.0, 0.0, 52.0, 26.0), 26.0);
    assert_eq!(arrow, (200.0, 0.0, 26.0, 26.0)); // arrow on the LEFT
    assert_eq!(plus, (226.0, 0.0, 26.0, 26.0)); // plus on the RIGHT
    // They abut exactly (no gap, no overlap).
    assert_eq!(arrow.0 + arrow.2, plus.0);
    // Degenerate: arrow wider than the button → clamped, plus width >= 0.
    let (a2, p2) = split_new_tab_button((0.0, 0.0, 20.0, 10.0), 999.0);
    assert_eq!(a2.2, 20.0);
    assert_eq!(p2.2, 0.0);
    assert!(p2.2 >= 0.0);

    let mut bar = kettle_render::TabBar::hidden();
    bar.new_tab_menu = arrow;
    bar.new_tab = plus;
    assert_eq!(
        new_tab_hover_target(&bar, 213.0, 13.0),
        Some(NewTabHoverTarget::Menu)
    );
    assert_eq!(
        new_tab_hover_target(&bar, 239.0, 13.0),
        Some(NewTabHoverTarget::Plus)
    );
    assert_eq!(new_tab_hover_target(&bar, 199.9, 13.0), None);
    assert_eq!(new_tab_hover_target(&bar, 252.1, 13.0), None);

    // Horizontal bars occupy one row; vertical bars keep the same row
    // height but expose a full-height interaction band. The latter is the
    // geometry that makes lower tab rows and their trailing `+` clickable.
    bar.height = 26.0;
    bar.band = (0.0, 0.0, 252.0, 26.0);
    assert!(tab_bar_pointer_region_contains(&bar, 239.0, 13.0));
    assert!(!tab_bar_pointer_region_contains(&bar, 239.0, 27.0));
    bar.band = (200.0, 0.0, 52.0, 400.0);
    bar.new_tab = (200.0, 78.0, 52.0, 26.0);
    assert!(tab_bar_pointer_region_contains(&bar, 226.0, 91.0));
    assert_eq!(
        new_tab_hover_target(&bar, 226.0, 91.0),
        Some(NewTabHoverTarget::Plus)
    );
    // Adding a vertical tab moves the button down one row. Re-running the
    // same hit-test against the new layout must clear the stale hover even
    // though the physical pointer has not moved.
    bar.new_tab = (200.0, 104.0, 52.0, 26.0);
    assert_eq!(new_tab_hover_target(&bar, 226.0, 91.0), None);
    assert!(!tab_bar_pointer_region_contains(&bar, 199.9, 91.0));
}

/// Every user-visible tab-add path funnels through this helper, so keep the
/// post-layout hover refresh beside the Lua/recording event rather than
/// relying on each input route to remember it independently.
#[test]
fn tab_add_event_recomputes_chrome_hover_after_layout_changes() {
    let src = production_source();
    let body = src
        .split("fn fire_tab_add_event(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("fire_tab_add_event body");
    assert!(
        body.contains("self.sync_cursor_icon(ws);"),
        "adding a tab must recompute hover after a vertical new-tab button moves"
    );
}

#[test]
fn tab_bar_controls_reconcile_focus_owners() {
    let src = production_source();
    let controls = src
        .split("} else if bcode == 0 && in_bar(bar.new_tab, px, py) {")
        .nth(1)
        .unwrap()
        .split("} else if let Some(seg) =")
        .next()
        .unwrap();
    assert_eq!(
        controls.matches("self.note_focus_change(ws, pre);").count(),
        3,
        "plus and both overflow arrows must reconcile focus owners"
    );
    assert_eq!(controls.matches("let pre = self.focus_key(ws);").count(), 3);
}

#[test]
fn finding_narrow_vertical_tabs_remain_selectable() {
    let source = include_str!("chrome.rs");
    let body = finding_body(source, "tab_bar_vertical");
    assert!(
        body.contains("self.cfg.close_button_on_tab && strip_w >= height"),
        "a close button must fit inside its vertical tab"
    );
    assert!(body.contains("tab_title_rect(rect, close, closable)"));
}

#[test]
fn docking_append_marker_follows_the_last_visible_tab() {
    let marker = kettle_render::tab_drag::INSERT_MARKER_PX;
    let layout = super::tab_strip_layout(2, 0, 400.0, 48.0, 24.0, 120.0, true);
    assert_eq!(
        super::horizontal_tab_insert_x(&layout, 2, 400.0),
        400.0 - marker,
        "append marker must be at the last tab's right edge"
    );
    let layout = super::tab_strip_layout(20, 0, 400.0, 48.0, 24.0, 120.0, true);
    let (last, x) = layout
        .xs
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, x)| x.map(|x| (index, x)))
        .unwrap();
    assert!(last + 1 < layout.xs.len());
    assert_eq!(
        super::horizontal_tab_insert_x(&layout, last + 1, 400.0),
        x + layout.width,
        "an insertion before a hidden tab must follow the visible predecessor"
    );
}

#[test]
fn vertical_tab_band_excludes_status_bands() {
    use kettle_config::TabBarPos;
    for pos in [TabBarPos::Left, TabBarPos::Right] {
        let band = super::tab_band_rect((800, 600), pos, 24.0, 180.0, 78.0, 0.0, 22.0);
        assert_eq!(
            band.1 + band.3,
            500.0,
            "vertical tabs must stop above bottom status and search"
        );
        let top = super::tab_band_rect((800, 600), pos, 24.0, 180.0, 0.0, 22.0, 0.0);
        assert_eq!(top.1, 22.0, "vertical tabs must start below top status");
        assert_eq!(top.3, 578.0);
        assert!(!super::rect_contains(top, top.0 + 10.0, 10.0));
        assert!(super::rect_contains(top, top.0 + 10.0, 30.0));
    }
}

#[test]
fn horizontal_tab_height_is_shared_with_restore_content_and_status_bands() {
    use super::*;
    for cell_height in [12.0, 16.0, 32.0] {
        for pos in [TabBarPos::Top, TabBarPos::Bottom, TabBarPos::Left, TabBarPos::Right] {
            let expected = cell_height + if pos.is_vertical() { 8.0 } else { 12.0 };
            let height = tab_bar_height(cell_height, pos);
            assert_eq!(height, expected);
            let bands = ContentBands::default().for_restore(TabBarMode::Auto, 2, cell_height, pos);
            assert_eq!(bands.tab_bar_h, height);
            if pos.is_vertical() { continue; }
            let area = content_rect_for_with_strip((800, 600), bands, pos, StatusBarMode::Off, 180.0);
            assert_eq!(area.3, 600.0 - height);
            assert_eq!(area.1, if pos == TabBarPos::Top { height } else { 0.0 });
            let band = tab_band_rect((800, 600), pos, height, 180.0, 0.0, 0.0, 22.0);
            assert_eq!(band.3, height);
            assert_eq!(band.1, if pos == TabBarPos::Top { 0.0 } else { 578.0 - height });
            assert!(rect_contains(band, 20.0, band.1 + height - 0.5));
            assert!(!rect_contains(band, 20.0, band.1 + height));
        }
    }
}

#[test]
fn review_narrow_titlebar_strips_park_tabs_without_panicking() {
    for strip in [0.0, 0.5, 1.0, 1.5] {
        for scroll in [true, false] {
            let layout = super::titlebar_tab_layout(2, 1, (strip, 56.0, 78.0), 28.0, 80.0, scroll);
            assert!(layout.xs.iter().all(Option::is_none));
            assert!(layout.arrow_left.is_none() && layout.arrow_right.is_none());
            assert_eq!(layout.width, 0.0);
        }
    }
}

#[test]
fn review_titlebar_preferred_width_preserves_configured_minimum() {
    let layout = super::titlebar_tab_layout(2, 1, (666.0, 56.0, 78.0), 28.0, 600.0, true);
    assert_eq!(layout.width, 600.0);
    assert!(layout.arrow_left.is_some() && layout.arrow_right.is_some());
    assert!(layout.xs[0].is_none());
    assert!(layout.xs[1].is_some());
    let fits = super::titlebar_tab_layout(2, 0, (1400.0, 56.0, 78.0), 28.0, 600.0, true);
    assert!(fits.width >= 600.0 && fits.last_width >= 600.0);
}

#[test]
fn narrow_chrome_keeps_active_title_and_separate_controls_reachable() {
    use super::*;
    for scale in [1.0, 1.5, 2.0] {
        for titlebar in [false, true] {
            for bottom in [false, true] {
                let left = if titlebar { 78.0 * scale } else { 0.0 };
                let y = if bottom { 232.0 * scale } else { 0.0 };
                let band = (left, y, 420.0 * scale - left, 28.0 * scale);
                let (menu, plus, reserve) = horizontal_new_tab_controls(band, scale, true);
                let mut bar = kettle_render::TabBar::hidden();
                bar.height = band.3;
                bar.band = band;
                bar.new_tab = plus;
                bar.new_tab_menu = menu;
                for (rect, target) in [
                    (menu, NewTabHoverTarget::Menu),
                    (plus, NewTabHoverTarget::Plus),
                ] {
                    assert!(rect.2 / scale >= 28.0 && rect.3 / scale >= 28.0);
                    // Check the whole target, including each corner.
                    for dx in [0.01, rect.2 - 0.01] {
                        for dy in [0.01, rect.3 - 0.01] {
                            assert!(tab_bar_pointer_region_contains(
                                &bar,
                                rect.0 + dx,
                                rect.1 + dy
                            ));
                            assert_eq!(
                                new_tab_hover_target(&bar, rect.0 + dx, rect.1 + dy),
                                Some(target)
                            );
                        }
                    }
                }
                assert_eq!(plus.0 - menu.0 - menu.2, 4.0 * scale);
                assert_eq!(420.0 * scale - plus.0 - plus.2, 8.0 * scale);
                assert_eq!(
                    new_tab_hover_target(&bar, menu.0 + menu.2 + scale, y + 14.0 * scale),
                    None
                );
                let nodes = tab_accessibility_projection(
                    &bar,
                    &[],
                    false,
                    &kettle_i18n::Translator::default(),
                );
                for rect in [plus, menu] {
                    let expected = accesskit::Rect::new(
                        rect.0 as f64,
                        rect.1 as f64,
                        (rect.0 + rect.2) as f64,
                        (rect.1 + rect.3) as f64,
                    );
                    assert!(
                        nodes
                            .iter()
                            .any(|(_, node)| node.role() == accesskit::Role::Button
                                && node.bounds() == Some(expected))
                    );
                }
                let geometry = super::ctl::tab_bar_geometry_json(&bar, Vec::new());
                for (key, rect) in [("new_tab", plus), ("new_tab_menu", menu)] {
                    assert_eq!(
                        geometry[key],
                        serde_json::json!({"x":rect.0,"y":rect.1,"width":rect.2,"height":rect.3})
                    );
                }
                for n in [1, 2, 9, 40] {
                    for active in [0, n - 1] {
                        let strip = band.2 - reserve;
                        let layout = if titlebar {
                            titlebar_tab_layout(
                                n,
                                active,
                                (strip, reserve, left),
                                band.3,
                                120.0,
                                true,
                            )
                        } else {
                            tab_strip_layout(n, active, strip, reserve, band.3, 120.0, true)
                        };
                        let x = left + layout.xs[active].expect("active tab stays visible");
                        let width = if active + 1 == n {
                            layout.last_width
                        } else {
                            layout.width
                        };
                        assert!(x + width <= menu.0 - 4.0 * scale + 0.01);
                        let content = kettle_render::horizontal_tab_content_layout(
                            (x, y, width, band.3),
                            n > 1,
                            true,
                            false,
                            "shell",
                            Some("Cmd+1"),
                            8.0 * scale,
                        );
                        assert!(
                            content.title.2 / scale >= 3.0 * 8.0,
                            "readable title for {n} tabs, active {active}"
                        );
                    }
                }
            }
        }
    }
    let ctl = include_str!("ctl.rs");
    for field in [
        "\"new_tab\": rect_json(bar.new_tab)",
        "\"new_tab_menu\": rect_json(bar.new_tab_menu)",
    ] {
        assert!(
            ctl.contains(field),
            "ui_geometry must serialize the shared control rectangles"
        );
    }
}

#[test]
fn new_tab_tooltips_translate_and_follow_the_live_shortcut() {
    use super::{NewTabHoverTarget, new_tab_tooltip};
    for (language, label, options) in [
        (kettle_i18n::Language::En, "New Tab", "New Tab Options"),
        (
            kettle_i18n::Language::Es,
            "Nueva pestaña",
            "Opciones de nueva pestaña",
        ),
    ] {
        let tr = kettle_i18n::Translator::new(language);
        assert_eq!(
            new_tab_tooltip(Some(NewTabHoverTarget::Plus), &tr, Some("Cmd+T")),
            Some(format!("{label}  Cmd+T"))
        );
        assert_eq!(
            new_tab_tooltip(Some(NewTabHoverTarget::Plus), &tr, Some("Ctrl+N")),
            Some(format!("{label}  Ctrl+N"))
        );
        assert_eq!(
            new_tab_tooltip(Some(NewTabHoverTarget::Plus), &tr, None),
            Some(label.into())
        );
        assert_eq!(
            new_tab_tooltip(Some(NewTabHoverTarget::Menu), &tr, None),
            Some(options.into())
        );
        assert_eq!(new_tab_tooltip(None, &tr, Some("Cmd+T")), None);
    }
}

#[test]
fn new_tab_control_targets_grow_with_zoomed_chrome() {
    for scale in [1.0, 2.0] {
        for height in [28.0, 40.0, 84.0] {
            let band = (78.0 * scale, 0.0, 922.0 * scale, height * scale);
            let (menu, plus, _) = super::horizontal_new_tab_controls(band, scale, true);
            for rect in [menu, plus] {
                assert_eq!(rect.2, height * scale);
                assert_eq!(rect.3, height * scale);
                assert!(rect.2 >= 28.0 * scale);
                assert_eq!(rect.1, band.1);
            }
        }
    }
}

#[test]
fn tab_hints_follow_the_go_to_tab_bindings_on_every_platform() {
    // The hints used to exist only on macOS; Linux tabs showed none although
    // Alt+1..9 switch to them.
    let defaults = kettle_config::keybinds::defaults();
    let expected = if cfg!(target_os = "macos") { "⌘1" } else { "Alt+1" };
    assert_eq!(super::chrome::tab_goto_hint(&defaults, 0).as_deref(), Some(expected));
    assert!(super::chrome::tab_goto_hint(&defaults, 8).is_some());
    assert!(super::chrome::tab_goto_hint(&defaults, 9).is_none());
    // A rebound shortcut is the one shown.
    let parsed =
        kettle_config::Config::parse_text("keybind = ctrl+shift+f2=goto_tab:2\n").keybinds;
    let (trigger, action) = parsed
        .iter()
        .find(|(trigger, action)| {
            **action == Action::GotoTab(1) && trigger.label().contains("F2")
        })
        .unwrap();
    let mut custom = defaults.clone();
    custom.retain(|_, bound| *bound != Action::GotoTab(1));
    custom.insert(*trigger, action.clone());
    let hint = super::chrome::tab_goto_hint(&custom, 1).unwrap();
    assert!(hint.contains("F2"), "{hint}");
    // Unbound: no hint.
    custom.retain(|_, bound| *bound != Action::GotoTab(2));
    assert!(super::chrome::tab_goto_hint(&custom, 2).is_none());
}

/// A macOS title bar, `width` logical px wide at `scale`, with its lane
/// starting after the traffic lights.
fn macos_titlebar_lane(width: f32, scale: f32) -> kettle_render::Rect4 {
    (78.0 * scale, 0.0, (width - 78.0) * scale, 32.0 * scale)
}

/// The macOS title bar gains a Settings gear that leads `▾ +` by a gap
/// wider than theirs; `▾` and `+` stay exactly where a strip without it
/// puts them, and no other strip gets one.
#[test]
fn macos_titlebar_settings_gear_leads_the_new_tab_pair() {
    use super::chrome::{HorizontalTabGeometry, horizontal_tab_geometry};
    let cfg = kettle_config::Config::default();
    for scale in [1.0, 2.0] {
        // 400 pt stands in for the narrow window C03 asks about; the
        // panel harness never sized one.
        for width in [400.0, 640.0, 1280.0] {
            let lane = macos_titlebar_lane(width, scale);
            for tabs in [1, 3, 20] {
                let HorizontalTabGeometry {
                    arrow_rect,
                    plus_rect,
                    settings_rect,
                    layout,
                    ..
                } = horizontal_tab_geometry(lane, scale, (false, true), (tabs, 0), &cfg);
                let (menu, plus, _) = super::horizontal_new_tab_controls(lane, scale, true);
                assert_eq!((arrow_rect, plus_rect), (menu, plus), "▾ + moved");
                let case = format!("{width}px @{scale}x, {tabs} tabs");
                assert!(settings_rect.2 > 0.0, "{case}: no gear");
                assert_eq!((settings_rect.2, settings_rect.3), (menu.2, menu.3), "{case}");
                assert_eq!(settings_rect.1, menu.1, "{case}");
                let gap = menu.0 - (settings_rect.0 + settings_rect.2);
                assert!(gap >= 8.0 * scale - 0.01, "{case}: gap {gap}");
                assert!(
                    settings_rect.0 >= lane.0 && settings_rect.1 >= lane.1,
                    "{case}: gear outside the band"
                );
                assert!(settings_rect.1 + settings_rect.3 <= lane.1 + lane.3 + 0.01);
                // The tabs give the gear its room: none reaches under it.
                for (i, x) in layout.xs.iter().enumerate() {
                    let Some(x) = x else { continue };
                    let w = if i + 1 == tabs {
                        layout.last_width
                    } else {
                        layout.width
                    };
                    assert!(
                        lane.0 + x + w <= settings_rect.0 + 0.01,
                        "{case}: tab {i} ends at {} past the gear at {}",
                        lane.0 + x + w,
                        settings_rect.0
                    );
                }
            }
            // No gear on a plain strip or in a Kettle-drawn GNOME header.
            for header in [(false, false), (true, true), (true, false)] {
                let geometry = horizontal_tab_geometry(lane, scale, header, (2, 0), &cfg);
                assert_eq!(geometry.settings_rect, (0.0, 0.0, 0.0, 0.0), "{header:?}");
            }
        }
    }
}

/// The gear costs the tabs its width and gap, never a tab: it shows only
/// while the lane keeps `tab-min-width` beside every control, whatever the
/// tab count, and `▾ +` never hide.
#[test]
fn macos_titlebar_settings_gear_hides_only_in_a_lane_too_narrow_for_a_tab() {
    use super::chrome::horizontal_tab_geometry;
    let cfg = kettle_config::Config::default();
    for scale in [1.0, 2.0] {
        let probe = macos_titlebar_lane(1280.0, scale);
        let (menu, plus, reserve) = super::horizontal_new_tab_controls(probe, scale, true);
        let extra = menu.2 + 8.0 * scale;
        let threshold = reserve + extra + cfg.tab_min_width;
        for (lane_width, shown) in [(threshold, true), (threshold - 1.0, false)] {
            let lane = (probe.0, 0.0, lane_width, probe.3);
            for tabs in [1, 20] {
                let geometry = horizontal_tab_geometry(lane, scale, (false, true), (tabs, 0), &cfg);
                assert_eq!(geometry.settings_rect.2 > 0.0, shown, "{lane_width} @{scale}x");
                let (menu, plus_now, _) = super::horizontal_new_tab_controls(lane, scale, true);
                assert_eq!(geometry.arrow_rect, menu);
                assert_eq!(geometry.plus_rect, plus_now);
                assert!(plus_now.2 == plus.2 && menu.2 > 0.0);
            }
        }
    }
}

/// A lone tab stays centred on the window with the gear in place. The gear
/// takes its room from the right of the strip only, so in a window too
/// narrow for the preferred tab beside it the tab narrows about the centre
/// instead of sliding left of it. The sweep covers the 500-660 pt widths
/// where that happens, at 1x and 2x and at both title-bar heights, and
/// holds the tab at least as centred as the strip without a gear held it.
#[test]
fn macos_titlebar_settings_gear_keeps_a_lone_tab_centred() {
    use super::chrome::horizontal_tab_geometry;
    let cfg = kettle_config::Config::default();
    let widths = (500..=660).map(|w| w as f32).chain([1280.0]);
    for scale in [1.0_f32, 2.0] {
        for height in [28.0_f32, 32.0] {
            for width in widths.clone() {
                let lane = (78.0 * scale, 0.0, (width - 78.0) * scale, height * scale);
                let case = format!("{width}pt @{scale}x, {height}pt bar");
                let centre_of_window = width * scale * 0.5;
                let geometry = horizontal_tab_geometry(lane, scale, (false, true), (1, 0), &cfg);
                assert!(geometry.settings_rect.2 > 0.0, "{case}: no gear");
                let x = lane.0 + geometry.layout.xs[0].expect("the lone tab shows");
                let w = geometry.layout.last_width;
                assert_eq!(geometry.layout.width, w, "{case}");
                let offset = (x + w * 0.5 - centre_of_window).abs();
                assert!(offset < 0.01, "{case}: lone tab {offset} px off centre");
                assert!(w >= cfg.tab_min_width, "{case}: tab {w} px wide");
                assert!(x >= lane.0, "{case}: tab under the traffic lights");
                assert!(
                    x + w <= geometry.settings_rect.0 + 0.01,
                    "{case}: tab ends at {} past the gear at {}",
                    x + w,
                    geometry.settings_rect.0
                );
                // The same strip without the gear, as the title bar laid it
                // out before the gear: its reserve is `▾ +` alone.
                let (_, _, reserve) = super::horizontal_new_tab_controls(lane, scale, true);
                let before = super::titlebar_tab_layout(
                    1,
                    0,
                    (lane.2 - reserve, reserve, lane.0),
                    lane.3,
                    cfg.tab_min_width,
                    cfg.scroll_tabbar,
                );
                let before_x = lane.0 + before.xs[0].expect("the lone tab showed");
                let before_offset = (before_x + before.last_width * 0.5 - centre_of_window).abs();
                assert!(
                    offset <= before_offset + 0.01,
                    "{case}: {offset} px off centre, {before_offset} px without the gear"
                );
                // Where the tab already fitted, centred and at its full
                // width, in the strip the gear leaves, it keeps that width.
                if before_offset < 0.01 && before_x + before.last_width <= lane.0 + geometry.strip {
                    assert_eq!(w, before.last_width, "{case}");
                }
            }
        }
    }
}

/// The narrowed lone tab stops at `tab-min-width`: a window too narrow for
/// that about the centre keeps the tab at the minimum, clamped into the
/// strip, and a strip narrower than the minimum keeps its own layout.
#[test]
fn a_lone_titlebar_tab_narrows_no_further_than_the_minimum() {
    use super::chrome::{centre_lone_titlebar_tab, titlebar_tab_layout};
    // Centred room 2 * min(160, 200 - 160) = 80, under a 120 minimum.
    let space = (200.0, 120.0, 0.0);
    let mut layout = titlebar_tab_layout(1, 0, space, 28.0, 120.0, true);
    centre_lone_titlebar_tab(&mut layout, space, 28.0, 120.0);
    assert_eq!((layout.width, layout.last_width), (120.0, 120.0));
    assert_eq!(layout.xs, vec![Some(80.0)], "clamped to end at the strip");
    // Room for 200 about a centre at 150: narrowed to it, centred.
    let space = (250.0, 50.0, 0.0);
    let mut layout = titlebar_tab_layout(1, 0, space, 28.0, 120.0, true);
    centre_lone_titlebar_tab(&mut layout, space, 28.0, 120.0);
    assert_eq!(layout.last_width, 200.0);
    assert_eq!(layout.xs, vec![Some(50.0)]);
    // Narrower than the minimum: untouched.
    let space = (100.0, 50.0, 0.0);
    let untouched = titlebar_tab_layout(1, 0, space, 28.0, 120.0, true);
    let mut layout = titlebar_tab_layout(1, 0, space, 28.0, 120.0, true);
    centre_lone_titlebar_tab(&mut layout, space, 28.0, 120.0);
    assert_eq!(
        (layout.xs, layout.last_width, layout.arrow_left),
        (untouched.xs, untouched.last_width, untouched.arrow_left)
    );
}

/// Press, hover and release all use the gear's own rect: a press there is
/// not empty title bar (no window drag, no double-click zoom), the pointer
/// hovers it at every corner, and a release opens Settings only when it
/// lands back on the gear.
#[test]
fn macos_titlebar_settings_gear_hit_tests_share_its_rect() {
    use super::*;
    use super::chrome::{horizontal_tab_geometry, settings_button_at, settings_release_action};
    use super::titlebar::empty_titlebar_space;
    let cfg = kettle_config::Config::default();
    for scale in [1.0, 2.0] {
        let lane = macos_titlebar_lane(1280.0, scale);
        let geometry = horizontal_tab_geometry(lane, scale, (false, true), (1, 0), &cfg);
        let mut bar = kettle_render::TabBar::hidden();
        bar.height = lane.3;
        bar.band = lane;
        bar.in_titlebar = true;
        bar.new_tab = geometry.plus_rect;
        bar.new_tab_menu = geometry.arrow_rect;
        bar.settings = geometry.settings_rect;
        let gear = bar.settings;
        let (cx, cy) = (gear.0 + gear.2 * 0.5, gear.1 + gear.3 * 0.5);
        assert!(!empty_titlebar_space(&bar, cx, cy), "a press on the gear drags");
        // The gap before `▾` still drags the window.
        assert!(empty_titlebar_space(&bar, gear.0 + gear.2 + 4.0 * scale, cy));
        for dx in [0.01, gear.2 - 0.01] {
            for dy in [0.01, gear.3 - 0.01] {
                assert_eq!(
                    new_tab_hover_target(&bar, gear.0 + dx, gear.1 + dy),
                    Some(NewTabHoverTarget::Settings)
                );
            }
        }
        assert!(tab_bar_pointer_region_contains(&bar, cx, cy));
        assert_eq!(
            settings_release_action(true, &bar, cx, cy),
            Some(Action::OpenSettings)
        );
        // Released off the gear, or never pressed on it: nothing.
        let off = (gear.0 - 2.0 * scale, cy);
        assert_eq!(settings_release_action(true, &bar, off.0, off.1), None);
        assert_eq!(
            settings_release_action(true, &bar, cx, gear.1 + gear.3 + scale),
            None
        );
        assert_eq!(settings_release_action(false, &bar, cx, cy), None);
        // Without a gear the same point is empty title bar again.
        bar.settings = (0.0, 0.0, 0.0, 0.0);
        assert!(!settings_button_at(&bar, cx, cy));
        assert_eq!(settings_release_action(true, &bar, cx, cy), None);
        assert!(empty_titlebar_space(&bar, cx, cy));
    }
}

/// Screen readers find the gear as a "Settings" button, in both languages,
/// at the rect that takes its clicks; ctl reports the same rect.
#[test]
fn macos_titlebar_settings_gear_is_a_named_button_for_accesskit_and_ctl() {
    use super::*;
    use super::chrome::horizontal_tab_geometry;
    let cfg = kettle_config::Config::default();
    let lane = macos_titlebar_lane(1280.0, 2.0);
    let geometry = horizontal_tab_geometry(lane, 2.0, (false, true), (1, 0), &cfg);
    let mut bar = kettle_render::TabBar::hidden();
    bar.height = lane.3;
    bar.band = lane;
    bar.new_tab = geometry.plus_rect;
    bar.new_tab_menu = geometry.arrow_rect;
    bar.settings = geometry.settings_rect;
    let gear = bar.settings;
    let bounds = accesskit::Rect::new(
        gear.0 as f64,
        gear.1 as f64,
        (gear.0 + gear.2) as f64,
        (gear.1 + gear.3) as f64,
    );
    for (language, label) in [
        (kettle_i18n::Language::En, "Settings"),
        (kettle_i18n::Language::Es, "Configuración"),
    ] {
        let tr = kettle_i18n::Translator::new(language);
        let nodes = tab_accessibility_projection(&bar, &[], false, &tr);
        let buttons: Vec<_> = nodes
            .iter()
            .filter(|(_, node)| node.label() == Some(label))
            .collect();
        assert_eq!(buttons.len(), 1, "{label}");
        let node = &buttons[0].1;
        assert_eq!(node.role(), accesskit::Role::Button);
        assert_eq!(node.bounds(), Some(bounds));
        assert!(node.supports_action(accesskit::Action::Click));
        let list = nodes
            .iter()
            .find(|(_, node)| node.role() == accesskit::Role::TabList)
            .unwrap();
        assert!(list.1.children().contains(&buttons[0].0));
    }
    let json = super::ctl::tab_bar_geometry_json(&bar, Vec::new());
    assert_eq!(
        json["settings"],
        serde_json::json!({"x":gear.0,"y":gear.1,"width":gear.2,"height":gear.3})
    );
    // No gear, no node.
    bar.settings = (0.0, 0.0, 0.0, 0.0);
    let tr = kettle_i18n::Translator::default();
    let nodes = tab_accessibility_projection(&bar, &[], false, &tr);
    assert!(!nodes.iter().any(|(_, node)| node.label() == Some("Settings")));
}

/// The gear's tooltip names Settings with whatever key opens it: a rebind
/// shows the new key, and an unbound action shows no key at all.
#[test]
fn macos_titlebar_settings_tooltip_follows_the_open_settings_binding() {
    use super::chrome::{NewTabHoverTarget, chrome_button_shortcut, new_tab_tooltip};
    let defaults = kettle_config::keybinds::defaults();
    let default_key = chrome_button_shortcut(&defaults, NewTabHoverTarget::Settings);
    assert_eq!(
        default_key,
        kettle_config::keybinds::ui_hint_label(&defaults, &Action::OpenSettings)
    );
    assert!(default_key.is_some(), "Settings has a default shortcut");
    assert_ne!(
        default_key,
        chrome_button_shortcut(&defaults, NewTabHoverTarget::Plus),
        "the gear must not show the New Tab key"
    );
    let parsed =
        kettle_config::Config::parse_text("keybind = ctrl+shift+f3=open_settings\n").keybinds;
    let (trigger, action) = parsed
        .iter()
        .find(|(trigger, action)| {
            **action == Action::OpenSettings && trigger.label().contains("F3")
        })
        .unwrap();
    let mut custom = defaults.clone();
    custom.retain(|_, bound| *bound != Action::OpenSettings);
    custom.insert(*trigger, action.clone());
    let key = chrome_button_shortcut(&custom, NewTabHoverTarget::Settings).unwrap();
    assert!(key.contains("F3"), "{key}");
    for (language, label) in [
        (kettle_i18n::Language::En, "Settings"),
        (kettle_i18n::Language::Es, "Configuración"),
    ] {
        let tr = kettle_i18n::Translator::new(language);
        assert_eq!(
            new_tab_tooltip(Some(NewTabHoverTarget::Settings), &tr, Some(&key)),
            Some(format!("{label}  {key}"))
        );
        assert_eq!(
            new_tab_tooltip(Some(NewTabHoverTarget::Settings), &tr, None),
            Some(label.into())
        );
    }
    custom.retain(|_, bound| *bound != Action::OpenSettings);
    assert_eq!(
        chrome_button_shortcut(&custom, NewTabHoverTarget::Settings),
        None
    );
}

/// Leaving the window or losing focus drops the gear's hover and press, so
/// it never stays lit and a later release elsewhere never opens Settings.
/// The rest of the strip's buttons let go the same way.
#[test]
fn leaving_the_window_or_losing_focus_forgets_the_settings_gear() {
    let lit = || {
        let mut ws = crate::window_state::WindowState::new(0, crate::mux::Mux::new());
        ws.hovered_settings = true;
        ws.pressed_settings = true;
        ws.hovered_new_tab = true;
        ws.pressed_new_tab = true;
        ws.hovered_new_tab_menu = true;
        ws.pressed_new_tab_menu = true;
        ws.hovered_close_idx = Some(0);
        ws.hovered_tab_idx = Some(0);
        ws
    };
    let mut focus_lost = lit();
    focus_lost.release_titlebar_buttons();
    assert!(!focus_lost.hovered_settings && !focus_lost.pressed_settings);
    assert!(!focus_lost.pressed_new_tab && !focus_lost.pressed_new_tab_menu);
    let mut left = lit();
    left.leave_tab_bar();
    assert!(!left.hovered_settings && !left.pressed_settings);
    assert!(!left.hovered_new_tab && !left.pressed_new_tab);
    assert!(!left.hovered_new_tab_menu && !left.pressed_new_tab_menu);
    assert_eq!((left.hovered_close_idx, left.hovered_tab_idx), (None, None));
}

/// A screen reader's click on the gear runs OpenSettings, the action
/// Cmd+, runs; New tab keeps its own action and a tab is not a button.
#[test]
fn a_screen_reader_click_on_the_settings_gear_opens_settings() {
    use super::chrome::horizontal_tab_geometry;
    use super::*;
    let cfg = kettle_config::Config::default();
    let lane = macos_titlebar_lane(1280.0, 2.0);
    let geometry = horizontal_tab_geometry(lane, 2.0, (false, true), (1, 0), &cfg);
    let mut bar = kettle_render::TabBar::hidden();
    bar.height = lane.3;
    bar.band = lane;
    bar.new_tab = geometry.plus_rect;
    bar.new_tab_menu = geometry.arrow_rect;
    bar.settings = geometry.settings_rect;
    let tr = kettle_i18n::Translator::default();
    let nodes = tab_accessibility_projection(&bar, &[7], false, &tr);
    let action_of = |label: &str| {
        let (id, _) = nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label))
            .unwrap_or_else(|| panic!("no {label} node"));
        tab_button_action(*id)
    };
    assert_eq!(action_of("Settings"), Some(Action::OpenSettings));
    assert_eq!(action_of("New tab"), Some(Action::NewTab));
    assert_eq!(tab_button_action(accessibility_tab_id(7, false)), None);
}

/// A tab torn off a macOS title bar is held by its pointer in the lone tab
/// the new window paints: centred on that window, at least
/// `tab-min-width` wide, and clear of the Settings gear. The torn window's
/// grip once assumed `▾ +` alone, so in a narrow window it held the pointer
/// in a full-width tab that ran under the gear.
#[test]
fn a_tab_torn_off_a_macos_title_bar_is_held_in_the_lone_tab_beside_the_gear() {
    use super::chrome::{titlebar_settings_control, titlebar_tear_off_grab_x};
    let cfg = kettle_config::Config::default();
    let buttons = crate::window_buttons::ButtonLayout {
        left: Vec::new(),
        right: Vec::new(),
    };
    for scale in [1.0_f32, 2.0] {
        for width in (500..=660).step_by(4).map(|w| w as f32).chain([1280.0]) {
            let band = macos_titlebar_lane(width, scale);
            let case = format!("{width}pt @{scale}x");
            let (menu, _, reserve) = super::horizontal_new_tab_controls(band, scale, true);
            let (gear, _) =
                titlebar_settings_control(menu, band.2, reserve, scale, cfg.tab_min_width);
            assert!(gear.2 > 0.0, "{case}: no gear");
            let in_tab = f64::from(32.0 * scale);
            let grab = titlebar_tear_off_grab_x(&cfg, band, (false, true), scale, &buttons, in_tab);
            let x = (grab - in_tab) as f32;
            // A tab centred on the window from `x` is this wide.
            let centre = width * scale * 0.5;
            let tab_w = 2.0 * (centre - x);
            assert!(x >= band.0, "{case}: grip under the traffic lights at {x}");
            assert!(tab_w >= cfg.tab_min_width, "{case}: tab {tab_w} px wide");
            assert!(
                x + tab_w <= gear.0 + 0.01,
                "{case}: the held tab ends at {} under the gear at {}",
                x + tab_w,
                gear.0
            );
        }
    }
}

/// With Settings open, a primary press on the gear goes to the gear, not
/// to Settings' close-on-outside-press, so its release does what Cmd+,
/// does. Settings keeps the press while it edits text or captures a key,
/// and anywhere off the gear.
#[test]
fn the_settings_gear_takes_a_press_ahead_of_open_settings() {
    use super::chrome::settings_gear_takes_press_over_settings as takes;
    let open = crate::settings::SettingsNav::default();
    assert!(takes(Some(&open), false, true, true));
    assert!(!takes(Some(&open), false, true, false), "off the gear");
    assert!(!takes(Some(&open), false, false, true), "a middle press");
    assert!(!takes(Some(&open), true, true, true), "editing a field");
    let capturing = crate::settings::SettingsNav {
        capturing: true,
        ..Default::default()
    };
    assert!(!takes(Some(&capturing), false, true, true), "capturing a key");
    assert!(!takes(None, false, true, true), "Settings closed");
}
