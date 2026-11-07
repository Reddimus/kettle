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

/// Use a real white background quad as the coverage oracle, independently
/// of the CPU mask bounds. Probe both ends of a cell on either side of the
/// rasterizer's half-pixel boundary, including its subpixel snap band.
#[test]
fn builtin_masks_and_cursor_match_backgrounds_at_half_pixel_edges() {
    let _serialized = gpu_test_guard();
    fn view(snap: &PaneSnapshot, pane_offset: f32) -> PaneView<'_> {
        PaneView {
            terminal: (pane_offset, pane_offset, 319.0, 119.0),
            ..pane(snap, 320, 120)
        }
    }
    let mut failures = Vec::new();
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        for (scale, width, pane_offset) in [
            (1.0, 1.0, 0.0),
            (1.0, 1.07, 0.25),
            (2.0, 1.07, 0.25),
            (1.25, 0.5, 0.25),
        ] {
            let mut cfg = Config {
                text_renderer: mode,
                cell_width: width,
                background_opacity: 1.0,
                ..gpu_test_config()
            };
            cfg.theme.background = Rgb::new(0, 0, 0);
            let mut renderer = frame_or_skip!(
                Renderer::headless_for_tests_with(&cfg, 320, 120, scale, false)
                    .expect("headless renderer builds")
            );
            eprintln!(
                "PIXEL_EDGE_GPU: {mode:?}, scale={scale}, width={width}, {:?}",
                renderer.gpu.adapter_info()
            );
            let (cw, ch) = (renderer.cell_w, renderer.cell_h);
            for end in [false, true] {
                for delta in [-0.004, -0.0001, 0.0, 0.0001, 0.004] {
                    let edge = if end { (40.5, 60.5) } else { (12.5, 12.5) };
                    cfg.padding_x = edge.0 + delta - pane_offset - if end { cw } else { 0.0 };
                    cfg.padding_y = edge.1 + delta - pane_offset - if end { ch } else { 0.0 };
                    let reference = snapshot_of(20, 4, b"\x1b[?25l\x1b[48;2;255;255;255m \x1b[0m");
                    let background = capture(
                        &mut renderer,
                        &cfg,
                        &[view(&reference, pane_offset)],
                        &focused(false),
                    );
                    let covered: Vec<_> = background
                        .enumerate_pixels()
                        .filter_map(|(x, y, p)| (p[0] == 255).then_some((x, y)))
                        .collect();
                    assert!(
                        !covered.is_empty(),
                        "the reference must draw a cell background"
                    );
                    let x0 = covered.iter().map(|p| p.0).min().unwrap();
                    let y0 = covered.iter().map(|p| p.1).min().unwrap();
                    let x1 = covered.iter().map(|p| p.0).max().unwrap() + 1;
                    let y1 = covered.iter().map(|p| p.1).max().unwrap() + 1;
                    assert_eq!(covered.len(), ((x1 - x0) * (y1 - y0)) as usize);
                    let context = format!(
                        "{mode:?} scale={scale} width={width} end={end} delta={delta} origin=({},{}) cell=({cw},{ch}) background=[{x0},{y0},{x1},{y1}]",
                        pane_offset + cfg.padding_x,
                        pane_offset + cfg.padding_y
                    );
                    for glyph in ['\u{2588}', '\u{2592}'] {
                        let snap = snapshot_of(20, 4, format!("{INK}{glyph}").as_bytes());
                        let image = capture(
                            &mut renderer,
                            &cfg,
                            &[view(&snap, pane_offset)],
                            &focused(false),
                        );
                        let ink = image.get_pixel((x0 + x1) / 2, (y0 + y1) / 2).0;
                        assert!(ink[0] > 0, "the glyph must draw: {context}");
                        let wrong: Vec<_> = image
                            .enumerate_pixels()
                            .filter_map(|(x, y, p)| {
                                let expected = if background.get_pixel(x, y)[0] == 255 {
                                    ink
                                } else {
                                    [0, 0, 0, 255]
                                };
                                (p.0 != expected).then_some((x, y, p.0, expected))
                            })
                            .collect();
                        if !wrong.is_empty() {
                            failures.push(format!(
                                "mask {glyph:?}: {context}; {} wrong pixels, first={:?}",
                                wrong.len(),
                                wrong[0]
                            ));
                        }
                    }
                    // A shade's normal ink must not affect either edge of the
                    // focused block. Compare two actual scenes, then crop the
                    // retained buffers in both blink phases without uploads.
                    let mut cursor_reference = None;
                    for fg in ["255;0;0", "0;255;0"] {
                        let snap = snapshot_of(
                            20,
                            4,
                            format!("\x1b[38;2;{fg};48;2;0;0;0m\u{2592}\x1b[D").as_bytes(),
                        );
                        let image = capture(
                            &mut renderer,
                            &cfg,
                            &[view(&snap, pane_offset)],
                            &focused(true),
                        );
                        if let Some(reference) = &cursor_reference {
                            if &image != reference {
                                failures.push(format!("cursor normal ink leaked: {context}"));
                            }
                        } else {
                            cursor_reference = Some(image);
                        }
                        let whole = PatchRect {
                            x: 0,
                            y: 0,
                            width: 320,
                            height: 120,
                        };
                        for phase in [true, false] {
                            let full = renderer.scene_window_for_tests(&cfg, whole, phase);
                            if phase {
                                let ink = full.get_pixel((x0 + x1) / 2, (y0 + y1) / 2);
                                let wrong: Vec<_> = covered
                                    .iter()
                                    .filter(|&&(x, y)| full.get_pixel(x, y) != ink)
                                    .collect();
                                if !wrong.is_empty() {
                                    let (x, y) = *wrong[0];
                                    failures.push(format!("cursor uneven: {context}; {} wrong pixels, first=({x},{y}) {:?}, interior={ink:?}", wrong.len(), full.get_pixel(x, y)));
                                }
                            }
                            for rect in [
                                PatchRect {
                                    x: x0,
                                    y: y0,
                                    width: x1 - x0,
                                    height: y1 - y0,
                                },
                                PatchRect {
                                    x: x0 + 1,
                                    y: y0 + 1,
                                    width: x1 - x0 - 2,
                                    height: y1 - y0 - 2,
                                },
                                PatchRect {
                                    x: x0 - 2,
                                    y: y0 - 2,
                                    width: x1 - x0 + 4,
                                    height: y1 - y0 + 4,
                                },
                            ] {
                                let cropped = renderer.scene_window_for_tests(&cfg, rect, phase);
                                let expected = image::imageops::crop_imm(
                                    &full,
                                    rect.x,
                                    rect.y,
                                    rect.width,
                                    rect.height,
                                )
                                .to_image();
                                if cropped != expected {
                                    failures.push(format!("cursor crop mismatch: {context}; phase={phase} rect={rect:?}"));
                                }
                            }
                        }
                    }
                    eprintln!("PIXEL_EDGE_CASE: {context}");
                }
            }
        }
    }
    for failure in &failures {
        eprintln!("PIXEL_EDGE_FAILURE: {failure}");
    }
    assert!(
        failures.is_empty(),
        "{} pixel-edge failures",
        failures.len()
    );
}

