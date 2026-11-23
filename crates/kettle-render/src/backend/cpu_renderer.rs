//! A CPU-backend renderer's frame: the CPU counterpart of the encoder,
//! acquire and present tail of `Renderer::render_frame_with_status_and_pre_present`.

use anyhow::{Result, anyhow};
use kettle_config::Config;

#[cfg(test)]
use super::cpu::CpuFrame;
use super::cpu::{CpuBackend, CpuPass, CpuPresentOutcome};
use super::{DeviceRef, RenderDevice, ScenePass};
use crate::cursor_patch::SceneFacts;
use crate::{FrameOutcome, Renderer, ScreenshotRequest, StartupFonts};

/// What the frame built for its submission.
pub(crate) struct CpuFrameRequest {
    pub(crate) target_size: [u32; 2],
    pub(crate) cursor_on: bool,
    /// Whether the scene needs the straight-alpha presentation pass.
    pub(crate) needs_presentation: bool,
    /// Whether this frame prepared text, so the atlas may be trimmed.
    pub(crate) need_prepare: bool,
    pub(crate) scene_facts: SceneFacts,
    pub(crate) screenshot: Option<ScreenshotRequest>,
}

impl Renderer {
    /// An offscreen CPU-backend renderer: it draws each frame into its own
    /// memory and creates no wgpu instance, adapter, device or surface.
    /// Screenshot requests capture that frame.
    ///
    /// Experimental: until the software rasterizer lands, a CPU frame is its
    /// clear, and every other draw is only recorded. No configuration selects
    /// this backend yet; windows render on the GPU.
    pub fn new_cpu_offscreen(
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
    ) -> Result<Self> {
        Self::with_cpu(
            width,
            height,
            scale,
            cfg,
            fonts,
            vec![wgpu::CompositeAlphaMode::Opaque],
            None,
        )
    }

    /// Draw and present the frame on the CPU.
    pub(crate) fn submit_cpu_frame<F>(
        &mut self,
        request: CpuFrameRequest,
        cfg: &Config,
        pre_present: F,
    ) -> Result<FrameOutcome>
    where
        F: FnOnce(),
    {
        // The pass borrows the renderer's layers while it draws into the
        // backend's frame, so the frame state is held apart until it ends.
        let Some(mut cpu) = (match &mut self.device {
            RenderDevice::Cpu(cpu) => cpu.take(),
            RenderDevice::Gpu(_) => None,
        }) else {
            if let Some(screenshot) = request.screenshot {
                Self::complete_screenshot_error(screenshot, "no CPU frame state".to_string());
            }
            return Err(anyhow!("the CPU renderer's frame state is missing"));
        };
        let result = self.draw_cpu_frame(&mut cpu, request, cfg, pre_present);
        if let RenderDevice::Cpu(slot) = &mut self.device {
            *slot = Some(cpu);
        }
        result
    }

