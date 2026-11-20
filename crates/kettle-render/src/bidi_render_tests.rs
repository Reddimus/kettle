//! Rows with right-to-left text drawn in bidi display order.
use crate::geometry::GridGlyphCell;
use crate::gpu_tests::gpu_test_guard;
use crate::headless_tests::{capture, focused, pane, renderer, snapshot_of};
use crate::*;
use glyphon::cosmic_text::{Attrs, AttrsList, BufferLine, Family, LineEnding, Shaping};

/// Each base character of row 0 with the column its glyph is drawn in, for
/// the cells that carry a grid column.
fn drawn_columns(renderer: &Renderer) -> Vec<(char, usize)> {
    let line = &renderer.pane_buffers[0].lines[0];
    line.attrs_list()
        .spans_iter()
        .filter_map(|(range, attrs)| {
            let cell = GridGlyphCell::from_metadata(attrs.metadata)?;
            line.text()
                .get(range.clone())?
                .chars()
                .next()
                .map(|c| (c, cell.column))
        })
        .collect()
}

#[test]
fn a_right_to_left_word_is_drawn_in_visual_cells() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(20, 4, "ab שלום cd".as_bytes());
    capture(
        &mut renderer,
        &cfg,
        &[pane(&snap, 320, 120)],
        &focused(false),
    );
    let columns = drawn_columns(&renderer);
    for (c, column) in [
        ('a', 0),
        ('b', 1),
        ('ש', 6),
        ('ל', 5),
        ('ו', 4),
        ('ם', 3),
        ('c', 8),
    ] {
        assert!(
            columns.contains(&(c, column)),
            "{c:?} must be drawn in column {column}: {columns:?}"
        );
    }
}

#[test]
fn arabic_joins_and_brackets_mirror_only_in_right_to_left_text() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(20, 4, "x (مرحبا) y".as_bytes());
    capture(
        &mut renderer,
        &cfg,
        &[pane(&snap, 320, 120)],
        &focused(false),
    );
    let columns = drawn_columns(&renderer);
    // The Arabic run reverses in place (columns 3..=7): alef final leftmost,
    // meem initial rightmost. Its brackets follow the Latin before them
    // (rule N0), so they stay put, unmirrored.
    for (c, column) in [
        ('x', 0),
        ('\u{FE8E}', 3),
        ('\u{FE92}', 4),
        ('\u{FEA3}', 5),
        ('\u{FEAE}', 6),
        ('\u{FEE3}', 7),
        ('y', 10),
    ] {
        assert!(
            columns.contains(&(c, column)),
            "{c:?} must be drawn in column {column}: {columns:?}"
        );
    }
    assert!(columns.contains(&(')', 8)) && columns.contains(&('(', 2)));
    assert!(
        !renderer.pane_buffers[0].lines[0]
            .text()
            .contains('\u{FFFD}'),
        "no cell falls back to the replacement character"
    );
    // After Hebrew the brackets are right to left: the closing bracket's cell
    // is drawn leftmost, mirrored to an opening one.
    let snap = snapshot_of(20, 4, "שלום (א)".as_bytes());
    capture(
        &mut renderer,
        &cfg,
        &[pane(&snap, 320, 120)],
        &focused(false),
    );
    let columns = drawn_columns(&renderer);
    for (c, column) in [('(', 0), ('א', 1), (')', 2), ('ם', 4), ('ש', 7)] {
        assert!(
            columns.contains(&(c, column)),
            "{c:?} must be drawn in column {column}: {columns:?}"
        );
    }
    // The shaper draws what the cells chose: row 0 (the buffer's first line,
    // created with it) shapes left to right, so its brackets are not
    // mirrored back. The bracket glyphs come from the pane's own font.
    let glyph_at = |renderer: &Renderer, column: usize| {
        let run = renderer.pane_buffers[0]
            .layout_runs()
            .next()
            .expect("row 0 is laid out");
        assert!(
            run.glyphs.iter().all(|glyph| !glyph.level.is_rtl()),
            "pane rows shape left to right"
        );
        run.glyphs
            .iter()
            .find(|glyph| {
                GridGlyphCell::from_metadata(glyph.metadata).map(|cell| cell.column) == Some(column)
            })
            .map(|glyph| glyph.glyph_id)
    };
    let mirrored = (glyph_at(&renderer, 0), glyph_at(&renderer, 2));
    // A Latin row whose brackets every renderer draws as written (one
    // Hebrew letter gives it a layout, so its cells carry columns too).
    let reference = snapshot_of(20, 4, "() א".as_bytes());
    capture(
        &mut renderer,
        &cfg,
        &[pane(&reference, 320, 120)],
        &focused(false),
    );
    let (open, close) = (glyph_at(&renderer, 0), glyph_at(&renderer, 1));
    assert!(open.is_some() && open != close);
    assert_eq!(
        mirrored,
        (open, close),
        "column 0 opens and column 2 closes"
    );
}

