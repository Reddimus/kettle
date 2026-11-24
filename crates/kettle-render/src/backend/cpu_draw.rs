//! The CPU backend's default rasterizer: each recorded [`CpuDraw`] goes to
//! the `cpu_raster` function that reproduces its wgpu pipeline, with that
//! pipeline's blend state, textures and scissor.
//!
//! Two things are added on top of a one-to-one replay:
//!
//! - **Bands.** A draw that covers many pixels runs on horizontal bands of
//!   the target at once ([`CpuTarget::draw_banded`]). Banding is
//!   bit-identical to one thread, so it only changes the time a frame takes.
//! - **A still-image cache.** An image draw that covers much of the target
//!   and repeats unchanged from one frame to the next (the wallpaper) is
//!   drawn once into a cache at its final size and then copied. Its four-tap
//!   filter costs 20–25 ns a pixel, 30–40 ms for a full-screen stretch.
//!   Whether a draw comes from the cache is settled in
//!   [`CpuRaster::plan`], once a frame, and reported there, because a
//!   translucent cached image can differ from a direct draw by one code.
//! - **Clipping.** A damage-tracked frame draws each draw once per repaint
//!   rectangle ([`CpuRaster::draw_clipped`]). The pixels inside are exactly
//!   the unclipped draw's, and banding follows the clipped area.

use std::sync::Arc;

use kettle_core::PixelBuffer;

use super::cpu::{CpuDraw, CpuRaster};
use crate::cpu_raster::glyphpipe::{GlyphAtlases, draw_glyphs};
use crate::cpu_raster::imgpipe::draw_image;
use crate::cpu_raster::outline::draw_outlines;
use crate::cpu_raster::present::present_straight;
use crate::cpu_raster::quad::{QuadBlend, draw_quads};
use crate::cpu_raster::rounded_surface::draw_rounded_surfaces;
use crate::cpu_raster::srgb::tables;
use crate::cpu_raster::starfield::draw_starfield;
use crate::cpu_raster::{
    AddressMode, CpuFramebuffer, CpuTarget, CpuTextureFormat, CpuTextureView, PixelRect, Premul,
    Viewport, blend_premul, rasterize, unpack,
};
use crate::imgpipe::ImageInstance;

/// Most bands a draw is split into.
const MAX_BANDS: u32 = 8;
/// Target pixels per band: a 1600x1000 frame gets four, a 4K one eight.
const PIXELS_PER_BAND: u64 = 400_000;
/// A draw covering fewer pixels than this, summed over its instances, stays
/// on one thread: spawning the bands would cost more than it saves.
const BAND_MIN_PIXELS: u64 = 256_000;
/// Glyph draws are cheap per pixel, so they band by instance count.
const BAND_MIN_GLYPHS: usize = 2_048;
/// The least a draw of images must cover for its result to be cached.
const IMAGE_CACHE_MIN_PIXELS: u64 = 128 * 1024;

/// The default [`CpuRaster`].
pub(crate) struct SoftRaster {
    /// Most bands a large draw runs on, from the host's parallelism.
    max_bands: u32,
    images: ImageCache,
}

impl SoftRaster {
    pub(crate) fn new() -> Self {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        Self::with_max_bands(u32::try_from(threads).unwrap_or(MAX_BANDS))
    }

    /// A rasterizer that never runs more than `bands` threads for one draw.
    pub(crate) fn with_max_bands(bands: u32) -> Self {
        Self {
            max_bands: bands.clamp(1, MAX_BANDS),
            images: ImageCache::default(),
        }
    }

    /// How many bands a draw covering about `pixels` pixels of the area
    /// `clip` (in a target's rows) gets.
    fn bands_for(&self, clip: PixelRect, pixels: u64) -> u32 {
        let area = u64::from(clip.w) * u64::from(clip.h);
        if self.max_bands == 1 || pixels < BAND_MIN_PIXELS || area < BAND_MIN_PIXELS {
            return 1;
        }
        u32::try_from(area.div_ceil(PIXELS_PER_BAND))
            .unwrap_or(MAX_BANDS)
            .clamp(1, self.max_bands)
    }
}

