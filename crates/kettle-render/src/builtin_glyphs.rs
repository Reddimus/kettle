//! Box drawing, block elements, Powerline separators, geometric triangles and
//! braille, drawn by Kettle instead of taken from a font.
//!
//! A font draws these from outlines designed for its own em box. Lines stop
//! short of the cell or overshoot into the next one, neighbouring glyphs
//! overlap and double their antialiasing at every join, shades show the
//! font's dither, and a separator leaves a seam above or below it whenever
//! the row is taller than the face. Ghostty, WezTerm, kitty and Alacritty
//! draw them themselves for that reason, and so does Kettle while
//! `builtin-box-drawing` is on.
//!
//! Each glyph here is a coverage mask exactly the size of its cell's
//! device-pixel rectangle (see [`pixel_edge`]). Straight strokes are whole
//! device pixels placed by one integer rule from the cell's width or height
//! alone, so every cell of a column puts a vertical line on the same pixels,
//! every cell of a row puts a horizontal line on the same pixels, and a line
//! runs across cells and rows with no seam and no doubled join. Arcs,
//! diagonals and the Powerline curves are antialiased, and meet the straight
//! strokes on pixel centres so the join has full coverage.

/// The fraction of an em used for the light stroke when the face reports no
/// underline thickness. Most monospace faces use 0.05 to 0.07.
const DEFAULT_UNDERLINE_EM: f32 = 0.0625;

/// The thickest light stroke, in device pixels, whatever the font says.
const MAX_LIGHT_STROKE: f32 = 64.0;

/// Whether Kettle draws `ch` itself while `builtin-box-drawing` is on.
pub(crate) fn is_builtin(ch: char) -> bool {
    matches!(
        ch,
        '\u{2500}'..='\u{259F}' | '\u{25E2}'..='\u{25E5}' | '\u{2800}'..='\u{28FF}' | '\u{E0B0}'..='\u{E0BF}'
    )
}

/// Line widths in device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Strokes {
    pub(crate) light: u16,
    pub(crate) heavy: u16,
}

impl Strokes {
    /// Widths for a face whose underline is `underline_em` of its em, at
    /// `font_px` physical pixels (the font size times the display scale).
    ///
    /// The light stroke is the underline thickness rounded up to whole
    /// pixels, at least one: 0.05 em is one pixel at 13 px and two at 26 px,
    /// so a 2x display keeps the weight a 1x display shows. Heavy is twice
    /// light.
    pub(crate) fn new(underline_em: Option<f32>, font_px: f32) -> Self {
        let em = underline_em
            .filter(|em| em.is_finite() && *em > 0.0)
            .unwrap_or(DEFAULT_UNDERLINE_EM);
        let px = em * font_px;
        // The tolerance keeps float noise in an exact product (2.0000002)
        // from adding a pixel.
        let light = if px.is_finite() {
            (px - 1.0e-3).ceil().clamp(1.0, MAX_LIGHT_STROKE) as u16
        } else {
            1
        };
        Self {
            light,
            heavy: light * 2,
        }
    }
}

impl Default for Strokes {
    fn default() -> Self {
        Self::new(None, 13.0)
    }
}

/// The first device pixel whose centre lies at or past `x`.
///
/// The rasteriser fills a pixel when its centre is inside a quad, so a
/// solid cell background whose edge is at `x` starts on this pixel. A glyph
/// spanning `pixel_edge(left)..pixel_edge(right)` covers exactly the pixels
/// its background does, and neighbouring cells share their boundary pixel
/// edge, so they tile with no gap and no overlap at any fractional cell size.
pub(crate) fn pixel_edge(x: f32) -> i32 {
    (x - 0.5).ceil() as i32
}

/// What one rasterized mask depends on: the glyph, its cell in device
/// pixels and the stroke widths. The scale is folded into all three.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BuiltinKey {
    pub(crate) ch: char,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) strokes: Strokes,
}

impl BuiltinKey {
    /// The key for `ch` in a `width` x `height` cell, or `None` when the cell
    /// is empty or too large to describe.
    pub(crate) fn new(ch: char, width: i32, height: i32, strokes: Strokes) -> Option<Self> {
        let width = u16::try_from(width).ok().filter(|w| *w > 0)?;
        let height = u16::try_from(height).ok().filter(|h| *h > 0)?;
        Some(Self {
            ch,
            width,
            height,
            strokes,
        })
    }
}

/// A cell Kettle draws itself this frame, recorded while its pane's cells
/// are walked and turned into a glyph instance when the pane's glyphs are
/// emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BuiltinCell {
    pub(crate) ch: char,
    /// The device pixels it fills, `[x0, y0, x1, y1)`.
    pub(crate) rect: [i32; 4],
    /// sRGB, the colour its text would have been drawn in.
    pub(crate) color: [u8; 3],
}

impl BuiltinCell {
    /// `ch` in the cell whose edges are at `left`, `top`, `right` and
    /// `bottom` surface pixels, snapped as its background is.
    pub(crate) fn new(
        ch: char,
        (left, top, right, bottom): (f32, f32, f32, f32),
        color: kettle_config::Rgb,
    ) -> Self {
        Self {
            ch,
            rect: [
                pixel_edge(left),
                pixel_edge(top),
                pixel_edge(right),
                pixel_edge(bottom),
            ],
            color: [color.r, color.g, color.b],
        }
    }

    pub(crate) fn key(&self, strokes: Strokes) -> Option<BuiltinKey> {
        let [x0, y0, x1, y1] = self.rect;
        BuiltinKey::new(self.ch, x1 - x0, y1 - y0, strokes)
    }
}

/// Draw `key` into `mask`: one coverage byte per pixel, row by row. Returns
/// `false` when `key.ch` is not built in or draws nothing (U+2800, the blank
/// braille pattern), leaving `mask` empty. `mask` is cleared and resized, so
/// a caller can keep one buffer for every miss.
pub(crate) fn rasterize(key: BuiltinKey, mask: &mut Vec<u8>) -> bool {
    let (w, h) = (i32::from(key.width), i32::from(key.height));
    mask.clear();
    mask.resize(usize::from(key.width) * usize::from(key.height), 0);
    let mut canvas = Canvas { w, h, px: mask };
    let s = key.strokes;
    let drawn = match key.ch {
        '\u{2500}'..='\u{257F}' => box_drawing(&mut canvas, key.ch, s),
        '\u{2580}'..='\u{259F}' => block(&mut canvas, key.ch),
        '\u{25E2}'..='\u{25E5}' => {
            canvas.fill_convex(&right_triangle(key.ch, w as f32, h as f32));
            true
        }
        '\u{2800}'..='\u{28FF}' => {
            braille(&mut canvas, key.ch as u32 - 0x2800);
            true
        }
        '\u{E0B0}'..='\u{E0BF}' => powerline(&mut canvas, key.ch, s),
        _ => false,
    };
    if !drawn || mask.iter().all(|&b| b == 0) {
        mask.clear();
        return false;
    }
    true
}