#[test]
fn a_selection_over_reordered_text_fills_its_visual_cells() {
    use alacritty_terminal::selection::SelectionRange;
    use kettle_core::{Column, Line, Point};
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let mut snap = snapshot_of(20, 4, "ab שלום\x1b[?25l".as_bytes());
    // Logical columns 3 and 4 (ש ל) are drawn in visual columns 6 and 5.
    snap.selection = Some(SelectionRange::new(
        Point::new(Line(0), Column(3)),
        Point::new(Line(0), Column(4)),
        false,
    ));
    let frame = capture(
        &mut renderer,
        &cfg,
        &[pane(&snap, 320, 120)],
        &focused(false),
    );
    let fill = super::theme::selection_fill(
        &cfg.theme,
        cfg.theme.background,
        true,
        &mut MinimumContrastCache::default(),
    );
    // Where the fill alone does not separate from the background, the
    // selection also draws a perimeter outline over its edge pixels.
    let outline = super::theme::PaneSelectionFills::new(
        &cfg.theme,
        cfg.theme.background,
        renderer.selection_backdrop_alpha,
        &mut MinimumContrastCache::default(),
    )
    .outline(&cfg.theme, true);
    let near = |pixel: &image::Rgba<u8>, color: Rgb| {
        pixel[0].abs_diff(color.r) <= 8
            && pixel[1].abs_diff(color.g) <= 8
            && pixel[2].abs_diff(color.b) <= 8
    };
    let (ox, oy) = pane_grid_origin((0.0, 0.0, 320.0, 120.0), (cfg.padding_x, cfg.padding_y));
    // The share of a cell's pixels painted by the selection (its fill or its
    // outline): glyph ink covers only part of a cell, and adapters round
    // blends differently.
    let filled = |col: usize| {
        let (left, top) = (ox + col as f32 * renderer.cell_w, oy);
        let (mut hits, mut total) = (0u32, 0u32);
        for y in top.ceil() as u32..(top + renderer.cell_h).floor() as u32 {
            for x in left.ceil() as u32..(left + renderer.cell_w).floor() as u32 {
                let pixel = frame.get_pixel(x, y);
                total += 1;
                if near(pixel, fill) || outline.is_some_and(|line| near(pixel, line)) {
                    hits += 1;
                }
            }
        }
        hits as f32 / total.max(1) as f32
    };
    for col in [5, 6] {
        assert!(
            filled(col) > 0.5,
            "visual {col} is selected: {}",
            filled(col)
        );
    }
    for col in [3, 4] {
        assert!(filled(col) < 0.1, "visual {col} is not: {}", filled(col));
    }
}

#[test]
fn the_alternate_screen_and_bidi_off_keep_logical_order() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let alternate = snapshot_of(20, 4, "\x1b[?1049hab שלום cd".as_bytes());
    assert!(alternate.alt_screen);
    capture(
        &mut renderer,
        &cfg,
        &[pane(&alternate, 320, 120)],
        &focused(false),
    );
    assert!(drawn_columns(&renderer).is_empty());
    assert!(!renderer.pane_bidi[0].any());

    let normal = snapshot_of(20, 4, "ab שלום cd".as_bytes());
    cfg.bidi = false;
    capture(
        &mut renderer,
        &cfg,
        &[pane(&normal, 320, 120)],
        &focused(false),
    );
    assert!(drawn_columns(&renderer).is_empty());
    assert!(!renderer.pane_bidi[0].any());
}

#[test]
fn the_block_cursor_draws_the_cells_joined_form() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    // The cursor on the middle beh of three: its medial form.
    let mut snap = snapshot_of(20, 4, "ببب\x1b[2D".as_bytes());
    snap.cursor.shape = alacritty_terminal::vte::ansi::CursorShape::Block;
    capture(
        &mut renderer,
        &cfg,
        &[pane(&snap, 320, 120)],
        &focused(true),
    );
    assert_eq!(renderer.cursor_glyph_buffer.lines[0].text(), "\u{FE92}");
}

/// The first glyph's id and every span's level for `text` shaped as one line.
fn shape(renderer: &mut Renderer, text: &str, ltr_override: bool) -> (u16, Vec<bool>) {
    let mut line = BufferLine::new(
        text,
        LineEnding::None,
        AttrsList::new(&Attrs::new().family(Family::Monospace)),
        Shaping::Advanced,
    );
    line.set_ltr_override(ltr_override);
    let shaped = line.shape(&mut renderer.font_system, 8);
    let first = shaped
        .spans
        .iter()
        .flat_map(|span| &span.words)
        .flat_map(|word| &word.glyphs)
        .find(|glyph| glyph.start == 0)
        .expect("the first character is shaped")
        .glyph_id;
    let rtl = shaped
        .spans
        .iter()
        .map(|span| span.level.is_rtl())
        .collect();
    (first, rtl)
}