#[test]
fn builtin_cursor_excludes_neighboring_ink_at_half_pixel_edges() {
    let _serialized = gpu_test_guard();
    let mut failures = Vec::new();
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        for scale in [1.0, 2.0] {
            let mut cfg = Config {
                text_renderer: mode,
                cell_width: 0.5,
                background_opacity: 1.0,
                ..gpu_test_config()
            };
            cfg.theme.background = Rgb::new(0, 0, 0);
            let mut renderer = frame_or_skip!(
                Renderer::headless_for_tests_with(&cfg, 320, 120, scale, false)
                    .expect("headless renderer builds")
            );
            eprintln!(
                "PIXEL_EDGE_NEIGHBOR_GPU: {mode:?}, scale={scale}, {:?}",
                renderer.gpu.adapter_info()
            );
            for delta in [-0.0001, 0.0, 0.0001] {
                cfg.padding_x = 12.5 + delta - renderer.cell_w;
                let reference =
                    snapshot_of(20, 4, b"\x1b[?25l\x1b[2G\x1b[48;2;255;255;255m \x1b[0m");
                let background = capture(
                    &mut renderer,
                    &cfg,
                    &[pane(&reference, 320, 120)],
                    &focused(false),
                );
                let covered: Vec<_> = background
                    .enumerate_pixels()
                    .filter_map(|(x, y, p)| (p[0] == 255).then_some((x, y)))
                    .collect();
                assert!(!covered.is_empty());
                let mut phases = Vec::new();
                for prefix in [" ", "W"] {
                    let snap = snapshot_of(
                        20,
                        4,
                        format!("\x1b[38;2;255;0;0;48;2;0;0;0m{prefix}\u{2592}\x1b[D").as_bytes(),
                    );
                    let mut pixels = Vec::new();
                    for phase in [true, false] {
                        let image = capture(
                            &mut renderer,
                            &cfg,
                            &[pane(&snap, 320, 120)],
                            &focused(phase),
                        );
                        pixels.push(
                            covered
                                .iter()
                                .map(|&(x, y)| image.get_pixel(x, y).0)
                                .collect::<Vec<_>>(),
                        );
                    }
                    phases.push(pixels);
                }
                let wrong: Vec<_> = phases[0][0]
                    .iter()
                    .zip(&phases[1][0])
                    .enumerate()
                    .filter_map(|(i, (a, b))| (a != b).then_some((covered[i], a, b)))
                    .collect();
                if !wrong.is_empty() {
                    failures.push(format!("{mode:?} scale={scale} delta={delta}: {} contaminated cursor pixels, first={:?}", wrong.len(), wrong[0]));
                }
                assert_ne!(
                    phases[0][1], phases[1][1],
                    "the off phase must expose real neighboring ink: {mode:?}, scale={scale}, delta={delta}"
                );
            }
        }
    }
    for failure in &failures {
        eprintln!("PIXEL_EDGE_NEIGHBOR_FAILURE: {failure}");
    }
    assert!(
        failures.is_empty(),
        "{} neighboring-ink failures",
        failures.len()
    );
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

