//! Main-thread handoff for bare-launch activation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver as Confirmation, SyncSender};
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
    pending: Arc<AtomicBool>,
}

impl PendingActivation {
    pub(crate) fn claim(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }
}

fn wait_for_confirmation(
    result: Confirmation<bool>,
    pending: &AtomicBool,
    timeout: Duration,
) -> bool {
    match result.recv_timeout(timeout) {
        Ok(opened) => opened,
        Err(_) => {
            pending.store(false, Ordering::Release);
            false
        }
    }
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
            let pending = Arc::new(AtomicBool::new(true));
            if sender
                .try_send(PendingActivation {
                    request,
                    completion,
                    pending: Arc::clone(&pending),
                })
                .is_err()
            {
                return false;
            }
            if proxy.send_event(UserEvent::Activation).is_err() {
                pending.store(false, Ordering::Release);
                return false;
            }
            wait_for_confirmation(result, &pending, UI_CONFIRM_TIMEOUT)
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

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_ctl::activation::LaunchIdentity;

    #[test]
    fn timed_out_activations_are_not_opened_when_the_inbox_drains() {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        let (completion, result) = std::sync::mpsc::sync_channel(1);
        let pending = Arc::new(AtomicBool::new(true));
        assert!(
            sender
                .try_send(PendingActivation {
                    request: ActivationRequest::new(None, LaunchIdentity::default()),
                    completion,
                    pending: Arc::clone(&pending),
                })
                .is_ok()
        );
        let inbox = ActivationInbox { receiver };

        assert!(!wait_for_confirmation(result, &pending, Duration::ZERO));
        let mut opened = 0;
        for request in inbox.drain() {
            if request.claim() {
                opened += 1;
            }
        }
        assert_eq!(opened, 0);
    }

    #[test]
    fn a_live_activation_can_be_claimed_once_and_confirmed() {
        let (completion, result) = std::sync::mpsc::sync_channel(1);
        let pending = Arc::new(AtomicBool::new(true));
        let request = PendingActivation {
            request: ActivationRequest::new(None, LaunchIdentity::default()),
            completion,
            pending: Arc::clone(&pending),
        };

        assert!(request.claim());
        assert!(!request.claim());
        request.completion.send(true).unwrap();
        assert!(wait_for_confirmation(result, &pending, Duration::ZERO));
    }
}
