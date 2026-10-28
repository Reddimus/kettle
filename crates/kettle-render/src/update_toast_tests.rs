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