/// A shade under a focused block cursor is the cursor's two colours mixed,
/// whatever colour the cell's own text has: the pane's copy, drawn in the
/// cell's colour first, must not show through the partly covered pixels.
#[test]
fn a_shade_under_the_cursor_shows_only_the_cursor_colours() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = crate::headless_tests::renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let mut cells = Vec::new();
    let mut block = Rgb::new(0, 0, 0);
    for fg in ["255;0;0", "0;255;0"] {
        let snap = snapshot_of(
            20,
            4,
            format!("\x1b[38;2;{fg};48;2;0;0;0m▒\x1b[D").as_bytes(),
        );
        block = crate::color::cursor_block_color(&cfg.theme, &snap.colors);
        let image = capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(true),
        );
        let frame = Frame::of(image, &renderer, &cfg);
        let [x0, y0, x1, y1] = frame.cell(0, 0);
        let pixels: Vec<[u8; 3]> = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .map(|(x, y)| frame.rgb(x, y))
            .collect();
        assert!(
            pixels.iter().all(|p| *p == pixels[0]),
            "the shade is one even tone under the cursor: {:?}",
            pixels.iter().collect::<std::collections::BTreeSet<_>>()
        );
        cells.push(pixels[0]);
    }
    assert_eq!(cells[0], cells[1], "the cell's own colour shows through");
    assert_ne!(cells[0], [block.r, block.g, block.b], "the shade is drawn");
}

