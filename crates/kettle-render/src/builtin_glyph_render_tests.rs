//! Box drawing, blocks, Powerline and braille drawn by Kettle, through the
//! live frame path: real panes from a real `Term`, `build_pane`, the glyph
//! pipeline and the offscreen capture.

use crate::builtin_glyphs::{bands, pixel_edge};
use crate::gpu_tests::{gpu_test_config, gpu_test_guard};
use crate::headless_tests::{capture, focused, pane, snapshot_of};
use crate::*;

/// Hidden cursor, white text on black, so a pixel's red channel is the
/// glyph's coverage there.
const INK: &str = "\x1b[?25l\x1b[38;2;255;255;255;48;2;0;0;0m";

/// A captured frame and the grid it was drawn on.
struct Frame {
    image: image::RgbaImage,
    origin: (f32, f32),
    cell: (f32, f32),
    strokes: crate::builtin_glyphs::Strokes,
}

/// Render `text` in a 20 x 4 pane filling a target of 320 x 120 logical
/// pixels at `scale`, on a black default background, or `None` on a host
/// with no GPU.
fn render(mut cfg: Config, scale: f32, text: &str) -> Option<Frame> {
    cfg.theme.background = Rgb::new(0, 0, 0);
    let (w, h) = ((320.0 * scale) as u32, (120.0 * scale) as u32);
    let mut renderer = Renderer::headless_for_tests_with(&cfg, w, h, scale, false)
        .expect("headless renderer builds")?;
    let snap = snapshot_of(20, 4, text.as_bytes());
    let image = capture(&mut renderer, &cfg, &[pane(&snap, w, h)], &focused(false));
    Some(Frame::of(image, &renderer, &cfg))
}

impl Frame {
    /// `image`, captured by `renderer` from one pane filling the target.
    fn of(image: image::RgbaImage, renderer: &Renderer, cfg: &Config) -> Self {
        let (w, h) = image.dimensions();
        Self {
            origin: pane_grid_origin(
                (0.0, 0.0, w as f32, h as f32),
                (cfg.padding_x, cfg.padding_y),
            ),
            cell: (renderer.cell_w, renderer.cell_h),
            strokes: renderer.builtin_strokes,
            image,
        }
    }

    /// Cell (`col`, `row`) in device pixels, `[x0, y0, x1, y1)`.
    fn cell(&self, col: usize, row: usize) -> [u32; 4] {
        let (ox, oy) = self.origin;
        let (cw, ch) = self.cell;
        [
            pixel_edge(ox + col as f32 * cw) as u32,
            pixel_edge(oy + row as f32 * ch) as u32,
            pixel_edge(ox + (col + 1) as f32 * cw) as u32,
            pixel_edge(oy + (row + 1) as f32 * ch) as u32,
        ]
    }

    fn red(&self, x: u32, y: u32) -> u8 {
        self.image.get_pixel(x, y)[0]
    }

    fn rgb(&self, x: u32, y: u32) -> [u8; 3] {
        let p = self.image.get_pixel(x, y);
        [p[0], p[1], p[2]]
    }

    /// Coverage down column `x` over pixel rows `ys`.
    fn column(&self, x: u32, ys: std::ops::Range<u32>) -> Vec<u8> {
        ys.map(|y| self.red(x, y)).collect()
    }

    /// Coverage along row `y` over pixel columns `xs`.
    fn row(&self, y: u32, xs: std::ops::Range<u32>) -> Vec<u8> {
        xs.map(|x| self.red(x, y)).collect()
    }

    /// Where a light line sits across a cell `len` pixels long, as pixel
    /// offsets from the cell's edge.
    fn light(&self, len: u32) -> std::ops::Range<u32> {
        let (a, b) = bands(len as i32, self.strokes).light;
        a as u32..b as u32
    }
}

/// The offsets in `profile` with any ink.
fn inked(profile: &[u8]) -> Vec<u32> {
    (0..profile.len() as u32)
        .filter(|&i| profile[i as usize] > 0)
        .collect()
}

