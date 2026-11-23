//! `outline.rs` on the CPU: pane outlines with rounded or dashed corners.
//!
//! Per the WGSL: the stroke's centreline is inset half a stroke. Selected
//! corners use a rounded-box distance and the rest an L-infinity one. Both
//! sides of the stroke antialias with one ramp on `|distance|`, half-width
//! `max(fwidth(d) / 2, 1/4)`. A dash is measured along the nearest edge and
//! a rounded arc stays solid. A zero width, zero coverage or a gap is a
//! `discard`, which leaves the pixel alone. The fragment is premultiplied
//! and blends with `PREMULTIPLIED_ALPHA_BLENDING`.
//!
//! Every pixel farther from the edges than the stroke and corner reach has
//! zero coverage, so only the band along the edges is evaluated.

use super::srgb::{srgb_to_linear, tables};
use super::{
    CpuTarget, PixelRect, Premul, Raster, blend_premul, fine_fwidth, rasterize, smoothstep,
};
use crate::outline::OutlineInstance;

/// Draw `outlines` in order, as `OutlinePipeline::draw` does on a
/// `screen`-sized uniform, under an optional scissor.
pub(crate) fn draw_outlines(
    target: &mut CpuTarget<'_>,
    screen: [f32; 2],
    outlines: &[OutlineInstance],
    scissor: Option<PixelRect>,
) {
    let Some(clip) = target.clip(scissor) else {
        return;
    };
    let t = tables();
    let viewport = target.viewport();
    for o in outlines {
        if o.border_width <= 0.0 {
            continue;
        }
        let Some(r) = rasterize(&viewport, screen, o.pos, o.size, clip) else {
            continue;
        };
        let stroke = Stroke::new(o, r);
        let band = stroke.band();
        for y in stroke.r.rows.clone() {
            let skip = band.and_then(|[bx0, bx1, by0, by1]| {
                (y >= by0 && y < by1 && bx1 > bx0).then_some(bx0..bx1)
            });
            let skip = skip.unwrap_or(stroke.r.cols.end..stroke.r.cols.end);
            for x in (stroke.r.cols.start..skip.start).chain(skip.end..stroke.r.cols.end) {
                if let Some(src) = stroke.fragment(x, y) {
                    let p = target.pixel(x, y);
                    *p = blend_premul(t, *p, src);
                }
            }
        }
    }
}

struct Stroke<'a> {
    o: &'a OutlineInstance,
    r: Raster,
    radius: f32,
    linear: [f32; 3],
}

/// WGSL `selected_corner_radius`.
fn selected_corner_radius(local: [f32; 2], size: [f32; 2], radius: f32, mask: u32) -> f32 {
    let left = local[0] < size[0] * 0.5;
    let top = local[1] < size[1] * 0.5;
    let bit = match (left, top) {
        (true, true) => 1,
        (false, true) => 2,
        (false, false) => 4,
        (true, false) => 8,
    };
    if mask & bit != 0 { radius } else { 0.0 }
}

/// WGSL `fitted_dash_on`.
fn fitted_dash_on(along: f32, len: f32, dash: f32, gap: f32) -> bool {
    if dash <= 0.0 || gap <= 0.0 || len < 2.0 * dash + gap {
        return true;
    }
    let count = ((len + gap) / (dash + gap)).floor();
    let step = (len - dash) / (count - 1.0);
    let k = (along / step).floor().max(0.0).min(count - 1.0);
    along - k * step < dash
}

impl<'a> Stroke<'a> {
    fn new(o: &'a OutlineInstance, r: Raster) -> Self {
        Self {
            o,
            r,
            radius: o.corner_radius.min(o.size[0].min(o.size[1]) * 0.5),
            linear: [0, 1, 2].map(|c| srgb_to_linear(o.color[c])),
        }
    }

    #[inline(always)]
    fn local(&self, x: u32, y: u32) -> [f32; 2] {
        [
            self.r.lerp_x(x, 0.0, self.o.size[0]),
            self.r.lerp_y(y, 0.0, self.o.size[1]),
        ]
    }

    /// The shader's `(radius, distance)` at pixel `(x, y)`.
    #[inline(always)]
    fn distance(&self, x: u32, y: u32) -> (f32, f32) {
        let local = self.local(x, y);
        let size = self.o.size;
        let radius = selected_corner_radius(local, size, self.radius, self.o.corner_mask);
        let inset = self.o.border_width * 0.5;
        let ol = [local[0] - inset, local[1] - inset];
        let os = [
            (size[0] - inset * 2.0).max(0.0),
            (size[1] - inset * 2.0).max(0.0),
        ];
        let or = (radius - inset).max(0.0);
        let half = [os[0] * 0.5, os[1] * 0.5];
        let distance = if or > 0.0 {
            let q = [
                (ol[0] - half[0]).abs() - (half[0] - or),
                (ol[1] - half[1]).abs() - (half[1] - or),
            ];
            let (qx, qy) = (q[0].max(0.0), q[1].max(0.0));
            (qx * qx + qy * qy).sqrt() + q[0].max(q[1]).min(0.0) - or
        } else {
            let q = [
                (ol[0] - half[0]).abs() - half[0],
                (ol[1] - half[1]).abs() - half[1],
            ];
            q[0].max(q[1])
        };
        (radius, distance)
    }

