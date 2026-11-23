//! The Kettle-owned Wayland presenter: one memfd, `wl_shm_pool` and
//! `wl_buffer` per buffer, on winit's connection and surface.

use std::fs::File;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use memmap2::{MmapMut, MmapOptions};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use wayland_client::backend::{Backend, ObjectId, WaylandError};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

use super::book::{Choice, SlotBook};
use super::damage::{DamageHistory, HISTORY_FRAMES, Rect, normalize};
use super::{
    DisplayOwner, FrameError, PixelFormat, PresentOutcome, PresenterKind, PresenterStats,
    WindowOwner, buffer_extent, check_extent,
};

/// A frame callback that has not come after this stops holding frames back.
/// Compositors withhold callbacks from hidden surfaces; a window that is
/// visible again commits at once through [`ShmPresenter::set_occluded`], and
/// this bounds the wait when nothing reports occlusion. While the callback
/// is still out, the presenter commits at most once per this interval and
/// asks for no further callback, so a hidden surface never piles them up.
pub const FRAME_CALLBACK_TIMEOUT: Duration = Duration::from_secs(1);

/// Events the presenter's own queue receives.
#[derive(Default)]
pub(super) struct QueueState {
    released: Vec<u64>,
    frames_done: Vec<u64>,
}

/// One buffer's shared memory and protocol objects.
pub(super) struct ShmBacking {
    map: MmapMut,
    _file: File,
    capacity: usize,
    pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
}

