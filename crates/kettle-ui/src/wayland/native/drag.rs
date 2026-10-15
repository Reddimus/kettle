use super::*;
use wayland_client::protocol::{
    wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source,
};
use wayland_protocols::xdg::shell::client::xdg_toplevel;
use wayland_protocols::xdg::toplevel_drag::v1::client::{
    xdg_toplevel_drag_manager_v1, xdg_toplevel_drag_v1,
};
use winit::platform::wayland::WindowExtWayland;

const ROOT_DROP: &str = "application/x-rootwindow-drop";
const FINISH_WITHIN: Duration = Duration::from_secs(2);
const MAX_OFFERS: usize = 16;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Point {
    pub(crate) window: u64,
    pub(crate) x: f64,
    pub(crate) y: f64,
}

pub(crate) enum Update {
    Hover(Option<Point>),
    End {
        performed: bool,
        drop: Option<Point>,
        activation: Option<DragActivation>,
    },
}

pub(crate) struct DragActivation {
    seat: u32,
    serial: u32,
    at: Instant,
}

struct Active {
    id: u64,
    mime: String,
    source: wl_data_source::WlDataSource,
    extension: Option<xdg_toplevel_drag_v1::XdgToplevelDragV1>,
    expected_carrier: Option<u64>,
    _origin: Arc<Window>,
    restore_decorations: bool,
    seat: u32,
    serial: u32,
    carrier: Option<Arc<Window>>,
    grab: (f64, f64),
    performed: bool,
    cancelled: bool,
    finished: bool,
    deadline: Option<Instant>,
    drop: Option<Point>,
}

struct Offer {
    proxy: wl_data_offer::WlDataOffer,
    internal: Option<u64>,
    uri_list: bool,
    action: wl_data_device_manager::DndAction,
}

struct Entered {
    offer: u32,
    point: Option<Point>,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) data_manager: Option<wl_data_device_manager::WlDataDeviceManager>,
    pub(super) manager: Option<xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1>,
    pub(super) files: super::files::Files,
    devices: BTreeMap<u32, wl_data_device::WlDataDevice>,
    offers: BTreeMap<u32, Offer>,
    entered: BTreeMap<u32, Entered>,
    active: Option<Active>,
    ended: Option<Update>,
    next_id: u64,
}

impl State {
    pub(super) fn ensure_devices(
        &mut self,
        seats: &BTreeMap<u32, Seat>,
        qh: &QueueHandle<super::State>,
    ) {
        if let Some(manager) = &self.data_manager {
            for (&id, seat) in seats {
                self.devices
                    .entry(id)
                    .or_insert_with(|| manager.get_data_device(&seat.proxy, qh, id));
            }
        }
    }

    pub(super) fn remove_seat(&mut self, seat: u32) {
        self.leave(seat);
        if let Some(device) = self.devices.remove(&seat) {
            device.release();
        }
    }

    fn leave(&mut self, seat: u32) {
        if let Some(entered) = self.entered.remove(&seat)
            && let Some(offer) = self.offers.remove(&entered.offer)
        {
            offer.proxy.destroy();
        }
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.active
            .as_ref()
            .and_then(|active| active.deadline)
            .into_iter()
            .chain(self.files.deadline())
            .min()
    }

    pub(super) fn close(&mut self) {
        self.files.close();
        for seat in self.entered.keys().copied().collect::<Vec<_>>() {
            self.leave(seat);
        }
        for (_, offer) in std::mem::take(&mut self.offers) {
            offer.proxy.destroy();
        }
        if let Some(active) = self.active.as_mut() {
            active.cancelled = true;
        }
        self.poll();
        for (_, device) in std::mem::take(&mut self.devices) {
            device.release();
        }
        if let Some(manager) = self.manager.take() {
            manager.destroy();
        }
    }