    /// The fragment at pixel `(x, y)`, `None` where the shader discards.
    fn fragment(&self, x: u32, y: u32) -> Option<Premul> {
        let (radius, distance) = self.distance(x, y);
        let fw = fine_fwidth(x, y, distance, |x, y| self.distance(x, y).1);
        let half_aa = (fw * 0.5).max(0.25);
        let half_width = self.o.border_width * 0.5;
        let coverage = 1.0
            - smoothstep(
                (half_width - half_aa).max(0.0),
                half_width + half_aa,
                distance.abs(),
            );
        if coverage <= 0.0 {
            return None;
        }
        let local = self.local(x, y);
        let size = self.o.size;
        let to_x = local[0].min(size[0] - local[0]);
        let to_y = local[1].min(size[1] - local[1]);
        let on_arc = radius > 0.0 && to_x < radius && to_y < radius;
        if self.o.dash > 0.0 && !on_arc {
            let on = if to_y <= to_x {
                fitted_dash_on(local[0], size[0], self.o.dash, self.o.gap)
            } else {
                fitted_dash_on(local[1], size[1], self.o.dash, self.o.gap)
            };
            if !on {
                return None;
            }
        }
        let alpha = self.o.color[3] * coverage;
        Some(Premul {
            r: self.linear[0] * alpha,
            g: self.linear[1] * alpha,
            b: self.linear[2] * alpha,
            a: alpha,
        })
    }

    /// Covered pixels `[x0, x1, y0, y1]` (half-open) with zero coverage
    /// throughout, or `None` when the mapping is not 1:1.
    fn band(&self) -> Option<[u32; 4]> {
        let one_to_one = |a: f32, b: f32, size: f32| ((b - a) - size).abs() <= size * 1.0e-3;
        if !one_to_one(self.r.x0, self.r.x1, self.o.size[0])
            || !one_to_one(self.r.y0, self.r.y1, self.o.size[1])
        {
            return None;
        }
        // |distance| exceeds half the width plus any reachable ramp there.
        let m = self.o.border_width + self.radius.max(0.0) + 3.0;
        let inner = |a: f32, b: f32| {
            let lo = (a + m + 1.0).max(0.0) as u32;
            let hi = (b - m - 1.0).max(0.0) as u32;
            [lo, hi]
        };
        let [x0, x1] = inner(self.r.x0, self.r.x1);
        let [y0, y1] = inner(self.r.y0, self.r.y1);
        (x1 > x0 && y1 > y0).then_some([x0, x1, y0, y1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_raster::{CpuFramebuffer, unpack};

    fn outline(size: [f32; 2], width: f32, radius: f32, mask: u32) -> OutlineInstance {
        OutlineInstance {
            pos: [0.0, 0.0],
            size,
            color: [1.0, 1.0, 1.0, 1.0],
            border_width: width,
            corner_radius: radius,
            corner_mask: mask,
            dash: 0.0,
            gap: 0.0,
        }
    }

    fn render(o: OutlineInstance) -> CpuFramebuffer {
        let (w, h) = (o.size[0] as u32, o.size[1] as u32);
        let mut fb = CpuFramebuffer::new(w, h).unwrap();
        fb.clear([0.0, 0.0, 0.0, 1.0]);
        draw_outlines(&mut fb.target(), [w as f32, h as f32], &[o], None);
        fb
    }

    #[test]
    fn the_skipped_band_has_no_coverage() {
        let o = outline([64.0, 48.0], 2.0, 10.0, 0b0101);
        let r = rasterize(
            &crate::cpu_raster::Viewport {
                x: 0.0,
                y: 0.0,
                w: 64.0,
                h: 48.0,
            },
            [64.0, 48.0],
            o.pos,
            o.size,
            PixelRect::new(0, 0, 64, 48),
        )
        .unwrap();
        let stroke = Stroke::new(&o, r);
        let [x0, x1, y0, y1] = stroke.band().unwrap();
        for y in y0..y1 {
            for x in x0..x1 {
                assert!(stroke.fragment(x, y).is_none(), "({x}, {y})");
            }
        }
    }

    #[test]
    fn a_one_pixel_stroke_is_opaque_on_its_centreline() {
        let fb = render(outline([16.0, 16.0], 1.0, 0.0, 0));
        assert_eq!(unpack(fb.pixels()[8 * 16]), [255; 4]);
        assert_eq!(unpack(fb.pixels()[8 * 16 + 8]), [0, 0, 0, 255]);
    }

    #[test]
    fn zero_width_draws_nothing_and_dashes_leave_gaps() {
        let fb = render(outline([16.0, 16.0], 0.0, 0.0, 0));
        assert!(fb.pixels().iter().all(|&p| unpack(p) == [0, 0, 0, 255]));
        let mut o = outline([40.0, 8.0], 1.0, 0.0, 0);
        o.dash = 4.0;
        o.gap = 4.0;
        let fb = render(o);
        let top: Vec<_> = (0..40).map(|x| unpack(fb.pixels()[x])[0]).collect();
        assert_eq!(top[1], 255);
        assert_eq!(top[6], 0, "gap");
        assert_eq!(top[39], 255, "the last dash ends at the edge");
    }
}
