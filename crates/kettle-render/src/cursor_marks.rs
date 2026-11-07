//! The block cursor's inverted copy of a cell whose marks fallback placement
//! moved (`batching::moved_cell_images`), drawn as glyphon custom glyphs.
//!
//! It has an atlas of its own. glyphon caches a custom raster by id and size
//! and, when an atlas grows, asks for every cached custom raster again and
//! panics on `None`; a shared atlas grows during prepares that cannot answer.
//! This atlas grows only in [`CursorMarks::prepare`], which keeps every mask
//! it has handed out. When its ids or masks reach [`MASK_LIMIT`] the caller
//! builds a new one, so neither an id nor the store is ever reused or grows
//! without bound.

use std::collections::HashMap;

use glyphon::cosmic_text::CacheKey;
use glyphon::{
    Buffer as TextBuffer, Cache, Color as GColor, ContentType, CustomGlyph, FontSystem, Metrics,
    RasterizedCustomGlyph, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};

use crate::batching::CellImage;

/// Rasters one atlas holds before a new one replaces it.
pub(crate) const MASK_LIMIT: usize = 4096;

pub(crate) struct CursorMarks {
    atlas: TextAtlas,
    renderer: TextRenderer,
    /// The text area's buffer, always empty: only custom glyphs are drawn.
    buffer: TextBuffer,
    ids: HashMap<CacheKey, u16>,
    /// Every raster this atlas may hold, by id and size.
    masks: HashMap<(u16, u16, u16), Vec<u8>>,
    /// Whether the last prepare drew anything.
    prepared: bool,
}