#[test]
fn pane_rows_shape_left_to_right_without_mirroring() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, _cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let (open_bracket, _) = shape(&mut renderer, "(a", true);
    // With the override the row is one left-to-right span, and '(' keeps its
    // own glyph: Kettle has already chosen what each cell shows.
    let (glyph, rtl) = shape(&mut renderer, "(שלום)", true);
    assert_eq!(glyph, open_bracket);
    assert!(rtl.iter().all(|&rtl| !rtl), "{rtl:?}");
    // Without it the line is a right-to-left paragraph and '(' is mirrored.
    let (glyph, rtl) = shape(&mut renderer, "(שלום)", false);
    assert!(rtl.iter().any(|&rtl| rtl));
    assert_ne!(glyph, open_bracket);
}

fn arabic_renderer(family: &str) -> Option<(Renderer, Config)> {
    let Some((mut renderer, cfg)) = renderer(640, 220) else {
        eprintln!("ARABIC_GPU_SKIP: no adapter; {family} was not rendered");
        return None;
    };
    crate::arabic_join_tests::isolate_named_face(&mut renderer.font_system, family)
        .then_some((renderer, cfg))
}

fn connector_band(renderer: &mut Renderer, row: usize) -> Vec<u32> {
    let run = renderer.pane_buffers[0]
        .layout_runs()
        .find(|run| run.line_i == row)
        .unwrap();
    let glyph = run
        .glyphs
        .iter()
        .find(|glyph| glyph.w > 0.0 && glyph.glyph_id != 0)
        .unwrap();
    let mut key = glyph.physical((0.0, 0.0), 1.0).cache_key;
    key.glyph_id = renderer
        .font_system
        .get_font(key.font_id, key.font_weight)
        .unwrap()
        .as_swash()
        .charmap()
        .map('\u{640}');
    assert_ne!(key.glyph_id, 0, "named Arabic fixture must have tatweel");
    key.x_bin = glyphon::cosmic_text::SubpixelBin::Zero;
    let tatweel = renderer
        .swash
        .get_image_uncached(&mut renderer.font_system, key)
        .unwrap();
    assert_eq!(tatweel.content, glyphon::SwashContent::Mask);
    let p = tatweel.placement;
    let baseline = run.line_y.round();
    let band: Vec<_> = (0..p.height)
        .filter(|&y| tatweel.data[(y * p.width + p.width / 2) as usize] > 127)
        .map(|y| (baseline + y as f32 - p.top as f32) as u32)
        .collect();
    assert!(
        !band.is_empty(),
        "named Arabic fixture must have a connector band"
    );
    band
}

fn band_has_ink(frame: &image::RgbaImage, x: u32, band: &[u32], bg: Rgb) -> bool {
    band.iter().any(|&y| {
        let p = frame.get_pixel(x, y);
        u32::from(p[0].abs_diff(bg.r))
            + u32::from(p[1].abs_diff(bg.g))
            + u32::from(p[2].abs_diff(bg.b))
            > 120
    })
}