/// A coverage mask being drawn. Shapes combine by maximum, so overlapping
/// strokes never brighten their overlap.
struct Canvas<'a> {
    w: i32,
    h: i32,
    px: &'a mut [u8],
}

impl Canvas<'_> {
    /// Fill pixels `x0..x1` by `y0..y1`, clipped to the cell.
    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.rect_alpha(x0, y0, x1, y1, u8::MAX);
    }

    fn rect_alpha(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8) {
        let (x0, x1) = (x0.clamp(0, self.w), x1.clamp(0, self.w));
        let (y0, y1) = (y0.clamp(0, self.h), y1.clamp(0, self.h));
        if x0 >= x1 {
            return;
        }
        for y in y0..y1 {
            let row = (y * self.w) as usize;
            for b in &mut self.px[row + x0 as usize..row + x1 as usize] {
                *b = (*b).max(alpha);
            }
        }
    }

    /// Give every pixel the coverage `f` returns at its centre.
    fn shade(&mut self, f: impl Fn(f32, f32) -> f32) {
        for y in 0..self.h {
            for x in 0..self.w {
                let coverage = f(x as f32 + 0.5, y as f32 + 0.5);
                let alpha = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
                let b = &mut self.px[(y * self.w + x) as usize];
                *b = (*b).max(alpha);
            }
        }
    }

    /// Fill the region where `inside` holds, antialiased by an 8x8 grid of
    /// samples in each pixel. No sample lies on a pixel edge, so a shape
    /// whose edge runs along the cell boundary covers its edge pixels fully.
    fn fill(&mut self, inside: impl Fn(f32, f32) -> bool) {
        const N: u32 = 8;
        for y in 0..self.h {
            for x in 0..self.w {
                let mut hits = 0u32;
                for j in 0..N {
                    let sy = y as f32 + (j as f32 + 0.5) / N as f32;
                    for i in 0..N {
                        let sx = x as f32 + (i as f32 + 0.5) / N as f32;
                        hits += u32::from(inside(sx, sy));
                    }
                }
                let alpha = ((hits * 255 + N * N / 2) / (N * N)) as u8;
                let b = &mut self.px[(y * self.w + x) as usize];
                *b = (*b).max(alpha);
            }
        }
    }

    /// Fill a convex polygon given in either winding.
    fn fill_convex(&mut self, points: &[(f32, f32)]) {
        let area: f32 = points
            .iter()
            .zip(points.iter().cycle().skip(1))
            .map(|(a, b)| a.0 * b.1 - b.0 * a.1)
            .sum();
        let sign = if area < 0.0 { -1.0 } else { 1.0 };
        self.fill(|x, y| {
            points
                .iter()
                .zip(points.iter().cycle().skip(1))
                .all(|(a, b)| sign * ((b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0)) >= 0.0)
        });
    }

    /// Stroke the polyline through `points`, `width` pixels wide.
    fn stroke(&mut self, points: &[(f32, f32)], width: f32) {
        self.shade(|x, y| {
            let distance = points
                .windows(2)
                .map(|pair| segment_distance((x, y), pair[0], pair[1]))
                .fold(f32::INFINITY, f32::min);
            stroke_coverage(distance, width)
        });
    }
}

/// Coverage of a pixel whose centre is `distance` from the middle of a
/// `width`-wide stroke. A straight stroke centred on a pixel boundary (an
/// even width) or a pixel centre (an odd width) comes out exactly as solid
/// pixels, which is what lets curves meet straight strokes seamlessly.
fn stroke_coverage(distance: f32, width: f32) -> f32 {
    (width / 2.0 + 0.5 - distance).clamp(0.0, 1.0)
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    let t = if length > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

/// `k / n` of `len`, rounded to a whole pixel.
fn split(len: i32, k: i32, n: i32) -> i32 {
    (len * k + n / 2) / n
}

/// The pixels a stroke occupies across a cell `len` pixels long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bands {
    pub(crate) light: (i32, i32),
    pub(crate) heavy: (i32, i32),
    /// The two strokes of a double line are light, either side of a light
    /// gap: `(light.0 - light width, light.0)` and `(light.1, light.1 +
    /// light width)`.
    pub(crate) light_width: i32,
}

