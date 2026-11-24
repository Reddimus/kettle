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

use super::cpu_damage::{DamageTracker, TileHashes};
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
        /// Changes whenever a texel an earlier frame may have sampled is
        /// written again (an evicted slot reused, the atlas cleared), so
        /// damage tracking sees an unchanged instance over changed texels.
        atlas_epoch: u64,
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

    /// Draw only the part of `draw` inside `clip`, which lies inside its
    /// `scissor`. The pixels inside `clip` must be exactly those
    /// [`Self::draw`] under `scissor` gives them: damage tracking repaints a
    /// frame piece by piece and keeps the rest from earlier frames.
    fn draw_clipped(
        &mut self,
        target: &mut CpuTarget<'_>,
        screen: [f32; 2],
        scissor: PixelRect,
        clip: PixelRect,
        draw: &CpuDraw<'_>,
    ) {
        let _ = scissor;
        self.draw(target, screen, clip, draw);
    }

    /// Called once for each draw of a frame, before any of it is drawn, in
    /// draw order: the rasterizer settles any per-frame choice of how to draw
    /// it (such as drawing an image from a cache), and returns a value that
    /// changes whenever that choice could change its pixels. Damage tracking
    /// hashes it with the draw.
    fn plan(&mut self, screen: [f32; 2], scissor: PixelRect, draw: &CpuDraw<'_>) -> u64 {
        let _ = (screen, scissor, draw);
        0
    }

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
    sink: PassSink<'e>,
    size: [u32; 2],
    records: Option<&'e mut Vec<CpuDrawRecord>>,
    scissor: [u32; 4],
}

