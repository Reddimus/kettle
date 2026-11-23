//! glyphon behind the backend seam.
//!
//! glyphon's `Cache`, `TextAtlas`, `Viewport` and `TextRenderer` each create
//! wgpu objects. The renderer uses these stand-ins of the same names and the
//! same calls instead. On the GPU they are glyphon, unchanged. On the CPU they
//! do what glyphon 0.12 does, step for step:
//!
//! - [`TextRenderer::prepare`] runs glyphon's layout pass over the text areas
//!   (visible runs, physical glyph positions and cache keys, colours, clip
//!   bounds, custom glyphs). Each glyph's raster comes from [`CpuTextAtlas`],
//!   which rasterizes a key it does not hold through
//!   `SwashCache::get_image_uncached` (or the custom-glyph callback), and
//!   keeps and evicts rasters as glyphon's two atlases do. The glyph is then
//!   placed by its raster's bearings and clipped to its bounds as glyphon's
//!   `prepare_glyph` does, into a [`CpuTextGlyph`]: glyphon's vertex.
//! - [`TextRenderer::render`] hands those glyphs to the pass as one
//!   [`CpuDraw::Text`](super::cpu::CpuDraw::Text), which the software
//!   rasterizer draws with [`draw_cpu_text`].
//!
//! [`draw_cpu_text`] is glyphon's `shader.wgsl` and pipeline state on an
//! sRGB target. Kettle builds its atlases with `TextAtlas::new`, so glyphon
//! runs in its `ColorMode::Accurate`:
//!
//! - A mask glyph is drawn in its colour, decoded from sRGB in the vertex
//!   stage, with alpha `color.a * coverage`.
//! - A colour glyph (emoji) samples an `Rgba8UnormSrgb` atlas and is drawn
//!   as sampled; its text colour is ignored.
//! - Both blend with `BlendState::ALPHA_BLENDING` (straight alpha) and a
//!   nearest sampler. glyphon's quads are whole pixels mapped one texel to
//!   one pixel, so each pixel takes exactly its texel.
//!
//! That is the grid glyph pipeline's shading for a mask glyph, and for a
//! colour glyph with no dimming, so the draw goes through
//! [`draw_glyphs`] one glyph at a time.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use glyphon::cosmic_text::{CacheKey, SubpixelBin};
use glyphon::{
    Color, ContentType, FontSystem, PrepareError, RasterizeCustomGlyphRequest,
    RasterizedCustomGlyph, RenderError, Resolution, SwashCache, SwashContent, TextArea,
};

use super::cpu::CpuDraw;
use super::{DeviceRef, ScenePass};
use crate::cpu_raster::glyphpipe::{GlyphAtlases, draw_glyphs};
use crate::cpu_raster::{CpuTarget, CpuTextureFormat, CpuTextureView, MAX_SIDE, PixelRect};
use crate::glyphpipe::GlyphInstance;

#[cfg(test)]
#[path = "text_parity_tests.rs"]
mod parity_tests;

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
    /// A custom glyph, rasterized by the renderer's callback.
    Custom {
        id: u16,
        width: u16,
        height: u16,
        x_bin: SubpixelBin,
        y_bin: SubpixelBin,
    },
}

/// One glyph raster as the CPU atlas holds it: a coverage mask (one byte per
/// texel, `R8Unorm`) or straight sRGB colour (`Rgba8UnormSrgb`), rows packed.
pub(crate) struct CpuGlyphRaster {
    color: bool,
    width: u16,
    height: u16,
    data: Box<[u8]>,
}

impl std::fmt::Debug for CpuGlyphRaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuGlyphRaster")
            .field("color", &self.color)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl CpuGlyphRaster {
    fn view(&self) -> Option<CpuTextureView<'_>> {
        let format = if self.color {
            CpuTextureFormat::Rgba8UnormSrgb
        } else {
            CpuTextureFormat::R8Unorm
        };
        CpuTextureView::new(self.width.into(), self.height.into(), format, &self.data)
    }
}

