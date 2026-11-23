//! The seam between building a frame and drawing it.
//!
//! A [`Renderer`](crate::Renderer) builds every frame the same way: the
//! instance lists for its pipelines and the text areas for glyphon. What
//! executes those lists is its backend:
//!
//! - [`RendererBackend::Gpu`] draws them with wgpu, as Kettle always has.
//! - [`RendererBackend::Cpu`] creates no wgpu instance, adapter, device or
//!   surface. Its pipelines keep CPU copies of what they are given and replay
//!   each draw into a CPU frame ([`cpu`]).
//!
//! Three handles cross the seam:
//!
//! - [`DeviceRef`]: what a pipeline creates its resources on and uploads to,
//!   a wgpu device and queue or CPU memory.
//! - [`ScenePass`]: what a pipeline draws into, a wgpu render pass or a
//!   [`cpu::CpuPass`].
//! - The layer types in [`layers`] and the glyphon stand-ins in [`text`],
//!   which hold either the existing GPU pipeline or its CPU twin and pick the
//!   matching path for each call.

pub(crate) mod cpu;
mod cpu_renderer;
pub(crate) mod layers;
pub(crate) mod text;

pub(crate) use cpu_renderer::CpuFrameRequest;

#[cfg(test)]
mod cpu_tests;

use crate::GpuContext;

/// Which backend draws a renderer's frames.
///
/// [`RendererBackend::Gpu`] is the default everywhere, and the backend of
/// every windowed renderer. [`RendererBackend::Cpu`] is not selectable from
/// configuration yet, and only [`Renderer::new_cpu_offscreen`] builds one:
/// until its rasterizer and presenter land it draws only its clear, and
/// records every other draw.
///
/// [`Renderer::new_cpu_offscreen`]: crate::Renderer::new_cpu_offscreen
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RendererBackend {
    /// wgpu, on the adapter Kettle selects.
    #[default]
    Gpu,
    /// Software rasterization into shared memory, with no GPU driver loaded.
    Cpu,
}

impl RendererBackend {
    /// The stable lower-case name, as logs and a future `renderer` key use it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
        }
    }
}

/// The largest texture side a CPU-backend pipeline accepts: the 2D limit of
/// common Vulkan adapters, lavapipe's included, so a CPU renderer sizes its
/// surface, atlases and image textures as such a GPU renderer would.
pub(crate) const CPU_MAX_TEXTURE_DIMENSION: u32 = 16_384;

/// The largest instance buffer a CPU-backend glyph pipeline grows to,
/// standing in for a device's `max_buffer_size`.
pub(crate) const CPU_MAX_BUFFER_SIZE: u64 = 1 << 30;

/// The device a renderer owns: the shared GPU context, or the CPU backend's
/// frame state.
pub(crate) enum RenderDevice {
    Gpu(GpuContext),
    /// `None` only while a frame is being drawn: the frame takes the state out
    /// so its pass can borrow the renderer's pipelines at the same time.
    Cpu(Option<Box<cpu::CpuBackend>>),
}

impl RenderDevice {
    pub(crate) fn backend(&self) -> RendererBackend {
        match self {
            Self::Gpu(_) => RendererBackend::Gpu,
            Self::Cpu(_) => RendererBackend::Cpu,
        }
    }

    pub(crate) fn gpu(&self) -> Option<&GpuContext> {
        match self {
            Self::Gpu(gpu) => Some(gpu),
            Self::Cpu(_) => None,
        }
    }

    /// The handle pipelines take for uploads.
    pub(crate) fn handle(&self) -> DeviceRef<'_> {
        match self {
            Self::Gpu(gpu) => DeviceRef::Gpu {
                device: &gpu.device,
                queue: &gpu.queue,
            },
            Self::Cpu(_) => DeviceRef::Cpu,
        }
    }

    pub(crate) fn max_texture_dimension_2d(&self) -> u32 {
        self.handle().max_texture_dimension_2d()
    }

    /// Whether uploads go through mapped buffers (see `upload.rs`). Never on
    /// the CPU backend.
    pub(crate) fn mapped_uploads(&self) -> bool {
        self.gpu().is_some_and(GpuContext::mapped_uploads)
    }
}

/// What a pipeline creates resources on and writes to.
#[derive(Clone, Copy)]
pub(crate) enum DeviceRef<'a> {
    Gpu {
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
    },
    Cpu,
}

impl<'a> DeviceRef<'a> {
    pub(crate) fn gpu(device: &'a wgpu::Device, queue: &'a wgpu::Queue) -> Self {
        Self::Gpu { device, queue }
    }

    pub(crate) fn max_texture_dimension_2d(self) -> u32 {
        match self {
            Self::Gpu { device, .. } => device.limits().max_texture_dimension_2d,
            Self::Cpu => CPU_MAX_TEXTURE_DIMENSION,
        }
    }
}

/// What one scene pass draws into.
pub(crate) enum ScenePass<'p, 'e> {
    Gpu(&'p mut wgpu::RenderPass<'e>),
    Cpu(&'p mut cpu::CpuPass<'e>),
}

impl ScenePass<'_, '_> {
    /// Clip later draws to `[x, y, width, height]` in target pixels.
    pub(crate) fn set_scissor_rect(&mut self, x: u32, y: u32, width: u32, height: u32) {
        match self {
            Self::Gpu(pass) => pass.set_scissor_rect(x, y, width, height),
            Self::Cpu(pass) => pass.set_scissor_rect(x, y, width, height),
        }
    }

    /// Map the scene onto the target with its top-left at `(x, y)`, as a
    /// cursor patch pass does.
    pub(crate) fn set_viewport(&mut self, x: f32, y: f32, width: f32, height: f32) {
        match self {
            Self::Gpu(pass) => pass.set_viewport(x, y, width, height, 0.0, 1.0),
            Self::Cpu(pass) => pass.set_viewport(x, y, width, height),
        }
    }
}