macro_rules! frame_or_skip {
    ($frame:expr) => {
        match $frame {
            Some(frame) => frame,
            None => {
                eprintln!("no GPU adapter on this host; skipped");
                return;
            }
        }
    };
}

/// A run of `─` is one line: every pixel column across the run, including
/// the ones either side of each cell boundary, shows the same solid stroke,
/// so no join is brighter, darker or broken. At 1x, at 2x, at a fractional
/// cell width, and on the Legacy text renderer.
#[test]
fn a_run_of_light_lines_is_one_even_line_across_cell_boundaries() {
    let _serialized = gpu_test_guard();
    let fractional = Config {
        cell_width: 1.07,
        ..gpu_test_config()
    };
    let legacy = Config {
        text_renderer: kettle_config::TextRenderer::Legacy,
        ..gpu_test_config()
    };
    for (cfg, scale) in [
        (gpu_test_config(), 1.0),
        (gpu_test_config(), 2.0),
        (fractional, 1.0),
        (legacy, 1.0),
    ] {
        let frame = frame_or_skip!(render(cfg, scale, &format!("{INK}──────────")));
        let (first, last) = (frame.cell(0, 0), frame.cell(9, 0));
        let rows = first[1]..first[3];
        let expected = frame.column(first[0], rows.clone());
        let stroke = frame.light(first[3] - first[1]);
        assert_eq!(
            inked(&expected),
            stroke.clone().collect::<Vec<_>>(),
            "the line sits on the light stroke's rows at {scale}x"
        );
        assert!(
            stroke.clone().all(|y| expected[y as usize] == 255),
            "the line is solid at {scale}x: {expected:?}"
        );
        for x in first[0]..last[2] {
            assert_eq!(
                frame.column(x, rows.clone()),
                expected,
                "pixel column {x} of the run at {scale}x, cell width {}",
                frame.cell.0
            );
        }
    }
}

/// `┼` continues the `│` above and below it and the `─` either side of it on
/// exactly their pixels: one straight vertical line and one straight
/// horizontal line through the crossing.
#[test]
fn a_crossing_meets_its_neighbouring_lines_exactly() {
    let _serialized = gpu_test_guard();
    for scale in [1.0, 2.0] {
        let text = format!("{INK} │\r\n─┼─\r\n │");
        let frame = frame_or_skip!(render(gpu_test_config(), scale, &text));
        let (top, left, centre, right, bottom) = (
            frame.cell(1, 0),
            frame.cell(0, 1),
            frame.cell(1, 1),
            frame.cell(2, 1),
            frame.cell(1, 2),
        );
        let across = left[0]..right[2];
        let down = top[1]..bottom[3];
        let horizontal: Vec<u32> = frame
            .light(centre[3] - centre[1])
            .map(|y| centre[1] + y)
            .collect();
        let vertical: Vec<u32> = frame
            .light(centre[2] - centre[0])
            .map(|x| centre[0] + x)
            .collect();
        // Every pixel row of the three cells' height, outside the
        // horizontal line, shows only the vertical line, solid, on the same
        // columns.
        let reference = frame.row(top[1], across.clone());
        assert_eq!(
            inked(&reference)
                .into_iter()
                .map(|x| across.start + x)
                .collect::<Vec<_>>(),
            vertical,
            "at {scale}x"
        );
        for y in down.clone().filter(|y| !horizontal.contains(y)) {
            assert_eq!(
                frame.row(y, across.clone()),
                reference,
                "pixel row {y} at {scale}x"
            );
        }
        // And every pixel column of the three cells' width, outside the
        // vertical line, shows only the horizontal line.
        let reference = frame.column(left[0], down.clone());
        assert_eq!(
            inked(&reference)
                .into_iter()
                .map(|y| down.start + y)
                .collect::<Vec<_>>(),
            horizontal,
            "at {scale}x"
        );
        for x in across.clone().filter(|x| !vertical.contains(x)) {
            assert_eq!(
                frame.column(x, down.clone()),
                reference,
                "pixel column {x} at {scale}x"
            );
        }
        for &x in &vertical {
            for &y in &horizontal {
                assert_eq!(frame.red(x, y), 255, "the crossing at {scale}x");
            }
        }
    }
}

