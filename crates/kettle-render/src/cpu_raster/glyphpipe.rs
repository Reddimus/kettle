//! `glyphpipe.rs` on the CPU: grid text, built-in glyphs and emoji.
//!
//! Per the WGSL, on an sRGB target:
//!
//! - A mask glyph (`kind != 0`) returns `(lin(fg.rgb), fg.a * coverage)`.
//! - A colour glyph (`kind == 0`) returns
//!   `(mix(lin(color.rgb), texel.rgb, color.a), texel.a)`: `color.rgb` is
//!   the pane's dimming backdrop and `color.a` the brightness it keeps.
//! - Both are straight alpha under `ALPHA_BLENDING`: colour `src * a +
//!   dst * (1 - a)`, alpha `a + dst_a * (1 - a)`.
//!
//! Atlas UVs are atlas pixels normalized by the texture's size, sampled
//! nearest with clamp-to-edge at the interpolated pixel centre. A glyph drawn
//! at its bitmap size on whole pixels maps texel to pixel, which is the fast
//! path here; anything else (fitted emoji, fractional positions) samples per
//! pixel exactly as the sampler does.

use super::srgb::{SrgbTables, srgb_to_linear, tables, unorm8};
use super::{
    CpuTarget, CpuTextureFormat, CpuTextureView, PixelRect, Raster, pack, rasterize,
    snap_to_subpixel, unpack,
};
use crate::glyphpipe::GlyphInstance;

/// The two atlases a glyph draw binds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GlyphAtlases<'a> {
    /// RGBA8, sRGB on an sRGB target (`kind == 0`).
    pub(crate) color: CpuTextureView<'a>,
    /// R8 coverage (`kind != 0`).
    pub(crate) mask: CpuTextureView<'a>,
}

#[cfg(test)]
/// The scissor `GlyphPipeline::draw` sets for a pane `rect` (`[x, y, w, h]`)
/// on a `target_size` surface: the pane clamped to the surface and truncated
/// to whole pixels. `None` when nothing of it is on the surface.
pub(crate) fn pane_scissor(rect: [f32; 4], target_size: [u32; 2]) -> Option<PixelRect> {
    crate::glyphpipe::pane_scissor(rect, target_size).map(|[x, y, w, h]| PixelRect::new(x, y, w, h))
}

/// Draw `glyphs` in order, as `GlyphPipeline` does on a `screen`-sized
/// uniform, under an optional scissor. The caller walks `GlyphClip`s and
/// the cursor's exclusion strips, one call per scissor, as the GPU path
/// sets one scissor per draw.
pub(crate) fn draw_glyphs(
    target: &mut CpuTarget<'_>,
    screen: [f32; 2],
    glyphs: &[GlyphInstance],
    atlases: GlyphAtlases<'_>,
    scissor: Option<PixelRect>,
) {
    let Some(clip) = target.clip(scissor) else {
        return;
    };
    let t = tables();
    let viewport = target.viewport();
    let mut state = MaskState::new(t);
    for g in glyphs {
        let Some(r) = rasterize(&viewport, screen, g.pos, g.size, clip) else {
            continue;
        };
        let atlas = if g.kind == 0 {
            atlases.color
        } else {
            atlases.mask
        };
        if atlas.format() != expected_format(g.kind) {
            continue;
        }
        let map = TexelMap::new(&r, g, atlas);
        if g.kind == 0 {
            draw_color(t, target, &r, &map, g, atlas);
        } else {
            draw_mask(t, target, &r, &map, g, atlas, &mut state);
        }
    }
}

fn expected_format(kind: u32) -> CpuTextureFormat {
    if kind == 0 {
        CpuTextureFormat::Rgba8UnormSrgb
    } else {
        CpuTextureFormat::R8Unorm
    }
}

/// Which atlas texel each covered pixel samples.
struct TexelMap {
    /// The UV planes evaluated at framebuffer pixel (0, 0). The strip's
    /// first triangle starts at the left endpoint, the second at the
    /// right. They share the V origin and both slopes.
    u_at_zero: [f32; 2],
    v_at_zero: f32,
    du: f32,
    dv: f32,
    /// The shared diagonal runs between the top right and bottom left.
    right: f64,
    top: f64,
    diagonal_dx: f64,
    diagonal_dy: f64,
    shared_edge_in_second: bool,
    dims: [f32; 2],
    max: [u32; 2],
    /// Texel of the first covered column and row when the glyph maps texel
    /// to pixel, so every later one is the next texel.
    direct: Option<[u32; 2]>,
}

