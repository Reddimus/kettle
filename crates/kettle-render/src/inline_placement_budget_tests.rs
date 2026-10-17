use super::{
    PaneSnapshot, fair_placement_quotas, inline_image_clip, inline_placement_rect,
    pane_backdrop_rect, pane_grid_origin, placement_is_visible, placement_viewport_row,
};
use kettle_core::{ImageData, Placement};

fn placement_at(abs_line: u64) -> Placement {
    Placement {
        abs_line,
        col: 0,
        cell_cols: 1,
        cell_rows: 1,
        x_offset_cells: 0.0,
        y_offset_cells: 0.0,
        display_cols: 1.0,
        display_rows: 1.0,
        img: ImageData::new(1, 1, vec![0, 0, 0, 0]).expect("pixel"),
        source_rect: None,
        source_crop: None,
        id: Some(1),
        placement_id: 0,
        kitty_params: None,
        z: 0,
    }
}

#[test]
fn busy_early_pane_cannot_starve_later_panes() {
    assert_eq!(fair_placement_quotas(&[256, 1], 256), vec![255, 1]);
    assert_eq!(
        fair_placement_quotas(&[256, 256, 256, 256], 256),
        vec![64, 64, 64, 64]
    );
    assert_eq!(fair_placement_quotas(&[0, 3, 100], 4), vec![0, 2, 2]);
}

#[test]
fn quotas_are_bounded_complete_and_deterministic() {
    for (counts, limit) in [
        (vec![], 10),
        (vec![0, 0], 10),
        (vec![1, 2, 3], 0),
        (vec![1, 2, 3], 99),
        (vec![5, 1, 8, 2], 9),
    ] {
        let first = fair_placement_quotas(&counts, limit);
        assert_eq!(first, fair_placement_quotas(&counts, limit));
        assert!(
            first
                .iter()
                .zip(&counts)
                .all(|(quota, count)| quota <= count)
        );
        assert_eq!(
            first.iter().sum::<usize>(),
            counts.iter().sum::<usize>().min(limit)
        );
    }
}

#[test]
fn kitty_offsets_and_fractional_extent_reach_the_gpu_destination() {
    let placement = Placement {
        abs_line: 12,
        col: 3,
        cell_cols: 3,
        cell_rows: 2,
        x_offset_cells: 0.25,
        y_offset_cells: 0.5,
        display_cols: 2.75,
        display_rows: 1.5,
        img: ImageData::new(1, 1, vec![0, 0, 0, 0]).expect("pixel"),
        source_rect: None,
        source_crop: None,
        id: Some(1),
        placement_id: 2,
        kitty_params: None,
        z: 0,
    };
    assert_eq!(
        inline_placement_rect(100.0, 200.0, 2, 10.0, 20.0, &placement),
        (132.5, 250.0, 27.5, 30.0)
    );
}

#[test]
fn monotonic_history_origin_prevents_capped_scrollback_aliasing() {
    let mut snap = PaneSnapshot {
        history_origin: 100,
        history_size: 3,
        display_offset: 0,
        screen_lines: 3,
        ..PaneSnapshot::default()
    };
    let current = placement_at(103);
    assert!(placement_is_visible(&snap, &current));
    assert_eq!(placement_viewport_row(&snap, &current), Some(0));

    let stale_legacy_coordinate = placement_at(3);
    assert!(!placement_is_visible(&snap, &stale_legacy_coordinate));
    assert_eq!(
        placement_viewport_row(&snap, &stale_legacy_coordinate),
        Some(-100)
    );

    snap.display_offset = 2;
    let retained_history = placement_at(101);
    assert!(placement_is_visible(&snap, &retained_history));
    assert_eq!(placement_viewport_row(&snap, &retained_history), Some(0));
}

#[test]
fn empty_viewport_never_admits_an_image_placement() {
    let snap = PaneSnapshot {
        history_origin: 100,
        history_size: 3,
        display_offset: 0,
        screen_lines: 0,
        ..PaneSnapshot::default()
    };
    let mut spanning = placement_at(100);
    spanning.cell_rows = 10;
    assert!(!placement_is_visible(&snap, &spanning));
}

#[test]
fn inline_image_clip_excludes_padding_titlebar_and_pane_edges() {
    // A 200x120 pane at (10, 30) with a 20px titlebar, which the layout
    // has already carved off the terminal: top, then bottom.
    let below_title = (10.0, 50.0, 200.0, 100.0);
    let above_title = (10.0, 30.0, 200.0, 100.0);
    let padding = (8.0, 8.0);
    // Pane interior after a 1px border and 20px top titlebar.
    let pane_body = pane_backdrop_rect(below_title, 1.0).expect("pane body");
    let top_origin = pane_grid_origin(below_title, padding);
    // The grid starts another 8px inside the pane and is wider/taller than
    // the remaining body. The intersection must start at the grid (so no
    // padding/titlebar paint) and end at the pane body (so no sibling/chrome
    // bleed).
    assert_eq!(
        inline_image_clip(pane_body, top_origin, (30, 10), (8.0, 16.0)),
        Some([18.0, 58.0, 191.0, 91.0])
    );
    assert_eq!(
        inline_image_clip(pane_body, top_origin, (30, 0), (8.0, 16.0)),
        None
    );

    let bottom_title_body = pane_backdrop_rect(above_title, 1.0).expect("bottom-title pane body");
    let bottom_origin = pane_grid_origin(above_title, padding);
    assert_eq!(
        inline_image_clip(bottom_title_body, bottom_origin, (30, 10), (8.0, 16.0)),
        Some([18.0, 38.0, 191.0, 91.0]),
        "moving the titlebar must translate the grid and clip together, \
             not shrink the drawable image viewport"
    );
}
