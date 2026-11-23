use super::settings::{
    SettingsTextKind, SettingsTextStyle, settings_row_surfaces, settings_text_color,
    settings_text_items, shape_settings_text,
};
use super::*;

fn grouped() -> SettingsOverlay {
    SettingsOverlay {
        categories: (0..11).map(|i| format!("Category {i}")).collect(),
        title: "Appearance".into(),
        reset_label: "Reset".into(),
        rows: (0..12)
            .map(|i| SettingsRow {
                label: format!("Setting {i}"),
                group: ["Colors and text", "Window", "Cursor"][i / 4].into(),
                value: "14 px".into(),
                description: "Description".into(),
                reset: Some(true),
                ..Default::default()
            })
            .collect(),
        hints: vec![
            ("Up/Down".into(), "field".into()),
            ("Left/Right".into(), "change".into()),
            ("g/G".into(), "ends".into()),
            ("Del".into(), "reset".into()),
            ("Tab".into(), "category".into()),
            ("Esc".into(), "close".into()),
        ],
        ..Default::default()
    }
}

fn center(r: Rect4) -> (f32, f32) {
    (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0)
}
fn disjoint(a: Rect4, b: Rect4) -> bool {
    a.0 + a.2 <= b.0 + 0.01
        || b.0 + b.2 <= a.0 + 0.01
        || a.1 + a.3 <= b.1 + 0.01
        || b.1 + b.3 <= a.1 + 0.01
}

#[test]
fn settings_geometry_keeps_logical_size_and_sidebar_at_both_scales() {
    let set = grouped();
    for font in [13.0, 14.0, 18.0] {
        for (surface, expected) in [
            ((640.0, 820.0), (560.0, 688.8)),
            ((1280.0, 820.0), (921.6, 688.8)),
            ((1600.0, 968.0), (980.0, 813.12)),
            ((300.0, 200.0), (268.0, 168.0)),
        ] {
            let one = settings_layout(&set, (font * 0.6, font * 1.25), surface, font * 0.6, 1.0);
            for scale in [1.0, 2.0] {
                let layout = settings_layout(
                    &set,
                    (font * 0.6 * scale, font * 1.25 * scale),
                    (surface.0 * scale, surface.1 * scale),
                    font * 0.6 * scale,
                    scale,
                );
                assert!((layout.rect.2 / scale - expected.0).abs() < 0.01);
                assert!((layout.rect.3 / scale - expected.1).abs() < 0.01);
                assert_eq!(layout.sidebar_rect.is_some(), surface.0 >= 1280.0);
                let panel = settings_panel_geometry(
                    &set,
                    font * 0.6 * scale,
                    font * 1.25 * scale,
                    surface.0 * scale,
                    surface.1 * scale,
                    scale,
                );
                assert_eq!(panel.rect, layout.rect);
                if let Some(row) = layout.rows.first() {
                    let editor = settings_value_rect(
                        &set,
                        font * 0.6 * scale,
                        font * 1.25 * scale,
                        surface.0 * scale,
                        surface.1 * scale,
                        font * 0.6 * scale,
                        scale,
                    );
                    assert!(
                        editor.0 >= row.chip_rect.0
                            && editor.0 + editor.2 <= row.chip_rect.0 + row.chip_rect.2 + 0.01
                    );
                    let point = center(row.chip_rect);
                    assert_eq!(
                        settings_hit_test(
                            &set,
                            font * 0.6 * scale,
                            font * 1.25 * scale,
                            surface.0 * scale,
                            surface.1 * scale,
                            point.0,
                            point.1,
                            scale
                        ),
                        SettingsHit::Field(row.index)
                    );
                }
                assert_eq!(layout.rows.len(), one.rows.len());
                assert_eq!(layout.sidebar_hints, one.sidebar_hints);
                for (a, b) in layout.rows.iter().zip(&one.rows) {
                    assert_eq!(a.index, b.index);
                    for (actual, expected) in [
                        (a.rect.0 / scale, b.rect.0),
                        (a.rect.1 / scale, b.rect.1),
                        (a.rect.2 / scale, b.rect.2),
                        (a.rect.3 / scale, b.rect.3),
                    ] {
                        assert!((actual - expected).abs() < 0.01);
                    }
                    assert_eq!(
                        settings_hit_test_with_text_width(
                            &set,
                            (font * 0.6 * scale, font * 1.25 * scale),
                            (surface.0 * scale, surface.1 * scale),
                            center(a.chip_rect),
                            font * 0.6 * scale,
                            scale
                        ),
                        SettingsHit::Field(a.index)
                    );
                }
            }
        }
    }
}

