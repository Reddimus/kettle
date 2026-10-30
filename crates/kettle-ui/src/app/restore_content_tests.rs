use super::*;

#[test]
fn restored_auto_tabs_reserve_chrome_before_spawning() {
    let surface = (800, 600);
    let empty = ContentBands::default();
    let restored = empty.for_restore(TabBarMode::Auto, 2, 16.0);
    let area = content_rect_for_with_strip(
        surface,
        restored,
        TabBarPos::Top,
        kettle_config::StatusBarMode::Off,
        200.0,
    );
    assert_eq!(area, (0.0, 24.0, 800.0, 576.0));
    assert_eq!(
        grid_dims_px((area.2, area.3), (8.0, 16.0), (0.0, 0.0)),
        (100, 36)
    );
    assert_eq!(empty.for_restore(TabBarMode::Auto, 1, 16.0).tab_bar_h, 0.0);
    assert_eq!(empty.for_restore(TabBarMode::Off, 2, 16.0).tab_bar_h, 0.0);
}