#[test]
fn a_builtin_block_cursor_excludes_neighboring_text_overhang() {
    let _serialized = gpu_test_guard();
    let whole = PatchRect {
        x: 0,
        y: 0,
        width: 320,
        height: 120,
    };
    let crop = |image: &image::RgbaImage, rect: PatchRect| {
        image::imageops::crop_imm(image, rect.x, rect.y, rect.width, rect.height).to_image()
    };
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        for allow_mapping in [false, true] {
            let cfg = Config {
                text_renderer: mode,
                cell_width: 0.5,
                background_opacity: 1.0,
                ..gpu_test_config()
            };
            let mut renderer = frame_or_skip!(
                Renderer::headless_for_tests_with_mapping(
                    &cfg,
                    320,
                    120,
                    1.0,
                    false,
                    allow_mapping
                )
                .expect("headless renderer builds")
            );
            eprintln!(
                "OVERHANG_GPU: {mode:?}, allow_mapping={allow_mapping}, mapped={}, {:?}",
                renderer.gpu.mapped_uploads(),
                renderer.gpu.adapter_info()
            );
            let mut reference = Vec::new();
            for prefix in [" ", "W"] {
                let mut snap = snapshot_of(
                    20,
                    4,
                    format!("\x1b[38;2;255;0;0;48;2;0;0;0m{prefix}\u{2592}{prefix}\u{2592}")
                        .as_bytes(),
                );
                let mut retained = None;
                for (step, col) in [1, 3, 1].into_iter().enumerate() {
                    snap.cursor.point.column = kettle_core::Column(col);
                    let initial = capture(
                        &mut renderer,
                        &cfg,
                        &[pane(&snap, 320, 120)],
                        &focused(true),
                    );
                    assert!(renderer.glyph_pipeline.cursor_clip.is_some());
                    let instances =
                        bytemuck::cast_slice::<_, u8>(&renderer.glyph_instances).to_vec();
                    if let Some(previous) = &retained {
                        assert_eq!(
                            &instances, previous,
                            "cursor moves retain normal pane instances"
                        );
                    }
                    retained = Some(instances);
                    let frame = Frame::of(initial.clone(), &renderer, &cfg);
                    let [x0, y0, x1, y1] = frame.cell(col, 0);
                    let cell = PatchRect {
                        x: x0,
                        y: y0,
                        width: x1 - x0,
                        height: y1 - y0,
                    };
                    let windows = [
                        cell,
                        PatchRect {
                            x: x0 + 1,
                            y: y0 + 1,
                            width: x1 - x0 - 2,
                            height: y1 - y0 - 2,
                        },
                        PatchRect {
                            x: x0 - 2,
                            y: y0 - 2,
                            width: x1 - x0 + 4,
                            height: y1 - y0 + 4,
                        },
                    ];
                    let mut phases = Vec::new();
                    // Draw directly from the retained buffers: no frame preparation
                    // or instance uploads occur between these blink phases.
                    for phase in [true, false, true] {
                        let image = renderer.scene_window_for_tests(&cfg, whole, phase);
                        if phase {
                            assert!(image == initial, "on phase restores the complete scene");
                        } else {
                            for (x, y, pixel) in image.enumerate_pixels() {
                                if x < x0 || x >= x1 || y < y0 || y >= y1 {
                                    assert_eq!(
                                        pixel,
                                        initial.get_pixel(x, y),
                                        "blink changed text outside cursor at {x},{y} in {mode:?}"
                                    );
                                }
                            }
                        }
                        for rect in windows {
                            assert_eq!(
                                renderer.scene_window_for_tests(&cfg, rect, phase),
                                crop(&image, rect),
                                "cropped pass: {mode:?}, mapping={allow_mapping}, col={col}, phase={phase}"
                            );
                        }
                        phases.push(crop(&image, cell));
                    }
                    if prefix == " " {
                        reference.push(phases.clone());
                    } else {
                        assert_eq!(
                            phases[0], reference[step][0],
                            "neighboring W contaminated cursor: {mode:?}, mapping={allow_mapping}, col={col}"
                        );
                        assert_ne!(
                            phases[1], reference[step][1],
                            "off phase must expose real neighboring overhang: {mode:?}"
                        );
                    }
                    // Put preedit on another row so it cannot mask a stale text
                    // exclusion in the old cursor cell. Closing it restores the block.
                    let mut overlay = focused(true);
                    overlay.ime_preedit = Some(ImePreedit {
                        text: "a".into(),
                        row: 1,
                        col,
                    });
                    capture(&mut renderer, &cfg, &[pane(&snap, 320, 120)], &overlay);
                    assert!(renderer.ime_prepared);
                    let composing = renderer.scene_window_for_tests(&cfg, whole, false);
                    assert_eq!(
                        crop(&composing, cell),
                        phases[1],
                        "IME must show normal text in the old block cell: {mode:?}"
                    );
                    assert_eq!(
                        renderer.scene_window_for_tests(&cfg, cell, false),
                        phases[1],
                        "IME cropped pass"
                    );
                    let closed = capture(
                        &mut renderer,
                        &cfg,
                        &[pane(&snap, 320, 120)],
                        &focused(true),
                    );
                    assert!(!renderer.ime_prepared);
                    assert!(closed == initial, "closing IME restores the block cursor");
                }
            }
            eprintln!("OVERHANG_MATRIX_PASS: {mode:?}, allow_mapping={allow_mapping}");
        }
    }
}

