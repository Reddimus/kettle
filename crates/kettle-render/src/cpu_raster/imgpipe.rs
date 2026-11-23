//! `imgpipe.rs` on the CPU: terminal images, wallpaper, posters, previews.
//!
//! Per the WGSL: the interpolated UV is clamped to the crop's texel centres
//! when `uv_clamp.x >= 0`, then four texels around it are fetched one by
//! one (nearest, so each fetch is a single texel), decoded from sRGB,
//! premultiplied by their own alpha and weighted bilinearly. The result is
//! premultiplied and blends with `PREMULTIPLIED_ALPHA_BLENDING`. Clamp and
//! repeat layers differ only in how a fetch outside the texture wraps.

use super::srgb::{SrgbTables, tables};
use super::{
    AddressMode, CpuTarget, CpuTextureFormat, CpuTextureView, PixelRect, Premul, blend_premul,
    rasterize, replace,
};
use crate::imgpipe::Inst;

/// Draw one image instance from `texture`, as `ImagePipeline::draw` does
/// for an item on a `screen`-sized uniform. A repeating layer item uses
/// [`AddressMode::Repeat`], every other one [`AddressMode::ClampToEdge`].
pub(crate) fn draw_image(
    target: &mut CpuTarget<'_>,
    screen: [f32; 2],
    inst: &Inst,
    texture: CpuTextureView<'_>,
    address: AddressMode,
    scissor: Option<PixelRect>,
) {
    let Some(clip) = target.clip(scissor) else {
        return;
    };
    let decode = match texture.format() {
        CpuTextureFormat::Rgba8UnormSrgb => true,
        CpuTextureFormat::Rgba8Unorm => false,
        CpuTextureFormat::R8Unorm => return,
    };
    let t = tables();
    let Some(r) = rasterize(&target.viewport(), screen, inst.pos, inst.size, clip) else {
        return;
    };
    let sampler = Sampler::new(t, texture, address, decode);
    let u = [inst.uv_origin[0], inst.uv_origin[0] + inst.uv_size[0]];
    let v = [inst.uv_origin[1], inst.uv_origin[1] + inst.uv_size[1]];
    let clamp = (inst.uv_clamp[0] >= 0.0).then_some(inst.uv_clamp);
    let dims = [texture.width() as f32, texture.height() as f32];
    // U depends only on the column: resolve each column's two taps once.
    let cols: Vec<Tap> = r
        .cols
        .clone()
        .map(|x| {
            let mut ux = r.lerp_x(x, u[0], u[1]);
            if let Some(c) = clamp {
                ux = ux.max(c[0]).min(c[2]);
            }
            sampler.tap(Axis::new(ux, dims[0]), texture.width(), 4)
        })
        .collect();
    let stride = texture.width() as usize * 4;
    for y in r.rows.clone() {
        let mut vy = r.lerp_y(y, v[0], v[1]);
        if let Some(c) = clamp {
            vy = vy.max(c[1]).min(c[3]);
        }
        let row = sampler.tap(Axis::new(vy, dims[1]), texture.height(), stride);
        let span = target.span(y, r.cols.start, r.cols.end);
        for (p, col) in span.iter_mut().zip(&cols) {
            let src = sampler.bilinear(col, &row);
            if src.a == 1.0 {
                // `src + dst * 0`: the destination drops out.
                *p = replace(t, src);
            } else if src.a != 0.0 || src.r != 0.0 || src.g != 0.0 || src.b != 0.0 {
                *p = blend_premul(t, *p, src);
            }
        }
    }
}

/// Two neighbouring texels along one axis, as byte offsets, and the
/// second's weight.
#[derive(Clone, Copy)]
struct Tap {
    at: [usize; 2],
    weight: f32,
}

/// One axis of the shader's footprint: the lower texel and the upper's
/// weight.
#[derive(Clone, Copy)]
struct Axis {
    base: i64,
    weight: f32,
}

impl Axis {
    #[inline(always)]
    fn new(coord: f32, dim: f32) -> Self {
        let position = coord * dim - 0.5;
        let base = position.floor();
        Self {
            base: base.clamp(-1.0e12, 1.0e12) as i64,
            weight: position - base,
        }
    }
}

struct Sampler<'a> {
    t: &'a SrgbTables,
    texture: CpuTextureView<'a>,
    address: AddressMode,
    decode: &'a [f32; 256],
}

impl<'a> Sampler<'a> {
    fn new(
        t: &'a SrgbTables,
        texture: CpuTextureView<'a>,
        address: AddressMode,
        decode: bool,
    ) -> Self {
        Self {
            t,
            texture,
            address,
            decode: if decode { &t.decf } else { &t.unorm },
        }
    }

    #[inline(always)]
    fn wrap(&self, i: i64, dim: u32) -> usize {
        match self.address {
            AddressMode::ClampToEdge => i.clamp(0, i64::from(dim) - 1) as usize,
            AddressMode::Repeat => i.rem_euclid(i64::from(dim)) as usize,
        }
    }

