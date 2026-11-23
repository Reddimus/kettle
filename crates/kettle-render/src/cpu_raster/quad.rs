//! `quad.rs` on the CPU: flat rectangles.
//!
//! The fragment decodes the instance's straight sRGB colour, premultiplies
//! it and returns `(lin * a, a)`. The blending layer composites it with
//! `PREMULTIPLIED_ALPHA_BLENDING`; the base layer replaces the pixel, alpha
//! included. Rectangles have no edge antialiasing: a pixel is in or out by
//! its centre.

use super::srgb::{srgb_to_linear, tables};
use super::{
    ConstBlend, CpuTarget, PixelRect, Premul, TABLE_SPAN_PIXELS, blend_premul, rasterize, replace,
};
use crate::quad::QuadInstance;

/// Which of the two quad pipelines a layer draws with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuadBlend {
    /// `QuadShared::blend`: premultiplied source-over.
    Blend,
    /// `QuadShared::replace`: no blend state.
    Replace,
}

/// The colour the quad fragment returns for `color`.
pub(crate) fn quad_fragment(color: [f32; 4]) -> Premul {
    let a = color[3];
    Premul {
        r: srgb_to_linear(color[0]) * a,
        g: srgb_to_linear(color[1]) * a,
        b: srgb_to_linear(color[2]) * a,
        a,
    }
}

/// Draw `quads` in order, as `QuadPipeline::draw` does on a `screen`-sized
/// uniform, under an optional scissor.
pub(crate) fn draw_quads(
    target: &mut CpuTarget<'_>,
    screen: [f32; 2],
    quads: &[QuadInstance],
    mode: QuadBlend,
    scissor: Option<PixelRect>,
) {
    let Some(clip) = target.clip(scissor) else {
        return;
    };
    let t = tables();
    let viewport = target.viewport();
    for quad in quads {
        let Some(r) = rasterize(&viewport, screen, quad.pos, quad.size, clip) else {
            continue;
        };
        let src = quad_fragment(quad.color);
        match mode {
            QuadBlend::Replace => {
                let px = replace(t, src);
                for y in r.rows.clone() {
                    target.span(y, r.cols.start, r.cols.end).fill(px);
                }
            }
            QuadBlend::Blend => match ConstBlend::new(t, src) {
                Some(blend) if blend.is_noop() => {}
                Some(blend) => {
                    let area =
                        u64::from(r.cols.end - r.cols.start) * u64::from(r.rows.end - r.rows.start);
                    if area > TABLE_SPAN_PIXELS && blend.opaque().is_none() {
                        let tables = blend.tables(t);
                        for y in r.rows.clone() {
                            tables.blend_span(target.span(y, r.cols.start, r.cols.end));
                        }
                    } else {
                        for y in r.rows.clone() {
                            blend.blend_span(t, target.span(y, r.cols.start, r.cols.end));
                        }
                    }
                }
                // Out-of-range colours keep the shader's float arithmetic.
                None => {
                    for y in r.rows.clone() {
                        for p in target.span(y, r.cols.start, r.cols.end) {
                            *p = blend_premul(t, *p, src);
                        }
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_raster::{CpuFramebuffer, unpack};

    fn quad(pos: [f32; 2], size: [f32; 2], color: [f32; 4]) -> QuadInstance {
        QuadInstance { pos, size, color }
    }

    #[test]
    fn a_half_opaque_white_quad_over_black_lands_at_half_linear() {
        let mut fb = CpuFramebuffer::new(8, 8).unwrap();
        fb.clear([0.0, 0.0, 0.0, 1.0]);
        let q = [quad([0.0; 2], [8.0; 2], [1.0, 1.0, 1.0, 0.5])];
        draw_quads(&mut fb.target(), [8.0; 2], &q, QuadBlend::Blend, None);
        assert_eq!(unpack(fb.pixels()[36]), [188, 188, 188, 255]);
    }

    #[test]
    fn replace_writes_premultiplied_colour_and_alpha() {
        let mut fb = CpuFramebuffer::new(4, 4).unwrap();
        fb.clear([1.0; 4]);
        let q = [quad([1.0, 1.0], [2.0, 2.0], [1.0, 0.0, 0.0, 0.5])];
        draw_quads(&mut fb.target(), [4.0; 2], &q, QuadBlend::Replace, None);
        assert_eq!(unpack(fb.pixels()[5]), [188, 0, 0, 128]);
        assert_eq!(unpack(fb.pixels()[0]), [255; 4]);
        assert_eq!(unpack(fb.pixels()[15]), [255; 4]);
    }

    #[test]
    fn the_scissor_clips_and_order_is_kept() {
        let mut fb = CpuFramebuffer::new(4, 1).unwrap();
        let q = [
            quad([0.0, 0.0], [4.0, 1.0], [1.0, 0.0, 0.0, 1.0]),
            quad([0.0, 0.0], [4.0, 1.0], [0.0, 0.0, 1.0, 1.0]),
        ];
        let scissor = Some(PixelRect::new(1, 0, 2, 1));
        draw_quads(&mut fb.target(), [4.0, 1.0], &q, QuadBlend::Blend, scissor);
        let px: Vec<_> = fb.pixels().iter().map(|&p| unpack(p)).collect();
        assert_eq!(px, [[0; 4], [0, 0, 255, 255], [0, 0, 255, 255], [0; 4]]);
    }
}
