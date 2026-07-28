//! Bridges `alacritty_terminal` events to the UI thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

pub use alacritty_terminal::event::Event as TermEvent;
use alacritty_terminal::event::EventListener;
use crossbeam_channel::Sender;

const UPSTREAM_PRIMARY_DA_REPLY: &str = "\x1b[?6c";

/// Primary Device Attributes advertised by Kettle.
///
/// `6` keeps the VT102/VT2xx-compatible identity that existing programs expect
/// and `4` advertises sixel graphics. Extension `52` advertises OSC 52
/// clipboard writes only while the live policy and platform clipboard allow
/// them, as required by the clipboard-extension capability contract.
pub(crate) const PRIMARY_DA_REPLY: &str = "\x1b[?6;4;52c";
pub(crate) const PRIMARY_DA_REPLY_NO_CLIPBOARD: &str = "\x1b[?6;4c";

/// Wakes the UI event loop (a winit `EventLoopProxy` is plugged in here).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

const OUTPUT_WAKE_ENABLED: u8 = 1 << 0;
const OUTPUT_WAKE_PENDING: u8 = 1 << 1;
const OUTPUT_WAKE_DIRTY: u8 = 1 << 2;

/// Per-pane output-wakeup state.
///
/// A hidden/minimized pane keeps parsing into its terminal grid, but publishing
/// one event-loop wake per PTY read would burn CPU while no frame can present.
/// This gate coalesces every output notification into one pending wake while
/// renderable, and into one dirty bit while quiescent. Re-enabling converts that
/// dirty bit into exactly one wake, so the first restore frame cannot miss
/// output accumulated while hidden.
pub struct OutputWakeGate {
    state: AtomicU8,
    waker: Waker,
}

impl OutputWakeGate {
    pub fn new(waker: Waker) -> Self {
        Self {
            state: AtomicU8::new(OUTPUT_WAKE_ENABLED),
            waker,
        }
    }

    /// Publish output damage. Returns `true` only when the caller owns a newly
    /// queued wake; duplicate output while that wake is pending is coalesced.
    pub fn request(&self) -> bool {
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            let (next, should_wake) = if current & OUTPUT_WAKE_ENABLED != 0 {
                if current & OUTPUT_WAKE_PENDING != 0 {
                    return false;
                }
                (current | OUTPUT_WAKE_PENDING, true)
            } else {
                (current | OUTPUT_WAKE_DIRTY, false)
            };
            match self.state.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if should_wake {
                        (self.waker)();
                    }
                    return should_wake;
                }
                Err(observed) => current = observed,
            }
        }
    }

    /// Re-open the per-pane latch after the UI proves a queued wake stale or
    /// immediately before it snapshots a real frame. Output racing after this
    /// acknowledgement owns a fresh wake.
    pub fn acknowledge(&self) {
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            let next = if current & OUTPUT_WAKE_ENABLED != 0 {
                current & !OUTPUT_WAKE_PENDING
            } else {
                let had_pending = current & OUTPUT_WAKE_PENDING != 0;
                (current & !OUTPUT_WAKE_PENDING) | if had_pending { OUTPUT_WAKE_DIRTY } else { 0 }
            };
            if next == current {
                return;
            }
            match self.state.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    /// Enable or quiesce output wakes. A pending wake converted to hidden state
    /// becomes dirty; a hidden dirty burst converted to visible state publishes
    /// one wake.
    pub fn set_enabled(&self, enabled: bool) {
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            let (next, should_wake) = if enabled {
                if current & OUTPUT_WAKE_ENABLED != 0 {
                    return;
                }
                let dirty = current & OUTPUT_WAKE_DIRTY != 0;
                let next = (current | OUTPUT_WAKE_ENABLED) & !OUTPUT_WAKE_DIRTY;
                if dirty && next & OUTPUT_WAKE_PENDING == 0 {
                    (next | OUTPUT_WAKE_PENDING, true)
                } else {
                    (next, false)
                }
            } else {
                if current & OUTPUT_WAKE_ENABLED == 0 {
                    return;
                }
                let had_pending = current & OUTPUT_WAKE_PENDING != 0;
                let next = current & !(OUTPUT_WAKE_ENABLED | OUTPUT_WAKE_PENDING);
                (
                    next | if had_pending { OUTPUT_WAKE_DIRTY } else { 0 },
                    false,
                )
            };
            match self.state.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if should_wake {
                        (self.waker)();
                    }
                    return;
                }
                Err(observed) => current = observed,
            }
        }
    }
}