impl ShmBacking {
    fn new(
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<QueueState>,
        id: u64,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<Self> {
        let bytes = width as usize * height as usize * 4;
        let file = memfd(bytes)?;
        // SAFETY: the memfd was just created and is owned here, so no other
        // code in this process maps or resizes it. It is sealed against
        // shrinking, so the compositor cannot truncate it under the mapping
        // (which would SIGBUS); it only reads it. The mapping lives as long
        // as `_file` and the pool, in this struct.
        let map = unsafe { MmapOptions::new().len(bytes).map_mut(&file) }
            .context("mapping CPU present buffer")?;
        let size = i32::try_from(bytes).context("CPU present buffer too large")?;
        let pool = shm.create_pool(file.as_fd(), size, qh, ());
        let buffer = create_buffer(&pool, qh, id, width, height, format)?;
        Ok(Self {
            map,
            _file: file,
            capacity: bytes,
            pool,
            buffer,
        })
    }

    /// Reuse this memory for another size or format when it fits without
    /// wasting more than half of it.
    fn try_reshape(
        &mut self,
        qh: &QueueHandle<QueueState>,
        id: u64,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> bool {
        let bytes = width as usize * height as usize * 4;
        if bytes > self.capacity || bytes * 2 < self.capacity {
            return false;
        }
        let Ok(buffer) = create_buffer(&self.pool, qh, id, width, height, format) else {
            return false;
        };
        let old = std::mem::replace(&mut self.buffer, buffer);
        old.destroy();
        true
    }

    fn pixels(&self, len: usize) -> &[u32] {
        bytemuck::cast_slice(&self.map[..len * 4])
    }

    fn pixels_mut(&mut self, len: usize) -> &mut [u32] {
        bytemuck::cast_slice_mut(&mut self.map[..len * 4])
    }
}

impl Drop for ShmBacking {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
    }
}

fn create_buffer(
    pool: &wl_shm_pool::WlShmPool,
    qh: &QueueHandle<QueueState>,
    id: u64,
    width: u32,
    height: u32,
    format: PixelFormat,
) -> Result<wl_buffer::WlBuffer> {
    let w = i32::try_from(width)?;
    let h = i32::try_from(height)?;
    let stride = w.checked_mul(4).ok_or_else(|| anyhow!("stride overflow"))?;
    let format = match format {
        PixelFormat::Opaque => wl_shm::Format::Xrgb8888,
        PixelFormat::Premultiplied => wl_shm::Format::Argb8888,
    };
    Ok(pool.create_buffer(0, w, h, stride, format, qh, id))
}

fn memfd(bytes: usize) -> Result<File> {
    use rustix::fs::{MemfdFlags, SealFlags};
    let fd = rustix::fs::memfd_create(
        c"kettle-cpu-present",
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
    )
    .context("memfd_create")?;
    rustix::fs::ftruncate(&fd, bytes as u64).context("sizing CPU present buffer")?;
    rustix::fs::fcntl_add_seals(&fd, SealFlags::SHRINK | SealFlags::SEAL)
        .context("sealing CPU present buffer")?;
    Ok(File::from(fd))
}

// Field order is drop order: the buffers (which send destroy requests) go
// before the queue and connection, and both before the window and display
// handles whose objects they borrow.
pub(super) struct ShmPresenter {
    book: SlotBook<ShmBacking>,
    history: DamageHistory,
    surface: wl_surface::WlSurface,
    shm: wl_shm::WlShm,
    qh: QueueHandle<QueueState>,
    queue: EventQueue<QueueState>,
    state: QueueState,
    conn: Connection,
    width: u32,
    height: u32,
    format: PixelFormat,
    /// The serial of the one frame callback out, until it is done. A commit
    /// made while it is out asks for no other.
    callback: Option<u64>,
    /// When the last commit went out.
    last_commit: Option<Instant>,
    /// [`FRAME_CALLBACK_TIMEOUT`]; shorter in tests.
    pub(super) frame_timeout: Duration,
    next_serial: u64,
    /// Commit the next frame in full, without waiting for a callback.
    force_next: bool,
    occluded: bool,
    /// The buffer holding a frame drawn but not committed.
    deferred: Option<u64>,
    /// Damage presented since the last commit. It outlives `deferred`: when
    /// the deferred frame's buffer is drawn over and abandoned, the frame is
    /// gone but the screen still lacks its changes, so the next commit
    /// carries them.
    uncommitted: Vec<Rect>,
    commits: u64,
    timed_out_commits: u64,
    deferrals: u64,
    allocations: u64,
    _window: WindowOwner,
    _display: DisplayOwner,
}

impl ShmPresenter {
    pub(super) fn new(window: WindowOwner, display: DisplayOwner) -> Result<Self> {
        let RawDisplayHandle::Wayland(wl_display) = display
            .display_handle()
            .map_err(|e| anyhow!("display handle: {e}"))?
            .as_raw()
        else {
            return Err(anyhow!("not a Wayland display"));
        };
        let RawWindowHandle::Wayland(wl_window) = window
            .window_handle()
            .map_err(|e| anyhow!("window handle: {e}"))?
            .as_raw()
        else {
            return Err(anyhow!("not a Wayland window"));
        };
        // SAFETY: the pointer is the live `wl_display` of winit's
        // connection, kept alive by `display`, which this presenter holds
        // until after `conn` drops (field order). A foreign backend never
        // closes the display.
        let backend = unsafe { Backend::from_foreign_display(wl_display.display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, queue) =
            registry_queue_init::<QueueState>(&conn).context("Wayland registry")?;
        let qh = queue.handle();
        let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).context("binding wl_shm")?;
        // SAFETY: the pointer is winit's live `wl_surface` proxy on this
        // display, kept alive by `window`, held until after `surface` drops.
        let surface_id = unsafe {
            ObjectId::from_ptr(
                wl_surface::WlSurface::interface(),
                wl_window.surface.as_ptr().cast(),
            )
        }
        .map_err(|e| anyhow!("wl_surface proxy: {e:?}"))?;
        let surface = wl_surface::WlSurface::from_id(&conn, surface_id)
            .map_err(|e| anyhow!("wl_surface proxy: {e:?}"))?;
        Ok(Self {
            book: SlotBook::default(),
            history: DamageHistory::default(),
            surface,
            shm,
            qh,
            queue,
            state: QueueState::default(),
            conn,
            width: 1,
            height: 1,
            format: PixelFormat::Opaque,
            callback: None,
            last_commit: None,
            frame_timeout: FRAME_CALLBACK_TIMEOUT,
            next_serial: 1,
            force_next: true,
            occluded: false,
            deferred: None,
            uncommitted: Vec::new(),
            commits: 0,
            timed_out_commits: 0,
            deferrals: 0,
            allocations: 0,
            _window: window,
            _display: display,
        })
    }

