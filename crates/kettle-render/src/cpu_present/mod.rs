//! Presents a CPU-drawn framebuffer on a Linux window, with no GPU driver in
//! the process.
//!
//! The renderer draws packed `u32` pixels, `0xAARRGGBB` in native order (the
//! byte order `wl_shm`'s ARGB8888/XRGB8888 and a 24/32-bit X11 ZPixmap use
//! on little-endian hosts), straight into shared memory, then presents the
//! frame with the rectangles it changed:
//!
//! ```ignore
//! let mut frame = presenter.buffer_mut()?;
//! for rect in frame.repaint_region(&damage) {
//!     draw(frame.pixels_mut(), frame.width(), rect);
//! }
//! frame.present_with(&damage, || window.pre_present_notify())?;
//! ```
//!
//! # Backends
//!
//! - **Wayland: a Kettle-owned `wl_shm` presenter** ([`PresenterKind::WaylandShm`]).
//!   It binds its own `wl_shm` and event queue on winit's `wl_display` and
//!   wraps winit's `wl_surface`, as softbuffer does. Each buffer is its own
//!   sealed memfd, `wl_shm_pool` and `wl_buffer`, so releasing one returns its
//!   memory. It presents XRGB8888 for opaque windows and premultiplied
//!   ARGB8888 for translucent ones, damages with `wl_surface.damage_buffer`,
//!   tracks buffer age, keeps up to [`MAX_BUFFERS`] buffers so drawing never
//!   waits for a release, drops spares after [`IDLE_RELEASE`], and paces
//!   commits with `wl_surface.frame`.
//! - **X11: softbuffer** ([`PresenterKind::SoftbufferX11`]): MIT-SHM, falling
//!   back to `PutImage`. Translucent only when winit created the window with
//!   a depth-32 visual (`with_transparent(true)` where the server has one);
//!   otherwise opaque.
//! - **Wayland fallback: softbuffer** ([`PresenterKind::SoftbufferWayland`])
//!   when the `wl_shm` presenter cannot start. Opaque only (softbuffer 0.4
//!   presents XRGB8888 on Wayland), two buffers always, and it blocks in
//!   `buffer_mut` while the compositor holds the back buffer.
//!
//! # What winit keeps
//!
//! winit owns the `wl_surface` and its scale state: `wl_surface.set_buffer_scale`
//! for integer scales, and `wp_viewport.set_destination` when fractional
//! scaling is in use (a surface has at most one viewport). The presenter's
//! buffers are always physical pixels, as the wgpu surface's are, and
//! [`CpuPresenter::resize`] takes the size and scale winit reports. winit
//! also keeps the opaque region (`set_transparent`), which must agree with
//! [`CpuPresenter::set_format`], and its own frame callback
//! (`pre_present_notify`), which throttles `RedrawRequested`.
//!
//! # Frame pacing
//!
//! A commit asks for a `wl_surface.frame` callback. A frame presented before
//! that callback arrives is not committed: it is kept as a deferred frame,
//! its damage carried into the next commit, and the next `buffer_mut`
//! returns the same buffer. So a flood of output never queues frames or
//! allocates buffers faster than the compositor shows them. A deferred frame
//! goes out with the next [`CpuFrame::present`] or [`CpuPresenter::flush`]
//! once the callback has arrived. Callers redraw on winit's frame-paced
//! `RedrawRequested` and pass `pre_present_notify` to
//! [`CpuFrame::present_with`], which calls it only for a frame that is
//! committed; a caller that may stop redrawing with a frame deferred calls
//! `flush` when its loop wakes (`about_to_wait`): the callback's arrival
//! wakes winit's loop, since it reads the same socket. A resize, a format change or a return from
//! occlusion commits at once. A callback that has not come after
//! [`FRAME_CALLBACK_TIMEOUT`] stops holding frames back.

mod book;
mod damage;
mod shm;
mod soft;
#[cfg(test)]
mod tests;

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, anyhow};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle};

pub use book::{IDLE_RELEASE, MAX_BUFFERS};
pub use damage::{HISTORY_FRAMES, MAX_DAMAGE_RECTS, Rect};
pub use shm::FRAME_CALLBACK_TIMEOUT;

/// How the compositor reads the alpha byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// The alpha byte is ignored: XRGB8888, an opaque window.
    #[default]
    Opaque,
    /// Premultiplied alpha: ARGB8888 for a translucent window
    /// (`background-opacity < 1`).
    Premultiplied,
}

/// Which presenter a [`CpuPresenter`] runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenterKind {
    WaylandShm,
    SoftbufferWayland,
    SoftbufferX11,
}

