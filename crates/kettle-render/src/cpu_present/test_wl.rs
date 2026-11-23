//! An in-process Wayland compositor for the presenter's tests.
//!
//! It serves `wl_compositor` v4, `wl_shm` (v1 or v2) and, optionally,
//! `wl_fixes` over socketpairs, on its own thread. It records every commit
//! (the attached buffer's pixels and the damage) and holds frame callbacks
//! and buffers until a test lets them go, so a test can play the compositor
//! and the surface owner (winit) at once.
//!
//! The test side is libwayland-client (wayland-client's `system` backend),
//! as in production. A host without libwayland-client skips these tests.

// The tests that use the rest arrive with the fixes they cover.
#![allow(dead_code)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle, WindowHandle,
};
use wayland_server::backend::{ClientData, ClientId, ObjectId};
use wayland_server::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_fixes, wl_region, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_server::{
    Client, DataInit, Dispatch, Display, DisplayHandle as ServerHandle, GlobalDispatch, New,
    Resource,
};

/// What the compositor advertises and how it treats buffers.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub shm_version: u32,
    pub fixes: bool,
    /// Release each buffer as soon as its commit is read, as mutter does
    /// for shm. Otherwise buffers are held until [`Server::release_all`].
    pub release_on_commit: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shm_version: 1,
            fixes: false,
            release_on_commit: true,
        }
    }
}

/// One `wl_surface.commit` as the compositor saw it.
#[derive(Clone, Debug)]
pub struct Commit {
    /// A buffer was attached since the previous commit.
    pub attached: bool,
    /// The attached buffer's pixels at the commit, row-major.
    pub pixels: Vec<u32>,
    pub damage: Vec<[i32; 4]>,
}

#[derive(Debug, Default)]
pub struct Log {
    pub commits: Vec<Commit>,
    pub callbacks_requested: usize,
    pub callbacks_done: usize,
    pub shm_binds: usize,
    pub shm_versions: Vec<u32>,
    pub shm_releases: usize,
    pub fixes_binds: usize,
    pub registries_destroyed: usize,
    /// Buffers committed and not yet released.
    pub held: usize,
}

impl Log {
    pub fn callbacks_outstanding(&self) -> usize {
        self.callbacks_requested - self.callbacks_done
    }
}

enum Cmd {
    FireFrames,
    ReleaseAll,
    Stop,
}

pub struct Server {
    log: Arc<Mutex<Log>>,
    cmds: mpsc::Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// A compositor with `clients` connections; returns the client ends.
    pub fn start(cfg: Config, clients: usize) -> (Self, Vec<UnixStream>) {
        let log = Arc::new(Mutex::new(Log::default()));
        let (tx, rx) = mpsc::channel();
        let mut ours = Vec::new();
        let mut theirs = Vec::new();
        for _ in 0..clients {
            let (a, b) = UnixStream::pair().expect("socketpair");
            ours.push(a);
            theirs.push(b);
        }
        let thread_log = log.clone();
        let thread = std::thread::spawn(move || serve(cfg, thread_log, theirs, rx));
        (
            Self {
                log,
                cmds: tx,
                thread: Some(thread),
            },
            ours,
        )
    }

    /// Send `done` for every frame callback the compositor holds, after it
    /// has read everything `client` sent, and deliver it to `client`.
    pub fn fire_frames(&self, client: &mut TestClient) {
        self.run(Cmd::FireFrames, client);
    }

    /// Release every buffer the compositor holds, likewise.
    pub fn release_all(&self, client: &mut TestClient) {
        self.run(Cmd::ReleaseAll, client);
    }

    fn run(&self, cmd: Cmd, client: &mut TestClient) {
        // The first roundtrip makes the compositor read the client's
        // requests so far; a command is run before the next request is
        // dispatched, so the second one delivers its events.
        client.roundtrip();
        let _ = self.cmds.send(cmd);
        client.roundtrip();
    }