impl CursorMarks {
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cache: &Cache,
        format: wgpu::TextureFormat,
        fonts: &mut FontSystem,
    ) -> Self {
        let mut atlas = TextAtlas::new(device, queue, cache, format);
        let renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        Self {
            atlas,
            renderer,
            buffer: TextBuffer::new(fonts, Metrics::new(1.0, 1.0)),
            ids: HashMap::new(),
            masks: HashMap::new(),
            prepared: false,
        }
    }

    /// Whether `images` fit beside what this atlas already holds.
    pub(crate) fn has_room(&self, images: &[CellImage]) -> bool {
        let new = images
            .iter()
            .filter(|image| !self.ids.contains_key(&image.key))
            .count();
        self.masks.len() + new <= MASK_LIMIT
    }

    /// Prepares `images` (surface pixels) in `color`, clipped to `bounds`.
    /// The caller checks [`Self::has_room`] first.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        fonts: &mut FontSystem,
        swash: &mut SwashCache,
        viewport: &Viewport,
        images: &[CellImage],
        color: GColor,
        bounds: TextBounds,
    ) -> Result<(), glyphon::PrepareError> {
        let mut custom = Vec::with_capacity(images.len());
        for image in images {
            let [x, y, width, height] = image.rect;
            let (Ok(w), Ok(h)) = (u16::try_from(width), u16::try_from(height)) else {
                continue;
            };
            let next = self.ids.len();
            let Some(&mut id) = (match self.ids.entry(image.key) {
                std::collections::hash_map::Entry::Occupied(id) => Some(id.into_mut()),
                std::collections::hash_map::Entry::Vacant(slot) => {
                    u16::try_from(next).ok().map(|id| slot.insert(id))
                }
            }) else {
                continue;
            };
            self.masks
                .entry((id, w, h))
                .or_insert_with(|| image.mask.clone());
            custom.push(CustomGlyph {
                id,
                left: x as f32,
                top: y as f32,
                width: f32::from(w),
                height: f32::from(h),
                color: None,
                snap_to_physical_pixel: true,
                metadata: 0,
            });
        }
        self.prepared = false;
        // The last cursor's glyphs are no longer drawn; they may be evicted.
        self.atlas.trim();
        let area = TextArea {
            buffer: &self.buffer,
            left: 0.0,
            top: 0.0,
            scale: 1.0,
            bounds,
            default_color: color,
            custom_glyphs: &custom,
        };
        let masks = &self.masks;
        self.renderer.prepare_with_custom(
            device,
            queue,
            fonts,
            &mut self.atlas,
            viewport,
            [area],
            swash,
            |request| {
                Some(RasterizedCustomGlyph {
                    data: masks
                        .get(&(request.id, request.width, request.height))?
                        .clone(),
                    content_type: ContentType::Mask,
                })
            },
        )?;
        self.prepared = !custom.is_empty();
        Ok(())
    }

    /// Stops drawing until the next [`Self::prepare`].
    pub(crate) fn hide(&mut self) {
        self.prepared = false;
    }

    pub(crate) fn render(
        &self,
        viewport: &Viewport,
        pass: &mut wgpu::RenderPass<'_>,
    ) -> Result<(), glyphon::RenderError> {
        if self.prepared {
            self.renderer.render(&self.atlas, viewport, pass)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu_tests::gpu_test_guard;
    use crate::headless_tests::renderer;

    /// A distinct `side`-pixel square image.
    fn image(glyph_id: u16, side: i32) -> CellImage {
        let (key, _, _) = CacheKey::new(
            glyphon::fontdb::ID::dummy(),
            glyph_id,
            13.0,
            (0.0, 0.0),
            glyphon::fontdb::Weight::NORMAL,
            glyphon::cosmic_text::CacheKeyFlags::empty(),
        );
        CellImage {
            key,
            rect: [0, 0, side, side],
            mask: vec![0x80; (side * side) as usize],
        }
    }

    /// Growing asks for every cached image again, those of earlier cells
    /// included; the atlas keeps them all, so the growth finds each one.
    #[test]
    fn the_atlas_grows_with_every_image_it_drew() {
        let _serial = gpu_test_guard();
        let Some((mut renderer, _)) = renderer(64, 64) else {
            eprintln!("MARK_GPU_SKIP: no GPU adapter on this host");
            return;
        };
        let mut marks = CursorMarks::new(
            &renderer.gpu.device,
            &renderer.gpu.queue,
            &renderer.glyphon_cache,
            renderer.config.format,
            &mut renderer.font_system,
        );
        let bounds = TextBounds {
            left: 0,
            top: 0,
            right: 64,
            bottom: 64,
        };
        let earlier: Vec<_> = (0..4).map(|id| image(id, 48)).collect();
        // Thirty 48-pixel squares need more than a 256-pixel atlas.
        let crowd: Vec<_> = (0..30).map(|id| image(100 + id, 48)).collect();
        for cell in [&earlier, &crowd, &earlier] {
            assert!(marks.has_room(cell));
            marks
                .prepare(
                    &renderer.gpu.device,
                    &renderer.gpu.queue,
                    &mut renderer.font_system,
                    &mut renderer.swash,
                    &renderer.viewport,
                    cell,
                    GColor::rgb(0, 0, 0),
                    bounds,
                )
                .expect("the cursor's images prepare");
            assert!(marks.prepared);
        }
        assert_eq!(marks.masks.len(), 34, "each image is kept once");
        marks.hide();
        assert!(!marks.prepared);
    }

    #[test]
    fn a_full_atlas_has_no_room_for_new_images() {
        let _serial = gpu_test_guard();
        let Some((mut renderer, _)) = renderer(64, 64) else {
            eprintln!("MARK_GPU_SKIP: no GPU adapter on this host");
            return;
        };
        let mut marks = CursorMarks::new(
            &renderer.gpu.device,
            &renderer.gpu.queue,
            &renderer.glyphon_cache,
            renderer.config.format,
            &mut renderer.font_system,
        );
        for id in 0..MASK_LIMIT as u16 - 1 {
            let image = image(id, 2);
            marks.ids.insert(image.key, id);
            marks.masks.insert((id, 2, 2), image.mask);
        }
        let held = [image(0, 2)];
        let new = [image(9000, 2), image(9001, 2)];
        assert!(marks.has_room(&held), "held images need no room");
        assert!(marks.has_room(&new[..1]));
        assert!(!marks.has_room(&new), "past the limit a new atlas is due");
    }
}
