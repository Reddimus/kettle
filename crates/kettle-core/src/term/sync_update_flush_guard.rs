use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::Processor;

use super::{
    GraphicsEvent, ImageHistoryPruner, LOCAL_OUTPUT_QUEUE_DEPTH, PTY_PUMP_QUEUE_DEPTH, ParserInput,
    SharedTerm, SyncFlushContext, SyncGraphicsDispatch, publish_output_if_ready,
    receive_parser_input, receive_pty_chunk,
};
use crate::event::OutputWakeGate;
use crate::{EventProxy, Waker};

struct Size;

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        4
    }

    fn screen_lines(&self) -> usize {
        4
    }

    fn columns(&self) -> usize {
        40
    }
}

fn shared_term() -> SharedTerm {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let proxy = EventProxy::new(tx, Arc::new(|| {}));
    Arc::new(std::sync::Mutex::new(Term::new(
        TermConfig::default(),
        &Size,
        proxy,
    )))
}

fn image_pruning_fixture() -> (crate::Images, ImageHistoryPruner) {
    (
        Arc::new(Mutex::new(Vec::new())),
        ImageHistoryPruner::default(),
    )
}

fn ignore_sync_graphics(_: SyncGraphicsDispatch<'_>) {}

#[test]
fn pending_sync_suppresses_generation_and_wake_until_close() {
    let term = shared_term();
    let mut processor: Processor = Processor::new();
    let generation = AtomicU64::new(0);
    let wakes = Arc::new(AtomicUsize::new(0));
    let wakes_for_callback = wakes.clone();
    let output_wake = OutputWakeGate::new(Arc::new(move || {
        wakes_for_callback.fetch_add(1, Ordering::Relaxed);
    }));

    processor.advance(&mut *term.lock().unwrap(), b"\x1b[?2026hbuffered");
    assert!(!publish_output_if_ready(
        &processor,
        &generation,
        &output_wake
    ));
    assert_eq!(generation.load(Ordering::Acquire), 0);
    assert_eq!(wakes.load(Ordering::Relaxed), 0);

    processor.advance(&mut *term.lock().unwrap(), b"\x1b[?2026l");
    assert!(publish_output_if_ready(
        &processor,
        &generation,
        &output_wake
    ));
    assert_eq!(generation.load(Ordering::Acquire), 1);
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
}

#[test]
fn omitted_sync_terminator_flushes_and_wakes_at_deadline() {
    let term = shared_term();
    let mut processor: Processor = Processor::new();
    {
        let mut term = term.lock().unwrap();
        processor.advance(&mut *term, b"\x1b[?2026h\x1b[2;3Hstale bottom text");
    }
    let deadline = processor
        .sync_timeout()
        .sync_timeout()
        .expect("DEC 2026 opened a synchronized update");

    let (tx, rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
    let delay = deadline
        .saturating_duration_since(std::time::Instant::now())
        .saturating_add(std::time::Duration::from_millis(20));
    let sender = std::thread::spawn(move || {
        std::thread::sleep(delay);
        tx.send(None).unwrap();
    });
    let generation = AtomicU64::new(0);
    let wakes = Arc::new(AtomicUsize::new(0));
    let wakes_for_callback = wakes.clone();
    let waker: Waker = Arc::new(move || {
        wakes_for_callback.fetch_add(1, Ordering::Relaxed);
    });
    let output_wake = OutputWakeGate::new(waker);
    let (images, mut image_pruner) = image_pruning_fixture();
    let graphics_gate = Mutex::new(());
    let mut on_graphics = ignore_sync_graphics;
    let mut sync_flush = SyncFlushContext {
        term: &term,
        images: &images,
        graphics_gate: &graphics_gate,
        image_pruner: &mut image_pruner,
        on_graphics: &mut on_graphics,
        out_gen: &generation,
        output_wake: &output_wake,
    };

    assert!(receive_pty_chunk(&mut processor, &rx, &mut sync_flush).is_none());
    sender.join().unwrap();
    assert!(processor.sync_timeout().sync_timeout().is_none());
    assert_eq!(generation.load(Ordering::Acquire), 1);
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    assert_eq!(
        term.lock().unwrap().grid()[Point::new(Line(1), Column(2))].c,
        's'
    );
}

#[test]
fn expired_sync_flushes_before_a_queued_chunk() {
    let term = shared_term();
    let mut processor: Processor = Processor::new();
    {
        let mut term = term.lock().unwrap();
        processor.advance(&mut *term, b"\x1b[?2026h\x1b[2;3Hexpired text");
    }
    let deadline = processor
        .sync_timeout()
        .sync_timeout()
        .expect("DEC 2026 opened a synchronized update");
    std::thread::sleep(
        deadline
            .saturating_duration_since(std::time::Instant::now())
            .saturating_add(std::time::Duration::from_millis(20)),
    );

    let (tx, rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
    tx.send(Some((7, b"next chunk".to_vec(), 10))).unwrap();
    let generation = AtomicU64::new(0);
    let wakes = Arc::new(AtomicUsize::new(0));
    let wakes_for_callback = wakes.clone();
    let waker: Waker = Arc::new(move || {
        wakes_for_callback.fetch_add(1, Ordering::Relaxed);
    });
    let output_wake = OutputWakeGate::new(waker);
    let (images, mut image_pruner) = image_pruning_fixture();
    let graphics_gate = Mutex::new(());
    let mut on_graphics = ignore_sync_graphics;
    let mut sync_flush = SyncFlushContext {
        term: &term,
        images: &images,
        graphics_gate: &graphics_gate,
        image_pruner: &mut image_pruner,
        on_graphics: &mut on_graphics,
        out_gen: &generation,
        output_wake: &output_wake,
    };

    let chunk = receive_pty_chunk(&mut processor, &rx, &mut sync_flush)
        .expect("queued chunk is preserved after the flush");
    assert_eq!(chunk, (7, b"next chunk".to_vec(), 10));
    assert!(processor.sync_timeout().sync_timeout().is_none());
    assert_eq!(generation.load(Ordering::Acquire), 1);
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    assert_eq!(
        term.lock().unwrap().grid()[Point::new(Line(1), Column(2))].c,
        'e'
    );
}

#[test]
fn sync_eof_flushes_without_waiting_for_the_deadline() {
    let term = shared_term();
    let mut processor: Processor = Processor::new();
    {
        let mut term = term.lock().unwrap();
        processor.advance(&mut *term, b"\x1b[?2026h\x1b[2;3Hfinal text");
    }

    let (tx, rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
    tx.send(None).unwrap();
    let generation = AtomicU64::new(0);
    let wakes = Arc::new(AtomicUsize::new(0));
    let wakes_for_callback = wakes.clone();
    let waker: Waker = Arc::new(move || {
        wakes_for_callback.fetch_add(1, Ordering::Relaxed);
    });
    let output_wake = OutputWakeGate::new(waker);
    let (images, mut image_pruner) = image_pruning_fixture();
    let graphics_gate = Mutex::new(());
    let mut on_graphics = ignore_sync_graphics;
    let mut sync_flush = SyncFlushContext {
        term: &term,
        images: &images,
        graphics_gate: &graphics_gate,
        image_pruner: &mut image_pruner,
        on_graphics: &mut on_graphics,
        out_gen: &generation,
        output_wake: &output_wake,
    };

    assert!(receive_pty_chunk(&mut processor, &rx, &mut sync_flush).is_none());
    assert!(processor.sync_timeout().sync_timeout().is_none());
    assert_eq!(generation.load(Ordering::Acquire), 1);
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    assert_eq!(
        term.lock().unwrap().grid()[Point::new(Line(1), Column(2))].c,
        'f'
    );
}

#[test]
fn local_output_waits_for_earlier_reads_and_a_closed_channel_is_skipped() {
    let term = shared_term();
    let mut processor: Processor = Processor::new();
    let generation = AtomicU64::new(0);
    let output_wake = OutputWakeGate::new(Arc::new(|| {}));
    let (images, mut image_pruner) = image_pruning_fixture();
    let graphics_gate = Mutex::new(());
    let mut on_graphics = ignore_sync_graphics;
    let mut sync_flush = SyncFlushContext {
        term: &term,
        images: &images,
        graphics_gate: &graphics_gate,
        image_pruner: &mut image_pruner,
        on_graphics: &mut on_graphics,
        out_gen: &generation,
        output_wake: &output_wake,
    };
    let (raw_tx, raw_rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
    let (local_tx, mut local_rx) = crossbeam_channel::bounded(LOCAL_OUTPUT_QUEUE_DEPTH);
    let mut pending = None;
    let mut receive = |pending: &mut Option<_>, local_rx: &mut _, handled| {
        receive_parser_input(
            &mut processor,
            &raw_rx,
            local_rx,
            pending,
            handled,
            &mut sync_flush,
        )
    };

    // Read 3 finished before the request, so it comes first even though
    // the request is also ready. `select!` picks at random, so repeat.
    for _ in 0..16 {
        raw_tx.send(Some((3, b"queued".to_vec(), 6))).unwrap();
        local_tx.send((3, b"\x1bc".to_vec())).unwrap();
        let first = receive(&mut pending, &mut local_rx, 2);
        assert!(matches!(first, Some(ParserInput::Pty(3, _, 6))));
        let second = receive(&mut pending, &mut local_rx, 3);
        assert!(matches!(second, Some(ParserInput::Local(bytes)) if bytes == b"\x1bc"));
    }

    // A request whose reads are all handled goes straight through.
    local_tx.send((3, b"\x1b[3J".to_vec())).unwrap();
    assert!(matches!(
        receive(&mut pending, &mut local_rx, 3),
        Some(ParserInput::Local(bytes)) if bytes == b"\x1b[3J"
    ));

    // Dropping the `Terminal` closes the local channel. The child's output
    // must keep arriving, and EOF must still end the reader.
    drop(local_tx);
    raw_tx.send(Some((4, b"tail".to_vec(), 4))).unwrap();
    assert!(matches!(
        receive(&mut pending, &mut local_rx, 3),
        Some(ParserInput::Pty(4, _, 4))
    ));
    raw_tx.send(None).unwrap();
    assert!(receive(&mut pending, &mut local_rx, 4).is_none());
}

#[test]
fn sync_eof_applies_buffered_graphics_before_publishing_the_flush() {
    let term = shared_term();
    let mut processor: Processor = Processor::new();
    {
        let mut term = term.lock().unwrap();
        processor.advance(&mut *term, b"\x1b[?2026h\x1b[2J");
        assert!(
            term.take_graphics_events().events.is_empty(),
            "ED2 remains buffered until the synchronized update is flushed"
        );
    }

    let (tx, rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
    tx.send(None).unwrap();
    let generation = AtomicU64::new(0);
    let wakes = Arc::new(AtomicUsize::new(0));
    let wakes_for_callback = wakes.clone();
    let waker: Waker = Arc::new(move || {
        wakes_for_callback.fetch_add(1, Ordering::Relaxed);
    });
    let output_wake = OutputWakeGate::new(waker);
    let (images, mut image_pruner) = image_pruning_fixture();
    let graphics_gate = Mutex::new(());
    let mut observed = Vec::new();
    let mut on_graphics = |dispatch: SyncGraphicsDispatch<'_>| {
        let SyncGraphicsDispatch::Batch(batch) = dispatch else {
            panic!("this fixture does not schedule synchronized graphics markers");
        };
        assert_eq!(
            generation.load(Ordering::Acquire),
            0,
            "graphics must apply before the output generation is published"
        );
        assert_eq!(
            wakes.load(Ordering::Relaxed),
            0,
            "graphics must apply before the render wake is published"
        );
        assert!(!batch.overflowed);
        observed.extend(batch.events);
    };
    let mut sync_flush = SyncFlushContext {
        term: &term,
        images: &images,
        graphics_gate: &graphics_gate,
        image_pruner: &mut image_pruner,
        on_graphics: &mut on_graphics,
        out_gen: &generation,
        output_wake: &output_wake,
    };

    assert!(receive_pty_chunk(&mut processor, &rx, &mut sync_flush).is_none());
    assert_eq!(observed, vec![GraphicsEvent::EraseDisplay]);
    assert_eq!(generation.load(Ordering::Acquire), 1);
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
}

#[test]
fn split_sync_terminator_arriving_before_deadline_does_not_force_flush() {
    let term = shared_term();
    let mut processor: Processor = Processor::new();
    {
        let mut term = term.lock().unwrap();
        processor.advance(&mut *term, b"\x1b[?2026h\x1b[2;3Hupdated");
    }
    let (tx, rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
    tx.send(Some((8, b"\x1b[?2026l".to_vec(), 8))).unwrap();
    let generation = AtomicU64::new(0);
    let waker: Waker = Arc::new(|| panic!("no timeout wake expected"));
    let output_wake = OutputWakeGate::new(waker);
    let (images, mut image_pruner) = image_pruning_fixture();
    let graphics_gate = Mutex::new(());
    let mut on_graphics = ignore_sync_graphics;
    let mut sync_flush = SyncFlushContext {
        term: &term,
        images: &images,
        graphics_gate: &graphics_gate,
        image_pruner: &mut image_pruner,
        on_graphics: &mut on_graphics,
        out_gen: &generation,
        output_wake: &output_wake,
    };

    let (read_generation, close, close_len) =
        receive_pty_chunk(&mut processor, &rx, &mut sync_flush).expect("close sequence received");
    assert_eq!(read_generation, 8);
    processor.advance(&mut *term.lock().unwrap(), &close[..close_len]);

    assert!(processor.sync_timeout().sync_timeout().is_none());
    assert_eq!(generation.load(Ordering::Acquire), 0);
}

#[test]
fn pty_pump_queue_has_a_hard_capacity() {
    let (tx, _rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
    for _ in 0..PTY_PUMP_QUEUE_DEPTH {
        tx.try_send(Some(vec![0; 1])).unwrap();
    }
    assert!(matches!(
        tx.try_send(Some(vec![0; 1])),
        Err(crossbeam_channel::TrySendError::Full(_))
    ));
}
