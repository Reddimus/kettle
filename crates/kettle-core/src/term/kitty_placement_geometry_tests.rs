use super::{
    ImageSourceCrop, ImageSourceRect, Placement, PlacementParams, PtyGeometry,
    kitty_cursor_movement, recompute_kitty_placements, resolve_kitty_placement,
};
use crate::ImageData;

fn image(width: u32, height: u32) -> ImageData {
    ImageData::new(width, height, vec![0; width as usize * height as usize * 4])
        .expect("test image")
}

fn close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
}

#[test]
fn crop_is_intersected_with_the_source_before_auto_sizing() {
    let resolved = resolve_kitty_placement(
        &image(100, 50),
        PlacementParams {
            source_x: 80,
            source_y: 10,
            source_width: 50,
            ..PlacementParams::default()
        },
        PtyGeometry::new(10, 5, 100, 50),
    )
    .expect("visible crop");
    assert_eq!(
        resolved.source_rect,
        Some(ImageSourceRect {
            x: 80.0,
            y: 10.0,
            width: 20.0,
            height: 40.0,
        })
    );
    assert_eq!((resolved.cell_cols, resolved.cell_rows), (2, 4));
    close(resolved.display_cols, 2.0);
    close(resolved.display_rows, 4.0);

    assert!(
        resolve_kitty_placement(
            &image(10, 10),
            PlacementParams {
                source_x: 10,
                ..PlacementParams::default()
            },
            PtyGeometry::new(10, 10, 100, 100),
        )
        .is_none(),
        "an empty source intersection must not create or move a placement"
    );
}

#[test]
fn explicit_size_ends_at_cell_boundaries_after_offsets() {
    let resolved = resolve_kitty_placement(
        &image(20, 10),
        PlacementParams {
            columns: 3,
            rows: 2,
            cell_x_offset: 3,
            cell_y_offset: 4,
            ..PlacementParams::default()
        },
        PtyGeometry::new(10, 5, 100, 50),
    )
    .expect("placement");
    assert_eq!((resolved.cell_cols, resolved.cell_rows), (3, 2));
    close(resolved.x_offset_cells, 0.3);
    close(resolved.y_offset_cells, 0.4);
    close(resolved.display_cols, 2.7);
    close(resolved.display_rows, 1.6);
}

#[test]
fn one_axis_auto_uses_aspect_ratio_and_keeps_effective_bounds_distinct() {
    let rows_only = resolve_kitty_placement(
        &image(20, 10),
        PlacementParams {
            rows: 2,
            cell_y_offset: 3,
            ..PlacementParams::default()
        },
        PtyGeometry::new(10, 10, 100, 100),
    )
    .expect("rows-only placement");
    assert_eq!((rows_only.cell_cols, rows_only.cell_rows), (5, 2));
    close(rows_only.display_cols, 3.4);
    close(rows_only.display_rows, 1.7);

    let columns_only = resolve_kitty_placement(
        &image(20, 10),
        PlacementParams {
            columns: 2,
            cell_x_offset: 3,
            ..PlacementParams::default()
        },
        PtyGeometry::new(10, 10, 100, 100),
    )
    .expect("columns-only placement");
    assert_eq!((columns_only.cell_cols, columns_only.cell_rows), (2, 2));
    close(columns_only.display_cols, 1.7);
    close(columns_only.display_rows, 0.85);
}

#[test]
fn natural_size_recomputes_after_a_monitor_pixel_geometry_change() {
    let img = image(96, 40);
    let params = PlacementParams::default();
    let first = resolve_kitty_placement(&img, params, PtyGeometry::new(100, 40, 960, 800)).unwrap();
    let second =
        resolve_kitty_placement(&img, params, PtyGeometry::new(100, 40, 1200, 800)).unwrap();
    assert_eq!((first.cell_cols, first.cell_rows), (10, 2));
    assert_eq!((second.cell_cols, second.cell_rows), (8, 2));
    close(first.display_cols, 10.0);
    close(second.display_cols, 8.0);

    let mut placements = vec![Placement {
        abs_line: 2,
        col: 3,
        cell_cols: first.cell_cols,
        cell_rows: first.cell_rows,
        x_offset_cells: first.x_offset_cells,
        y_offset_cells: first.y_offset_cells,
        display_cols: first.display_cols,
        display_rows: first.display_rows,
        img,
        source_rect: first.source_rect,
        source_crop: None,
        id: Some(7),
        placement_id: 9,
        kitty_params: Some(params),
        z: 0,
    }];
    recompute_kitty_placements(&mut placements, PtyGeometry::new(100, 40, 1200, 800));
    assert_eq!(
        (placements[0].cell_cols, placements[0].cell_rows),
        (8, 2),
        "stored raw Kitty parameters must be re-resolved on a monitor/DPI change"
    );
    close(placements[0].display_cols, 8.0);
}

