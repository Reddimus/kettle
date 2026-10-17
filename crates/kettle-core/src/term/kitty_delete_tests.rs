use std::collections::HashSet;

use kettle_vt::kitty::{Delete, DeleteTarget};

use super::{
    KittyDeleteGeometry, Placement, kitty_delete_freed_ids, kitty_delete_matches_placement,
    kitty_delete_matches_virtual,
};
use crate::ImageData;

fn delete(target: DeleteTarget, free_data: bool) -> Delete {
    Delete {
        target,
        free_data,
        free_candidates: Vec::new(),
    }
}

fn placement() -> Placement {
    Placement {
        abs_line: 101,
        col: 3,
        cell_cols: 4,
        cell_rows: 3,
        x_offset_cells: 0.0,
        y_offset_cells: 0.0,
        display_cols: 4.0,
        display_rows: 3.0,
        img: ImageData::new(1, 1, vec![1, 2, 3, 255]).expect("pixel"),
        source_rect: None,
        source_crop: None,
        id: Some(10),
        placement_id: 7,
        kitty_params: None,
        z: -2,
    }
}

fn geometry() -> KittyDeleteGeometry {
    KittyDeleteGeometry {
        screen_top: 100,
        screen_lines: 4,
        cursor_abs_line: 102,
        cursor_col: 4,
    }
}

#[test]
fn all_kitty_delete_selectors_match_exact_placement_geometry() {
    let placement = placement();
    let geometry = geometry();
    let matching = [
        DeleteTarget::Visible,
        DeleteTarget::Image {
            id: 10,
            placement_id: None,
        },
        DeleteTarget::Image {
            id: 10,
            placement_id: Some(7),
        },
        DeleteTarget::Cursor,
        DeleteTarget::Cell { x: 4, y: 2 },
        DeleteTarget::CellAtZ { x: 4, y: 2, z: -2 },
        DeleteTarget::IdRange { first: 9, last: 11 },
        DeleteTarget::Column { x: 7 },
        DeleteTarget::Row { y: 3 },
        DeleteTarget::ZIndex { z: -2 },
    ];
    for target in matching {
        assert!(
            kitty_delete_matches_placement(&delete(target.clone(), false), &placement, geometry),
            "{target:?} should match"
        );
    }

    let nonmatching = [
        DeleteTarget::Image {
            id: 11,
            placement_id: None,
        },
        DeleteTarget::Image {
            id: 10,
            placement_id: Some(8),
        },
        DeleteTarget::Cell { x: 2, y: 2 },
        DeleteTarget::Cell { x: 4, y: 0 },
        DeleteTarget::CellAtZ { x: 4, y: 2, z: 0 },
        DeleteTarget::IdRange {
            first: 11,
            last: 20,
        },
        DeleteTarget::Column { x: 8 },
        DeleteTarget::Row { y: 5 },
        DeleteTarget::ZIndex { z: 2 },
    ];
    for target in nonmatching {
        assert!(
            !kitty_delete_matches_placement(&delete(target.clone(), false), &placement, geometry),
            "{target:?} should not match"
        );
    }
}

#[test]
fn visible_delete_keeps_scrollback_and_kitty_never_deletes_other_protocols() {
    let mut above = placement();
    above.abs_line = 96;
    above.cell_rows = 4; // bottom is exactly the active-screen origin.
    assert!(!kitty_delete_matches_placement(
        &delete(DeleteTarget::Visible, false),
        &above,
        geometry()
    ));

    let mut below = placement();
    below.abs_line = 104; // starts exactly below the active screen.
    assert!(!kitty_delete_matches_placement(
        &delete(DeleteTarget::Visible, false),
        &below,
        geometry()
    ));
    below.abs_line = 103; // one visible row, remainder below the screen.
    assert!(kitty_delete_matches_placement(
        &delete(DeleteTarget::Visible, false),
        &below,
        geometry()
    ));

    let mut sixel = placement();
    sixel.id = None;
    assert!(!kitty_delete_matches_placement(
        &delete(DeleteTarget::ZIndex { z: -2 }, false),
        &sixel,
        geometry()
    ));
}

#[test]
fn virtual_placements_only_match_id_and_range_selectors() {
    assert!(kitty_delete_matches_virtual(
        &delete(
            DeleteTarget::Image {
                id: 10,
                placement_id: Some(7)
            },
            false
        ),
        10,
        7
    ));
    assert!(kitty_delete_matches_virtual(
        &delete(DeleteTarget::IdRange { first: 9, last: 11 }, false),
        10,
        7
    ));
    for target in [
        DeleteTarget::Visible,
        DeleteTarget::Cursor,
        DeleteTarget::Cell { x: 1, y: 1 },
        DeleteTarget::CellAtZ { x: 1, y: 1, z: 0 },
        DeleteTarget::Column { x: 1 },
        DeleteTarget::Row { y: 1 },
        DeleteTarget::ZIndex { z: 0 },
    ] {
        assert!(
            !kitty_delete_matches_virtual(&delete(target, false), 10, 7),
            "spatial selectors must not delete virtual prototypes"
        );
    }
}

#[test]
fn uppercase_frees_only_unreferenced_candidates() {
    let candidates = HashSet::from([1, 2]);
    let referenced = HashSet::from([2]);
    assert!(
        kitty_delete_freed_ids(
            &delete(DeleteTarget::Visible, false),
            &candidates,
            &referenced
        )
        .is_empty()
    );
    assert_eq!(
        kitty_delete_freed_ids(
            &delete(DeleteTarget::Visible, true),
            &candidates,
            &referenced
        ),
        vec![1]
    );
}
