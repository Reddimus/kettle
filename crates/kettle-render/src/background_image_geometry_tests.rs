use super::{background_image_rect, rect_covers_surface};

#[test]
fn oversized_center_wallpaper_keeps_natural_size_and_crops() {
    assert_eq!(
        background_image_rect("center", "center", "middle", [100.0, 80.0], [140.0, 120.0]),
        [-20.0, -20.0, 140.0, 120.0]
    );
    assert_eq!(
        background_image_rect("center", "right", "bottom", [100.0, 80.0], [140.0, 120.0]),
        [-40.0, -40.0, 140.0, 120.0]
    );
}

#[test]
fn wallpaper_coverage_requires_all_four_surface_edges() {
    let surface = [100.0, 80.0];
    assert!(rect_covers_surface([0.0, 0.0, 100.0, 80.0], surface));
    assert!(rect_covers_surface([-20.0, -10.0, 140.0, 100.0], surface));
    for rect in [
        [1.0, 0.0, 100.0, 80.0],
        [0.0, 1.0, 100.0, 80.0],
        [0.0, 0.0, 99.0, 80.0],
        [0.0, 0.0, 100.0, 79.0],
    ] {
        assert!(
            !rect_covers_surface(rect, surface),
            "a wallpaper missing any surface edge is not an opaque base: {rect:?}"
        );
    }
}
