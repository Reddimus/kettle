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

/// Decode the body of a Sixel DCS (the bytes after the `q`, before `ST`).
pub fn decode(data: &[u8]) -> Option<ImageData> {
    let mut palette = default_palette();
    let mut color = 1usize;
    let mut x = 0usize;
    let mut band_y = 0usize;
    let mut width = 0usize;
    let mut height = 0usize;
    // RGBA, grown on demand.
    let mut buf: Vec<u8> = Vec::new();

    let ensure = |buf: &mut Vec<u8>, w: &mut usize, h: &mut usize, nx: usize, ny: usize| -> bool {
        let nw = (*w).max(nx);
        let nh = (*h).max(ny);
        if nw > MAX_DIM || nh > MAX_DIM {
            return false;
        }
        if nw == *w && nh == *h {
            return true;
        }
        let mut nb = vec![0u8; nw * nh * 4];
        for row in 0..*h {
            let src = row * *w * 4;
            let dst = row * nw * 4;
            nb[dst..dst + *w * 4].copy_from_slice(&buf[src..src + *w * 4]);
        }
        *buf = nb;
        *w = nw;
        *h = nh;
        true
    };

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
                            if !ensure(&mut buf, &mut width, &mut height, x + 1, band_y + 6) {
                                return None;
                            }
                            put(&mut buf, width, x, band_y, bits, palette[color]);
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
                    ensure(
                        &mut buf,
                        &mut width,
                        &mut height,
                        vals[3] as usize,
                        vals[2] as usize,
                    );
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
                if !ensure(&mut buf, &mut width, &mut height, x + 1, band_y + 6) {
                    return None;
                }
                put(&mut buf, width, x, band_y, bits, palette[color]);
                x += 1;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    ImageData::new(width as u32, height as u32, buf)
}

fn put(buf: &mut [u8], width: usize, x: usize, band_y: usize, bits: u8, c: Rgb) {
    for r in 0..6 {
        if bits & (1 << r) != 0 {
            let y = band_y + r;
            let idx = (y * width + x) * 4;
            if idx + 4 <= buf.len() {
                buf[idx] = c.0;
                buf[idx + 1] = c.1;
                buf[idx + 2] = c.2;
                buf[idx + 3] = 255;
            }
        }
    }
}

fn read_num(data: &[u8], mut i: usize) -> (i64, usize) {
    let mut v: i64 = 0;
    let mut any = false;
    while i < data.len() && data[i].is_ascii_digit() {
        v = v * 10 + (data[i] - b'0') as i64;
        i += 1;
        any = true;
    }
    if !any { (0, i) } else { (v, i) }
}
