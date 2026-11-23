//! glyphon behind the backend seam.
//!
//! glyphon's `Cache`, `TextAtlas`, `Viewport` and `TextRenderer` each create
//! wgpu objects. The renderer uses these stand-ins of the same names and the
//! same calls instead. On the GPU they are glyphon, unchanged. On the CPU:
//!
//! - [`TextRenderer::prepare`] runs glyphon's own layout pass over the text
//!   areas (visible runs, physical glyph positions and cache keys, colours,
//!   clip bounds, custom glyphs) and keeps the result as [`CpuTextGlyph`]s,
//!   which is everything glyphon's `prepare_glyph` starts from.
//! - [`TextRenderer::render`] hands those glyphs to the pass as one
//!   [`CpuDraw::Text`](super::cpu::CpuDraw::Text).
//!
//! What glyphon does next, rasterizing each key through `SwashCache`, placing
//! it by its raster bearings, clipping it to its bounds and blending it, is
//! the CPU text compositor's job ([`CpuTextAtlas`] is where it caches the
//! rasters).

use glyphon::cosmic_text::{CacheKey, SubpixelBin};
use glyphon::{
    Color, ContentType, FontSystem, PrepareError, RasterizeCustomGlyphRequest,
    RasterizedCustomGlyph, RenderError, Resolution, SwashCache, TextArea,
};

use super::cpu::CpuDraw;
use super::{DeviceRef, ScenePass};

/// glyphon's pipeline cache, or nothing on the CPU.
pub(crate) enum Cache {
    Gpu(glyphon::Cache),
    Cpu,
}

impl Cache {
    pub(crate) fn new(device: DeviceRef<'_>) -> Self {
        match device {
            DeviceRef::Gpu { device, .. } => Self::Gpu(glyphon::Cache::new(device)),
            DeviceRef::Cpu => Self::Cpu,
        }
    }
}

/// What a CPU text glyph's raster is looked up by: glyphon's cache key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CpuTextKey {
    /// A font glyph, rasterized through `SwashCache::get_image_uncached`.
    Text(CacheKey),
    /// A custom glyph, with its raster already in [`CpuTextGlyph::custom`].
    Custom {
        id: u16,
        width: u16,
        height: u16,
        x_bin: SubpixelBin,
        y_bin: SubpixelBin,
    },
}

/// A custom glyph's raster, as the renderer's callback returned it.
#[expect(dead_code, reason = "the CPU text compositor reads these")]
#[derive(Clone, Debug)]
pub(crate) struct CpuCustomRaster {
    pub(crate) color: bool,
    pub(crate) data: Vec<u8>,
}

/// One glyph glyphon would prepare, before its raster is placed: glyphon's
/// `GlyphMetadata` and `GlyphBounds` for it.
///
/// The compositor finishes it as glyphon's `prepare_glyph` does: with the
/// raster's `left`, `top`, `width` and `height`, the glyph covers
/// `x + left .. + width` by `round(line_y * scale) + y - top .. + height`,
/// is dropped when that lies wholly outside `bounds`, and is otherwise
/// clipped to it. A mask raster is drawn in `color`; a colour raster as is.
/// An empty raster draws nothing.
#[expect(dead_code, reason = "the CPU text compositor reads every field")]
#[derive(Clone, Debug)]
pub(crate) struct CpuTextGlyph {
    pub(crate) key: CpuTextKey,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) line_y: f32,
    pub(crate) scale: f32,
    pub(crate) color: Color,
    /// `[x_min, y_min, x_max, y_max]`, the area's bounds clamped to the
    /// viewport as glyphon clamps them.
    pub(crate) bounds: [i32; 4],
    pub(crate) custom: Option<CpuCustomRaster>,
}

/// The CPU text atlas. glyphon's holds every prepared raster in two GPU
/// textures; the CPU compositor keeps its rasters here.
#[derive(Debug, Default)]
pub(crate) struct CpuTextAtlas;

/// glyphon's shared text atlas.
#[expect(
    clippy::large_enum_variant,
    reason = "one per renderer, built once and never moved while drawing"
)]
pub(crate) enum TextAtlas {
    Gpu(glyphon::TextAtlas),
    Cpu(CpuTextAtlas),
}

