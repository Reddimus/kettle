//! The CPU backend: its draw list, and the two seams the software
//! rasterizer and the shared-memory presenter plug into.
//!
//! A CPU-backend renderer builds each frame exactly as a GPU one does. Its
//! pipelines keep CPU copies of their instances and textures, and the scene
//! pass ([`Renderer::encode_scene_pass`](crate::Renderer)) replays each draw
//! here as one [`CpuDraw`], in the order and under the scissor a wgpu pass
//! would execute it. [`CpuPass`] records every draw and hands it to the
//! renderer's [`CpuRaster`], by default [`SoftRaster`](super::cpu_draw::SoftRaster),
//! which draws it into a [`CpuTarget`]: the window's shared-memory buffer,
//! or for an offscreen renderer its own [`CpuFramebuffer`].
//!
//! Pixels are `cpu_raster`'s packed `0xAARRGGBB`: sRGB-encoded colour,
//! premultiplied by alpha in linear light. That is what an `Rgba8UnormSrgb`
//! scene target holds after the same draws, so a GPU readback and a CPU frame
//! compare directly, and it is the byte order of `wl_shm`'s ARGB8888.

use std::ops::Range;
use std::time::Instant;

use kettle_core::ImageData;

use crate::cpu_raster::{CpuFramebuffer, CpuTarget, CpuTextureView, PixelRect};
use crate::glyphpipe::GlyphInstance;
use crate::imgpipe::ImageInstance;
use crate::outline::OutlineInstance;
use crate::quad::QuadInstance;
use crate::rounded_surface::RoundedSurface;
use crate::starfield::StarfieldUniforms;

use super::text::{CpuTextAtlas, CpuTextGlyph};

/// The pipelines a scene pass draws with. The cursor patch is not among
/// them: it re-encodes the scene for a macOS Core Animation layer, which a
/// CPU renderer never has (see `cursor_patch.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CpuPipeline {
    /// `quad.rs`, premultiplied source-over.
    QuadBlend,
    /// `quad.rs`, replacing what it covers, alpha included.
    QuadReplace,
    /// `glyphpipe.rs`, the cell-locked grid glyphs.
    Glyph,
    /// `imgpipe.rs`, one image texture per draw.
    Image,
    /// `rounded_surface.rs`.
    RoundedSurface,
    /// `outline.rs`.
    Outline,
    /// `starfield.rs`.
    Starfield,
    /// glyphon's `TextRenderer` (chrome and overlay text).
    Text,
}

/// One draw, with everything the matching WGSL shader would read.
pub(crate) enum CpuDraw<'a> {
    /// `quad.wgsl`: flat fills in straight sRGB colour.
    Quads {
        replace: bool,
        instances: &'a [QuadInstance],
    },
    /// `glyphpipe.rs`'s shader over its two atlases.
    Glyphs {
        instances: &'a [GlyphInstance],
        color_atlas: CpuTextureView<'a>,
        mask_atlas: CpuTextureView<'a>,
    },
    /// `imgpipe.rs`'s four-tap shader over one image, clamped or repeating.
    Images {
        instances: &'a [ImageInstance],
        image: &'a ImageData,
        repeat: bool,
    },
    RoundedSurfaces {
        instances: &'a [RoundedSurface],
    },
    Outlines {
        instances: &'a [OutlineInstance],
    },
    /// The whole-target starfield, from this frame's uniforms.
    Starfield {
        uniforms: &'a StarfieldUniforms,
    },
    /// One glyphon renderer's prepared text, laid out as glyphon lays it out
    /// (see `text.rs`).
    Text {
        glyphs: &'a [CpuTextGlyph],
        #[allow(dead_code, reason = "glyph rasters carry their own pixels")]
        atlas: &'a CpuTextAtlas,
    },
}

impl CpuDraw<'_> {
    pub(crate) fn pipeline(&self) -> CpuPipeline {
        match self {
            Self::Quads { replace: true, .. } => CpuPipeline::QuadReplace,
            Self::Quads { replace: false, .. } => CpuPipeline::QuadBlend,
            Self::Glyphs { .. } => CpuPipeline::Glyph,
            Self::Images { .. } => CpuPipeline::Image,
            Self::RoundedSurfaces { .. } => CpuPipeline::RoundedSurface,
            Self::Outlines { .. } => CpuPipeline::Outline,
            Self::Starfield { .. } => CpuPipeline::Starfield,
            Self::Text { .. } => CpuPipeline::Text,
        }
    }

    /// How many instances (or glyphs) the draw covers; 1 for the starfield.
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Quads { instances, .. } => instances.len(),
            Self::Glyphs { instances, .. } => instances.len(),
            Self::Images { instances, .. } => instances.len(),
            Self::RoundedSurfaces { instances } => instances.len(),
            Self::Outlines { instances } => instances.len(),
            Self::Starfield { .. } => 1,
            Self::Text { glyphs, .. } => glyphs.len(),
        }
    }
}