/// Implements `EventListener`; semantic terminal events are forwarded to the
/// UI channel and wake the loop. Engine-only `Wakeup` is suppressed because
/// the PTY reader publishes its generation-ordered output wake separately.
#[derive(Clone)]
pub struct EventProxy {
    tx: Sender<TermEvent>,
    waker: Waker,
    output_wake: Option<Arc<OutputWakeGate>>,
    osc52_copy_allowed: Arc<AtomicBool>,
    overflowed: Arc<AtomicBool>,
}

impl EventProxy {
    pub fn new(tx: Sender<TermEvent>, waker: Waker) -> Self {
        Self {
            tx,
            waker,
            output_wake: None,
            // Compatibility constructor: callers that do not provide a live
            // policy retain Kettle's default copy-enabled behavior.
            osc52_copy_allowed: Arc::new(AtomicBool::new(true)),
            overflowed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_output_wake(
        tx: Sender<TermEvent>,
        waker: Waker,
        output_wake: Arc<OutputWakeGate>,
    ) -> Self {
        Self::with_output_wake_and_osc52(tx, waker, output_wake, Arc::new(AtomicBool::new(true)))
    }

    pub fn with_output_wake_and_osc52(
        tx: Sender<TermEvent>,
        waker: Waker,
        output_wake: Arc<OutputWakeGate>,
        osc52_copy_allowed: Arc<AtomicBool>,
    ) -> Self {
        Self::with_output_wake_osc52_and_overflow(
            tx,
            waker,
            output_wake,
            osc52_copy_allowed,
            Arc::new(AtomicBool::new(false)),
        )
    }

    pub fn with_output_wake_osc52_and_overflow(
        tx: Sender<TermEvent>,
        waker: Waker,
        output_wake: Arc<OutputWakeGate>,
        osc52_copy_allowed: Arc<AtomicBool>,
        overflowed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            tx,
            waker,
            output_wake: Some(output_wake),
            osc52_copy_allowed,
            overflowed,
        }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TermEvent) {
        if self.overflowed.load(Ordering::Acquire) {
            return;
        }
        let event = match event {
            TermEvent::PtyWrite(s) if s == UPSTREAM_PRIMARY_DA_REPLY => TermEvent::PtyWrite(
                if self.osc52_copy_allowed.load(Ordering::Acquire) {
                    PRIMARY_DA_REPLY
                } else {
                    PRIMARY_DA_REPLY_NO_CLIPBOARD
                }
                .to_string(),
            ),
            other => other,
        };
        if matches!(event, TermEvent::Wakeup) && self.output_wake.is_some() {
            // The engine emits Wakeup while parsing, before the reader
            // publishes its output generation. Waking here can therefore race
            // the UI into observing the old generation. Wakeup has no semantic
            // payload, so suppress it; the reader requests through this gate
            // immediately after its Release generation increment.
            return;
        }
        match self.tx.try_send(event) {
            Ok(()) => (self.waker)(),
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                // The parser calls this while holding the terminal mutex, so
                // blocking here would deadlock the UI renderer. A full bounded
                // semantic queue is therefore an explicit fail-pane condition:
                // retain no unbounded overflow, drop no reply silently while
                // pretending the pane is healthy, and wake the owner to tear
                // the hostile/stalled pane down.
                if !self.overflowed.swap(true, Ordering::AcqRel) {
                    (self.waker)();
                }
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EventListener, EventProxy, OutputWakeGate, TermEvent};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
        // The nonblocking send swallows a closed-channel error,
        // so a terminal that outlives the UI receiver can't crash on its
        // next event.
        let (tx, rx) = crossbeam_channel::unbounded();
        let proxy = EventProxy::new(tx, Arc::new(|| {}));
        drop(rx);
        proxy.send_event(TermEvent::Wakeup); // must not panic
    }

    #[test]
    fn bounded_semantic_queue_fails_closed_without_blocking_or_growing() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let woke = Arc::new(AtomicUsize::new(0));
        let woke_for_callback = woke.clone();
        let waker: super::Waker = Arc::new(move || {
            woke_for_callback.fetch_add(1, Ordering::SeqCst);
        });
        let output_wake = Arc::new(OutputWakeGate::new(waker.clone()));
        let overflowed = Arc::new(AtomicBool::new(false));
        let proxy = EventProxy::with_output_wake_osc52_and_overflow(
            tx,
            waker,
            output_wake,
            Arc::new(AtomicBool::new(true)),
            overflowed.clone(),
        );

        proxy.send_event(TermEvent::Bell);
        proxy.send_event(TermEvent::Title("hostile flood".into()));
        proxy.send_event(TermEvent::PtyWrite("must not allocate an overflow".into()));

        assert!(overflowed.load(Ordering::Acquire));
        assert!(matches!(rx.try_recv(), Ok(TermEvent::Bell)));
        assert!(rx.try_recv().is_err());
        assert_eq!(
            woke.load(Ordering::SeqCst),
            2,
            "one normal event and the first overflow each wake exactly once"
        );
    }