    pub fn log(&self) -> MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Tag;
impl ClientData for Tag {}

#[derive(Default)]
struct PendingSurface {
    buffer: Option<Option<wl_buffer::WlBuffer>>,
    damage: Vec<[i32; 4]>,
    callbacks: Vec<wl_callback::WlCallback>,
}

struct State {
    cfg: Config,
    log: Arc<Mutex<Log>>,
    surfaces: HashMap<ObjectId, PendingSurface>,
    frames: Vec<wl_callback::WlCallback>,
    held: Vec<wl_buffer::WlBuffer>,
}

impl State {
    fn log(&self) -> MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn run(&mut self, cmd: &Cmd) {
        match cmd {
            Cmd::FireFrames => {
                let n = self.frames.len();
                for cb in self.frames.drain(..) {
                    cb.done(0);
                }
                self.log().callbacks_done += n;
            }
            Cmd::ReleaseAll => {
                for b in self.held.drain(..) {
                    if b.is_alive() {
                        b.release();
                    }
                }
                self.log().held = 0;
            }
            Cmd::Stop => {}
        }
    }
}

fn serve(cfg: Config, log: Arc<Mutex<Log>>, clients: Vec<UnixStream>, rx: mpsc::Receiver<Cmd>) {
    let mut display: Display<State> = Display::new().expect("wayland-server display");
    let dh = display.handle();
    dh.create_global::<State, wl_compositor::WlCompositor, ()>(4, ());
    dh.create_global::<State, wl_shm::WlShm, ()>(cfg.shm_version, ());
    if cfg.fixes {
        dh.create_global::<State, wl_fixes::WlFixes, ()>(1, ());
    }
    for stream in clients {
        display
            .handle()
            .insert_client(stream, Arc::new(Tag))
            .expect("insert client");
    }
    let mut state = State {
        cfg,
        log,
        surfaces: HashMap::new(),
        frames: Vec::new(),
        held: Vec::new(),
    };
    loop {
        // Commands sent before a client request are run before that
        // request is dispatched, so a roundtrip after a command sees it.
        let mut stop = false;
        let mut drain = |state: &mut State| {
            while let Ok(cmd) = rx.try_recv() {
                stop |= matches!(cmd, Cmd::Stop);
                state.run(&cmd);
            }
        };
        drain(&mut state);
        let _ = display.dispatch_clients(&mut state);
        drain(&mut state);
        let _ = display.flush_clients();
        if stop {
            return;
        }
        std::thread::sleep(Duration::from_micros(300));
    }
}

struct Pool {
    map: Mutex<Option<memmap2::Mmap>>,
    fd: OwnedFd,
}

impl Pool {
    fn map(&self, size: i32) {
        let len = usize::try_from(size).unwrap_or(0);
        // SAFETY: test-only read-only mapping of the client's memfd. The
        // presenter seals its memfds against shrinking, so the mapping
        // cannot be truncated under us while it is read at commit time.
        let map = unsafe { memmap2::MmapOptions::new().len(len).map(&self.fd) }.ok();
        *self.map.lock().unwrap_or_else(|e| e.into_inner()) = map;
    }
}

struct BufferData {
    pool: Arc<Pool>,
    offset: i32,
    width: i32,
    height: i32,
    stride: i32,
}

impl BufferData {
    fn pixels(&self) -> Vec<u32> {
        let map = self.pool.map.lock().unwrap_or_else(|e| e.into_inner());
        let Some(map) = map.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity((self.width * self.height) as usize);
        for y in 0..self.height {
            let start = (self.offset + y * self.stride) as usize;
            let row = &map[start..start + self.width as usize * 4];
            out.extend(
                row.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| u32::from_ne_bytes(*b)),
            );
        }
        out
    }
}

