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
        status: None,
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
        status: None,
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
        status: None,
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
            status: None,
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
        status: None,
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

// Source regression for parent RED/GREEN demonstration; UNRUN here.
#[test]
fn pinned_status_shares_card_surface_bounds_but_has_no_action_index() {
    let mut menu = ContextMenu {
        anchor: (16.0, 180.0),
        rows: (0..10)
            .map(|index| ContextMenuRow {
                matched_chars: None,
                label: format!("action {index}"),
                enabled: true,
                separator: false,
                hint: String::new(),
            })
            .collect(),
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 296.0,
        panel_h_clamped: 112.0,
        a11y_label: None,
        picker_header_height: 30.0,
        status: Some(super::MenuStatus::new(
            "Profiles weren't ready. Reopen to refresh.\nEnter run / Tab select / Esc cancel",
            296.0,
            (8.0, 16.0),
        )),
    };
    let band = menu.status_rect().unwrap();
    let card = menu.card_rect();
    let surfaces =
        super::overlays::picker_surfaces(&menu, &kettle_config::Theme::default(), 8.0).unwrap();
    assert_eq!(surfaces[1].pos, [card.0, card.1]);
    assert_eq!(surfaces[1].size, [card.2, card.3]);
    assert_eq!(
        menu.text_buffer_count(),
        menu.rows.len() + menu.status.as_ref().unwrap().lines.len()
    );
    assert!(menu.visible_row_rect(menu.rows.len()).is_none());
    for offset in [0, 4, 8] {
        menu.scroll_offset = offset;
        assert_eq!(menu.status_rect(), Some(band));
        for index in 0..menu.rows.len() {
            if let Some(row) = menu.visible_row_rect(index) {
                assert!(row.1 >= band.1 + band.3);
            }
        }
    }
    let fg = kettle_config::Rgb::new(200, 200, 200);
    let bg = kettle_config::Rgb::new(20, 20, 20);
    let key = super::context_menu_text_damage_key(Some(&menu), fg, bg);
    menu.status.as_mut().unwrap().lines[0].push('!');
    assert_ne!(
        key,
        super::context_menu_text_damage_key(Some(&menu), fg, bg)
    );
}

#[test]
fn pinned_status_lines_fit_natural_overlay_font_bounds() {
    use super::{Attrs, Family, FontSystem, Shaping, TextBuffer, Wrap};
    use kettle_i18n::{Language, Text, Translator};

    let mut fonts = FontSystem::new();
    super::load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    let family = kettle_config::font::FAMILY;
    for scale in [1.0, 1.25, 2.0] {
        let metrics = super::metrics_for(13.0, scale);
        let mut buffer = TextBuffer::new(&mut fonts, metrics);
        let text_cell = super::measure_cell(&mut fonts, &mut buffer, family, metrics);
        for language in [Language::En, Language::Es] {
            let tr = Translator::new(language);
            let text = format!(
                "{}\nEnter {} / Tab/\u{2191}\u{2193} {} / Esc {}",
                tr.text(Text::MenuProfilesPending),
                tr.text(Text::PickerHintRun),
                tr.text(Text::PickerHintSelect),
                tr.text(Text::PickerHintCancel),
            );
            for width in [296.0, 328.0, 720.0] {
                let width = width * scale;
                let status = super::MenuStatus::new(&text, width, text_cell);
                let band = (16.0, 30.0, width, status.height());
                for (index, line) in status.lines.iter().enumerate() {
                    let slot = status.line_rect(band, index);
                    buffer.set_metrics(metrics);
                    buffer.set_size(Some(slot.2), Some(slot.3));
                    buffer.set_wrap(Wrap::None);
                    buffer.set_text(
                        line,
                        &Attrs::new().family(Family::Name(family)),
                        Shaping::Advanced,
                        None,
                    );
                    buffer.shape_until_scroll(&mut fonts, false);
                    let runs: Vec<_> = buffer.layout_runs().collect();
                    assert_eq!(
                        runs.len(),
                        1,
                        "each projected line stays on one shaped line"
                    );
                    let run = &runs[0];
                    assert!(
                        run.line_w <= slot.2 + 0.01,
                        "status text fits without clipping"
                    );
                    assert!(run.line_height <= slot.3);
                    assert!(slot.1 + run.line_top >= band.1);
                    assert!(slot.1 + run.line_top + run.line_height <= band.1 + band.3 + 0.01);
                }
            }
        }
    }
}