#[test]
fn settings_headings_stick_and_never_hit_or_overlap_rows() {
    let mut set = grouped();
    for scale in [1.0, 2.0] {
        for focus in 0..12 {
            set.focused_row = focus;
            let layout = settings_layout(
                &set,
                (8.4 * scale, 17.5 * scale),
                (1280.0 * scale, 820.0 * scale),
                8.4 * scale,
                scale,
            );
            assert!(layout.rows.iter().any(|row| row.index == focus));
            assert_eq!(layout.groups.first().unwrap().0, layout.first_row);
            for (first, heading) in &layout.groups {
                assert_eq!(
                    layout.pointer_hit(center(*heading), None),
                    SettingsHit::Inert
                );
                assert!(layout.rows.iter().all(|row| disjoint(*heading, row.rect)));
                assert!(
                    layout.rows.iter().any(
                        |row| row.index == *first && row.rect.1 >= heading.1 + heading.3 - 0.01
                    )
                );
            }
            let items = settings_text_items(&set, &layout, layout.text_cw);
            for (index, _) in &layout.groups {
                assert!(
                    items
                        .iter()
                        .any(|item| item.kind == SettingsTextKind::Heading
                            && item.text == set.heading(*index))
                );
            }
        }
    }
}

#[test]
fn settings_short_panel_reserves_the_whole_heading_or_keeps_the_focused_row() {
    let mut set = grouped();
    set.hints.clear();
    set.footer_note = Some("note\nnote\nnote\nnote\nnote\nnote".into());
    for scale in [1.0, 2.0] {
        for (height, headings) in [(291.375, 1), (290.875, 0), (287.0, 0)] {
            for focus in 0..12 {
                set.focused_row = focus;
                let layout = settings_layout(
                    &set,
                    (8.4 * scale, 17.5 * scale),
                    (1600.0 * scale, height * scale),
                    8.4 * scale,
                    scale,
                );
                assert_eq!(
                    layout.groups.len(),
                    headings,
                    "height={height} focus={focus}"
                );
                assert_eq!(layout.rows.len(), 1);
                assert_eq!(layout.rows[0].index, focus);
                assert_eq!(layout.footer.len(), 6);
                assert!(
                    layout
                        .footer
                        .iter()
                        .all(|(_, rect)| disjoint(*rect, layout.rows[0].rect))
                );
            }
        }
        set.footer_note = Some("pending notice\n".repeat(100));
        let layout = settings_layout(
            &set,
            (8.4 * scale, 17.5 * scale),
            (1600.0 * scale, 287.0 * scale),
            8.4 * scale,
            scale,
        );
        assert_eq!(layout.rows.len(), 1);
        assert!(layout.footer.len() < 100);
        set.footer_note = Some("note\nnote\nnote\nnote\nnote\nnote".into());
    }
}

