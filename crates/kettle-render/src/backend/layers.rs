//! Each scene pipeline behind the backend seam.
//!
//! A layer holds either the existing wgpu pipeline, whose code is unchanged,
//! or its CPU twin, which keeps a copy of the same instances (and, for
//! images, the same textures) and replays each draw into a
//! [`CpuPass`](super::cpu::CpuPass). A renderer builds every layer on its own
//! backend, so a GPU layer only ever meets a GPU device and pass, and a CPU
//! layer a CPU one.
//!
//! The glyph pipeline crosses the seam inside `glyphpipe.rs` instead, because
//! its atlas packing, slot cache and scissors must be the same code on both
//! backends; glyphon crosses it in [`super::text`].

use std::ops::Range;

use kettle_core::{GraphicsBudget, GraphicsReservation, ImageData};

use super::cpu::{CpuDraw, instance_slice};
use super::{DeviceRef, ScenePass};
use crate::imgpipe::{CpuImageLayer, ImageInstance, ImageItem, ImagePipeline, ImageShared};
use crate::outline::{OutlineInstance, OutlinePipeline};
use crate::present::PresentationPipeline;
use crate::quad::{QuadInstance, QuadPipeline, visible_instance_ranges};
use crate::rounded_surface::{RoundedSurface, RoundedSurfacePipeline};
use crate::starfield::{CpuStarfield, StarfieldPipeline};
use crate::upload::UploadCounts;

/// A GPU layer was handed a CPU device or pass, or the reverse. A renderer
/// builds all its layers on one backend, so this cannot happen; the call is
/// skipped rather than drawn on the wrong backend.
fn backend_mismatch() {
    debug_assert!(
        false,
        "a layer was used with another backend's device or pass"
    );
}

/// The instances a layer last uploaded, from either backend.
pub(crate) enum Uploaded<G, C> {
    Gpu(G),
    Cpu(C),
}

impl<T, G: Iterator<Item = T>, C: Iterator<Item = T>> Iterator for Uploaded<G, C> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        match self {
            Self::Gpu(iter) => iter.next(),
            Self::Cpu(iter) => iter.next(),
        }
    }
}

/// `quad.rs`: flat rects, blended or replacing.
pub(crate) enum QuadLayer {
    Gpu(QuadPipeline),
    Cpu {
        replace: bool,
        instances: Vec<QuadInstance>,
    },
}

impl QuadLayer {
    /// A CPU layer that blends over what lies beneath it.
    pub(crate) fn cpu_blend() -> Self {
        Self::Cpu {
            replace: false,
            instances: Vec::new(),
        }
    }

    /// A CPU layer that replaces the pixels it covers, alpha included.
    pub(crate) fn cpu_replace() -> Self {
        Self::Cpu {
            replace: true,
            instances: Vec::new(),
        }
    }

    pub(crate) fn upload(
        &mut self,
        device: DeviceRef<'_>,
        screen: [f32; 2],
        data: &[QuadInstance],
    ) {
        match (self, device) {
            (Self::Gpu(pipeline), DeviceRef::Gpu { device, queue }) => {
                pipeline.upload(device, queue, screen, data);
            }
            (Self::Cpu { instances, .. }, DeviceRef::Cpu) => {
                instances.clear();
                // The GPU layer draws nothing when the count overflows a draw.
                if u32::try_from(data.len()).is_ok() {
                    instances.extend_from_slice(data);
                }
            }
            _ => backend_mismatch(),
        }
    }

    /// How many instances the last upload left to draw.
    pub(crate) fn count(&self) -> u32 {
        match self {
            Self::Gpu(pipeline) => pipeline.count,
            Self::Cpu { instances, .. } => instances.len() as u32,
        }
    }

    pub(crate) fn upload_counts(&self) -> UploadCounts {
        match self {
            Self::Gpu(pipeline) => pipeline.upload_counts(),
            Self::Cpu { .. } => UploadCounts::default(),
        }
    }