/// Which Wayland presenter [`CpuPresenter::with_preference`] tries first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PresenterPreference {
    /// The `wl_shm` presenter, falling back to softbuffer.
    #[default]
    Auto,
    /// softbuffer even on Wayland (for comparison and tests).
    Softbuffer,
}

/// What a present did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentOutcome {
    /// Committed to the compositor (or put to the X11 window).
    Presented,
    /// Drawn but held until the compositor's frame callback; its damage goes
    /// out with the next commit.
    Deferred,
    /// Nothing changed and nothing was pending: no commit.
    Unchanged,
}

/// Why no frame could be handed out.
#[derive(Debug)]
pub enum FrameError {
    /// The compositor holds every buffer and [`MAX_BUFFERS`] exist. Draw
    /// again on the next frame callback; nothing is lost.
    Busy,
    /// The size is past what the presenter allocates.
    TooLarge { width: u32, height: u32 },
    /// The window system refused: the presenter cannot continue.
    Backend(anyhow::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("every CPU present buffer is held by the compositor"),
            Self::TooLarge { width, height } => {
                write!(f, "CPU present buffer {width}x{height} is too large")
            }
            Self::Backend(err) => write!(f, "CPU present failed: {err:#}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Counters for logs, tests and memory measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresenterStats {
    pub kind: PresenterKind,
    /// Buffers allocated now.
    pub buffers: usize,
    /// Of those, how many the compositor holds.
    pub busy_buffers: usize,
    /// Shared-memory bytes mapped for those buffers.
    pub buffer_bytes: usize,
    pub commits: u64,
    pub deferred: u64,
    /// Buffers created over the presenter's life.
    pub allocations: u64,
    pub frame_callback_pending: bool,
}

/// Largest buffer side, and largest buffer, the presenter allocates: an 8K
/// window at scale 1 fits.
pub const MAX_BUFFER_SIDE: u32 = 16_384;
pub const MAX_BUFFER_BYTES: usize = 256 << 20;

/// The buffer size for a window of `width` x `height` physical pixels at
/// `scale`. With an integer scale above 1 and no viewport, Wayland requires
/// the buffer to be a multiple of the scale; winit's physical sizes always
/// are, and rounding up guards a size that is not.
pub fn buffer_extent(width: u32, height: u32, scale: f64) -> (u32, u32) {
    let (w, h) = (width.max(1), height.max(1));
    if !(scale.is_finite() && scale >= 2.0 && scale.fract() == 0.0 && scale <= 16.0) {
        return (w, h);
    }
    let s = scale as u32;
    (
        w.div_ceil(s).saturating_mul(s),
        h.div_ceil(s).saturating_mul(s),
    )
}

fn check_extent(width: u32, height: u32) -> Result<(), FrameError> {
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4));
    if width > MAX_BUFFER_SIDE
        || height > MAX_BUFFER_SIDE
        || bytes.is_none_or(|b| b > MAX_BUFFER_BYTES)
    {
        return Err(FrameError::TooLarge { width, height });
    }
    Ok(())
}

pub(crate) type WindowOwner = Arc<dyn HasWindowHandle + Send + Sync>;
pub(crate) type DisplayOwner = Arc<dyn HasDisplayHandle + Send + Sync>;

/// A CPU framebuffer presenter for one window.
pub struct CpuPresenter {
    inner: Inner,
}

// A presenter can be built on a worker thread and handed to the window's.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<CpuPresenter>();
};

enum Inner {
    Shm(Box<shm::ShmPresenter>),
    Soft(Box<soft::SoftPresenter>),
}

impl CpuPresenter {
    /// A presenter for `window`, on the connection `display` (winit's
    /// `owned_display_handle()`, or the window itself). Both handles are
    /// kept for the presenter's life. Opaque until [`Self::set_format`].
    pub fn new<W, D>(window: Arc<W>, display: D) -> Result<Self>
    where
        W: HasWindowHandle + Send + Sync + 'static,
        D: HasDisplayHandle + Send + Sync + 'static,
    {
        Self::with_preference(window, display, PresenterPreference::Auto)
    }