impl TextAtlas {
    pub(crate) fn new(device: DeviceRef<'_>, cache: &Cache, format: wgpu::TextureFormat) -> Self {
        match (device, cache) {
            (DeviceRef::Gpu { device, queue }, Cache::Gpu(cache)) => {
                Self::Gpu(glyphon::TextAtlas::new(device, queue, cache, format))
            }
            _ => Self::Cpu(CpuTextAtlas),
        }
    }

    /// Let glyphs no renderer prepared since the last trim be evicted.
    pub(crate) fn trim(&mut self) {
        match self {
            Self::Gpu(atlas) => atlas.trim(),
            Self::Cpu(_) => {}
        }
    }
}

/// glyphon's viewport: the target resolution its shaders and clipping use.
pub(crate) enum Viewport {
    Gpu(glyphon::Viewport),
    Cpu(Resolution),
}

impl Viewport {
    pub(crate) fn new(device: DeviceRef<'_>, cache: &Cache) -> Self {
        match (device, cache) {
            (DeviceRef::Gpu { device, .. }, Cache::Gpu(cache)) => {
                Self::Gpu(glyphon::Viewport::new(device, cache))
            }
            _ => Self::Cpu(Resolution {
                width: 0,
                height: 0,
            }),
        }
    }

    pub(crate) fn update(&mut self, device: DeviceRef<'_>, resolution: Resolution) {
        match (self, device) {
            (Self::Gpu(viewport), DeviceRef::Gpu { queue, .. }) => {
                viewport.update(queue, resolution);
            }
            (Self::Cpu(current), _) => *current = resolution,
            (Self::Gpu(_), DeviceRef::Cpu) => {}
        }
    }

    fn resolution(&self) -> Resolution {
        match self {
            Self::Gpu(viewport) => viewport.resolution(),
            Self::Cpu(resolution) => *resolution,
        }
    }
}

/// One glyphon text renderer: the text areas prepared into it last.
pub(crate) enum TextRenderer {
    Gpu(glyphon::TextRenderer),
    Cpu(Vec<CpuTextGlyph>),
}

impl TextRenderer {
    /// A renderer drawing from `atlas`, as glyphon's is built everywhere in
    /// Kettle: single-sampled, with no depth.
    pub(crate) fn new(atlas: &mut TextAtlas, device: DeviceRef<'_>) -> Self {
        match (atlas, device) {
            (TextAtlas::Gpu(atlas), DeviceRef::Gpu { device, .. }) => Self::Gpu(
                glyphon::TextRenderer::new(atlas, device, wgpu::MultisampleState::default(), None),
            ),
            _ => Self::Cpu(Vec::new()),
        }
    }

    /// Prepare `text_areas` for the next [`Self::render`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare<'a>(
        &mut self,
        device: DeviceRef<'_>,
        font_system: &mut FontSystem,
        atlas: &mut TextAtlas,
        viewport: &Viewport,
        text_areas: impl IntoIterator<Item = TextArea<'a>>,
        cache: &mut SwashCache,
    ) -> Result<(), PrepareError> {
        self.prepare_with_custom(
            device,
            font_system,
            atlas,
            viewport,
            text_areas,
            cache,
            |_| None,
        )
    }

    /// [`Self::prepare`], with the areas' custom glyphs rasterized by
    /// `rasterize_custom_glyph`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_with_custom<'a>(
        &mut self,
        device: DeviceRef<'_>,
        font_system: &mut FontSystem,
        atlas: &mut TextAtlas,
        viewport: &Viewport,
        text_areas: impl IntoIterator<Item = TextArea<'a>>,
        cache: &mut SwashCache,
        rasterize_custom_glyph: impl FnMut(RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph>,
    ) -> Result<(), PrepareError> {
        match (self, device, atlas, viewport) {
            (
                Self::Gpu(renderer),
                DeviceRef::Gpu { device, queue },
                TextAtlas::Gpu(atlas),
                Viewport::Gpu(viewport),
            ) => renderer.prepare_with_custom(
                device,
                queue,
                font_system,
                atlas,
                viewport,
                text_areas,
                cache,
                rasterize_custom_glyph,
            ),
            (Self::Cpu(glyphs), DeviceRef::Cpu, TextAtlas::Cpu(_), viewport) => {
                glyphs.clear();
                lay_out_text(
                    glyphs,
                    viewport.resolution(),
                    text_areas,
                    rasterize_custom_glyph,
                );
                Ok(())
            }
            _ => {
                debug_assert!(false, "a text renderer was used with another backend");
                Ok(())
            }
        }
    }

    /// Draw what the last prepare laid out.
    pub(crate) fn render(
        &self,
        atlas: &TextAtlas,
        viewport: &Viewport,
        pass: &mut ScenePass<'_, '_>,
    ) -> Result<(), RenderError> {
        match (self, atlas, viewport, pass) {
            (
                Self::Gpu(renderer),
                TextAtlas::Gpu(atlas),
                Viewport::Gpu(viewport),
                ScenePass::Gpu(pass),
            ) => renderer.render(atlas, viewport, pass),
            (Self::Cpu(glyphs), TextAtlas::Cpu(atlas), _, ScenePass::Cpu(pass)) => {
                pass.draw(CpuDraw::Text { glyphs, atlas });
                Ok(())
            }
            _ => {
                debug_assert!(false, "a text renderer was used with another backend");
                Ok(())
            }
        }
    }

    /// How many glyphs the last CPU prepare laid out, for tests.
    #[cfg(test)]
    pub(crate) fn cpu_glyph_count(&self) -> Option<usize> {
        match self {
            Self::Gpu(_) => None,
            Self::Cpu(glyphs) => Some(glyphs.len()),
        }
    }
}