/// What a scene pass drew, as [`CpuPass`] records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CpuDrawRecord {
    pub(crate) pipeline: CpuPipeline,
    pub(crate) instances: usize,
    /// `[x, y, width, height]` in target pixels.
    pub(crate) scissor: [u32; 4],
}

/// The software rasterizer: executes each draw as its WGSL shader would.
///
/// `screen` is the scene's size, the screen uniform every GPU pipeline is
/// given; the target's viewport maps it 1:1 from the target's origin.
/// `scissor` is already inside the target, and never empty.
pub(crate) trait CpuRaster: Send {
    fn draw(
        &mut self,
        target: &mut CpuTarget<'_>,
        screen: [f32; 2],
        scissor: PixelRect,
        draw: &CpuDraw<'_>,
    );

    /// The presentation pass (`present.rs`): `scene` converted to straight
    /// alpha into `output`, for a translucent `PostMultiplied` surface.
    /// `output` already has `scene`'s size.
    fn present(&mut self, scene: &CpuFramebuffer, output: &mut CpuFramebuffer);

    /// A new frame begins: per-frame caches may age out what the last frame
    /// did not use.
    fn begin_frame(&mut self) {}
}

/// A rasterizer that draws nothing and presents the scene as is, for tests
/// that look only at the draw list.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NoRaster;

#[cfg(test)]
impl CpuRaster for NoRaster {
    fn draw(&mut self, _: &mut CpuTarget<'_>, _: [f32; 2], _: PixelRect, _: &CpuDraw<'_>) {}

    fn present(&mut self, scene: &CpuFramebuffer, output: &mut CpuFramebuffer) {
        output.pixels_mut().copy_from_slice(scene.pixels());
    }
}

/// A scene pass on the CPU: the stand-in for a `wgpu::RenderPass`.
pub(crate) struct CpuPass<'e> {
    target: CpuTarget<'e>,
    size: [u32; 2],
    raster: &'e mut dyn CpuRaster,
    records: &'e mut Vec<CpuDrawRecord>,
    scissor: [u32; 4],
}

impl<'e> CpuPass<'e> {
    /// A pass over `target`, a whole `size` frame the caller has already
    /// cleared, as the GPU pass's `LoadOp::Clear` does.
    pub(crate) fn new(
        target: CpuTarget<'e>,
        size: [u32; 2],
        raster: &'e mut dyn CpuRaster,
        records: &'e mut Vec<CpuDrawRecord>,
    ) -> Self {
        Self {
            target,
            size,
            raster,
            records,
            scissor: [0, 0, size[0], size[1]],
        }
    }

    pub(crate) fn set_scissor_rect(&mut self, x: u32, y: u32, width: u32, height: u32) {
        let [target_w, target_h] = self.size;
        let x = x.min(target_w);
        let y = y.min(target_h);
        self.scissor = [x, y, width.min(target_w - x), height.min(target_h - y)];
    }

    /// A windowed pass, which only the macOS cursor patch encodes. A CPU
    /// renderer never offers that hand-off, so this is never reached.
    pub(crate) fn set_viewport(&mut self, _x: f32, _y: f32, _width: f32, _height: f32) {
        debug_assert!(false, "the CPU backend encodes no windowed scene pass");
    }

    /// Record `draw` and rasterize it under the current scissor.
    pub(crate) fn draw(&mut self, draw: CpuDraw<'_>) {
        if draw.len() == 0 {
            return;
        }
        self.records.push(CpuDrawRecord {
            pipeline: draw.pipeline(),
            instances: draw.len(),
            scissor: self.scissor,
        });
        let [x, y, w, h] = self.scissor;
        if w > 0 && h > 0 {
            let screen = [self.size[0] as f32, self.size[1] as f32];
            self.raster
                .draw(&mut self.target, screen, PixelRect::new(x, y, w, h), &draw);
        }
    }
}

/// The instances of `all` that `range` selects, clamped as a wgpu draw's
/// instance range is validated: past the end draws nothing.
pub(crate) fn instance_slice<T>(all: &[T], range: Range<u32>) -> &[T] {
    let end = (range.end as usize).min(all.len());
    let start = (range.start as usize).min(end);
    &all[start..end]
}

/// What a presenter reports for one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CpuPresentOutcome {
    /// The frame is on screen, or drawn and held for the compositor's next
    /// frame callback (see [`CpuSurface::flush`]).
    Presented,
    /// The window cannot show it now; keep damage and try later.
    Occluded,
    /// The compositor holds every buffer; draw again later.
    RetryLater,
    /// The window's buffers are gone; rebuild the presenter.
    Lost,
    /// The draw callback refused the frame, and nothing was presented.
    NotDrawn,
}

/// A window's shared-memory buffers, as the CPU backend draws into them:
/// `cpu_present::CpuPresenter` on Linux, a fake in tests.
pub(crate) trait CpuSurface: Send {
    /// The window is now `width` x `height` physical pixels at `scale`.
    fn resize(&mut self, width: u32, height: u32, scale: f64);

    /// Whether the window can show premultiplied alpha.
    fn supports_translucency(&self) -> bool;