    pub(super) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub(super) fn format(&self) -> PixelFormat {
        self.format
    }

    fn invalidate(&mut self) {
        self.book.invalidate();
        self.history.clear();
        self.deferred = None;
        self.uncommitted.clear();
        self.force_next = true;
    }

    pub(super) fn resize(&mut self, width: u32, height: u32, scale: f64) {
        let (w, h) = buffer_extent(width, height, scale);
        if (w, h) != (self.width, self.height) {
            self.width = w;
            self.height = h;
            self.invalidate();
        }
    }

    pub(super) fn set_format(&mut self, format: PixelFormat) -> PixelFormat {
        if format != self.format {
            self.format = format;
            self.invalidate();
        }
        format
    }

    /// Take the events winit's reads put on this queue. It never reads the
    /// socket itself: another reader may hold a prepared read, and
    /// `wl_display_read_events` would then block.
    fn dispatch(&mut self) {
        if let Err(err) = self.queue.dispatch_pending(&mut self.state) {
            log::warn!("CPU present: Wayland dispatch failed: {err}");
        }
        for id in self.state.released.drain(..) {
            self.book.released(id);
        }
        for serial in self.state.frames_done.drain(..) {
            if self.callback.is_some_and(|out| serial >= out) {
                self.callback = None;
            }
        }
    }

    fn gate_open(&self, now: Instant) -> bool {
        self.force_next
            || self.callback.is_none()
            || self
                .last_commit
                .is_none_or(|at| now.saturating_duration_since(at) >= self.frame_timeout)
    }

    pub(super) fn frame_pending(&mut self) -> bool {
        self.dispatch();
        !self.gate_open(Instant::now())
    }

    pub(super) fn has_deferred(&self) -> bool {
        self.deferred.is_some()
    }

    pub(super) fn pending_deadline(&self) -> Option<Instant> {
        self.deferred?;
        let now = Instant::now();
        if self.gate_open(now) {
            // Due now, or as soon as a pending callback is dispatched.
            return Some(now);
        }
        Some(self.last_commit.map_or(now, |at| at + self.frame_timeout))
    }

    fn remove(&mut self, ids: Vec<u64>) -> usize {
        let mut removed = 0;
        for id in ids {
            if self.deferred == Some(id) {
                self.deferred = None;
            }
            if self.book.remove(id).is_some() {
                removed += 1;
            }
        }
        if removed > 0 {
            self.flush_connection();
        }
        removed
    }

    fn release_expired(&mut self, now: Instant) {
        let idle = self.book.idle_spares(now);
        self.remove(idle);
    }

    pub(super) fn release_idle_buffers(&mut self) -> usize {
        self.dispatch();
        let spares = self.book.spares(true);
        self.remove(spares)
    }

    pub(super) fn idle_release_deadline(&self) -> Option<Instant> {
        self.book.idle_deadline()
    }

    pub(super) fn set_occluded(&mut self, occluded: bool) {
        if occluded == self.occluded {
            return;
        }
        self.occluded = occluded;
        if occluded {
            self.dispatch();
            let all = self.book.spares(false);
            self.remove(all);
        } else {
            // The withheld callback stays the one out: the compositor sends
            // it once it shows the surface again.
            self.force_next = true;
        }
    }

    pub(super) fn buffer_mut(&mut self) -> Result<ShmFrame<'_>, FrameError> {
        self.dispatch();
        let now = Instant::now();
        self.release_expired(now);
        let (width, height, format) = (self.width, self.height, self.format);
        check_extent(width, height)?;
        let id = match self.book.choose(width, height, format) {
            Choice::Reuse(id) => id,
            Choice::Replace(id) => {
                let reshaped = self.book.get_mut(id).is_some_and(|slot| {
                    let ok = slot
                        .backing
                        .try_reshape(&self.qh, id, width, height, format);
                    if ok {
                        slot.width = width;
                        slot.height = height;
                        slot.format = format;
                        slot.age = 0;
                    }
                    ok
                });
                if reshaped {
                    id
                } else {
                    self.remove(vec![id]);
                    self.allocate(width, height, format, now)?
                }
            }
            Choice::Allocate => self.allocate(width, height, format, now)?,
            Choice::Busy => return Err(FrameError::Busy),
        };
        let age = self.book.get(id).map_or(0, |slot| slot.age);
        Ok(ShmFrame {
            presenter: self,
            id,
            age,
            done: false,
        })
    }

