//! Bridges `alacritty_terminal` events to the UI thread.

use std::sync::Arc;

pub use alacritty_terminal::event::Event as TermEvent;
use alacritty_terminal::event::EventListener;
use crossbeam_channel::Sender;

/// Wakes the UI event loop (a winit `EventLoopProxy` is plugged in here).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Implements `EventListener`; every terminal event is forwarded to the UI
/// channel and the event loop is woken.
#[derive(Clone)]
pub struct EventProxy {
    tx: Sender<TermEvent>,
    waker: Waker,
}

impl EventProxy {
    pub fn new(tx: Sender<TermEvent>, waker: Waker) -> Self {
        Self { tx, waker }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TermEvent) {
        let _ = self.tx.send(event);
        (self.waker)();
    }
}
