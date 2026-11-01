use super::update_toast_geometry;

#[test]
fn toast_stays_top_center_at_each_scale_without_overlapping_controls() {
    for scale in [1.0, 1.5, 2.0] {
        let geometry = update_toast_geometry(
            (800.0 * scale, 600.0 * scale),
            30.0 * scale,
            scale,
            (8.0 * scale, 16.0 * scale),
            8,
            "v6.0.0 is available.",
        )
        .unwrap();
        let (x, y, width, height) = geometry.rect;
        assert_eq!(x + width * 0.5, 400.0 * scale);
        assert_eq!(y, 42.0 * scale);
        assert!(x >= 16.0 * scale && y + height < 600.0 * scale);
        assert!(geometry.message.0 + geometry.message.2 <= geometry.details.0);
        assert!(geometry.details.0 + geometry.details.2 <= geometry.close.0);
        assert!(geometry.close.0 + geometry.close.2 <= x + width);
    }
}

#[test]
fn an_unusable_surface_defers_the_notification() {
    for size in [(100.0, 100.0), (800.0, 20.0), (f32::NAN, 600.0)] {
        assert!(
            update_toast_geometry(size, 30.0, 1.0, (8.0, 16.0), 8, "v6.0.0 is available.")
                .is_none()
        );
    }
}

#[test]
fn large_fonts_keep_the_whole_warning_and_both_controls() {
    let geometry = update_toast_geometry(
        (1000.0, 700.0),
        40.0,
        1.0,
        (24.0, 48.0),
        8,
        "Could not install v6.0.0.",
    )
    .unwrap();
    assert!(geometry.message.1 + geometry.message.3 <= geometry.details.1);
    assert!(geometry.message.3 >= 2.0 * 48.0);
    assert!(geometry.details.2 >= 8.0 * 24.0);
    assert!(geometry.details.0 + geometry.details.2 <= geometry.close.0);
    assert!(
        update_toast_geometry(
            (1000.0, 700.0),
            40.0,
            1.0,
            (24.0, 48.0),
            8,
            &format!("Could not install v6.0.0-{}.", "x".repeat(200))
        )
        .is_none()
    );
}

#[test]
fn hyphenated_version_text_fits_its_actual_shaped_rows() {
    use super::{Attrs, Family, FontSystem, Metrics, Shaping, TextBuffer};
    let mut fonts = FontSystem::new();
    super::load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    let metrics = Metrics::new(40.0, 50.0);
    let mut buffer = TextBuffer::new(&mut fonts, metrics);
    let family = kettle_config::font::FAMILY;
    let cell = super::measure_cell(&mut fonts, &mut buffer, family, metrics);
    let message = "Could not install v6.0.0-aaa-bbbbbbbbbb-cccccccccc.";
    let geometry = update_toast_geometry((1000.0, 700.0), 40.0, 1.0, cell, 8, message).unwrap();
    let (text, wrap) = super::toast_buffer_text(message, geometry.message.2, cell.0, true);
    buffer.set_wrap(wrap);
    buffer.set_size(Some(geometry.message.2), Some(geometry.message.3));
    buffer.set_text(
        &text,
        &Attrs::new().family(Family::Name(family)),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(&mut fonts, false);
    let rows: usize = buffer
        .lines
        .iter()
        .filter_map(|line| line.layout_opt())
        .map(Vec::len)
        .sum();
    assert!(
        rows as f32 * cell.1 <= geometry.message.3,
        "{rows} shaped rows exceed {:?}",
        geometry.message
    );
}

#[test]
fn toast_text_and_focus_ring_are_readable_in_every_bundled_theme() {
    for name in kettle_config::Theme::list() {
        let theme = kettle_config::Theme::by_name(name);
        let background = theme.palette[0];
        let text = super::color::with_min_contrast(theme.foreground, background, 4.5);
        assert!(
            super::color::contrast_ratio(text, background) + 1e-6 >= 4.5,
            "{name}"
        );
        for accent in theme.palette {
            let ring = super::color::with_min_contrast(accent, background, 3.0);
            assert!(
                super::color::contrast_ratio(ring, background) + 1e-6 >= 3.0,
                "{name}"
            );
        }
    }
}

#[test]
fn toast_uses_chrome_metrics_with_scaled_terminal_cells() {
    let paint = super::production_source();
    let shape = paint
        .split("if let Some(toast) = &overlay.update_toast {")
        .nth(1)
        .unwrap()
        .split("// Context-menu row labels")
        .next()
        .unwrap();
    let ui = include_str!("../../kettle-ui/src/app/update_toast.rs");
    assert!(
        shape.contains("self.overlay_text_cell_width()")
            && ui.contains("renderer.overlay_text_cell_width()")
            && ui.contains("renderer.overlay_text_line_height()"),
        "toast uses terminal cell metrics instead of chrome metrics"
    );
    for (width_scale, height_scale) in [(0.5, 0.5), (2.0, 2.0)] {
        let terminal = (8.0 * width_scale, 16.0 * height_scale);
        let chrome = (terminal.0 / width_scale, terminal.1 / height_scale);
        let message = "Could not install v6.0.0-aaa-bbbbbbbbbb-cccccccccc.";
        let geometry = update_toast_geometry((800.0, 600.0), 0.0, 1.0, chrome, 8, message).unwrap();
        let (text, _) = super::toast_buffer_text(message, geometry.message.2, chrome.0, true);
        assert!(text.lines().count() as f32 * chrome.1 <= geometry.message.3);
    }
}

#[test]
fn toast_redesign_short_notice_is_one_row_with_inset_controls() {
    let g = update_toast_geometry(
        (800.0, 600.0),
        30.0,
        1.0,
        (8.0, 16.0),
        7,
        "v6.0.0 is available.",
    )
    .unwrap();
    assert_eq!(
        g.message.3, 16.0,
        "a short notice must not reserve an empty second row"
    );
    assert!(
        g.rect.2 < 480.0,
        "short notifications should hug their content"
    );
    assert!(g.close.0 + g.close.2 < g.rect.0 + g.rect.2);
    assert!(g.close.1 > g.rect.1);
}

#[test]
fn toast_contour_owns_only_visible_body_and_inset_control_targets() {
    use super::UpdateToastHit;
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let g = update_toast_geometry(
            (800.0 * scale, 600.0 * scale),
            30.0 * scale,
            scale,
            (8.0 * scale, 16.0 * scale),
            7,
            "v6.0.0 is available.",
        )
        .unwrap();
        let (x, y, w, h) = g.rect;
        for (px, py) in [
            (x, y),
            (x + w - 1.0, y),
            (x, y + h - 1.0),
            (x + w, y + h * 0.5),
        ] {
            assert_eq!(g.hit_test(px, py), None);
        }
        assert_eq!(
            g.hit_test(x + w * 0.5, y + 2.0 * scale),
            Some(UpdateToastHit::Body)
        );
        for (rect, hit) in [
            (g.details, UpdateToastHit::Details),
            (g.close, UpdateToastHit::Close),
        ] {
            assert_eq!(
                g.hit_test(rect.0 + rect.2 * 0.5, rect.1 + rect.3 * 0.5),
                Some(hit)
            );
        }
        // The circular Close retains a forgiving rectangular target inside the body.
        assert_eq!(
            g.hit_test(g.close.0 + scale, g.close.1 + scale),
            Some(UpdateToastHit::Close)
        );
        assert_eq!(g.hit_test(f32::NAN, y), None);
    }
}

