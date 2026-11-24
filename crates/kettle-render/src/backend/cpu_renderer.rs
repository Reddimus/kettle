//! A CPU-backend renderer's frame: the CPU counterpart of the encoder,
//! acquire and present tail of `Renderer::render_frame_with_status_and_pre_present`.

use std::time::Instant;

use anyhow::{Result, anyhow};
use kettle_config::Config;

use super::cpu::{CpuBackend, CpuPass, CpuPresentOutcome, CpuSurface};
use super::{RenderDevice, ScenePass};
use crate::cpu_raster::{CpuFramebuffer, CpuTarget};
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

    /// A CPU-backend renderer for `window`: frames are drawn on the CPU
    /// straight into shared memory and presented with `wl_shm` on Wayland or
    /// MIT-SHM on X11. No wgpu instance, adapter or device is created, so no
    /// GPU driver is loaded. `display` is the event loop's display handle.
    ///
    /// Fails when the window system offers no shared-memory presentation;
    /// the caller then falls back to a GPU renderer.
    #[cfg(target_os = "linux")]
    pub fn new_cpu_window<W, D>(
        window: std::sync::Arc<W>,
        display: D,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
    ) -> Result<Self>
    where
        W: raw_window_handle::HasWindowHandle + Send + Sync + 'static,
        D: raw_window_handle::HasDisplayHandle + Send + Sync + 'static,
    {
        let presenter = crate::cpu_present::CpuPresenter::new(window, display)?;
        let mut alpha_modes = vec![wgpu::CompositeAlphaMode::Opaque];
        if CpuSurface::supports_translucency(&presenter) {
            // A premultiplied scene is shown as drawn, as a GPU surface's
            // `PreMultiplied` mode shows it.
            alpha_modes.push(wgpu::CompositeAlphaMode::PreMultiplied);
        }
        let renderer = Self::with_cpu(
            width,
            height,
            scale,
            cfg,
            fonts,
            alpha_modes,
            Some(Box::new(presenter)),
        )?;
        log::info!(
            "renderer: CPU, presenting with {}",
            renderer.cpu_presenter_name().unwrap_or("no presenter")
        );
        Ok(renderer)
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

    /// Draw the scene pass into `target`, which holds the GPU pass's
    /// transparent clear, then hand a pending screenshot its pixels.
    #[allow(clippy::too_many_arguments)]
    fn draw_cpu_scene(
        &self,
        cpu_raster: &mut dyn super::cpu::CpuRaster,
        draws: &mut Vec<super::cpu::CpuDrawRecord>,
        mut target: CpuTarget<'_>,
        pixels_for_capture: impl FnOnce(&mut CpuTarget<'_>) -> Vec<u32>,
        size: [u32; 2],
        cfg: &Config,
        cursor_on: bool,
        screenshot: &mut Option<ScreenshotRequest>,
    ) -> Result<()> {
        cpu_raster.begin_frame();
        draws.clear();
        {
            let mut pass = CpuPass::new(target.reborrow(), size, cpu_raster, draws);
            if let Err(error) =
                self.encode_scene_pass(ScenePass::Cpu(&mut pass), size, cfg, cursor_on, None)
            {
                if let Some(screenshot) = screenshot.take() {
                    Self::complete_screenshot_error(
                        screenshot,
                        format!("screenshot render failed: {error}"),
                    );
                }
                return Err(error);
            }
        }
        // A capture holds the scene as drawn, before presentation, as the
        // GPU's offscreen capture pass does.
        if let Some(screenshot) = screenshot.take() {
            let pixels = pixels_for_capture(&mut target);
            self.dispatch_cpu_screenshot(screenshot, size, &pixels);
        }
        Ok(())
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
            target_size,
            cursor_on,
            needs_presentation,
            need_prepare,
            scene_facts,
            mut screenshot,
        } = request;
        let [width, height] = target_size;
        let CpuBackend {
            scene,
            presented,
            converted,
            raster,
            surface,
            draws,
        } = cpu;
        let windowed = surface.is_some();
        let outcome = match surface.as_mut() {
            Some(surface) => {
                // A window's frame is drawn where the compositor reads it. Its
                // surface offers no `PostMultiplied` mode, so the scene is
                // never converted.
                debug_assert!(!needs_presentation, "a CPU window presents premultiplied");
                *converted = false;
                let mut drawn = Ok(());
                let mut pre_present = Some(pre_present);
                let outcome = surface.present_frame(
                    &mut |pixels, buffer_w, buffer_h| {
                        // The scene is drawn 1:1 from the buffer's origin. A
                        // buffer that lags a resize shows its top-left part.
                        pixels.fill(0);
                        let Some(mut target) = CpuTarget::from_pixels(pixels, buffer_w, buffer_h)
                        else {
                            drawn = Err(anyhow!("a {buffer_w}x{buffer_h} CPU buffer is malformed"));
                            return false;
                        };
                        target.set_viewport(crate::cpu_raster::Viewport {
                            x: 0.0,
                            y: 0.0,
                            w: width as f32,
                            h: height as f32,
                        });
                        drawn = self.draw_cpu_scene(
                            raster.as_mut(),
                            draws,
                            target,
                            |target| capture_pixels(target, buffer_w, buffer_h, target_size),
                            target_size,
                            cfg,
                            cursor_on,
                            &mut screenshot,
                        );
                        drawn.is_ok()
                    },
                    &mut || {
                        if let Some(pre_present) = pre_present.take() {
                            pre_present();
                        }
                    },
                );
                if let Some(screenshot) = screenshot.take() {
                    Self::complete_screenshot_error(
                        screenshot,
                        "the window had no buffer to draw the screenshot into".to_string(),
                    );
                }
                drawn?;
                outcome
            }
            None => {
                // `resize` also clears.
                if !scene
                    .as_mut()
                    .is_some_and(|scene| scene.resize(width, height))
                {
                    *scene = CpuFramebuffer::new(width, height);
                }
                let Some(scene) = scene.as_mut() else {
                    if let Some(screenshot) = screenshot {
                        Self::complete_screenshot_error(
                            screenshot,
                            "CPU frame size overflow".to_string(),
                        );
                    }
                    return Err(anyhow!("a {width}x{height} CPU frame is too large"));
                };
                // The clear is the GPU pass's `LoadOp::Clear(TRANSPARENT)`.
                self.draw_cpu_scene(
                    raster.as_mut(),
                    draws,
                    scene.target(),
                    |target| target.pixels().to_vec(),
                    target_size,
                    cfg,
                    cursor_on,
                    &mut screenshot,
                )?;
                *converted = false;
                if needs_presentation {
                    if !self
                        .presentation
                        .ensure_target(super::DeviceRef::Cpu, width, height)
                    {
                        if need_prepare {
                            self.atlas.trim();
                        }
                        return Err(anyhow!(
                            "graphics budget exhausted while creating the presentation target"
                        ));
                    }
                    if !presented
                        .as_mut()
                        .is_some_and(|presented| presented.resize(width, height))
                    {
                        *presented = CpuFramebuffer::new(width, height);
                    }
                    let Some(presented) = presented.as_mut() else {
                        return Err(anyhow!("a {width}x{height} CPU frame is too large"));
                    };
                    raster.present(scene, presented);
                    *converted = true;
                } else {
                    *presented = None;
                }
                // Offscreen: the frame stays in the backend, as a headless GPU
                // renderer's capture is its whole frame.
                CpuPresentOutcome::Occluded
            }
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
                if !windowed {
                    // Test-only, as for a headless GPU renderer: the frame in
                    // the backend stands in for a presented one.
                    self.last_scene = Some(scene_facts);
                    self.commit_painted_cards();
                }
                Ok(FrameOutcome::Occluded)
            }
            CpuPresentOutcome::RetryLater | CpuPresentOutcome::NotDrawn => {
                Ok(FrameOutcome::RetryLater)
            }
            CpuPresentOutcome::Lost => Ok(FrameOutcome::SurfaceLost),
        }
    }

    fn cpu_surface(&self) -> Option<&dyn CpuSurface> {
        match &self.device {
            RenderDevice::Cpu(Some(cpu)) => cpu.surface.as_deref(),
            _ => None,
        }
    }

    fn cpu_surface_mut(&mut self) -> Option<&mut (dyn CpuSurface + 'static)> {
        match &mut self.device {
            RenderDevice::Cpu(Some(cpu)) => cpu.surface.as_deref_mut(),
            _ => None,
        }
    }

    /// The CPU presenter's name (`wl_shm (Wayland)`, ...), or `None` for a
    /// GPU or offscreen renderer.
    pub fn cpu_presenter_name(&self) -> Option<&'static str> {
        self.cpu_surface().map(CpuSurface::describe)
    }

    /// When the window's spare shared-memory buffer has been idle long
    /// enough to release. A window with nothing to draw wakes then and calls
    /// [`Self::release_idle_buffers`]. Always `None` on the GPU backend.
    pub fn idle_release_deadline(&self) -> Option<Instant> {
        self.cpu_surface()?.idle_release_deadline()
    }

    /// Release the window's spare shared-memory buffers, keeping the one on
    /// screen. Returns how many went; 0 on the GPU backend.
    pub fn release_idle_buffers(&mut self) -> usize {
        self.cpu_surface_mut()
            .map_or(0, CpuSurface::release_idle_buffers)
    }

    /// The window was hidden or shown again. A CPU window releases every
    /// buffer the compositor has let go while hidden, and repaints in full
    /// when shown. Nothing on the GPU backend.
    pub fn set_occluded(&mut self, occluded: bool) {
        if let Some(surface) = self.cpu_surface_mut() {
            surface.set_occluded(occluded);
        }
    }

    /// Whether a drawn frame is held for the compositor's frame callback.
    /// The caller then calls [`Self::flush_held_frame`] when its loop wakes.
    pub fn has_held_frame(&self) -> bool {
        self.cpu_surface().is_some_and(CpuSurface::has_deferred)
    }

    /// Commit a frame held for the compositor's frame callback, once the
    /// compositor is ready for it, running `pre_present` (winit's
    /// `pre_present_notify`) just before the commit. Returns whether a frame
    /// was committed.
    pub fn flush_held_frame(&mut self, pre_present: impl FnOnce()) -> bool {
        let mut pre_present = Some(pre_present);
        self.cpu_surface_mut().is_some_and(|surface| {
            surface.flush(&mut || {
                if let Some(pre_present) = pre_present.take() {
                    pre_present();
                }
            })
        })
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

    /// A CPU renderer that presents to `surface`, for tests of the window
    /// path without a window system.
    #[cfg(test)]
    pub(crate) fn cpu_window_for_tests(
        cfg: &Config,
        width: u32,
        height: u32,
        surface: Box<dyn CpuSurface>,
    ) -> Result<Self> {
        let mut alpha_modes = vec![wgpu::CompositeAlphaMode::Opaque];
        if surface.supports_translucency() {
            alpha_modes.push(wgpu::CompositeAlphaMode::PreMultiplied);
        }
        Self::with_cpu(width, height, 1.0, cfg, None, alpha_modes, Some(surface))
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

    /// The frame last presented by an offscreen CPU renderer.
    #[cfg(test)]
    pub(crate) fn cpu_presented_for_tests(&self) -> Option<&CpuFramebuffer> {
        let cpu = self.cpu_backend_for_tests()?;
        if cpu.converted {
            cpu.presented.as_ref()
        } else {
            cpu.scene.as_ref()
        }
    }
}

/// A screenshot's `size` pixels from a window buffer of `buffer_w` x
/// `buffer_h`: the same pixels when the sizes agree, else the overlap, padded
/// with transparent black.
fn capture_pixels(
    target: &mut CpuTarget<'_>,
    buffer_w: u32,
    buffer_h: u32,
    [width, height]: [u32; 2],
) -> Vec<u32> {
    if [buffer_w, buffer_h] == [width, height] {
        return target.pixels().to_vec();
    }
    let mut out = vec![0u32; width as usize * height as usize];
    let (w, h) = (width.min(buffer_w) as usize, height.min(buffer_h) as usize);
    for y in 0..h {
        let src = &target.pixels()[y * buffer_w as usize..][..w];
        out[y * width as usize..][..w].copy_from_slice(src);
    }
    out
}
