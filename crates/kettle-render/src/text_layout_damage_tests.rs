use super::{Config, PaneSnapshot, PaneView, text_layout_damage_key};

/// A pane's rect can move while the window size, cell size, and every row's
/// text stay identical — bottom chrome or a banner appearing, for instance.
/// The layout damage key must still change, or the cached cell-locked glyph
/// instances keep drawing at the old positions, which is the "leftover
/// text" symptom. Cursor blink must NOT change it; that direction is pinned
/// by `grid_upload_damage_excludes_cursor_blink`.
#[test]
fn pane_rect_shift_invalidates_layout_key() {
    let cfg = Config::default();
    let snap = PaneSnapshot::default();
    let pane = |rect: (f32, f32, f32, f32)| PaneView {
        id: 1,
        rect,
        terminal: rect,
        titlebar: None,
        snap: &snap,
        inline_cards: None,
        tr: kettle_i18n::Translator::default(),
        focused: true,
        images: &[],
        title: "",
        title_prefix: "",
        title_path: None,
        size_cols: 80,
        size_rows: 24,
        bell: false,
        bell_flash: 0.0,
        group_name: None,
    };
    let surface = (800.0, 600.0);
    let cell = (8.0, 16.0);

    let full = text_layout_damage_key(&[pane((0.0, 0.0, 800.0, 600.0))], &cfg, surface, cell, 0.0);
    let shortened =
        text_layout_damage_key(&[pane((0.0, 0.0, 800.0, 560.0))], &cfg, surface, cell, 0.0);
    assert_ne!(
        full, shortened,
        "a pane losing height must invalidate the text layout damage key"
    );

    let moved =
        text_layout_damage_key(&[pane((0.0, 40.0, 800.0, 560.0))], &cfg, surface, cell, 0.0);
    assert_ne!(
        shortened, moved,
        "a pane of unchanged size at a new origin must invalidate the key too"
    );

    // A preview lane opening shrinks the terminal inside an unmoved pane.
    let leaf = (0.0, 0.0, 800.0, 600.0);
    let mut carved = pane(leaf);
    carved.terminal = (0.0, 0.0, 800.0, 360.0);
    assert_ne!(
        full,
        text_layout_damage_key(&[carved], &cfg, surface, cell, 0.0),
        "a terminal carved inside an unmoved pane must invalidate the key"
    );
}