/// How far from a whole pixel a corner may land and still map texel to
/// pixel: sampling happens half a texel in, so this cannot change a texel.
const WHOLE: f32 = 1.0e-3;

impl TexelMap {
    fn new(r: &Raster, g: &GlyphInstance, atlas: CpuTextureView<'_>) -> Self {
        let dims = [atlas.width() as f32, atlas.height() as f32];
        let max = [atlas.width() - 1, atlas.height() - 1];
        // The grid's case: a bitmap drawn at its size on whole pixels. The
        // pixel-to-NDC round trip leaves its corners a few ulps off whole,
        // and each pixel samples its texel half a texel in, so the texel is
        // the pixel's offset from the corner.
        let near_whole = |v: f32| (v - v.round()).abs() <= WHOLE;
        let one_to_one = (r.x1 - r.x0 - g.uv_size[0]).abs() <= WHOLE
            && (r.y1 - r.y0 - g.uv_size[1]).abs() <= WHOLE
            && near_whole(r.x0)
            && near_whole(r.y0)
            && [g.uv[0], g.uv[1], g.uv_size[0], g.uv_size[1]]
                .into_iter()
                .all(|v| v.fract() == 0.0 && v >= 0.0);
        if one_to_one {
            let tx = g.uv[0] + (r.cols.start as f32 - r.x0.round());
            let ty = g.uv[1] + (r.rows.start as f32 - r.y0.round());
            let (w, h) = (r.cols.end - r.cols.start, r.rows.end - r.rows.start);
            // Keep the fast path only where it cannot meet the clamp.
            if tx >= 0.0 && ty >= 0.0 && tx + w as f32 <= dims[0] && ty + h as f32 <= dims[1] {
                return Self {
                    u_at_zero: [0.0; 2],
                    v_at_zero: 0.0,
                    du: 0.0,
                    dv: 0.0,
                    right: 0.0,
                    top: 0.0,
                    diagonal_dx: 0.0,
                    diagonal_dy: 0.0,
                    shared_edge_in_second: false,
                    dims,
                    max,
                    direct: Some([tx as u32, ty as u32]),
                };
            }
        }
        let (u0, u1) = (g.uv[0] / dims[0], (g.uv[0] + g.uv_size[0]) / dims[0]);
        let (v0, v1) = (g.uv[1] / dims[1], (g.uv[1] + g.uv_size[1]) / dims[1]);
        let w = r.x1 - r.x0;
        let h = r.y1 - r.y0;
        // llvmpipe's triangle setup multiplies the orthogonal edge by
        // the reciprocal area before applying the UV delta. Directly
        // dividing by the axis length can round to the other side of an
        // exact texel edge.
        let inv_area = 1.0 / (w * h);
        let du = (u1 - u0) * (h * inv_area);
        let dv = (v1 - v0) * (w * inv_area);
        let [left, right, top, bottom] =
            [r.x0, r.x1, r.y0, r.y1].map(|v| f64::from(snap_to_subpixel(v)));
        let dx = right - left;
        let dy = bottom - top;
        Self {
            u_at_zero: [u0 - (r.x0 - 0.5) * du, u1 - (r.x1 - 0.5) * du],
            v_at_zero: v0 - (r.y0 - 0.5) * dv,
            du,
            dv,
            right,
            top,
            diagonal_dx: dx.abs().copysign(dy),
            diagonal_dy: dy.abs().copysign(dx),
            shared_edge_in_second: dx > 0.0,
            dims,
            max,
            direct: None,
        }
    }

    #[inline(always)]
    fn texel_x(&self, x: u32, y: u32) -> u32 {
        let row = (f64::from(y) - self.top) * self.diagonal_dx;
        let col = (self.right - f64::from(x)) * self.diagonal_dy;
        let second = row > col || (row == col && self.shared_edge_in_second);
        let u = (x as f32).mul_add(self.du, self.u_at_zero[usize::from(second)]);
        nearest(u, self.dims[0], self.max[0])
    }

    #[inline(always)]
    fn texel_y(&self, y: u32) -> u32 {
        let v = (y as f32).mul_add(self.dv, self.v_at_zero);
        nearest(v, self.dims[1], self.max[1])
    }
}

/// Nearest selects `floor(coord * dim)`, using the interpolated f32
/// coordinate and f32 product. An exact integer selects that texel, even
/// if the unrounded UV was just below the boundary.
#[inline(always)]
fn nearest(coord: f32, dim: f32, max: u32) -> u32 {
    let i = (coord * dim).floor();
    if i <= 0.0 { 0 } else { (i as u32).min(max) }
}

