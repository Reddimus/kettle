//! softbuffer: the X11 presenter, and the Wayland fallback.

use std::num::NonZeroU32;

use anyhow::{Result, anyhow};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use super::damage::{DamageHistory, Exposures, Rect, normalize};
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
    x11_put_image_limit: Option<usize>,
    /// The size frames get, as asked; the surface is sized to it at the
    /// next `buffer_mut`, which refuses a size past the limits.
    width: u32,
    height: u32,
    /// The size the softbuffer surface has.
    surface_size: (u32, u32),
    format: PixelFormat,
    ages: SoftAges,
    commits: u64,
    /// X11: what the server lost of the window since the last present, and
    /// the connection that hears of it ([`ExposeWatcher`]).
    exposures: Exposures,
    watcher: Option<ExposeWatcher>,
    /// X11 with no watcher: every present sends the whole buffer, since an
    /// exposed region could not be told apart from an unchanged one.
    send_whole: bool,
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
        let (alpha, x11_put_image_limit, watcher) = if kind == PresenterKind::SoftbufferX11 {
            let props = x11_window_properties(raw_display, raw_window)
                .ok_or_else(|| anyhow!("could not query the X11 CPU presentation transport"))?;
            (props.alpha, props.put_image_limit, props.watcher)
        } else {
            (false, None, None)
        };
        let send_whole = kind == PresenterKind::SoftbufferX11 && watcher.is_none();
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
            x11_put_image_limit,
            width: 0,
            height: 0,
            surface_size: (0, 0),
            format: PixelFormat::Opaque,
            ages: SoftAges::default(),
            commits: 0,
            exposures: Exposures::default(),
            watcher,
            send_whole,
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

    /// X11: exposed regions are heard of and sent again, region by region.
    #[cfg(test)]
    pub(super) fn watches_exposures(&self) -> bool {
        self.watcher.is_some()
    }

    pub(super) fn resize(&mut self, width: u32, height: u32, scale: f64) {
        let (w, h) = buffer_extent(width, height, scale);
        if (w, h) != (self.width, self.height) {
            self.width = w;
            self.height = h;
            self.history.clear();
            self.ages.reset();
        }
    }

    /// Size the surface to the asked size, or refuse it as the wl_shm
    /// presenter does: a frame of the old size would be drawn for a window
    /// of the new one.
    fn apply_size(&mut self) -> Result<(), FrameError> {
        let (w, h) = (self.width, self.height);
        check_extent(w, h)?;
        if let Some(limit) = self.x11_put_image_limit {
            check_put_image(w, h, limit)?;
        }
        if self.surface_size == (w, h) {
            return Ok(());
        }
        let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else {
            return Err(FrameError::TooLarge {
                width: w,
                height: h,
            });
        };
        self.surface
            .resize(nw, nh)
            .map_err(|e| FrameError::Backend(anyhow!("softbuffer resize to {w}x{h}: {e}")))?;
        self.surface_size = (w, h);
        Ok(())
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
        self.apply_size()?;
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
            exposures: &mut self.exposures,
            watcher: &mut self.watcher,
            send_whole: &mut self.send_whole,
            width,
            height,
            age,
            format: self.format,
            wayland: self.kind == PresenterKind::SoftbufferWayland,
            force_alpha: self.alpha && self.format == PixelFormat::Opaque,
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
            buffer_bytes: buffers * self.surface_size.0 as usize * self.surface_size.1 as usize * 4,
            commits: self.commits,
            timed_out_commits: 0,
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
    exposures: &'a mut Exposures,
    watcher: &'a mut Option<ExposeWatcher>,
    send_whole: &'a mut bool,
    width: u32,
    height: u32,
    age: u8,
    format: PixelFormat,
    /// On Wayland every present commits, even with no damage.
    wayland: bool,
    /// An opaque frame on a window whose visual carries alpha.
    force_alpha: bool,
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
        let Some(mut buffer) = self.buffer.take() else {
            return Ok(PresentOutcome::Unchanged);
        };
        let (w, h) = (self.width, self.height);
        let current = normalize(damage, w, h);
        self.history.push(current.clone());
        let full = self.ages.presented();
        // X11 without a compositing manager drops the pixels of a window
        // that another one covered: the regions uncovered since the last
        // present are sent again from this buffer, which still holds them.
        if let Some(watcher) = self.watcher.as_mut()
            && let Err(error) = watcher.take(self.exposures)
        {
            log::warn!(
                "CPU present: lost the X11 exposure watch ({error}); every frame now sends \
                 the whole window"
            );
            *self.watcher = None;
            *self.send_whole = true;
        }
        let out = if full || *self.send_whole {
            self.exposures.clear();
            vec![Rect::full(w, h)]
        } else {
            self.exposures.upload(&current, w, h)
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
        if self.force_alpha {
            force_opaque(&mut buffer, w, &out);
        }
        pre_commit();
        buffer
            .present_with_damage(&rects)
            .map_err(|e| anyhow!("softbuffer present: {e}"))?;
        *self.commits += 1;
        // softbuffer's Wayland backend attaches and commits whatever the
        // damage; its X11 backend sends nothing for none.
        Ok(if self.wayland || !out.is_empty() {
            PresentOutcome::Presented
        } else {
            PresentOutcome::Unchanged
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

/// Set the alpha byte of `rects` to opaque. On an X11 window with a
/// depth-32 visual softbuffer sends all 32 bits, and winit's
/// `set_transparent(false)` does nothing on X11, so the `0x00RRGGBB` pixels
/// of an opaque frame would be see-through under a compositing manager.
pub(super) fn force_opaque(pixels: &mut [u32], width: u32, rects: &[Rect]) {
    for rect in rects {
        for y in rect.y..rect.y + rect.height {
            let row = y as usize * width as usize;
            let span = row + rect.x as usize..row + (rect.x + rect.width) as usize;
            for px in &mut pixels[span] {
                *px |= 0xff00_0000;
            }
        }
    }
}

/// softbuffer sends the entire 32-bit image in one request without MIT-SHM.
fn check_put_image(width: u32, height: u32, maximum_bytes: usize) -> Result<(), FrameError> {
    let bytes = 24 + u64::from(width) * u64::from(height) * 4;
    let wire_bytes = bytes + 4 * u64::from(bytes / 4 > u64::from(u16::MAX));
    if wire_bytes > maximum_bytes as u64 {
        return Err(FrameError::Backend(anyhow!(
            "X11 MIT-SHM is unavailable and a {width}x{height} PutImage needs \
             {wire_bytes} bytes (server limit: {maximum_bytes} bytes)"
        )));
    }
    Ok(())
}

/// What the presenter needs to know of an X11 window.
struct X11Window {
    /// The window has a depth-32 visual, so the alpha byte reaches the
    /// compositing manager.
    alpha: bool,
    /// Without MIT-SHM: the largest request the server takes.
    put_image_limit: Option<usize>,
    watcher: Option<ExposeWatcher>,
}

/// Use the same connection as softbuffer; `$DISPLAY` may name a different server.
fn x11_window_properties(display: RawDisplayHandle, window: RawWindowHandle) -> Option<X11Window> {
    use as_raw_xcb_connection::AsRawXcbConnection as _;
    use x11rb::connection::RequestConnection as _;
    use x11rb::protocol::xproto::ConnectionExt as _;
    use x11rb::xcb_ffi::XCBConnection;
    let id = match window {
        RawWindowHandle::Xlib(h) => u32::try_from(h.window).ok()?,
        RawWindowHandle::Xcb(h) => h.window.get(),
        _ => return None,
    };
    let raw = match display {
        RawDisplayHandle::Xcb(h) => h.connection?.as_ptr(),
        RawDisplayHandle::Xlib(h) => {
            // SAFETY: winit's live Xlib display, which the caller's display
            // handle keeps open for this call. `from_ptr` returns it in a
            // ManuallyDrop, so it is never closed here.
            let display = unsafe { tiny_xlib::Display::from_ptr(h.display?.as_ptr()) };
            display.as_raw_xcb_connection().cast()
        }
        _ => return None,
    };
    // SAFETY: `raw` is the live XCB connection under the window's display,
    // open for this call; `false`: x11rb does not disconnect it on drop.
    let conn = unsafe { XCBConnection::from_raw_xcb_connection(raw, false) }.ok()?;
    let depth = conn.get_geometry(id).ok()?.reply().ok()?.depth;
    let put_image_limit = (!x11_shm_available(&conn)).then(|| conn.maximum_request_bytes());
    let watcher = match ExposeWatcher::open(&conn, id) {
        Ok(watcher) => Some(watcher),
        Err(reason) => {
            log::info!(
                "CPU present: not watching the X11 window for exposures ({reason}); every \
                 frame sends the whole window"
            );
            None
        }
    };
    Some(X11Window {
        alpha: depth == 32,
        put_image_limit,
        watcher,
    })
}

/// The X11 window's `Expose` events, on a connection of the presenter's own.
///
/// Without a compositing manager the X server keeps no copy of what another
/// window covers: uncovering it asks the client to draw it again. winit
/// turns that into `RedrawRequested`, the same event as Kettle's own redraw
/// requests (and merged with one pending), so the renderer sees a frame
/// that changed nowhere there and repaints nothing, and softbuffer's MIT-SHM
/// path sends only the frame's damage. The retained buffer still holds the
/// lost pixels; they only need sending again. winit reads every event on its
/// connection, so a second connection to the same server selects `Expose`
/// on the window (any number of clients may), and each present first reads
/// what it heard.
pub(super) struct ExposeWatcher {
    conn: x11rb::xcb_ffi::XCBConnection,
}

/// The property that proves the watcher's server is the window's.
const PROBE_PROPERTY: &[u8] = b"_KETTLE_CPU_PRESENT_PROBE";

impl ExposeWatcher {
    /// Connect to the window's server and select `Expose` on `window`.
    /// `winit` is the window's own connection: a nonce written on the window
    /// through it must read back through the new one, so a `$DISPLAY` that
    /// names another server is refused.
    fn open(winit: &x11rb::xcb_ffi::XCBConnection, window: u32) -> Result<Self, String> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::{
            AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, PropMode,
        };
        use x11rb::wrapper::ConnectionExt as _;
        let name = watcher_display().ok_or("no display name to connect to")?;
        let name = std::ffi::CString::new(name).map_err(|_| "a display name with a NUL")?;
        let (conn, _) = x11rb::xcb_ffi::XCBConnection::connect(Some(&name))
            .map_err(|e| format!("connecting: {e}"))?;
        let atom = winit
            .intern_atom(false, PROBE_PROPERTY)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .atom;
        let nonce = probe_nonce();
        winit
            .change_property8(PropMode::REPLACE, window, atom, AtomEnum::CARDINAL, &nonce)
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())?;
        let same = (|| {
            let here = conn
                .intern_atom(true, PROBE_PROPERTY)
                .ok()?
                .reply()
                .ok()?
                .atom;
            if here != atom {
                return Some(false);
            }
            let value = conn
                .get_property(false, window, atom, AtomEnum::CARDINAL, 0, 4)
                .ok()?
                .reply()
                .ok()?
                .value;
            Some(value == nonce)
        })();
        if let Ok(cookie) = winit.delete_property(window, atom) {
            let _ = cookie.check();
        }
        if same != Some(true) {
            return Err("the display names another server than the window's".into());
        }
        conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::EXPOSURE),
        )
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| e.to_string())?;
        conn.flush().map_err(|e| e.to_string())?;
        Ok(Self { conn })
    }

    /// Add every region exposed so far to `into`. After the round trip the
    /// server has sent this connection every `Expose` it made before, so
    /// before the one that brought winit's redraw.
    fn take(&mut self, into: &mut Exposures) -> Result<(), String> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::Event;
        use x11rb::protocol::xproto::ConnectionExt as _;
        self.conn
            .get_input_focus()
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        while let Some(event) = self.conn.poll_for_event().map_err(|e| e.to_string())? {
            if let Event::Expose(e) = event {
                into.add(Rect::new(
                    e.x.into(),
                    e.y.into(),
                    e.width.into(),
                    e.height.into(),
                ));
            }
        }
        Ok(())
    }
}