    /// The instances the last upload left to draw. `None` when the GPU layer's
    /// retained copy does not cover them.
    pub(crate) fn uploaded(&self) -> Option<impl Iterator<Item = QuadInstance> + '_> {
        match self {
            Self::Gpu(pipeline) => pipeline.uploaded().map(Uploaded::Gpu),
            Self::Cpu { instances, .. } => Some(Uploaded::Cpu(instances.iter().copied())),
        }
    }

    pub(crate) fn draw(&self, pass: &mut ScenePass<'_, '_>) {
        self.draw_hiding(pass, None);
    }

    /// Draw every instance except `hidden`, in their uploaded order.
    pub(crate) fn draw_hiding(&self, pass: &mut ScenePass<'_, '_>, hidden: Option<Range<u32>>) {
        match (self, pass) {
            (Self::Gpu(pipeline), ScenePass::Gpu(pass)) => pipeline.draw_hiding(pass, hidden),
            (Self::Cpu { .. }, ScenePass::Cpu(pass)) => {
                for range in visible_instance_ranges(self.count(), hidden) {
                    self.draw_cpu_range(pass, range);
                }
            }
            _ => backend_mismatch(),
        }
    }

    pub(crate) fn draw_range(&self, pass: &mut ScenePass<'_, '_>, range: Range<u32>) {
        match (self, pass) {
            (Self::Gpu(pipeline), ScenePass::Gpu(pass)) => pipeline.draw_range(pass, range),
            (Self::Cpu { .. }, ScenePass::Cpu(pass)) => self.draw_cpu_range(pass, range),
            _ => backend_mismatch(),
        }
    }

    fn draw_cpu_range(&self, pass: &mut super::cpu::CpuPass<'_>, range: Range<u32>) {
        if let Self::Cpu { replace, instances } = self {
            pass.draw(CpuDraw::Quads {
                replace: *replace,
                instances: instance_slice(instances, range),
            });
        }
    }

    /// The GPU pipeline, for tests that inspect it.
    #[cfg(test)]
    pub(crate) fn gpu(&self) -> Option<&QuadPipeline> {
        match self {
            Self::Gpu(pipeline) => Some(pipeline),
            Self::Cpu { .. } => None,
        }
    }
}

/// `outline.rs`: rounded or dashed pane outlines.
pub(crate) enum OutlineLayer {
    Gpu(OutlinePipeline),
    Cpu(Vec<OutlineInstance>),
}

impl OutlineLayer {
    pub(crate) fn upload(
        &mut self,
        device: DeviceRef<'_>,
        screen: [f32; 2],
        data: &[OutlineInstance],
    ) {
        match (self, device) {
            (Self::Gpu(pipeline), DeviceRef::Gpu { device, queue }) => {
                pipeline.upload(device, queue, screen, data);
            }
            (Self::Cpu(instances), DeviceRef::Cpu) => {
                instances.clear();
                if u32::try_from(data.len()).is_ok() {
                    instances.extend_from_slice(data);
                } else {
                    log::warn!(
                        "pane outline instance count {} exceeds u32; skipping upload",
                        data.len()
                    );
                }
            }
            _ => backend_mismatch(),
        }
    }

    pub(crate) fn upload_counts(&self) -> UploadCounts {
        match self {
            Self::Gpu(pipeline) => pipeline.upload_counts(),
            Self::Cpu(_) => UploadCounts::default(),
        }
    }

    pub(crate) fn uploaded(&self) -> Option<impl Iterator<Item = OutlineInstance> + '_> {
        match self {
            Self::Gpu(pipeline) => pipeline.uploaded().map(Uploaded::Gpu),
            Self::Cpu(instances) => Some(Uploaded::Cpu(instances.iter().copied())),
        }
    }

    pub(crate) fn draw(&self, pass: &mut ScenePass<'_, '_>) {
        match (self, pass) {
            (Self::Gpu(pipeline), ScenePass::Gpu(pass)) => pipeline.draw(pass),
            (Self::Cpu(instances), ScenePass::Cpu(pass)) => {
                pass.draw(CpuDraw::Outlines { instances });
            }
            _ => backend_mismatch(),
        }
    }
}

/// `rounded_surface.rs`: menus, dialogs, toasts, swatches and media icons.
pub(crate) enum RoundedSurfaceLayer {
    Gpu(RoundedSurfacePipeline),
    Cpu {
        capacity: usize,
        instances: Vec<RoundedSurface>,
    },
}

impl RoundedSurfaceLayer {
    /// A CPU layer drawing at most `capacity` surfaces a frame, as
    /// [`RoundedSurfacePipeline::with_capacity`] builds one.
    pub(crate) fn cpu(capacity: usize) -> Self {
        Self::Cpu {
            capacity,
            instances: Vec::new(),
        }
    }

    pub(crate) fn upload(
        &mut self,
        device: DeviceRef<'_>,
        screen: [f32; 2],
        data: &[RoundedSurface],
    ) {
        match (self, device) {
            (Self::Gpu(pipeline), DeviceRef::Gpu { device, queue }) => {
                pipeline.upload(device, queue, screen, data);
            }
            (
                Self::Cpu {
                    capacity,
                    instances,
                },
                DeviceRef::Cpu,
            ) => {
                instances.clear();
                if data.len() > *capacity {
                    log::warn!("rounded surface budget exceeded; skipping frame");
                } else if u32::try_from(data.len()).is_ok() {
                    instances.extend_from_slice(data);
                }
            }
            _ => backend_mismatch(),
        }
    }

    pub(crate) fn upload_counts(&self) -> UploadCounts {
        match self {
            Self::Gpu(pipeline) => pipeline.upload_counts(),
            Self::Cpu { .. } => UploadCounts::default(),
        }
    }