    #[test]
    fn primary_da_reply_advertises_shipped_extensions() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let proxy = EventProxy::new(tx, Arc::new(|| {}));

        proxy.send_event(TermEvent::PtyWrite("\x1b[?6c".into()));
        assert!(matches!(
            rx.try_recv(),
            Ok(TermEvent::PtyWrite(s)) if s == super::PRIMARY_DA_REPLY
        ));

        proxy.send_event(TermEvent::PtyWrite("\x1b[>0;276;0c".into()));
        assert!(matches!(
            rx.try_recv(),
            Ok(TermEvent::PtyWrite(s)) if s == "\x1b[>0;276;0c"
        ));
    }

    #[test]
    fn primary_da_clipboard_extension_tracks_live_copy_policy() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let waker: super::Waker = Arc::new(|| {});
        let output_wake = Arc::new(OutputWakeGate::new(waker.clone()));
        let allowed = Arc::new(AtomicBool::new(false));
        let proxy = EventProxy::with_output_wake_and_osc52(tx, waker, output_wake, allowed.clone());

        proxy.send_event(TermEvent::PtyWrite("\x1b[?6c".into()));
        assert!(matches!(
            rx.try_recv(),
            Ok(TermEvent::PtyWrite(s)) if s == super::PRIMARY_DA_REPLY_NO_CLIPBOARD
        ));

        allowed.store(true, Ordering::Release);
        proxy.send_event(TermEvent::PtyWrite("\x1b[?6c".into()));
        assert!(matches!(
            rx.try_recv(),
            Ok(TermEvent::PtyWrite(s)) if s == super::PRIMARY_DA_REPLY
        ));

        allowed.store(false, Ordering::Release);
        proxy.send_event(TermEvent::PtyWrite("\x1b[?6c".into()));
        assert!(matches!(
            rx.try_recv(),
            Ok(TermEvent::PtyWrite(s)) if s == super::PRIMARY_DA_REPLY_NO_CLIPBOARD
        ));
    }

    #[test]
    fn output_wake_gate_bounds_flood_and_publishes_one_restore_wake() {
        let woke = Arc::new(AtomicUsize::new(0));
        let woke2 = woke.clone();
        let gate = OutputWakeGate::new(Arc::new(move || {
            woke2.fetch_add(1, Ordering::SeqCst);
        }));

        for _ in 0..10_000 {
            gate.request();
        }
        assert_eq!(woke.load(Ordering::SeqCst), 1);

        gate.acknowledge();
        gate.set_enabled(false);
        for _ in 0..10_000 {
            gate.request();
        }
        assert_eq!(woke.load(Ordering::SeqCst), 1);

        gate.set_enabled(true);
        assert_eq!(
            woke.load(Ordering::SeqCst),
            2,
            "one hidden burst must publish exactly one restore wake"
        );
        for _ in 0..10_000 {
            gate.request();
        }
        assert_eq!(woke.load(Ordering::SeqCst), 2);

        gate.acknowledge();
        gate.request();
        assert_eq!(woke.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn gated_event_proxy_leaves_output_waking_to_post_generation_reader_publish() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let woke = Arc::new(AtomicUsize::new(0));
        let woke2 = woke.clone();
        let waker: super::Waker = Arc::new(move || {
            woke2.fetch_add(1, Ordering::SeqCst);
        });
        let gate = Arc::new(OutputWakeGate::new(waker.clone()));
        let proxy = EventProxy::with_output_wake(tx, waker, gate.clone());

        proxy.send_event(TermEvent::Wakeup);
        proxy.send_event(TermEvent::Wakeup);
        assert!(rx.try_recv().is_err());
        assert_eq!(
            woke.load(Ordering::SeqCst),
            0,
            "engine Wakeup precedes generation publication and must stay inert"
        );

        assert!(gate.request(), "the reader owns the post-generation wake");
        assert_eq!(woke.load(Ordering::SeqCst), 1);

        proxy.send_event(TermEvent::Wakeup);
        proxy.send_event(TermEvent::Bell);
        assert!(matches!(rx.try_recv(), Ok(TermEvent::Bell)));
        assert!(rx.try_recv().is_err());
        assert_eq!(
            woke.load(Ordering::SeqCst),
            2,
            "semantic events continue to wake independently"
        );
    }
}
