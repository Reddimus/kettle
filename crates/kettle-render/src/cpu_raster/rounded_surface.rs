//! `rounded_surface.rs` on the CPU: menus, settings shells, toasts,
//! swatches, shadows and the media transport icons.
//!
//! Per the WGSL: a rounded-box distance, or one of three icon distances
//! (`params.z` 1 play, 2 fullscreen, 3 sound with `params.w` 1 muted), feeds
//! `aa = max(fwidth(d) / 2, 1/4)`. Fill coverage is `1 - smoothstep(-aa, aa,
//! d + stroke)` and the stroke is the outer ramp minus it. Kind 4 scales the
//! fill by `params.w`. The fragment is premultiplied and blends with
//! `PREMULTIPLIED_ALPHA_BLENDING`.
//!
//! Inside a box, well clear of its edges and corners, every ramp saturates,
//! so the colour is the constant fill; those spans blend as flat fills and
//! only the band near the outline evaluates the distance.

use super::srgb::{srgb_to_linear, tables};
use super::{
    ConstBlend, CpuTarget, PixelRect, Premul, Raster, TABLE_SPAN_PIXELS, blend_premul, fine_fwidth,
    rasterize, smoothstep,
};
use crate::rounded_surface::RoundedSurface;

/// Draw `surfaces` in order, as `RoundedSurfacePipeline::draw_range` does on
/// a `screen`-sized uniform, under an optional scissor.
pub(crate) fn draw_rounded_surfaces(
    target: &mut CpuTarget<'_>,
    screen: [f32; 2],
    surfaces: &[RoundedSurface],
    scissor: Option<PixelRect>,
) {
    let Some(clip) = target.clip(scissor) else {
        return;
    };
    let t = tables();
    let viewport = target.viewport();
    for s in surfaces {
        let Some(r) = rasterize(&viewport, screen, s.pos, s.size, clip) else {
            continue;
        };
        let shape = Shape::new(s, &r);
        let interior = shape.interior().and_then(|c| ConstBlend::new(t, c));
        let area = u64::from(r.cols.end - r.cols.start) * u64::from(r.rows.end - r.rows.start);
        let tables = interior
            .filter(|b| area > TABLE_SPAN_PIXELS && b.opaque().is_none())
            .map(|b| b.tables(t));
        for y in r.rows.clone() {
            let flat = interior.and_then(|blend| shape.interior_cols(&r, y).map(|c| (blend, c)));
            let (x_flat0, x_flat1) = match flat {
                Some((blend, cols)) => {
                    let span = target.span(y, cols.start, cols.end);
                    match &tables {
                        Some(tables) => tables.blend_span(span),
                        None => blend.blend_span(t, span),
                    }
                    (cols.start, cols.end)
                }
                None => (r.cols.end, r.cols.end),
            };
            for x in (r.cols.start..x_flat0).chain(x_flat1..r.cols.end) {
                let src = shape.fragment(x, y);
                let p = target.pixel(x, y);
                *p = blend_premul(t, *p, src);
            }
        }
    }
}

/// One instance, with what its fragment needs precomputed.
struct Shape<'a> {
    s: &'a RoundedSurface,
    r: Raster,
    radius: f32,
    fill: [f32; 3],
    stroke: [f32; 3],
}

impl<'a> Shape<'a> {
    fn new(s: &'a RoundedSurface, r: &Raster) -> Self {
        let radius = s.params[0].max(0.0).min(s.size[0].min(s.size[1]) * 0.5);
        Self {
            s,
            r: r.clone(),
            radius,
            fill: [0, 1, 2].map(|c| srgb_to_linear(s.fill[c])),
            stroke: [0, 1, 2].map(|c| srgb_to_linear(s.stroke[c])),
        }
    }

    fn kind(&self) -> f32 {
        self.s.params[2]
    }

    #[inline(always)]
    fn local(&self, x: u32, y: u32) -> [f32; 2] {
        [
            self.r.lerp_x(x, 0.0, self.s.size[0]),
            self.r.lerp_y(y, 0.0, self.s.size[1]),
        ]
    }

    /// The shader's `distance` at pixel `(x, y)`.
    #[inline(always)]
    fn distance(&self, x: u32, y: u32) -> f32 {
        let local = self.local(x, y);
        let size = self.s.size;
        let half = [size[0] * 0.5, size[1] * 0.5];
        let q = [
            (local[0] - half[0]).abs() - (half[0] - self.radius),
            (local[1] - half[1]).abs() - (half[1] - self.radius),
        ];
        let (qx, qy) = (q[0].max(0.0), q[1].max(0.0));
        let outside = (qx * qx + qy * qy).sqrt();
        let mut distance = outside + q[0].max(q[1]).min(0.0) - self.radius;
        let p = [
            (local[0] - half[0]) / half[0],
            (local[1] - half[1]) / half[1],
        ];
        let scale = size[0].min(size[1]) * 0.5;
        let kind = self.kind();
        if kind == 1.0 {
            distance =
                (-p[0] - 0.541_666_7).max(p[0] + p[1].abs() * 1.777_777_8 - 0.791_666_7) * scale;
        } else if kind == 2.0 {
            let a = [p[0].abs(), p[1].abs()];
            distance = (a[0].max(a[1]) - 0.85)
                .max((0.4 - a[0].min(a[1])).max(0.65 - a[0].max(a[1])))
                * scale;
        } else if kind == 3.0 {
            let body = ((p[0] + 0.45).abs() - 0.25)
                .max(p[1].abs() - 0.3)
                .max(-p[0] - 0.8);
            let cone = ((p[0] + 0.15).abs() - 0.3).max(p[1].abs() - (p[0] + 0.8));
            let arc = ((p[0] + 0.25) * (p[0] + 0.25) + p[1] * p[1]).sqrt();
            let mut sound = ((arc - 0.95).abs() - 0.07).max(0.35 - p[0]);
            if self.s.params[3] == 1.0 {
                sound = ((p[0] + p[1]).abs() - 0.09).max(p[0].abs().max(p[1].abs()) - 0.85);
            }
            distance = body.min(cone).min(sound) * scale;
        }
        distance
    }

