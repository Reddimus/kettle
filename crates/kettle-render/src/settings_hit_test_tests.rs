use super::{
    SETTINGS_FIELD_START, SettingsHit, SettingsOverlay, SettingsRow, settings_display_lines,
    settings_fit_line, settings_hit_test, settings_panel_cols, settings_panel_geometry,
    settings_panel_layout,
};

fn overlay() -> SettingsOverlay {
    SettingsOverlay {
        categories: vec!["Appearance".into(), "Graphics".into(), "Behavior".into()],
        label_cols: 31,
        value_cols: 25,
        active_category: 0,
        background_page: false,
        rows: vec![
            SettingsRow {
                label: "Theme".into(),
                value: "Mocha".into(),
                disabled: false,
            },
            SettingsRow {
                label: "Font size".into(),
                value: "14".into(),
                disabled: false,
            },
        ],
        focused_row: 0,
        title: "Settings — Appearance".into(),
        hints: "↑↓ field    ←→ change    Tab category    Esc close".into(),
        footer_note: None,
    }
}

#[test]
fn background_preview_opacity_uses_page_identity_in_every_language() {
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let mut set = overlay();
        set.categories = vec![
            tr.text(kettle_i18n::Text::SettingsCategoryBackground)
                .into(),
        ];
        set.active_category = 0;
        set.background_page = true;
        assert_eq!(set.backdrop_alpha(), 0.30);
        set.background_page = false;
        assert_eq!(set.backdrop_alpha(), 0.55);
    }
}

// Recompute the panel geometry the same way the draw + hit-test do, so the
// probes target real on-screen positions.
fn geom(set: &SettingsOverlay, cw: f32, ch: f32, sw: f32, sh: f32) -> (f32, f32, f32) {
    let lines = settings_display_lines(set);
    let row_h = ch + 6.0;
    let panel_w = (settings_panel_cols(set, &lines) * cw + 48.0).min((sw - 40.0).max(120.0));
    let panel_h = (lines.len() as f32 * row_h + 24.0).min((sh - 40.0).max(80.0));
    let px = ((sw - panel_w) * 0.5).max(0.0);
    let py = ((sh - panel_h) * 0.5).max(0.0);
    (px, py, row_h)
}

#[test]
fn long_labels_have_a_fixed_gap_and_columns_do_not_shift_with_categories() {
    let mut set = overlay();
    set.rows[0].label = "Completion overlay (new shells)".into();
    set.rows[0].value = "Automatic".into();
    let lines = settings_display_lines(&set);
    assert!(lines[SETTINGS_FIELD_START].contains("(new shells)  Automatic"));
    let value_col = super::display_width(
        lines[SETTINGS_FIELD_START]
            .split("Automatic")
            .next()
            .unwrap(),
    );
    set.active_category = 1;
    set.rows[0].label = "GPU device".into();
    let lines = settings_display_lines(&set);
    assert_eq!(
        super::display_width(
            lines[SETTINGS_FIELD_START]
                .split("Automatic")
                .next()
                .unwrap()
        ),
        value_col
    );
}

#[test]
fn narrow_rows_keep_label_gap_and_value_with_unicode_ellipsis() {
    let mut set = overlay();
    set.rows[0].label = "Completion overlay (new shells)".into();
    set.rows[0].value = "Automatic".into();
    let line = super::settings_fit_display_line(&set, SETTINGS_FIELD_START, "", 208.0, 8.0);
    assert!(line.contains("…  Automatic"), "{line}");
    assert!(super::display_width(&line) <= 22);
    set.rows[0].label = "界界界界界界界界界界界界界界界界".into();
    set.rows[0].value = "A very long value with 界".into();
    for cols in 1..70 {
        let line = super::settings_row_line(&set, &set.rows[0], "▸ ", cols);
        assert!(super::display_width(&line) <= cols, "{cols}: {line}");
    }
}

#[test]
fn tab_text_and_hit_regions_are_stable_and_clipped_tabs_are_inert() {
    let mut set = overlay();
    let text = settings_display_lines(&set)[1].clone();
    assert_eq!(text, "Appearance  Graphics  Behavior");
    set.active_category = 1;
    assert_eq!(settings_display_lines(&set)[1], text);
    let (px, py, row_h) = geom(&set, 8.0, 16.0, 208.0, 600.0);
    let y = py + 12.0 + row_h * 1.5;
    assert_eq!(
        settings_hit_test(&set, 8.0, 16.0, 208.0, 600.0, px + 16.0 + 9.5 * 8.0, y),
        SettingsHit::Category(0)
    );
    assert_eq!(
        settings_hit_test(&set, 8.0, 16.0, 208.0, 600.0, px + 16.0 + 11.0 * 8.0, y),
        SettingsHit::Inert
    );
    assert_eq!(
        settings_hit_test(&set, 8.0, 16.0, 208.0, 600.0, px + 16.0 + 16.5 * 8.0, y),
        SettingsHit::Inert
    );
}