/// One glyph to draw: glyphon's `GlyphToRender`, after placement and
/// clipping. It covers `pos .. pos + dim` in target pixels and samples its
/// raster from `uv` one texel per pixel. The raster is shared with the atlas,
/// so a renderer that draws its last prepare again draws the same pixels
/// whatever the atlas has evicted since.
#[derive(Clone, Debug)]
pub(crate) struct CpuTextGlyph {
    pub(crate) pos: [i32; 2],
    pub(crate) dim: [u16; 2],
    pub(crate) uv: [u16; 2],
    pub(crate) color: Color,
    pub(crate) raster: Arc<CpuGlyphRaster>,
}

/// glyphon's `GlyphDetails` for one cached key.
#[derive(Clone, Debug)]
struct CpuGlyphDetails {
    left: i16,
    top: i16,
    width: u16,
    height: u16,
    /// `None` is glyphon's `SkipRasterization`: an empty raster, which is
    /// cached but never drawn.
    raster: Option<Arc<CpuGlyphRaster>>,
}

/// What rasterizing a key returned: glyphon's `GetGlyphImageResult`.
struct CpuGlyphImage {
    color: bool,
    left: i16,
    top: i16,
    width: u16,
    height: u16,
    data: Vec<u8>,
}

/// One of glyphon's two atlases (`InnerAtlas`): an LRU cache of glyph
/// details, the keys prepared since the last trim, and the room the atlas
/// texture has.
///
/// glyphon packs rasters into a square texture that starts at 256 texels a
/// side and doubles when every cached glyph is in use, up to the device's
/// largest texture. Here each raster is its own buffer, and the texture is
/// only a capacity: a raster fits when it is no wider or taller than the side
/// and the rasters held cover no more than its area. Eviction, in-use
/// protection, growth and `AtlasFull` follow glyphon's `try_allocate` and
/// `grow` exactly. The one difference is packing: glyphon's packer can
/// fragment and evict a little earlier. Which glyphs are drawn, and how, is
/// the same either way.
#[derive(Debug)]
struct CpuInnerAtlas {
    side: u32,
    max_side: u32,
    /// Texels held by sized rasters.
    used: u64,
    stamp: u64,
    cache: HashMap<CpuTextKey, (CpuGlyphDetails, u64)>,
    /// Keys by last use, least recent first.
    order: BTreeMap<u64, CpuTextKey>,
    in_use: HashSet<CpuTextKey>,
}

impl CpuInnerAtlas {
    /// glyphon's `InnerAtlas::INITIAL_SIZE`.
    const INITIAL_SIDE: u32 = 256;

    fn new(side: u32, max_side: u32) -> Self {
        Self {
            side: side.min(max_side),
            max_side,
            used: 0,
            stamp: 0,
            cache: HashMap::new(),
            order: BTreeMap::new(),
            in_use: HashSet::new(),
        }
    }

    fn next_stamp(&mut self) -> u64 {
        self.stamp += 1;
        self.stamp
    }

    /// `LruCache::get`: the details, now the most recently used.
    fn get(&mut self, key: &CpuTextKey) -> Option<CpuGlyphDetails> {
        let stamp = self.next_stamp();
        let (details, last) = self.cache.get_mut(key)?;
        self.order.remove(last);
        *last = stamp;
        self.order.insert(stamp, *key);
        Some(details.clone())
    }

    fn insert(&mut self, key: CpuTextKey, details: CpuGlyphDetails) {
        let stamp = self.next_stamp();
        if details.raster.is_some() {
            self.used += area(details.width, details.height);
        }
        if let Some((old, last)) = self.cache.insert(key, (details, stamp)) {
            self.order.remove(&last);
            if old.raster.is_some() {
                self.used -= area(old.width, old.height);
            }
        }
        self.order.insert(stamp, key);
    }

    fn pop_lru(&mut self) {
        let Some((_, key)) = self.order.pop_first() else {
            return;
        };
        if let Some((details, _)) = self.cache.remove(&key)
            && details.raster.is_some()
        {
            self.used -= area(details.width, details.height);
        }
    }