/// Run `draw` on `bands` bands of the rows of `target` that `clip` covers,
/// from an even row so every band starts on one ([`CpuTarget::draw_banded`]).
fn draw_rows(
    target: &mut CpuTarget<'_>,
    clip: PixelRect,
    bands: u32,
    draw: impl Fn(&mut CpuTarget<'_>) + Sync,
) {
    if bands <= 1 {
        draw(target);
        return;
    }
    let rows = target.rows();
    let start = (clip.y & !1).clamp(rows.start, rows.end);
    let end = clip.y.saturating_add(clip.h).clamp(start, rows.end);
    let (_, rest) = target.reborrow().split_at_row(start);
    let (mut middle, _) = rest.split_at_row(end);
    middle.draw_banded(bands, draw);
}

impl Default for SoftRaster {
    fn default() -> Self {
        Self::new()
    }
}

/// The pixels the instances at `rects` (`pos`, `size`) cover inside `clip`,
/// summed: what decides whether a draw is worth banding.
fn covered_pixels(clip: PixelRect, rects: impl Iterator<Item = ([f32; 2], [f32; 2])>) -> u64 {
    let limit = u64::from(clip.w) * u64::from(clip.h);
    let mut total = 0u64;
    for (_, size) in rects {
        let area = f64::from(size[0].abs()) * f64::from(size[1].abs());
        total = total.saturating_add(if area.is_finite() {
            (area as u64).min(limit)
        } else {
            limit
        });
        if total >= BAND_MIN_PIXELS * 4 {
            break;
        }
    }
    total
}

/// The view an image draw samples: its decoded RGBA8 pixels, sRGB as the
/// GPU's image textures are.
fn image_view(image: &kettle_core::ImageData) -> Option<CpuTextureView<'_>> {
    CpuTextureView::new(
        image.width,
        image.height,
        CpuTextureFormat::Rgba8UnormSrgb,
        &image.rgba[..],
    )
}

fn address(repeat: bool) -> AddressMode {
    if repeat {
        AddressMode::Repeat
    } else {
        AddressMode::ClampToEdge
    }
}

impl CpuRaster for SoftRaster {
    fn draw(
        &mut self,
        target: &mut CpuTarget<'_>,
        screen: [f32; 2],
        scissor: PixelRect,
        draw: &CpuDraw<'_>,
    ) {
        self.draw_clipped(target, screen, scissor, scissor, draw);
    }

    fn plan(&mut self, screen: [f32; 2], scissor: PixelRect, draw: &CpuDraw<'_>) -> u64 {
        let CpuDraw::Images {
            instances,
            image,
            repeat,
        } = *draw
        else {
            return 0;
        };
        let Some(texture) = image_view(image) else {
            return 0;
        };
        let pixels = covered_pixels(scissor, instances.iter().map(|i| (i.pos, i.size)));
        let cached = pixels >= IMAGE_CACHE_MIN_PIXELS
            && self.images.plan(
                screen,
                scissor,
                instances,
                image,
                texture,
                repeat,
                self.max_bands,
            );
        u64::from(cached)
    }