/// A mask foreground and the results of blending it at each coverage over
/// the last two pixel values seen there. Consecutive glyphs mostly share a
/// foreground, and text sits on flat cell backgrounds, so both repeat.
struct MaskState {
    fg: [u32; 4],
    ink: MaskInk,
    /// Two `(dst, out)` ways per coverage, valid while stamped with
    /// `generation`; the newer way first.
    entries: [[(u32, u32); 2]; 256],
    stamps: [[u32; 2]; 256],
    generation: u32,
}

impl MaskState {
    fn new(t: &SrgbTables) -> Self {
        Self {
            fg: [0; 4],
            ink: MaskInk::new(t, [0.0; 4]),
            entries: [[(0, 0); 2]; 256],
            stamps: [[0; 2]; 256],
            generation: 1,
        }
    }

    /// Switch to `color`, keeping the cached blends when it is unchanged.
    #[inline(always)]
    fn set_color(&mut self, t: &SrgbTables, color: [f32; 4]) {
        let fg = color.map(f32::to_bits);
        if fg == self.fg {
            return;
        }
        self.fg = fg;
        self.ink = MaskInk::new(t, color);
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.stamps = [[0; 2]; 256];
            self.generation = 1;
        }
    }

    /// The foreground at `cov` over `dst`.
    #[inline(always)]
    fn shade(&mut self, t: &SrgbTables, dst: u32, cov: u8) -> u32 {
        let i = usize::from(cov);
        let (ways, stamps) = (&mut self.entries[i], &mut self.stamps[i]);
        for way in 0..2 {
            if stamps[way] == self.generation && ways[way].0 == dst {
                return ways[way].1;
            }
        }
        let out = self.ink.blend(t, dst, cov);
        ways[1] = ways[0];
        stamps[1] = stamps[0];
        ways[0] = (dst, out);
        stamps[0] = self.generation;
        out
    }
}

/// A mask glyph's foreground, prepared once per instance.
struct MaskInk {
    /// Linear foreground, 16-bit.
    lin: [u32; 3],
    /// Linear foreground in `f32`, for alphas the integer path cannot hold.
    linf: [f32; 3],
    alpha: f32,
    /// The pixel full coverage stores when the foreground is opaque.
    solid: Option<u32>,
}

impl MaskInk {
    fn new(t: &SrgbTables, color: [f32; 4]) -> Self {
        let linf = [0, 1, 2].map(|c| srgb_to_linear(color[c]));
        let alpha = color[3];
        let solid = (alpha == 1.0)
            .then(|| pack(t.encode(linf[0]), t.encode(linf[1]), t.encode(linf[2]), 255));
        Self {
            lin: linf.map(|v| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u32),
            linf,
            alpha,
            solid,
        }
    }

    /// Whether the 16-bit integer blend represents this foreground.
    fn integer(&self) -> bool {
        self.linf.iter().all(|v| (0.0..=1.0).contains(v)) && (0.0..=1.0).contains(&self.alpha)
    }

    /// `ALPHA_BLENDING` of the foreground at `cov` over `dst`.
    #[inline(always)]
    fn blend(&self, t: &SrgbTables, dst: u32, cov: u8) -> u32 {
        let a = self.alpha * t.unorm[usize::from(cov)];
        // `a` in 1/65536 steps; the products stay inside u32.
        let aq = (a * 65536.0 + 0.5) as u32;
        let keep = 65536 - aq;
        let [dr, dg, db, da] = unpack(dst);
        let ch = |s: u32, d: u8| {
            let lin = (s * aq + u32::from(t.dec16[usize::from(d)]) * keep + 32768) >> 16;
            t.encode16(lin.min(65535) as u16)
        };
        pack(
            ch(self.lin[0], dr),
            ch(self.lin[1], dg),
            ch(self.lin[2], db),
            ((aq * 255 + u32::from(da) * keep + 32768) >> 16).min(255) as u8,
        )
    }

    /// The same blend in `f32`, for out-of-range colours.
    fn blend_f32(&self, t: &SrgbTables, dst: u32, cov: u8) -> u32 {
        let a = self.alpha * t.unorm[usize::from(cov)];
        straight_over(t, dst, self.linf, a)
    }
}