#[test]
fn settings_hints_use_sidebar_only_when_pairs_fit_and_keep_footer_gap_only_for_notes() {
    let mut set = grouped();
    for scale in [1.0, 2.0] {
        let wide = settings_layout(
            &set,
            (8.4 * scale, 17.5 * scale),
            (1280.0 * scale, 820.0 * scale),
            8.4 * scale,
            scale,
        );
        assert!(wide.sidebar_hints);
        assert!(wide.footer.is_empty());
        assert_eq!(wide.hints.len(), 6);
        assert_eq!(wide.rows.len(), 10);
        let side = wide.sidebar_rect.unwrap();
        let category_bottom = wide
            .category_rects
            .iter()
            .flatten()
            .map(|r| r.1 + r.3)
            .fold(0.0_f32, f32::max);
        for hint in &wide.hints {
            assert!(hint.rect.1 >= category_bottom + 12.0 * scale - 0.01);
            assert!(hint.rect.0 >= side.0 && hint.rect.0 + hint.rect.2 <= side.0 + side.2);
            assert_eq!(hint.runs.iter().filter(|(_, key)| *key).count(), 1);
        }
        for surface in [(640.0, 820.0), (889.0, 360.0)] {
            let layout = settings_layout(
                &set,
                (8.4 * scale, 17.5 * scale),
                (surface.0 * scale, surface.1 * scale),
                8.4 * scale,
                scale,
            );
            assert!(!layout.sidebar_hints);
            let lines: Vec<_> = layout.hints.iter().map(|hint| hint.text()).collect();
            for (key, action) in &set.hints {
                assert!(
                    lines
                        .iter()
                        .any(|line| line.contains(&format!("{key} {action}"))),
                    "{lines:?}"
                );
            }
            assert!(
                layout
                    .hints
                    .iter()
                    .all(|hint| display_width(&hint.text()) as f32 * layout.text_cw
                        <= hint.rect.2 + 0.01)
            );
        }
    }
    set.footer_note = Some("Restart required".into());
    let layout = settings_layout(&set, (8.4, 17.5), (1600.0, 968.0), 8.4, 1.0);
    assert_eq!(layout.rows.len(), 11);
    assert_eq!(layout.footer.last().unwrap().0, "Restart required");
}

#[test]
fn settings_reset_segments_share_an_outline_and_switches_keep_four_logical_pixels() {
    let mut set = grouped();
    for control in [
        SettingsControl::Value,
        SettingsControl::Choice,
        SettingsControl::Toggle(false),
    ] {
        set.rows[0].control = control;
        for scale in [1.0, 2.0] {
            for width in (400..=1600).step_by(40) {
                let layout = settings_layout(
                    &set,
                    (8.4 * scale, 17.5 * scale),
                    (width as f32 * scale, 1000.0 * scale),
                    8.4 * scale,
                    scale,
                );
                let row = &layout.rows[0];
                let reset = row.reset.unwrap();
                let gap = (row.chip_rect.0 - reset.rect.0 - reset.rect.2) / scale;
                assert!(
                    (gap - if matches!(control, SettingsControl::Toggle(_)) {
                        4.0
                    } else {
                        0.0
                    })
                    .abs()
                        < 0.01
                );
                assert_eq!(
                    layout.pointer_hit(center(reset.rect), None),
                    SettingsHit::Reset(0)
                );
                assert_eq!(
                    layout.pointer_hit(center(reset.rect), Some(1)),
                    SettingsHit::Field(0)
                );
                let theme = kettle_config::Theme::by_name("TokyoNight Night");
                let mut colors = chrome::Colors::default();
                let palette = settings::settings_colors(&theme, theme.accent, true, &mut colors);
                let surfaces = settings_row_surfaces(
                    row,
                    &set.rows[0],
                    &palette,
                    (theme.background, theme.accent, theme.foreground),
                    true,
                    scale,
                );
                if !matches!(control, SettingsControl::Toggle(_)) {
                    assert_eq!(surfaces[0].rect.0, reset.rect.0);
                    assert_eq!(
                        surfaces[0].rect.0 + surfaces[0].rect.2,
                        row.chip_rect.0 + row.chip_rect.2
                    );
                    assert!(surfaces.iter().any(
                        |surface| surface.rect.2 == scale && surface.rect.3 == row.chip_rect.3
                    ));
                }
            }
        }
    }
}