#[test]
fn cropped_natural_size_recomputes_without_restoring_discarded_source_rows() {
    let img = image(96, 40);
    let params = PlacementParams::default();
    let initial =
        resolve_kitty_placement(&img, params, PtyGeometry::new(100, 40, 960, 800)).unwrap();
    let crop = ImageSourceCrop {
        top: 0.5,
        bottom: 1.0,
    };
    let mut placements = vec![Placement {
        abs_line: 77,
        col: 4,
        cell_cols: initial.cell_cols,
        cell_rows: 1,
        x_offset_cells: initial.x_offset_cells,
        y_offset_cells: 0.0,
        display_cols: initial.display_cols,
        display_rows: initial.display_rows * (crop.bottom - crop.top),
        img,
        source_rect: initial.source_rect,
        source_crop: Some(crop),
        id: Some(7),
        placement_id: 9,
        kitty_params: Some(params),
        z: 0,
    }];

    recompute_kitty_placements(&mut placements, PtyGeometry::new(100, 40, 1200, 1000));

    let retained = &placements[0];
    assert_eq!((retained.abs_line, retained.col), (77, 4));
    assert_eq!(retained.source_crop, Some(crop));
    assert_eq!(retained.kitty_params, Some(params));
    assert_eq!((retained.cell_cols, retained.cell_rows), (8, 1));
    close(retained.display_cols, 8.0);
    close(retained.display_rows, 0.8);
}

#[test]
fn cropped_one_axis_auto_recomputes_width_but_preserves_scrolled_y_offset() {
    let img = image(120, 100);
    let params = PlacementParams {
        source_x: 10,
        source_y: 20,
        source_width: 80,
        source_height: 60,
        columns: 4,
        cell_x_offset: 3,
        cell_y_offset: 4,
        ..PlacementParams::default()
    };
    let initial =
        resolve_kitty_placement(&img, params, PtyGeometry::new(100, 40, 1000, 800)).unwrap();
    let crop = ImageSourceCrop {
        top: 0.2,
        bottom: 0.6,
    };
    let preserved_y_offset = 0.125;
    let mut placements = vec![Placement {
        abs_line: 121,
        col: 6,
        cell_cols: initial.cell_cols,
        cell_rows: 1,
        x_offset_cells: initial.x_offset_cells,
        y_offset_cells: preserved_y_offset,
        display_cols: initial.display_cols,
        display_rows: initial.display_rows * (crop.bottom - crop.top),
        img,
        source_rect: initial.source_rect,
        source_crop: Some(crop),
        id: Some(8),
        placement_id: 10,
        kitty_params: Some(params),
        z: 0,
    }];

    recompute_kitty_placements(&mut placements, PtyGeometry::new(100, 40, 1200, 1000));

    let retained = &placements[0];
    assert_eq!((retained.abs_line, retained.col), (121, 6));
    assert_eq!(
        retained.source_rect,
        Some(ImageSourceRect {
            x: 10.0,
            y: 20.0,
            width: 80.0,
            height: 60.0,
        })
    );
    assert_eq!(retained.source_crop, Some(crop));
    assert_eq!(retained.kitty_params, Some(params));
    assert_eq!((retained.cell_cols, retained.cell_rows), (4, 1));
    close(retained.x_offset_cells, 0.25);
    close(retained.y_offset_cells, preserved_y_offset);
    close(retained.display_cols, 3.75);
    close(retained.display_rows, 0.54);
}

#[test]
fn offsets_are_bounded_and_cursor_policy_uses_effective_cells() {
    let resolved = resolve_kitty_placement(
        &image(96, 20),
        PlacementParams {
            cell_x_offset: u32::MAX,
            ..PlacementParams::default()
        },
        PtyGeometry::new(100, 10, 960, 200),
    )
    .expect("bounded placement");
    // 9.6 px cell: the largest integral in-cell offset is 9 px.
    close(resolved.x_offset_cells, 9.0 / 9.6);
    assert_eq!(resolved.cell_cols, 11);

    assert_eq!(
        kitty_cursor_movement(PlacementParams::default(), 5, 3).as_deref(),
        Some("\x1b[5C\x1b[2B")
    );
    assert_eq!(
        kitty_cursor_movement(PlacementParams::default(), 5, 1).as_deref(),
        Some("\x1b[5C")
    );
    assert_eq!(
        kitty_cursor_movement(
            PlacementParams {
                suppress_cursor_movement: true,
                ..PlacementParams::default()
            },
            5,
            3,
        ),
        None
    );
}
