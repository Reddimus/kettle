//! Minimal Sixel (DEC VT3xx) decoder — enough for `img2sixel`, `chafa -f
//! sixel`, libsixel and friends. Modelled on the Contour state machine
//! (`contour/src/vtbackend/SixelParser.cpp`) and vt100.net chapter 14.

use crate::image::ImageData;

const MAX_DIM: usize = 8192;

#[derive(Clone, Copy)]
struct Rgb(u8, u8, u8);

fn default_palette() -> Vec<Rgb> {
    // VT340 16-color defaults (percent-based, scaled to 0..255).
    let p = |r: u32, g: u32, b: u32| {
        Rgb(
            (r * 255 / 100) as u8,
            (g * 255 / 100) as u8,
            (b * 255 / 100) as u8,
        )
    };
    let mut v = vec![
        p(0, 0, 0),
        p(20, 20, 80),
        p(80, 13, 13),
        p(20, 80, 20),
        p(80, 20, 80),
        p(20, 80, 80),
        p(80, 80, 20),
        p(53, 53, 53),
        p(26, 26, 26),
        p(33, 33, 60),
        p(60, 26, 26),
        p(33, 60, 33),
        p(60, 33, 60),
        p(33, 60, 60),
        p(60, 60, 33),
        p(80, 80, 80),
    ];
    v.resize(256, p(0, 0, 0));
    v
}