/// Where light and heavy strokes sit across a cell `len` pixels long. Each
/// starts at `(len - width) / 2` in whole pixels, so the light stroke always
/// lies inside the heavy one and every cell of the same size agrees.
pub(crate) fn bands(len: i32, s: Strokes) -> Bands {
    let (t, heavy) = (i32::from(s.light), i32::from(s.heavy));
    let l0 = (len - t).max(0) / 2;
    let h0 = (len - heavy).max(0) / 2;
    Bands {
        light: (l0, l0 + t),
        heavy: (h0, h0 + heavy),
        light_width: t,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line {
    None,
    Light,
    Heavy,
    Double,
}

/// The four arms of a box-drawing character, from the centre to each edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Arms {
    up: Line,
    right: Line,
    down: Line,
    left: Line,
}

const fn arms(up: Line, right: Line, down: Line, left: Line) -> Arms {
    Arms {
        up,
        right,
        down,
        left,
    }
}

/// The arms of every U+2500..U+257F character drawn from straight lines,
/// listed as (up, right, down, left).
fn box_arms(ch: char) -> Option<Arms> {
    use Line::{Double as D, Heavy as H, Light as L, None as N};
    Some(match ch {
        '─' => arms(N, L, N, L),
        '━' => arms(N, H, N, H),
        '│' => arms(L, N, L, N),
        '┃' => arms(H, N, H, N),
        '┌' => arms(N, L, L, N),
        '┍' => arms(N, H, L, N),
        '┎' => arms(N, L, H, N),
        '┏' => arms(N, H, H, N),
        '┐' => arms(N, N, L, L),
        '┑' => arms(N, N, L, H),
        '┒' => arms(N, N, H, L),
        '┓' => arms(N, N, H, H),
        '└' => arms(L, L, N, N),
        '┕' => arms(L, H, N, N),
        '┖' => arms(H, L, N, N),
        '┗' => arms(H, H, N, N),
        '┘' => arms(L, N, N, L),
        '┙' => arms(L, N, N, H),
        '┚' => arms(H, N, N, L),
        '┛' => arms(H, N, N, H),
        '├' => arms(L, L, L, N),
        '┝' => arms(L, H, L, N),
        '┞' => arms(H, L, L, N),
        '┟' => arms(L, L, H, N),
        '┠' => arms(H, L, H, N),
        '┡' => arms(H, H, L, N),
        '┢' => arms(L, H, H, N),
        '┣' => arms(H, H, H, N),
        '┤' => arms(L, N, L, L),
        '┥' => arms(L, N, L, H),
        '┦' => arms(H, N, L, L),
        '┧' => arms(L, N, H, L),
        '┨' => arms(H, N, H, L),
        '┩' => arms(H, N, L, H),
        '┪' => arms(L, N, H, H),
        '┫' => arms(H, N, H, H),
        '┬' => arms(N, L, L, L),
        '┭' => arms(N, L, L, H),
        '┮' => arms(N, H, L, L),
        '┯' => arms(N, H, L, H),
        '┰' => arms(N, L, H, L),
        '┱' => arms(N, L, H, H),
        '┲' => arms(N, H, H, L),
        '┳' => arms(N, H, H, H),
        '┴' => arms(L, L, N, L),
        '┵' => arms(L, L, N, H),
        '┶' => arms(L, H, N, L),
        '┷' => arms(L, H, N, H),
        '┸' => arms(H, L, N, L),
        '┹' => arms(H, L, N, H),
        '┺' => arms(H, H, N, L),
        '┻' => arms(H, H, N, H),
        '┼' => arms(L, L, L, L),
        '┽' => arms(L, L, L, H),
        '┾' => arms(L, H, L, L),
        '┿' => arms(L, H, L, H),
        '╀' => arms(H, L, L, L),
        '╁' => arms(L, L, H, L),
        '╂' => arms(H, L, H, L),
        '╃' => arms(H, L, L, H),
        '╄' => arms(H, H, L, L),
        '╅' => arms(L, L, H, H),
        '╆' => arms(L, H, H, L),
        '╇' => arms(H, H, L, H),
        '╈' => arms(L, H, H, H),
        '╉' => arms(H, L, H, H),
        '╊' => arms(H, H, H, L),
        '╋' => arms(H, H, H, H),
        '═' => arms(N, D, N, D),
        '║' => arms(D, N, D, N),
        '╒' => arms(N, D, L, N),
        '╓' => arms(N, L, D, N),
        '╔' => arms(N, D, D, N),
        '╕' => arms(N, N, L, D),
        '╖' => arms(N, N, D, L),
        '╗' => arms(N, N, D, D),
        '╘' => arms(L, D, N, N),
        '╙' => arms(D, L, N, N),
        '╚' => arms(D, D, N, N),
        '╛' => arms(L, N, N, D),
        '╜' => arms(D, N, N, L),
        '╝' => arms(D, N, N, D),
        '╞' => arms(L, D, L, N),
        '╟' => arms(D, L, D, N),
        '╠' => arms(D, D, D, N),
        '╡' => arms(L, N, L, D),
        '╢' => arms(D, N, D, L),
        '╣' => arms(D, N, D, D),
        '╤' => arms(N, D, L, D),
        '╥' => arms(N, L, D, L),
        '╦' => arms(N, D, D, D),
        '╧' => arms(L, D, N, D),
        '╨' => arms(D, L, N, L),
        '╩' => arms(D, D, N, D),
        '╪' => arms(L, D, L, D),
        '╫' => arms(D, L, D, L),
        '╬' => arms(D, D, D, D),
        '╴' => arms(N, N, N, L),
        '╵' => arms(L, N, N, N),
        '╶' => arms(N, L, N, N),
        '╷' => arms(N, N, L, N),
        '╸' => arms(N, N, N, H),
        '╹' => arms(H, N, N, N),
        '╺' => arms(N, H, N, N),
        '╻' => arms(N, N, H, N),
        '╼' => arms(N, H, N, L),
        '╽' => arms(L, N, H, N),
        '╾' => arms(N, L, N, H),
        '╿' => arms(H, N, L, N),
        _ => return None,
    })
}

fn box_drawing(c: &mut Canvas, ch: char, s: Strokes) -> bool {
    if let Some(a) = box_arms(ch) {
        draw_arms(c, a, s);
        return true;
    }
    // (horizontal, heavy, dashes)
    let dashed = match ch {
        '┄' => Some((true, false, 3)),
        '┅' => Some((true, true, 3)),
        '┆' => Some((false, false, 3)),
        '┇' => Some((false, true, 3)),
        '┈' => Some((true, false, 4)),
        '┉' => Some((true, true, 4)),
        '┊' => Some((false, false, 4)),
        '┋' => Some((false, true, 4)),
        '╌' => Some((true, false, 2)),
        '╍' => Some((true, true, 2)),
        '╎' => Some((false, false, 2)),
        '╏' => Some((false, true, 2)),
        _ => None,
    };
    if let Some((horizontal, heavy, count)) = dashed {
        dashes(c, horizontal, heavy, count, s);
        return true;
    }
    let (w, h) = (c.w as f32, c.h as f32);
    let t = f32::from(s.light);
    match ch {
        // Arcs, named by the two arms they join: (right, down) is +1.
        '╭' => arc(c, 1.0, 1.0, s),
        '╮' => arc(c, -1.0, 1.0, s),
        '╯' => arc(c, -1.0, -1.0, s),
        '╰' => arc(c, 1.0, -1.0, s),
        // Corner to corner, so diagonals in neighbouring cells continue.
        '╱' => c.stroke(&[(w, 0.0), (0.0, h)], t),
        '╲' => c.stroke(&[(0.0, 0.0), (w, h)], t),
        '╳' => {
            c.stroke(&[(w, 0.0), (0.0, h)], t);
            c.stroke(&[(0.0, 0.0), (w, h)], t);
        }
        _ => return false,
    }
    true
}

/// Straight box lines. Each arm runs from its edge into the centre. An arm
/// that turns a corner, or crosses or continues past a perpendicular line,
/// reaches the far side of that line, so the corner is square and the
/// crossing solid. An arm that only meets a line passing straight through
/// stops at its near edge. Double lines are two light strokes with a light
/// gap; where two double arms meet, the inner strokes stop at each other to
/// leave the gap open.
fn draw_arms(c: &mut Canvas, a: Arms, s: Strokes) {
    use Line::{Double, Heavy, Light, None};
    let (w, h) = (c.w, c.h);
    let rows = bands(h, s);
    let cols = bands(w, s);
    let t = rows.light_width;
    let (hl0, hl1) = rows.light;
    let (hh0, hh1) = rows.heavy;
    let (vl0, vl1) = cols.light;
    let (vh0, vh1) = cols.heavy;

    let across_heavy = a.left == Heavy || a.right == Heavy;
    let across_double = a.left == Double || a.right == Double;
    let along_heavy = a.up == Heavy || a.down == Heavy;
    let along_double = a.up == Double || a.down == Double;
    let turns_or_crosses = a.left != a.right || a.up == a.down;
    let crosses_or_turns = a.up != a.down || a.left == a.right;

    // How far the vertical arms reach into the horizontal line's rows.
    let up_end = if across_heavy {
        hh1
    } else if turns_or_crosses {
        if across_double { hl1 + t } else { hl1 }
    } else if a.left == None {
        hl1
    } else {
        hl0
    };
    let down_start = if across_heavy {
        hh0
    } else if turns_or_crosses {
        if across_double { hl0 - t } else { hl0 }
    } else if a.left == None {
        hl0
    } else {
        hl1
    };
    // How far the horizontal arms reach into the vertical line's columns.
    let left_end = if along_heavy {
        vh1
    } else if crosses_or_turns {
        if along_double { vl1 + t } else { vl1 }
    } else if a.up == None {
        vl1
    } else {
        vl0
    };
    let right_start = if along_heavy {
        vh0
    } else if crosses_or_turns {
        if along_double { vl0 - t } else { vl0 }
    } else if a.up == None {
        vl0
    } else {
        vl1
    };

    match a.up {
        None => {}
        Light => c.rect(vl0, 0, vl1, up_end),
        Heavy => c.rect(vh0, 0, vh1, up_end),
        Double => {
            let left = if a.left == Double { hl0 } else { up_end };
            let right = if a.right == Double { hl0 } else { up_end };
            c.rect(vl0 - t, 0, vl0, left);
            c.rect(vl1, 0, vl1 + t, right);
        }
    }
    match a.down {
        None => {}
        Light => c.rect(vl0, down_start, vl1, h),
        Heavy => c.rect(vh0, down_start, vh1, h),
        Double => {
            let left = if a.left == Double { hl1 } else { down_start };
            let right = if a.right == Double { hl1 } else { down_start };
            c.rect(vl0 - t, left, vl0, h);
            c.rect(vl1, right, vl1 + t, h);
        }
    }
    match a.left {
        None => {}
        Light => c.rect(0, hl0, left_end, hl1),
        Heavy => c.rect(0, hh0, left_end, hh1),
        Double => {
            let top = if a.up == Double { vl0 } else { left_end };
            let bottom = if a.down == Double { vl0 } else { left_end };
            c.rect(0, hl0 - t, top, hl0);
            c.rect(0, hl1, bottom, hl1 + t);
        }
    }
    match a.right {
        None => {}
        Light => c.rect(right_start, hl0, w, hl1),
        Heavy => c.rect(right_start, hh0, w, hh1),
        Double => {
            let top = if a.up == Double { vl1 } else { right_start };
            let bottom = if a.down == Double { vl1 } else { right_start };
            c.rect(top, hl0 - t, w, hl0);
            c.rect(bottom, hl1, w, hl1 + t);
        }
    }
}

/// `count` dashes along the cell, with a whole-pixel gap after each. Half a
/// gap sits at each end, so a run of cells shows one evenly dashed line.
fn dashes(c: &mut Canvas, horizontal: bool, heavy: bool, count: i32, s: Strokes) {
    let (len, across) = if horizontal { (c.w, c.h) } else { (c.h, c.w) };
    let b = bands(across, s);
    let (a0, a1) = if heavy { b.heavy } else { b.light };
    let gap = (len / (2 * count)).min(i32::from(s.light).max(4)).max(1);
    let ink = len - gap * count;
    if ink < count {
        // Too small to dash: a solid line still reads as the line.
        if horizontal {
            c.rect(0, a0, len, a1);
        } else {
            c.rect(a0, 0, a1, len);
        }
        return;
    }
    let (dash, mut extra) = (ink / count, ink % count);
    let mut at = gap / 2;
    for _ in 0..count {
        let mut d = dash;
        if extra > 0 {
            extra -= 1;
            d += 1;
        }
        if horizontal {
            c.rect(at, a0, at + d, a1);
        } else {
            c.rect(a0, at, a1, at + d);
        }
        at += d + gap;
    }
}

/// A light quarter circle joining the arm toward `sx` (+1 right, -1 left)
/// to the arm toward `sy` (+1 down, -1 up), the rest of each arm straight.
///
/// Both arms are light lines centred where `─` and `│` are. The radius is
/// the largest that keeps each tangent point on a pixel centre inside the
/// cell, where the antialiased curve has exactly the straight stroke's
/// coverage, so the arc meets the next cell's line with no notch.
fn arc(c: &mut Canvas, sx: f32, sy: f32, s: Strokes) {
    let rows = bands(c.h, s).light;
    let cols = bands(c.w, s).light;
    let t = f32::from(s.light);
    let (w, h) = (c.w as f32, c.h as f32);
    let xc = cols.0 as f32 + t / 2.0;
    let yc = rows.0 as f32 + t / 2.0;
    let r = (xc - 0.5)
        .min(w - 0.5 - xc)
        .min(yc - 0.5)
        .min(h - 0.5 - yc)
        .max(0.0);
    let (cx, cy) = (xc + sx * r, yc + sy * r);
    let (col0, col1) = (cols.0 as f32, cols.1 as f32);
    let (row0, row1) = (rows.0 as f32, rows.1 as f32);
    c.shade(|x, y| {
        let past_x = (x - cx) * sx > 0.0;
        let past_y = (y - cy) * sy > 0.0;
        match (past_x, past_y) {
            // The curve itself, inside its quadrant.
            (false, false) => stroke_coverage(((x - cx).hypot(y - cy) - r).abs(), t),
            // The straight vertical arm beyond the curve.
            (false, true) => f32::from(col0 < x && x < col1),
            // The straight horizontal arm beyond the curve.
            (true, false) => f32::from(row0 < y && y < row1),
            (true, true) => 0.0,
        }
    });
}

fn block(c: &mut Canvas, ch: char) -> bool {
    let (w, h) = (c.w, c.h);
    // Every fraction ends on the same rounded eighth, so complementary
    // blocks (▀ and ▄, ▌ and ▐) tile their cell exactly. A part keeps at
    // least one pixel.
    let top_of_lower = |k: i32| split(h, 8 - k, 8).min(h - 1);
    let right_of_left = |k: i32| split(w, k, 8).max(1);
    let code = ch as i32;
    match ch {
        '▀' => c.rect(0, 0, w, split(h, 1, 2)),
        // Lower one eighth (U+2581) to lower seven eighths (U+2587).
        '▁'..='▇' => c.rect(0, top_of_lower(code - 0x2580), w, h),
        '█' => c.rect(0, 0, w, h),
        // Left seven eighths (U+2589) to left one eighth (U+258F).
        '▉'..='▏' => c.rect(0, 0, right_of_left(0x2590 - code), h),
        '▐' => c.rect(split(w, 1, 2), 0, w, h),
        // Shades are an even tone, not a dither.
        '░' => c.rect_alpha(0, 0, w, h, 64),
        '▒' => c.rect_alpha(0, 0, w, h, 128),
        '▓' => c.rect_alpha(0, 0, w, h, 191),
        '▔' => c.rect(0, 0, w, split(h, 1, 8).max(1)),
        '▕' => c.rect(split(w, 7, 8).min(w - 1), 0, w, h),
        _ => {
            // Quadrants: upper left 1, upper right 2, lower left 4, lower right 8.
            let quadrants = match ch {
                '▖' => 4,
                '▗' => 8,
                '▘' => 1,
                '▙' => 1 | 4 | 8,
                '▚' => 1 | 8,
                '▛' => 1 | 2 | 4,
                '▜' => 1 | 2 | 8,
                '▝' => 2,
                '▞' => 2 | 4,
                '▟' => 2 | 4 | 8,
                _ => return false,
            };
            let (mx, my) = (split(w, 1, 2), split(h, 1, 2));
            for (bit, (x0, y0, x1, y1)) in [
                (1, (0, 0, mx, my)),
                (2, (mx, 0, w, my)),
                (4, (0, my, mx, h)),
                (8, (mx, my, w, h)),
            ] {
                if quadrants & bit != 0 {
                    c.rect(x0, y0, x1, y1);
                }
            }
        }
    }
    true
}

/// The corners of the right triangle `◢ ◣ ◤ ◥` (and their Powerline twins)
/// that fills half the cell.
fn right_triangle(ch: char, w: f32, h: f32) -> [(f32, f32); 3] {
    match ch {
        // Lower right.
        '◢' | '\u{E0BA}' => [(w, 0.0), (w, h), (0.0, h)],
        // Lower left.
        '◣' | '\u{E0B8}' => [(0.0, 0.0), (0.0, h), (w, h)],
        // Upper left.
        '◤' | '\u{E0BC}' => [(0.0, 0.0), (w, 0.0), (0.0, h)],
        // Upper right.
        _ => [(0.0, 0.0), (w, 0.0), (w, h)],
    }
}

/// Powerline separators. Solid shapes span the full cell height with their
/// flat side on the cell edge, so they meet the neighbouring background with
/// no seam; the thin ones are light strokes along the same outlines.
fn powerline(c: &mut Canvas, ch: char, s: Strokes) -> bool {
    let (w, h) = (c.w as f32, c.h as f32);
    let t = f32::from(s.light);
    let mid = h / 2.0;
    match ch {
        '\u{E0B0}' => c.fill_convex(&[(0.0, 0.0), (w, mid), (0.0, h)]),
        '\u{E0B1}' => c.stroke(&[(0.0, 0.0), (w, mid), (0.0, h)], t),
        '\u{E0B2}' => c.fill_convex(&[(w, 0.0), (0.0, mid), (w, h)]),
        '\u{E0B3}' => c.stroke(&[(w, 0.0), (0.0, mid), (w, h)], t),
        // Half ellipses: the right half of one centred on the left edge, and
        // the left half of one centred on the right edge.
        '\u{E0B4}' => half_ellipse(c, 0.0, None),
        '\u{E0B5}' => half_ellipse(c, 0.0, Some(t)),
        '\u{E0B6}' => half_ellipse(c, w, None),
        '\u{E0B7}' => half_ellipse(c, w, Some(t)),
        '\u{E0B8}' | '\u{E0BA}' | '\u{E0BC}' | '\u{E0BE}' => {
            c.fill_convex(&right_triangle(ch, w, h));
        }
        // The slashes along those triangles' long sides.
        '\u{E0B9}' | '\u{E0BF}' => c.stroke(&[(0.0, 0.0), (w, h)], t),
        '\u{E0BB}' | '\u{E0BD}' => c.stroke(&[(0.0, h), (w, 0.0)], t),
        _ => return false,
    }
    true
}

/// The half of an ellipse spanning the cell's height, centred on its edge at
/// `cx`: filled, or stroked `stroke` pixels wide.
fn half_ellipse(c: &mut Canvas, cx: f32, stroke: Option<f32>) {
    let (a, b) = (c.w as f32, c.h as f32 / 2.0);
    let cy = b;
    match stroke {
        None => c.fill(|x, y| {
            let (u, v) = ((x - cx) / a, (y - cy) / b);
            u * u + v * v <= 1.0
        }),
        Some(width) => c.shade(|x, y| {
            let (dx, dy) = (x - cx, y - cy);
            // The distance to the curve, to first order: the implicit
            // function over its gradient.
            let f = dx * dx / (a * a) + dy * dy / (b * b) - 1.0;
            let gx = 2.0 * dx / (a * a);
            let gy = 2.0 * dy / (b * b);
            let gradient = gx.hypot(gy);
            let distance = if gradient > 0.0 {
                (f / gradient).abs()
            } else {
                f32::INFINITY
            };
            stroke_coverage(distance, width)
        }),
    }
}

/// Braille: eight square dots on a 2 x 4 grid at the centres of the cell's
/// quarters and eighths, so the spacing is even within a cell and across
/// neighbouring cells. `bits` follows Unicode: dots 1-3 down the left
/// column, 4-6 down the right, then 7 and 8 on the bottom row.
fn braille(c: &mut Canvas, bits: u32) {
    let (w, h) = (c.w, c.h);
    let dot = (w / 4).min(h / 8).max(1);
    let half = dot as f32 / 2.0;
    let column = |i: i32| ((w * (2 * i + 1)) as f32 / 4.0 - half).round() as i32;
    let row = |j: i32| ((h * (2 * j + 1)) as f32 / 8.0 - half).round() as i32;
    for (bit, (i, j)) in [
        (0, (0, 0)),
        (1, (0, 1)),
        (2, (0, 2)),
        (3, (1, 0)),
        (4, (1, 1)),
        (5, (1, 2)),
        (6, (0, 3)),
        (7, (1, 3)),
    ] {
        if bits & (1 << bit) != 0 {
            let (x, y) = (column(i), row(j));
            c.rect(x, y, x + dot, y + dot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(ch: char, w: i32, h: i32, s: Strokes) -> Vec<u8> {
        let mut out = Vec::new();
        assert!(
            rasterize(BuiltinKey::new(ch, w, h, s).unwrap(), &mut out),
            "{ch:?} draws"
        );
        assert_eq!(out.len(), (w * h) as usize);
        out
    }

    fn at(m: &[u8], w: i32, x: i32, y: i32) -> u8 {
        m[(y * w + x) as usize]
    }

    /// The columns with any ink in row `y`.
    fn inked_columns(m: &[u8], w: i32, y: i32) -> Vec<i32> {
        (0..w).filter(|&x| at(m, w, x, y) > 0).collect()
    }

    /// The rows with any ink in column `x`.
    fn inked_rows(m: &[u8], w: i32, h: i32, x: i32) -> Vec<i32> {
        (0..h).filter(|&y| at(m, w, x, y) > 0).collect()
    }

    #[test]
    fn stroke_widths_follow_the_underline_at_1x_and_2x() {
        // JetBrains Mono's underline is 50/1000 em.
        let em = Some(0.05);
        // Kettle's default 13 pt at 1x and 2x, and 14 pt.
        assert_eq!(Strokes::new(em, 13.0), Strokes { light: 1, heavy: 2 });
        assert_eq!(Strokes::new(em, 26.0), Strokes { light: 2, heavy: 4 });
        assert_eq!(Strokes::new(em, 14.0), Strokes { light: 1, heavy: 2 });
        assert_eq!(Strokes::new(em, 28.0), Strokes { light: 2, heavy: 4 });
        // An exact product does not gain a pixel from float noise.
        assert_eq!(Strokes::new(Some(0.1), 20.0).light, 2);
        // At least one device pixel, however small the face or its metric.
        assert_eq!(Strokes::new(Some(0.001), 5.0).light, 1);
        // A face without underline metrics gets a sixteenth of an em.
        assert_eq!(Strokes::new(None, 16.0).light, 1);
        assert_eq!(Strokes::new(None, 48.0).light, 3);
        for bad in [Some(f32::NAN), Some(-1.0), Some(0.0)] {
            assert_eq!(Strokes::new(bad, 16.0), Strokes::new(None, 16.0));
        }
        assert_eq!(Strokes::new(em, f32::NAN).light, 1);
    }

    #[test]
    fn cells_snap_to_the_pixels_their_backgrounds_fill() {
        // A pixel belongs to a quad when its centre is inside, so an edge
        // at 10.4 starts on pixel 10 and one at 10.6 on pixel 11.
        assert_eq!(pixel_edge(10.4), 10);
        assert_eq!(pixel_edge(10.5), 10);
        assert_eq!(pixel_edge(10.6), 11);
        assert_eq!(pixel_edge(0.0), 0);
        // Cells of a fractional width tile with no gap and no overlap,
        // each one floor or ceil of the width, at 1x and 2x.
        for (origin, cell) in [
            (8.0, 7.8),
            (8.0, 8.4),
            (16.0, 15.6),
            (16.0, 16.8),
            (3.3, 9.5),
        ] {
            let mut edge = pixel_edge(origin);
            let mut widths = std::collections::BTreeSet::new();
            for k in 1..200 {
                let next = pixel_edge(origin + k as f32 * cell);
                widths.insert(next - edge);
                edge = next;
            }
            assert!(
                widths
                    .iter()
                    .all(|&w| w == cell.floor() as i32 || w == cell.ceil() as i32),
                "{cell}: {widths:?}"
            );
        }
    }

    #[test]
    fn strokes_are_centred_in_whole_pixels_at_1x_and_2x() {
        let one = Strokes::new(Some(0.05), 13.0);
        // An 8 px wide, 16 px tall cell at 1x.
        assert_eq!(bands(8, one).light, (3, 4));
        assert_eq!(bands(8, one).heavy, (3, 5));
        assert_eq!(bands(16, one).light, (7, 8));
        assert_eq!(bands(17, one).light, (8, 9));
        assert_eq!(bands(17, one).heavy, (7, 9));
        // The same cell at 2x.
        let two = Strokes::new(Some(0.05), 26.0);
        assert_eq!(bands(16, two).light, (7, 9));
        assert_eq!(bands(16, two).heavy, (6, 10));
        assert_eq!(bands(33, two).light, (15, 17));
        assert_eq!(bands(33, two).heavy, (14, 18));
        // The light stroke always lies inside the heavy one.
        for len in 4..80 {
            for s in [one, two, Strokes::new(Some(0.05), 60.0)] {
                let b = bands(len, s);
                assert!(
                    b.heavy.0 <= b.light.0 && b.light.1 <= b.heavy.1,
                    "{len} {s:?}"
                );
            }
        }
    }

    #[test]
    fn every_covered_codepoint_draws_inside_its_cell() {
        let ranges = [
            '\u{2500}'..='\u{259F}',
            '\u{25E2}'..='\u{25E5}',
            '\u{2801}'..='\u{28FF}',
            '\u{E0B0}'..='\u{E0BF}',
        ];
        for s in [
            Strokes::new(Some(0.05), 13.0),
            Strokes::new(Some(0.05), 26.0),
        ] {
            for (w, h) in [(8, 17), (7, 15), (17, 34), (16, 33), (4, 6)] {
                for ch in ranges.iter().cloned().flatten() {
                    assert!(is_builtin(ch));
                    let m = mask(ch, w, h, s);
                    assert!(m.iter().any(|&b| b > 0), "{ch:?} at {w}x{h} draws nothing");
                }
            }
        }
        let mut out = vec![1];
        let blank = BuiltinKey::new('\u{2800}', 8, 16, Strokes::default()).unwrap();
        assert!(
            !rasterize(blank, &mut out),
            "the blank pattern draws nothing"
        );
        assert!(out.is_empty());
        let letter = BuiltinKey::new('a', 8, 16, Strokes::default()).unwrap();
        assert!(!rasterize(letter, &mut out));
        assert!(!is_builtin('a') && !is_builtin('\u{25E1}') && !is_builtin('\u{E0C0}'));
        assert!(BuiltinKey::new('─', 0, 16, Strokes::default()).is_none());
    }

    #[test]
    fn lines_are_solid_whole_pixels() {
        let s = Strokes::new(Some(0.05), 13.0);
        let (w, h) = (8, 17);
        let m = mask('─', w, h, s);
        for x in 0..w {
            assert_eq!(inked_rows(&m, w, h, x), [8], "─ column {x}");
            assert_eq!(at(&m, w, x, 8), 255);
        }
        let m = mask('│', w, h, s);
        for y in 0..h {
            assert_eq!(inked_columns(&m, w, y), [3], "│ row {y}");
        }
        let m = mask('━', w, h, s);
        for x in 0..w {
            assert_eq!(inked_rows(&m, w, h, x), [7, 8]);
        }
    }

    #[test]
    fn junctions_reach_every_edge_on_the_lines_of_their_arms() {
        let s = Strokes::new(Some(0.05), 26.0);
        let (w, h) = (17, 34);
        let rows = bands(h, s).light;
        let cols = bands(w, s).light;
        let light_rows: Vec<i32> = (rows.0..rows.1).collect();
        let light_cols: Vec<i32> = (cols.0..cols.1).collect();
        for ch in ['┼', '├', '┤', '┬', '┴', '┌', '┐', '└', '┘'] {
            let m = mask(ch, w, h, s);
            let a = box_arms(ch).unwrap();
            let edge = |present: bool, inked: Vec<i32>, line: &[i32]| {
                if present {
                    assert_eq!(inked, line, "{ch:?}");
                } else {
                    assert!(inked.is_empty(), "{ch:?}");
                }
            };
            edge(a.left != Line::None, inked_rows(&m, w, h, 0), &light_rows);
            edge(
                a.right != Line::None,
                inked_rows(&m, w, h, w - 1),
                &light_rows,
            );
            edge(a.up != Line::None, inked_columns(&m, w, 0), &light_cols);
            edge(
                a.down != Line::None,
                inked_columns(&m, w, h - 1),
                &light_cols,
            );
        }
    }

    #[test]
    fn double_lines_keep_an_even_gap_and_open_crossings() {
        for s in [
            Strokes::new(Some(0.05), 13.0),
            Strokes::new(Some(0.05), 26.0),
        ] {
            let t = i32::from(s.light);
            let (w, h) = (8 * t + 1, 17 * t);
            let rows = bands(h, s).light;
            let cols = bands(w, s).light;
            let m = mask('═', w, h, s);
            let expect: Vec<i32> = (rows.0 - t..rows.0).chain(rows.1..rows.1 + t).collect();
            for x in 0..w {
                assert_eq!(inked_rows(&m, w, h, x), expect, "═ {s:?}");
            }
            let m = mask('║', w, h, s);
            let expect: Vec<i32> = (cols.0 - t..cols.0).chain(cols.1..cols.1 + t).collect();
            for y in 0..h {
                assert_eq!(inked_columns(&m, w, y), expect, "║ {s:?}");
            }
            // ╬ is four corners: the gaps of both lines run through.
            let m = mask('╬', w, h, s);
            for x in cols.0..cols.1 {
                for y in 0..h {
                    assert_eq!(at(&m, w, x, y), 0, "╬ vertical gap at {x},{y}");
                }
            }
            for y in rows.0..rows.1 {
                for x in 0..w {
                    assert_eq!(at(&m, w, x, y), 0, "╬ horizontal gap at {x},{y}");
                }
            }
            // ╦ keeps its top stroke whole.
            let m = mask('╦', w, h, s);
            for x in 0..w {
                assert_eq!(at(&m, w, x, rows.0 - 1), 255);
            }
        }
    }

    #[test]
    fn arcs_meet_their_arms_on_the_straight_lines() {
        for s in [
            Strokes::new(Some(0.05), 13.0),
            Strokes::new(Some(0.05), 26.0),
        ] {
            for (w, h) in [(8, 17), (7, 16), (17, 34), (16, 33)] {
                let rows = bands(h, s).light;
                let cols = bands(w, s).light;
                let light_rows: Vec<i32> = (rows.0..rows.1).collect();
                let light_cols: Vec<i32> = (cols.0..cols.1).collect();
                for (ch, right, down) in [
                    ('╭', true, true),
                    ('╮', false, true),
                    ('╯', false, false),
                    ('╰', true, false),
                ] {
                    let m = mask(ch, w, h, s);
                    let side = if right { w - 1 } else { 0 };
                    let end = if down { h - 1 } else { 0 };
                    let solid_rows: Vec<i32> =
                        (0..h).filter(|&y| at(&m, w, side, y) == 255).collect();
                    assert_eq!(inked_rows(&m, w, h, side), light_rows, "{ch:?} {w}x{h}");
                    assert_eq!(solid_rows, light_rows, "{ch:?} {w}x{h} edge coverage");
                    let solid_cols: Vec<i32> =
                        (0..w).filter(|&x| at(&m, w, x, end) == 255).collect();
                    assert_eq!(inked_columns(&m, w, end), light_cols, "{ch:?} {w}x{h}");
                    assert_eq!(solid_cols, light_cols, "{ch:?} {w}x{h} edge coverage");
                    // The far edges stay clear: an arc is not a corner.
                    let other = if right { 0 } else { w - 1 };
                    assert!(inked_rows(&m, w, h, other).is_empty(), "{ch:?}");
                    // And it is antialiased.
                    assert!(m.iter().any(|&b| b > 0 && b < 255), "{ch:?} {w}x{h}");
                }
            }
        }
    }

    #[test]
    fn dashes_tile_with_even_gaps() {
        let s = Strokes::new(Some(0.05), 13.0);
        for (ch, count) in [('┄', 3), ('┈', 4), ('╌', 2)] {
            for w in [8, 9, 16, 17] {
                let m = mask(ch, w, 17, s);
                let row = bands(17, s).light.0;
                let lit: Vec<bool> = (0..w).map(|x| at(&m, w, x, row) > 0).collect();
                // Count dashes in two cells side by side, and the gaps
                // between them, including across the boundary.
                let pair: Vec<bool> = lit.iter().chain(lit.iter()).copied().collect();
                let starts =
                    pair.windows(2).filter(|p| !p[0] && p[1]).count() + usize::from(pair[0]);
                assert_eq!(starts, 2 * count, "{ch:?} at {w}");
                // The leading gap is only part of one.
                let first = pair.iter().position(|&on| on).unwrap();
                let mut gaps = Vec::new();
                let mut run = 0;
                for &on in &pair[first..] {
                    if on {
                        if run > 0 {
                            gaps.push(run);
                        }
                        run = 0;
                    } else {
                        run += 1;
                    }
                }
                assert!(
                    gaps.iter().all(|&g| g == gaps[0]),
                    "{ch:?} at {w}: gaps {gaps:?}"
                );
            }
        }
    }

    #[test]
    fn blocks_tile_and_shades_are_even_tones() {
        let (w, h) = (9, 17);
        let s = Strokes::default();
        assert!(mask('█', w, h, s).iter().all(|&b| b == 255));
        let upper = mask('▀', w, h, s);
        let lower = mask('▄', w, h, s);
        for (u, l) in upper.iter().zip(&lower) {
            assert_eq!(u16::from(*u) + u16::from(*l), 255, "▀ and ▄ tile the cell");
        }
        let left = mask('▌', w, h, s);
        let right = mask('▐', w, h, s);
        for (a, b) in left.iter().zip(&right) {
            assert_eq!(u16::from(*a) + u16::from(*b), 255, "▌ and ▐ tile the cell");
        }
        let mut levels = Vec::new();
        for ch in ['░', '▒', '▓'] {
            let m = mask(ch, w, h, s);
            assert!(m.iter().all(|&b| b == m[0]), "{ch:?} is one tone");
            levels.push(m[0]);
        }
        assert_eq!(levels, [64, 128, 191]);
        // The eighths grow by about an eighth each.
        for k in 1..8 {
            let m = mask(char::from_u32(0x2580 + k).unwrap(), w, h, s);
            let rows = inked_rows(&m, w, h, 0);
            assert_eq!(rows.last(), Some(&(h - 1)));
            assert_eq!(rows.len() as i32, h - split(h, 8 - k as i32, 8));
        }
    }

    #[test]
    fn solid_separators_fill_the_cell_height_and_their_flat_edge() {
        let s = Strokes::default();
        for (w, h) in [(8, 17), (8, 22), (17, 34)] {
            // The rows at each end where a slope crosses the flat edge.
            let slope = (h as f32 / 2.0 / w as f32).ceil() as i32;
            for (ch, flat) in [('\u{E0B0}', 0), ('\u{E0B2}', w - 1)] {
                let m = mask(ch, w, h, s);
                // The flat edge is solid between the slopes, and the shape
                // reaches the first and last rows.
                for y in slope..h - slope {
                    assert_eq!(at(&m, w, flat, y), 255, "{ch:?} {w}x{h} row {y}");
                }
                assert!(at(&m, w, flat, 0) > 0 && at(&m, w, flat, h - 1) > 0);
                // The point reaches the far edge.
                let tip = w - 1 - flat;
                assert!(at(&m, w, tip, h / 2) > 0, "{ch:?} {w}x{h} tip");
            }
        }
    }

    #[test]
    fn triangles_fill_the_named_half_of_the_cell() {
        let s = Strokes::default();
        let (w, h) = (8, 17);
        // (glyph, its Powerline twin, the corner it fills, the one it leaves)
        for (ch, twin, on, off) in [
            ('◢', '\u{E0BA}', (w - 1, h - 1), (0, 0)),
            ('◣', '\u{E0B8}', (0, h - 1), (w - 1, 0)),
            ('◤', '\u{E0BC}', (0, 0), (w - 1, h - 1)),
            ('◥', '\u{E0BE}', (w - 1, 0), (0, h - 1)),
        ] {
            let m = mask(ch, w, h, s);
            assert_eq!(m, mask(twin, w, h, s), "{ch:?} and {twin:?}");
            assert_eq!(at(&m, w, on.0, on.1), 255, "{ch:?}");
            assert_eq!(at(&m, w, off.0, off.1), 0, "{ch:?}");
            let half = m.iter().map(|&b| u32::from(b)).sum::<u32>() as f32 / (255 * w * h) as f32;
            assert!((half - 0.5).abs() < 0.02, "{ch:?} covers {half}");
        }
    }

    #[test]
    fn braille_dots_are_evenly_spaced_across_cells() {
        let s = Strokes::default();
        let (w, h) = (8, 16);
        let m = mask('\u{28FF}', w, h, s);
        let cols = inked_columns(&m, w, inked_rows(&m, w, h, 1)[0]);
        assert_eq!(cols, [1, 2, 5, 6]);
        // From the right dot to the next cell's left dot is the same step.
        assert_eq!(5 - 1, w + 1 - 5);
        let rows = inked_rows(&m, w, h, 1);
        assert_eq!(rows, [1, 2, 5, 6, 9, 10, 13, 14]);
        let one = mask('\u{2801}', w, h, s);
        assert_eq!(one.iter().filter(|&&b| b > 0).count(), 4, "dot 1 alone");
    }
}