    fn allocate(
        &mut self,
        width: u32,
        height: u32,
        format: PixelFormat,
        now: Instant,
    ) -> Result<u64, FrameError> {
        // The buffer's user data is the id the book is about to assign.
        let id = self.book.peek_next_id();
        let backing = ShmBacking::new(&self.shm, &self.qh, id, width, height, format)
            .map_err(FrameError::Backend)?;
        let inserted = self.book.insert(width, height, format, now, backing);
        debug_assert_eq!(inserted, id);
        self.allocations += 1;
        log::debug!(
            "CPU present: buffer {id} {width}x{height} {format:?} ({} live)",
            self.book.len()
        );
        Ok(id)
    }

    fn present_slot(
        &mut self,
        id: u64,
        damage: &[Rect],
        pre_commit: impl FnOnce(),
    ) -> Result<PresentOutcome> {
        let now = Instant::now();
        let (width, height) = (self.width, self.height);
        let current = normalize(damage, width, height);
        self.history.push(current.clone());
        self.book.drew(id, now, HISTORY_FRAMES);
        self.uncommitted.extend(current);
        self.uncommitted = normalize(&self.uncommitted, width, height);
        if self.uncommitted.is_empty() && !self.force_next {
            return Ok(PresentOutcome::Unchanged);
        }
        // `id` now holds every change not yet on screen.
        self.deferred = Some(id);
        self.dispatch();
        if !self.gate_open(now) {
            self.deferrals += 1;
            return Ok(PresentOutcome::Deferred);
        }
        self.commit(id, now, pre_commit)
    }

    pub(super) fn flush(&mut self, pre_commit: impl FnOnce()) -> Result<PresentOutcome> {
        self.dispatch();
        let now = Instant::now();
        let Some(id) = self.deferred else {
            return Ok(PresentOutcome::Unchanged);
        };
        if !self.gate_open(now) {
            return Ok(PresentOutcome::Deferred);
        }
        self.commit(id, now, pre_commit)
    }

    fn commit(
        &mut self,
        id: u64,
        now: Instant,
        pre_commit: impl FnOnce(),
    ) -> Result<PresentOutcome> {
        let damage = if self.force_next {
            vec![Rect::full(self.width, self.height)]
        } else {
            self.uncommitted.clone()
        };
        let timed_out = self.callback.is_some() && !self.force_next;
        let buffer = self
            .book
            .get(id)
            .map(|slot| slot.backing.buffer.clone())
            .ok_or_else(|| anyhow!("CPU present buffer {id} vanished"))?;
        // Caller code runs first, before anything is staged on the surface.
        // If it unwinds, the surface owner's next commit carries nothing of
        // ours, and the frame stays deferred. It runs only when this commit
        // is certain: winit's `pre_present_notify` asks for its own frame
        // callback at once, and would wait for a commit that never comes.
        pre_commit();
        // From here to the commit nothing fails and no caller code runs, so
        // on the surface owner's thread (see the module docs) the attach
        // and the commit are one step. The buffer is the compositor's from
        // the attach on.
        self.book.committed(id);
        self.surface.attach(Some(&buffer), 0, 0);
        if self.surface.version() >= 4 {
            for rect in &damage {
                self.surface.damage_buffer(
                    rect.x as i32,
                    rect.y as i32,
                    rect.width as i32,
                    rect.height as i32,
                );
            }
        } else {
            // Before wl_surface v4 damage is in surface coordinates; damage
            // everything, as Mesa's WSI does.
            self.surface.damage(0, 0, i32::MAX, i32::MAX);
        }
        if self.callback.is_none() {
            let serial = self.next_serial;
            self.next_serial += 1;
            self.surface.frame(&self.qh, serial);
            self.callback = Some(serial);
        }
        self.surface.commit();
        self.deferred = None;
        self.uncommitted.clear();
        self.last_commit = Some(now);
        self.force_next = false;
        self.commits += 1;
        self.timed_out_commits += u64::from(timed_out);
        // Buffers of an old size or format are no longer needed once a frame
        // of the new one is up; busy ones go when released.
        let stale = self.book.stale(self.width, self.height, self.format);
        self.remove(stale);
        self.flush_connection();
        Ok(PresentOutcome::Presented)
    }

