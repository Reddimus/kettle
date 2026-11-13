use super::{
    ContextMenu, ContextMenuRow, context_menu_clip_indicators, context_menu_panel_width, menu,
    menu_chrome_quads, menu_row_chars,
};

#[test]
fn menu_width_and_inverse_share_one_formula() {
    assert_eq!(menu::panel_width(20.0, 8.0), 200.0);
    assert_eq!(menu::panel_columns(200.0, 8.0), 20);
    assert_eq!(menu::panel_width(1.0, 8.0), menu::MIN_W);
}

#[test]
fn row_budget_uses_display_columns() {
    let row = ContextMenuRow {
        toggle: None,
        swatches: None,
        matched_chars: None,
        label: "主题 👩‍💻".to_string(),
        separator: false,
        enabled: true,
        hint: "Ctrl+界".to_string(),
    };
    assert_eq!(
        menu_row_chars(&row),
        unicode_width::UnicodeWidthStr::width(row.label.as_str())
            + unicode_width::UnicodeWidthStr::width(row.hint.as_str())
            + 2
    );
}

#[test]
fn app_clamped_width_is_authoritative_for_every_render_pass() {
    let row = ContextMenuRow {
        toggle: None,
        swatches: None,
        matched_chars: None,
        label: "short".to_string(),
        separator: false,
        enabled: true,
        hint: String::new(),
    };
    let menu = ContextMenu {
        anchor: (0.0, 0.0),
        rows: vec![row],
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 237.5,
        panel_h_clamped: 0.0,
        a11y_label: None,
        picker_header_height: 0.0,
    };
    assert_eq!(context_menu_panel_width(&menu, 9.6), 237.5);
}

#[test]
fn scroll_indicators_follow_the_remaining_suffix() {
    let rows = [false, false, true, false, false]
        .into_iter()
        .enumerate()
        .map(|(index, separator)| ContextMenuRow {
            toggle: None,
            swatches: None,
            matched_chars: None,
            label: format!("row {index}"),
            separator,
            enabled: !separator,
            hint: String::new(),
        })
        .collect::<Vec<_>>();
    let row_h = 24.0;
    let sep_h = 8.0;
    let panel_h = 48.0;

    assert_eq!(
        context_menu_clip_indicators(&rows, 0, panel_h, row_h, sep_h),
        (false, true),
        "the first window has content below but none above"
    );
    assert_eq!(
        context_menu_clip_indicators(&rows, 2, panel_h, row_h, sep_h),
        (true, true),
        "the middle window has content on both sides"
    );
    assert_eq!(
        context_menu_clip_indicators(&rows, 3, panel_h, row_h, sep_h),
        (true, false),
        "the final two rows fit exactly, so no down indicator remains"
    );
    assert_eq!(
        context_menu_clip_indicators(&rows, rows.len(), panel_h, row_h, sep_h),
        (true, false),
        "an out-of-range offset is clamped to the empty suffix"
    );
}

#[test]
fn tiny_or_invalid_menu_never_emits_negative_or_nonfinite_quads() {
    let menu = ContextMenu {
        anchor: (0.0, 0.0),
        rows: vec![ContextMenuRow {
            toggle: None,
            swatches: None,
            matched_chars: None,
            label: String::new(),
            separator: true,
            enabled: false,
            hint: String::new(),
        }],
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 1.0,
        panel_h_clamped: 1.0,
        a11y_label: None,
        picker_header_height: 0.0,
    };
    let theme = kettle_config::Theme::default();
    let quads = menu_chrome_quads(
        &menu,
        &theme,
        kettle_config::Rgb::new(1, 2, 3),
        8.0,
        16.0,
        &mut crate::chrome::Colors::default(),
    );
    assert!(quads.iter().all(|quad| {
        quad.pos.iter().all(|value| value.is_finite())
            && quad
                .size
                .iter()
                .all(|value| value.is_finite() && *value >= 0.0)
    }));
    let surfaces = |menu: &ContextMenu| {
        super::overlays::menu_level_surfaces(
            menu,
            &theme,
            kettle_config::Rgb::new(1, 2, 3),
            (8.0, 16.0),
            2.0,
            &mut crate::chrome::Colors::default(),
            true,
        )
    };
    let tiny = surfaces(&menu);
    assert!(!tiny.is_empty(), "a tiny panel still paints");
    assert!(tiny.iter().all(|surface| {
        surface
            .pos
            .iter()
            .chain(&surface.params)
            .all(|v| v.is_finite())
            && surface.size.iter().all(|v| v.is_finite() && *v >= 0.0)
            && surface.params[0] <= surface.size[0].min(surface.size[1]) * 0.5 + 10.0 * 2.0
    }));

    let invalid = ContextMenu {
        anchor: (f32::NAN, 0.0),
        ..menu
    };
    assert!(
        menu_chrome_quads(
            &invalid,
            &theme,
            kettle_config::Rgb::new(1, 2, 3),
            8.0,
            16.0,
            &mut crate::chrome::Colors::default(),
        )
        .is_empty()
    );
    assert!(surfaces(&invalid).is_empty());
}