    pub(crate) fn uploaded(&self) -> Option<impl Iterator<Item = RoundedSurface> + '_> {
        match self {
            Self::Gpu(pipeline) => pipeline.uploaded().map(Uploaded::Gpu),
            Self::Cpu { instances, .. } => Some(Uploaded::Cpu(instances.iter().copied())),
        }
    }

    pub(crate) fn draw_range(&self, pass: &mut ScenePass<'_, '_>, range: Range<u32>) {
        match (self, pass) {
            (Self::Gpu(pipeline), ScenePass::Gpu(pass)) => pipeline.draw_range(pass, range),
            (Self::Cpu { instances, .. }, ScenePass::Cpu(pass)) => {
                pass.draw(CpuDraw::RoundedSurfaces {
                    instances: instance_slice(instances, range),
                });
            }
            _ => backend_mismatch(),
        }
    }
}

/// `imgpipe.rs`: inline images, wallpaper, posters, previews.
#[expect(
    clippy::large_enum_variant,
    reason = "one per renderer, built once and never moved while drawing"
)]
pub(crate) enum ImageLayer {
    Gpu(ImagePipeline),
    Cpu(CpuImageLayer),
}

impl ImageLayer {
    pub(crate) fn prepare_frame(&mut self, device: DeviceRef<'_>, items: &[ImageItem]) {
        match (self, device) {
            (Self::Gpu(pipeline), DeviceRef::Gpu { device, .. }) => {
                pipeline.prepare_frame(device, items);
            }
            (Self::Cpu(layer), DeviceRef::Cpu) => layer.prepare_frame(items),
            _ => backend_mismatch(),
        }
    }

    pub(crate) fn upload(&mut self, device: DeviceRef<'_>, screen: [f32; 2], items: &[ImageItem]) {
        match (self, device) {
            (Self::Gpu(pipeline), DeviceRef::Gpu { device, queue }) => {
                pipeline.upload(device, queue, screen, items);
            }
            (Self::Cpu(layer), DeviceRef::Cpu) => layer.upload(items),
            _ => backend_mismatch(),
        }
    }

    /// Upload unless `items` match the last complete upload; whether every
    /// item is now drawable.
    pub(crate) fn upload_retained(
        &mut self,
        device: DeviceRef<'_>,
        screen: [f32; 2],
        items: &[ImageItem],
    ) -> bool {
        match (self, device) {
            (Self::Gpu(pipeline), DeviceRef::Gpu { device, queue }) => {
                pipeline.upload_retained(device, queue, screen, items)
            }
            (Self::Cpu(layer), DeviceRef::Cpu) => layer.upload_retained(screen, items),
            _ => {
                backend_mismatch();
                false
            }
        }
    }

    pub(crate) fn upload_counts(&self) -> UploadCounts {
        match self {
            Self::Gpu(pipeline) => pipeline.upload_counts(),
            Self::Cpu(_) => UploadCounts::default(),
        }
    }

    pub(crate) fn drawn_item_indices(&self) -> impl Iterator<Item = usize> + '_ {
        match self {
            Self::Gpu(pipeline) => Uploaded::Gpu(pipeline.drawn_item_indices()),
            Self::Cpu(layer) => Uploaded::Cpu(layer.drawn_item_indices()),
        }
    }

    pub(crate) fn has_draws(&self) -> bool {
        match self {
            Self::Gpu(pipeline) => pipeline.has_draws(),
            Self::Cpu(layer) => layer.has_draws(),
        }
    }

    pub(crate) fn drawn_rects(&self) -> Option<Vec<[f32; 4]>> {
        match self {
            Self::Gpu(pipeline) => pipeline.drawn_rects(),
            Self::Cpu(layer) => Some(layer.drawn_rects()),
        }
    }

    pub(crate) fn draw(&self, pass: &mut ScenePass<'_, '_>) {
        match (self, pass) {
            (Self::Gpu(pipeline), ScenePass::Gpu(pass)) => pipeline.draw(pass),
            (Self::Cpu(layer), ScenePass::Cpu(pass)) => {
                for (instances, image, repeat) in layer.draws() {
                    pass.draw(image_draw(instances, image, repeat));
                }
            }
            _ => backend_mismatch(),
        }
    }

    /// The GPU pipeline, for tests that inspect it.
    #[cfg(test)]
    pub(crate) fn gpu(&self) -> Option<&ImagePipeline> {
        match self {
            Self::Gpu(pipeline) => Some(pipeline),
            Self::Cpu(_) => None,
        }
    }
}

fn image_draw<'a>(
    instances: &'a [ImageInstance],
    image: &'a ImageData,
    repeat: bool,
) -> CpuDraw<'a> {
    CpuDraw::Images {
        instances,
        image,
        repeat,
    }
}

