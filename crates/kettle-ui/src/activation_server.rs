//! Main-thread handoff for bare-launch activation.

use std::sync::mpsc::SyncSender;
use std::time::Duration;

use crossbeam_channel::{Receiver, TryRecvError};
use kettle_ctl::activation::{ActivationRequest, PrimaryHandle};
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

const MAX_PENDING_ACTIVATIONS: usize = 32;
const UI_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct PendingActivation {
    pub(crate) request: ActivationRequest,
    pub(crate) completion: SyncSender<bool>,
}

pub(crate) struct ActivationInbox {
    receiver: Receiver<PendingActivation>,
}

impl ActivationInbox {
    pub(crate) fn start(
        primary: PrimaryHandle,
        proxy: EventLoopProxy<UserEvent>,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = crossbeam_channel::bounded(MAX_PENDING_ACTIVATIONS);
        kettle_ctl::activation::spawn_server(primary, move |request| {
            let (completion, result) = std::sync::mpsc::sync_channel(1);
            if sender
                .try_send(PendingActivation {
                    request,
                    completion,
                })
                .is_err()
            {
                return false;
            }
            if proxy.send_event(UserEvent::Activation).is_err() {
                return false;
            }
            result.recv_timeout(UI_CONFIRM_TIMEOUT).unwrap_or(false)
        })?;
        Ok(Self { receiver })
    }

    pub(crate) fn drain(&self) -> Vec<PendingActivation> {
        let mut pending = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(request) => pending.push(request),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return pending,
            }
        }
    }
}