#[test]
fn cut_menu_reserves_scroll_targets_and_fitting_menu_has_none() {
    let (inset, content) = menu::scroll_content(600.0, 200.0, 28.0);
    assert!(
        inset >= 18.0,
        "cut menu has no scroll target: inset={inset}"
    );
    assert_eq!(content + 2.0 * inset, 200.0);
    assert_eq!(menu::scroll_content(140.0, 140.0, 28.0), (0.0, 140.0));
}

#[test]
fn cut_menu_paints_arrow_inside_reserved_strip() {
    let theme = kettle_config::Theme::by_name("Builtin Light");
    let mut menu = ContextMenu {
        anchor: (10.0, 10.0),
        rows: (0..20)
            .map(|_| ContextMenuRow {
                toggle: None,
                swatches: None,
                matched_chars: None,
                label: "Command".into(),
                hint: String::new(),
                separator: false,
                enabled: true,
            })
            .collect(),
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 200.0,
        panel_h_clamped: 200.0,
        a11y_label: None,
        picker_header_height: 0.0,
    };
    let arrows = |menu: &ContextMenu| {
        menu_chrome_quads(
            menu,
            &theme,
            theme.accent,
            8.0,
            16.0,
            &mut crate::chrome::Colors::default(),
        )
        .into_iter()
        .filter(|quad| quad.size[1] == 2.0 && quad.size[0] < 20.0 && quad.color[3] == 1.0)
        .collect::<Vec<_>>()
    };
    let cut = arrows(&menu);
    assert_eq!(cut.len(), 3);
    assert!(
        cut.iter()
            .all(|quad| quad.pos[1] >= 10.0 + 200.0 - menu::SCROLL_H
                && quad.pos[1] + quad.size[1] <= 210.0)
    );
    menu.panel_h_clamped = 0.0;
    assert!(arrows(&menu).is_empty());
}