    fn draw_clipped(
        &mut self,
        target: &mut CpuTarget<'_>,
        screen: [f32; 2],
        scissor: PixelRect,
        clip_rect: PixelRect,
        draw: &CpuDraw<'_>,
    ) {
        let clip = Some(clip_rect);
        match *draw {
            CpuDraw::Quads { replace, instances } => {
                let mode = if replace {
                    QuadBlend::Replace
                } else {
                    QuadBlend::Blend
                };
                // Fills are a few nanoseconds a pixel; only very large ones band.
                let pixels =
                    covered_pixels(clip_rect, instances.iter().map(|q| (q.pos, q.size))) / 4;
                let bands = self.bands_for(clip_rect, pixels);
                draw_rows(target, clip_rect, bands, |band| {
                    draw_quads(band, screen, instances, mode, clip);
                });
            }
            CpuDraw::Glyphs {
                instances,
                color_atlas,
                mask_atlas,
                ..
            } => {
                let atlases = GlyphAtlases {
                    color: color_atlas,
                    mask: mask_atlas,
                };
                let bands = if instances.len() >= BAND_MIN_GLYPHS {
                    self.bands_for(clip_rect, u64::MAX)
                } else {
                    1
                };
                draw_rows(target, clip_rect, bands, |band| {
                    draw_glyphs(band, screen, instances, atlases, clip);
                });
            }
            CpuDraw::Images {
                instances,
                image,
                repeat,
            } => {
                let Some(texture) = image_view(image) else {
                    return;
                };
                // The cache is keyed by the draw's own scissor, so it serves
                // every repaint rectangle of the draw alike.
                let whole = covered_pixels(scissor, instances.iter().map(|i| (i.pos, i.size)));
                if whole >= IMAGE_CACHE_MIN_PIXELS
                    && let Some(entry) = self
                        .images
                        .planned(screen, scissor, instances, image, repeat)
                {
                    composite(target, entry, clip_rect);
                    return;
                }
                let pixels = covered_pixels(clip_rect, instances.iter().map(|i| (i.pos, i.size)));
                let bands = self.bands_for(clip_rect, pixels.saturating_mul(4));
                draw_rows(target, clip_rect, bands, |band| {
                    for inst in instances {
                        draw_image(band, screen, inst, texture, address(repeat), clip);
                    }
                });
            }
            CpuDraw::RoundedSurfaces { instances } => {
                let pixels = covered_pixels(clip_rect, instances.iter().map(|s| (s.pos, s.size)));
                let bands = self.bands_for(clip_rect, pixels);
                draw_rows(target, clip_rect, bands, |band| {
                    draw_rounded_surfaces(band, screen, instances, clip);
                });
            }
            CpuDraw::Outlines { instances } => {
                let pixels = covered_pixels(clip_rect, instances.iter().map(|o| (o.pos, o.size)));
                let bands = self.bands_for(clip_rect, pixels);
                draw_rows(target, clip_rect, bands, |band| {
                    draw_outlines(band, screen, instances, clip);
                });
            }
            CpuDraw::Starfield { uniforms } => {
                let live = &uniforms.stars[..(uniforms.count as usize).min(uniforms.stars.len())];
                let resolution = uniforms.resolution;
                let bands = self.bands_for(clip_rect, u64::MAX);
                draw_rows(target, clip_rect, bands, |band| {
                    draw_starfield(band, resolution, live, clip, &mut Vec::new());
                });
            }
            // glyphon's text areas, prepared by the CPU text compositor in
            // `text.rs`: chrome, titles, hints and overlay text.
            CpuDraw::Text { glyphs, atlas } => {
                super::text::draw_cpu_text(target, screen, clip_rect, glyphs, atlas);
            }
        }
    }

    fn present(&mut self, scene: &CpuFramebuffer, output: &mut CpuFramebuffer) {
        let converted = present_straight(scene, output);
        debug_assert!(converted, "the presentation target has the scene's size");
    }

    fn begin_frame(&mut self) {
        self.images.begin_frame();
    }
}

/// What makes two image draws draw the same pixels.
#[derive(Clone, PartialEq, Eq)]
struct ImageKey {
    /// The decoded pixels' allocation. The cache holds a clone of the `Arc`,
    /// so the address cannot be reused while the entry lives.
    pixels: usize,
    width: u32,
    height: u32,
    repeat: bool,
    screen: [u32; 2],
    scissor: PixelRect,
    /// The instances, byte for byte.
    instances: Vec<u8>,
}

impl ImageKey {
    fn new(
        image: &kettle_core::ImageData,
        repeat: bool,
        screen: [f32; 2],
        scissor: PixelRect,
        instances: &[ImageInstance],
    ) -> Self {
        Self {
            pixels: Arc::as_ptr(&image.rgba) as usize,
            width: image.width,
            height: image.height,
            repeat,
            screen: screen.map(f32::to_bits),
            scissor,
            instances: bytemuck::cast_slice(instances).to_vec(),
        }
    }
}