#[test]
fn a_builtin_block_cursor_preserves_highlight_decorations() {
    use alacritty_terminal::selection::SelectionRange;
    use kettle_core::{Column, Line, Point};

    let _serialized = gpu_test_guard();
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        let cfg = Config {
            text_renderer: mode,
            ..gpu_test_config()
        };
        let mut renderer = frame_or_skip!(
            Renderer::headless_for_tests(&cfg, 320, 120).expect("headless renderer builds")
        );
        for search in [false, true] {
            for style in ["4", "9", "0"] {
                let mut snap =
                    snapshot_of(20, 4, format!("\x1b[{style}m\u{2502}\u{2502}\r").as_bytes());
                let mut overlay = focused(true);
                if search {
                    overlay.highlights.push(HighlightRect {
                        col: 0,
                        row: 0,
                        width: 2,
                        active: true,
                    });
                } else {
                    snap.selection = Some(SelectionRange::new(
                        Point::new(Line(0), Column(0)),
                        Point::new(Line(0), Column(1)),
                        false,
                    ));
                }
                if style == "0" {
                    overlay.links.push(LinkRect {
                        col: 0,
                        row: 0,
                        width: 2,
                        hover: true,
                        explicit: true,
                    });
                }
                // Move across the retained pane instances, then blink both
                // ways: only the current cursor cell may be omitted.
                for col in [0, 1, 0] {
                    snap.cursor.point.column = Column(col);
                    for phase in [true, false, true] {
                        overlay.cursor_visible = phase;
                        let image =
                            capture(&mut renderer, &cfg, &[pane(&snap, 320, 120)], &overlay);
                        assert!(renderer.glyph_pipeline.cursor_clip.is_some());
                        let frame = Frame::of(image, &renderer, &cfg);
                        let [x0, y0, x1, y1] = frame.cell(col, 0);
                        let [rx0, _, rx1, _] = frame.cell(1 - col, 0);
                        let x = x0 + (x1 - x0) / 4;
                        let rx = rx0 + (rx1 - rx0) / 4;
                        let y = match style {
                            "4" => pixel_edge(frame.origin.1 + frame.cell.1 - 2.0) as u32,
                            "9" => pixel_edge(frame.origin.1 + frame.cell.1 * 0.5) as u32,
                            _ => y1 - 1,
                        };
                        let expected = frame.rgb(rx, y);
                        assert_ne!(expected, frame.rgb(rx, y0 + 2), "decoration is visible");
                        assert_eq!(
                            frame.rgb(x, y),
                            expected,
                            "{mode:?}, search={search}, SGR={style}, col={col}, phase={phase}"
                        );
                        let rect = PatchRect {
                            x: x0,
                            y: y0,
                            width: x1 - x0,
                            height: y1 - y0,
                        };
                        let window = renderer.scene_window_for_tests(&cfg, rect, phase);
                        assert_eq!(
                            window,
                            image::imageops::crop_imm(
                                &frame.image,
                                x0,
                                y0,
                                rect.width,
                                rect.height
                            )
                            .to_image(),
                            "cursor window matches the full scene"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_builtin_block_cursor_preserves_inline_images_and_pane_outlines() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = crate::headless_tests::renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(20, 4, "\u{2500}\r".as_bytes());
    let images = [kettle_core::Placement {
        abs_line: snap.history_origin,
        col: 0,
        cell_cols: 1,
        cell_rows: 1,
        x_offset_cells: 0.0,
        y_offset_cells: 0.0,
        display_cols: 1.0,
        display_rows: 1.0,
        img: kettle_core::ImageData::new(1, 1, vec![0, 255, 0, 255]).unwrap(),
        source_rect: None,
        source_crop: None,
        id: None,
        placement_id: 0,
        kitty_params: None,
        z: 0,
    }];
    for outline in [false, true] {
        let mut view = pane(&snap, 320, 120);
        if !outline {
            view.images = &images;
        }
        let image = capture(&mut renderer, &cfg, &[view], &focused(true));
        let frame = Frame::of(image, &renderer, &cfg);
        let [x0, y0, x1, y1] = frame.cell(0, 0);
        if outline {
            // Exercise the actual outline pipeline over the cursor cell,
            // independently of native window corner and split layout policy.
            renderer.pane_outlines.upload(
                &renderer.gpu.device,
                &renderer.gpu.queue,
                [320.0, 120.0],
                &[pane_outline(
                    (x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32),
                    Rgb::new(0, 255, 0),
                    2.0,
                    0.0,
                    0,
                )],
            );
        }
        let rect = PatchRect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        };
        for phase in [true, false, true] {
            let window = renderer.scene_window_for_tests(&cfg, rect, phase);
            assert_eq!(
                window.get_pixel(2, 0).0,
                [0, 255, 0, 255],
                "overlay survives: outline={outline}, phase={phase}"
            );
        }
    }
}

/// The cursor's built-in glyph reuses the slot the pane resolved for the
/// same cell and never allocates one itself: it is resolved after the pane
/// instances are uploaded, and an allocation then could evict a slot they
/// still draw from. With no resident slot it draws nothing and writes
/// nothing to the atlas.
#[test]
fn the_cursor_glyph_never_allocates_atlas_space() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = crate::headless_tests::renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(20, 4, "\x1b[38;2;255;255;255;48;2;0;0;0m▒\x1b[D".as_bytes());
    let panes = [pane(&snap, 320, 120)];
    capture(&mut renderer, &cfg, &panes, &focused(true));
    assert!(renderer.glyph_pipeline.cursor_clip.is_some());
    // Lose every slot without asking for a pane upload, as eviction under
    // atlas pressure can.
    renderer.glyph_pipeline.clear();
    let before = renderer.glyph_pipeline.upload_counts().texture_writes;
    capture(&mut renderer, &cfg, &panes, &focused(true));
    assert_eq!(
        renderer.glyph_pipeline.upload_counts().texture_writes,
        before,
        "the cursor rasterized into the atlas"
    );
    assert!(renderer.glyph_pipeline.cursor_clip.is_none());
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
