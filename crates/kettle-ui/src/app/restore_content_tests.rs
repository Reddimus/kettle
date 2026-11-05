use super::*;

#[test]
fn restored_auto_tabs_reserve_chrome_before_spawning() {
    let surface = (800, 600);
    let empty = ContentBands::default();
    let restored = empty.for_restore(TabBarMode::Auto, 2, 16.0, TabBarPos::Top);
    let area = content_rect_for_with_strip(
        surface,
        restored,
        TabBarPos::Top,
        kettle_config::StatusBarMode::Off,
        200.0,
    );
    assert_eq!(area, (0.0, 28.0, 800.0, 572.0));
    assert_eq!(
        grid_dims_px((area.2, area.3), (8.0, 16.0), (0.0, 0.0)),
        (100, 35)
    );
    assert_eq!(
        empty
            .for_restore(TabBarMode::Auto, 1, 16.0, TabBarPos::Top)
            .tab_bar_h,
        0.0
    );
    assert_eq!(
        empty
            .for_restore(TabBarMode::Off, 2, 16.0, TabBarPos::Top)
            .tab_bar_h,
        0.0
    );
}

#[test]
fn hidden_side_tabs_leave_the_full_terminal_width() {
    for position in [TabBarPos::Left, TabBarPos::Right] {
        for bands in [
            ContentBands::default(),
            ContentBands::default().for_restore(TabBarMode::Auto, 1, 16.0, TabBarPos::Top),
        ] {
            assert_eq!(
                content_rect_for_with_strip(
                    (800, 600),
                    bands,
                    position,
                    kettle_config::StatusBarMode::Off,
                    180.0,
                ),
                (0.0, 0.0, 800.0, 600.0),
                "hidden side tabs must not reserve a strip"
            );
        }
    }
}
