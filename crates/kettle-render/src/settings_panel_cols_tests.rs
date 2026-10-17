use super::{SettingsOverlay, settings_display_lines, settings_panel_cols};

#[test]
fn contextual_notes_do_not_move_the_panel() {
    let mut set = SettingsOverlay {
        categories: vec!["Appearance".into(), "Graphics".into()],
        label_cols: 31,
        value_cols: 40,
        active_category: 0,
        background_page: false,
        rows: vec![],
        focused_row: 0,
        title: "Settings — Appearance".into(),
        hints: "↑↓ field    ←→ change    Tab category    Esc close".into(),
        footer_note: None,
    };
    let cols = settings_panel_cols(&set, &settings_display_lines(&set));
    assert_eq!(cols as usize, 75);
    set.active_category = 1;
    set.footer_note = Some("A very long active GPU note that must fit without moving the value column across category switches".into());
    assert_eq!(
        settings_panel_cols(&set, &settings_display_lines(&set)),
        cols
    );
}