    /// The fragment at pixel `(x, y)`.
    fn fragment(&self, x: u32, y: u32) -> Premul {
        let distance = self.distance(x, y);
        let fw = fine_fwidth(x, y, distance, |x, y| self.distance(x, y));
        let aa = (fw * 0.5).max(0.25);
        let outer = 1.0 - smoothstep(-aa, aa, distance);
        let inner = 1.0 - smoothstep(-aa, aa, distance + self.s.params[1].max(0.0));
        let opacity = if self.kind() == 4.0 {
            self.s.params[3]
        } else {
            1.0
        };
        let fa = self.s.fill[3] * inner * opacity;
        let sa = self.s.stroke[3] * (outer - inner).max(0.0);
        Premul {
            r: self.fill[0] * fa + self.stroke[0] * sa,
            g: self.fill[1] * fa + self.stroke[1] * sa,
            b: self.fill[2] * fa + self.stroke[2] * sa,
            a: fa + sa,
        }
    }

    /// Whether a flat interior exists to fill without the distance field:
    /// not for icons, nor for mappings that are not 1:1, where the
    /// distance's slope per pixel, and so `aa`, is not bounded by one.
    fn has_flat_interior(&self) -> bool {
        let kind = self.kind();
        if kind == 1.0 || kind == 2.0 || kind == 3.0 {
            return false;
        }
        let one_to_one = |a: f32, b: f32, size: f32| ((b - a) - size).abs() <= size * 1.0e-3;
        one_to_one(self.r.x0, self.r.x1, self.s.size[0])
            && one_to_one(self.r.y0, self.r.y1, self.s.size[1])
            && self.s.stroke[3].is_finite()
    }

    /// The constant colour of the saturated interior.
    fn interior(&self) -> Option<Premul> {
        if !self.has_flat_interior() {
            return None;
        }
        let opacity = if self.kind() == 4.0 {
            self.s.params[3]
        } else {
            1.0
        };
        let fa = self.s.fill[3] * opacity;
        Some(Premul {
            r: self.fill[0] * fa,
            g: self.fill[1] * fa,
            b: self.fill[2] * fa,
            a: fa,
        })
    }

    /// The covered columns of row `y` where every ramp has saturated. The
    /// box distance is convex, so they are one run: walk in from each end.
    fn interior_cols(&self, r: &Raster, y: u32) -> Option<std::ops::Range<u32>> {
        if !self.has_flat_interior() {
            return None;
        }
        // The distance is at most -(stroke + 2) there, and aa is at most 1.
        let limit = -(self.s.params[1].max(0.0) + 2.0);
        let saturated = |x: u32| self.distance(x, y) <= limit;
        let mid = r.cols.start + (r.cols.end - r.cols.start) / 2;
        let mut x0 = r.cols.start;
        while x0 < mid && !saturated(x0) {
            x0 += 1;
        }
        if !saturated(x0) {
            return None;
        }
        let mut x1 = r.cols.end;
        while x1 > x0 && !saturated(x1 - 1) {
            x1 -= 1;
        }
        Some(x0..x1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_raster::{CpuFramebuffer, unpack};

    fn surface(size: [f32; 2], radius: f32, stroke_width: f32) -> RoundedSurface {
        RoundedSurface {
            pos: [0.0, 0.0],
            size,
            fill: [1.0, 1.0, 1.0, 1.0],
            stroke: [1.0, 0.0, 0.0, 1.0],
            params: [radius, stroke_width, 0.0, 0.0],
        }
    }

    fn render(s: RoundedSurface, w: u32, h: u32) -> CpuFramebuffer {
        let mut fb = CpuFramebuffer::new(w, h).unwrap();
        fb.clear([0.0, 0.0, 0.0, 1.0]);
        draw_rounded_surfaces(&mut fb.target(), [w as f32, h as f32], &[s], None);
        fb
    }

    #[test]
    fn the_flat_interior_matches_the_distance_field() {
        let s = surface([40.0, 30.0], 6.0, 1.5);
        let fast = render(s, 40, 30);
        let viewport = crate::cpu_raster::Viewport {
            x: 0.0,
            y: 0.0,
            w: 40.0,
            h: 30.0,
        };
        let shape_r = rasterize(
            &viewport,
            [40.0, 30.0],
            s.pos,
            s.size,
            PixelRect::new(0, 0, 40, 30),
        )
        .unwrap();
        let shape = Shape::new(&s, &shape_r);
        let t = tables();
        let base = crate::cpu_raster::clear_pixel([0.0, 0.0, 0.0, 1.0]);
        for y in 0..30 {
            for x in 0..40 {
                let slow = blend_premul(t, base, shape.fragment(x, y));
                assert_eq!(fast.pixels()[(y * 40 + x) as usize], slow, "({x}, {y})");
            }
        }
    }

    #[test]
    fn corners_are_cut_and_the_stroke_is_inset() {
        let fb = render(surface([20.0, 20.0], 8.0, 2.0), 20, 20);
        assert_eq!(unpack(fb.pixels()[0]), [0, 0, 0, 255], "corner outside");
        assert_eq!(unpack(fb.pixels()[10 * 20]), [255, 0, 0, 255], "stroke");
        assert_eq!(unpack(fb.pixels()[10 * 20 + 10]), [255; 4], "fill");
    }
}