    pub(super) fn poll(&mut self) {
        self.files.poll();
        if self.active.as_ref().is_some_and(|active| {
            active.cancelled
                || active.finished
                || active.deadline.is_some_and(|end| Instant::now() >= end)
        }) {
            let active = self.active.take().unwrap();
            // The extension ends at performed/cancelled, before the data source.
            if let Some(extension) = active.extension {
                extension.destroy();
            }
            active.source.destroy();
            if active.restore_decorations
                && let Some(window) = &active.carrier
            {
                window.set_decorations(true);
            }
            self.ended = Some(Update::End {
                performed: active.performed,
                drop: active.drop,
                activation: (active.performed && active.drop.is_some()).then_some(DragActivation {
                    seat: active.seat,
                    serial: active.serial,
                    at: Instant::now(),
                }),
            });
        }
    }
}

impl Bridge {
    pub(crate) fn begin_drag(
        &mut self,
        seq: u64,
        origin: Arc<Window>,
        carrier_seq: Option<u64>,
        grab: (f64, f64),
    ) -> bool {
        let state = &mut self.state;
        if state.drag.active.is_some() || state.drag.ended.is_some() {
            return false;
        }
        let (Some(manager), Some(data)) = (&state.drag.manager, &state.drag.data_manager) else {
            return false;
        };
        // Winit's pointer events do not identify their seat. Never guess when
        // more than one pointer seat can have supplied this motion.
        let mut seats = state
            .seats
            .iter()
            .filter(|(_, seat)| seat.pointer.is_some());
        let Some((&seat_id, _)) = seats.next() else {
            return false;
        };
        if seats.next().is_some() {
            return false;
        }
        let Some(device) = state.drag.devices.get(&seat_id) else {
            return false;
        };
        let Some(surface) = surface(&self.connection, &origin) else {
            return false;
        };
        let top = if carrier_seq.is_none() {
            let Some(top) = toplevel(&self.connection, &origin) else {
                return false;
            };
            Some(top)
        } else {
            None
        };
        let mut random = [0u8; 16];
        if getrandom::fill(&mut random).is_err() {
            return false;
        }
        let Some(id) = state.drag.next_id.checked_add(1) else {
            return false;
        };
        let Some(serial) = state.primary.take(seat_id, seq) else {
            return false;
        };
        state.drag.next_id = id;
        let mime = format!(
            "application/x-kettle-tab-{}",
            random
                .iter()
                .map(|v| format!("{v:02x}"))
                .collect::<String>()
        );
        let source = data.create_data_source(&self.queue.handle(), id);
        source.offer(mime.clone());
        source.offer(ROOT_DROP.into());
        source.set_actions(wl_data_device_manager::DndAction::Move);
        let extension = manager.get_xdg_toplevel_drag(&source, &self.queue.handle(), ());
        // A mapped carrier's CSD subsurfaces can intercept its drop after
        // Mutter restores a tiled window. Keep only our tab strip while dragging.
        let restore_decorations = top.is_some() && origin.is_decorated();
        if restore_decorations {
            origin.set_decorations(false);
        }
        device.start_drag(Some(&source), &surface, None, serial);
        // Mutter needs the source's seat before attaching a mapped window.
        // The extension factory still precedes start_drag, as the protocol requires.
        if let Some(top) = top {
            let (x, y) = offset(&origin, grab);
            extension.attach(&top, x, y);
        }
        state.drag.active = Some(Active {
            id,
            mime,
            source,
            extension: Some(extension),
            expected_carrier: carrier_seq,
            _origin: origin.clone(),
            restore_decorations,
            seat: seat_id,
            serial,
            carrier: carrier_seq.is_none().then_some(origin),
            grab,
            performed: false,
            cancelled: false,
            finished: false,
            deadline: None,
            drop: None,
        });
        let _ = self.connection.flush();
        true
    }

