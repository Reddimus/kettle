use super::{
    NEW_TAB_MENU_GLYPH, NEW_TAB_PLUS_GLYPH, OUTLINE_BOTTOM_LEFT, OUTLINE_BOTTOM_RIGHT,
    centered_text_origin, pane_bottom_window_corner_mask, text_bounds_for_rect,
};

#[test]
fn new_tab_glyphs_are_unpadded_and_centered_in_their_own_hit_rects() {
    assert_eq!(NEW_TAB_MENU_GLYPH, NEW_TAB_MENU_GLYPH.trim());
    assert_eq!(NEW_TAB_PLUS_GLYPH, NEW_TAB_PLUS_GLYPH.trim());
    assert_eq!(
        centered_text_origin((10.0, 20.0, 26.0, 26.0), (8.0, 18.0)),
        (19.0, 24.0)
    );
    assert_eq!(
        centered_text_origin((10.0, 20.0, 12.0, 10.0), (20.0, 20.0)),
        (10.0, 20.0)
    );
    let lower_row_clip = text_bounds_for_rect((10.0, 72.0, 26.0, 26.0));
    assert_eq!(lower_row_clip.left, 10);
    assert_eq!(lower_row_clip.top, 72);
    assert_eq!(lower_row_clip.right, 36);
    assert_eq!(lower_row_clip.bottom, 98);
}

#[test]
fn only_panes_touching_a_rounded_window_bottom_receive_corner_masks() {
    let surface = (1200.0, 800.0);
    assert_eq!(
        pane_bottom_window_corner_mask((0.0, 100.0, 1200.0, 700.0), surface, true),
        OUTLINE_BOTTOM_LEFT | OUTLINE_BOTTOM_RIGHT
    );
    assert_eq!(
        pane_bottom_window_corner_mask((0.0, 100.0, 600.0, 700.0), surface, true),
        OUTLINE_BOTTOM_LEFT
    );
    assert_eq!(
        pane_bottom_window_corner_mask((600.0, 100.0, 600.0, 700.0), surface, true),
        OUTLINE_BOTTOM_RIGHT
    );
    assert_eq!(
        pane_bottom_window_corner_mask((0.0, 100.0, 1200.0, 350.0), surface, true),
        0,
        "an internal split edge must stay square"
    );
    assert_eq!(
        pane_bottom_window_corner_mask((0.0, 100.0, 1200.0, 700.0), surface, false),
        0,
        "borderless, fullscreen, Linux and Windows windows stay unchanged"
    );
}

#[test]
fn fractional_layout_rounding_still_matches_the_surface_edge() {
    assert_eq!(
        pane_bottom_window_corner_mask((0.25, 64.0, 799.5, 535.25), (800.0, 600.0), true,),
        OUTLINE_BOTTOM_LEFT | OUTLINE_BOTTOM_RIGHT
    );
}