    /// The texels the shader fetches along one axis: `base` and `base + 1`,
    /// each a single nearest texel at its centre, wrapped by the sampler.
    fn tap(&self, axis: Axis, dim: u32, scale: usize) -> Tap {
        Tap {
            at: [
                self.wrap(axis.base, dim) * scale,
                self.wrap(axis.base + 1, dim) * scale,
            ],
            weight: axis.weight,
        }
    }

    /// The premultiplied texel at byte offset `at`.
    #[inline(always)]
    fn fetch(&self, at: usize) -> [f32; 4] {
        let texel = &self.texture.data()[at..at + 4];
        let a = self.t.unorm[usize::from(texel[3])];
        [
            self.decode[usize::from(texel[0])] * a,
            self.decode[usize::from(texel[1])] * a,
            self.decode[usize::from(texel[2])] * a,
            a,
        ]
    }

    /// The shader's loop: `y` outer, `x` inner, weights `(1 - w)` and `w`.
    #[inline(always)]
    fn bilinear(&self, col: &Tap, row: &Tap) -> Premul {
        let mut c = [0f32; 4];
        for dy in 0..2 {
            let wy = if dy == 1 {
                row.weight
            } else {
                1.0 - row.weight
            };
            for dx in 0..2 {
                let wx = if dx == 1 {
                    col.weight
                } else {
                    1.0 - col.weight
                };
                let texel = self.fetch(row.at[dy] + col.at[dx]);
                for (acc, v) in c.iter_mut().zip(texel) {
                    *acc += v * wx * wy;
                }
            }
        }
        Premul {
            r: c[0],
            g: c[1],
            b: c[2],
            a: c[3],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_raster::{CpuFramebuffer, unpack};

    fn inst(pos: [f32; 2], size: [f32; 2]) -> Inst {
        Inst {
            pos,
            size,
            uv_origin: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            uv_clamp: [-1.0; 4],
        }
    }

    #[test]
    fn a_transparent_texel_does_not_bleed_its_colour() {
        // Red opaque next to transparent green: halfway is half-opaque red.
        let rgba = [255, 0, 0, 255, 0, 255, 0, 0];
        let tex = CpuTextureView::new(2, 1, CpuTextureFormat::Rgba8UnormSrgb, &rgba).unwrap();
        let mut fb = CpuFramebuffer::new(4, 1).unwrap();
        let i = inst([0.0, 0.0], [4.0, 1.0]);
        draw_image(
            &mut fb.target(),
            [4.0, 1.0],
            &i,
            tex,
            AddressMode::ClampToEdge,
            None,
        );
        let px: Vec<_> = fb.pixels().iter().map(|&p| unpack(p)).collect();
        assert_eq!(px[0], [255, 0, 0, 255]);
        // u = 0.375 is a quarter of the way to the transparent texel.
        assert_eq!(px[1], [225, 0, 0, 191]);
        assert_eq!(px[2][1], 0, "no green from the transparent texel");
        assert_eq!(px[3], [0; 4]);
    }

    #[test]
    fn a_unorm_texture_is_sampled_without_decoding() {
        let rgba = [128, 0, 0, 255];
        let tex = CpuTextureView::new(1, 1, CpuTextureFormat::Rgba8Unorm, &rgba).unwrap();
        let mut fb = CpuFramebuffer::new(1, 1).unwrap();
        let i = inst([0.0, 0.0], [1.0, 1.0]);
        draw_image(
            &mut fb.target(),
            [1.0, 1.0],
            &i,
            tex,
            AddressMode::ClampToEdge,
            None,
        );
        // 128/255 taken as linear light encodes to 188.
        assert_eq!(unpack(fb.pixels()[0]), [188, 0, 0, 255]);
    }

    #[test]
    fn repeat_wraps_and_clamp_holds_the_edge() {
        let rgba = [255, 255, 255, 255, 0, 0, 0, 255];
        let tex = CpuTextureView::new(2, 1, CpuTextureFormat::Rgba8UnormSrgb, &rgba).unwrap();
        let mut i = inst([0.0, 0.0], [4.0, 1.0]);
        i.uv_size = [2.0, 1.0];
        let mut fb = CpuFramebuffer::new(4, 1).unwrap();
        draw_image(
            &mut fb.target(),
            [4.0, 1.0],
            &i,
            tex,
            AddressMode::Repeat,
            None,
        );
        let repeat: Vec<_> = fb.pixels().iter().map(|&p| unpack(p)[0]).collect();
        assert_eq!(repeat, [255, 0, 255, 0]);
        let mut fb = CpuFramebuffer::new(4, 1).unwrap();
        draw_image(
            &mut fb.target(),
            [4.0, 1.0],
            &i,
            tex,
            AddressMode::ClampToEdge,
            None,
        );
        let clamped: Vec<_> = fb.pixels().iter().map(|&p| unpack(p)[0]).collect();
        assert_eq!(clamped, [255, 0, 0, 0]);
    }
}