/// `█` fills exactly its cell's pixels: all of them solid, none beyond.
#[test]
fn a_full_block_fills_its_whole_cell() {
    let _serialized = gpu_test_guard();
    for (cfg, scale) in [(gpu_test_config(), 1.0), (gpu_test_config(), 2.0)] {
        let frame = frame_or_skip!(render(cfg, scale, &format!("{INK}█")));
        let [x0, y0, x1, y1] = frame.cell(0, 0);
        for y in y0..y1 {
            for x in x0..x1 {
                assert_eq!(frame.red(x, y), 255, "{x},{y} at {scale}x");
            }
        }
        for y in y0..y1 {
            assert_eq!(frame.red(x0 - 1, y), 0, "left of the cell at {scale}x");
            assert_eq!(frame.red(x1, y), 0, "right of the cell at {scale}x");
        }
        for x in x0..x1 {
            assert_eq!(frame.red(x, y0 - 1), 0, "above the cell at {scale}x");
            assert_eq!(frame.red(x, y1), 0, "below the cell at {scale}x");
        }
    }
}

/// `░ ▒ ▓` are three even tones, light to dark, not the font's dither.
#[test]
fn shades_are_three_distinct_even_tones() {
    let _serialized = gpu_test_guard();
    let frame = frame_or_skip!(render(gpu_test_config(), 1.0, &format!("{INK}░▒▓")));
    let mut levels = Vec::new();
    for col in 0..3 {
        let [x0, y0, x1, y1] = frame.cell(col, 0);
        let level = frame.rgb(x0, y0);
        for y in y0..y1 {
            for x in x0..x1 {
                assert_eq!(frame.rgb(x, y), level, "shade {col} at {x},{y}");
            }
        }
        levels.push(level[0]);
    }
    assert!(
        0 < levels[0] && levels[0] < levels[1] && levels[1] < levels[2] && levels[2] < 255,
        "{levels:?}"
    );
}

/// A Powerline separator spans the whole cell height, and its flat side is
/// solid against the neighbouring segment's background, with no column of
/// its own background between them: `` after a segment and `` before
/// one. Run with a taller row too, where a font glyph falls short.
#[test]
fn powerline_separators_fill_the_cell_height_and_meet_their_neighbours() {
    let _serialized = gpu_test_guard();
    const BLUE: [u8; 3] = [0, 0, 255];
    const GREEN: [u8; 3] = [0, 255, 0];
    let tall = Config {
        cell_height: 1.3,
        ..gpu_test_config()
    };
    for (cfg, scale) in [
        (gpu_test_config(), 1.0),
        (gpu_test_config(), 2.0),
        (tall, 1.0),
    ] {
        let text = "\x1b[?25l\x1b[48;2;0;0;255m \x1b[38;2;0;0;255;48;2;0;255;0m\u{E0B0}\
                    \x1b[48;2;0;255;0m \r\n\
                    \x1b[48;2;0;255;0m \x1b[38;2;0;0;255;48;2;0;255;0m\u{E0B2}\x1b[48;2;0;0;255m ";
        let frame = frame_or_skip!(render(cfg, scale, text));
        for (row, flat_col, neighbour) in [(0, 1, 0), (1, 1, 2)] {
            let [x0, y0, x1, y1] = frame.cell(flat_col, row);
            let (flat, tip) = if neighbour < flat_col {
                (x0, x1 - 1)
            } else {
                (x1 - 1, x0)
            };
            let next = frame.cell(neighbour, row);
            let beside = if neighbour < flat_col {
                next[2] - 1
            } else {
                next[0]
            };
            // The slopes cross the flat side within this many rows of
            // each end.
            let slope = ((y1 - y0) as f32 / 2.0 / (x1 - x0) as f32).ceil() as u32;
            for y in y0 + slope..y1 - slope {
                assert_eq!(frame.rgb(flat, y), BLUE, "row {row} flat side, y {y}");
                assert_eq!(frame.rgb(beside, y), BLUE, "row {row} neighbour, y {y}");
            }
            for y in [y0, y1 - 1] {
                let [_, _, b] = frame.rgb(flat, y);
                assert!(b > 0, "row {row}: the shape reaches pixel row {y}");
            }
            // The point reaches the far side of the cell.
            let mid = (y0 + y1) / 2;
            let [_, g, b] = frame.rgb(tip, mid);
            assert!(
                b > 0 && g < 255,
                "row {row}: the point reaches column {tip}"
            );
            assert_eq!(frame.rgb(tip, y0), GREEN, "row {row}: its own background");
        }
    }
}