    pub fn with_preference<W, D>(
        window: Arc<W>,
        display: D,
        preference: PresenterPreference,
    ) -> Result<Self>
    where
        W: HasWindowHandle + Send + Sync + 'static,
        D: HasDisplayHandle + Send + Sync + 'static,
    {
        let started = Instant::now();
        let window: WindowOwner = window;
        let display: DisplayOwner = Arc::new(display);
        let raw = display
            .display_handle()
            .map_err(|e| anyhow!("CPU present: no display handle: {e}"))?
            .as_raw();
        let inner = match raw {
            RawDisplayHandle::Wayland(_) if preference == PresenterPreference::Auto => {
                match shm::ShmPresenter::new(window.clone(), display.clone()) {
                    Ok(p) => Inner::Shm(Box::new(p)),
                    Err(err) => {
                        log::warn!(
                            "CPU present: wl_shm presenter unavailable ({err:#}); using softbuffer"
                        );
                        Inner::Soft(Box::new(soft::SoftPresenter::new(window, display)?))
                    }
                }
            }
            RawDisplayHandle::Wayland(_) | RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_) => {
                Inner::Soft(Box::new(soft::SoftPresenter::new(window, display)?))
            }
            other => return Err(anyhow!("CPU present: unsupported display {other:?}")),
        };
        let presenter = Self { inner };
        log::info!(
            "CPU present: {:?} ready in {} us",
            presenter.kind(),
            started.elapsed().as_micros()
        );
        Ok(presenter)
    }

    pub fn kind(&self) -> PresenterKind {
        match &self.inner {
            Inner::Shm(_) => PresenterKind::WaylandShm,
            Inner::Soft(p) => p.kind(),
        }
    }

    /// Size the next frame for a window of `width` x `height` physical
    /// pixels at `scale`. A change makes every buffer's contents unknown, so
    /// the next frame repaints in full and commits at once.
    pub fn resize(&mut self, width: u32, height: u32, scale: f64) {
        match &mut self.inner {
            Inner::Shm(p) => p.resize(width, height, scale),
            Inner::Soft(p) => p.resize(width, height, scale),
        }
    }

    /// The buffer size the next frame gets.
    pub fn size(&self) -> (u32, u32) {
        match &self.inner {
            Inner::Shm(p) => p.size(),
            Inner::Soft(p) => p.size(),
        }
    }

    /// Ask for `format` and return the format frames get. A presenter that
    /// cannot show alpha (softbuffer on Wayland, a depth-24 X11 window)
    /// stays [`PixelFormat::Opaque`], and the renderer must then draw an
    /// opaque frame.
    pub fn set_format(&mut self, format: PixelFormat) -> PixelFormat {
        match &mut self.inner {
            Inner::Shm(p) => p.set_format(format),
            Inner::Soft(p) => p.set_format(format),
        }
    }

    pub fn format(&self) -> PixelFormat {
        match &self.inner {
            Inner::Shm(p) => p.format(),
            Inner::Soft(p) => p.format(),
        }
    }

    pub fn supports_translucency(&self) -> bool {
        match &self.inner {
            Inner::Shm(_) => true,
            Inner::Soft(p) => p.supports_translucency(),
        }
    }

    /// The buffer the next frame draws into, with its age.
    pub fn buffer_mut(&mut self) -> Result<CpuFrame<'_>, FrameError> {
        let inner = match &mut self.inner {
            Inner::Shm(p) => FrameInner::Shm(p.buffer_mut()?),
            Inner::Soft(p) => FrameInner::Soft(p.buffer_mut()?),
        };
        Ok(CpuFrame { inner })
    }

    /// Commit a deferred frame if the compositor is ready for it.
    pub fn flush(&mut self) -> Result<PresentOutcome> {
        self.flush_with(|| {})
    }

    /// [`Self::flush`], running `pre_commit` just before a commit, as
    /// [`CpuFrame::present_with`] does.
    pub fn flush_with(&mut self, pre_commit: impl FnOnce()) -> Result<PresentOutcome> {
        match &mut self.inner {
            Inner::Shm(p) => p.flush(pre_commit),
            Inner::Soft(_) => Ok(PresentOutcome::Unchanged),
        }
    }

    /// A frame is drawn but not yet committed.
    pub fn has_deferred(&self) -> bool {
        match &self.inner {
            Inner::Shm(p) => p.has_deferred(),
            Inner::Soft(_) => false,
        }
    }

    /// The last commit's frame callback has not arrived: a frame presented
    /// now would be deferred.
    pub fn frame_pending(&mut self) -> bool {
        match &mut self.inner {
            Inner::Shm(p) => p.frame_pending(),
            Inner::Soft(_) => false,
        }
    }

    /// Release every spare buffer now, keeping the one with the newest
    /// frame. Returns how many went.
    pub fn release_idle_buffers(&mut self) -> usize {
        match &mut self.inner {
            Inner::Shm(p) => p.release_idle_buffers(),
            Inner::Soft(_) => 0,
        }
    }

    /// When a spare buffer becomes idle enough for
    /// [`Self::release_idle_buffers`]; `None` with no spare. A caller with
    /// nothing to draw wakes at this time to release it.
    pub fn idle_release_deadline(&self) -> Option<Instant> {
        match &self.inner {
            Inner::Shm(p) => p.idle_release_deadline(),
            Inner::Soft(_) => None,
        }
    }

    /// The window is hidden (minimised, or occluded as winit reports). While
    /// occluded every buffer the compositor has released goes, the newest
    /// too: the compositor keeps showing what it last read. On return the
    /// next frame repaints in full and commits without waiting for a frame
    /// callback, which compositors withhold from hidden surfaces.
    pub fn set_occluded(&mut self, occluded: bool) {
        match &mut self.inner {
            Inner::Shm(p) => p.set_occluded(occluded),
            Inner::Soft(p) => p.set_occluded(occluded),
        }
    }

    pub fn stats(&self) -> PresenterStats {
        match &self.inner {
            Inner::Shm(p) => p.stats(),
            Inner::Soft(p) => p.stats(),
        }
    }
}