/// glyphon 0.12's `prepare_with_depth_and_custom` layout pass, up to its
/// first atlas lookup.
fn lay_out_text<'a>(
    out: &mut Vec<CpuTextGlyph>,
    resolution: Resolution,
    text_areas: impl IntoIterator<Item = TextArea<'a>>,
    mut rasterize_custom_glyph: impl FnMut(RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph>,
) {
    for area in text_areas {
        let x_min = area.bounds.left.max(0);
        let y_min = area.bounds.top.max(0);
        let bounds = [
            x_min,
            y_min,
            area.bounds.right.min(resolution.width as i32).max(x_min),
            area.bounds.bottom.min(resolution.height as i32).max(y_min),
        ];
        for glyph in area.custom_glyphs {
            let x = area.left + glyph.left * area.scale;
            let y = area.top + glyph.top * area.scale;
            let width = (glyph.width * area.scale).round() as u16;
            let height = (glyph.height * area.scale).round() as u16;
            let (x, y, x_bin, y_bin) = if glyph.snap_to_physical_pixel {
                (
                    x.round() as i32,
                    y.round() as i32,
                    SubpixelBin::Zero,
                    SubpixelBin::Zero,
                )
            } else {
                let (x, x_bin) = SubpixelBin::new(x);
                let (y, y_bin) = SubpixelBin::new(y);
                (x, y, x_bin, y_bin)
            };
            if width == 0 || height == 0 {
                continue;
            }
            let Some(raster) = rasterize_custom_glyph(RasterizeCustomGlyphRequest {
                id: glyph.id,
                width,
                height,
                x_bin,
                y_bin,
                scale: area.scale,
            }) else {
                continue;
            };
            out.push(CpuTextGlyph {
                key: CpuTextKey::Custom {
                    id: glyph.id,
                    width,
                    height,
                    x_bin,
                    y_bin,
                },
                x,
                y,
                line_y: 0.0,
                scale: area.scale,
                color: glyph.color.unwrap_or(area.default_color),
                bounds,
                custom: Some(CpuCustomRaster {
                    color: raster.content_type == ContentType::Color,
                    data: raster.data,
                }),
            });
        }
        let is_run_visible = |run: &glyphon::cosmic_text::LayoutRun| {
            let start = (area.top + run.line_top * area.scale) as i32;
            let end = start + (run.line_height * area.scale) as i32;
            start <= area.bounds.bottom && area.bounds.top <= end
        };
        for run in area
            .buffer
            .layout_runs()
            .skip_while(|run| !is_run_visible(run))
            .take_while(is_run_visible)
        {
            for glyph in run.glyphs {
                let physical = glyph.physical((area.left, area.top), area.scale);
                out.push(CpuTextGlyph {
                    key: CpuTextKey::Text(physical.cache_key),
                    x: physical.x,
                    y: physical.y,
                    line_y: run.line_y,
                    scale: area.scale,
                    color: glyph.color_opt.unwrap_or(area.default_color),
                    bounds,
                    custom: None,
                });
            }
        }
    }
}