impl GlobalDispatch<wl_compositor::WlCompositor, ()> for State {
    fn bind(
        _: &mut Self,
        _: &ServerHandle,
        _: &Client,
        resource: New<wl_compositor::WlCompositor>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_compositor::WlCompositor,
        request: wl_compositor::Request,
        _: &(),
        _: &ServerHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                let surface = init.init(id, ());
                state
                    .surfaces
                    .insert(surface.id(), PendingSurface::default());
            }
            wl_compositor::Request::CreateRegion { id } => {
                init.init(id, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_region::WlRegion, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_region::WlRegion,
        _: wl_region::Request,
        _: &(),
        _: &ServerHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        surface: &wl_surface::WlSurface,
        request: wl_surface::Request,
        _: &(),
        _: &ServerHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        use wl_surface::Request;
        let release_on_commit = state.cfg.release_on_commit;
        let id = surface.id();
        match request {
            Request::Attach { buffer, .. } => {
                state.surfaces.entry(id).or_default().buffer = Some(buffer);
            }
            Request::Damage {
                x,
                y,
                width,
                height,
            }
            | Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                state
                    .surfaces
                    .entry(id)
                    .or_default()
                    .damage
                    .push([x, y, width, height]);
            }
            Request::Frame { callback } => {
                let cb = init.init(callback, ());
                state.surfaces.entry(id).or_default().callbacks.push(cb);
                state.log().callbacks_requested += 1;
            }
            Request::Commit => {
                let pending = std::mem::take(state.surfaces.entry(id).or_default());
                state.frames.extend(pending.callbacks);
                let attached = pending.buffer.is_some();
                let buffer = pending.buffer.flatten();
                let pixels = buffer
                    .as_ref()
                    .and_then(|b| b.data::<BufferData>())
                    .map_or_else(Vec::new, BufferData::pixels);
                state.log().commits.push(Commit {
                    attached,
                    pixels,
                    damage: pending.damage,
                });
                if let Some(b) = buffer {
                    if release_on_commit {
                        b.release();
                    } else {
                        state.held.push(b);
                        state.log().held += 1;
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_callback::WlCallback,
        _: wl_callback::Request,
        _: &(),
        _: &ServerHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl GlobalDispatch<wl_shm::WlShm, ()> for State {
    fn bind(
        state: &mut Self,
        _: &ServerHandle,
        _: &Client,
        resource: New<wl_shm::WlShm>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        let shm = init.init(resource, ());
        shm.format(wl_shm::Format::Argb8888);
        shm.format(wl_shm::Format::Xrgb8888);
        let mut log = state.log();
        log.shm_binds += 1;
        log.shm_versions.push(shm.version());
    }
}

impl Dispatch<wl_shm::WlShm, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_shm::WlShm,
        request: wl_shm::Request,
        _: &(),
        _: &ServerHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_shm::Request::CreatePool { id, fd, size } => {
                let pool = Arc::new(Pool {
                    map: Mutex::new(None),
                    fd,
                });
                pool.map(size);
                init.init(id, pool);
            }
            wl_shm::Request::Release => state.log().shm_releases += 1,
            _ => {}
        }
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, Arc<Pool>> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_shm_pool::WlShmPool,
        request: wl_shm_pool::Request,
        pool: &Arc<Pool>,
        _: &ServerHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_shm_pool::Request::CreateBuffer {
                id,
                offset,
                width,
                height,
                stride,
                ..
            } => {
                init.init(
                    id,
                    BufferData {
                        pool: pool.clone(),
                        offset,
                        width,
                        height,
                        stride,
                    },
                );
            }
            wl_shm_pool::Request::Resize { size } => pool.map(size),
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, BufferData> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Request,
        _: &BufferData,
        _: &ServerHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(state: &mut Self, _: ClientId, buffer: &wl_buffer::WlBuffer, _: &BufferData) {
        let before = state.held.len();
        state.held.retain(|b| b.id() != buffer.id());
        let gone = before - state.held.len();
        state.log().held -= gone;
    }
}

impl GlobalDispatch<wl_fixes::WlFixes, ()> for State {
    fn bind(
        state: &mut Self,
        _: &ServerHandle,
        _: &Client,
        resource: New<wl_fixes::WlFixes>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
        state.log().fixes_binds += 1;
    }
}

impl Dispatch<wl_fixes::WlFixes, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_fixes::WlFixes,
        request: wl_fixes::Request,
        _: &(),
        _: &ServerHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let wl_fixes::Request::DestroyRegistry { .. } = request {
            state.log().registries_destroyed += 1;
        }
    }
}

// --- the client side: the test plays winit ---

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_compositor as c_compositor, wl_registry as c_registry, wl_surface as c_surface,
};
use wayland_client::{Connection, EventQueue, Proxy as _, QueueHandle};

pub struct ClientState;

/// One client connection with a surface on it, as winit would hold them.
pub struct TestClient {
    pub conn: Connection,
    queue: EventQueue<ClientState>,
    pub surface: c_surface::WlSurface,
    _compositor: c_compositor::WlCompositor,
}

impl TestClient {
    /// `None` when libwayland-client cannot be loaded on this host.
    pub fn connect(stream: UnixStream) -> Option<Self> {
        let conn = match Connection::from_socket(stream) {
            Ok(conn) => conn,
            Err(err) => {
                eprintln!("skipping: no libwayland-client ({err})");
                return None;
            }
        };
        let (globals, mut queue) =
            registry_queue_init::<ClientState>(&conn).expect("client registry");
        let qh: QueueHandle<ClientState> = queue.handle();
        let compositor: c_compositor::WlCompositor =
            globals.bind(&qh, 4..=4, ()).expect("wl_compositor");
        let surface = compositor.create_surface(&qh, ());
        queue.roundtrip(&mut ClientState).expect("roundtrip");
        Some(Self {
            conn,
            queue,
            surface,
            _compositor: compositor,
        })
    }