fn hls_to_rgb(h: f32, l: f32, s: f32) -> Rgb {
    let l = l / 100.0;
    let s = s / 100.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h % 360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    Rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

/// Geometric capacity growth: the smallest power-of-two multiple of `cur`
/// (floored at 1) that is `>= needed`, never exceeding `MAX_DIM` (callers
/// guarantee `needed <= MAX_DIM`). Cycle 824 (audit): doubling the allocation
/// makes total re-layout work across a decode amortized **O(W·H)** instead of
/// the old exact-fit regrow's **O(W²·H)**.
fn grow_cap(cur: usize, needed: usize) -> usize {
    let mut c = cur.max(1);
    while c < needed {
        c *= 2;
    }
    c.min(MAX_DIM).max(needed)
}

/// A Sixel raster being decoded. The **allocated** stride/rows (`cap_w`/`cap_h`)
/// are decoupled from the **logical** extent (`width`/`height`): capacity grows
/// geometrically (cheap, rare), while the extent tracks the last pixel touched.
///
/// Cycle 824 (audit): the previous decoder reallocated to the EXACT new size and
/// full-copied every existing row on each growth — and a spec-legal sixel that
/// omits the raster-attribute size hint grows its width one pixel at a time, so
/// that was O(W) reallocations of O(W·H) each = O(W²·H), seconds-to-minutes of
/// single-threaded work blocking the render/PTY loop on one small escape.
struct SixelCanvas {
    buf: Vec<u8>,
    cap_w: usize,
    cap_h: usize,
    width: usize,
    height: usize,
}

impl SixelCanvas {
    fn new() -> Self {
        SixelCanvas {
            buf: Vec::new(),
            cap_w: 0,
            cap_h: 0,
            width: 0,
            height: 0,
        }
    }

    /// Make room for a pixel at column `nx-1`, row `ny-1`. Grows capacity
    /// geometrically when needed (re-laying-out existing data at the new
    /// stride). Returns `false` if the logical extent would exceed `MAX_DIM`.
    fn ensure(&mut self, nx: usize, ny: usize) -> bool {
        let new_w = self.width.max(nx);
        let new_h = self.height.max(ny);
        if new_w > MAX_DIM || new_h > MAX_DIM {
            return false;
        }
        if new_w <= self.cap_w && new_h <= self.cap_h {
            self.width = new_w;
            self.height = new_h;
            return true;
        }
        let ncw = grow_cap(self.cap_w, new_w);
        let nch = grow_cap(self.cap_h, new_h);
        let mut nb = vec![0u8; ncw * nch * 4];
        // Copy only the rows/cols that hold data (the old logical extent), at
        // the old stride → new stride. The rest of `nb` stays zero.
        let row_bytes = self.width * 4;
        for row in 0..self.height {
            let src = row * self.cap_w * 4;
            let dst = row * ncw * 4;
            nb[dst..dst + row_bytes].copy_from_slice(&self.buf[src..src + row_bytes]);
        }
        self.buf = nb;
        self.cap_w = ncw;
        self.cap_h = nch;
        self.width = new_w;
        self.height = new_h;
        true
    }

    /// Paint one 6-pixel sixel column at `(x, band_y)` using the ALLOCATED
    /// stride (`cap_w`), not the logical width.
    fn put(&mut self, x: usize, band_y: usize, bits: u8, c: Rgb) {
        for r in 0..6 {
            if bits & (1 << r) != 0 {
                let y = band_y + r;
                let idx = (y * self.cap_w + x) * 4;
                if idx + 4 <= self.buf.len() {
                    self.buf[idx] = c.0;
                    self.buf[idx + 1] = c.1;
                    self.buf[idx + 2] = c.2;
                    self.buf[idx + 3] = 255;
                }
            }
        }
    }

    /// Compact the used `width × height` region (stride `cap_w`) into a tight
    /// `width × height` RGBA buffer and build the image. One final O(W·H) pass.
    fn into_image(self) -> Option<ImageData> {
        if self.cap_w == self.width && self.cap_h == self.height {
            return ImageData::new(self.width as u32, self.height as u32, self.buf);
        }
        let mut tight = vec![0u8; self.width.saturating_mul(self.height).saturating_mul(4)];
        let row_bytes = self.width * 4;
        for row in 0..self.height {
            let src = row * self.cap_w * 4;
            let dst = row * self.width * 4;
            tight[dst..dst + row_bytes].copy_from_slice(&self.buf[src..src + row_bytes]);
        }
        ImageData::new(self.width as u32, self.height as u32, tight)
    }
}

/// Decode the body of a Sixel DCS (the bytes after the `q`, before `ST`).
pub fn decode(data: &[u8]) -> Option<ImageData> {
    let mut palette = default_palette();
    let mut color = 1usize;
    let mut x = 0usize;
    let mut band_y = 0usize;
    let mut canvas = SixelCanvas::new();

    let mut i = 0;
    let n = data.len();
    while i < n {
        let b = data[i];
        match b {
            b'#' => {
                i += 1;
                let (pc, ni) = read_num(data, i);
                i = ni;
                if i < n && data[i] == b';' {
                    i += 1;
                    let (pu, ni) = read_num(data, i);
                    i = ni;
                    let mut comps = [0i64; 3];
                    let mut ci = 0;
                    while ci < 3 && i < n && data[i] == b';' {
                        i += 1;
                        let (v, ni) = read_num(data, i);
                        comps[ci] = v;
                        i = ni;
                        ci += 1;
                    }
                    let rgb = if pu == 2 {
                        Rgb(
                            (comps[0] * 255 / 100) as u8,
                            (comps[1] * 255 / 100) as u8,
                            (comps[2] * 255 / 100) as u8,
                        )
                    } else {
                        hls_to_rgb(comps[0] as f32, comps[1] as f32, comps[2] as f32)
                    };
                    if (pc as usize) < palette.len() {
                        palette[pc as usize] = rgb;
                    }
                }
                color = (pc as usize).min(palette.len() - 1);
            }
            b'!' => {
                i += 1;
                let (cnt, ni) = read_num(data, i);
                i = ni;
                if i < n {
                    let s = data[i];
                    i += 1;
                    if (0x3f..=0x7e).contains(&s) {
                        let bits = s - 0x3f;
                        for _ in 0..cnt.max(1) {
                            if !canvas.ensure(x + 1, band_y + 6) {
                                return None;
                            }
                            canvas.put(x, band_y, bits, palette[color]);
                            x += 1;
                        }
                    }
                }
            }
            b'"' => {
                i += 1;
                // raster attrs: Pan;Pad;Ph;Pw — use Pw,Ph as a size hint.
                let mut vals = [0i64; 4];
                for (k, slot) in vals.iter_mut().enumerate() {
                    let (v, ni) = read_num(data, i);
                    *slot = v;
                    i = ni;
                    if k < 3 && i < n && data[i] == b';' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                if vals[2] > 0 && vals[3] > 0 {
                    // Raster-attribute size hint (Pw, Ph): pre-grow once so the
                    // common (hinted) case allocates a single time.
                    canvas.ensure(vals[3] as usize, vals[2] as usize);
                }
            }
            b'$' => {
                x = 0;
                i += 1;
            }
            b'-' => {
                x = 0;
                band_y += 6;
                i += 1;
            }
            0x3f..=0x7e => {
                let bits = b - 0x3f;
                if !canvas.ensure(x + 1, band_y + 6) {
                    return None;
                }
                canvas.put(x, band_y, bits, palette[color]);
                x += 1;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    canvas.into_image()
}

fn read_num(data: &[u8], mut i: usize) -> (i64, usize) {
    let mut v: i64 = 0;
    let mut any = false;
    while i < data.len() && data[i].is_ascii_digit() {
        // Cycle 830 (audit): saturating, not `v * 10 + d`. Every numeric param
        // (`!<n>` repeat, `#<n>` palette, `"<n>` raster) is attacker-controlled
        // from a DCS body up to 64 MiB; a ~20-digit run overflowed the i64
        // multiply — a hard process abort under debug/test (panic=abort), a
        // silent wrap in release. Saturating makes it total; downstream already
        // rejects over-large dims/indices, and the repeat loop bails via the
        // MAX_DIM `ensure` cap, so no legal sixel is affected.
        v = v.saturating_mul(10).saturating_add((data[i] - b'0') as i64);
        i += 1;
        any = true;
    }
    if !any { (0, i) } else { (v, i) }
}

#[cfg(test)]
mod tests {
    use super::{MAX_DIM, decode, grow_cap};

    /// Cycle 824 (audit) drift guard: capacity grows GEOMETRICALLY (doubling),
    /// which is what turns the decoder's total re-layout work from O(W²·H) into
    /// amortized O(W·H). If a refactor reverts to exact-fit growth (`grow_cap`
    /// returning `needed`), the doubling assertions below fail.
    #[test]
    fn grow_cap_doubles_and_caps_at_max_dim() {
        assert_eq!(grow_cap(0, 1), 1);
        assert_eq!(grow_cap(1, 1), 1);
        assert_eq!(grow_cap(1, 100), 128); // 1→2→…→128, NOT 100 (exact)
        assert_eq!(grow_cap(128, 200), 256);
        assert_eq!(grow_cap(0, 5000), 8192); // smallest pow2 ≥ 5000
        assert_eq!(grow_cap(8192, 8192), 8192);
        assert!(grow_cap(0, MAX_DIM) >= MAX_DIM);
    }

    /// Cycle 830 (audit): a long digit run in any numeric param must not
    /// overflow `read_num`'s i64 accumulate (a debug/test panic=abort). A
    /// 25-digit count decodes cleanly (saturates, then the dim/ensure caps
    /// reject it) rather than aborting the process.
    #[test]
    fn long_digit_run_does_not_overflow() {
        // 25 nines as a repeat count, then a sixel char — saturates, the ensure
        // cap rejects the absurd width → clean None, never a panic.
        let body = format!("!{}~", "9".repeat(25));
        assert!(decode(body.as_bytes()).is_none());
        // The same in a raster attribute (dimension param).
        let body2 = format!("\"{};1;1;1@", "9".repeat(25));
        let _ = decode(body2.as_bytes()); // must not panic (result either way ok)
    }

    /// Cycle 824 (audit): a wide raster-attribute-LESS sixel (width grows one
    /// pixel at a time) was the O(W²·H) DoS trigger. It must still decode to the
    /// correct dimensions — and now does so in O(W·H) (instant rather than the
    /// seconds-to-minutes the old exact-fit regrow took).
    #[test]
    fn wide_unhinted_sixel_decodes_correctly() {
        // `!2000~` = repeat the full-column sixel (~, all 6 bits) 2000× in band
        // 0, no `"`-raster hint → 2000 one-pixel width growths.
        let img = decode(b"!2000~").expect("wide sixel decodes");
        assert_eq!((img.width, img.height), (2000, 6));
        // Spot-check the last column is painted (extent tracked correctly).
        let last = ((5 * 2000 + 1999) * 4) as usize;
        assert_eq!(img.rgba[last + 3], 255, "last column, bottom row opaque");
    }

    // One sixel char encodes a 6-pixel-tall column; bits = byte - 0x3f.
    // `@` (0x40) → bits 0b1 → only the top pixel of the band is lit.
    #[test]
    fn single_sixel_yields_a_1x6_column() {
        let img = decode(b"@").expect("one sixel char should decode");
        assert_eq!((img.width, img.height), (1, 6));
        // Top pixel lit with palette color 1 (VT340 default 20/20/80%),
        // opaque; the five below it are transparent.
        assert_eq!(&img.rgba[0..4], &[51, 51, 204, 255]);
        assert_eq!(&img.rgba[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn empty_band_char_is_transparent_but_sized() {
        // `?` (0x3f) → bits 0 → no pixel lit, but the column still sizes
        // the image to 1×6.
        let img = decode(b"?").expect("'?' sizes an empty band");
        assert_eq!((img.width, img.height), (1, 6));
        assert!(img.rgba.iter().all(|&b| b == 0));
    }

    #[test]
    fn repeat_introducer_widens_the_band() {
        // `!3@` = repeat `@` three times → a 3-wide, 6-tall band.
        let img = decode(b"!3@").expect("repeat should decode");
        assert_eq!((img.width, img.height), (3, 6));
    }

    #[test]
    fn newline_introducer_adds_a_band_below() {
        // `@-@` = column in band 0, carriage `-` to the next band, column
        // in band 1 → 1 wide, 12 tall.
        let img = decode(b"@-@").expect("two bands should decode");
        assert_eq!((img.width, img.height), (1, 12));
    }

    #[test]
    fn empty_input_decodes_to_nothing() {
        // No pixels ⇒ 0×0 ⇒ ImageData::new rejects it as None.
        assert!(decode(b"").is_none());
    }

    #[test]
    fn oversized_raster_attr_is_rejected_without_panicking() {
        // A raster-attribute size hint past MAX_DIM must not allocate or
        // panic; the `ensure` bound rejects it and the (still 0×0) image
        // decodes to None.
        let attr = format!("\"1;1;1;{}", MAX_DIM + 1);
        assert!(decode(attr.as_bytes()).is_none());
    }
}
