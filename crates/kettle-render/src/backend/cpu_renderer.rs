//! A CPU-backend renderer's frame: the CPU counterpart of the encoder,
//! acquire and present tail of `Renderer::render_frame_with_status_and_pre_present`.
//! Its window path (damage, presenting) runs only on Linux, where CPU windows
//! exist; other platforms use the offscreen path.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::time::Instant;

use anyhow::{Result, anyhow};
use kettle_config::Config;

use super::cpu::{CpuBackend, CpuPass, CpuPresentOutcome, CpuSurface, FrameDraw, HeldFrameFlush};
use super::cpu_damage::{DamageStats, DamageTracker, TileHashes, disjoint};
use super::{RenderDevice, ScenePass};
use crate::cpu_raster::{CpuFramebuffer, CpuTarget, PixelRect};
use crate::cursor_patch::SceneFacts;
use crate::{FrameOutcome, Renderer, ScreenshotRequest, StartupFonts};
use kettle_core::{GraphicsBudget, GraphicsReservation};

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
        W: raw_window_handle::HasWindowHandle
            + raw_window_handle::HasDisplayHandle
            + Send
            + Sync
            + 'static,
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
    /// transparent clear (inside `clips` when given), then hand a pending
    /// screenshot its pixels. The caller has begun the raster's frame;
    /// `plan` is false when a signing pass of this frame already planned it.
    #[allow(clippy::too_many_arguments)]
    fn draw_cpu_scene(
        &self,
        cpu_raster: &mut dyn super::cpu::CpuRaster,
        draws: &mut Vec<super::cpu::CpuDrawRecord>,
        mut target: CpuTarget<'_>,
        size: [u32; 2],
        cfg: &Config,
        cursor_on: bool,
        screenshot: &mut Option<ScreenshotRequest>,
        clips: Option<&[PixelRect]>,
        plan: bool,
    ) -> Result<()> {
        draws.clear();
        {
            let mut pass =
                CpuPass::clipped(target.reborrow(), size, cpu_raster, draws, clips, plan);
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
            self.dispatch_cpu_screenshot(screenshot, size, target.pixels(), target.width());
        }
        Ok(())
    }

    /// Run the scene pass into `tiles` (`cpu_damage.rs`): the frame's damage
    /// signature, with no pixel drawn.
    fn sign_cpu_scene(
        &self,
        cpu_raster: &mut dyn super::cpu::CpuRaster,
        tiles: &mut TileHashes,
        size: [u32; 2],
        cfg: &Config,
        cursor_on: bool,
    ) -> Result<()> {
        let mut pass = CpuPass::signing(tiles, size, cpu_raster);
        self.encode_scene_pass(ScenePass::Cpu(&mut pass), size, cfg, cursor_on, None)
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
        if cpu.surface.is_some() {
            cpu.note_present_thread();
        }
        let CpuBackend {
            scene,
            scene_reservation,
            presented,
            converted,
            raster,
            surface,
            held,
            paced,
            draws,
            damage,
            ..
        } = cpu;
        let windowed = surface.is_some();
        // Whether the presenter holds a drawn frame for a later commit.
        let mut still_held = false;
        let outcome = match surface.as_mut() {
            Some(surface) => {
                // A window's frame is drawn where the compositor reads it, and
                // the scene's pixels are the presenter's format as they are:
                // `wl_shm` ARGB8888 is premultiplied, and a GPU window's
                // `PreMultiplied` surface is handed exactly these bytes
                // (sRGB-encoded colour premultiplied in linear light; half-
                // alpha white is 188, not 128). Converting to an
                // encoded-space premultiply would move a translucent CPU
                // window away from the GPU's by up to 44 codes, measured live
                // (see `a_translucent_cpu_window_hands_over_what_a_
                // premultiplied_gpu_surface_gets`). The surface offers no
                // `PostMultiplied` mode, so no straight-alpha pass runs.
                debug_assert!(!needs_presentation, "a CPU window presents premultiplied");
                *converted = false;
                let mut frame = WindowFrame {
                    renderer: &*self,
                    raster: raster.as_mut(),
                    draws: &mut *draws,
                    damage: &mut *damage,
                    cfg,
                    target_size,
                    cursor_on,
                    screenshot: &mut screenshot,
                    signed: false,
                    stats: DamageStats::default(),
                    drawn: Ok(()),
                };
                let mut pre_present = Some(pre_present);
                let outcome = surface.present_frame(&mut frame, &mut || {
                    if let Some(pre_present) = pre_present.take() {
                        pre_present();
                    }
                });
                let WindowFrame {
                    signed,
                    stats,
                    drawn,
                    ..
                } = frame;
                still_held = surface.has_deferred();
                match outcome {
                    // The buffer now holds this frame: the next one is
                    // compared with it.
                    CpuPresentOutcome::Presented
                    | CpuPresentOutcome::Deferred
                    | CpuPresentOutcome::Unchanged => {
                        if signed {
                            damage.commit();
                        } else {
                            damage.reset();
                        }
                        log::trace!(
                            target: "kettle_render::cpu_damage",
                            "CPU frame {width}x{height}: {} damage rects, repainted {} of {} px{}, \
                             sign {:?}, raster {:?}, {} cached draws",
                            stats.damage.len(),
                            stats.repainted.iter().map(|r| u64::from(r.w) * u64::from(r.h)).sum::<u64>(),
                            u64::from(width) * u64::from(height),
                            if stats.full { " (full)" } else { "" },
                            stats.sign,
                            stats.raster,
                            stats.planned,
                        );
                        damage.last = stats;
                    }
                    CpuPresentOutcome::Lost => damage.reset(),
                    // Not drawn into any buffer: the last drawn frame stays
                    // the reference.
                    _ => {}
                }
                if let Some(screenshot) = screenshot.take() {
                    if outcome == CpuPresentOutcome::Paced && self.pending_screenshot.is_none() {
                        // Not drawn yet: the frame that is drawn captures it.
                        self.pending_screenshot = Some(screenshot);
                    } else {
                        Self::complete_screenshot_error(
                            screenshot,
                            "the window had no buffer to draw the screenshot into".to_string(),
                        );
                    }
                }
                drawn?;
                outcome
            }
            None => {
                if !size_offscreen_frame(
                    scene,
                    scene_reservation,
                    &self.graphics_budget,
                    width,
                    height,
                ) {
                    if let Some(screenshot) = screenshot {
                        Self::complete_screenshot_error(
                            screenshot,
                            "CPU frame too large for the graphics budget".to_string(),
                        );
                    }
                    if need_prepare {
                        self.atlas.trim();
                    }
                    return Err(anyhow!(
                        "graphics budget exhausted while creating a {width}x{height} CPU frame"
                    ));
                }
                let Some(scene) = scene.as_mut() else {
                    return Err(anyhow!("a {width}x{height} CPU frame is too large"));
                };
                // The clear is the GPU pass's `LoadOp::Clear(TRANSPARENT)`,
                // which sizing the frame did.
                raster.begin_frame();
                self.draw_cpu_scene(
                    raster.as_mut(),
                    draws,
                    scene.target(),
                    target_size,
                    cfg,
                    cursor_on,
                    &mut screenshot,
                    None,
                    true,
                )?;
                *converted = false;
                if needs_presentation {
                    // The budget is charged before the conversion buffer is
                    // allocated, and both go when a frame no longer needs them.
                    if !self
                        .presentation
                        .ensure_target(super::DeviceRef::Cpu, width, height)
                    {
                        *presented = None;
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
                        *presented = None;
                        *presented = CpuFramebuffer::new(width, height);
                    }
                    let Some(presented) = presented.as_mut() else {
                        self.presentation.discard_target();
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
        *paced = outcome == CpuPresentOutcome::Paced;
        match outcome {
            CpuPresentOutcome::Presented => {
                // This frame replaced any held one on screen.
                *held = None;
                self.frames_presented += 1;
                self.last_scene = Some(scene_facts);
                self.commit_painted_cards();
                Ok(FrameOutcome::Presented)
            }
            CpuPresentOutcome::Deferred => {
                // Drawn, and on screen once the loop flushes it: the output
                // it shows is consumed, but what it painted is not on screen
                // until the commit (`flush_held_frame`).
                *held = Some(self.hold_painted(scene_facts));
                Ok(FrameOutcome::Presented)
            }
            CpuPresentOutcome::Unchanged => {
                // Nothing to commit: a presenter reports a frame unchanged
                // only when its pixels equal the last drawn frame's and
                // nothing drawn waits for a commit, so the screen already
                // shows this frame. What it painted is on screen with it:
                // its cards and bidi rows can differ from the last frame's
                // while its pixels do not (two tabs alike but for the pane
                // and card they show). A frame still held puts it on screen
                // with its own commit, so it takes this frame's instead.
                if still_held {
                    *held = Some(self.hold_painted(scene_facts));
                } else {
                    *held = None;
                    self.last_scene = Some(scene_facts);
                    self.commit_painted_cards();
                }
                Ok(FrameOutcome::Presented)
            }
            CpuPresentOutcome::Paced => Ok(FrameOutcome::Paced),
            CpuPresentOutcome::Occluded => {
                if !windowed {
                    // Test-only, as for a headless GPU renderer: the frame in
                    // the backend stands in for a presented one.
                    self.last_scene = Some(scene_facts);
                    self.commit_painted_cards();
                }
                Ok(FrameOutcome::Occluded)
            }
            CpuPresentOutcome::RetryLater => Ok(FrameOutcome::RetryLater),
            CpuPresentOutcome::NotDrawn => {
                // Dropping the frame dropped any held one with it.
                *held = None;
                Ok(FrameOutcome::RetryLater)
            }
            CpuPresentOutcome::Lost => {
                *held = None;
                Ok(FrameOutcome::SurfaceLost)
            }
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

    /// Give a CPU window's presenter the surface size and scale. The
    /// presenter may round the buffer up (an integer-scale Wayland buffer is
    /// a multiple of the scale); frames are drawn at the surface size and
    /// repeat their edge into the rest.
    pub(crate) fn resize_cpu_surface(&mut self) {
        let (width, height, scale) = (self.config.width, self.config.height, self.scale);
        if let Some(surface) = self.cpu_surface_mut() {
            surface.resize(width, height, f64::from(scale));
            let extent = surface.buffer_size();
            if extent != (width, height) {
                log::debug!(
                    "renderer: CPU buffer {}x{} for a {width}x{height} surface at scale {scale}",
                    extent.0,
                    extent.1
                );
            }
        }
    }

    /// The CPU presenter's name (`wl_shm (Wayland)`, ...), or `None` for a
    /// GPU or offscreen renderer.
    pub fn cpu_presenter_name(&self) -> Option<&'static str> {
        self.cpu_surface().map(CpuSurface::describe)
    }

    /// When to call [`Self::release_idle_buffers`] next: possibly now or
    /// in the past (a spare that is due), else a time the presenter keeps
    /// until the release runs, so a loop that recomputes it on each wake
    /// neither spins nor misses it. Always `None` on the GPU backend.
    pub fn idle_release_deadline(&self) -> Option<Instant> {
        self.cpu_surface()?.idle_release_deadline()
    }

    /// Release the window's spare shared-memory buffers, keeping the one on
    /// screen (none while occluded). Returns how many went; 0 on the GPU
    /// backend.
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
    /// The loop then calls [`Self::flush_held_frame`] each time it wakes.
    pub fn has_held_frame(&self) -> bool {
        self.cpu_surface().is_some_and(CpuSurface::has_deferred)
    }

    /// Commit a frame held for the compositor's frame callback, once the
    /// compositor is ready for it, running `pre_present` (winit's
    /// `pre_present_notify`) only for, and just before, the commit. What the
    /// frame painted ([`Self::painted_cards`]) is on screen from a
    /// [`HeldFrameFlush::Presented`] on. Call it on the event-loop thread.
    pub fn flush_held_frame(&mut self, pre_present: impl FnOnce()) -> HeldFrameFlush {
        let RenderDevice::Cpu(Some(cpu)) = &mut self.device else {
            return HeldFrameFlush::Nothing;
        };
        if cpu.surface.is_none() {
            return HeldFrameFlush::Nothing;
        }
        cpu.note_present_thread();
        let Some(surface) = cpu.surface.as_mut() else {
            return HeldFrameFlush::Nothing;
        };
        let mut pre_present = Some(pre_present);
        let flushed = surface.flush(&mut || {
            if let Some(pre_present) = pre_present.take() {
                pre_present();
            }
        });
        let held = match flushed {
            HeldFrameFlush::Presented | HeldFrameFlush::Failed => cpu.held.take(),
            HeldFrameFlush::Nothing | HeldFrameFlush::StillHeld => None,
        };
        if flushed == HeldFrameFlush::Presented
            && let Some(held) = held
        {
            self.commit_held_paint(held);
        }
        flushed
    }

    /// A frame came back [`FrameOutcome::Paced`] and the presenter can now
    /// take one: the window should redraw. Checks the events already read,
    /// so call it when the loop wakes.
    pub fn cpu_frame_ready(&mut self) -> bool {
        let RenderDevice::Cpu(Some(cpu)) = &mut self.device else {
            return false;
        };
        let paced = cpu.paced;
        paced
            && cpu
                .surface
                .as_mut()
                .is_some_and(|surface| !surface.frame_pending())
    }

    /// The latest time the loop must wake for this window's presenter: a
    /// held frame's flush deadline, or, while a paced frame waits, the time
    /// the compositor's frame callback stops holding frames back. Arm
    /// `ControlFlow::WaitUntil` with it (or an earlier time). Under
    /// `ControlFlow::Wait`, winit 0.30's Wayland loop does not run
    /// `about_to_wait` for a wake that brought only the presenter's frame
    /// callback; with any `WaitUntil` armed it does, so the held or paced
    /// frame goes out when the callback arrives. `None` on the GPU backend.
    pub fn cpu_wake_deadline(&self) -> Option<Instant> {
        let RenderDevice::Cpu(Some(cpu)) = &self.device else {
            return None;
        };
        let surface = cpu.surface.as_deref()?;
        let held = surface.pending_deadline();
        let paced = cpu.paced.then(|| surface.frame_deadline()).flatten();
        match (held, paced) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
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

    /// The last frame's scene repainted whole by `raster` into a new
    /// buffer: what a damage-tracked window buffer must hold after it.
    /// Returns the pixels and their size.
    #[cfg(test)]
    pub(crate) fn cpu_full_repaint_for_tests(
        &self,
        raster: &mut dyn super::cpu::CpuRaster,
        cfg: &Config,
        cursor_on: bool,
    ) -> Option<(Vec<u32>, u32, u32)> {
        let size = [self.config.width.max(1), self.config.height.max(1)];
        let mut frame = CpuFramebuffer::new(size[0], size[1])?;
        raster.begin_frame();
        let mut draws = Vec::new();
        {
            let mut pass = CpuPass::new(frame.target(), size, raster, &mut draws);
            self.encode_scene_pass(ScenePass::Cpu(&mut pass), size, cfg, cursor_on, None)
                .ok()?;
        }
        Some((frame.pixels().to_vec(), size[0], size[1]))
    }

    /// Make every frame of this CPU window a full repaint, as
    /// `KETTLE_CPU_FULL_REPAINT=1` does.
    #[cfg(test)]
    pub(crate) fn set_cpu_full_repaint_for_tests(&mut self, full: bool) {
        if let RenderDevice::Cpu(Some(cpu)) = &mut self.device {
            cpu.damage.force_full = full;
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

/// A window frame, drawn into the buffer the presenter hands out.
struct WindowFrame<'a> {
    renderer: &'a Renderer,
    raster: &'a mut dyn super::cpu::CpuRaster,
    draws: &'a mut Vec<super::cpu::CpuDrawRecord>,
    damage: &'a mut DamageTracker,
    cfg: &'a Config,
    target_size: [u32; 2],
    cursor_on: bool,
    screenshot: &'a mut Option<ScreenshotRequest>,
    /// The frame's damage came from its signature: committing it makes the
    /// signature the next frame's reference.
    signed: bool,
    stats: DamageStats,
    drawn: Result<()>,
}

impl FrameDraw for WindowFrame<'_> {
    fn begin(&mut self, buffer_w: u32, buffer_h: u32) -> Vec<PixelRect> {
        // The frame starts here, with a buffer to draw into: a frame that is
        // paced, or finds every buffer busy, must not age the rasterizer's
        // caches (a wallpaper drawn every frame would lose its cache entry
        // to the attempts in between).
        self.raster.begin_frame();
        // Damage: what changed since the frame last drawn into a buffer, from
        // this frame's draw list. A buffer the presenter rounded past the
        // scene repeats the scene's edge, which the signature does not see,
        // so it repaints in full.
        let [width, height] = self.target_size;
        let whole = PixelRect::new(0, 0, buffer_w, buffer_h);
        let signing = Instant::now();
        self.signed = !self.damage.force_full
            && (buffer_w, buffer_h) == (width, height)
            && self
                .renderer
                .sign_cpu_scene(
                    self.raster,
                    self.damage.begin(width, height),
                    self.target_size,
                    self.cfg,
                    self.cursor_on,
                )
                .is_ok();
        let frame_damage = match (self.signed, self.screenshot.is_none()) {
            // A screenshot is taken from a whole frame drawn now.
            (true, true) => self.damage.damage().unwrap_or_else(|| vec![whole]),
            _ => vec![whole],
        };
        self.stats = DamageStats {
            damage: frame_damage.clone(),
            planned: if self.signed {
                self.damage.next().planned()
            } else {
                0
            },
            sign: signing.elapsed(),
            ..DamageStats::default()
        };
        frame_damage
    }

    fn draw(
        &mut self,
        pixels: &mut [u32],
        buffer_w: u32,
        buffer_h: u32,
        repaint: &[PixelRect],
    ) -> bool {
        let drawing = Instant::now();
        let [width, height] = self.target_size;
        let whole = PixelRect::new(0, 0, buffer_w, buffer_h);
        let full = [buffer_w, buffer_h] != self.target_size
            || repaint.iter().any(|r| r.intersect(whole) == Some(whole));
        let clips = if full { vec![whole] } else { disjoint(repaint) };
        // The repainted pixels' contents are undefined; the clear is the GPU
        // pass's `LoadOp::Clear(TRANSPARENT)`.
        clear_rects(pixels, buffer_w, &clips);
        self.stats.full = full;
        self.stats.repainted.clone_from(&clips);
        let Some(mut target) = CpuTarget::from_pixels(pixels, buffer_w, buffer_h) else {
            self.drawn = Err(anyhow!("a {buffer_w}x{buffer_h} CPU buffer is malformed"));
            return false;
        };
        // The scene is drawn 1:1 from the buffer's origin. A buffer that lags
        // a resize shows its top-left part.
        target.set_viewport(crate::cpu_raster::Viewport {
            x: 0.0,
            y: 0.0,
            w: width as f32,
            h: height as f32,
        });
        self.drawn = self.renderer.draw_cpu_scene(
            self.raster,
            self.draws,
            target,
            self.target_size,
            self.cfg,
            self.cursor_on,
            self.screenshot,
            (!full).then_some(&clips[..]),
            !self.signed,
        );
        if self.drawn.is_ok() {
            extend_edges(pixels, [buffer_w, buffer_h], self.target_size);
        }
        self.stats.raster = drawing.elapsed();
        self.drawn.is_ok()
    }
}

/// Size an offscreen frame to `width` x `height`, cleared to transparent
/// black. The graphics budget is charged before any allocation, the old
/// frame and its charge go before a new size is taken, and growth is
/// fallible ([`CpuFramebuffer::new`]), so a large resize fails cleanly
/// instead of aborting. `false` leaves no frame.
fn size_offscreen_frame(
    frame: &mut Option<CpuFramebuffer>,
    reservation: &mut Option<GraphicsReservation>,
    budget: &GraphicsBudget,
    width: u32,
    height: u32,
) -> bool {
    if let Some(current) = frame.as_mut()
        && reservation.is_some()
        && [current.width(), current.height()] == [width, height]
    {
        current.pixels_mut().fill(0);
        return true;
    }
    *frame = None;
    *reservation = None;
    let Some(bytes) = CpuFramebuffer::bytes_for(width, height) else {
        return false;
    };
    let Some(charge) = crate::present::reserve_scene_target(budget, bytes) else {
        return false;
    };
    let Some(new) = CpuFramebuffer::new(width, height) else {
        return false;
    };
    *frame = Some(new);
    *reservation = Some(charge);
    true
}

/// Clear `rects` of a `width`-pixel-wide buffer to transparent black.
fn clear_rects(pixels: &mut [u32], width: u32, rects: &[PixelRect]) {
    let width = width as usize;
    for r in rects {
        let (x0, x1) = (r.x as usize, (r.x + r.w) as usize);
        for y in r.y as usize..(r.y + r.h) as usize {
            if let Some(row) = pixels.get_mut(y * width + x0..y * width + x1) {
                row.fill(0);
            }
        }
    }
}

/// A buffer larger than the scene (an integer-scale Wayland buffer is
/// rounded up to a multiple of the scale) repeats the scene's last column
/// and row into the extra pixels, so its edge shows no seam.
fn extend_edges(pixels: &mut [u32], [buffer_w, buffer_h]: [u32; 2], [width, height]: [u32; 2]) {
    let (bw, bh) = (buffer_w as usize, buffer_h as usize);
    let (w, h) = ((width as usize).min(bw), (height as usize).min(bh));
    if w == 0 || h == 0 || pixels.len() < bw * bh {
        return;
    }
    if w < bw {
        for row in pixels[..h * bw].chunks_exact_mut(bw) {
            let edge = row[w - 1];
            row[w..].fill(edge);
        }
    }
    if h < bh {
        let (top, rest) = pixels.split_at_mut(h * bw);
        let last = &top[(h - 1) * bw..];
        for row in rest[..(bh - h) * bw].chunks_exact_mut(bw) {
            row.copy_from_slice(last);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_core::GraphicsLimits;

    const MIB: usize = 1024 * 1024;

    fn one_mib_budget() -> GraphicsBudget {
        GraphicsBudget::independent(GraphicsLimits {
            image_bytes: MIB,
            retained_bytes: MIB,
            process_gpu_bytes: MIB,
            ..GraphicsLimits::default()
        })
        .expect("valid limits")
    }

    #[test]
    fn an_offscreen_frame_is_charged_before_it_is_allocated() {
        let budget = one_mib_budget();
        let (mut frame, mut charge) = (None, None);
        // 400x400 is 640,000 bytes: it fits.
        assert!(size_offscreen_frame(
            &mut frame,
            &mut charge,
            &budget,
            400,
            400
        ));
        assert_eq!(frame.as_ref().map(CpuFramebuffer::width), Some(400));
        // 600x600 does not: nothing is allocated, and the old frame and its
        // charge are gone.
        assert!(!size_offscreen_frame(
            &mut frame,
            &mut charge,
            &budget,
            600,
            600
        ));
        assert!(frame.is_none() && charge.is_none());
        assert!(size_offscreen_frame(
            &mut frame,
            &mut charge,
            &budget,
            100,
            100
        ));
        // Only the 40,000-byte frame is charged now.
        let rest = budget.reserve_retained_gpu(MIB - 40_000);
        assert!(rest.is_some(), "a shrink gives its charge back");
        drop(rest);
        // Two sides at the texture limit are 1 GiB: refused before any
        // allocation, under any budget.
        let big = GraphicsBudget::default();
        assert!(!size_offscreen_frame(
            &mut frame,
            &mut charge,
            &big,
            crate::cpu_raster::MAX_SIDE,
            crate::cpu_raster::MAX_SIDE
        ));
        assert!(frame.is_none() && charge.is_none());
    }

    #[test]
    fn the_same_size_is_cleared_and_kept() {
        let budget = one_mib_budget();
        let (mut frame, mut charge) = (None, None);
        assert!(size_offscreen_frame(&mut frame, &mut charge, &budget, 8, 4));
        frame.as_mut().expect("a frame").pixels_mut().fill(7);
        assert!(size_offscreen_frame(&mut frame, &mut charge, &budget, 8, 4));
        assert!(frame.expect("kept").pixels().iter().all(|&p| p == 0));
    }

    #[test]
    fn a_huge_offscreen_resize_fails_the_frame_instead_of_allocating() {
        let cfg = kettle_config::Config::default();
        let mut renderer =
            Renderer::new_cpu_offscreen(64, 32, 1.0, &cfg, None).expect("CPU renderer");
        renderer.resize(crate::cpu_raster::MAX_SIDE, crate::cpu_raster::MAX_SIDE);
        let snap = crate::headless_tests::snapshot_of(8, 2, b"x");
        let result = renderer.render_frame_with_status_and_pre_present(
            &[crate::headless_tests::pane(&snap, 64, 32)],
            &crate::TabBar::hidden(),
            &cfg,
            &crate::headless_tests::focused(true),
            &crate::StatusBar::hidden(),
            || {},
        );
        assert!(result.is_err(), "a 1 GiB frame is refused");
        let cpu = renderer.cpu_backend_for_tests().expect("CPU backend");
        assert!(cpu.scene.is_none() && cpu.scene_reservation.is_none());
    }

    #[test]
    fn edges_repeat_into_a_rounded_buffer() {
        // A 2x2 scene in a 3x3 buffer.
        let mut pixels = vec![1, 2, 0, 3, 4, 0, 0, 0, 0];
        extend_edges(&mut pixels, [3, 3], [2, 2]);
        assert_eq!(pixels, [1, 2, 2, 3, 4, 4, 3, 4, 4]);
        // Equal sizes, or a buffer smaller than the scene, change nothing.
        let mut same = vec![5, 6, 7, 8];
        extend_edges(&mut same, [2, 2], [2, 2]);
        assert_eq!(same, [5, 6, 7, 8]);
        extend_edges(&mut same, [2, 2], [3, 3]);
        assert_eq!(same, [5, 6, 7, 8]);
    }
}