    // Only a completed owned drag creates this one-use permit. The fresh drop,
    // rather than the age of its still-held original press, authorizes focus.
    pub(crate) fn activate_drag(
        &mut self,
        permit: DragActivation,
        to: u64,
        target: Arc<Window>,
    ) -> bool {
        if permit.at.elapsed() >= ACTIVATION_WITHIN {
            return false;
        }
        // Pointer focus has moved to the drop recipient. Mutter checks that
        // surface against the original grab serial when activation is applied.
        self.activate_with_serial(permit.seat, permit.serial, target.clone(), to, target)
    }

    pub(crate) fn abandon_drag_carrier(&mut self) {
        if let Some(active) = self.state.drag.active.as_mut() {
            active.expected_carrier = None;
        }
    }

    /// Called before the new carrier's first buffer is presented.
    pub(crate) fn attach_drag_carrier(&mut self, seq: u64, window: Arc<Window>) {
        if self.queue.dispatch_pending(&mut self.state).is_err() {
            return;
        }
        let Some(active) = self.state.drag.active.as_mut() else {
            return;
        };
        if active.expected_carrier != Some(seq)
            || active.carrier.is_some()
            || active.performed
            || active.cancelled
        {
            return;
        }
        if let (Some(top), Some(extension)) =
            (toplevel(&self.connection, &window), &active.extension)
        {
            let (x, y) = offset(&window, active.grab);
            extension.attach(&top, x, y);
            active.carrier = Some(window);
            let _ = self.connection.flush();
        }
    }

    pub(crate) fn drag_update(&mut self) -> Option<Update> {
        if let Some(ended) = self.state.drag.ended.take() {
            return Some(ended);
        }
        let active = self.state.drag.active.as_ref()?;
        let point = self.state.drag.entered.values().find_map(|entered| {
            (self.state.drag.offers.get(&entered.offer)?.internal == Some(active.id))
                .then_some(entered.point)
                .flatten()
        });
        Some(Update::Hover(point))
    }
}

fn offset(window: &Window, grab: (f64, f64)) -> (i32, i32) {
    let scale = window.scale_factor();
    let header = window
        .outer_size()
        .height
        .saturating_sub(window.inner_size().height);
    (
        (grab.0 / scale).round() as i32,
        ((grab.1 + f64::from(header)) / scale).round() as i32,
    )
}

fn toplevel(connection: &Connection, window: &Window) -> Option<xdg_toplevel::XdgToplevel> {
    let pointer = window.xdg_toplevel()?;
    // SAFETY: the window Arc is retained for the protocol operation's lifetime.
    let id = unsafe {
        ObjectId::from_ptr(
            xdg_toplevel::XdgToplevel::interface(),
            pointer.as_ptr().cast(),
        )
    }
    .ok()?;
    xdg_toplevel::XdgToplevel::from_id(connection, id).ok()
}