/// `ALPHA_BLENDING` of straight linear `rgb` at alpha `a` over `dst`.
#[inline(always)]
fn straight_over(t: &SrgbTables, dst: u32, rgb: [f32; 3], a: f32) -> u32 {
    let [dr, dg, db, da] = unpack(dst);
    let k = 1.0 - a;
    pack(
        t.encode(rgb[0] * a + t.decf[usize::from(dr)] * k),
        t.encode(rgb[1] * a + t.decf[usize::from(dg)] * k),
        t.encode(rgb[2] * a + t.decf[usize::from(db)] * k),
        unorm8(a + t.unorm[usize::from(da)] * k),
    )
}

fn draw_mask(
    t: &SrgbTables,
    target: &mut CpuTarget<'_>,
    r: &Raster,
    map: &TexelMap,
    g: &GlyphInstance,
    atlas: CpuTextureView<'_>,
    state: &mut MaskState,
) {
    state.set_color(t, g.color);
    if !state.ink.integer() {
        let ink = &state.ink;
        for_each_texel(target, r, map, atlas, 1, |p, texel| {
            *p = ink.blend_f32(t, *p, texel[0]);
        });
        return;
    }
    let solid = state.ink.solid;
    let mut shade = |p: &mut u32, cov: u8| match (cov, solid) {
        (0, _) => {}
        (255, Some(solid)) => *p = solid,
        _ => *p = state.shade(t, *p, cov),
    };
    if let Some([tx, ty]) = map.direct {
        let stride = atlas.width() as usize;
        let data = atlas.data();
        let w = (r.cols.end - r.cols.start) as usize;
        for (i, y) in r.rows.clone().enumerate() {
            let start = (ty as usize + i) * stride + tx as usize;
            let src = &data[start..start + w];
            let dst = target.span(y, r.cols.start, r.cols.end);
            for (p, &cov) in dst.iter_mut().zip(src) {
                shade(p, cov);
            }
        }
        return;
    }
    for_each_texel(target, r, map, atlas, 1, |p, texel| shade(p, texel[0]));
}

fn draw_color(
    t: &SrgbTables,
    target: &mut CpuTarget<'_>,
    r: &Raster,
    map: &TexelMap,
    g: &GlyphInstance,
    atlas: CpuTextureView<'_>,
) {
    let dim = [0, 1, 2].map(|c| srgb_to_linear(g.color[c]));
    let keep = g.color[3];
    let decode = &t.decf;
    for_each_texel(target, r, map, atlas, 4, |p, texel| {
        let a = t.unorm[usize::from(texel[3])];
        // `mix(dim, texel, keep)`.
        let rgb = [0, 1, 2].map(|c| {
            let s = decode[usize::from(texel[c])];
            dim[c] * (1.0 - keep) + s * keep
        });
        *p = straight_over(t, *p, rgb, a);
    });
}

