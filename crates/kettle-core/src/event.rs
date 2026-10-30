//! Bridges `alacritty_terminal` events to the UI thread.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub use alacritty_terminal::event::{Event as TermEvent, EventListener};
#[cfg(test)]
use alacritty_terminal::term::ClipboardType;
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

/// A window title a program asked for. Only the newest one is kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TitleChange {
    Set(String),
    Reset,
}

const MAX_CLIPBOARD_LOADS: usize = 1024;
const MAX_CLIPBOARD_ENTRIES: usize = 2 * MAX_CLIPBOARD_LOADS + 2;
const MAX_CLIPBOARD_ENTRY_BYTES: usize = 1024 * 1024;
const MAX_CLIPBOARD_BYTES: usize = 2 * MAX_CLIPBOARD_ENTRY_BYTES;

#[derive(Default)]
struct ClipboardJournal {
    entries: Vec<TermEvent>,
    bytes: usize,
    loads: usize,
}

impl ClipboardJournal {
    fn push(&mut self, mut event: TermEvent) -> bool {
        let first = self.entries.is_empty();
        match &mut event {
            TermEvent::ClipboardStore(target, text) => {
                match crate::scrub_card_markers(std::mem::take(text)) {
                    Ok(clean) => *text = clean,
                    Err(error) => {
                        log::warn!("OSC 52: could not prepare text: {error}");
                        return false;
                    }
                }
                if text.len() > MAX_CLIPBOARD_ENTRY_BYTES {
                    let mut end = MAX_CLIPBOARD_ENTRY_BYTES;
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                    text.shrink_to_fit();
                }
                // Targets can alias the same system clipboard, so every read
                // is a coalescing boundary, including reads of the other target.
                let after_read = self
                    .entries
                    .iter()
                    .rposition(|entry| matches!(entry, TermEvent::ClipboardLoad(..)))
                    .map_or(0, |index| index + 1);
                if let Some(index) = self.entries[after_read..].iter().position(
                    |entry| matches!(entry, TermEvent::ClipboardStore(kept, _) if kept == target),
                ) {
                    self.remove_store(after_read + index);
                }
                self.bytes += text.len();
            }
            TermEvent::ClipboardLoad(..) => {
                if self.loads == MAX_CLIPBOARD_LOADS {
                    return false;
                }
                self.loads += 1;
            }
            _ => unreachable!(),
        }
        self.entries.push(event);
        while self.entries.len() > MAX_CLIPBOARD_ENTRIES || self.bytes > MAX_CLIPBOARD_BYTES {
            let Some(index) = self
                .entries
                .iter()
                .position(|entry| matches!(entry, TermEvent::ClipboardStore(..)))
            else {
                break;
            };
            self.evict_store_and_reads(index);
        }
        first
    }

    fn evict_store_and_reads(&mut self, index: usize) {
        let TermEvent::ClipboardStore(target, _) = &self.entries[index] else {
            unreachable!();
        };
        let target = *target;
        self.remove_store(index);
        // Reads can depend on this write even across writes to the other
        // target: platforms may keep the targets separate or alias them.
        let mut next = index;
        while next < self.entries.len() {
            match &self.entries[next] {
                TermEvent::ClipboardStore(kept, _) if *kept == target => break,
                TermEvent::ClipboardLoad(..) => {
                    self.entries.remove(next);
                    self.loads -= 1;
                }
                _ => next += 1,
            }
        }
    }

    fn remove_store(&mut self, index: usize) {
        if let TermEvent::ClipboardStore(_, text) = self.entries.remove(index) {
            self.bytes -= text.len();
        }
    }
}

/// Terminal events that only matter as their latest state, held until the
/// owner takes them. They are latched here, not queued: output that repeats
/// them, such as `cat` of a binary file or a loop setting the title, would
/// otherwise fill the bounded event queue faster than a slow frame drains it.
#[derive(Default)]
pub(crate) struct LatchedEvents {
    bell: AtomicBool,
    cursor_blinking_changed: AtomicBool,
    exited: AtomicBool,
    title: Mutex<Option<TitleChange>>,
    clipboard: Mutex<ClipboardJournal>,
}

