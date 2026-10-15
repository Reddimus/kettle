use std::collections::BTreeMap;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use wayland_client::backend::{Backend, ObjectId};
use wayland_client::protocol::{wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::activation::v1::client::{xdg_activation_token_v1, xdg_activation_v1};
use winit::event_loop::OwnedDisplayHandle;
use winit::raw_window_handle::{
    HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
};
use winit::window::Window;

const ACTIVATION_WITHIN: Duration = Duration::from_secs(2);
const MAX_PENDING: usize = 8;

struct LiveWindow {
    window: Weak<Window>,
    surface: ObjectId,
}

struct Seat {
    proxy: wl_seat::WlSeat,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    mouse_window: Option<u64>,
    key_window: Option<u64>,
}

struct Pending {
    token: xdg_activation_token_v1::XdgActivationTokenV1,
    ready: Option<String>,
    target_seq: u64,
    target: Arc<Window>,
    _source: Arc<Window>,
    deadline: Instant,
}

#[derive(Default)]
struct State {
    activation: Option<xdg_activation_v1::XdgActivationV1>,
    seats: BTreeMap<u32, Seat>,
    windows: BTreeMap<u64, LiveWindow>,
    gestures: super::Gestures,
    pending: BTreeMap<u64, Pending>,
    next_request: u64,
}

/// Protocol objects share Winit's display, but own their event queue. The
/// reader wakes Winit after every socket read, including reads containing
/// only Winit's events. Neither loop can steal the other's only wakeup.
pub(crate) struct Bridge {
    reader: Option<JoinHandle<()>>,
    stop: UnixStream,
    state: State,
    queue: EventQueue<State>,
    _registry: wl_registry::WlRegistry,
    connection: Connection,
    _display: OwnedDisplayHandle,
}

impl Bridge {
    pub(crate) fn new(display: OwnedDisplayHandle, wake: kettle_core::Waker) -> Option<Self> {
        let RawDisplayHandle::Wayland(raw) = display.display_handle().ok()?.as_raw() else {
            return None;
        };
        // SAFETY: the owned display is retained until all protocol objects and
        // the joined reader are gone. This backend never disconnects it.
        let backend = unsafe { Backend::from_foreign_display(raw.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let queue = connection.new_event_queue::<State>();
        let registry = connection.display().get_registry(&queue.handle(), ());
        connection.flush().ok()?;
        let (stop, stopped) = UnixStream::pair().ok()?;
        let read = connection.clone();
        let reader = std::thread::Builder::new()
            .name("kettle-wayland".into())
            .spawn(move || read_events(read, stopped, wake))
            .ok()?;
        Some(Self {
            reader: Some(reader),
            stop,
            state: State::default(),
            queue,
            _registry: registry,
            connection,
            _display: display,
        })
    }

    pub(crate) fn poll(&mut self, live: &BTreeMap<u64, Arc<Window>>) {
        self.state.windows.retain(|seq, entry| {
            live.get(seq).is_some_and(|window| {
                entry
                    .window
                    .upgrade()
                    .is_some_and(|old| Arc::ptr_eq(&old, window))
            })
        });
        for (seq, window) in live {
            if !self.state.windows.contains_key(seq)
                && let Some(surface) = surface(&self.connection, window)
            {
                self.state.windows.insert(
                    *seq,
                    LiveWindow {
                        window: Arc::downgrade(window),
                        surface: surface.id(),
                    },
                );
            }
        }
        if self.queue.dispatch_pending(&mut self.state).is_err() {
            for (_, request) in std::mem::take(&mut self.state.pending) {
                request.token.destroy();
            }
            if let Some(manager) = self.state.activation.take() {
                manager.destroy();
            }
            return;
        }
        let now = Instant::now();
        self.state.pending.retain(|_, request| {
            let live = live
                .get(&request.target_seq)
                .is_some_and(|window| Arc::ptr_eq(window, &request.target));
            if !live || now >= request.deadline {
                request.token.destroy();
                return false;
            }
            if let Some(token) = request.ready.take() {
                if let (Some(manager), Some(target)) = (
                    &self.state.activation,
                    surface(&self.connection, &request.target),
                ) {
                    manager.activate(token, &target);
                }
                request.token.destroy();
                return false;
            }
            true
        });
        let _ = self.connection.flush();
    }

    pub(crate) fn activate(
        &mut self,
        from: u64,
        source: Arc<Window>,
        to: u64,
        target: Arc<Window>,
    ) -> bool {
        if self.state.pending.len() >= MAX_PENDING {
            return false;
        }
        let Some(manager) = self.state.activation.as_ref() else {
            return false;
        };
        let Some((seat, serial)) = self.state.gestures.take(from, Instant::now()) else {
            return false;
        };
        let Some(seat) = self.state.seats.get(&seat) else {
            return false;
        };
        let Some(origin) = surface(&self.connection, &source) else {
            return false;
        };
        let Some(id) = self.state.next_request.checked_add(1) else {
            return false;
        };
        self.state.next_request = id;
        let token = manager.get_activation_token(&self.queue.handle(), id);
        token.set_serial(serial, &seat.proxy);
        token.set_surface(&origin);
        token.commit();
        self.state.pending.insert(
            id,
            Pending {
                token,
                ready: None,
                target_seq: to,
                target,
                _source: source,
                deadline: Instant::now() + ACTIVATION_WITHIN,
            },
        );
        let _ = self.connection.flush();
        true
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.state
            .pending
            .values()
            .map(|request| request.deadline)
            .min()
    }
}

fn surface(connection: &Connection, window: &Window) -> Option<wl_surface::WlSurface> {
    let RawWindowHandle::Wayland(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: the borrowed window remains alive through this call; callers
    // retain an Arc whenever the proxy is used by an asynchronous request.
    let id = unsafe {
        ObjectId::from_ptr(
            wl_surface::WlSurface::interface(),
            handle.surface.as_ptr().cast(),
        )
    }
    .ok()?;
    wl_surface::WlSurface::from_id(connection, id).ok()
}

fn read_events(connection: Connection, stopped: UnixStream, wake: kettle_core::Waker) {
    loop {
        let Some(guard) = connection.prepare_read() else {
            if connection.backend().dispatch_inner_queue().is_err() {
                wake();
                break;
            }
            wake();
            continue;
        };
        let mut fds = [
            libc::pollfd {
                fd: connection.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stopped.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: both borrowed descriptors and the writable array stay live
        // until poll returns. Closing the control writer interrupts shutdown.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
        if result < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if result < 0 || fds[1].revents != 0 {
            break;
        }
        match guard.read() {
            Ok(_) => {}
            Err(wayland_client::backend::WaylandError::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => {
                wake();
                break;
            }
        }
        if connection.backend().dispatch_inner_queue().is_err() {
            wake();
            break;
        }
        wake();
    }
}

impl State {
    fn window(&self, surface: &wl_surface::WlSurface) -> Option<u64> {
        self.windows.iter().find_map(|(&seq, entry)| {
            (entry.surface.protocol_id() == surface.id().protocol_id()
                && entry.window.upgrade().is_some())
            .then_some(seq)
        })
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "xdg_activation_v1" if state.activation.is_none() => {
                    state.activation = Some(registry.bind(name, version.min(1), qh, ()))
                }
                "wl_seat" if state.seats.len() < super::MAX_SEATS => {
                    let proxy = registry.bind(name, version.min(7), qh, name);
                    state.seats.insert(
                        name,
                        Seat {
                            proxy,
                            pointer: None,
                            keyboard: None,
                            mouse_window: None,
                            key_window: None,
                        },
                    );
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(seat) = state.seats.remove(&name) {
                    seat.destroy();
                }
                state.gestures.0.remove(&name);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        name: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let Some(seat) = state.seats.get_mut(name) else {
            return;
        };
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Pointer) {
                if seat.pointer.is_none() {
                    seat.pointer = Some(seat.proxy.get_pointer(qh, *name));
                }
            } else if let Some(pointer) = seat.pointer.take() {
                if pointer.version() >= 3 {
                    pointer.release();
                }
                seat.mouse_window = None;
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                if seat.keyboard.is_none() {
                    seat.keyboard = Some(seat.proxy.get_keyboard(qh, *name));
                }
            } else if let Some(keyboard) = seat.keyboard.take() {
                if keyboard.version() >= 3 {
                    keyboard.release();
                }
                seat.key_window = None;
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let entered = if let wl_pointer::Event::Enter { ref surface, .. } = event {
            state.window(surface)
        } else {
            None
        };
        let Some(seat) = state.seats.get_mut(name) else {
            return;
        };
        match event {
            wl_pointer::Event::Enter { .. } => seat.mouse_window = entered,
            wl_pointer::Event::Leave { .. } => seat.mouse_window = None,
            wl_pointer::Event::Button {
                serial,
                state: WEnum::Value(wl_pointer::ButtonState::Pressed),
                ..
            } => {
                if let Some(window) = seat.mouse_window {
                    state.gestures.press(*name, window, serial, Instant::now());
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let entered = if let wl_keyboard::Event::Enter { ref surface, .. } = event {
            state.window(surface)
        } else {
            None
        };
        let Some(seat) = state.seats.get_mut(name) else {
            return;
        };
        match event {
            wl_keyboard::Event::Enter { .. } => seat.key_window = entered,
            wl_keyboard::Event::Leave { .. } => seat.key_window = None,
            wl_keyboard::Event::Key {
                serial,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => {
                if let Some(window) = seat.key_window {
                    state.gestures.press(*name, window, serial, Instant::now());
                }
            }
            // Dropping Keymap closes its owned descriptor; no text is read.
            _ => {}
        }
    }
}

impl Dispatch<xdg_activation_v1::XdgActivationV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &xdg_activation_v1::XdgActivationV1,
        _: xdg_activation_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_activation_token_v1::XdgActivationTokenV1, u64> for State {
    fn event(
        state: &mut Self,
        _: &xdg_activation_token_v1::XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event
            && let Some(request) = state.pending.get_mut(id)
        {
            request.ready = Some(token);
        }
    }
}

impl Seat {
    fn destroy(self) {
        if let Some(pointer) = self.pointer
            && pointer.version() >= 3
        {
            pointer.release();
        }
        if let Some(keyboard) = self.keyboard
            && keyboard.version() >= 3
        {
            keyboard.release();
        }
        if self.proxy.version() >= 5 {
            self.proxy.release();
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.stop.write_all(&[0]);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        for (_, request) in std::mem::take(&mut self.state.pending) {
            request.token.destroy();
        }
        for (_, seat) in std::mem::take(&mut self.state.seats) {
            seat.destroy();
        }
        if let Some(manager) = self.state.activation.take() {
            manager.destroy();
        }
        let _ = self.connection.flush();
    }
}