/// Visit every covered pixel with the texel it samples (`bpp` bytes).
#[inline(always)]
fn for_each_texel(
    target: &mut CpuTarget<'_>,
    r: &Raster,
    map: &TexelMap,
    atlas: CpuTextureView<'_>,
    bpp: usize,
    mut f: impl FnMut(&mut u32, &[u8]),
) {
    let stride = atlas.width() as usize * bpp;
    let data = atlas.data();
    for y in r.rows.clone() {
        let row = match map.direct {
            Some([_, ty]) => ty + (y - r.rows.start),
            None => map.texel_y(y),
        } as usize
            * stride;
        let span = target.span(y, r.cols.start, r.cols.end);
        for (p, x) in span.iter_mut().zip(r.cols.clone()) {
            let tx = match map.direct {
                Some([tx, _]) => tx + (x - r.cols.start),
                None => map.texel_x(x, y),
            } as usize;
            let at = row + tx * bpp;
            f(p, &data[at..at + bpp]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_raster::{CpuFramebuffer, CpuTexture};

    fn glyph(pos: [f32; 2], size: [f32; 2], uv: [f32; 2], uv_size: [f32; 2]) -> GlyphInstance {
        GlyphInstance {
            pos,
            size,
            uv,
            uv_size,
            color: [1.0, 1.0, 1.0, 1.0],
            kind: 1,
            _pad: [0],
        }
    }

    fn atlases(mask: &CpuTexture, color: &CpuTexture) -> GlyphAtlases<'static> {
        // Tests leak two small textures to keep the signature simple.
        let mask: &'static CpuTexture = Box::leak(Box::new(mask.clone()));
        let color: &'static CpuTexture = Box::leak(Box::new(color.clone()));
        GlyphAtlases {
            color: color.view(),
            mask: mask.view(),
        }
    }

    #[test]
    fn full_coverage_writes_the_foreground_and_zero_leaves_the_pixel() {
        let mut mask = CpuTexture::new(8, 8, CpuTextureFormat::R8Unorm).unwrap();
        assert!(mask.write(1, 1, 3, 1, &[255, 0, 128]));
        let color = CpuTexture::new(8, 8, CpuTextureFormat::Rgba8UnormSrgb).unwrap();
        let mut fb = CpuFramebuffer::new(4, 1).unwrap();
        fb.clear([0.0, 0.0, 0.0, 1.0]);
        let g = [glyph([0.0, 0.0], [3.0, 1.0], [1.0, 1.0], [3.0, 1.0])];
        draw_glyphs(
            &mut fb.target(),
            [4.0, 1.0],
            &g,
            atlases(&mask, &color),
            None,
        );
        let px: Vec<_> = fb.pixels().iter().map(|&p| unpack(p)).collect();
        // 128/255 coverage of white over black is 0.502 linear, ~188 sRGB.
        assert_eq!(
            px,
            [
                [255; 4],
                [0, 0, 0, 255],
                [188, 188, 188, 255],
                [0, 0, 0, 255]
            ]
        );
    }

    #[test]
    fn whole_pixel_glyphs_map_texel_to_pixel_despite_ndc_rounding() {
        let mask = CpuTexture::new(1024, 512, CpuTextureFormat::R8Unorm).unwrap();
        let viewport = crate::cpu_raster::Viewport {
            x: 0.0,
            y: 0.0,
            w: 1600.0,
            h: 1000.0,
        };
        let clip = PixelRect::new(0, 0, 1600, 1000);
        for (x, y) in [(37.0, 11.0), (1203.0, 977.0), (0.0, 0.0), (999.0, 3.0)] {
            let g = glyph([x, y], [7.0, 11.0], [20.0, 30.0], [7.0, 11.0]);
            let r = rasterize(&viewport, [1600.0, 1000.0], g.pos, g.size, clip).unwrap();
            let map = TexelMap::new(&r, &g, mask.view());
            assert_eq!(map.direct, Some([20, 30]), "({x}, {y})");
        }
        let g = glyph([37.25, 11.0], [7.0, 11.0], [20.0, 30.0], [7.0, 11.0]);
        let r = rasterize(&viewport, [1600.0, 1000.0], g.pos, g.size, clip).unwrap();
        assert_eq!(TexelMap::new(&r, &g, mask.view()).direct, None);
    }

    #[test]
    fn a_scaled_glyph_samples_the_nearest_texel() {
        let mut mask = CpuTexture::new(4, 4, CpuTextureFormat::R8Unorm).unwrap();
        assert!(mask.write(0, 0, 2, 1, &[255, 0]));
        let color = CpuTexture::new(4, 4, CpuTextureFormat::Rgba8UnormSrgb).unwrap();
        let mut fb = CpuFramebuffer::new(4, 1).unwrap();
        fb.clear([0.0, 0.0, 0.0, 1.0]);
        let g = [glyph([0.0, 0.0], [4.0, 1.0], [0.0, 0.0], [2.0, 1.0])];
        draw_glyphs(
            &mut fb.target(),
            [4.0, 1.0],
            &g,
            atlases(&mask, &color),
            None,
        );
        let red: Vec<_> = fb.pixels().iter().map(|&p| unpack(p)[0]).collect();
        assert_eq!(red, [255, 255, 0, 0]);
    }

    #[test]
    fn colour_glyphs_mix_toward_the_dimming_backdrop() {
        let mask = CpuTexture::new(4, 4, CpuTextureFormat::R8Unorm).unwrap();
        let mut color = CpuTexture::new(4, 4, CpuTextureFormat::Rgba8UnormSrgb).unwrap();
        assert!(color.write(0, 0, 1, 1, &[255, 0, 0, 255]));
        let mut fb = CpuFramebuffer::new(1, 1).unwrap();
        let mut g = glyph([0.0, 0.0], [1.0, 1.0], [0.0, 0.0], [1.0, 1.0]);
        g.kind = 0;
        g.color = [0.0, 0.0, 1.0, 0.5];
        draw_glyphs(
            &mut fb.target(),
            [1.0, 1.0],
            &[g],
            atlases(&mask, &color),
            None,
        );
        assert_eq!(unpack(fb.pixels()[0]), [188, 0, 188, 255]);
    }
}