impl LatchedEvents {
    pub(crate) fn take_bell(&self) -> bool {
        self.bell.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn take_cursor_blinking_changed(&self) -> bool {
        self.cursor_blinking_changed.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn take_exited(&self) -> bool {
        self.exited.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn take_title(&self) -> Option<TitleChange> {
        self.title.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub(crate) fn take_clipboard_events(&self) -> Vec<TermEvent> {
        let mut journal = self.clipboard.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *journal).entries
    }

    /// Latch `event` if it is one of the latched kinds. Returns whether it
    /// was, and whether it is the first of its kind since the owner last took
    /// it, so the owner is woken once per run rather than once per event.
    fn latch(&self, event: TermEvent) -> Result<bool, TermEvent> {
        let flag = |flag: &AtomicBool| !flag.swap(true, Ordering::AcqRel);
        Ok(match event {
            TermEvent::Bell => flag(&self.bell),
            TermEvent::CursorBlinkingChange => flag(&self.cursor_blinking_changed),
            TermEvent::Exit => flag(&self.exited),
            TermEvent::Title(title) => self
                .title
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .replace(TitleChange::Set(title))
                .is_none(),
            TermEvent::ResetTitle => self
                .title
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .replace(TitleChange::Reset)
                .is_none(),
            event @ (TermEvent::ClipboardStore(..) | TermEvent::ClipboardLoad(..)) => self
                .clipboard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(event),
            other => return Err(other),
        })
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
    /// Semantic/lifecycle work must not be mistaken for paint-only output.
    /// The UI consumes this edge before applying its output-generation gate.
    lifecycle_pending: Arc<AtomicBool>,
    osc52_copy_allowed: Arc<AtomicBool>,
    /// State events the owner takes as their latest value: see
    /// [`EventProxy::send_event`].
    latched: Arc<LatchedEvents>,
    /// Replies dropped in the current run of a full queue.
    dropped_replies: Arc<AtomicUsize>,
}

impl EventProxy {
    pub fn new(tx: Sender<TermEvent>, waker: Waker) -> Self {
        Self {
            tx,
            waker,
            output_wake: None,
            lifecycle_pending: Arc::new(AtomicBool::new(false)),
            // Compatibility constructor: callers that do not provide a live
            // policy retain Kettle's default copy-enabled behavior.
            osc52_copy_allowed: Arc::new(AtomicBool::new(true)),
            latched: Arc::default(),
            dropped_replies: Arc::default(),
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
        Self {
            tx,
            waker,
            output_wake: Some(output_wake),
            lifecycle_pending: Arc::new(AtomicBool::new(false)),
            osc52_copy_allowed,
            latched: Arc::default(),
            dropped_replies: Arc::default(),
        }
    }

    pub(crate) fn lifecycle_pending(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.lifecycle_pending)
    }

    pub(crate) fn latched(&self) -> Arc<LatchedEvents> {
        Arc::clone(&self.latched)
    }

    /// Take OSC 52 operations in arrival order.
    pub fn take_clipboard_events(&self) -> Vec<TermEvent> {
        self.latched.take_clipboard_events()
    }

    fn wake_owner(&self) {
        self.lifecycle_pending.store(true, Ordering::Release);
        (self.waker)();
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TermEvent) {
        // The grid's mouse-cursor shape is read on every frame; nothing
        // consumes this hint, and a run of mode toggles would only fill the
        // queue with it.
        if matches!(event, TermEvent::MouseCursorDirty) {
            return;
        }
        // State events are latched, the newest kept, and wake the owner once
        // per run; see [`LatchedEvents`].
        let event = match self.latched.latch(event) {
            Ok(first) => {
                if first {
                    self.wake_owner();
                }
                return;
            }
            Err(event) => event,
        };
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
            Ok(()) => {
                let dropped = self.dropped_replies.swap(0, Ordering::AcqRel);
                if dropped > 0 {
                    log::warn!("terminal queue drained; {dropped} replies were dropped");
                }
                self.wake_owner();
            }
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                // Only replies to the program's queries (and the advisory
                // child-exit status) reach the queue. The parser calls this
                // while holding the terminal mutex, so it must not block, and
                // the queue must not grow. A program asking faster than the
                // window answers loses the newest replies until the queue
                // drains, as one that does not read its replies already does.
                if self.dropped_replies.fetch_add(1, Ordering::AcqRel) == 0 {
                    log::warn!("terminal queue full; dropping replies until it drains");
                }
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ClipboardType, EventListener, EventProxy, OutputWakeGate, TermEvent};
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
        let lifecycle_pending = proxy.lifecycle_pending();

        proxy.send_event(TermEvent::Wakeup);
        assert!(lifecycle_pending.swap(false, Ordering::AcqRel));
        proxy.send_event(TermEvent::PtyWrite("\x1b[0n".into()));
        assert!(lifecycle_pending.swap(false, Ordering::AcqRel));

        // Both events arrive on the channel in order…
        assert!(matches!(rx.try_recv(), Ok(TermEvent::Wakeup)));
        assert!(matches!(rx.try_recv(), Ok(TermEvent::PtyWrite(s)) if s == "\x1b[0n"));
        assert!(rx.try_recv().is_err());
        // …and the waker fired once per event.
        assert_eq!(woke.load(Ordering::SeqCst), 2);
    }

    fn gated_proxy(
        capacity: usize,
    ) -> (
        EventProxy,
        crossbeam_channel::Receiver<TermEvent>,
        Arc<AtomicUsize>,
    ) {
        let (tx, rx) = crossbeam_channel::bounded(capacity);
        let woke = Arc::new(AtomicUsize::new(0));
        let woke_for_callback = woke.clone();
        let waker: super::Waker = Arc::new(move || {
            woke_for_callback.fetch_add(1, Ordering::SeqCst);
        });
        let output_wake = Arc::new(OutputWakeGate::new(waker.clone()));
        let proxy = EventProxy::with_output_wake_and_osc52(
            tx,
            waker,
            output_wake,
            Arc::new(AtomicBool::new(true)),
        );
        (proxy, rx, woke)
    }

    /// Output that repeats state events, such as `cat` of a binary file or a
    /// loop setting the title, is latched, the newest kept, and wakes the
    /// owner once per run. A flood of them once filled the 1,024-deep queue
    /// and closed the pane, and with it a one-pane Kettle.
    #[test]
    fn floods_of_state_events_are_latched_not_queued() {
        let (proxy, rx, woke) = gated_proxy(1024);
        let latched = proxy.latched();

        for i in 0..10_000 {
            proxy.send_event(TermEvent::Bell);
            proxy.send_event(TermEvent::Title(format!("title {i}")));
            proxy.send_event(TermEvent::CursorBlinkingChange);
            proxy.send_event(TermEvent::MouseCursorDirty);
            proxy.send_event(TermEvent::ClipboardStore(
                ClipboardType::Clipboard,
                format!("copy {i}"),
            ));
        }
        proxy.send_event(TermEvent::ClipboardStore(
            ClipboardType::Selection,
            "selected".into(),
        ));
        assert!(rx.is_empty(), "nothing a program repeats reaches the queue");
        assert_eq!(
            woke.load(Ordering::SeqCst),
            4,
            "each kind wakes once per run: bell, title, blinking, clipboard"
        );
        assert!(latched.take_bell());
        assert!(!latched.take_bell());
        assert_eq!(
            latched.take_title(),
            Some(super::TitleChange::Set("title 9999".into()))
        );
        assert_eq!(latched.take_title(), None);
        assert!(latched.take_cursor_blinking_changed());
        assert_eq!(
            latched
                .take_clipboard_events()
                .into_iter()
                .map(|event| match event {
                    TermEvent::ClipboardStore(target, text) => (target, text),
                    _ => unreachable!(),
                })
                .collect::<Vec<_>>(),
            vec![
                (ClipboardType::Clipboard, "copy 9999".into()),
                (ClipboardType::Selection, "selected".into()),
            ]
        );
        assert!(
            latched
                .take_clipboard_events()
                .into_iter()
                .map(|event| match event {
                    TermEvent::ClipboardStore(target, text) => (target, text),
                    _ => unreachable!(),
                })
                .collect::<Vec<_>>()
                .is_empty()
        );
        // Writes come back oldest first: where both targets are one system
        // clipboard, the newer must be applied last.
        for (target, text) in [
            (ClipboardType::Clipboard, "a"),
            (ClipboardType::Selection, "b"),
            (ClipboardType::Clipboard, "c"),
        ] {
            proxy.send_event(TermEvent::ClipboardStore(target, text.into()));
        }
        assert_eq!(
            latched
                .take_clipboard_events()
                .into_iter()
                .map(|event| match event {
                    TermEvent::ClipboardStore(target, text) => (target, text),
                    _ => unreachable!(),
                })
                .collect::<Vec<_>>(),
            vec![
                (ClipboardType::Selection, "b".into()),
                (ClipboardType::Clipboard, "c".into()),
            ]
        );

        // A reset after a title wins, as the newer request.
        proxy.send_event(TermEvent::Title("gone".into()));
        proxy.send_event(TermEvent::ResetTitle);
        assert_eq!(latched.take_title(), Some(super::TitleChange::Reset));

        proxy.send_event(TermEvent::Exit);
        proxy.send_event(TermEvent::Exit);
        assert!(latched.take_exited());
        assert!(!latched.take_exited());
        assert!(rx.is_empty());
    }

    /// Replies are the only events that reach the bounded queue. A program
    /// asking faster than the window answers loses the newest replies while
    /// the queue is full; the queue never grows and the pane stays open.
    #[test]
    fn clipboard_journal_normalizes_before_its_byte_limit() {
        let (proxy, _, _) = gated_proxy(1024);
        let prefix = "A".repeat(super::MAX_CLIPBOARD_ENTRY_BYTES - 2);
        let marker: String = std::iter::once('\u{10eeee}')
            .chain(std::iter::repeat_n('\u{0305}', 8))
            .collect();
        proxy.send_event(TermEvent::ClipboardStore(
            ClipboardType::Clipboard,
            format!("{prefix}{marker}Z"),
        ));
        let entries = proxy.take_clipboard_events();
        let TermEvent::ClipboardStore(_, text) = &entries[0] else {
            panic!("expected store");
        };
        assert!(
            text.ends_with(" Z"),
            "normalize complete card clusters before enforcing the journal byte cap"
        );
        assert_eq!(text.len(), super::MAX_CLIPBOARD_ENTRY_BYTES);
        assert!(text.starts_with(&prefix));
    }

    #[test]
    fn clipboard_journal_bounds_stores_and_refuses_newest_loads() {
        let (proxy, rx, _) = gated_proxy(1024);
        proxy.send_event(TermEvent::ClipboardStore(
            ClipboardType::Clipboard,
            "x".repeat(super::MAX_CLIPBOARD_ENTRY_BYTES + 1),
        ));
        let entries = proxy.take_clipboard_events();
        let TermEvent::ClipboardStore(_, text) = &entries[0] else {
            panic!("expected store");
        };
        assert_eq!(text.len(), super::MAX_CLIPBOARD_ENTRY_BYTES);
        for index in 0..super::MAX_CLIPBOARD_LOADS + 10 {
            proxy.send_event(TermEvent::ClipboardStore(
                ClipboardType::Clipboard,
                "x".into(),
            ));
            proxy.send_event(TermEvent::ClipboardLoad(
                ClipboardType::Clipboard,
                Arc::new(move |_| index.to_string()),
            ));
        }
        let journal = proxy.latched.clipboard.lock().unwrap();
        assert_eq!(journal.loads, super::MAX_CLIPBOARD_LOADS);
        assert!(journal.bytes <= super::MAX_CLIPBOARD_BYTES);
        assert!(journal.entries.len() <= super::MAX_CLIPBOARD_ENTRIES);
        let loads: Vec<_> = journal
            .entries
            .iter()
            .filter_map(|event| match event {
                TermEvent::ClipboardLoad(_, fmt) => Some(fmt("")),
                TermEvent::ClipboardStore(_, text) => {
                    assert!(text.len() <= super::MAX_CLIPBOARD_ENTRY_BYTES);
                    None
                }
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            loads,
            (0..super::MAX_CLIPBOARD_LOADS)
                .map(|index| index.to_string())
                .collect::<Vec<_>>()
        );
        assert!(rx.is_empty());
    }

    #[test]
    fn clipboard_byte_eviction_never_answers_reads_with_unrelated_contents() {
        let (proxy, _rx, _) = gated_proxy(1024);
        for label in ["A", "B", "C"] {
            proxy.send_event(TermEvent::ClipboardStore(
                ClipboardType::Clipboard,
                label.repeat(super::MAX_CLIPBOARD_ENTRY_BYTES),
            ));
            proxy.send_event(TermEvent::ClipboardLoad(
                ClipboardType::Clipboard,
                Arc::new(move |text| {
                    assert!(
                        text.starts_with(label),
                        "read {label} saw unrelated clipboard"
                    );
                    assert_eq!(text.len(), super::MAX_CLIPBOARD_ENTRY_BYTES);
                    label.to_string()
                }),
            ));
        }
        let journal = proxy.latched.clipboard.lock().unwrap();
        assert!(journal.bytes <= super::MAX_CLIPBOARD_BYTES);
        assert!(journal.entries.len() <= super::MAX_CLIPBOARD_ENTRIES);
        drop(journal);
        let mut clipboard = "old OS clipboard".to_string();
        let mut replies = Vec::new();
        for event in proxy.take_clipboard_events() {
            match event {
                TermEvent::ClipboardStore(_, text) => clipboard = text,
                TermEvent::ClipboardLoad(_, fmt) => replies.push(fmt(&clipboard)),
                _ => unreachable!(),
            }
        }
        assert_eq!(
            replies,
            ["B", "C"],
            "evicted writes must take their reads with them"
        );
    }

    #[test]
    fn clipboard_eviction_drops_dependent_reads_across_other_target_writes() {
        let (proxy, _rx, _) = gated_proxy(1024);
        for (target, label) in [
            (ClipboardType::Clipboard, "A"),
            (ClipboardType::Selection, "B"),
            (ClipboardType::Clipboard, "C"),
        ] {
            proxy.send_event(TermEvent::ClipboardStore(
                target,
                label.repeat(super::MAX_CLIPBOARD_ENTRY_BYTES),
            ));
            proxy.send_event(TermEvent::ClipboardLoad(
                ClipboardType::Clipboard,
                Arc::new(str::to_owned),
            ));
        }
        let mut clipboard = "old OS clipboard".to_string();
        let mut replies = Vec::new();
        for event in proxy.take_clipboard_events() {
            match event {
                TermEvent::ClipboardStore(ClipboardType::Clipboard, text) => clipboard = text,
                TermEvent::ClipboardStore(ClipboardType::Selection, _) => {}
                TermEvent::ClipboardLoad(_, fmt) => replies.push(fmt(&clipboard)),
                _ => unreachable!(),
            }
        }
        assert_eq!(
            replies.len(),
            1,
            "reads still depend on A after a selection write"
        );
        assert_eq!(replies[0], "C".repeat(super::MAX_CLIPBOARD_ENTRY_BYTES));
    }

    #[test]
    fn clipboard_reads_keep_intervening_writes() {
        let (proxy, _rx, _) = gated_proxy(1024);
        for text in ["A", "B"] {
            proxy.send_event(TermEvent::ClipboardStore(
                ClipboardType::Clipboard,
                text.into(),
            ));
            proxy.send_event(TermEvent::ClipboardLoad(
                ClipboardType::Clipboard,
                Arc::new(str::to_owned),
            ));
        }
        let mut clipboard = String::new();
        let mut replies = Vec::new();
        for event in proxy.take_clipboard_events() {
            match event {
                TermEvent::ClipboardStore(_, text) => clipboard = text,
                TermEvent::ClipboardLoad(_, fmt) => replies.push(fmt(&clipboard)),
                _ => unreachable!(),
            }
        }
        assert_eq!(
            replies,
            ["A", "B"],
            "each read must observe the preceding write"
        );
    }

    #[test]
    fn a_full_queue_drops_replies_without_blocking_or_growing() {
        let (proxy, rx, woke) = gated_proxy(2);

        for _ in 0..1025 {
            proxy.send_event(TermEvent::PtyWrite("\x1b[0n".into()));
        }
        assert_eq!(rx.len(), 2, "the queue holds its bound and no more");
        assert_eq!(
            woke.load(Ordering::SeqCst),
            2,
            "only delivered replies wake"
        );
        assert_eq!(proxy.dropped_replies.load(Ordering::Acquire), 1023);

        // Once the owner drains it, replies flow again and the run ends.
        while rx.try_recv().is_ok() {}
        proxy.send_event(TermEvent::PtyWrite("\x1b[?6c".into()));
        assert!(matches!(rx.try_recv(), Ok(TermEvent::PtyWrite(_))));
        assert_eq!(proxy.dropped_replies.load(Ordering::Acquire), 0);
    }

    #[test]
    fn suppressed_engine_wakeup_is_not_a_semantic_lifecycle_edge() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let waker: super::Waker = Arc::new(|| {});
        let output_wake = Arc::new(OutputWakeGate::new(waker.clone()));
        let proxy = EventProxy::with_output_wake(tx, waker, output_wake);
        let lifecycle_pending = proxy.lifecycle_pending();

        proxy.send_event(TermEvent::Wakeup);

        assert!(rx.is_empty());
        assert!(!lifecycle_pending.load(Ordering::Acquire));
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
        proxy.send_event(TermEvent::PtyWrite("\x1b[0n".into()));
        assert!(matches!(rx.try_recv(), Ok(TermEvent::PtyWrite(_))));
        assert!(rx.try_recv().is_err());
        assert_eq!(
            woke.load(Ordering::SeqCst),
            2,
            "semantic events continue to wake independently"
        );
    }
}
