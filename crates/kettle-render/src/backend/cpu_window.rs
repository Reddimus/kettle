//! The CPU backend's window presenter on Linux: `cpu_present`'s wl_shm
//! (Wayland) or softbuffer (X11) presenter behind [`CpuSurface`].

use std::time::Instant;

use super::cpu::{CpuPresentOutcome, CpuSurface, HeldFrameFlush};
use crate::cpu_present::{
    CpuPresenter, FrameError, PixelFormat, PresentOutcome, PresenterKind, Rect,
};

impl CpuSurface for CpuPresenter {
    fn resize(&mut self, width: u32, height: u32, scale: f64) {
        CpuPresenter::resize(self, width, height, scale);
    }

    fn buffer_size(&self) -> (u32, u32) {
        self.size()
    }

    fn supports_translucency(&self) -> bool {
        CpuPresenter::supports_translucency(self)
    }

    fn set_translucent(&mut self, translucent: bool) -> bool {
        let wanted = if translucent {
            PixelFormat::Premultiplied
        } else {
            PixelFormat::Opaque
        };
        self.set_format(wanted) == PixelFormat::Premultiplied
    }

    fn validate_startup(&mut self, pixel: u32) -> anyhow::Result<()> {
        validate_initial_cpu_frame(self, pixel)
    }

    fn present_frame(
        &mut self,
        draw: &mut dyn FnMut(&mut [u32], u32, u32) -> bool,
        pre_commit: &mut dyn FnMut(),
    ) -> CpuPresentOutcome {
        // A frame drawn now would only be held until the compositor's frame
        // callback, then drawn over by the next one: draw the latest state
        // once the callback has come instead.
        if self.frame_pending() {
            return CpuPresentOutcome::Paced;
        }
        let mut frame = match self.buffer_mut() {
            Ok(frame) => frame,
            Err(FrameError::Busy) => return CpuPresentOutcome::RetryLater,
            Err(error) => {
                log::warn!("CPU present: no buffer to draw into: {error}");
                return CpuPresentOutcome::Lost;
            }
        };
        let (width, height) = (frame.width(), frame.height());
        // Every frame damages the whole window until the renderer tracks
        // damage. The buffer's age still decides what must be repainted, and
        // with whole-window damage that is the whole buffer.
        let damage = [Rect::full(width, height)];
        if !frame.repaint_region(&damage).is_empty() && !draw(frame.pixels_mut(), width, height) {
            return CpuPresentOutcome::NotDrawn;
        }
        match frame.present_with(&damage, pre_commit) {
            Ok(PresentOutcome::Presented) => CpuPresentOutcome::Presented,
            Ok(PresentOutcome::Deferred) => CpuPresentOutcome::Deferred,
            Ok(PresentOutcome::Unchanged) => CpuPresentOutcome::Unchanged,
            Err(error) => {
                log::warn!("CPU present failed: {error:#}");
                CpuPresentOutcome::Lost
            }
        }
    }

    fn flush(&mut self, pre_commit: &mut dyn FnMut()) -> HeldFrameFlush {
        if !self.has_deferred() {
            return HeldFrameFlush::Nothing;
        }
        match self.flush_with(pre_commit) {
            Ok(PresentOutcome::Presented) => HeldFrameFlush::Presented,
            Ok(PresentOutcome::Deferred) => HeldFrameFlush::StillHeld,
            // The held frame went with an abandoned draw: nothing to commit.
            Ok(PresentOutcome::Unchanged) => HeldFrameFlush::Nothing,
            Err(error) => {
                log::warn!("CPU present: flushing a held frame failed: {error:#}");
                HeldFrameFlush::Failed
            }
        }
    }

    fn has_deferred(&self) -> bool {
        CpuPresenter::has_deferred(self)
    }

    fn pending_deadline(&self) -> Option<Instant> {
        CpuPresenter::pending_deadline(self)
    }

    fn frame_pending(&mut self) -> bool {
        CpuPresenter::frame_pending(self)
    }

    fn frame_deadline(&self) -> Option<Instant> {
        CpuPresenter::frame_deadline(self)
    }

    fn release_idle_buffers(&mut self) -> usize {
        CpuPresenter::release_idle_buffers(self)
    }

    fn idle_release_deadline(&self) -> Option<Instant> {
        CpuPresenter::idle_release_deadline(self)
    }

    fn set_occluded(&mut self, occluded: bool) {
        CpuPresenter::set_occluded(self, occluded);
    }

    fn describe(&self) -> &'static str {
        match self.kind() {
            PresenterKind::WaylandShm => "wl_shm (Wayland)",
            PresenterKind::SoftbufferWayland => "softbuffer (Wayland)",
            PresenterKind::SoftbufferX11 => "softbuffer (X11)",
        }
    }
}

pub(super) fn validate_initial_cpu_frame(
    surface: &mut dyn CpuSurface,
    pixel: u32,
) -> anyhow::Result<()> {
    let outcome = surface.present_frame(
        &mut |pixels, _, _| {
            pixels.fill(pixel);
            true
        },
        &mut || {},
    );
    if outcome != CpuPresentOutcome::Presented {
        anyhow::bail!("initial CPU buffer acquire/present failed ({outcome:?})");
    }
    Ok(())
}