#[test]
fn switching_categories_with_long_notes_keeps_the_panel_x_and_width() {
    let mut set = overlay();
    let before = settings_panel_geometry(&set, 8.0, 16.0, 1200.0, 900.0).rect;
    set.active_category = 1;
    set.footer_note = Some("Active GPU: A much longer GPU name than the old Appearance footer (Integrated, Vulkan)\nRestart Kettle or open a new window to apply pending changes.".into());
    let after = settings_panel_geometry(&set, 8.0, 16.0, 1200.0, 900.0).rect;
    assert_eq!((before.0, before.2), (after.0, after.2));
    let lines = settings_display_lines(&set);
    assert_eq!(
        lines.last().unwrap(),
        "Restart Kettle or open a new window to apply pending changes."
    );
}

#[test]
fn pending_notice_is_painted_in_a_284_pixel_graphics_window() {
    let mut set = overlay();
    set.active_category = 1;
    set.rows = (0..4)
        .map(|i| SettingsRow {
            label: format!("Graphics {i}"),
            value: "Automatic".into(),
            disabled: false,
        })
        .collect();
    let pending = "Restart Kettle or open a new window to apply pending changes.";
    set.footer_note = Some(format!("Active GPU: Test (Integrated, Vulkan)\n{pending}"));
    for focused in 0..4 {
        set.focused_row = focused;
        assert_notes_are_painted(&set, 284.0, &["Active GPU: Test", pending]);
    }
}

#[test]
fn contextual_and_pending_notes_are_painted_while_fields_scroll() {
    let mut set = overlay();
    set.rows = (0..12)
        .map(|i| SettingsRow {
            label: format!("Appearance {i}"),
            value: "On".into(),
            disabled: false,
        })
        .collect();
    let context = "Blur requires background opacity below 100%. Applies to new windows.";
    let pending = "Restart Kettle or open a new window to apply pending changes.";
    set.footer_note = Some(format!("{context}\n{pending}"));
    for focused in 0..12 {
        set.focused_row = focused;
        assert_notes_are_painted(
            &set,
            200.0,
            &["Blur requires background opacity below 100%.", pending],
        );
    }
}

fn assert_notes_are_painted(set: &SettingsOverlay, surface_h: f32, notes: &[&str]) {
    let lines = settings_display_lines(set);
    let layout = settings_panel_layout(set, &lines, 8.0, 16.0, 1200.0, surface_h);
    // This is the iterator used by the live TextArea pass, not a search
    // of all display strings. Check glyph placement against its clip.
    let painted: Vec<_> = super::settings_painted_lines(&layout).collect();
    for note in notes {
        let &(slot, index) = painted
            .iter()
            .find(|&&(_, index)| lines[index].starts_with(note))
            .unwrap_or_else(|| panic!("notice not painted at height {surface_h}: {note}"));
        let y = layout.py + 12.0 + slot as f32 * layout.row_h;
        assert!(y + 3.0 + 16.0 <= layout.py + layout.panel_h);
        assert!(y + 3.0 + 16.0 <= surface_h);
        let fitted =
            super::settings_fit_display_line(set, index, &lines[index], layout.panel_w, 8.0);
        assert!(fitted.starts_with(note), "notice clipped: {fitted}");
        assert_eq!(
            settings_hit_test(
                set,
                8.0,
                16.0,
                1200.0,
                surface_h,
                layout.px + 40.0,
                y + 11.0
            ),
            SettingsHit::Inert
        );
    }
    let focused = SETTINGS_FIELD_START + set.focused_row;
    let &(slot, _) = painted
        .iter()
        .find(|&&(_, i)| i == focused)
        .expect("focused field painted");
    let y = layout.py + 12.0 + slot as f32 * layout.row_h;
    assert!(y + layout.row_h <= layout.py + layout.panel_h);
    assert_eq!(
        settings_hit_test(
            set,
            8.0,
            16.0,
            1200.0,
            surface_h,
            layout.px + 40.0,
            y + 11.0
        ),
        SettingsHit::Field(set.focused_row)
    );
}

#[test]
fn inline_edits_keep_the_tail_and_caret_when_the_value_is_ellipsized() {
    let mut set = overlay();
    set.rows[0].label = "Image file".into();
    set.rows[0].value = "/a/very/long/path/with/a/wide/界/wallpaper.png▏".into();
    for cols in 6..70 {
        let line = super::settings_row_line(&set, &set.rows[0], "▸ ", cols);
        assert!(line.ends_with('▏'), "{cols}: {line}");
        assert!(super::display_width(&line) <= cols);
    }
}

