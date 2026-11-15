//! PRIMARY through `zwp_primary_selection_device_manager_v1`: one device per
//! seat beside the data device, read for a middle-click paste and written
//! when text is selected. Text types only. A compositor without the protocol
//! leaves this module idle and PRIMARY to the caller's fallback.
use super::selection::{self, PrimarySource};
use super::*;
use crate::wayland::offer::{Kinds, TEXT_MIMES};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
    zwp_primary_selection_device_v1::{self, ZwpPrimarySelectionDeviceV1},
    zwp_primary_selection_offer_v1::{self, ZwpPrimarySelectionOfferV1},
    zwp_primary_selection_source_v1::{self, ZwpPrimarySelectionSourceV1},
};

pub(super) type Manager = ZwpPrimarySelectionDeviceManagerV1;

/// Offers introduced and not yet named by a `selection` event.
const MAX_OFFERS: usize = 16;

#[derive(Default)]
pub(super) struct State {
    pub(super) manager: Option<ZwpPrimarySelectionDeviceManagerV1>,
    devices: BTreeMap<u32, ZwpPrimarySelectionDeviceV1>,
    offers: BTreeMap<u32, (ZwpPrimarySelectionOfferV1, Kinds)>,
}

impl State {
    pub(super) fn ensure_devices(
        &mut self,
        seats: &BTreeMap<u32, Seat>,
        qh: &QueueHandle<super::State>,
    ) {
        if let Some(manager) = &self.manager {
            for (&id, seat) in seats {
                self.devices
                    .entry(id)
                    .or_insert_with(|| manager.get_device(&seat.proxy, qh, id));
            }
        }
    }

    pub(super) fn device(&self, seat: u32) -> Option<&ZwpPrimarySelectionDeviceV1> {
        self.devices.get(&seat)
    }

    pub(super) fn remove_seat(&mut self, seat: u32) {
        if let Some(device) = self.devices.remove(&seat) {
            device.destroy();
        }
    }

    pub(super) fn close(&mut self) {
        for (_, (offer, _)) in std::mem::take(&mut self.offers) {
            offer.destroy();
        }
        for (_, device) in std::mem::take(&mut self.devices) {
            device.destroy();
        }
        if let Some(manager) = self.manager.take() {
            manager.destroy();
        }
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceV1, u32> for super::State {
    fn event(
        state: &mut Self,
        _: &ZwpPrimarySelectionDeviceV1,
        event: zwp_primary_selection_device_v1::Event,
        _: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let primary = &mut state.primary_selection;
        match event {
            zwp_primary_selection_device_v1::Event::DataOffer { offer } => {
                if primary.offers.len() < MAX_OFFERS {
                    primary
                        .offers
                        .insert(offer.id().protocol_id(), (offer, Kinds::default()));
                } else {
                    offer.destroy();
                }
            }
            zwp_primary_selection_device_v1::Event::Selection { id: Some(id) } => {
                match primary.offers.remove(&id.id().protocol_id()) {
                    Some((offer, kinds)) if kinds.text.is_some() => {
                        selection::shared().primary_offer(Some((offer, kinds)));
                    }
                    Some((offer, _)) => {
                        offer.destroy();
                        selection::shared().primary_offer(None);
                    }
                    None => selection::shared().primary_offer(None),
                }
            }
            zwp_primary_selection_device_v1::Event::Selection { id: None } => {
                selection::shared().primary_offer(None);
            }
            _ => {}
        }
    }
    wayland_client::event_created_child!(super::State, ZwpPrimarySelectionDeviceV1, [0 => (ZwpPrimarySelectionOfferV1, ())]);
}

impl Dispatch<ZwpPrimarySelectionOfferV1, ()> for super::State {
    fn event(
        state: &mut Self,
        proxy: &ZwpPrimarySelectionOfferV1,
        event: zwp_primary_selection_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_primary_selection_offer_v1::Event::Offer { mime_type } = event
            && let Some((_, kinds)) = state
                .primary_selection
                .offers
                .get_mut(&proxy.id().protocol_id())
        {
            kinds.note(&mime_type);
            *kinds = kinds.text_only();
        }
    }
}

impl Dispatch<ZwpPrimarySelectionSourceV1, PrimarySource> for super::State {
    fn event(
        _: &mut Self,
        _: &ZwpPrimarySelectionSourceV1,
        event: zwp_primary_selection_source_v1::Event,
        id: &PrimarySource,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_primary_selection_source_v1::Event::Send { mime_type, fd } => {
                if TEXT_MIMES.contains(&mime_type.as_str())
                    && let Some(text) = selection::shared().primary_text_for(id.0)
                {
                    selection::send(text, fd);
                }
            }
            zwp_primary_selection_source_v1::Event::Cancelled => {
                selection::shared().primary_cancelled(id.0);
            }
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(super::State: ignore ZwpPrimarySelectionDeviceManagerV1);