/// `╭` curves from the `│` below it into the `─` beside it: at each edge it
/// leaves on exactly that line's pixels, solid, and its corner is round.
#[test]
fn an_arc_joins_the_lines_beside_and_below_it() {
    let _serialized = gpu_test_guard();
    for scale in [1.0, 2.0] {
        let frame = frame_or_skip!(render(gpu_test_config(), scale, &format!("{INK}╭─\r\n│")));
        let (arc, beside, below) = (frame.cell(0, 0), frame.cell(1, 0), frame.cell(0, 1));
        let rows = arc[1]..arc[3];
        let cols = arc[0]..arc[2];
        // Across the boundary into `─`.
        let edge = frame.column(arc[2] - 1, rows.clone());
        assert_eq!(edge, frame.column(beside[0], rows.clone()), "at {scale}x");
        let line: Vec<u32> = frame.light(arc[3] - arc[1]).collect();
        assert_eq!(inked(&edge), line, "at {scale}x");
        assert!(line.iter().all(|&y| edge[y as usize] == 255), "at {scale}x");
        // Across the boundary into `│`.
        let edge = frame.row(arc[3] - 1, cols.clone());
        assert_eq!(edge, frame.row(below[1], cols.clone()), "at {scale}x");
        let line: Vec<u32> = frame.light(arc[2] - arc[0]).collect();
        assert_eq!(inked(&edge), line, "at {scale}x");
        assert!(line.iter().all(|&x| edge[x as usize] == 255), "at {scale}x");
        // Not a square corner: where `┌` would turn, the arc has cut inside.
        let corner = (
            arc[0] + frame.light(arc[2] - arc[0]).start,
            arc[1] + frame.light(arc[3] - arc[1]).start,
        );
        assert!(frame.red(corner.0, corner.1) < 255, "at {scale}x");
        // And the curve is antialiased.
        assert!(
            rows.clone()
                .any(|y| cols.clone().any(|x| (1..255).contains(&frame.red(x, y)))),
            "at {scale}x"
        );
    }
}

/// With `builtin-box-drawing = false` the font draws these characters again:
/// they are in the shaped text, Kettle records no cells of its own, and the
/// font's shade is not an even tone. The switch reloads live, both ways, on
/// one renderer.
#[test]
fn turning_builtin_box_drawing_off_returns_the_font_glyphs_live() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = crate::headless_tests::renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(20, 4, format!("{INK}─░").as_bytes());
    let shade_is_even = |frame: &Frame| {
        let [x0, y0, x1, y1] = frame.cell(1, 0);
        let first = frame.red(x0, y0);
        (y0..y1).all(|y| (x0..x1).all(|x| frame.red(x, y) == first))
    };
    for builtin in [true, false, true] {
        cfg.builtin_box_drawing = builtin;
        let image = capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(false),
        );
        let frame = Frame::of(image, &renderer, &cfg);
        let text = renderer.pane_buffers[0].lines[0].text();
        let cells = renderer.pane_builtins[0].len();
        if builtin {
            assert_eq!(cells, 2);
            assert!(!text.contains('─') && !text.contains('░'), "{text:?}");
            assert!(shade_is_even(&frame));
        } else {
            assert_eq!(cells, 0);
            assert!(text.starts_with("─░"), "{text:?}");
            assert!(!shade_is_even(&frame), "the font's shade is a dither");
        }
    }
}

