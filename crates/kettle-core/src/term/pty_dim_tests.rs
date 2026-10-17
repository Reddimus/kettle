use super::{
    NATIVE_PTY_GRID_MAX, PtyGeometry, clamp_native_pty_grid, clamp_pty_dim, image_cells_for_pixels,
    native_pty_size, native_resize_required,
};

#[test]
fn ordinary_sizes_pass_through() {
    // A typical 4K-wide grid: 8px cells × 480 cols = 3840px, well
    // within u16. The row/col count case uses cell = 1.
    assert_eq!(clamp_pty_dim(1, 200), 200);
    assert_eq!(clamp_pty_dim(8, 480), 3840);
    assert_eq!(clamp_pty_dim(20, 100), 2000); // HiDPI cell
}

#[test]
fn overflowing_product_saturates_instead_of_wrapping() {
    // 30px HiDPI cell × 5000 cols = 150_000, which overflows u16. A bare
    // `cell_w * cols as u16` panics in debug and wraps to 18928 in
    // release; clamp_pty_dim saturates at u16::MAX instead.
    assert_eq!(clamp_pty_dim(30, 5000), u16::MAX);
    // Pathological count that a bare `cols as u16` would truncate.
    assert_eq!(clamp_pty_dim(1, usize::MAX), u16::MAX);
    assert_eq!(clamp_pty_dim(10, usize::MAX), u16::MAX);
}

#[test]
fn zero_inputs_are_benign() {
    assert_eq!(clamp_pty_dim(0, 80), 0);
    assert_eq!(clamp_pty_dim(8, 0), 0);
}

#[test]
fn native_grid_clamps_before_the_portable_pty_boundary() {
    assert_eq!(clamp_native_pty_grid(120), 120);
    assert_eq!(
        clamp_native_pty_grid(usize::MAX),
        NATIVE_PTY_GRID_MAX as u16
    );
    #[cfg(windows)]
    assert_eq!(
        clamp_native_pty_grid(i16::MAX as usize + 1),
        i16::MAX as u16
    );
}

#[test]
fn native_size_preserves_exact_total_pixels() {
    let size = native_pty_size(PtyGeometry::new(100, 41, 960, 787));
    assert_eq!((size.cols, size.rows), (100, 41));
    assert_eq!((size.pixel_width, size.pixel_height), (960, 787));
}

#[test]
fn failed_native_resize_remains_retryable() {
    let applied = PtyGeometry::new(80, 24, 640, 384);
    let desired = PtyGeometry::new(120, 40, 1152, 768);
    assert!(native_resize_required(applied, desired));
    // A failure deliberately leaves `applied` unchanged; asking for the
    // same desired geometry must therefore still require a native retry.
    assert!(native_resize_required(applied, desired));
}

#[cfg(windows)]
#[test]
fn conpty_skips_pixel_only_resize() {
    let applied = PtyGeometry::new(120, 40, 1152, 768);
    let desired = PtyGeometry::new(120, 40, 1153, 768);
    assert!(!native_resize_required(applied, desired));
}

#[cfg(not(windows))]
#[test]
fn unix_publishes_pixel_only_resize() {
    let applied = PtyGeometry::new(120, 40, 1152, 768);
    let desired = PtyGeometry::new(120, 40, 1153, 768);
    assert!(native_resize_required(applied, desired));
}

#[test]
fn image_placement_uses_exact_fractional_cell_geometry() {
    // 960 / 100 = 9.6 px per cell. Dividing by the rounded 10 px metric
    // would give a 960 px image only 96 cells.
    assert_eq!(image_cells_for_pixels(960, 100, 960), 100);
    assert_eq!(image_cells_for_pixels(96, 100, 960), 10);
    assert_eq!(image_cells_for_pixels(97, 100, 960), 11);
    assert_eq!(image_cells_for_pixels(0, 100, 960), 0);
    assert_eq!(image_cells_for_pixels(960, 0, 960), 0);
    assert_eq!(
        image_cells_for_pixels(u32::MAX, usize::MAX, 1),
        usize::MAX,
        "hostile image/grid products must saturate rather than wrap"
    );
}