/// Where a pass's draws go.
enum PassSink<'e> {
    /// Rasterized into `target`, only inside `clips` when given.
    Raster {
        target: CpuTarget<'e>,
        raster: &'e mut dyn CpuRaster,
        clips: Option<&'e [PixelRect]>,
        /// [`CpuRaster::plan`] each draw first; a signing pass already did.
        plan: bool,
    },
    /// Hashed into a damage signature (`cpu_damage.rs`).
    Sign {
        tiles: &'e mut TileHashes,
        raster: &'e mut dyn CpuRaster,
    },
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
        Self::clipped(target, size, raster, records, None, true)
    }

    /// A pass over `target` that draws only inside `clips` (all of it when
    /// `None`), which must not overlap and which the caller has cleared.
    /// `plan` is false when a [`Self::signing`] pass of the same frame has
    /// already planned every draw.
    pub(crate) fn clipped(
        target: CpuTarget<'e>,
        size: [u32; 2],
        raster: &'e mut dyn CpuRaster,
        records: &'e mut Vec<CpuDrawRecord>,
        clips: Option<&'e [PixelRect]>,
        plan: bool,
    ) -> Self {
        Self {
            sink: PassSink::Raster {
                target,
                raster,
                clips,
                plan,
            },
            size,
            records: Some(records),
            scissor: [0, 0, size[0], size[1]],
        }
    }

    /// A pass that draws nothing and hashes every draw into `tiles` (begun
    /// at `size`), planning each with `raster` as the drawing pass will
    /// find it.
    pub(crate) fn signing(
        tiles: &'e mut TileHashes,
        size: [u32; 2],
        raster: &'e mut dyn CpuRaster,
    ) -> Self {
        Self {
            sink: PassSink::Sign { tiles, raster },
            size,
            records: None,
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
        if let Some(records) = self.records.as_deref_mut() {
            records.push(CpuDrawRecord {
                pipeline: draw.pipeline(),
                instances: draw.len(),
                scissor: self.scissor,
            });
        }
        let [x, y, w, h] = self.scissor;
        if w == 0 || h == 0 {
            return;
        }
        let scissor = PixelRect::new(x, y, w, h);
        let screen = [self.size[0] as f32, self.size[1] as f32];
        match &mut self.sink {
            PassSink::Sign { tiles, raster } => {
                let salt = raster.plan(screen, scissor, &draw);
                tiles.draw(screen, scissor, salt, &draw);
            }
            PassSink::Raster {
                target,
                raster,
                clips,
                plan,
            } => {
                if *plan {
                    raster.plan(screen, scissor, &draw);
                }
                match clips {
                    None => raster.draw_clipped(target, screen, scissor, scissor, &draw),
                    Some(clips) => {
                        for clip in clips.iter() {
                            if let Some(clip) = scissor.intersect(*clip) {
                                raster.draw_clipped(target, screen, scissor, clip, &draw);
                            }
                        }
                    }
                }
            }
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
    /// The frame was committed: it is on its way to the screen.
    Presented,
    /// The frame was drawn but is held for the compositor's frame callback;
    /// [`CpuSurface::flush`] commits it. Its damage is not lost.
    Deferred,
    /// Nothing changed and nothing was pending: no commit.
    Unchanged,
    /// The compositor has not taken the last frame yet, so the frame was
    /// not drawn: drawing it now would only be held, and drawn over by the
    /// next one. Draw again once [`CpuSurface::frame_pending`] clears.
    Paced,
    /// The window cannot show it now; keep damage and try later.
    Occluded,
    /// The compositor holds every buffer; draw again later.
    RetryLater,
    /// The window's buffers are gone; rebuild the presenter.
    Lost,
    /// The draw callback refused the frame, and nothing was presented.
    NotDrawn,
}

/// What flushing a held frame did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeldFrameFlush {
    /// No frame was held.
    Nothing,
    /// The held frame was committed.
    Presented,
    /// The compositor is still showing the last frame; the frame stays held
    /// until [`crate::Renderer::cpu_wake_deadline`].
    StillHeld,
    /// The presenter failed; the frame is gone.
    Failed,
}

/// What draws a window frame into a presenter's buffer: its pixels, width
/// and height, and its repaint region ([`CpuSurface::present_frame`]).
pub(crate) type DrawFrame<'a> = dyn FnMut(&mut [u32], u32, u32, &[PixelRect]) -> bool + 'a;

/// A window's shared-memory buffers, as the CPU backend draws into them:
/// `cpu_present::CpuPresenter` on Linux, a fake in tests.
///
/// Frames are presented and flushed on the event loop's thread only: a
/// Wayland commit applies everything staged on the surface, which the
/// window system's own code also commits from that thread.
pub(crate) trait CpuSurface: Send {
    /// The window is now `width` x `height` physical pixels at `scale`.
    fn resize(&mut self, width: u32, height: u32, scale: f64);

    /// The size the next buffer gets, which may round the window's size up
    /// (an integer-scale Wayland buffer is a multiple of the scale).
    fn buffer_size(&self) -> (u32, u32);

    /// Whether the window can show premultiplied alpha.
    fn supports_translucency(&self) -> bool;

    /// Ask for premultiplied (`true`) or opaque buffers. Returns whether
    /// frames are premultiplied now.
    fn set_translucent(&mut self, translucent: bool) -> bool;

    /// Draw the next buffer with `draw` and present it with `damage`, what
    /// changed since the previous frame presented, running `pre_commit` only
    /// if, and just before, the frame is committed.
    ///
    /// `draw` gets the buffer's pixels, width and height, and its repaint
    /// region: `damage` widened by the buffer's age, the whole buffer when
    /// its contents are unknown. Pixels outside the region hold the
    /// previous frame; inside it they are undefined and must all be drawn.
    /// `draw` is not called when the region is empty, and returns `false` to
    /// drop the frame. While [`Self::frame_pending`] holds, the frame is not
    /// drawn at all ([`CpuPresentOutcome::Paced`]).
    fn present_frame(
        &mut self,
        damage: &[PixelRect],
        draw: &mut DrawFrame<'_>,
        pre_commit: &mut dyn FnMut(),
    ) -> CpuPresentOutcome;

    /// Commit a frame held for the compositor's frame callback, if it is
    /// ready for it, running `pre_commit` only for a commit.
    fn flush(&mut self, pre_commit: &mut dyn FnMut()) -> HeldFrameFlush;

    /// A frame is drawn but not yet committed.
    fn has_deferred(&self) -> bool;

    /// While a frame is held, the latest time to flush it.
    fn pending_deadline(&self) -> Option<Instant>;

    /// The compositor has not taken the last frame: a frame presented now
    /// would be held.
    fn frame_pending(&mut self) -> bool;

    /// While [`Self::frame_pending`], when it stops holding frames back.
    fn frame_deadline(&self) -> Option<Instant>;

    /// Release spare buffers now; returns how many went.
    fn release_idle_buffers(&mut self) -> usize;

    /// When to call [`Self::release_idle_buffers`] next (possibly now).
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
    /// The graphics budget's charge for `scene`, taken before it is
    /// allocated and released with it.
    pub(crate) scene_reservation: Option<kettle_core::GraphicsReservation>,
    /// The scene's straight-alpha conversion, for a translucent
    /// `PostMultiplied` offscreen surface.
    pub(crate) presented: Option<CpuFramebuffer>,
    /// Whether the last frame presented `presented` rather than `scene`.
    pub(crate) converted: bool,
    pub(crate) raster: Box<dyn CpuRaster>,
    /// `None` for an offscreen renderer.
    pub(crate) surface: Option<Box<dyn CpuSurface>>,
    /// What the frame held for the compositor's frame callback painted,
    /// committed with it.
    pub(crate) held: Option<crate::terminal::HeldPaint>,
    /// A frame was not drawn because the compositor had not taken the last
    /// one ([`CpuPresentOutcome::Paced`]); it is drawn once it can be.
    pub(crate) paced: bool,
    /// The thread that presents, which must stay the event loop's.
    pub(crate) present_thread: Option<std::thread::ThreadId>,
    /// The draws of the last scene pass.
    pub(crate) draws: Vec<CpuDrawRecord>,
    /// A window's damage tracking (`cpu_damage.rs`).
    pub(crate) damage: DamageTracker,
}

impl CpuBackend {
    pub(crate) fn new(surface: Option<Box<dyn CpuSurface>>) -> Self {
        Self {
            scene: None,
            scene_reservation: None,
            presented: None,
            converted: false,
            raster: Box::new(super::cpu_draw::SoftRaster::new()),
            surface,
            held: None,
            paced: false,
            present_thread: None,
            draws: Vec::new(),
            damage: DamageTracker::default(),
        }
    }

    /// Note that the window's frames are presented from this thread. Every
    /// present and flush must come from the same one, winit's event-loop
    /// thread (see `cpu_present`'s module docs): a debug build panics on a
    /// second thread, a release build logs it once.
    pub(crate) fn note_present_thread(&mut self) {
        let current = std::thread::current().id();
        match self.present_thread {
            None => self.present_thread = Some(current),
            Some(thread) if thread == current => {}
            Some(thread) => {
                debug_assert!(
                    false,
                    "a CPU window presented from {current:?} after {thread:?}: \
                     present only on the event-loop thread"
                );
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    log::error!(
                        "renderer: a CPU window was presented from a second thread; \
                         present only on the event-loop thread"
                    );
                });
            }
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

    #[cfg(debug_assertions)]
    #[test]
    fn presenting_from_a_second_thread_is_refused() {
        let mut backend = CpuBackend::new(None);
        backend.note_present_thread();
        backend.note_present_thread();
        let second = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        backend.note_present_thread();
                    }))
                })
                .join()
                .expect("the thread finishes")
        });
        assert!(second.is_err(), "a second presenting thread is a bug");
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