struct CachedImage {
    key: ImageKey,
    /// Pins `key.pixels`.
    _source: Arc<PixelBuffer>,
    /// Where `pixels` lies in the target.
    rect: PixelRect,
    /// The draw over transparent black, `rect`'s size.
    pixels: CpuFramebuffer,
}

/// One cached large image draw, and the key of the last frame's uncached
/// one. An image is cached the second frame in a row it is drawn
/// unchanged, so a video's frames, which change every time, are never
/// cached, and an entry no frame used is dropped.
#[derive(Default)]
struct ImageCache {
    entry: Option<CachedImage>,
    used: bool,
    candidate: Option<ImageKey>,
    candidate_seen: bool,
}

impl ImageCache {
    fn begin_frame(&mut self) {
        if !std::mem::take(&mut self.used) {
            self.entry = None;
        }
        if !std::mem::take(&mut self.candidate_seen) {
            self.candidate = None;
        }
    }

    /// Settle whether this frame draws the image from the cache, caching it
    /// first when it repeats. `true` when it does.
    #[allow(clippy::too_many_arguments)]
    fn plan(
        &mut self,
        screen: [f32; 2],
        scissor: PixelRect,
        instances: &[ImageInstance],
        image: &kettle_core::ImageData,
        texture: CpuTextureView<'_>,
        repeat: bool,
        max_bands: u32,
    ) -> bool {
        let key = ImageKey::new(image, repeat, screen, scissor, instances);
        if self.entry.as_ref().is_none_or(|entry| entry.key != key) {
            if self.candidate.as_ref() != Some(&key) {
                self.candidate = Some(key);
                self.candidate_seen = true;
                return false;
            }
            self.candidate = None;
            self.entry = cache_image(key, screen, scissor, instances, image, texture, max_bands);
        }
        if self.entry.is_none() {
            return false;
        }
        self.used = true;
        true
    }

    /// The cached draw this frame's plan chose for this draw, if any.
    fn planned(
        &self,
        screen: [f32; 2],
        scissor: PixelRect,
        instances: &[ImageInstance],
        image: &kettle_core::ImageData,
        repeat: bool,
    ) -> Option<&CachedImage> {
        let entry = self.entry.as_ref().filter(|_| self.used)?;
        (entry.key == ImageKey::new(image, repeat, screen, scissor, instances)).then_some(entry)
    }
}

/// Draw `instances` once over transparent black, into a buffer the size of
/// what they cover.
fn cache_image(
    key: ImageKey,
    screen: [f32; 2],
    scissor: PixelRect,
    instances: &[ImageInstance],
    image: &kettle_core::ImageData,
    texture: CpuTextureView<'_>,
    max_bands: u32,
) -> Option<CachedImage> {
    let viewport = Viewport {
        x: 0.0,
        y: 0.0,
        w: screen[0],
        h: screen[1],
    };
    let rect = instances
        .iter()
        .filter_map(|inst| rasterize(&viewport, screen, inst.pos, inst.size, scissor))
        .map(|r| {
            PixelRect::new(
                r.cols.start,
                r.rows.start,
                r.cols.len() as u32,
                r.rows.len() as u32,
            )
        })
        .reduce(|a, b| {
            let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
            let x1 = (a.x + a.w).max(b.x + b.w);
            let y1 = (a.y + a.h).max(b.y + b.h);
            PixelRect::new(x0, y0, x1 - x0, y1 - y0)
        })?;
    let mut pixels = CpuFramebuffer::new(rect.w, rect.h)?;
    let scene = [screen[0] as u32, screen[1] as u32];
    let bands = max_bands.min(MAX_BANDS);
    pixels
        .window_target([rect.x, rect.y, rect.w, rect.h], scene)
        .draw_banded(bands, |band| {
            for inst in instances {
                draw_image(band, screen, inst, texture, address(key.repeat), None);
            }
        });
    Some(CachedImage {
        key,
        _source: image.rgba.clone(),
        rect,
        pixels,
    })
}

