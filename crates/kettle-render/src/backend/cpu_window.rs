//! The CPU backend's window presenter on Linux: `cpu_present`'s wl_shm
//! (Wayland) or softbuffer (X11) presenter behind [`CpuSurface`].

use std::time::Instant;

use super::cpu::{CpuPresentOutcome, CpuSurface};
use crate::cpu_present::{
    CpuPresenter, FrameError, PixelFormat, PresentOutcome, PresenterKind, Rect,
};

impl CpuSurface for CpuPresenter {
    fn resize(&mut self, width: u32, height: u32, scale: f64) {
        CpuPresenter::resize(self, width, height, scale);
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

    fn present_frame(
        &mut self,
        draw: &mut dyn FnMut(&mut [u32], u32, u32) -> bool,
        pre_commit: &mut dyn FnMut(),
    ) -> CpuPresentOutcome {
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
            Ok(
                PresentOutcome::Presented | PresentOutcome::Deferred | PresentOutcome::Unchanged,
            ) => CpuPresentOutcome::Presented,
            Err(error) => {
                log::warn!("CPU present failed: {error:#}");
                CpuPresentOutcome::Lost
            }
        }
    }

    fn flush(&mut self, pre_commit: &mut dyn FnMut()) -> bool {
        match self.flush_with(pre_commit) {
            Ok(outcome) => outcome == PresentOutcome::Presented,
            Err(error) => {
                log::warn!("CPU present: flushing a held frame failed: {error:#}");
                false
            }
        }
    }

    fn has_deferred(&self) -> bool {
        CpuPresenter::has_deferred(self)
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
            PresenterKind::SoftbufferX11 => "softbuffer (X11 SHM)",
        }
    }
}