    fn fits(&self, width: u16, height: u16) -> bool {
        u32::from(width) <= self.side
            && u32::from(height) <= self.side
            && self.used + area(width, height) <= u64::from(self.side) * u64::from(self.side)
    }

    /// glyphon's `InnerAtlas::try_allocate`: make room for a `width` x
    /// `height` raster by evicting the least recently used glyphs no prepare
    /// has used since the last trim. False when the next one to go is in use.
    fn try_allocate(&mut self, width: u16, height: u16) -> bool {
        loop {
            if self.fits(width, height) {
                return true;
            }
            // Evict up to and including the least recent sized glyph.
            loop {
                let Some((_, key)) = self.order.first_key_value() else {
                    return false;
                };
                if self.in_use.contains(key) {
                    return false;
                }
                let sized = self
                    .cache
                    .get(key)
                    .is_some_and(|(details, _)| details.raster.is_some());
                self.pop_lru();
                if sized {
                    break;
                }
            }
        }
    }

    /// glyphon's `InnerAtlas::grow`: double the side, up to the largest.
    fn grow(&mut self) -> bool {
        if self.side >= self.max_side {
            return false;
        }
        self.side = self.side.saturating_mul(2).min(self.max_side);
        true
    }
}

fn area(width: u16, height: u16) -> u64 {
    u64::from(width) * u64::from(height)
}

/// The CPU text atlas: glyphon's mask and colour atlases, holding the
/// rasters every CPU text renderer that shares it draws from.
#[derive(Debug)]
pub(crate) struct CpuTextAtlas {
    mask: CpuInnerAtlas,
    color: CpuInnerAtlas,
}

impl Default for CpuTextAtlas {
    fn default() -> Self {
        Self::with_sides(CpuInnerAtlas::INITIAL_SIDE, MAX_SIDE)
    }
}

impl CpuTextAtlas {
    /// Atlases starting `side` texels a side and growing up to `max_side`.
    fn with_sides(side: u32, max_side: u32) -> Self {
        Self {
            mask: CpuInnerAtlas::new(side, max_side),
            color: CpuInnerAtlas::new(side, max_side),
        }
    }

    /// glyphon's `TextAtlas::trim`: no glyph is in use any more.
    fn trim(&mut self) {
        self.mask.in_use.clear();
        self.color.in_use.clear();
    }

    /// The details of `key`, rasterizing it with `rasterize` when neither
    /// atlas holds it, as glyphon's `prepare_glyph` looks it up. `Ok(None)`
    /// when it cannot be rasterized; a raster that is not exactly its size
    /// is refused the same way.
    fn glyph(
        &mut self,
        key: CpuTextKey,
        rasterize: impl FnOnce() -> Option<CpuGlyphImage>,
    ) -> Result<Option<CpuGlyphDetails>, PrepareError> {
        if let Some(details) = self.mask.get(&key) {
            self.mask.in_use.insert(key);
            return Ok(Some(details));
        }
        if let Some(details) = self.color.get(&key) {
            self.color.in_use.insert(key);
            return Ok(Some(details));
        }
        let Some(mut image) = rasterize() else {
            return Ok(None);
        };
        let (inner, raster) = if image.width > 0 && image.height > 0 {
            let bytes = usize::from(image.width)
                * usize::from(image.height)
                * if image.color { 4 } else { 1 };
            // glyphon uploads exactly `width * height` texels and treats a
            // subpixel mask as a mask; wgpu rejects a short upload.
            if image.data.len() < bytes {
                return Ok(None);
            }
            image.data.truncate(bytes);
            let inner = if image.color {
                &mut self.color
            } else {
                &mut self.mask
            };
            while !inner.try_allocate(image.width, image.height) {
                if !inner.grow() {
                    return Err(PrepareError::AtlasFull);
                }
            }
            let raster = Arc::new(CpuGlyphRaster {
                color: image.color,
                width: image.width,
                height: image.height,
                data: image.data.into_boxed_slice(),
            });
            (inner, Some(raster))
        } else {
            (&mut self.color, None)
        };
        let details = CpuGlyphDetails {
            left: image.left,
            top: image.top,
            width: image.width,
            height: image.height,
            raster,
        };
        inner.in_use.insert(key);
        inner.insert(key, details.clone());
        Ok(Some(details))
    }