#[test]
fn flat_arabic_joins_are_one_unbroken_stroke() {
    let _serial = gpu_test_guard();
    for family in crate::arabic_join_tests::ARABIC_FACES {
        let Some((mut renderer, mut cfg)) = arabic_renderer(family) else {
            continue;
        };
        cfg.padding_x = 4.25;
        cfg.padding_y = 4.0;
        for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
            cfg.text_renderer = mode;
            for size in [13.0, 14.0] {
                cfg.font_size = size;
                renderer.set_font_size(size);
                for scale in [1.0, 2.0] {
                    renderer.set_scale(scale);
                    for text in ["ـببببـ", "ـــــ"] {
                        let snap = snapshot_of(30, 4, format!("{text}\x1b[?25l").as_bytes());
                        let frame = capture(
                            &mut renderer,
                            &cfg,
                            &[pane(&snap, 640, 220)],
                            &focused(false),
                        );
                        let band: Vec<_> = connector_band(&mut renderer, 0)
                            .into_iter()
                            .map(|y| y + cfg.padding_y as u32)
                            .collect();
                        let left = cfg.padding_x.floor() as u32;
                        let right = (cfg.padding_x + text.chars().count() as f32 * renderer.cell_w)
                            .ceil() as u32;
                        let ink: Vec<_> = (left..right)
                            .filter(|&x| band_has_ink(&frame, x, &band, cfg.theme.background))
                            .collect();
                        assert!(!ink.is_empty());
                        for x in ink[0]..=*ink.last().unwrap() {
                            assert!(
                                band_has_ink(&frame, x, &band, cfg.theme.background),
                                "{family} {mode:?} {size}px@{scale}x {text:?}: gap at {x}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn joined_word_edges_are_inked_and_a_nonjoining_seam_keeps_its_hole() {
    let _serial = gpu_test_guard();
    for family in crate::arabic_join_tests::ARABIC_FACES {
        let Some((mut renderer, mut cfg)) = arabic_renderer(family) else {
            continue;
        };
        for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
            cfg.text_renderer = mode;
            for scale in [1.0, 2.0] {
                renderer.set_scale(scale);
                let snap = snapshot_of(30, 4, "مرحبا\x1b[?25l".as_bytes());
                let frame = capture(
                    &mut renderer,
                    &cfg,
                    &[pane(&snap, 640, 220)],
                    &focused(false),
                );
                let band: Vec<_> = connector_band(&mut renderer, 0)
                    .into_iter()
                    .map(|y| y + cfg.padding_y as u32)
                    .collect();
                for boundary in [1.0, 2.0, 4.0] {
                    let x = (cfg.padding_x + boundary * renderer.cell_w).round() as u32;
                    assert!(
                        (x - 1..=x + 1).all(|x| band_has_ink(
                            &frame,
                            x,
                            &band,
                            cfg.theme.background
                        )),
                        "{family} {mode:?}@{scale}: joining edge {boundary}"
                    );
                }
                let seam = (cfg.padding_x + 3.0 * renderer.cell_w).round() as u32;
                assert!(
                    (seam - 3..seam + 3).any(|x| !band_has_ink(
                        &frame,
                        x,
                        &band,
                        cfg.theme.background
                    )),
                    "hah/reh must remain unjoined"
                );
            }
        }
    }
}

#[test]
fn arabic_style_and_selection_boundaries_keep_bridge_colour_inside_its_cell() {
    use alacritty_terminal::selection::SelectionRange;
    use kettle_core::{Column, Line, Point};
    let _serial = gpu_test_guard();
    for family in crate::arabic_join_tests::ARABIC_FACES {
        let Some((mut renderer, mut cfg)) = arabic_renderer(family) else {
            continue;
        };
        for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
            cfg.text_renderer = mode;
            for scale in [1.0, 2.0] {
                renderer.set_scale(scale);
                let mut snap =
                    snapshot_of(30, 4, "ـ\x1b[31mب\x1b[1;3mب\x1b[0mـ\x1b[?25l".as_bytes());
                snap.selection = Some(SelectionRange::new(
                    Point::new(Line(0), Column(1)),
                    Point::new(Line(0), Column(1)),
                    false,
                ));
                let mut overlay = focused(false);
                overlay.highlights.push(HighlightRect {
                    row: 0,
                    col: 2,
                    width: 1,
                    active: true,
                });
                capture(&mut renderer, &cfg, &[pane(&snap, 640, 220)], &overlay);
                let mut bridges = 0;
                for bridge in renderer
                    .glyph_instances
                    .iter()
                    .filter(|g| g.uv_size[0] == 1.0 && g.size[0] > 1.0)
                {
                    bridges += 1;
                    let column = ((bridge.pos[0] + bridge.size[0] / 2.0 - cfg.padding_x)
                        / renderer.cell_w)
                        .floor() as usize;
                    let left = (cfg.padding_x + column as f32 * renderer.cell_w).round();
                    let right = (cfg.padding_x + (column + 1) as f32 * renderer.cell_w).round();
                    assert!(bridge.pos[0] >= left && bridge.pos[0] + bridge.size[0] <= right);
                    let glyph = renderer.pane_buffers[0]
                        .layout_runs()
                        .next()
                        .unwrap()
                        .glyphs
                        .iter()
                        .find(|g| {
                            g.w > 0.0
                                && GridGlyphCell::from_metadata(g.metadata)
                                    .is_some_and(|c| c.column == column)
                        })
                        .unwrap();
                    let c = glyph.color_opt.unwrap();
                    assert_eq!(
                        bridge.color,
                        [
                            c.r() as f32 / 255.0,
                            c.g() as f32 / 255.0,
                            c.b() as f32 / 255.0,
                            c.a() as f32 / 255.0
                        ]
                    );
                }
                assert!(bridges > 0);
            }
        }
    }
}

#[test]
fn block_cursor_keeps_arabic_initial_and_its_bridge_in_the_panes_position() {
    let _serial = gpu_test_guard();
    for family in crate::arabic_join_tests::ARABIC_FACES {
        let Some((mut renderer, mut cfg)) = arabic_renderer(family) else {
            continue;
        };
        for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
            cfg.text_renderer = mode;
            for scale in [1.0, 2.0] {
                renderer.set_scale(scale);
                let mut snap = snapshot_of(30, 4, "مرحبا\r".as_bytes());
                snap.cursor.shape = alacritty_terminal::vte::ansi::CursorShape::Block;
                capture(
                    &mut renderer,
                    &cfg,
                    &[pane(&snap, 640, 220)],
                    &focused(true),
                );
                let cursor = renderer.pending_cursor_glyph.as_ref().unwrap();
                assert!(cursor.marks.len() >= 2, "base plus its bridge");
                assert!(cursor.marks.iter().any(|image| image.sample[2] == 1));
                let glyph = renderer.pane_buffers[0]
                    .layout_runs()
                    .next()
                    .unwrap()
                    .glyphs
                    .iter()
                    .find(|g| {
                        g.w > 0.0
                            && GridGlyphCell::from_metadata(g.metadata)
                                .is_some_and(|c| c.column == 4)
                    })
                    .unwrap();
                let image = renderer
                    .swash
                    .get_image_uncached(
                        &mut renderer.font_system,
                        glyph.physical((0.0, 0.0), 1.0).cache_key,
                    )
                    .unwrap();
                assert_eq!(
                    cursor.marks[0].rect[0],
                    (cfg.padding_x + 5.0 * renderer.cell_w - glyph.w).round() as i32
                        + image.placement.left
                );
                assert_eq!(
                    renderer.cursor_mark_images,
                    cursor
                        .marks
                        .iter()
                        .map(|image| image.rect)
                        .collect::<Vec<_>>()
                );
            }
        }
    }
}

#[test]
fn joined_arabic_leaves_latin_and_hebrew_rows_pixel_identical() {
    let _serial = gpu_test_guard();
    for family in crate::arabic_join_tests::ARABIC_FACES {
        let Some((mut renderer, mut cfg)) = arabic_renderer(family) else {
            continue;
        };
        for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
            cfg.text_renderer = mode;
            for scale in [1.0, 2.0] {
                renderer.set_scale(scale);
                let before = snapshot_of(
                    30,
                    5,
                    "Latin: A -> B\r\nשלום (123)\r\nplain\x1b[?25l".as_bytes(),
                );
                let after = snapshot_of(
                    30,
                    5,
                    "Latin: A -> B\r\nשלום (123)\r\nـبببـ\x1b[?25l".as_bytes(),
                );
                let before = capture(
                    &mut renderer,
                    &cfg,
                    &[pane(&before, 640, 220)],
                    &focused(false),
                );
                let after = capture(
                    &mut renderer,
                    &cfg,
                    &[pane(&after, 640, 220)],
                    &focused(false),
                );
                let bottom = (cfg.padding_y + 2.0 * renderer.cell_h).floor() as u32;
                for y in 0..bottom {
                    for x in 0..640 {
                        assert_eq!(
                            before.get_pixel(x, y),
                            after.get_pixel(x, y),
                            "{family} {mode:?}@{scale}: unrelated row pixel {x},{y}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn arabic_hidden_cells_and_row_ends_have_no_outward_seam() {
    let _serial = gpu_test_guard();
    for family in crate::arabic_join_tests::ARABIC_FACES {
        let Some((mut renderer, mut cfg)) = arabic_renderer(family) else {
            continue;
        };
        for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
            cfg.text_renderer = mode;
            let snap = snapshot_of(4, 4, "ب\x1b[8mب\x1b[0mبببب\x1b[?25l".as_bytes());
            capture(
                &mut renderer,
                &cfg,
                &[pane(&snap, 640, 220)],
                &focused(false),
            );
            for run in renderer.pane_buffers[0].layout_runs() {
                for (_, attrs) in renderer.pane_buffers[0].lines[run.line_i]
                    .attrs_list()
                    .spans_iter()
                {
                    let Some(cell) = GridGlyphCell::from_metadata(attrs.metadata) else {
                        continue;
                    };
                    if cell.column == 0 {
                        assert!(!cell.joins.left);
                    }
                    if cell.column + cell.width == 4 {
                        assert!(!cell.joins.right);
                    }
                    if run.line_i == 0 && cell.column == 2 {
                        assert_eq!(
                            cell.joins,
                            kettle_core::bidi::Joins::default(),
                            "hidden logical column 1"
                        );
                    }
                }
            }
        }
    }
}

/// The four fills a pane paints under highlighted cells.
#[derive(Clone, Copy, Debug)]
enum Highlight {
    Match,
    OtherMatch,
    Selection,
    UnfocusedSelection,
}

impl Highlight {
    const ALL: [Self; 4] = [
        Self::Match,
        Self::OtherMatch,
        Self::Selection,
        Self::UnfocusedSelection,
    ];

    /// Highlight logical `columns` of row 0.
    fn apply(
        self,
        snap: &mut crate::snapshot::PaneSnapshot,
        overlay: &mut Overlay,
        columns: std::ops::RangeInclusive<usize>,
    ) {
        use alacritty_terminal::selection::SelectionRange;
        use kettle_core::{Column, Line, Point};
        match self {
            Self::Match | Self::OtherMatch => overlay.highlights.push(HighlightRect {
                row: 0,
                col: *columns.start(),
                width: columns.end() + 1 - columns.start(),
                active: matches!(self, Self::Match),
            }),
            Self::Selection | Self::UnfocusedSelection => {
                snap.selection = Some(SelectionRange::new(
                    Point::new(Line(0), Column(*columns.start())),
                    Point::new(Line(0), Column(*columns.end())),
                    false,
                ));
                overlay.window_focused = matches!(self, Self::Selection);
            }
        }
    }

    /// The perimeter colour a low-contrast selection fill also draws.
    fn outline(self, renderer: &Renderer, cfg: &Config) -> Option<Rgb> {
        let focused = match self {
            Self::Match | Self::OtherMatch => return None,
            Self::Selection => true,
            Self::UnfocusedSelection => false,
        };
        super::theme::PaneSelectionFills::new(
            &cfg.theme,
            cfg.theme.background,
            renderer.selection_backdrop_alpha,
            &mut MinimumContrastCache::default(),
        )
        .outline(&cfg.theme, focused)
    }
}

/// The first pixel column (or row) a rectangle edge at `x` paints: the first
/// whose centre it covers.
fn first_pixel(x: f32) -> i64 {
    (x - 0.5).ceil() as i64
}

/// The frame's pixel rows of viewport row `row` of a pane at the origin.
fn row_pixels(renderer: &Renderer, cfg: &Config, row: usize) -> std::ops::Range<u32> {
    let (_, oy) = pane_grid_origin((0.0, 0.0, 640.0, 220.0), (cfg.padding_x, cfg.padding_y));
    let top = oy + row as f32 * renderer.cell_h;
    first_pixel(top) as u32..first_pixel(top + renderer.cell_h) as u32
}

/// The pixel columns covered by the glyph images of `rows` that reach into
/// `[left, right)`: the text of those cells as drawn, clipped to the frame.
fn ink_columns(
    renderer: &Renderer,
    rows: &std::ops::Range<u32>,
    (left, right): (f32, f32),
    width: u32,
) -> Option<(i64, i64)> {
    renderer
        .glyph_instances
        .iter()
        .filter(|glyph| {
            let middle = glyph.pos[1] + glyph.size[1] / 2.0;
            middle >= rows.start as f32
                && middle < rows.end as f32
                && glyph.pos[0] < right
                && glyph.pos[0] + glyph.size[0] > left
        })
        .map(|glyph| (glyph.pos[0] as i64, (glyph.pos[0] + glyph.size[0]) as i64))
        .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
        .map(|(a, b)| (a.max(0), b.min(i64::from(width))))
}

fn near(pixel: &image::Rgba<u8>, color: Rgb) -> bool {
    pixel[0].abs_diff(color.r) <= 8
        && pixel[1].abs_diff(color.g) <= 8
        && pixel[2].abs_diff(color.b) <= 8
}

/// The share of pixel column `x` within `rows` painted one of `colors`.
fn painted(frame: &image::RgbaImage, x: i64, rows: &std::ops::Range<u32>, colors: &[Rgb]) -> f32 {
    let Ok(x) = u32::try_from(x) else { return 0.0 };
    if x >= frame.width() {
        return 0.0;
    }
    let hits = rows
        .clone()
        .filter(|&y| {
            colors
                .iter()
                .any(|&color| near(frame.get_pixel(x, y), color))
        })
        .count();
    hits as f32 / rows.len().max(1) as f32
}

/// The colour most pixels of `columns` x `rows` show.
fn dominant(
    frame: &image::RgbaImage,
    columns: std::ops::Range<i64>,
    rows: &std::ops::Range<u32>,
) -> Rgb {
    let mut counts = std::collections::HashMap::new();
    for x in columns {
        for y in rows.clone() {
            let p = frame.get_pixel(x as u32, y);
            *counts.entry((p[0], p[1], p[2])).or_insert(0u32) += 1;
        }
    }
    let ((r, g, b), _) = counts
        .into_iter()
        .max_by_key(|&(color, count)| (count, color))
        .unwrap();
    Rgb::new(r, g, b)
}

/// A highlighted Arabic word's fill spans exactly the pixels its letters are
/// drawn over. A final wider than its cell overhangs on its open side, into
/// the padding or the cells before it, and an isolated letter overhangs to
/// its right; the fill reaches as far, and no further.
#[test]
fn highlights_cover_the_drawn_width_of_arabic_words() {
    let _serial = gpu_test_guard();
    // The text, its highlighted logical columns, the visual columns those
    // cells occupy, and whether its sad overhangs them on the left (or the
    // right). Every named face draws sad's final and isolated forms well
    // over a cell wide at this size.
    let cases = [
        ("نص", 0..=1, 0..2, true),
        ("ab نص cd", 3..=4, 3..5, true),
        ("ص", 0..=0, 0..1, false),
    ];
    for family in crate::arabic_join_tests::ARABIC_FACES {
        let Some((mut renderer, mut cfg)) = arabic_renderer(family) else {
            continue;
        };
        // Fractional, so cell edges fall between pixels while glyphs do not.
        cfg.padding_x = 24.25;
        cfg.padding_y = 4.0;
        let (ox, _) = pane_grid_origin((0.0, 0.0, 640.0, 220.0), (cfg.padding_x, cfg.padding_y));
        for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
            cfg.text_renderer = mode;
            for scale in [1.0, 2.0] {
                renderer.set_scale(scale);
                for (text, logical, visual, leftward) in &cases {
                    for highlight in Highlight::ALL {
                        let context = format!("{family} {mode:?}@{scale} {text:?} {highlight:?}");
                        let mut snap = snapshot_of(30, 4, format!("{text}\x1b[?25l").as_bytes());
                        let mut overlay = focused(false);
                        highlight.apply(&mut snap, &mut overlay, logical.clone());
                        let frame =
                            capture(&mut renderer, &cfg, &[pane(&snap, 640, 220)], &overlay);
                        let rows = row_pixels(&renderer, &cfg, 0);
                        let left = ox + visual.start as f32 * renderer.cell_w;
                        let right = ox + visual.end as f32 * renderer.cell_w;
                        let (grid_left, grid_right) = (first_pixel(left), first_pixel(right));
                        let (ink_left, ink_right) =
                            ink_columns(&renderer, &rows, (left, right), frame.width())
                                .unwrap_or_else(|| panic!("{context}: the word is drawn"));
                        assert!(
                            if *leftward {
                                ink_left < grid_left - 1
                            } else {
                                ink_right > grid_right + 1
                            },
                            "{context}: the fixture's sad overhangs its cells: drawn over \
                             {ink_left}..{ink_right}, cells {grid_left}..{grid_right}"
                        );
                        let fill = dominant(&frame, grid_left..grid_right, &rows);
                        assert_ne!(fill, cfg.theme.background, "{context}: highlighted");
                        let colors: Vec<_> = std::iter::once(fill)
                            .chain(highlight.outline(&renderer, &cfg))
                            .collect();
                        let (lo, hi) = (ink_left.min(grid_left), ink_right.max(grid_right));
                        for x in lo..hi {
                            let share = painted(&frame, x, &rows, &colors);
                            assert!(
                                share >= 0.1,
                                "{context}: column {x} of the text's {lo}..{hi} is drawn but \
                                 not highlighted ({share:.2}); its cells are {grid_left}..{grid_right}"
                            );
                        }
                        for x in [lo - 1, hi] {
                            let share = painted(&frame, x, &rows, &colors);
                            assert!(
                                share < 0.05,
                                "{context}: column {x}, past the text's {lo}..{hi}, is highlighted ({share:.2})"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Without bidi, and in rows without right-to-left text, every highlight
/// fill stays on the cell grid as before.
#[test]
fn highlights_off_bidi_rows_stay_on_the_grid() {
    let _serial = gpu_test_guard();
    for family in crate::arabic_join_tests::ARABIC_FACES {
        let Some((mut renderer, mut cfg)) = arabic_renderer(family) else {
            continue;
        };
        cfg.padding_x = 24.25;
        let (ox, oy) = pane_grid_origin((0.0, 0.0, 640.0, 220.0), (cfg.padding_x, cfg.padding_y));
        for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
            cfg.text_renderer = mode;
            for (bidi, text, columns) in [
                (false, "ab نص cd", 3..=4),
                (false, "ص", 0..=0),
                (true, "ab cd", 0..=4),
                (true, "\x1b[?1049hab نص cd", 3..=4),
            ] {
                cfg.bidi = bidi;
                for highlight in Highlight::ALL {
                    let context = format!("{family} {mode:?} bidi={bidi} {text:?} {highlight:?}");
                    let mut snap = snapshot_of(30, 4, format!("{text}\x1b[?25l").as_bytes());
                    let mut overlay = focused(false);
                    highlight.apply(&mut snap, &mut overlay, columns.clone());
                    capture(&mut renderer, &cfg, &[pane(&snap, 640, 220)], &overlay);
                    assert!(!renderer.pane_bidi[0].any(), "{context}: no bidi layout");
                    let quads: Vec<QuadInstance> = renderer.quads.uploaded().unwrap().collect();
                    let cells: Vec<_> = quads
                        .iter()
                        .filter(|quad| quad.pos[1] == oy && quad.size[1] == renderer.cell_h)
                        .collect();
                    assert!(!cells.is_empty(), "{context}: a highlight is drawn");
                    let on_grid = |x: f32| {
                        let column = (x - ox) / renderer.cell_w;
                        (column - column.round()).abs() < 1e-3
                    };
                    for quad in cells {
                        assert!(
                            on_grid(quad.pos[0]) && on_grid(quad.pos[0] + quad.size[0]),
                            "{context}: {:?} wide {:?} is off the grid",
                            quad.pos,
                            quad.size
                        );
                    }
                }
            }
        }
    }
}

/// A selection's outline traces its fill, widened over a final's overhang,
/// not the cell edge inside it.
#[test]
fn a_selection_outline_follows_the_arabic_overhang() {
    let _serial = gpu_test_guard();
    let Some(theme) = kettle_config::Theme::list().iter().find_map(|name| {
        let theme = kettle_config::Theme::by_name(name);
        super::theme::PaneSelectionFills::new(
            &theme,
            theme.background,
            1.0,
            &mut MinimumContrastCache::default(),
        )
        .outline(&theme, true)
        .is_some()
        .then_some(theme)
    }) else {
        eprintln!("SELECTION_OUTLINE_SKIP: no bundled theme outlines its selection");
        return;
    };
    for family in crate::arabic_join_tests::ARABIC_FACES {
        let Some((mut renderer, mut cfg)) = arabic_renderer(family) else {
            continue;
        };
        cfg.theme = theme.clone();
        cfg.padding_x = 24.25;
        cfg.padding_y = 4.0;
        let (ox, _) = pane_grid_origin((0.0, 0.0, 640.0, 220.0), (cfg.padding_x, cfg.padding_y));
        for scale in [1.0, 2.0] {
            renderer.set_scale(scale);
            let context = format!("{family}@{scale}");
            let mut snap = snapshot_of(30, 4, "نص\x1b[?25l".as_bytes());
            let mut overlay = focused(false);
            Highlight::Selection.apply(&mut snap, &mut overlay, 0..=1);
            let frame = capture(&mut renderer, &cfg, &[pane(&snap, 640, 220)], &overlay);
            let outline = Highlight::Selection
                .outline(&renderer, &cfg)
                .expect("the fixture theme outlines its selection");
            let rows = row_pixels(&renderer, &cfg, 0);
            let left = ox;
            let right = ox + 2.0 * renderer.cell_w;
            let (ink_left, _) = ink_columns(&renderer, &rows, (left, right), frame.width())
                .unwrap_or_else(|| panic!("{context}: the word is drawn"));
            assert!(
                ink_left < first_pixel(left) - 1,
                "{context}: the final overhangs"
            );
            // The outline's sides: strokes as tall as the row, in its colour.
            // Its fill can sit within a few levels of that colour, so the
            // strokes are told apart as quads, not pixels.
            let ink = [outline.r, outline.g, outline.b].map(|c| f32::from(c) / 255.0);
            let sides: Vec<f32> = renderer
                .quads
                .uploaded()
                .unwrap()
                .filter(|quad| {
                    quad.color[..3]
                        .iter()
                        .zip(ink)
                        .all(|(a, b)| (a - b).abs() < 0.5 / 255.0)
                        && quad.size[0] < quad.size[1]
                        && quad.size[1] > renderer.cell_h / 2.0
                        && quad.pos[1] < rows.end as f32
                        && quad.pos[1] + quad.size[1] > rows.start as f32
                })
                .map(|quad| quad.pos[0])
                .collect();
            assert_eq!(
                sides.iter().copied().reduce(f32::min),
                Some(ink_left as f32),
                "{context}: the outline's left side is at the drawn edge, not the \
                 cell edge {}: {sides:?}",
                left.round()
            );
            assert!(
                !sides.contains(&left.round()),
                "{context}: no side through the letter: {sides:?}"
            );
            assert_eq!(sides.len(), 2, "{context}: {sides:?}");
        }
    }
}