/// Blend `entry` over `target` with `PREMULTIPLIED_ALPHA_BLENDING`, as the
/// image pipeline blends: an opaque pixel is copied, a transparent one
/// skipped.
fn composite(target: &mut CpuTarget<'_>, entry: &CachedImage, scissor: PixelRect) {
    let Some(clip) = target
        .clip(Some(scissor))
        .and_then(|c| c.intersect(entry.rect))
    else {
        return;
    };
    let t = tables();
    let stride = entry.rect.w as usize;
    for y in clip.y..clip.y + clip.h {
        let row = (y - entry.rect.y) as usize * stride;
        let from = row + (clip.x - entry.rect.x) as usize;
        let src = &entry.pixels.pixels()[from..from + clip.w as usize];
        let dst = target.span(y, clip.x, clip.x + clip.w);
        for (d, &s) in dst.iter_mut().zip(src) {
            match s >> 24 {
                0xff => *d = s,
                0 => {}
                _ => {
                    let [r, g, b, a] = unpack(s);
                    let src = Premul {
                        r: t.decf[usize::from(r)],
                        g: t.decf[usize::from(g)],
                        b: t.decf[usize::from(b)],
                        a: t.unorm[usize::from(a)],
                    };
                    *d = blend_premul(t, *d, src);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_raster::pack;

    fn wallpaper(w: u32, h: u32) -> kettle_core::ImageData {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&[(x * 7) as u8, (y * 5) as u8, 90, 255]);
            }
        }
        kettle_core::ImageData::new(w, h, rgba).expect("small image")
    }

    fn stretched(screen: [f32; 2], image: &kettle_core::ImageData) -> ImageInstance {
        ImageInstance {
            pos: [3.5, 1.25],
            size: [screen[0] - 7.0, screen[1] - 3.0],
            uv_origin: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            uv_clamp: [
                0.5 / image.width as f32,
                0.5 / image.height as f32,
                1.0 - 0.5 / image.width as f32,
                1.0 - 0.5 / image.height as f32,
            ],
        }
    }

    /// Draw `frames` frames of the same image over a busy background, with
    /// `raster`, and return the last.
    fn frames(raster: &mut SoftRaster, frames: usize, translucent: bool) -> CpuFramebuffer {
        let (w, h) = (640, 400);
        let screen = [w as f32, h as f32];
        let image = wallpaper(64, 40);
        let inst = stretched(screen, &image);
        let image = if translucent {
            let mut rgba = image.rgba.to_vec();
            for px in rgba.as_chunks_mut::<4>().0 {
                px[3] = 120;
            }
            kettle_core::ImageData::new(64, 40, rgba).expect("small image")
        } else {
            image
        };
        let mut fb = CpuFramebuffer::new(w, h).expect("frame");
        for _ in 0..frames {
            raster.begin_frame();
            for (i, p) in fb.pixels_mut().iter_mut().enumerate() {
                *p = pack((i % 251) as u8, (i % 13) as u8 * 9, 40, 255);
            }
            let instances = [inst];
            let draw = CpuDraw::Images {
                instances: &instances,
                image: &image,
                repeat: false,
            };
            let scissor = PixelRect::new(0, 0, w, h - 20);
            raster.plan(screen, scissor, &draw);
            raster.draw(&mut fb.target(), screen, scissor, &draw);
        }
        fb
    }

    #[test]
    fn a_repeated_wallpaper_is_cached_and_draws_exactly_as_uncached() {
        let direct = frames(&mut SoftRaster::with_max_bands(1), 1, false);
        let mut raster = SoftRaster::with_max_bands(3);
        let cached = frames(&mut raster, 3, false);
        assert!(raster.images.entry.is_some(), "the third frame is cached");
        assert_eq!(direct, cached);
    }

    #[test]
    fn a_cached_translucent_image_blends_within_one_code() {
        let direct = frames(&mut SoftRaster::with_max_bands(1), 1, true);
        let mut raster = SoftRaster::with_max_bands(1);
        let cached = frames(&mut raster, 3, true);
        assert!(raster.images.entry.is_some());
        let worst = direct
            .pixels()
            .iter()
            .zip(cached.pixels())
            .flat_map(|(&a, &b)| {
                let (a, b) = (unpack(a), unpack(b));
                (0..4).map(move |c| a[c].abs_diff(b[c]))
            })
            .max();
        assert!(worst <= Some(1), "worst channel difference {worst:?}");
    }

    #[test]
    fn an_image_is_cached_only_once_it_repeats_and_dropped_once_unused() {
        let mut raster = SoftRaster::with_max_bands(1);
        frames(&mut raster, 1, false);
        assert!(
            raster.images.entry.is_none(),
            "a first sight draws directly"
        );
        frames(&mut raster, 2, false);
        assert!(raster.images.entry.is_some());
        raster.begin_frame();
        raster.begin_frame();
        assert!(
            raster.images.entry.is_none(),
            "a frame without the image frees its cache"
        );
    }

    /// A busy scene of every shape pipeline and a cached translucent image,
    /// drawn whole, and drawn again only inside a few rectangles over the
    /// whole frame with those rectangles cleared, on one band and on eight.
    #[test]
    fn clipped_draws_match_the_whole_draw_inside_their_rectangles() {
        use crate::outline::OutlineInstance;
        use crate::quad::QuadInstance;
        use crate::rounded_surface::RoundedSurface;
        let (w, h) = (640u32, 400u32);
        let screen = [w as f32, h as f32];
        let mut image = wallpaper(64, 40).rgba.to_vec();
        for px in image.as_chunks_mut::<4>().0 {
            px[3] = 150;
        }
        let image = kettle_core::ImageData::new(64, 40, image).expect("small image");
        let inst = stretched(screen, &image);
        let quads: Vec<QuadInstance> = (0..40)
            .map(|i| QuadInstance {
                pos: [(i * 37 % 600) as f32 + 0.3, (i * 23 % 380) as f32 + 0.6],
                size: [41.5, 17.25],
                color: [0.9, (i % 5) as f32 / 5.0, 0.3, 0.55],
            })
            .collect();
        let surfaces = [RoundedSurface {
            pos: [100.5, 60.25],
            size: [300.0, 200.0],
            fill: [0.2, 0.3, 0.9, 0.7],
            stroke: [1.0, 1.0, 1.0, 0.8],
            params: [14.0, 1.5, 0.0, 0.0],
        }];
        let outlines = [OutlineInstance {
            pos: [50.5, 40.5],
            size: [500.0, 300.0],
            color: [1.0, 0.8, 0.0, 0.9],
            border_width: 2.0,
            corner_radius: 9.0,
            corner_mask: 0b1111,
            dash: 6.0,
            gap: 4.0,
        }];
        let instances = [inst];
        let draws = [
            CpuDraw::Quads {
                replace: true,
                instances: &quads[..4],
            },
            CpuDraw::Images {
                instances: &instances,
                image: &image,
                repeat: false,
            },
            CpuDraw::Quads {
                replace: false,
                instances: &quads,
            },
            CpuDraw::RoundedSurfaces {
                instances: &surfaces,
            },
            CpuDraw::Outlines {
                instances: &outlines,
            },
        ];
        let scissor = PixelRect::new(0, 0, w, h - 10);
        let frame = |raster: &mut SoftRaster,
                     fb: &mut CpuFramebuffer,
                     clips: Option<&[PixelRect]>| {
            raster.begin_frame();
            for draw in &draws {
                raster.plan(screen, scissor, draw);
            }
            for draw in &draws {
                match clips {
                    None => raster.draw(&mut fb.target(), screen, scissor, draw),
                    Some(clips) => {
                        for clip in clips {
                            if let Some(clip) = scissor.intersect(*clip) {
                                raster.draw_clipped(&mut fb.target(), screen, scissor, clip, draw);
                            }
                        }
                    }
                }
            }
        };
        let clips = [
            PixelRect::new(0, 0, 64, 64),
            PixelRect::new(96, 33, 333, 101),
            PixelRect::new(97, 300, 500, 100),
            PixelRect::new(600, 10, 40, 290),
        ];
        for bands in [1, 8] {
            let mut whole = SoftRaster::with_max_bands(bands);
            let mut clipped = SoftRaster::with_max_bands(bands);
            let mut reference = CpuFramebuffer::new(w, h).expect("frame");
            let mut fb = CpuFramebuffer::new(w, h).expect("frame");
            // The second and third frames draw the image from the cache.
            for _ in 0..3 {
                reference.pixels_mut().fill(0);
                frame(&mut whole, &mut reference, None);
                fb.pixels_mut().copy_from_slice(reference.pixels());
                for c in &clips {
                    for y in c.y..c.y + c.h {
                        for x in c.x..c.x + c.w {
                            fb.pixels_mut()[(y * w + x) as usize] = 0;
                        }
                    }
                }
                frame(&mut clipped, &mut fb, Some(&clips));
                assert_eq!(fb, reference, "{bands} bands");
            }
            assert!(clipped.images.entry.is_some(), "the image was cached");
        }
    }

    #[test]
    fn banded_and_single_threaded_draws_match() {
        let one = frames(&mut SoftRaster::with_max_bands(1), 1, false);
        let many = frames(&mut SoftRaster::with_max_bands(8), 1, false);
        assert_eq!(one, many);
    }

    #[test]
    fn bands_follow_the_frame_size_and_the_host() {
        let raster = SoftRaster::with_max_bands(8);
        let small = PixelRect::new(0, 0, 800, 500);
        let panel = PixelRect::new(0, 0, 1600, 1000);
        let uhd = PixelRect::new(0, 0, 3840, 2160);
        assert_eq!(raster.bands_for(small, 1_000), 1);
        assert_eq!(raster.bands_for(small, u64::MAX), 1);
        assert_eq!(raster.bands_for(panel, u64::MAX), 4);
        assert_eq!(raster.bands_for(uhd, u64::MAX), 8);
        let two = SoftRaster::with_max_bands(2);
        assert_eq!(two.bands_for(uhd, u64::MAX), 2);
        // A small repaint rectangle of a large frame stays on one thread.
        assert_eq!(
            raster.bands_for(PixelRect::new(64, 64, 96, 64), u64::MAX),
            1
        );
    }

    /// The animated starfield's CPU cost per frame, single-threaded and on
    /// the bands a frame of that size gets. Run by hand:
    /// `CARGO_PROFILE_DEV_OPT_LEVEL=3 cargo test -p kettle-render --lib
    /// starfield_frame_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing; run by hand with optimizations"]
    fn starfield_frame_cost() {
        for (w, h) in [(1600, 933), (1600, 1000), (3840, 2160)] {
            let mut field = crate::starfield::CpuStarfield::new();
            let mut fb = CpuFramebuffer::new(w, h).expect("frame");
            for bands in [1, 8] {
                let mut raster = SoftRaster::with_max_bands(bands);
                let mut times = Vec::new();
                for frame in 0..40 {
                    // 10 fps: each frame is 0.1 s later.
                    field.upload([w as f32, h as f32], 3.0 + frame as f32 * 0.1);
                    let started = std::time::Instant::now();
                    raster.draw(
                        &mut fb.target(),
                        [w as f32, h as f32],
                        PixelRect::new(0, 0, w, h),
                        &CpuDraw::Starfield {
                            uniforms: field.uniforms(),
                        },
                    );
                    times.push(started.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(f64::total_cmp);
                let used = raster.bands_for(PixelRect::new(0, 0, w, h), u64::MAX);
                println!(
                    "starfield {w}x{h} max_bands={bands} (used {used}): median {:.2} ms, p90 {:.2} ms, \
                     {:.1}% of one core at 10 fps",
                    times[times.len() / 2],
                    times[times.len() * 9 / 10],
                    // ms per frame x 10 frames per second / 1000 ms x 100 %.
                    times[times.len() / 2],
                );
            }
        }
    }
}