    /// Read and dispatch everything the compositor has sent, as winit's
    /// loop does: events for the presenter's queue wait there until the
    /// presenter dispatches it.
    pub fn roundtrip(&mut self) {
        self.queue.roundtrip(&mut ClientState).expect("roundtrip");
    }

    fn display_ptr(&self) -> NonNull<c_void> {
        NonNull::new(self.conn.backend().display_ptr().cast()).expect("wl_display")
    }

    fn surface_ptr(&self) -> NonNull<c_void> {
        NonNull::new(self.surface.id().as_ptr().cast()).expect("wl_surface")
    }

    /// The window handle a winit window gives: its surface, and its own
    /// connection's display.
    pub fn window(&self) -> Arc<TestWindow> {
        Arc::new(TestWindow {
            surface: self.surface_ptr(),
            display: self.display_ptr(),
            _conn: self.conn.clone(),
        })
    }

    /// A window handle that claims `other`'s display: a handle that lies.
    pub fn window_claiming(&self, other: &TestClient) -> Arc<TestWindow> {
        Arc::new(TestWindow {
            surface: self.surface_ptr(),
            display: other.display_ptr(),
            _conn: self.conn.clone(),
        })
    }

    /// The display handle `owned_display_handle()` gives.
    pub fn display(&self) -> TestDisplay {
        TestDisplay {
            display: self.display_ptr(),
            _conn: self.conn.clone(),
        }
    }
}

pub struct TestWindow {
    surface: NonNull<c_void>,
    display: NonNull<c_void>,
    _conn: Connection,
}

// SAFETY: the pointers name libwayland objects, which are thread-safe; the
// connection they belong to is kept alive by `_conn`.
unsafe impl Send for TestWindow {}
// SAFETY: as above; nothing here is mutated through `&self`.
unsafe impl Sync for TestWindow {}

impl HasWindowHandle for TestWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = RawWindowHandle::Wayland(WaylandWindowHandle::new(self.surface));
        // SAFETY: the surface lives as long as the test client that made
        // this handle, which outlives every presenter in a test.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

impl HasDisplayHandle for TestWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        let raw = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(self.display));
        // SAFETY: `_conn` keeps the display open.
        Ok(unsafe { DisplayHandle::borrow_raw(raw) })
    }
}

pub struct TestDisplay {
    display: NonNull<c_void>,
    _conn: Connection,
}

// SAFETY: see `TestWindow`.
unsafe impl Send for TestDisplay {}
// SAFETY: see `TestWindow`.
unsafe impl Sync for TestDisplay {}

impl HasDisplayHandle for TestDisplay {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        let raw = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(self.display));
        // SAFETY: `_conn` keeps the display open.
        Ok(unsafe { DisplayHandle::borrow_raw(raw) })
    }
}

impl wayland_client::Dispatch<c_registry::WlRegistry, GlobalListContents> for ClientState {
    fn event(
        _: &mut Self,
        _: &c_registry::WlRegistry,
        _: c_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl wayland_client::Dispatch<c_compositor::WlCompositor, ()> for ClientState {
    fn event(
        _: &mut Self,
        _: &c_compositor::WlCompositor,
        _: c_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl wayland_client::Dispatch<c_surface::WlSurface, ()> for ClientState {
    fn event(
        _: &mut Self,
        _: &c_surface::WlSurface,
        _: c_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
