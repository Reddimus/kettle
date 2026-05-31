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

#[cfg(test)]
mod tests {
    use super::{EventListener, EventProxy, TermEvent};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn forwards_event_to_channel_and_wakes_loop() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let woke = Arc::new(AtomicUsize::new(0));
        let woke2 = woke.clone();
        let proxy = EventProxy::new(
            tx,
            Arc::new(move || {
                woke2.fetch_add(1, Ordering::SeqCst);
            }),
        );

        proxy.send_event(TermEvent::Wakeup);
        proxy.send_event(TermEvent::Bell);

        // Both events arrive on the channel in order…
        assert!(matches!(rx.try_recv(), Ok(TermEvent::Wakeup)));
        assert!(matches!(rx.try_recv(), Ok(TermEvent::Bell)));
        assert!(rx.try_recv().is_err());
        // …and the waker fired once per event.
        assert_eq!(woke.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn send_after_receiver_dropped_does_not_panic() {
        // The `let _ = self.tx.send(...)` swallows a closed-channel error,
        // so a terminal that outlives the UI receiver can't crash on its
        // next event — it just wakes a loop that no longer listens.
        let (tx, rx) = crossbeam_channel::unbounded();
        let proxy = EventProxy::new(tx, Arc::new(|| {}));
        drop(rx);
        proxy.send_event(TermEvent::Wakeup); // must not panic
    }
}
