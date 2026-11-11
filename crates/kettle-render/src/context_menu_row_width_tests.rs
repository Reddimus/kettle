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
        true,
    );
    assert!(!quads.is_empty());
    assert!(quads.iter().all(|quad| {
        quad.pos.iter().all(|value| value.is_finite())
            && quad
                .size
                .iter()
                .all(|value| value.is_finite() && *value >= 0.0)
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
            true,
        )
        .is_empty()
    );
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
            true,
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
        // Shadow, panel and four border quads precede the two arrows.
        let quads = menu_chrome_quads(
            &menu,
            &theme,
            theme.accent,
            8.0,
            16.0,
            &mut crate::chrome::Colors::default(),
            true,
        );
        assert_eq!(quads.len(), 12);
        for (arrow, top) in [
            (&quads[6..9], 10.0),
            (&quads[9..12], 10.0 + panel_h - inset),
        ] {
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

#[test]
fn picker_surface_encloses_header_and_selection_stays_inside_rounded_corners() {
    let theme = kettle_config::Theme::default();
    let menu = ContextMenu {
        anchor: (140.0, 100.0),
        rows: vec![ContextMenuRow {
            toggle: None,
            swatches: None,
            label: "one".into(),
            separator: false,
            enabled: true,
            hint: String::new(),
            matched_chars: None,
        }],
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 360.0,
        panel_h_clamped: 28.0,
        picker_header_height: 30.0,
        a11y_label: None,
    };
    let surfaces = super::overlays::picker_surfaces(&menu, &theme, 8.0).unwrap();
    assert_eq!(surfaces[1].pos, [140.0, 70.0]);
    assert_eq!(surfaces[1].size, [360.0, 58.0]);
    assert_eq!(surfaces[1].params[..2], [8.0, 1.0]);
    let quads = menu_chrome_quads(
        &menu,
        &theme,
        theme.accent,
        8.0,
        16.0,
        &mut super::chrome::Colors::default(),
        true,
    );
    assert!(quads.iter().any(|quad| quad.pos[1] > 125.0));
    for quad in quads.iter().filter(|quad| quad.pos[1] > 125.0) {
        assert!(
            quad.pos[0] > 141.0,
            "the selected rail must not square off the lower corner"
        );
        assert!(quad.pos[0] + quad.size[0] < 499.0);
        assert!(quad.pos[1] + quad.size[1] <= 127.0);
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
            let row_mid = ay + (ch + menu::ROW_PAD) * 0.5;
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

    let row_h = 16.0 + menu::ROW_PAD;
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