#[test]
fn settings_new_control_surfaces_and_inks_meet_their_adjacent_backgrounds() {
    let mut colors = chrome::Colors::default();
    for name in kettle_config::Theme::list() {
        let theme = kettle_config::Theme::by_name(name);
        for linear in [false, true] {
            for accent in [theme.accent, Rgb::new(0xbb, 0x99, 0xf8)] {
                let palette = settings::settings_colors(&theme, accent, linear, &mut colors);
                assert_ne!(palette.well, theme.background, "{name}");
                for outside in [theme.background, palette.selected] {
                    assert!(
                        color::contrast_ratio(palette.unavailable_control, outside) >= 3.0,
                        "{name} {linear} unavailable"
                    );
                }
                for (ink, outside) in [
                    (palette.well_control, theme.background),
                    (palette.selected_well_control, palette.selected),
                ] {
                    let floor = if [Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)]
                        .iter()
                        .any(|ink| {
                            color::contrast_ratio(*ink, palette.well)
                                .min(color::contrast_ratio(*ink, outside))
                                >= 4.5
                        }) {
                        4.5
                    } else {
                        3.0
                    };
                    assert!(
                        color::contrast_ratio(ink, palette.well)
                            .min(color::contrast_ratio(ink, outside))
                            >= floor,
                        "{name} {linear} well"
                    );
                }
                assert!(
                    color::contrast_ratio(palette.selected_well_control, theme.background) >= 3.0,
                    "{name} joined Reset"
                );
                for control in [
                    SettingsControl::Value,
                    SettingsControl::Text,
                    SettingsControl::Choice,
                    SettingsControl::Toggle(true),
                ] {
                    for disabled in [false, true] {
                        let mut set = grouped();
                        set.rows[0].control = control;
                        set.rows[0].disabled = disabled;
                        set.rows[0].unavailable = true;
                        let well =
                            matches!(control, SettingsControl::Value | SettingsControl::Text);
                        let ink = settings_text_color(
                            &set,
                            SettingsTextKind::Value(0),
                            &palette,
                            (&theme, accent, theme.foreground),
                            &mut colors,
                        );
                        assert!(
                            color::contrast_ratio(
                                ink,
                                if well { palette.well } else { theme.background }
                            ) >= 4.5,
                            "{name} {control:?} disabled={disabled}"
                        );
                        let layout = settings_layout(&set, (8.4, 17.5), (1280.0, 820.0), 8.4, 1.0);
                        let surfaces = settings_row_surfaces(
                            &layout.rows[0],
                            &set.rows[0],
                            &palette,
                            (theme.background, accent, theme.foreground),
                            true,
                            1.0,
                        );
                        assert_eq!(
                            surfaces[0].fill,
                            if well { palette.well } else { theme.background }
                        );
                        if matches!(control, SettingsControl::Toggle(_)) && set.rows[0].unavailable
                        {
                            assert_eq!(surfaces[0].stroke, palette.unavailable_control);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn settings_rich_hint_cache_recolours_unchanged_text_after_theme_and_metrics_changes() {
    let mut fs = FontSystem::new();
    fonts::load_bundled_font(&mut fs, kettle_config::font::REGULAR);
    let mut buffer = TextBuffer::new(&mut fs, Metrics::new(14.0, 17.5));
    let mut cache = 0;
    let set = grouped();
    let mut before = None;
    for name in ["TokyoNight Night", "Kettle Day"] {
        let theme = kettle_config::Theme::by_name(name);
        let mut colors = chrome::Colors::default();
        let palette = settings::settings_colors(&theme, theme.accent, true, &mut colors);
        let layout = settings_layout(&set, (8.4, 17.5), (1280.0, 820.0), 8.4, 1.0);
        let item = settings_text_items(&set, &layout, 8.4)
            .into_iter()
            .find(|item| !item.runs.is_empty())
            .unwrap();
        let style = SettingsTextStyle {
            metrics: Metrics::new(14.0, 17.5),
            family: kettle_config::font::FAMILY,
            color: colors.text(theme.foreground, theme.background),
            description: palette.description,
        };
        shape_settings_text(&mut fs, &mut buffer, &mut cache, &item, &style);
        let inks: Vec<_> = buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.color_opt))
            .collect();
        assert!(inks.contains(&Some(GColor::rgb(
            style.color.r,
            style.color.g,
            style.color.b
        ))));
        assert!(inks.contains(&Some(GColor::rgb(
            style.description.r,
            style.description.g,
            style.description.b
        ))));
        if let Some((key, old)) = &before {
            assert_ne!(*key, cache);
            assert_ne!(*old, inks);
        }
        before = Some((cache, inks));
        // A stable frame keeps the cache; width and font metrics invalidate it.
        let stable = cache;
        shape_settings_text(&mut fs, &mut buffer, &mut cache, &item, &style);
        assert_eq!(cache, stable);
        let mut narrower = item;
        narrower.rect.2 -= 20.0;
        shape_settings_text(&mut fs, &mut buffer, &mut cache, &narrower, &style);
        assert_ne!(cache, stable);
        let key = cache;
        let larger = SettingsTextStyle {
            metrics: Metrics::new(15.0, 18.75),
            ..style
        };
        shape_settings_text(&mut fs, &mut buffer, &mut cache, &narrower, &larger);
        assert_ne!(cache, key);
    }
}

#[test]
fn settings_cold_open_loads_bundled_bold_for_title_headings_and_labels() {
    let _serial = gpu_tests::gpu_test_guard();
    let Some((mut renderer, mut cfg)) = headless_tests::renderer(1280, 820) else {
        eprintln!("SETTINGS_BOLD_GPU_SKIP: no adapter; portable font coverage is separate");
        return;
    };
    cfg.font_family = kettle_config::font::FAMILY.into();
    renderer.font_family = cfg.font_family.clone().into();
    renderer.font_system =
        FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
    fonts::load_bundled_font(&mut renderer.font_system, kettle_config::font::REGULAR);
    renderer.bundled_style_faces_loaded = false;
    renderer.chrome_style_key = 0;
    let _ = renderer
        .render_frame(&[], &TabBar::hidden(), &cfg, &Overlay::default())
        .unwrap();
    assert!(!renderer.bundled_style_faces_loaded);
    let set = grouped();
    let overlay = Overlay {
        settings: Some(set.clone()),
        ..Default::default()
    };
    let _ = renderer
        .render_frame(&[], &TabBar::hidden(), &cfg, &overlay)
        .unwrap();
    assert!(renderer.bundled_style_faces_loaded);
    let layout = settings_layout(
        &set,
        (renderer.cell_w, renderer.overlay_text_line_height()),
        (1280.0, 820.0),
        renderer.overlay_text_cell_width(),
        renderer.scale,
    );
    let items = settings_text_items(&set, &layout, renderer.overlay_text_cell_width());
    for (index, item) in items.iter().enumerate().filter(|(_, item)| {
        matches!(
            item.kind,
            SettingsTextKind::Title | SettingsTextKind::Heading | SettingsTextKind::Label(_)
        )
    }) {
        let glyphs: Vec<_> = renderer.settings_buffers[index]
            .layout_runs()
            .flat_map(|run| run.glyphs)
            .collect();
        assert!(!glyphs.is_empty());
        for glyph in glyphs {
            let face = renderer.font_system.db().face(glyph.font_id).unwrap();
            assert_eq!(face.weight, fontdb::Weight::BOLD, "{}", item.text);
        }
    }
    eprintln!(
        "SETTINGS_BOLD_GPU_PASS: {:?}",
        renderer.gpu().adapter_info()
    );
}

#[test]
fn settings_bundled_bold_label_advances_fit_their_measured_slots() {
    let mut fs = FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
    for face in kettle_config::font::all() {
        fonts::load_bundled_font(&mut fs, face);
    }
    let mut buffer = TextBuffer::new(&mut fs, Metrics::new(14.0, 17.5));
    for font in [13.0, 14.0, 18.0] {
        for scale in [1.0, 2.0] {
            for text in [
                "Window padding",
                "Colores y texto",
                "Mostrar títulos de paneles",
            ] {
                let cw = font * 0.6 * scale;
                let item = settings::SettingsTextItem {
                    text: text.into(),
                    rect: (
                        0.0,
                        0.0,
                        display_width(text) as f32 * cw,
                        font * 1.25 * scale,
                    ),
                    kind: SettingsTextKind::Label(0),
                    runs: Vec::new(),
                };
                let style = SettingsTextStyle {
                    metrics: Metrics::new(font * scale, font * 1.25 * scale),
                    family: kettle_config::font::FAMILY,
                    color: Rgb::new(255, 255, 255),
                    description: Rgb::new(200, 200, 200),
                };
                shape_settings_text(&mut fs, &mut buffer, &mut 0, &item, &style);
                let runs: Vec<_> = buffer.layout_runs().collect();
                assert_eq!(runs.len(), 1);
                assert!(
                    runs[0].line_w <= item.rect.2 + 0.02,
                    "{text} at {font} {scale}x"
                );
                for glyph in runs[0].glyphs {
                    assert_eq!(
                        fs.db().face(glyph.font_id).unwrap().weight,
                        fontdb::Weight::BOLD
                    );
                }
            }
        }
    }
}
