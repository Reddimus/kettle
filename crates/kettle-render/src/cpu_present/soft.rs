//! softbuffer: the X11 presenter, and the Wayland fallback.

use std::num::NonZeroU32;

use anyhow::{Result, anyhow};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use super::damage::{DamageHistory, Rect, normalize};
use super::{
    DisplayOwner, FrameError, PixelFormat, PresentOutcome, PresenterKind, PresenterStats,
    WindowOwner, buffer_extent, check_extent,
};

type Surface = softbuffer::Surface<DisplayOwner, WindowOwner>;

pub(super) struct SoftPresenter {
    surface: Surface,
    _context: softbuffer::Context<DisplayOwner>,
    history: DamageHistory,
    kind: PresenterKind,
    /// X11 only: the window has a depth-32 visual, so the alpha byte reaches
    /// the compositing manager.
    alpha: bool,
    width: u32,
    height: u32,
    format: PixelFormat,
    ages: SoftAges,
    commits: u64,
}

/// Corrects the buffer age softbuffer reports.
///
/// softbuffer 0.4's Wayland backend keeps a buffer's age when a resize
/// reallocates it, so after a resize the second buffer reports an age its
/// (reallocated) contents do not have. An age reaching back past the last
/// resize or format change is therefore treated as unknown, and so is a
/// buffer the renderer took and dropped without presenting.
#[derive(Debug, Default)]
pub(super) struct SoftAges {
    /// Frames presented since the last resize or format change.
    since_reset: u32,
    /// The next buffer's contents are unknown (a frame was dropped).
    unknown_next: bool,
    /// The next present damages the whole buffer.
    damage_full: bool,
}

impl SoftAges {
    pub(super) fn reset(&mut self) {
        self.since_reset = 0;
        self.damage_full = true;
    }

    pub(super) fn age(&self, reported: u8) -> u8 {
        if self.unknown_next || u32::from(reported) > self.since_reset {
            0
        } else {
            reported
        }
    }

    /// A frame was presented; returns whether it damages the whole buffer.
    pub(super) fn presented(&mut self) -> bool {
        self.since_reset = self.since_reset.saturating_add(1);
        self.unknown_next = false;
        std::mem::take(&mut self.damage_full)
    }

    pub(super) fn abandoned(&mut self) {
        self.unknown_next = true;
    }
}

impl SoftPresenter {
    pub(super) fn new(window: WindowOwner, display: DisplayOwner) -> Result<Self> {
        let raw_display = display
            .display_handle()
            .map_err(|e| anyhow!("display handle: {e}"))?
            .as_raw();
        let raw_window = window
            .window_handle()
            .map_err(|e| anyhow!("window handle: {e}"))?
            .as_raw();
        let kind = match raw_display {
            RawDisplayHandle::Wayland(_) => PresenterKind::SoftbufferWayland,
            _ => PresenterKind::SoftbufferX11,
        };
        let alpha =
            kind == PresenterKind::SoftbufferX11 && x11_window_depth(raw_window) == Some(32);
        let context =
            softbuffer::Context::new(display).map_err(|e| anyhow!("softbuffer context: {e}"))?;
        let surface =
            Surface::new(&context, window).map_err(|e| anyhow!("softbuffer surface: {e}"))?;
        let mut presenter = Self {
            surface,
            _context: context,
            history: DamageHistory::default(),
            kind,
            alpha,
            width: 0,
            height: 0,
            format: PixelFormat::Opaque,
            ages: SoftAges::default(),
            commits: 0,
        };
        presenter.resize(1, 1, 1.0);
        Ok(presenter)
    }

    pub(super) fn kind(&self) -> PresenterKind {
        self.kind
    }

    pub(super) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub(super) fn format(&self) -> PixelFormat {
        self.format
    }

    pub(super) fn supports_translucency(&self) -> bool {
        self.alpha
    }

    pub(super) fn resize(&mut self, width: u32, height: u32, scale: f64) {
        let (w, h) = buffer_extent(width, height, scale);
        if (w, h) == (self.width, self.height) || check_extent(w, h).is_err() {
            return;
        }
        let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else {
            return;
        };
        match self.surface.resize(nw, nh) {
            Ok(()) => {
                self.width = w;
                self.height = h;
                self.history.clear();
                self.ages.reset();
            }
            Err(err) => log::warn!("CPU present: softbuffer resize to {w}x{h} failed: {err}"),
        }
    }

    pub(super) fn set_format(&mut self, format: PixelFormat) -> PixelFormat {
        let format = if self.alpha {
            format
        } else {
            PixelFormat::Opaque
        };
        if format != self.format {
            self.format = format;
            self.history.clear();
            self.ages.reset();
        }
        format
    }

