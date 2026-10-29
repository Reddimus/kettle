use super::{Config, PaneSnapshot, PaneView, text_layout_damage_key};

#[test]
fn inactive_color_glyph_dimming_invalidates_cached_instances() {
    let mut cfg = Config::default();
    let snap = PaneSnapshot::default();
    let mut panes = [
        super::headless_tests::pane(&snap, 200, 160),
        super::headless_tests::pane(&snap, 200, 160),
    ];
    panes[1].id = 2;
    panes[1].focused = false;
    let key = |panes: &[PaneView<'_>], cfg: &Config| {
        text_layout_damage_key(panes, cfg, (400.0, 160.0), (8.0, 16.0), 0.0)
    };
    let active = key(&panes, &cfg);
    panes[0].focused = false;
    panes[1].focused = true;
    let inactive = key(&panes, &cfg);
    assert_ne!(
        active, inactive,
        "focus changes color glyph dimming even if text colors hit the contrast floor"
    );
    cfg.unfocused_split_opacity = 0.1;
    assert_ne!(
        inactive,
        key(&panes, &cfg),
        "opacity changes must refresh color glyph instances"
    );
}

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

#[test]
fn ime_preedit_layer_covers_terminal_text_before_cursor() {
    let src = super::production_source();
    let scene = src
        .split("fn encode_scene_pass(")
        .nth(1)
        .unwrap()
        .split("fn create_screenshot_target(")
        .next()
        .unwrap();
    let terminal = scene.find("self.text_renderer").unwrap();
    let background = scene.find("self.ime_quads.draw(&mut pass)");
    assert!(
        background.is_some_and(|index| index > terminal),
        "IME background must be drawn after terminal glyphs so existing command text cannot bleed through"
    );
    let background = background.unwrap();
    let preedit = scene
        .find("self.ime_text_renderer")
        .expect("separate preedit text layer");
    let cursor = scene.find("self.cursor_glyph_renderer").unwrap();
    assert!(background < preedit && preedit < cursor);
}

#[test]
fn ime_preedit_background_is_opaque_and_clipped_to_its_pane() {
    use super::*;
    let mut quads = Vec::new();
    push_ime_preedit_quads(
        &mut quads,
        ime_preedit_rect(
            &ImePreedit {
                text: "composition".into(),
                row: 1,
                col: 3,
            },
            (10.0, 20.0),
            (8.0, 16.0),
            (0.0, 16.0),
        ),
        (10.0, 20.0, 50.0, 30.0),
        Rgb::new(10, 20, 30),
        Rgb::new(40, 50, 60),
    );
    assert_eq!(quads[0].pos, [34.0, 36.0]);
    assert_eq!(quads[0].size, [26.0, 14.0]);
    for quad in quads {
        assert_eq!(quad.color[3], 1.0);
        assert!(quad.pos[0] >= 10.0 && quad.pos[1] >= 20.0);
        assert!(quad.pos[0] + quad.size[0] <= 60.0);
        assert!(quad.pos[1] + quad.size[1] <= 50.0);
    }
}

#[test]
fn ime_preedit_uses_a_caret_instead_of_an_opaque_block() {
    use alacritty_terminal::vte::ansi::CursorShape;
    for shape in [
        CursorShape::Block,
        CursorShape::HollowBlock,
        CursorShape::Underline,
        CursorShape::Beam,
    ] {
        assert_eq!(super::ime_cursor_shape(shape), CursorShape::Beam);
    }
    assert_eq!(
        super::ime_cursor_shape(CursorShape::Hidden),
        CursorShape::Hidden
    );
}

