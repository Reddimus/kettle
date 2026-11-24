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
mod cpu_draw;
mod cpu_renderer;
#[cfg(target_os = "linux")]
mod cpu_window;
pub(crate) mod layers;
pub(crate) mod text;

pub(crate) use cpu_renderer::CpuFrameRequest;

#[cfg(test)]
mod cpu_parity_tests;
#[cfg(test)]
mod cpu_tests;

use crate::GpuContext;

/// Which backend draws a renderer's frames.
///
/// The `renderer` key chooses ([`preferred_backend`]): the CPU on Linux by
/// default, the GPU elsewhere. [`Renderer::new_cpu_window`] builds a CPU
/// window renderer and [`Renderer::new_cpu_offscreen`] an offscreen one.
///
/// [`Renderer::new_cpu_window`]: crate::Renderer
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
    /// The stable lower-case name, as logs and the `renderer` key use it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
        }
    }
}

/// The backend a new window starts on, with the reason for any request the
/// platform cannot honour, which the caller logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendChoice {
    pub backend: RendererBackend,
    pub note: Option<&'static str>,
}

/// The backend `cfg` asks for on this platform (see
/// [`kettle_config::RendererPreference`]).
///
/// - Linux: `auto` and `cpu` draw on the CPU; so does `gpu-force-software`,
///   which means "no GPU driver" there. `gpu` uses wgpu.
/// - Elsewhere: always the GPU. `renderer = cpu` is noted and ignored, since
///   the CPU renderer presents only through Linux shared memory.
///
/// The caller still falls back: to the GPU when the CPU presenter cannot
/// start, and to the CPU when no GPU adapter works.
pub fn preferred_backend(cfg: &kettle_config::Config) -> BackendChoice {
    backend_for(
        cfg.renderer,
        cfg.gpu_force_software,
        cfg!(target_os = "linux"),
    )
}

pub(crate) fn backend_for(
    preference: kettle_config::RendererPreference,
    force_software: bool,
    linux: bool,
) -> BackendChoice {
    use kettle_config::RendererPreference as Pref;
    let (backend, note) = match (linux, preference) {
        (true, Pref::Auto | Pref::Cpu) => (RendererBackend::Cpu, None),
        (true, Pref::Gpu) if force_software => (
            RendererBackend::Cpu,
            Some("gpu-force-software = true selects the CPU renderer on Linux"),
        ),
        (true, Pref::Gpu) | (false, Pref::Auto | Pref::Gpu) => (RendererBackend::Gpu, None),
        (false, Pref::Cpu) => (
            RendererBackend::Gpu,
            Some("renderer = cpu is available on Linux only; using the GPU"),
        ),
    };
    BackendChoice { backend, note }
}

#[cfg(test)]
mod choice_tests {
    use super::*;
    use kettle_config::RendererPreference as Pref;

    #[test]
    fn linux_draws_on_the_cpu_unless_the_gpu_is_asked_for() {
        assert_eq!(
            backend_for(Pref::Auto, false, true).backend,
            RendererBackend::Cpu
        );
        assert_eq!(
            backend_for(Pref::Cpu, false, true).backend,
            RendererBackend::Cpu
        );
        assert_eq!(
            backend_for(Pref::Gpu, false, true).backend,
            RendererBackend::Gpu
        );
        assert_eq!(backend_for(Pref::Auto, false, true).note, None);
    }

    #[test]
    fn force_software_means_the_cpu_renderer_on_linux() {
        let choice = backend_for(Pref::Gpu, true, true);
        assert_eq!(choice.backend, RendererBackend::Cpu);
        assert!(choice.note.is_some());
        assert_eq!(
            backend_for(Pref::Auto, true, true).backend,
            RendererBackend::Cpu
        );
        // Elsewhere it keeps its meaning: wgpu's fallback adapter.
        assert_eq!(
            backend_for(Pref::Auto, true, false).backend,
            RendererBackend::Gpu
        );
    }

    #[test]
    fn gpu_info_reports_the_cpu_renderer_and_survives_a_missing_adapter() {
        let cfg = kettle_config::Config::default();
        let missing = || Err(anyhow::anyhow!("no adapter"));
        let cpu =
            crate::gpu::renderer_info_text(&cfg, backend_for(Pref::Auto, false, true), missing())
                .expect("the CPU renderer needs no adapter");
        assert!(
            cpu.starts_with("Renderer:       CPU (renderer = auto)"),
            "{cpu}"
        );
        assert!(
            cpu.contains("GPU adapter:    none usable (no adapter)"),
            "{cpu}"
        );
        let with_adapter = crate::gpu::renderer_info_text(
            &cfg,
            backend_for(Pref::Auto, false, true),
            Ok("Backend:        Vulkan".into()),
        )
        .expect("text");
        assert!(with_adapter.ends_with("Backend:        Vulkan"));
        let gpu = kettle_config::Config {
            renderer: Pref::Gpu,
            ..cfg.clone()
        };
        assert!(
            crate::gpu::renderer_info_text(&gpu, backend_for(Pref::Gpu, false, true), missing())
                .is_err(),
            "the GPU renderer still fails without an adapter"
        );
        let mac = crate::gpu::renderer_info_text(
            &cfg,
            backend_for(Pref::Cpu, false, false),
            Ok(String::new()),
        )
        .expect("text");
        assert!(mac.contains("Renderer note:  renderer = cpu is available on Linux only"));
    }

    #[test]
    fn other_platforms_draw_on_the_gpu_and_note_a_cpu_request() {
        assert_eq!(
            backend_for(Pref::Auto, false, false),
            BackendChoice {
                backend: RendererBackend::Gpu,
                note: None,
            }
        );
        let cpu = backend_for(Pref::Cpu, false, false);
        assert_eq!(cpu.backend, RendererBackend::Gpu);
        assert!(cpu.note.is_some_and(|note| note.contains("Linux only")));
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