    pub(super) fn set_occluded(&mut self, _occluded: bool) {
        // softbuffer keeps its buffers; nothing to release.
    }

    pub(super) fn buffer_mut(&mut self) -> Result<SoftFrame<'_>, FrameError> {
        let (width, height) = (self.width, self.height);
        let buffer = self
            .surface
            .buffer_mut()
            .map_err(|e| FrameError::Backend(anyhow!("softbuffer: {e}")))?;
        let age = self.ages.age(buffer.age());
        Ok(SoftFrame {
            buffer: Some(buffer),
            history: &mut self.history,
            ages: &mut self.ages,
            commits: &mut self.commits,
            width,
            height,
            age,
            format: self.format,
        })
    }

    pub(super) fn stats(&self) -> PresenterStats {
        let buffers = match self.kind {
            // softbuffer keeps a front and a back wl_shm buffer once drawn.
            PresenterKind::SoftbufferWayland if self.commits > 0 => 2,
            _ if self.commits > 0 => 1,
            _ => 0,
        };
        PresenterStats {
            kind: self.kind,
            buffers,
            busy_buffers: 0,
            buffer_bytes: buffers * self.width as usize * self.height as usize * 4,
            commits: self.commits,
            deferred: 0,
            allocations: buffers as u64,
            frame_callback_pending: false,
        }
    }
}

pub(super) struct SoftFrame<'a> {
    /// `None` once presented.
    buffer: Option<softbuffer::Buffer<'a, DisplayOwner, WindowOwner>>,
    history: &'a mut DamageHistory,
    ages: &'a mut SoftAges,
    commits: &'a mut u64,
    width: u32,
    height: u32,
    age: u8,
    format: PixelFormat,
}

impl SoftFrame<'_> {
    pub(super) fn width(&self) -> u32 {
        self.width
    }

    pub(super) fn height(&self) -> u32 {
        self.height
    }

    pub(super) fn age(&self) -> u8 {
        self.age
    }

    pub(super) fn format(&self) -> PixelFormat {
        self.format
    }

    pub(super) fn repaint_region(&self, damage: &[Rect]) -> Vec<Rect> {
        let (w, h) = (self.width, self.height);
        self.history
            .repaint_region(self.age, &normalize(damage, w, h), w, h)
    }

    pub(super) fn pixels(&self) -> &[u32] {
        self.buffer.as_deref().unwrap_or(&[])
    }

    pub(super) fn pixels_mut(&mut self) -> &mut [u32] {
        self.buffer.as_deref_mut().unwrap_or(&mut [])
    }

    /// softbuffer swaps buffers on every present, so even an empty frame is
    /// presented: skipping it would leave softbuffer's ages one frame behind
    /// the damage history.
    pub(super) fn present(
        mut self,
        damage: &[Rect],
        pre_commit: impl FnOnce(),
    ) -> Result<PresentOutcome> {
        let Some(buffer) = self.buffer.take() else {
            return Ok(PresentOutcome::Unchanged);
        };
        let (w, h) = (self.width, self.height);
        let current = normalize(damage, w, h);
        self.history.push(current.clone());
        let full = self.ages.presented();
        let out = if full {
            vec![Rect::full(w, h)]
        } else {
            current
        };
        let rects: Vec<softbuffer::Rect> = out
            .iter()
            .filter_map(|r| {
                Some(softbuffer::Rect {
                    x: r.x,
                    y: r.y,
                    width: NonZeroU32::new(r.width)?,
                    height: NonZeroU32::new(r.height)?,
                })
            })
            .collect();
        pre_commit();
        buffer
            .present_with_damage(&rects)
            .map_err(|e| anyhow!("softbuffer present: {e}"))?;
        *self.commits += 1;
        Ok(if out.is_empty() {
            PresentOutcome::Unchanged
        } else {
            PresentOutcome::Presented
        })
    }
}

impl Drop for SoftFrame<'_> {
    fn drop(&mut self) {
        // Dropped unpresented: softbuffer keeps the buffer and its age, but
        // the renderer may have drawn into it, so its contents are unknown.
        if self.buffer.is_some() {
            self.ages.abandoned();
        }
    }
}

/// The depth of an X11 window's visual, asked on a short-lived connection
/// of its own (softbuffer keeps its depth private).
fn x11_window_depth(window: RawWindowHandle) -> Option<u8> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::ConnectionExt as _;
    let id = match window {
        RawWindowHandle::Xlib(h) => u32::try_from(h.window).ok()?,
        RawWindowHandle::Xcb(h) => h.window.get(),
        _ => return None,
    };
    let (conn, _) = x11rb::connect(None).ok()?;
    let depth = conn.get_geometry(id).ok()?.reply().ok()?.depth;
    let _ = conn.flush();
    Some(depth)
}