    fn draw_cpu_frame<F>(
        &mut self,
        cpu: &mut CpuBackend,
        request: CpuFrameRequest,
        cfg: &Config,
        pre_present: F,
    ) -> Result<FrameOutcome>
    where
        F: FnOnce(),
    {
        let CpuFrameRequest {
            target_size: [width, height],
            cursor_on,
            needs_presentation,
            need_prepare,
            scene_facts,
            screenshot,
        } = request;
        if !cpu.scene.resize(width, height) {
            if let Some(screenshot) = screenshot {
                Self::complete_screenshot_error(screenshot, "CPU frame size overflow".to_string());
            }
            return Err(anyhow!("a {width}x{height} CPU frame overflows memory"));
        }
        // The GPU pass's `LoadOp::Clear(TRANSPARENT)`.
        cpu.scene.clear([0; 4]);
        cpu.draws.clear();
        let drawn = {
            let CpuBackend {
                scene,
                raster,
                draws,
                ..
            } = cpu;
            let mut pass = CpuPass::new(scene, raster.as_mut(), draws);
            self.encode_scene_pass(
                ScenePass::Cpu(&mut pass),
                [width, height],
                cfg,
                cursor_on,
                None,
            )
        };
        if let Err(error) = drawn {
            if let Some(screenshot) = screenshot {
                Self::complete_screenshot_error(
                    screenshot,
                    format!("screenshot render failed: {error}"),
                );
            }
            return Err(error);
        }
        // A capture holds the scene as drawn, before presentation, as the
        // GPU's offscreen capture pass does.
        if let Some(screenshot) = screenshot {
            self.dispatch_cpu_screenshot(screenshot, &cpu.scene);
        }
        cpu.converted = false;
        if needs_presentation {
            if !self
                .presentation
                .ensure_target(DeviceRef::Cpu, width, height)
            {
                if need_prepare {
                    self.atlas.trim();
                }
                return Err(anyhow!(
                    "graphics budget exhausted while creating the presentation target"
                ));
            }
            let CpuBackend {
                scene,
                presented,
                raster,
                ..
            } = cpu;
            raster.present(scene, presented);
            cpu.converted = true;
        }
        let outcome = match cpu.presenter.as_mut() {
            Some(presenter) => {
                let frame = if cpu.converted {
                    &cpu.presented
                } else {
                    &cpu.scene
                };
                pre_present();
                presenter.present(frame)
            }
            // Offscreen: the frame stays in the backend, as a headless GPU
            // renderer's capture is its whole frame.
            None => CpuPresentOutcome::Occluded,
        };
        if need_prepare {
            self.atlas.trim();
        }
        match outcome {
            CpuPresentOutcome::Presented => {
                self.frames_presented += 1;
                self.last_scene = Some(scene_facts);
                self.commit_painted_cards();
                Ok(FrameOutcome::Presented)
            }
            CpuPresentOutcome::Occluded => {
                if cpu.presenter.is_none() {
                    // Test-only, as for a headless GPU renderer: the frame in
                    // the backend stands in for a presented one.
                    self.last_scene = Some(scene_facts);
                    self.commit_painted_cards();
                }
                Ok(FrameOutcome::Occluded)
            }
            CpuPresentOutcome::RetryLater => Ok(FrameOutcome::RetryLater),
            CpuPresentOutcome::Lost => Ok(FrameOutcome::SurfaceLost),
        }
    }

    /// A headless renderer on `backend`, for tests that render one scene
    /// through both: on the GPU as [`Self::headless_for_tests_with`] builds
    /// it (`None` when the host has no adapter), on the CPU with no wgpu
    /// object at all. With `translucent_surface` the pretend surface also
    /// offers `PostMultiplied`, as on the GPU.
    #[cfg(test)]
    pub(crate) fn headless_for_tests_on(
        backend: super::RendererBackend,
        cfg: &Config,
        width: u32,
        height: u32,
        scale: f32,
        translucent_surface: bool,
    ) -> Result<Option<Self>> {
        match backend {
            super::RendererBackend::Gpu => {
                Self::headless_for_tests_with(cfg, width, height, scale, translucent_surface)
            }
            super::RendererBackend::Cpu => {
                let mut alpha_modes = vec![wgpu::CompositeAlphaMode::Opaque];
                if translucent_surface {
                    alpha_modes.push(wgpu::CompositeAlphaMode::PostMultiplied);
                }
                Self::with_cpu(width, height, scale, cfg, None, alpha_modes, None).map(Some)
            }
        }
    }

    /// Draw this CPU renderer's frames with `raster`, as the software
    /// rasterizer will by default. False on a GPU renderer.
    #[cfg(test)]
    pub(crate) fn set_cpu_raster_for_tests(
        &mut self,
        raster: Box<dyn super::cpu::CpuRaster>,
    ) -> bool {
        match &mut self.device {
            RenderDevice::Cpu(Some(cpu)) => {
                cpu.raster = raster;
                true
            }
            _ => false,
        }
    }

    /// The CPU backend's state, between frames.
    #[cfg(test)]
    pub(crate) fn cpu_backend_for_tests(&self) -> Option<&CpuBackend> {
        match &self.device {
            RenderDevice::Cpu(cpu) => cpu.as_deref(),
            RenderDevice::Gpu(_) => None,
        }
    }

    /// The frame last presented by a CPU renderer.
    #[cfg(test)]
    pub(crate) fn cpu_presented_for_tests(&self) -> Option<&CpuFrame> {
        let cpu = self.cpu_backend_for_tests()?;
        Some(if cpu.converted {
            &cpu.presented
        } else {
            &cpu.scene
        })
    }
}
