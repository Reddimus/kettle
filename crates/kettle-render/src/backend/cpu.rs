//! The CPU backend: its frame, its draw list, and the two seams the software
//! rasterizer and the shared-memory presenter plug into.
//!
//! A CPU-backend renderer builds each frame exactly as a GPU one does. Its
//! pipelines keep CPU copies of their instances and textures, and the scene
//! pass ([`Renderer::encode_scene_pass`](crate::Renderer)) replays each draw
//! here as one [`CpuDraw`], in the order and under the scissor a wgpu pass
//! would execute it. [`CpuPass`] records every draw and hands it to the
//! renderer's [`CpuRaster`]. Until the software rasterizer lands, that is
//! [`NoRaster`], which draws nothing, so a CPU frame is its clear.

use std::ops::Range;

use kettle_core::ImageData;

use crate::glyphpipe::GlyphInstance;
use crate::imgpipe::ImageInstance;
use crate::outline::OutlineInstance;
use crate::quad::QuadInstance;
use crate::rounded_surface::RoundedSurface;
use crate::starfield::StarfieldUniforms;

use super::text::{CpuTextAtlas, CpuTextGlyph};

/// A CPU render target.
///
/// RGBA8, top-left origin, rows packed. Each pixel holds what an
/// `Rgba8UnormSrgb` scene target holds after the same draws: sRGB-encoded
/// colour, premultiplied by alpha in linear light. A frame read back from the
/// GPU and one drawn here therefore compare byte for byte.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CpuFrame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl CpuFrame {
    /// Bytes per pixel.
    pub(crate) const BPP: usize = 4;

    /// A transparent `width` x `height` frame, or `None` when its size
    /// overflows.
    #[cfg(test)]
    pub(crate) fn new(width: u32, height: u32) -> Option<Self> {
        let mut frame = Self::default();
        frame.resize(width, height).then_some(frame)
    }

    /// Make the frame `width` x `height` pixels. Its pixels are unspecified
    /// until the next [`Self::clear`]. Returns false, leaving the frame
    /// empty, when the size overflows.
    pub(crate) fn resize(&mut self, width: u32, height: u32) -> bool {
        let Some(bytes) = frame_bytes(width, height) else {
            *self = Self::default();
            return false;
        };
        self.width = width;
        self.height = height;
        self.pixels.resize(bytes, 0);
        true
    }

    /// Fill every pixel with `rgba`, as a render pass's clear load does.
    pub(crate) fn clear(&mut self, rgba: [u8; 4]) {
        for pixel in self.pixels.as_chunks_mut::<{ Self::BPP }>().0 {
            *pixel = rgba;
        }
    }

    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    pub(crate) fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    #[expect(dead_code, reason = "the software rasterizer writes here")]
    pub(crate) fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// Copy `other` into this frame, resizing it to match.
    pub(crate) fn copy_from(&mut self, other: &Self) {
        self.width = other.width;
        self.height = other.height;
        self.pixels.clear();
        self.pixels.extend_from_slice(&other.pixels);
    }
}

fn frame_bytes(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(CpuFrame::BPP)
}