#[test]
fn toast_shaped_layout_covers_localized_narrow_and_large_font_notices() {
    use super::{Attrs, Family, FontSystem, Metrics, Shaping, TextBuffer};
    let mut fonts = FontSystem::new();
    super::load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    for scale in [1.0, 1.25, 2.0] {
        for font_size in [14.0, 24.0, 40.0, 64.0] {
            let metrics = Metrics::new(font_size * scale, font_size * 1.25 * scale);
            let mut buffer = TextBuffer::new(&mut fonts, metrics);
            let family = kettle_config::font::FAMILY;
            let cell = super::measure_cell(&mut fonts, &mut buffer, family, metrics);
            for width in [360.0, 800.0] {
                for (message, details) in [
                    ("v6.0.0 is available.", "Details"),
                    ("Kettle v6.0.0 est\u{00e1} instalado", "Detalles"),
                    (
                        "Could not install v6.0.0-aaa-bbbbbbbbbb-cccccccccc.",
                        "Details",
                    ),
                ] {
                    let Some(g) = update_toast_geometry(
                        (width * scale, 1200.0 * scale),
                        30.0 * scale,
                        scale,
                        cell,
                        details.len(),
                        message,
                    ) else {
                        continue;
                    };
                    for (text, rect, wrap) in [
                        (message, g.message, true),
                        (details, g.details, false),
                        ("\u{00d7}", g.close, false),
                    ] {
                        let (text, mode) = super::toast_buffer_text(text, rect.2, cell.0, wrap);
                        buffer.set_wrap(mode);
                        buffer.set_size(Some(rect.2), Some(rect.3));
                        buffer.set_text(
                            &text,
                            &Attrs::new().family(Family::Name(family)),
                            Shaping::Advanced,
                            None,
                        );
                        buffer.shape_until_scroll(&mut fonts, false);
                        let runs: Vec<_> = buffer.layout_runs().collect();
                        assert!(
                            runs.len() as f32 * cell.1 <= rect.3 + 0.01,
                            "{message}: {g:?}"
                        );
                        assert!(
                            runs.iter().all(|r| r.line_w <= rect.2 + 0.01),
                            "{text}: {g:?}"
                        );
                    }
                    assert!(
                        g.details.0 >= g.rect.0 && g.close.0 + g.close.2 <= g.rect.0 + g.rect.2
                    );
                }
            }
        }
    }
    let g = update_toast_geometry(
        (360.0, 600.0),
        30.0,
        1.0,
        (8.0, 16.0),
        8,
        "Kettle v6.0.0 est\u{00e1} instalado",
    )
    .unwrap();
    assert_eq!(g.message.3, 32.0);
    assert!(g.rect.3 <= 56.0, "two rows should keep inline controls");
}