    /// Ask for premultiplied (`true`) or opaque buffers. Returns whether
    /// frames are premultiplied now.
    fn set_translucent(&mut self, translucent: bool) -> bool;

    /// Draw the next buffer with `draw` (its pixels, width and height,
    /// whose contents are undefined) and present it with `pre_commit` run
    /// just before the commit. `draw` returns `false` to drop the frame.
    fn present_frame(
        &mut self,
        draw: &mut dyn FnMut(&mut [u32], u32, u32) -> bool,
        pre_commit: &mut dyn FnMut(),
    ) -> CpuPresentOutcome;

    /// Commit a frame held for the compositor's frame callback, if it is
    /// ready for it. Returns whether something was committed.
    fn flush(&mut self, pre_commit: &mut dyn FnMut()) -> bool;

    /// A frame is drawn but not yet committed.
    fn has_deferred(&self) -> bool;

    /// Release spare buffers now; returns how many went.
    fn release_idle_buffers(&mut self) -> usize;

    /// When a spare buffer becomes idle enough to release.
    fn idle_release_deadline(&self) -> Option<Instant>;

    /// The window is hidden, or visible again.
    fn set_occluded(&mut self, occluded: bool);

    /// The presenter's name, for logs and `--gpu-info`.
    fn describe(&self) -> &'static str;
}

/// A CPU renderer's frame state.
pub(crate) struct CpuBackend {
    /// An offscreen renderer's scene target, which every frame clears and
    /// draws into. A windowed renderer draws into its surface's buffer
    /// instead, and keeps none.
    pub(crate) scene: Option<CpuFramebuffer>,
    /// The scene's straight-alpha conversion, for a translucent
    /// `PostMultiplied` offscreen surface.
    pub(crate) presented: Option<CpuFramebuffer>,
    /// Whether the last frame presented `presented` rather than `scene`.
    pub(crate) converted: bool,
    pub(crate) raster: Box<dyn CpuRaster>,
    /// `None` for an offscreen renderer.
    pub(crate) surface: Option<Box<dyn CpuSurface>>,
    /// The draws of the last scene pass.
    pub(crate) draws: Vec<CpuDrawRecord>,
}

impl CpuBackend {
    pub(crate) fn new(surface: Option<Box<dyn CpuSurface>>) -> Self {
        Self {
            scene: None,
            presented: None,
            converted: false,
            raster: Box::new(super::cpu_draw::SoftRaster::new()),
            surface,
            draws: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass_over<'e>(
        frame: &'e mut CpuFramebuffer,
        raster: &'e mut dyn CpuRaster,
        records: &'e mut Vec<CpuDrawRecord>,
    ) -> CpuPass<'e> {
        let size = [frame.width(), frame.height()];
        CpuPass::new(frame.target(), size, raster, records)
    }

    #[test]
    fn scissors_are_clamped_to_the_target() {
        let mut frame = CpuFramebuffer::new(10, 8).expect("small frame");
        let mut raster = NoRaster;
        let mut records = Vec::new();
        let mut pass = pass_over(&mut frame, &mut raster, &mut records);
        pass.set_scissor_rect(6, 2, 100, 100);
        assert_eq!(pass.scissor, [6, 2, 4, 6]);
        pass.set_scissor_rect(20, 20, 5, 5);
        assert_eq!(pass.scissor, [10, 8, 0, 0]);
    }

    #[test]
    fn a_pass_records_each_non_empty_draw_under_its_scissor() {
        let mut frame = CpuFramebuffer::new(10, 8).expect("small frame");
        let mut raster = NoRaster;
        let mut records = Vec::new();
        let quad = QuadInstance {
            pos: [0.0, 0.0],
            size: [1.0, 1.0],
            color: [1.0; 4],
        };
        {
            let mut pass = pass_over(&mut frame, &mut raster, &mut records);
            pass.draw(CpuDraw::Quads {
                replace: true,
                instances: &[quad, quad],
            });
            pass.draw(CpuDraw::Quads {
                replace: false,
                instances: &[],
            });
            pass.set_scissor_rect(1, 2, 3, 4);
            pass.draw(CpuDraw::Quads {
                replace: false,
                instances: &[quad],
            });
        }
        assert_eq!(
            records,
            [
                CpuDrawRecord {
                    pipeline: CpuPipeline::QuadReplace,
                    instances: 2,
                    scissor: [0, 0, 10, 8],
                },
                CpuDrawRecord {
                    pipeline: CpuPipeline::QuadBlend,
                    instances: 1,
                    scissor: [1, 2, 3, 4],
                },
            ]
        );
    }

    #[test]
    fn instance_ranges_clamp_like_a_validated_draw() {
        let all = [1, 2, 3, 4];
        assert_eq!(instance_slice(&all, 1..3), &[2, 3]);
        assert_eq!(instance_slice(&all, 2..u32::MAX), &[3, 4]);
        assert!(instance_slice(&all, 9..12).is_empty());
        let (start, end) = (3, 1);
        assert!(instance_slice(&all, start..end).is_empty());
    }
}