#[test]
fn a_clipped_tab_ellipsis_cannot_switch_categories() {
    let set = overlay();
    let (px, py, row_h) = geom(&set, 8.0, 16.0, 208.0, 600.0);
    assert_eq!(
        settings_hit_test(
            &set,
            8.0,
            16.0,
            208.0,
            600.0,
            px + 16.0 + 16.5 * 8.0,
            py + 12.0 + row_h * 1.5
        ),
        SettingsHit::Inert,
    );
}

#[test]
fn outside_the_panel_dismisses() {
    let set = overlay();
    assert_eq!(
        settings_hit_test(&set, 8.0, 16.0, 800.0, 600.0, 2.0, 2.0),
        SettingsHit::Outside
    );
}

#[test]
fn field_rows_map_by_index() {
    let set = overlay();
    let (px, py, row_h) = geom(&set, 8.0, 16.0, 800.0, 600.0);
    for f in 0..set.rows.len() {
        let line = SETTINGS_FIELD_START + f;
        let y = py + 12.0 + line as f32 * row_h + row_h * 0.5;
        assert_eq!(
            settings_hit_test(&set, 8.0, 16.0, 800.0, 600.0, px + 40.0, y),
            SettingsHit::Field(f),
            "field {f} at y={y}"
        );
    }
}

#[test]
fn title_and_blank_rows_are_inert() {
    let set = overlay();
    let (px, py, row_h) = geom(&set, 8.0, 16.0, 800.0, 600.0);
    // Line 0 = title.
    let y0 = py + 12.0 + row_h * 0.5;
    assert_eq!(
        settings_hit_test(&set, 8.0, 16.0, 800.0, 600.0, px + 40.0, y0),
        SettingsHit::Inert
    );
}

#[test]
fn category_tabs_map_by_x() {
    let set = overlay();
    let (px, py, row_h) = geom(&set, 8.0, 16.0, 800.0, 600.0);
    let y = py + 12.0 + row_h * 1.5; // line 1 = tab strip
    // Tab 0 "Appearance" starts at text_left = px + 16.
    let hit0 = settings_hit_test(&set, 8.0, 16.0, 800.0, 600.0, px + 16.0 + 4.0, y);
    assert_eq!(hit0, SettingsHit::Category(0));
    // The two-space gap must stay inert rather than selecting a tab.
    assert_eq!(
        settings_hit_test(&set, 8.0, 16.0, 800.0, 600.0, px + 16.0 + 11.0 * 8.0, y),
        SettingsHit::Inert
    );
    // Tab 1 "Graphics" begins after tab0 (10 cols) + 2 spaces = col 12.
    let x1 = px + 16.0 + (12.0 + 3.0) * 8.0;
    assert_eq!(
        settings_hit_test(&set, 8.0, 16.0, 800.0, 600.0, x1, y),
        SettingsHit::Category(1)
    );
}

#[test]
fn focused_row_stays_inside_a_short_settings_panel() {
    let mut set = overlay();
    set.rows = (0..12)
        .map(|index| SettingsRow {
            label: format!("Setting {index}"),
            value: index.to_string(),
            disabled: false,
        })
        .collect();
    set.focused_row = 11;

    let lines = settings_display_lines(&set);
    let layout = settings_panel_layout(&set, &lines, 8.0, 16.0, 400.0, 200.0);
    let focused_line = SETTINGS_FIELD_START + set.focused_row;

    assert!(layout.first_line > 0, "the tall panel must scroll");
    assert!(focused_line >= layout.first_line);
    assert!(focused_line < layout.first_line + layout.visible_lines);
    let highlight_y = layout.py + 12.0 + (focused_line - layout.first_line) as f32 * layout.row_h;
    assert!(highlight_y >= layout.py);
    assert!(highlight_y + layout.row_h <= layout.py + layout.panel_h);

    let public = settings_panel_geometry(&set, 8.0, 16.0, 400.0, 200.0);
    assert_eq!(public.first_line, layout.first_line);
    assert_eq!(public.visible_lines, layout.visible_lines);
    assert_eq!(public.focused_rect.1, highlight_y);
    assert!(public.focused_rect.1 + public.focused_rect.3 <= public.rect.1 + public.rect.3);

    assert_eq!(
        settings_hit_test(
            &set,
            8.0,
            16.0,
            400.0,
            200.0,
            layout.px + 40.0,
            highlight_y + layout.row_h * 0.5,
        ),
        SettingsHit::Field(11)
    );
}

#[test]
fn narrow_settings_rows_are_ellipsized_to_one_line() {
    let fitted = settings_fit_line(
        "  Background image          /Users/example/a/very/long/wallpaper.png",
        208.0,
        8.0,
    );

    assert_eq!(super::display_width(&fitted), 22);
    assert!(fitted.ends_with('…'));
}
