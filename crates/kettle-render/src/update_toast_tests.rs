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
        assert!(update_toast_geometry(size, 30.0, 1.0, (8.0, 16.0), 8).is_none());
    }
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
