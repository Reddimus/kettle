//! `starfield.rs` on the CPU: the procedural wallpaper.
//!
//! Per the WGSL: every pixel of the target starts black and sums, for each
//! live star, `tint * (cc^2 * 1.15 + exp(-d^2 / halo^2) * 0.22)` with
//! `cc = core^2 / (d^2 + core^2)`, measured from the pixel centre to the star
//! around the surface centre. The sum is clamped and replaces the pixel with
//! alpha 1.
//!
//! The shader sums every star at every pixel. Here a star only visits the
//! square where its term can reach [`TERM_FLOOR`] in linear light; outside
//! it adds less than that, so skipping it moves a channel by under 0.02 of a
//! code even next to black, where the sRGB curve is steepest. The stars are
//! the uniform `StarfieldPipeline::upload` writes, resolved by the same
//! `build_frame_stars`.

use super::srgb::tables;
use super::{CpuTarget, PixelRect, pack};
use crate::starfield::{NSTARS, Star};
#[cfg(test)]
use crate::starfield::{StarSeed, build_frame_stars};

/// The smallest per-star term, in linear light, still drawn.
const TERM_FLOOR: f32 = 1.0e-6;

#[cfg(test)]
/// The stars one frame of the shader sums: what `StarfieldPipeline::upload`
/// writes into its uniform for `resolution` and `time_secs`.
pub(crate) fn frame_stars(
    seeds: &[StarSeed; NSTARS],
    resolution: [f32; 2],
    time_secs: f32,
) -> ([Star; NSTARS], usize) {
    let mut stars = [Star::default(); NSTARS];
    let count = build_frame_stars(seeds, resolution, time_secs, &mut stars) as usize;
    (stars, count)
}

/// Half the side of the square around a star outside which its term is
/// below [`TERM_FLOOR`] in every channel.
fn reach(star: &Star) -> f32 {
    let tint = star.tint[0].max(star.tint[1]).max(star.tint[2]);
    if tint.is_nan() || tint <= 0.0 {
        return 0.0;
    }
    let (core2, halo2) = (star.geom[2], star.geom[3]);
    // Each falloff gets half the floor: tint * cc^2 * 1.15 <= floor / 2 and
    // tint * exp(-d2 / halo2) * 0.22 <= floor / 2.
    let cc = (TERM_FLOOR / (2.0 * 1.15 * tint)).sqrt();
    let core_d2 = if cc < 1.0 { core2 / cc - core2 } else { 0.0 };
    let halo_ratio = 0.44 * tint / TERM_FLOOR;
    let halo_d2 = if halo_ratio > 1.0 {
        halo2 * halo_ratio.ln()
    } else {
        0.0
    };
    let d2 = core_d2.max(halo_d2);
    if d2.is_finite() {
        d2.sqrt() + 1.0
    } else {
        f32::INFINITY
    }
}

/// Draw the starfield with `stars` (the first `count` live) on a
/// `resolution` uniform, as `StarfieldPipeline::draw` does, under an
/// optional scissor. `scratch` is a reusable row of linear sums.
pub(crate) fn draw_starfield(
    target: &mut CpuTarget<'_>,
    resolution: [f32; 2],
    stars: &[Star],
    scissor: Option<PixelRect>,
    scratch: &mut Vec<[f32; 3]>,
) {
    let Some(clip) = target.clip(scissor) else {
        return;
    };
    let t = tables();
    let res = [resolution[0].max(1.0), resolution[1].max(1.0)];
    let centre = [res[0] * 0.5, res[1] * 0.5];
    let (x0, x1) = (clip.x, clip.x + clip.w);
    // Each star's reach as framebuffer columns and rows.
    let mut boxes = [(0u32, 0u32, 0u32, 0u32); NSTARS];
    let live = &stars[..stars.len().min(NSTARS)];
    for (b, star) in boxes.iter_mut().zip(live) {
        let r = reach(star);
        let (sx, sy) = (centre[0] + star.geom[0], centre[1] + star.geom[1]);
        let span = |c: f32, lo: u32, hi: u32| {
            let a = (c - r - 0.5).floor().max(lo as f32).min(hi as f32) as u32;
            let b = (c + r + 0.5).ceil().max(lo as f32).min(hi as f32) as u32;
            (a, b)
        };
        let (bx0, bx1) = span(sx, x0, x1);
        let (by0, by1) = span(sy, clip.y, clip.y + clip.h);
        *b = (bx0, bx1, by0, by1);
    }
    let black = pack(0, 0, 0, 255);
    scratch.clear();
    scratch.resize(clip.w as usize, [0.0; 3]);
    for y in clip.y..clip.y + clip.h {
        let mut lit = (x1, x0);
        let py = y as f32 + 0.5 - centre[1];
        for (star, &(bx0, bx1, by0, by1)) in live.iter().zip(&boxes) {
            if y < by0 || y >= by1 || bx0 >= bx1 {
                continue;
            }
            lit = (lit.0.min(bx0), lit.1.max(bx1));
            let dy = py - star.geom[1];
            for x in bx0..bx1 {
                let dx = x as f32 + 0.5 - centre[0] - star.geom[0];
                let d2 = dx * dx + dy * dy;
                let core2 = star.geom[2];
                let cc = core2 / (d2 + core2);
                let halo = (-d2 / star.geom[3]).exp();
                let k = cc * cc * 1.15 + halo * 0.22;
                let acc = &mut scratch[(x - x0) as usize];
                acc[0] += star.tint[0] * k;
                acc[1] += star.tint[1] * k;
                acc[2] += star.tint[2] * k;
            }
        }
        let span = target.span(y, x0, x1);
        span.fill(black);
        if lit.0 < lit.1 {
            for x in lit.0..lit.1 {
                let acc = &mut scratch[(x - x0) as usize];
                span[(x - x0) as usize] =
                    pack(t.encode(acc[0]), t.encode(acc[1]), t.encode(acc[2]), 255);
                *acc = [0.0; 3];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_raster::{CpuFramebuffer, unpack};

    #[test]
    fn a_star_lights_its_centre_and_its_reach_bounds_the_term() {
        let star = Star {
            geom: [0.0, 0.0, 1.0, 9.0],
            tint: [1.0, 0.5, 0.25, 0.0],
        };
        let r = reach(&star);
        let d2 = r * r;
        let cc = 1.0 / (d2 + 1.0);
        let term = cc * cc * 1.15 + (-d2 / 9.0).exp() * 0.22;
        assert!(term <= TERM_FLOOR, "{term} at {r}");
        let mut fb = CpuFramebuffer::new(32, 32).unwrap();
        let mut scratch = Vec::new();
        draw_starfield(&mut fb.target(), [32.0, 32.0], &[star], None, &mut scratch);
        let at = |x: usize, y: usize| unpack(fb.pixels()[y * 32 + x]);
        assert_eq!(at(16, 16)[3], 255);
        assert!(at(15, 15)[0] > 200, "{:?}", at(15, 15));
        assert_eq!(at(0, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn a_dark_star_reaches_nothing() {
        let star = Star {
            geom: [0.0, 0.0, 1.0, 9.0],
            tint: [0.0; 4],
        };
        assert_eq!(reach(&star), 0.0);
    }
}
