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
    let (ox, oy) = pane_grid_origin((0.0, 0.0, 320.0, 120.0), (cfg.padding_x, cfg.padding_y));
    // The share of a cell's pixels painted in the selection fill: glyph ink
    // covers only part of a cell, and adapters round blends differently.
    let filled = |col: usize| {
        let (left, top) = (ox + col as f32 * renderer.cell_w, oy);
        let (mut near, mut total) = (0u32, 0u32);
        for y in top.ceil() as u32..(top + renderer.cell_h).floor() as u32 {
            for x in left.ceil() as u32..(left + renderer.cell_w).floor() as u32 {
                let pixel = frame.get_pixel(x, y);
                total += 1;
                if pixel[0].abs_diff(fill.r) <= 8
                    && pixel[1].abs_diff(fill.g) <= 8
                    && pixel[2].abs_diff(fill.b) <= 8
                {
                    near += 1;
                }
            }
        }
        near as f32 / total.max(1) as f32
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