/// What a renderer needs to make image layers after startup.
pub(crate) enum ImageSharedLayer {
    Gpu(ImageShared),
    Cpu,
}

impl ImageSharedLayer {
    /// A layer allowed `max_instances` placements per frame. `None` when it
    /// may draw none or the budget cannot hold its buffers.
    pub(crate) fn layer_with_instance_limit(
        &self,
        device: DeviceRef<'_>,
        budget: GraphicsBudget,
        max_instances: usize,
    ) -> Option<ImageLayer> {
        match (self, device) {
            (Self::Gpu(shared), DeviceRef::Gpu { device, .. }) => shared
                .layer_with_instance_limit(device, budget, max_instances)
                .map(ImageLayer::Gpu),
            (Self::Cpu, DeviceRef::Cpu) => {
                CpuImageLayer::new(budget, max_instances).map(ImageLayer::Cpu)
            }
            _ => {
                backend_mismatch();
                None
            }
        }
    }
}

/// `starfield.rs`: the procedural wallpaper.
#[expect(
    clippy::large_enum_variant,
    reason = "one per renderer, built once and never moved while drawing"
)]
pub(crate) enum StarfieldLayer {
    Gpu(StarfieldPipeline),
    Cpu(CpuStarfield),
}

impl StarfieldLayer {
    pub(crate) fn upload(&mut self, device: DeviceRef<'_>, resolution: [f32; 2], time_secs: f32) {
        match (self, device) {
            (Self::Gpu(pipeline), DeviceRef::Gpu { queue, .. }) => {
                pipeline.upload(queue, resolution, time_secs);
            }
            (Self::Cpu(starfield), DeviceRef::Cpu) => starfield.upload(resolution, time_secs),
            _ => backend_mismatch(),
        }
    }

    pub(crate) fn upload_counts(&self) -> UploadCounts {
        match self {
            Self::Gpu(pipeline) => pipeline.upload_counts(),
            Self::Cpu(_) => UploadCounts::default(),
        }
    }

    pub(crate) fn draw(&self, pass: &mut ScenePass<'_, '_>) {
        match (self, pass) {
            (Self::Gpu(pipeline), ScenePass::Gpu(pass)) => pipeline.draw(pass),
            (Self::Cpu(starfield), ScenePass::Cpu(pass)) => {
                pass.draw(CpuDraw::Starfield {
                    uniforms: starfield.uniforms(),
                });
            }
            _ => backend_mismatch(),
        }
    }
}

/// `present.rs`: the straight-alpha conversion target for a translucent
/// `PostMultiplied` surface. On the CPU the conversion itself is
/// [`CpuRaster::present`](super::cpu::CpuRaster::present); this layer only
/// charges the graphics budget for its target, as the GPU one does.
#[expect(
    clippy::large_enum_variant,
    reason = "one per renderer, built once and never moved while drawing"
)]
pub(crate) enum PresentLayer {
    Gpu(PresentationPipeline),
    Cpu {
        budget: GraphicsBudget,
        target: Option<([u32; 2], GraphicsReservation)>,
    },
}

impl PresentLayer {
    pub(crate) fn cpu(budget: GraphicsBudget) -> Self {
        Self::Cpu {
            budget,
            target: None,
        }
    }

    /// Hold a `width` x `height` presentation target; false when the budget
    /// refuses it.
    pub(crate) fn ensure_target(&mut self, device: DeviceRef<'_>, width: u32, height: u32) -> bool {
        match (self, device) {
            (Self::Gpu(pipeline), DeviceRef::Gpu { device, .. }) => {
                pipeline.ensure_target(device, width, height)
            }
            (Self::Cpu { budget, target }, DeviceRef::Cpu) => {
                if target
                    .as_ref()
                    .is_some_and(|(size, _)| *size == [width, height])
                {
                    return true;
                }
                *target = None;
                let Some(bytes) = u64::from(width)
                    .checked_mul(u64::from(height))
                    .and_then(|pixels| pixels.checked_mul(4))
                    .and_then(|bytes| usize::try_from(bytes).ok())
                else {
                    return false;
                };
                let Some(reservation) = crate::present::reserve_scene_target(budget, bytes) else {
                    return false;
                };
                *target = Some(([width, height], reservation));
                true
            }
            _ => {
                backend_mismatch();
                false
            }
        }
    }

    pub(crate) fn discard_target(&mut self) {
        match self {
            Self::Gpu(pipeline) => pipeline.discard_target(),
            Self::Cpu { target, .. } => *target = None,
        }
    }

    /// The GPU pipeline, which the GPU frame draws through.
    pub(crate) fn gpu(&self) -> Option<&PresentationPipeline> {
        match self {
            Self::Gpu(pipeline) => Some(pipeline),
            Self::Cpu { .. } => None,
        }
    }
}