/// One frame being drawn. Present it with [`CpuFrame::present`]; dropping
/// it instead leaves the buffer's contents unknown (its next use repaints
/// in full) and presents nothing.
pub struct CpuFrame<'a> {
    inner: FrameInner<'a>,
}

enum FrameInner<'a> {
    Shm(shm::ShmFrame<'a>),
    Soft(soft::SoftFrame<'a>),
}

impl CpuFrame<'_> {
    pub fn width(&self) -> u32 {
        match &self.inner {
            FrameInner::Shm(f) => f.width(),
            FrameInner::Soft(f) => f.width(),
        }
    }

    pub fn height(&self) -> u32 {
        match &self.inner {
            FrameInner::Shm(f) => f.height(),
            FrameInner::Soft(f) => f.height(),
        }
    }

    /// Pixels per row. Equal to [`Self::width`] today.
    pub fn stride(&self) -> u32 {
        self.width()
    }

    /// 0: the contents are unknown. n: the buffer holds the frame drawn n
    /// frames ago.
    pub fn age(&self) -> u8 {
        match &self.inner {
            FrameInner::Shm(f) => f.age(),
            FrameInner::Soft(f) => f.age(),
        }
    }

    pub fn format(&self) -> PixelFormat {
        match &self.inner {
            FrameInner::Shm(f) => f.format(),
            FrameInner::Soft(f) => f.format(),
        }
    }

    /// What must be repainted, given this frame's own `damage`, so the
    /// buffer ends up holding the whole new frame: `damage` plus whatever
    /// changed since this buffer's contents were drawn, or the whole buffer
    /// when its age is unknown.
    pub fn repaint_region(&self, damage: &[Rect]) -> Vec<Rect> {
        match &self.inner {
            FrameInner::Shm(f) => f.repaint_region(damage),
            FrameInner::Soft(f) => f.repaint_region(damage),
        }
    }

    /// `width * height` pixels, row-major.
    pub fn pixels(&self) -> &[u32] {
        match &self.inner {
            FrameInner::Shm(f) => f.pixels(),
            FrameInner::Soft(f) => f.pixels(),
        }
    }

    pub fn pixels_mut(&mut self) -> &mut [u32] {
        match &mut self.inner {
            FrameInner::Shm(f) => f.pixels_mut(),
            FrameInner::Soft(f) => f.pixels_mut(),
        }
    }

    /// Present the frame. `damage` is what changed since the previous
    /// frame (not the age-widened repaint region); it is clipped to the
    /// buffer.
    pub fn present(self, damage: &[Rect]) -> Result<PresentOutcome> {
        self.present_with(damage, || {})
    }

    /// [`Self::present`], running `pre_commit` just before the commit, and
    /// not at all when the frame is deferred or unchanged. Pass winit's
    /// `pre_present_notify` here, not before the call: winit asks for its
    /// frame callback at once, so calling it for a frame that is then
    /// deferred leaves winit waiting on a callback no commit will bring,
    /// and `RedrawRequested` never comes again.
    pub fn present_with(
        self,
        damage: &[Rect],
        pre_commit: impl FnOnce(),
    ) -> Result<PresentOutcome> {
        match self.inner {
            FrameInner::Shm(f) => f.present(damage, pre_commit),
            FrameInner::Soft(f) => f.present(damage, pre_commit),
        }
    }
}