/// The server the watcher connects to: winit's, which Xlib opens from
/// `$DISPLAY`. Tests use only their own server, never `$DISPLAY`.
fn watcher_display() -> Option<String> {
    #[cfg(test)]
    let name = std::env::var("KETTLE_TEST_X11_DISPLAY");
    #[cfg(not(test))]
    let name = std::env::var("DISPLAY");
    name.ok().filter(|n| !n.is_empty())
}

/// 16 bytes no other process writes: per-process random hash keys.
fn probe_nonce() -> [u8; 16] {
    use std::hash::{BuildHasher, Hasher};
    let half = |i: u64| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        h.write_u64(i);
        h.finish().to_ne_bytes()
    };
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&half(0));
    out[8..].copy_from_slice(&half(1));
    out
}

// Querying the extension alone does not prove fd attachment is supported
// (e.g. a forwarded X connection). Match softbuffer's attach/detach probe.
fn x11_shm_available(conn: &x11rb::xcb_ffi::XCBConnection) -> bool {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::shm::ConnectionExt as _;

    fn attach_and_detach(conn: &x11rb::xcb_ffi::XCBConnection) -> anyhow::Result<()> {
        let fd =
            rustix::fs::memfd_create(c"kettle-x11-shm-probe", rustix::fs::MemfdFlags::CLOEXEC)?;
        rustix::fs::ftruncate(&fd, 4096)?;
        let id = conn.generate_id()?;
        let attach = conn.shm_attach_fd(id, fd, false)?;
        let detach = conn.shm_detach(id)?;
        let attached = attach.check();
        let detached = detach.check();
        attached?;
        detached?;
        Ok(())
    }
    attach_and_detach(conn).is_ok()
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[test]
    fn x11_wire_frames_must_fit_one_request_including_the_big_request_header() {
        let core_limit = usize::from(u16::MAX) * 4;
        assert!(check_put_image(128, 128, core_limit).is_ok());
        assert!(check_put_image(1920, 1080, core_limit).is_err());
        let normal_bytes = 24 + 1920 * 1080 * 4;
        assert!(check_put_image(1920, 1080, normal_bytes).is_err());
        assert!(check_put_image(1920, 1080, normal_bytes + 4).is_ok());
    }
}