    fn flush_connection(&self) {
        match self.conn.flush() {
            Ok(()) => {}
            // The socket is full; winit's loop flushes again before it sleeps.
            Err(WaylandError::Io(err)) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => log::warn!("CPU present: Wayland flush failed: {err}"),
        }
    }

    pub(super) fn stats(&self) -> PresenterStats {
        PresenterStats {
            kind: PresenterKind::WaylandShm,
            buffers: self.book.len(),
            busy_buffers: self.book.busy_count(),
            buffer_bytes: self.book.iter().map(|s| s.backing.capacity).sum(),
            commits: self.commits,
            timed_out_commits: self.timed_out_commits,
            deferred: self.deferrals,
            allocations: self.allocations,
            frame_callback_pending: self.callback.is_some(),
        }
    }
}

impl Drop for ShmPresenter {
    fn drop(&mut self) {
        // Destroy the buffers while the connection is up, and send it.
        let all: Vec<u64> = self.book.iter().map(|s| s.id).collect();
        for id in all {
            self.book.remove(id);
        }
        self.flush_connection();
    }
}

pub(super) struct ShmFrame<'a> {
    presenter: &'a mut ShmPresenter,
    id: u64,
    age: u8,
    done: bool,
}

impl ShmFrame<'_> {
    pub(super) fn width(&self) -> u32 {
        self.presenter.width
    }

    pub(super) fn height(&self) -> u32 {
        self.presenter.height
    }

    pub(super) fn age(&self) -> u8 {
        self.age
    }

    pub(super) fn format(&self) -> PixelFormat {
        self.presenter.format
    }

    fn len(&self) -> usize {
        self.presenter.width as usize * self.presenter.height as usize
    }

    pub(super) fn repaint_region(&self, damage: &[Rect]) -> Vec<Rect> {
        let (w, h) = (self.presenter.width, self.presenter.height);
        self.presenter
            .history
            .repaint_region(self.age, &normalize(damage, w, h), w, h)
    }

    pub(super) fn pixels(&self) -> &[u32] {
        let len = self.len();
        self.presenter
            .book
            .get(self.id)
            .map_or(&[][..], |slot| slot.backing.pixels(len))
    }

    pub(super) fn pixels_mut(&mut self) -> &mut [u32] {
        let len = self.len();
        match self.presenter.book.get_mut(self.id) {
            Some(slot) => slot.backing.pixels_mut(len),
            None => &mut [],
        }
    }

    pub(super) fn present(
        mut self,
        damage: &[Rect],
        pre_commit: impl FnOnce(),
    ) -> Result<PresentOutcome> {
        self.done = true;
        self.presenter.present_slot(self.id, damage, pre_commit)
    }
}

impl Drop for ShmFrame<'_> {
    fn drop(&mut self) {
        if !self.done {
            // The buffer may hold half a frame now. If it held the deferred
            // frame, that frame is gone: nothing may commit this buffer
            // until a frame is presented into it again. Its damage stays in
            // `uncommitted`, and the buffer's age is 0, so the next frame
            // repaints it in full and commits the lost frame's changes too.
            self.presenter.book.abandoned(self.id);
            if self.presenter.deferred == Some(self.id) {
                self.presenter.deferred = None;
            }
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for QueueState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for QueueState {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for QueueState {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, u64> for QueueState {
    fn event(
        state: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.released.push(*id);
        }
    }
}

impl Dispatch<wl_callback::WlCallback, u64> for QueueState {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        serial: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.frames_done.push(*serial);
        }
    }
}