    /// How many keys each atlas caches, `(mask, colour)`, for tests.
    #[cfg(test)]
    fn cached(&self) -> (usize, usize) {
        (self.mask.cache.len(), self.color.cache.len())
    }
}

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
            _ => Self::Cpu(CpuTextAtlas::default()),
        }
    }

    /// Let glyphs no renderer prepared since the last trim be evicted.
    pub(crate) fn trim(&mut self) {
        match self {
            Self::Gpu(atlas) => atlas.trim(),
            Self::Cpu(atlas) => atlas.trim(),
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
            (Self::Cpu(glyphs), DeviceRef::Cpu, TextAtlas::Cpu(atlas), viewport) => {
                glyphs.clear();
                prepare_cpu_text(
                    glyphs,
                    atlas,
                    font_system,
                    cache,
                    viewport.resolution(),
                    text_areas,
                    rasterize_custom_glyph,
                )
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

    /// How many glyphs the last CPU prepare will draw, for tests.
    #[cfg(test)]
    pub(crate) fn cpu_glyph_count(&self) -> Option<usize> {
        self.cpu_glyphs().map(<[CpuTextGlyph]>::len)
    }

    /// The glyphs the last CPU prepare will draw, for tests.
    #[cfg(test)]
    pub(crate) fn cpu_glyphs(&self) -> Option<&[CpuTextGlyph]> {
        match self {
            Self::Gpu(_) => None,
            Self::Cpu(glyphs) => Some(glyphs),
        }
    }
}

/// glyphon's clip bounds, `[x_min, y_min, x_max, y_max]`.
type GlyphBounds = [i32; 4];

/// glyphon 0.12's `prepare_with_depth_and_custom` on the CPU: every glyph of
/// `text_areas` that glyphon would draw, into `out`.
fn prepare_cpu_text<'a>(
    out: &mut Vec<CpuTextGlyph>,
    atlas: &mut CpuTextAtlas,
    font_system: &mut FontSystem,
    cache: &mut SwashCache,
    resolution: Resolution,
    text_areas: impl IntoIterator<Item = TextArea<'a>>,
    mut rasterize_custom_glyph: impl FnMut(RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph>,
) -> Result<(), PrepareError> {
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
            let key = CpuTextKey::Custom {
                id: glyph.id,
                width,
                height,
                x_bin,
                y_bin,
            };
            let details = atlas.glyph(key, || {
                if width == 0 || height == 0 {
                    return None;
                }
                let raster = rasterize_custom_glyph(RasterizeCustomGlyphRequest {
                    id: glyph.id,
                    width,
                    height,
                    x_bin,
                    y_bin,
                    scale: area.scale,
                })?;
                let color = raster.content_type == ContentType::Color;
                // glyphon asserts the size (`RasterizedCustomGlyph::validate`);
                // the CPU drops a wrongly sized raster instead.
                let bytes = usize::from(width) * usize::from(height) * if color { 4 } else { 1 };
                (raster.data.len() == bytes).then_some(CpuGlyphImage {
                    color,
                    left: 0,
                    top: 0,
                    width,
                    height,
                    data: raster.data,
                })
            })?;
            let color = glyph.color.unwrap_or(area.default_color);
            out.extend(place_glyph(details, [x, y], 0.0, area.scale, color, bounds));
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
                let details = atlas.glyph(CpuTextKey::Text(physical.cache_key), || {
                    let image = cache.get_image_uncached(font_system, physical.cache_key)?;
                    Some(CpuGlyphImage {
                        // A subpixel mask is drawn as a mask, as glyphon does.
                        color: matches!(image.content, SwashContent::Color),
                        left: image.placement.left as i16,
                        top: image.placement.top as i16,
                        width: image.placement.width as u16,
                        height: image.placement.height as u16,
                        data: image.data,
                    })
                })?;
                let color = glyph.color_opt.unwrap_or(area.default_color);
                out.extend(place_glyph(
                    details,
                    [physical.x, physical.y],
                    run.line_y,
                    area.scale,
                    color,
                    bounds,
                ));
            }
        }
    }
    Ok(())
}