/// A CPU copy of a texture a pipeline was given, as the draw that samples
/// it sees it.
#[expect(dead_code, reason = "the software rasterizer samples these")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct CpuTexture<'a> {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// 1 for a coverage mask (`R8Unorm`), 4 for colour (`Rgba8Unorm[Srgb]`).
    pub(crate) bytes_per_pixel: u32,
    /// Whether sampling decodes sRGB, as an `*Srgb` texture format does.
    pub(crate) srgb: bool,
    /// `width * height * bytes_per_pixel` bytes, rows packed.
    pub(crate) pixels: &'a [u8],
}

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
#[expect(dead_code, reason = "the software rasterizer reads every field")]
pub(crate) enum CpuDraw<'a> {
    /// `quad.wgsl`: flat fills in straight sRGB colour.
    Quads {
        replace: bool,
        instances: &'a [QuadInstance],
    },
    /// `glyphpipe.rs`'s shader over its two atlases.
    Glyphs {
        instances: &'a [GlyphInstance],
        color_atlas: CpuTexture<'a>,
        mask_atlas: CpuTexture<'a>,
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
/// `scissor` is `[x, y, width, height]`, already inside `target`, and never
/// empty. Instance positions are in scene pixels, as the shaders take them;
/// the scene's origin is the target's.
pub(crate) trait CpuRaster: Send {
    fn draw(&mut self, target: &mut CpuFrame, scissor: [u32; 4], draw: &CpuDraw<'_>);

    /// The presentation pass (`present.rs`): `scene` converted to straight
    /// alpha into `output`, for a translucent `PostMultiplied` surface.
    fn present(&mut self, scene: &CpuFrame, output: &mut CpuFrame);
}

/// The placeholder rasterizer: draws nothing and presents the scene as is.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NoRaster;

impl CpuRaster for NoRaster {
    fn draw(&mut self, _target: &mut CpuFrame, _scissor: [u32; 4], _draw: &CpuDraw<'_>) {}

    fn present(&mut self, scene: &CpuFrame, output: &mut CpuFrame) {
        output.copy_from(scene);
    }
}

/// A scene pass on the CPU: the stand-in for a `wgpu::RenderPass`.
pub(crate) struct CpuPass<'e> {
    target: &'e mut CpuFrame,
    raster: &'e mut dyn CpuRaster,
    records: &'e mut Vec<CpuDrawRecord>,
    scissor: [u32; 4],
}

impl<'e> CpuPass<'e> {
    /// A pass over `target`, which the caller has already cleared, as the
    /// GPU pass's `LoadOp::Clear` does.
    pub(crate) fn new(
        target: &'e mut CpuFrame,
        raster: &'e mut dyn CpuRaster,
        records: &'e mut Vec<CpuDrawRecord>,
    ) -> Self {
        let scissor = [0, 0, target.width(), target.height()];
        Self {
            target,
            raster,
            records,
            scissor,
        }
    }

    pub(crate) fn set_scissor_rect(&mut self, x: u32, y: u32, width: u32, height: u32) {
        let (target_w, target_h) = (self.target.width(), self.target.height());
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
        if self.scissor[2] > 0 && self.scissor[3] > 0 {
            self.raster.draw(self.target, self.scissor, &draw);
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
#[expect(dead_code, reason = "the window presenter returns these")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CpuPresentOutcome {
    /// The frame is on screen.
    Presented,
    /// The window cannot show it now; keep damage and try later.
    Occluded,
    /// The compositor is not ready for another buffer yet.
    RetryLater,
    /// The window's buffers are gone; rebuild the presenter.
    Lost,
}

/// Shows CPU frames in a window: shared-memory buffers on Wayland and X11.
pub(crate) trait CpuPresenter: Send {
    /// The window's buffers now have `width` x `height` pixels.
    fn resize(&mut self, width: u32, height: u32);

    /// Show `frame`, whose pixels are straight or premultiplied as the
    /// surface's alpha mode asks (see `Renderer::render_frame`).
    fn present(&mut self, frame: &CpuFrame) -> CpuPresentOutcome;
}

/// A CPU renderer's frame state.
pub(crate) struct CpuBackend {
    /// The scene target every frame clears and draws into.
    pub(crate) scene: CpuFrame,
    /// The scene's straight-alpha conversion, for a translucent
    /// `PostMultiplied` surface.
    pub(crate) presented: CpuFrame,
    /// Whether the last frame presented `presented` rather than `scene`.
    /// With no presenter, that frame stays here: an offscreen renderer
    /// presents to this buffer.
    pub(crate) converted: bool,
    pub(crate) raster: Box<dyn CpuRaster>,
    /// `None` for an offscreen renderer.
    pub(crate) presenter: Option<Box<dyn CpuPresenter>>,
    /// The draws of the last scene pass.
    pub(crate) draws: Vec<CpuDrawRecord>,
}

impl CpuBackend {
    pub(crate) fn new(presenter: Option<Box<dyn CpuPresenter>>) -> Self {
        Self {
            scene: CpuFrame::default(),
            presented: CpuFrame::default(),
            converted: false,
            raster: Box::new(NoRaster),
            presenter,
            draws: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_clears_every_pixel() {
        let mut frame = CpuFrame::new(3, 2).expect("small frame");
        frame.clear([1, 2, 3, 4]);
        assert_eq!(frame.pixels().len(), 3 * 2 * 4);
        assert!(
            frame
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [1, 2, 3, 4])
        );
        assert!(frame.resize(1, 1));
        assert_eq!(frame.pixels().len(), 4);
    }

    #[test]
    fn an_overflowing_frame_is_refused_and_left_empty() {
        let mut frame = CpuFrame::new(2, 2).expect("small frame");
        assert!(!frame.resize(u32::MAX, u32::MAX));
        assert_eq!((frame.width(), frame.height()), (0, 0));
        assert!(frame.pixels().is_empty());
    }

    #[test]
    fn scissors_are_clamped_to_the_target() {
        let mut frame = CpuFrame::new(10, 8).expect("small frame");
        let mut raster = NoRaster;
        let mut records = Vec::new();
        let mut pass = CpuPass::new(&mut frame, &mut raster, &mut records);
        pass.set_scissor_rect(6, 2, 100, 100);
        assert_eq!(pass.scissor, [6, 2, 4, 6]);
        pass.set_scissor_rect(20, 20, 5, 5);
        assert_eq!(pass.scissor, [10, 8, 0, 0]);
    }

    #[test]
    fn a_pass_records_each_non_empty_draw_under_its_scissor() {
        let mut frame = CpuFrame::new(10, 8).expect("small frame");
        let mut raster = NoRaster;
        let mut records = Vec::new();
        let quad = QuadInstance {
            pos: [0.0, 0.0],
            size: [1.0, 1.0],
            color: [1.0; 4],
        };
        {
            let mut pass = CpuPass::new(&mut frame, &mut raster, &mut records);
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