impl Dispatch<wl_data_device::WlDataDevice, u32> for super::State {
    fn event(
        state: &mut Self,
        _: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        seat: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => {
                if state.drag.offers.len() < MAX_OFFERS {
                    state.drag.offers.insert(
                        id.id().protocol_id(),
                        Offer {
                            proxy: id,
                            internal: None,
                            uri_list: false,
                            action: wl_data_device_manager::DndAction::None,
                        },
                    );
                } else {
                    id.destroy();
                }
            }
            wl_data_device::Event::Enter {
                serial,
                surface,
                x,
                y,
                id,
            } => {
                state.drag.leave(*seat);
                if let Some(id) = id {
                    let key = id.id().protocol_id();
                    let point = state.window(&surface).map(|window| Point { window, x, y });
                    if let Some(offer) = state.drag.offers.get(&key) {
                        let mime = state
                            .drag
                            .active
                            .as_ref()
                            .filter(|active| Some(active.id) == offer.internal)
                            .map(|active| active.mime.clone());
                        let uri = mime.is_none() && offer.uri_list && point.is_some();
                        let accepted = mime
                            .clone()
                            .or_else(|| uri.then(|| super::files::URI_LIST.into()));
                        offer.proxy.accept(serial, accepted);
                        let action = if mime.is_some() {
                            wl_data_device_manager::DndAction::Move
                        } else if uri {
                            wl_data_device_manager::DndAction::Copy
                        } else {
                            wl_data_device_manager::DndAction::None
                        };
                        offer.proxy.set_actions(action, action);
                        state
                            .drag
                            .entered
                            .insert(*seat, Entered { offer: key, point });
                    }
                }
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                if let Some(entered) = state.drag.entered.get_mut(seat)
                    && let Some(point) = entered.point.as_mut()
                {
                    point.x = x;
                    point.y = y;
                }
            }
            wl_data_device::Event::Leave => state.drag.leave(*seat),
            wl_data_device::Event::Drop => {
                if let Some(entered) = state.drag.entered.remove(seat)
                    && let Some(offer) = state.drag.offers.remove(&entered.offer)
                {
                    if let Some(active) = state.drag.active.as_mut()
                        && offer.internal == Some(active.id)
                        && offer.action == wl_data_device_manager::DndAction::Move
                    {
                        log::debug!(
                            "Wayland internal tab drop has target: {}",
                            entered.point.is_some()
                        );
                        active.drop = entered.point;
                        offer.proxy.finish();
                    }
                    if offer.internal.is_none()
                        && offer.uri_list
                        && offer.action == wl_data_device_manager::DndAction::Copy
                        && let (Some(point), Some(wake)) = (entered.point, state.wake.clone())
                    {
                        state.drag.files.start(offer.proxy, point.window, wake);
                    } else {
                        offer.proxy.destroy();
                    }
                }
            }
            wl_data_device::Event::Selection { id: Some(id) } => {
                if let Some(offer) = state.drag.offers.remove(&id.id().protocol_id()) {
                    offer.proxy.destroy();
                }
            }
            _ => {}
        }
    }
    wayland_client::event_created_child!(super::State, wl_data_device::WlDataDevice, [0 => (wl_data_offer::WlDataOffer, ())]);
}

impl Dispatch<wl_data_offer::WlDataOffer, ()> for super::State {
    fn event(
        state: &mut Self,
        proxy: &wl_data_offer::WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(offer) = state.drag.offers.get_mut(&proxy.id().protocol_id()) else {
            return;
        };
        match event {
            wl_data_offer::Event::Offer { mime_type } => {
                if mime_type == super::files::URI_LIST {
                    offer.uri_list = true;
                }
                if let Some(active) = &state.drag.active
                    && mime_type == active.mime
                {
                    offer.internal = Some(active.id);
                }
            }
            wl_data_offer::Event::Action {
                dnd_action: WEnum::Value(action),
            } => offer.action = action,
            _ => {}
        }
    }
}

impl Dispatch<wl_data_source::WlDataSource, u64> for super::State {
    fn event(
        state: &mut Self,
        _: &wl_data_source::WlDataSource,
        event: wl_data_source::Event,
        id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(active) = state.drag.active.as_mut().filter(|active| active.id == *id) else {
            return;
        };
        match event {
            // Closing the owned Send descriptor transfers no tab data.
            wl_data_source::Event::Send { .. } => {}
            wl_data_source::Event::DndDropPerformed => {
                log::debug!("Wayland tab drag released");
                active.performed = true;
                active.deadline = Some(Instant::now() + FINISH_WITHIN);
            }
            wl_data_source::Event::Cancelled => {
                log::debug!("Wayland tab drag cancelled");
                active.cancelled = true;
            }
            wl_data_source::Event::DndFinished => active.finished = true,
            _ => {}
        }
        if (active.performed || active.cancelled)
            && let Some(extension) = active.extension.take()
        {
            extension.destroy();
        }
    }
}

wayland_client::delegate_noop!(super::State: ignore wl_data_device_manager::WlDataDeviceManager);
wayland_client::delegate_noop!(super::State: ignore xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1);
wayland_client::delegate_noop!(super::State: ignore xdg_toplevel_drag_v1::XdgToplevelDragV1);