#[test]
fn ime_preedit_background_covers_naturally_shaped_text_in_narrow_cells() {
    use super::*;
    let mut fonts = FontSystem::new();
    for face in kettle_config::font::all() {
        load_bundled_font(&mut fonts, face);
    }
    let mut buffer = TextBuffer::new(&mut fonts, Metrics::new(16.0, 20.0));
    buffer.set_wrap(Wrap::None);
    buffer.set_size(Some(20.0), Some(20.0));
    buffer.set_text(
        "abcd",
        &Attrs::new().family(Family::Name(kettle_config::font::FAMILY)),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(&mut fonts, false);
    let shaped_width = buffer.layout_runs().next().unwrap().line_w;
    assert!(
        shaped_width > 20.0,
        "fixture must exceed four narrowed cells"
    );
    let preedit = ImePreedit {
        text: "abcd".into(),
        row: 0,
        col: 0,
    };
    let bounds = ime_preedit_rect(&preedit, (8.0, 8.0), (5.0, 20.0), (shaped_width, 20.0));
    let mut quads = Vec::new();
    let clip = push_ime_preedit_quads(
        &mut quads,
        bounds,
        (0.0, 0.0, 320.0, 120.0),
        Rgb::new(10, 20, 30),
        Rgb::new(40, 50, 60),
    );
    assert!(
        quads[0].size[0] >= shaped_width,
        "opaque background {} must cover shaped width {shaped_width}",
        quads[0].size[0]
    );
    assert!(
        quads[1].size[0] >= shaped_width,
        "underline must follow the composition extent"
    );
    assert_eq!(clip.left, 8);
    assert_eq!(clip.right as f32, bounds.0 + bounds.2);
    assert_eq!(
        ime_preedit_rect(&preedit, (8.0, 8.0), (20.0, 20.0), (shaped_width, 20.0)).2,
        80.0
    );
}

#[test]
fn ime_preedit_reduced_height_covers_the_shaped_line() {
    use super::*;
    let metrics = Metrics::new(16.0, 20.0);
    let mut fonts = FontSystem::new();
    for face in kettle_config::font::all() {
        load_bundled_font(&mut fonts, face);
    }
    let mut buffer = TextBuffer::new(&mut fonts, metrics);
    buffer.set_wrap(Wrap::None);
    buffer.set_size(None, Some(10.0));
    buffer.set_text(
        "gyjp",
        &Attrs::new().family(Family::Name(kettle_config::font::FAMILY)),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(&mut fonts, false);
    let run = buffer.layout_runs().next().unwrap();
    assert_eq!(run.line_height, metrics.line_height);
    let preedit = ImePreedit {
        text: "gyjp".into(),
        row: 2,
        col: 1,
    };
    let bounds = ime_preedit_rect(
        &preedit,
        (8.0, 8.0),
        (8.0, 10.0),
        (run.line_w, run.line_height),
    );
    assert_eq!(
        bounds.1, 28.0,
        "composition origin still follows the reduced grid"
    );
    let mut quads = Vec::new();
    let clip = push_ime_preedit_quads(
        &mut quads,
        bounds,
        (0.0, 0.0, 320.0, 120.0),
        Rgb::new(10, 20, 30),
        Rgb::new(40, 50, 60),
    );
    assert!(
        quads[0].size[1] >= run.line_height,
        "background must cover the shaped line"
    );
    assert!(
        clip.bottom as f32 >= bounds.1 + run.line_height,
        "descenders must remain inside the clip"
    );
    assert_eq!(quads[1].pos[1], bounds.1 + run.line_height - 2.0);
}

#[test]
fn ime_preedit_text_clip_stays_inside_opaque_pixels_and_pane() {
    use super::*;
    let preedit = ImePreedit {
        text: "abcd".into(),
        row: 0,
        col: 0,
    };
    let bounds = ime_preedit_rect(&preedit, (12.25, 23.75), (5.0, 20.0), (38.4, 20.0));
    let mut quads = Vec::new();
    let clip = push_ime_preedit_quads(
        &mut quads,
        bounds,
        (10.0, 20.0, 35.0, 30.0),
        Rgb::new(10, 20, 30),
        Rgb::new(40, 50, 60),
    );
    assert_eq!(
        clip,
        TextBounds {
            left: 13,
            top: 24,
            right: 45,
            bottom: 43
        }
    );
    assert_eq!(quads[0].pos, [12.25, 23.75]);
    assert_eq!(quads[0].size, [32.75, 20.0]);
}