/// Under a focused block cursor the glyph is drawn in the cursor's text
/// colour on the block, from the same mask; in the blink's off phase it is
/// the cell's own colour again.
#[test]
fn a_block_cursor_draws_the_glyph_in_the_cursor_text_colour() {
    let _serialized = gpu_test_guard();
    let cfg = gpu_test_config();
    let Some((mut renderer, _)) = crate::headless_tests::renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(
        20,
        4,
        b"\x1b[38;2;255;255;255;48;2;0;0;0m\xe2\x94\x80\x1b[D",
    );
    let block = crate::color::cursor_block_color(&cfg.theme, &snap.colors);
    let text = crate::color::cursor_glyph_color(&cfg.theme, &snap.colors, Rgb::new(0, 0, 0));
    assert_ne!(block, text);
    let (ox, oy) = pane_grid_origin((0.0, 0.0, 320.0, 120.0), (cfg.padding_x, cfg.padding_y));
    let [x0, y0, x1, y1] = [
        pixel_edge(ox) as u32,
        pixel_edge(oy) as u32,
        pixel_edge(ox + renderer.cell_w) as u32,
        pixel_edge(oy + renderer.cell_h) as u32,
    ];
    let (a, b) = bands((y1 - y0) as i32, renderer.builtin_strokes).light;
    let line = y0 + a as u32..y0 + b as u32;
    for (phase, stroke, fill) in [
        (true, [text.r, text.g, text.b], [block.r, block.g, block.b]),
        (false, [255, 255, 255], [0, 0, 0]),
    ] {
        let image = capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(phase),
        );
        assert!(
            renderer
                .pending_cursor_glyph
                .as_ref()
                .is_some_and(|cursor| cursor.builtin.is_some())
        );
        for y in y0..y1 {
            for x in x0..x1 {
                let p = image.get_pixel(x, y);
                let expected = if line.contains(&y) { stroke } else { fill };
                assert_eq!(
                    [p[0], p[1], p[2]],
                    expected,
                    "{x},{y} cursor shown: {phase}"
                );
            }
        }
    }
}

/// In a right-to-left row a built-in glyph moves with its cell to its
/// visual column.
#[test]
fn a_reordered_row_draws_the_glyph_at_its_visual_column() {
    let _serialized = gpu_test_guard();
    // Logical א│בג is drawn ג ב │ א: the line moves from column 1 to 2.
    let frame = frame_or_skip!(render(gpu_test_config(), 1.0, &format!("{INK}א│בג")));
    let row = frame.cell(2, 0)[1] + 1;
    for (col, expect) in [(1, false), (2, true)] {
        let [x0, _, x1, _] = frame.cell(col, 0);
        let line: Vec<u32> = frame.light(x1 - x0).map(|x| x0 + x).collect();
        let inked: Vec<u32> = (x0..x1).filter(|&x| frame.red(x, row) == 255).collect();
        assert_eq!(inked == line, expect, "column {col}: {inked:?}");
    }
}

/// Stroke widths come from the face's own underline: the bundled JetBrains
/// Mono reports 50/1000 em, and a family nobody has reports nothing.
#[test]
fn stroke_widths_read_the_faces_underline() {
    let mut fonts = FontSystem::new();
    load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    let em = crate::fonts::underline_em(&mut fonts, kettle_config::font::FAMILY)
        .expect("the bundled face reports an underline");
    assert!((em - 0.05).abs() < 1e-6, "{em}");
    assert_eq!(
        crate::fonts::underline_em(&mut fonts, "No Such Family Kettle Test"),
        None
    );
}

/// A scale or size change re-derives the stroke widths, as it re-measures the
/// cell: 13 pt is one pixel at 1x and two at 2x.
#[test]
fn stroke_widths_follow_the_scale_and_font_size() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, _)) = crate::headless_tests::renderer(64, 64) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    assert_eq!(renderer.builtin_strokes.light, 1);
    renderer.set_scale(2.0);
    assert_eq!(renderer.builtin_strokes.light, 2);
    assert_eq!(renderer.builtin_strokes.heavy, 4);
    renderer.set_scale(1.0);
    renderer.set_font_size(40.0);
    assert_eq!(renderer.builtin_strokes.light, 2);
    renderer.set_font_size(13.0);
    assert_eq!(renderer.builtin_strokes.light, 1);
}