#[test]
fn regression_scroll_arrows_stay_inside_short_reserved_strips() {
    let theme = kettle_config::Theme::default();
    for panel_h in [28.25, 29.0, 32.0, 40.0, 64.0] {
        let menu = ContextMenu {
            anchor: (10.0, 10.0),
            rows: (0..10)
                .map(|_| ContextMenuRow {
                    toggle: None,
                    swatches: None,
                    matched_chars: None,
                    label: "row".into(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                })
                .collect(),
            highlight: usize::MAX,
            scroll_offset: 1,
            panel_w_clamped: 200.0,
            panel_h_clamped: panel_h,
            a11y_label: None,
            picker_header_height: 0.0,
        };
        let (inset, _) = menu::scroll_content(280.0, panel_h, 28.0);
        // The panel is a rounded surface; the quads are the two arrows.
        let quads = menu_chrome_quads(
            &menu,
            &theme,
            theme.accent,
            8.0,
            16.0,
            &mut crate::chrome::Colors::default(),
        );
        assert_eq!(quads.len(), 6);
        for (arrow, top) in [(&quads[0..3], 10.0), (&quads[3..6], 10.0 + panel_h - inset)] {
            for q in arrow {
                assert!(
                    q.pos[1] >= top && q.pos[1] + q.size[1] <= top + inset + 0.0001,
                    "panel {panel_h}, strip {top}..{}, quad {}..{}",
                    top + inset,
                    q.pos[1],
                    q.pos[1] + q.size[1]
                );
            }
        }
    }
}

#[test]
fn ordinary_menu_spans_keep_default_color_and_unicode_matches_use_scalars() {
    let mut row = ContextMenuRow {
        toggle: None,
        swatches: None,
        label: "Cerrar pestaña".into(),
        hint: String::new(),
        separator: false,
        enabled: true,
        matched_chars: None,
    };
    assert_eq!(
        super::overlays::menu_label_spans(&row),
        vec![("Cerrar pestaña", false)]
    );
    let accent = kettle_config::Rgb::new(1, 2, 3);
    let plain_key = super::overlays::menu_match_key(&row, accent);
    row.matched_chars = Some(std::iter::once(11..13).collect());
    let spans = super::overlays::menu_label_spans(&row);
    assert_eq!(
        spans
            .iter()
            .filter(|(_, matched)| *matched)
            .map(|(text, _)| *text)
            .collect::<String>(),
        "añ"
    );
    assert_ne!(plain_key, super::overlays::menu_match_key(&row, accent));
    assert_ne!(
        super::overlays::menu_match_key(&row, accent),
        super::overlays::menu_match_key(&row, kettle_config::Rgb::new(3, 2, 1))
    );
}

/// Whether `(px, py)` lies inside the rounded rect `(x, y, w, h)` of corner
/// `radius`, shrunk by `inset` on every side.
fn inside_rounded(
    (px, py): (f32, f32),
    (x, y, w, h): (f32, f32, f32, f32),
    radius: f32,
    inset: f32,
) -> bool {
    let (x, y, w, h) = (x + inset, y + inset, w - 2.0 * inset, h - 2.0 * inset);
    let r = (radius - inset).max(0.0);
    if px < x - 1e-3 || py < y - 1e-3 || px > x + w + 1e-3 || py > y + h + 1e-3 {
        return false;
    }
    let cx = px.clamp(x + r, x + w - r);
    let cy = py.clamp(y + r, y + h - r);
    (px - cx).hypot(py - cy) <= r + 1e-3
}

/// A picker's card holds its header and rows; a plain menu's panel holds its
/// rows. Both are rounded at about 10 logical pixels with a one-pixel edge,
/// sit on a layered shadow, and keep the selection's rounded corners inside
/// their own at every scale, including on the first and last rows.
#[test]
fn menu_and_picker_panels_are_rounded_and_keep_the_selection_inside() {
    let theme = kettle_config::Theme::default();
    let row = |label: &str| ContextMenuRow {
        toggle: None,
        swatches: None,
        label: label.into(),
        separator: false,
        enabled: true,
        hint: String::new(),
        matched_chars: None,
    };
    let level = |labels: &[&str], highlight, (w, h, header)| ContextMenu {
        anchor: (140.0, 100.0),
        rows: labels.iter().map(|label| row(label)).collect(),
        highlight,
        scroll_offset: 0,
        panel_w_clamped: w,
        panel_h_clamped: h,
        picker_header_height: header,
        a11y_label: None,
    };
    let picker = |highlight| level(&["one", "two"], highlight, (360.0, 56.0, 30.0));
    let plain = |highlight| level(&["Copy", "Paste", "Find"], highlight, (0.0, 0.0, 0.0));
    let surfaces_of = |menu: &ContextMenu, scale: f32, cell: (f32, f32)| {
        super::overlays::menu_level_surfaces(
            menu,
            &theme,
            theme.accent,
            cell,
            scale,
            &mut super::chrome::Colors::default(),
            true,
        )
    };
    let layers = super::overlays::MENU_SHADOW_LAYERS.len();
    let surfaces = surfaces_of(&picker(1), 1.0, (8.0, 16.0));
    assert_eq!(surfaces.len(), layers + 2, "shadow, card and selection");
    let card = surfaces[layers];
    assert_eq!(card.pos, [140.0, 70.0]);
    assert_eq!(card.size, [360.0, 86.0]);
    assert_eq!(card.params[..2], [10.0, 1.0]);
    for scale in [1.0_f32, 1.25, 1.5, 2.0, 3.0] {
        for (make, rows, cell) in [
            (&picker as &dyn Fn(usize) -> ContextMenu, 2, (8.0, 16.0)),
            (&plain, 3, (8.0, 16.0)),
            (&plain, 3, (9.6, 19.0)),
        ] {
            for highlight in 0..rows {
                let menu = make(highlight);
                let surfaces = surfaces_of(&menu, scale, cell);
                assert_eq!(surfaces.len(), layers + 2);
                let (shadow, panel, selection) =
                    (&surfaces[..layers], surfaces[layers], surfaces[layers + 1]);
                let rect = |s: &crate::rounded_surface::RoundedSurface| {
                    (s.pos[0], s.pos[1], s.size[0], s.size[1])
                };
                assert_eq!(panel.params[0], 10.0 * scale);
                // Every layer is larger than the panel, faint, and no two
                // share an outline: the edge fades instead of stepping once.
                for layer in shadow {
                    let (x, y, w, h) = rect(layer);
                    assert!(w > panel.size[0] && h > panel.size[1]);
                    assert!(x < panel.pos[0] && y < panel.pos[1] + h);
                    assert!(layer.fill[3] > 0.0 && layer.fill[3] <= 0.12);
                }
                for pair in shadow.windows(2) {
                    assert!(pair[0].size[0] > pair[1].size[0], "widest layer first");
                }
                let (sx, sy, sw, sh) = rect(&selection);
                let r = selection.params[0];
                let row_rect = super::overlays::context_menu_row_rects(&menu, cell)
                    .into_iter()
                    .find(|(i, _)| *i == highlight)
                    .unwrap()
                    .1;
                assert!(sx > row_rect.0 && sx + sw < row_rect.0 + row_rect.2);
                assert!(sy > row_rect.1 && sy + sh < row_rect.1 + row_rect.3);
                assert!(
                    sx <= menu.anchor.0 + 16.0 - 4.0,
                    "selection reaches past the label start"
                );
                for step in 0..=18 {
                    let angle = step as f32 * std::f32::consts::FRAC_PI_2 / 18.0;
                    let (dx, dy) = (angle.cos() * r, angle.sin() * r);
                    for (cx, cy, ox, oy) in [
                        (sx + r, sy + r, -dx, -dy),
                        (sx + sw - r, sy + r, dx, -dy),
                        (sx + r, sy + sh - r, -dx, dy),
                        (sx + sw - r, sy + sh - r, dx, dy),
                    ] {
                        assert!(
                            inside_rounded((cx + ox, cy + oy), rect(&panel), panel.params[0], 1.0),
                            "scale {scale}, row {highlight}: selection corner leaves the panel"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn submenu_surfaces_clip_ancestor_labels_into_disjoint_rectangles() {
    let bounds = crate::TextBounds {
        left: 0,
        top: 0,
        right: 100,
        bottom: 100,
    };
    let parts = crate::overlays::menu_text_bounds_outside(bounds, (20.0, 30.0, 60.0, 40.0));
    assert_eq!(parts.len(), 4);
    assert_eq!(
        parts
            .iter()
            .map(|r| (r.right - r.left) * (r.bottom - r.top))
            .sum::<i32>(),
        7600
    );
    for part in parts {
        assert!(part.right <= 20 || part.left >= 80 || part.bottom <= 30 || part.top >= 70);
    }
    assert!(crate::overlays::menu_text_bounds_outside(bounds, (0.0, 0.0, 100.0, 100.0)).is_empty());
    assert_eq!(
        crate::overlays::menu_text_bounds_outside(bounds, (200.0, 200.0, 10.0, 10.0)),
        vec![bounds]
    );
}

fn swatch_row(label: &str, hint: &str) -> ContextMenuRow {
    ContextMenuRow {
        toggle: None,
        swatches: Some(std::array::from_fn(|index| {
            kettle_config::Rgb::new(index as u8 * 30, 200 - index as u8 * 20, 7)
        })),
        matched_chars: None,
        label: label.into(),
        separator: false,
        enabled: true,
        hint: hint.into(),
    }
}

fn natural_menu(rows: Vec<ContextMenuRow>) -> ContextMenu {
    ContextMenu {
        anchor: (50.0, 40.0),
        rows,
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 0.0,
        panel_h_clamped: 0.0,
        a11y_label: None,
        picker_header_height: 0.0,
    }
}

/// The row measure reserves the strip's columns, and the painter puts the
/// strip inside them: right of the label and its gap, left of the hint and
/// its gap, in a panel exactly as wide as the measure asks.
#[test]
fn swatches_paint_inside_the_columns_their_row_reserves() {
    let ch = 16.0;
    let label = "A Rather Long Theme Name";
    for cw in [8.0_f32, 7.3, 9.6] {
        for hint in ["✓ dark", ""] {
            let row = swatch_row(label, hint);
            let mut bare = swatch_row(label, hint);
            bare.swatches = None;
            assert_eq!(
                menu_row_chars(&row),
                menu_row_chars(&bare) + menu::swatch_columns()
            );
            let colors = row.swatches.unwrap();
            let menu = natural_menu(vec![row]);
            let panel_w = context_menu_panel_width(&menu, cw);
            assert_eq!(
                panel_w,
                menu_row_chars(&menu.rows[0]) as f32 * cw + menu::H_PAD
            );
            let painted = super::overlays::menu_swatch_rects(&menu, cw, ch);
            assert_eq!(painted.len(), menu::SWATCH_COUNT, "cw {cw} hint {hint:?}");
            let (ax, ay) = menu.anchor;
            let label_end = ax + 16.0 + label.len() as f32 * cw;
            let hint_w = unicode_width::UnicodeWidthStr::width(hint) as f32 * cw;
            let strip_left = painted[0].0.0;
            let strip_right = painted.last().map(|((x, _, w, _), _)| x + w).unwrap();
            let reserved = (menu::swatch_columns() - menu::SWATCH_GAP_COLUMNS) as f32 * cw;
            assert!(strip_right - strip_left <= reserved, "cw {cw}");
            // The label's gap is the hint's spacer, or the strip's own gap.
            let left_gap = if hint.is_empty() {
                menu::SWATCH_GAP_COLUMNS
            } else {
                2
            };
            assert!(
                strip_left >= label_end + left_gap as f32 * cw - 0.5,
                "cw {cw} hint {hint:?}: strip {strip_left} overlaps the label"
            );
            let limit = if hint.is_empty() {
                ax + panel_w - 16.0
            } else {
                ax + panel_w - 16.0 - hint_w - menu::SWATCH_GAP_COLUMNS as f32 * cw
            };
            assert!(
                strip_right <= limit + 0.5,
                "cw {cw} hint {hint:?}: strip {strip_right} reaches the hint"
            );
            let row_mid = ay + menu::row_height(ch) * 0.5;
            for (((x, y, w, h), color), expected) in painted.iter().zip(colors) {
                assert_eq!(*color, expected);
                assert_eq!((*w, *h), (cw, cw));
                assert!((y + h * 0.5 - row_mid).abs() <= 0.5, "vertically centred");
                assert!(*x >= strip_left && x + w <= strip_right);
            }
        }
    }
}

/// Ordinary menu rows paint no swatches, and a picker paints them only for
/// rows in its scrolled view, outlined in the menu's border colour.
#[test]
fn only_visible_rows_with_swatches_paint_them() {
    let theme = kettle_config::Theme::default();
    let plain = ["Copy", "Paste", "", "New Tab"]
        .into_iter()
        .map(|label| ContextMenuRow {
            toggle: None,
            swatches: None,
            matched_chars: None,
            label: label.into(),
            separator: label.is_empty(),
            enabled: true,
            hint: "Ctrl+C".into(),
        })
        .collect();
    let menu = natural_menu(plain);
    assert!(super::overlays::menu_swatch_rects(&menu, 8.0, 16.0).is_empty());
    assert!(super::overlays::menu_swatch_surfaces([&menu], &theme, 8.0, 16.0).is_empty());

    let row_h = menu::row_height(16.0);
    let mut menu = natural_menu(
        (0..10)
            .map(|i| swatch_row(&format!("t{i}"), "dark"))
            .collect(),
    );
    menu.panel_w_clamped = 360.0;
    menu.panel_h_clamped = 4.0 * row_h;
    menu.scroll_offset = 4;
    let painted = super::overlays::menu_swatch_rects(&menu, 8.0, 16.0);
    let (inset, content_h) = menu::scroll_content(10.0 * row_h, 4.0 * row_h, row_h);
    let rows = (content_h / row_h).floor() as usize;
    assert!(rows > 0 && rows < 10);
    assert_eq!(painted.len(), rows * menu::SWATCH_COUNT);
    let top = menu.anchor.1 + inset;
    assert!(
        painted
            .iter()
            .all(|((_, y, _, h), _)| *y >= top && y + h <= top + content_h)
    );

    let surfaces = super::overlays::menu_swatch_surfaces([&menu], &theme, 8.0, 16.0);
    assert_eq!(surfaces.len(), painted.len());
    let unit = |c: kettle_config::Rgb, a| {
        [
            c.r as f32 / 255.0,
            c.g as f32 / 255.0,
            c.b as f32 / 255.0,
            a,
        ]
    };
    for (surface, ((x, y, w, h), color)) in surfaces.iter().zip(&painted) {
        assert_eq!((surface.pos, surface.size), ([*x, *y], [*w, *h]));
        assert_eq!(surface.fill, unit(*color, 1.0));
        assert_eq!(surface.stroke, unit(theme.foreground, 0.45));
        assert_eq!(surface.params[1], 1.0, "a 1 px outline");
    }
}

/// A command-palette row's matched characters read at WCAG AA (4.5:1) on the
/// row's own surface, the selected row's accent-tinted fill included, in
/// every bundled theme with its own accent and every Peacock accent, in both
/// blend spaces. They are also bold: in some themes the accent is the
/// foreground, so colour alone cannot show the match.
#[test]
fn picker_match_color_meets_text_contrast_on_the_selected_row_in_every_theme() {
    use super::chrome::{Colors, TEXT_CONTRAST};
    use super::overlays::{
        menu_level_surfaces, menu_match_attrs, menu_match_color, menu_row_selected,
    };
    use kettle_config::Theme;

    let row = |label: &str| ContextMenuRow {
        toggle: None,
        swatches: None,
        label: label.into(),
        separator: false,
        enabled: true,
        hint: String::new(),
        matched_chars: Some(std::iter::once(0..5).collect()),
    };
    let menu = ContextMenu {
        anchor: (140.0, 100.0),
        rows: vec![row("Split automatically"), row("Split up (new pane above)")],
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 360.0,
        panel_h_clamped: 56.0,
        picker_header_height: 30.0,
        a11y_label: None,
    };
    assert!(menu_row_selected(&menu, 0) && !menu_row_selected(&menu, 1));
    let mut failures = Vec::new();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let mut accents = vec![theme.accent];
        accents.extend(kettle_config::peacock_pool(&theme));
        for accent in accents {
            for linear in [false, true] {
                let mut colors = Colors::default();
                // The selection is the level's last surface; read its fill
                // back rather than restating how it is blended.
                let surfaces = menu_level_surfaces(
                    &menu,
                    &theme,
                    accent,
                    (8.0, 16.0),
                    1.0,
                    &mut colors,
                    linear,
                );
                let fill = surfaces.last().expect("selection surface").fill;
                let to_u8 = |c: f32| (c * 255.0).round() as u8;
                let selection =
                    kettle_config::Rgb::new(to_u8(fill[0]), to_u8(fill[1]), to_u8(fill[2]));
                for (index, surface) in [(0, selection), (1, theme.background)] {
                    let selected = menu_row_selected(&menu, index);
                    let matched = menu_match_color(selected, &theme, accent, &mut colors, linear);
                    let ratio = super::color::contrast_ratio(matched, surface);
                    if ratio < TEXT_CONTRAST {
                        failures.push(format!(
                            "{name} accent {accent:?} linear={linear} selected={selected}: \
                             match {matched:?} on {surface:?} is {ratio:.2}:1"
                        ));
                    }
                    let attrs = menu_match_attrs(&glyphon::Attrs::new(), matched);
                    if attrs.color_opt != Some(super::gc(matched))
                        || attrs.weight != glyphon::Weight::BOLD
                    {
                        failures.push(format!(
                            "{name} selected={selected}: match is shaped {:?} {:?}, \
                             not bold {matched:?}",
                            attrs.weight, attrs.color_opt
                        ));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