/// The rest of glyphon's `prepare_glyph`: place the raster by its bearings,
/// drop it when it lies wholly outside `bounds`, and clip it to them.
fn place_glyph(
    details: Option<CpuGlyphDetails>,
    [x, y]: [i32; 2],
    line_y: f32,
    scale: f32,
    color: Color,
    bounds: GlyphBounds,
) -> Option<CpuTextGlyph> {
    let details = details?;
    let [x_min, y_min, x_max, y_max] = bounds;
    let mut x = x + i32::from(details.left);
    let mut y = (line_y * scale).round() as i32 + y - i32::from(details.top);
    let raster = details.raster?;
    let (mut atlas_x, mut atlas_y) = (0u16, 0u16);
    let mut width = i32::from(details.width);
    let mut height = i32::from(details.height);
    let right = x + width;
    if x > x_max || right < x_min {
        return None;
    }
    let bottom = y + height;
    if y > y_max || bottom < y_min {
        return None;
    }
    if x < x_min {
        atlas_x += (x_min - x) as u16;
        x = x_min;
        width = right - x_min;
    }
    if x + width > x_max {
        width = x_max - x;
    }
    if y < y_min {
        atlas_y += (y_min - y) as u16;
        y = y_min;
        height = bottom - y_min;
    }
    if y + height > y_max {
        height = y_max - y;
    }
    Some(CpuTextGlyph {
        pos: [x, y],
        dim: [width as u16, height as u16],
        uv: [atlas_x, atlas_y],
        color,
        raster,
    })
}

/// Draw one CPU text renderer's `glyphs` into `target` under `scissor`, as
/// glyphon's pipeline draws its vertices with a `screen`-sized viewport
/// uniform (the target's size). `atlas` is the one the glyphs were prepared
/// from; each glyph already holds its raster.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the CPU raster's `CpuDraw::Text` arm calls it")
)]
pub(crate) fn draw_cpu_text(
    target: &mut CpuTarget<'_>,
    screen: [f32; 2],
    scissor: PixelRect,
    glyphs: &[CpuTextGlyph],
    _atlas: &CpuTextAtlas,
) {
    // The atlas a glyph does not sample, which `draw_glyphs` still binds.
    static UNUSED_MASK: [u8; 1] = [0];
    static UNUSED_COLOR: [u8; 4] = [0; 4];
    let (Some(unused_mask), Some(unused_color)) = (
        CpuTextureView::new(1, 1, CpuTextureFormat::R8Unorm, &UNUSED_MASK),
        CpuTextureView::new(1, 1, CpuTextureFormat::Rgba8UnormSrgb, &UNUSED_COLOR),
    ) else {
        return;
    };
    for glyph in glyphs {
        if glyph.dim[0] == 0 || glyph.dim[1] == 0 {
            continue;
        }
        let Some(view) = glyph.raster.view() else {
            continue;
        };
        let size = [f32::from(glyph.dim[0]), f32::from(glyph.dim[1])];
        let (atlases, color, kind) = if glyph.raster.color {
            // Sampled as is: `mix(_, texel, 1.0)` in the grid shader.
            (
                GlyphAtlases {
                    color: view,
                    mask: unused_mask,
                },
                [0.0, 0.0, 0.0, 1.0],
                0,
            )
        } else {
            let c = glyph.color;
            (
                GlyphAtlases {
                    color: unused_color,
                    mask: view,
                },
                [c.r(), c.g(), c.b(), c.a()].map(|v| f32::from(v) / 255.0),
                1,
            )
        };
        let instance = GlyphInstance {
            pos: [glyph.pos[0] as f32, glyph.pos[1] as f32],
            size,
            uv: [f32::from(glyph.uv[0]), f32::from(glyph.uv[1])],
            uv_size: size,
            color,
            kind,
            _pad: [0],
        };
        draw_glyphs(
            target,
            screen,
            std::slice::from_ref(&instance),
            atlases,
            Some(scissor),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_raster::{CpuFramebuffer, unpack};

    fn mask(width: u16, height: u16) -> CpuGlyphImage {
        CpuGlyphImage {
            color: false,
            left: 0,
            top: 0,
            width,
            height,
            data: vec![255; usize::from(width) * usize::from(height)],
        }
    }

    fn key(id: u16) -> CpuTextKey {
        CpuTextKey::Custom {
            id,
            width: 1,
            height: 1,
            x_bin: SubpixelBin::Zero,
            y_bin: SubpixelBin::Zero,
        }
    }

    fn details(width: u16, height: u16, left: i16, top: i16) -> CpuGlyphDetails {
        CpuGlyphDetails {
            left,
            top,
            width,
            height,
            raster: Some(Arc::new(CpuGlyphRaster {
                color: false,
                width,
                height,
                data: vec![255; usize::from(width) * usize::from(height)].into(),
            })),
        }
    }

    #[test]
    fn a_cached_key_is_rasterized_once() {
        let mut atlas = CpuTextAtlas::default();
        let mut calls = 0;
        for _ in 0..3 {
            let got = atlas
                .glyph(key(1), || {
                    calls += 1;
                    Some(mask(4, 5))
                })
                .unwrap()
                .unwrap();
            assert_eq!((got.width, got.height), (4, 5));
        }
        assert_eq!(calls, 1);
        assert_eq!(atlas.cached(), (1, 0));
    }

    #[test]
    fn empty_rasters_are_cached_in_the_colour_atlas_and_never_drawn() {
        let mut atlas = CpuTextAtlas::default();
        let got = atlas.glyph(key(1), || Some(mask(0, 0))).unwrap();
        assert_eq!(atlas.cached(), (0, 1));
        assert!(place_glyph(got, [0, 0], 0.0, 1.0, Color::rgb(1, 2, 3), [0, 0, 9, 9]).is_none());
        // A key that cannot be rasterized is not cached.
        assert!(atlas.glyph(key(2), || None).unwrap().is_none());
        assert_eq!(atlas.cached(), (0, 1));
    }

    #[test]
    fn a_short_raster_is_refused() {
        let mut atlas = CpuTextAtlas::default();
        let mut short = mask(4, 4);
        short.data.truncate(15);
        assert!(atlas.glyph(key(1), || Some(short)).unwrap().is_none());
        assert_eq!(atlas.cached(), (0, 0));
    }

    #[test]
    fn a_full_atlas_evicts_the_least_recent_glyph_not_in_use() {
        // 8x8 texels: four 4x4 glyphs.
        let mut atlas = CpuTextAtlas::with_sides(8, 8);
        for id in 0..4 {
            atlas.glyph(key(id), || Some(mask(4, 4))).unwrap().unwrap();
        }
        // All in use: no room, and no growth past the largest side.
        assert!(matches!(
            atlas.glyph(key(9), || Some(mask(4, 4))),
            Err(PrepareError::AtlasFull)
        ));
        atlas.trim();
        // Key 0 is used again, so key 1 is the least recent.
        atlas.glyph(key(0), || unreachable!()).unwrap().unwrap();
        atlas.glyph(key(9), || Some(mask(4, 4))).unwrap().unwrap();
        assert_eq!(atlas.cached(), (4, 0));
        let mut again = false;
        atlas
            .glyph(key(1), || {
                again = true;
                Some(mask(4, 4))
            })
            .unwrap()
            .unwrap();
        assert!(again, "key 1 was evicted and is rasterized again");
        // That evicted key 2, the next least recent, and kept 0 and 9.
        for id in [0, 9] {
            atlas.glyph(key(id), || unreachable!()).unwrap().unwrap();
        }
    }

    #[test]
    fn an_atlas_whose_glyphs_are_all_in_use_grows() {
        let mut atlas = CpuTextAtlas::with_sides(4, 16);
        for id in 0..5 {
            atlas.glyph(key(id), || Some(mask(4, 4))).unwrap().unwrap();
        }
        assert_eq!(atlas.mask.side, 16);
        assert_eq!(atlas.cached(), (5, 0));
        // A raster wider than the largest side never fits.
        assert!(matches!(
            atlas.glyph(key(7), || Some(mask(17, 1))),
            Err(PrepareError::AtlasFull)
        ));
    }

    #[test]
    fn glyphs_are_placed_by_their_bearings_and_clipped_as_glyphon_clips_them() {
        let color = Color::rgb(1, 2, 3);
        // `x + left`, `round(line_y * scale) + y - top`.
        let g = place_glyph(
            Some(details(4, 6, 2, 5)),
            [10, 3],
            7.4,
            2.0,
            color,
            [0, 0, 100, 100],
        )
        .unwrap();
        assert_eq!((g.pos, g.dim, g.uv), ([12, 13], [4, 6], [0, 0]));
        // Clipped on every side: the raster is sampled from the clip.
        let g = place_glyph(
            Some(details(10, 10, 0, 0)),
            [5, 5],
            0.0,
            1.0,
            color,
            [7, 8, 12, 11],
        )
        .unwrap();
        assert_eq!((g.pos, g.dim, g.uv), ([7, 8], [5, 3], [2, 3]));
        // Wholly outside: dropped. Touching an edge: kept, zero-sized.
        let outside = |x, y| {
            place_glyph(
                Some(details(4, 4, 0, 0)),
                [x, y],
                0.0,
                1.0,
                color,
                [10, 10, 20, 20],
            )
        };
        assert!(outside(21, 12).is_none());
        assert!(outside(5, 12).is_none());
        assert!(outside(12, 21).is_none());
        assert!(outside(12, 5).is_none());
        assert_eq!(outside(20, 12).unwrap().dim, [0, 4]);
        assert_eq!(outside(6, 12).unwrap().dim, [0, 4]);
    }

    #[test]
    fn mask_text_blends_straight_and_colour_text_ignores_its_colour() {
        let mut fb = CpuFramebuffer::new(3, 1).unwrap();
        // Opaque black.
        fb.clear([0.0, 0.0, 0.0, 1.0]);
        let mask = Arc::new(CpuGlyphRaster {
            color: false,
            width: 2,
            height: 1,
            data: vec![255, 128].into(),
        });
        let emoji = Arc::new(CpuGlyphRaster {
            color: true,
            width: 1,
            height: 1,
            data: vec![255, 0, 0, 255].into(),
        });
        let glyphs = [
            CpuTextGlyph {
                pos: [0, 0],
                dim: [2, 1],
                uv: [0, 0],
                color: Color::rgba(255, 255, 255, 255),
                raster: mask,
            },
            CpuTextGlyph {
                pos: [2, 0],
                dim: [1, 1],
                uv: [0, 0],
                color: Color::rgba(0, 0, 255, 10),
                raster: emoji,
            },
        ];
        draw_cpu_text(
            &mut fb.target(),
            [3.0, 1.0],
            PixelRect::new(0, 0, 3, 1),
            &glyphs,
            &CpuTextAtlas::default(),
        );
        let px: Vec<_> = fb.pixels().iter().map(|&p| unpack(p)).collect();
        // 128/255 white over black is 0.502 linear: 188 in sRGB.
        assert_eq!(
            px,
            [[255; 4], [188, 188, 188, 255], [255, 0, 0, 255]],
            "{px:?}"
        );
        // A scissor clips the draw.
        fb.clear([0.0, 0.0, 0.0, 1.0]);
        draw_cpu_text(
            &mut fb.target(),
            [3.0, 1.0],
            PixelRect::new(1, 0, 1, 1),
            &glyphs,
            &CpuTextAtlas::default(),
        );
        let px: Vec<_> = fb.pixels().iter().map(|&p| unpack(p)).collect();
        assert_eq!(px, [[0, 0, 0, 255], [188, 188, 188, 255], [0, 0, 0, 255]]);
    }
}
