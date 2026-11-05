//! One PCM-only worker per decoded stream. Queues and credits stay off the GUI thread.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use crate::client::{RenderControl, Shared, SpawnedWorker};
use crate::lifecycle::{self, Budgets, Running};
use crate::pcm::{self, Feedback, Flow, Message};
use crate::wire::{self, Direction, Frame};
use crate::{HandshakeOutcome, Hello, PlayCommand, check_ready};

pub(crate) const FRAME_LIMIT: usize = wire::HEADER_BYTES + pcm::BYTES + 64;
type FeedbackSink = Box<dyn Fn(Feedback) + Send>;

struct Queue {
    flow: Flow,
    blocks: VecDeque<Message>,
    dispatched: u64,
    play: Option<PlayCommand>,
    mute: Option<PlayCommand>,
    heartbeat: bool,
    closed: bool,
}
impl Queue {
    fn dispatch(&mut self) -> Option<Message> {
        let message = self.blocks.pop_front();
        if matches!(&message, Some(Message::Block(_))) {
            self.dispatched += 1;
        }
        message
    }
}

struct State {
    queue: Mutex<Queue>,
    changed: Condvar,
    cancel: RenderControl,
}

#[derive(Clone)]
pub(crate) struct Control(Arc<State>);

impl Control {
    pub(crate) fn offer(&self, message: Message) -> bool {
        let mut queue = self.0.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let limit = pcm::WINDOW as usize + usize::from(matches!(&message, Message::End { .. }));
        if !queue.closed && queue.blocks.len() >= limit {
            return false;
        }
        let accepted = match &message {
            Message::Block(block) => queue.flow.offer(block),
            Message::End {
                generation,
                sequence,
            } => queue.flow.end(*generation, *sequence),
            _ => return false,
        };
        if accepted.is_err() {
            return false;
        }
        if !queue.closed {
            queue.blocks.push_back(message);
        }
        self.0.changed.notify_all();
        true
    }

    fn credit(&self, feedback: Feedback) -> bool {
        if Message::Feedback(feedback).validate().is_err() {
            return false;
        }
        // Retain enough admission state to drain the already-credited window.
        let credit = Feedback {
            failed: false,
            ..feedback
        };
        let mut queue = self.0.queue.lock().unwrap_or_else(PoisonError::into_inner);
        feedback.consumed <= queue.dispatched && queue.flow.credit(credit).is_ok()
    }

    pub(crate) fn command(&self, command: PlayCommand) {
        if command == PlayCommand::Close {
            self.close();
            return;
        }
        let mut queue = self.0.queue.lock().unwrap_or_else(PoisonError::into_inner);
        match command {
            PlayCommand::Play | PlayCommand::Pause => queue.play = Some(command),
            PlayCommand::Mute | PlayCommand::Unmute => queue.mute = Some(command),
            PlayCommand::Heartbeat => queue.heartbeat = true,
            PlayCommand::Close => unreachable!(),
        }
        self.0.changed.notify_all();
    }

    fn close(&self) {
        let mut queue = self.0.queue.lock().unwrap_or_else(PoisonError::into_inner);
        queue.closed = true;
        queue.blocks.clear();
        drop(queue);
        self.0.cancel.cancel();
        self.0.changed.notify_all();
    }
}

/// Commands can precede the broker's creation while the decoder opens.
#[derive(Default)]
pub(crate) struct Commands {
    play: Option<PlayCommand>,
    mute: Option<PlayCommand>,
    connected: bool,
}

impl Commands {
    pub(crate) fn forward(
        &mut self,
        broker: Option<&Control>,
        play: Option<PlayCommand>,
        mute: Option<PlayCommand>,
        heartbeat: bool,
        closing: bool,
    ) {
        self.play = play.or(self.play);
        self.mute = mute.or(self.mute);
        let Some(broker) = broker else { return };
        if closing {
            broker.command(PlayCommand::Close);
            return;
        }
        let commands = if self.connected {
            [play, mute]
        } else {
            [self.play, self.mute]
        };
        self.connected = true;
        for command in commands.into_iter().flatten() {
            broker.command(command);
        }
        if heartbeat {
            broker.command(PlayCommand::Heartbeat);
        }
    }
}

/// The decoder's reader owns the lease; control copies never extend it.
pub(crate) struct Broker(pub(crate) Control);
impl Drop for Broker {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl Broker {
    pub(crate) fn start(
        shared: Arc<Shared>,
        hello: Hello,
        budgets: Budgets,
        open: pcm::Open,
        memory: [Arc<AtomicU64>; 2],
        feedback: FeedbackSink,
    ) -> std::io::Result<Self> {
        let flow =
            Flow::new(open.clone()).map_err(|_| std::io::Error::other("invalid PCM stream"))?;
        let control = Control(Arc::new(State {
            queue: Mutex::new(Queue {
                flow,
                blocks: VecDeque::new(),
                dispatched: 0,
                play: None,
                mute: None,
                heartbeat: false,
                closed: false,
            }),
            changed: Condvar::new(),
            cancel: RenderControl::default(),
        }));
        let owned = control.clone();
        std::thread::Builder::new()
            .name("kettle-pcm-watch".into())
            .spawn(move || {
                supervise(&shared, &hello, budgets, &open, &owned, memory, &feedback);
                let queue = owned.0.queue.lock().unwrap_or_else(PoisonError::into_inner);
                let failed = !queue.closed;
                let consumed = queue.flow.consumed();
                drop(queue);
                owned.close();
                if failed {
                    feedback(Feedback {
                        generation: open.generation,
                        consumed,
                        played_frame: None,
                        age_us: 0,
                        done: false,
                        failed: true,
                    });
                }
            })?;
        Ok(Self(control))
    }
}

fn supervise(
    shared: &Arc<Shared>,
    hello: &Hello,
    budgets: Budgets,
    open: &pcm::Open,
    control: &Control,
    memory: [Arc<AtomicU64>; 2],
    feedback: &FeedbackSink,
) {
    let until = Instant::now() + budgets.ready;
    let Ok(SpawnedWorker {
        process,
        stdin,
        mut stdout,
    }) = lifecycle::spawn_within(shared, until, budgets, Some(&control.0.cancel))
    else {
        return;
    };
    let mut worker = Running::new(process, shared, budgets).with_memory(memory);
    let (events, received) = mpsc::sync_channel(8);
    if std::thread::Builder::new()
        .name("kettle-pcm-read".into())
        .spawn(move || {
            loop {
                let frame = wire::read_frame_within(
                    &mut stdout,
                    Direction::WorkerToParent,
                    wire::MAX_READY_FRAME_BYTES,
                );
                let terminal = !matches!(frame, Ok(Some(_)));
                if events.send((frame, Instant::now())).is_err() || terminal {
                    return;
                }
            }
        })
        .is_err()
    {
        return;
    }
    let (go, opening) = mpsc::channel();
    let writer_control = control.clone();
    let writer_shared = Arc::clone(shared);
    let hello_bytes = wire::encode(&Frame::Hello(hello.clone()), Direction::ParentToWorker);
    let open_bytes = wire::encode(
        &Frame::Pcm(Message::Open(open.clone())),
        Direction::ParentToWorker,
    );
    let (Ok(hello_bytes), Ok(open_bytes)) = (hello_bytes, open_bytes) else {
        return;
    };
    if std::thread::Builder::new()
        .name("kettle-pcm-write".into())
        .spawn(move || {
            if writer_shared.platform().guard_pipe_writes().is_err() {
                return;
            }
            write(stdin, &writer_control, &hello_bytes, &open_bytes, opening);
        })
        .is_err()
    {
        return;
    }
    match worker.next(&received, until, Some(&control.0.cancel)) {
        Ok((Ok(Some(Frame::Ready(ready))), _))
            if check_ready(hello, &ready) == HandshakeOutcome::Compatible => {}
        _ => return,
    }
    if go.send(()).is_err() {
        return;
    }
    loop {
        match worker.next(
            &received,
            Instant::now() + Duration::from_secs(3600),
            Some(&control.0.cancel),
        ) {
            Ok((Ok(Some(Frame::Pcm(Message::Feedback(now)))), received)) => {
                if !control.credit(now) {
                    return;
                }
                feedback(aged_feedback(now, received.elapsed()));
                if now.failed {
                    worker.stop();
                    return;
                }
            }
            Err(lifecycle::Wait::Timeout) if worker.own_exit().is_none() => {}
            Err(lifecycle::Wait::Interrupted) => {
                worker.stop();
                return;
            }
            _ => return,
        }
    }
}

fn write(
    mut stdin: Box<dyn Write + Send>,
    control: &Control,
    hello: &[u8],
    open: &[u8],
    opening: mpsc::Receiver<()>,
) {
    let mut send = |bytes: &[u8]| stdin.write_all(bytes).and_then(|()| stdin.flush());
    if send(hello).is_err() || opening.recv().is_err() || send(open).is_err() {
        return;
    }
    loop {
        let mut queue = control
            .0
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while !queue.closed
            && queue.blocks.is_empty()
            && queue.play.is_none()
            && queue.mute.is_none()
            && !queue.heartbeat
        {
            queue = control
                .0
                .changed
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
        let closed = queue.closed;
        let commands = [
            queue.play.take(),
            queue.mute.take(),
            std::mem::take(&mut queue.heartbeat).then_some(PlayCommand::Heartbeat),
        ];
        let message = queue.dispatch();
        drop(queue);
        if closed {
            if let Ok(bytes) = wire::encode(
                &Frame::PlayCommand(PlayCommand::Close),
                Direction::ParentToWorker,
            ) {
                let _ = send(&bytes);
            }
            return;
        }
        for frame in commands
            .into_iter()
            .flatten()
            .map(Frame::PlayCommand)
            .chain(message.map(Frame::Pcm))
        {
            match wire::encode(&frame, Direction::ParentToWorker) {
                Ok(bytes) if send(&bytes).is_ok() => {}
                _ => return,
            }
        }
    }
}

/// Queue residence ages a hardware timestamp; stale timestamps carry only credits.
pub(crate) fn aged_feedback(mut feedback: Feedback, elapsed: Duration) -> Feedback {
    let age = u128::from(feedback.age_us) + elapsed.as_micros();
    if age > 250_000 {
        feedback.played_frame = None;
        feedback.age_us = 0;
    } else {
        feedback.age_us = age as u32;
    }
    feedback
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control() -> Control {
        Control(Arc::new(State {
            queue: Mutex::new(Queue {
                flow: Flow::new(pcm::Open {
                    generation: 7,
                    start_frame: 0,
                    end_frame: 48_000,
                    muted: false,
                })
                .unwrap(),
                blocks: VecDeque::new(),
                dispatched: 0,
                play: None,
                mute: None,
                heartbeat: false,
                closed: false,
            }),
            changed: Condvar::new(),
            cancel: RenderControl::default(),
        }))
    }
    fn block(sequence: u64) -> Message {
        Message::Block(pcm::Block {
            generation: 7,
            sequence,
            at_frame: sequence * pcm::FRAMES as u64,
            samples: vec![0; pcm::BYTES],
        })
    }
    fn feedback() -> Feedback {
        Feedback {
            generation: 7,
            consumed: 0,
            played_frame: Some(100),
            age_us: 50_000,
            done: false,
            failed: false,
        }
    }
    #[test]
    fn hardware_clock_ages_in_the_reader_channel() {
        use crate::client::{
            FileIdentity, UnavailableCause, WorkerClient, WorkerExit, WorkerPlatform, WorkerProcess,
        };
        use std::io::{Cursor, Read};
        use std::path::Path;

        struct QueuedReader {
            bytes: Cursor<Vec<u8>>,
            start: Option<mpsc::Receiver<()>>,
            drained: Option<mpsc::Sender<()>>,
        }
        impl Read for QueuedReader {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                if let Some(start) = self.start.take() {
                    start.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                let n = self.bytes.read(bytes)?;
                if n == 0
                    && let Some(drained) = self.drained.take()
                {
                    drained.send(()).unwrap();
                }
                Ok(n)
            }
        }
        struct DelayedProbe {
            start: Option<mpsc::Sender<()>>,
            drained: mpsc::Receiver<()>,
            killed: bool,
        }
        impl WorkerProcess for DelayedProbe {
            fn try_wait(&mut self) -> std::io::Result<Option<WorkerExit>> {
                if let Some(start) = self.start.take() {
                    start.send(()).unwrap();
                    self.drained.recv_timeout(Duration::from_secs(5)).unwrap();
                    std::thread::sleep(Duration::from_millis(300));
                }
                Ok(self.killed.then_some(WorkerExit::Signal(9)))
            }
            fn kill(&mut self) {
                self.killed = true;
            }
            fn footprint(&mut self) -> std::io::Result<u64> {
                Ok(1)
            }
        }
        struct Fake(Mutex<Option<SpawnedWorker>>);
        impl WorkerPlatform for Fake {
            fn worker_path(&self) -> Result<&Path, UnavailableCause> {
                Ok(Path::new("/install/kettle-media-worker"))
            }
            fn inspect(&self, _: &Path) -> Result<FileIdentity, UnavailableCause> {
                Ok(FileIdentity {
                    dev: 1,
                    ino: 2,
                    size: 3,
                    mtime_seconds: 4,
                    mtime_nanos: 5,
                    ctime_seconds: 6,
                    ctime_nanos: 7,
                })
            }
            fn verify(&self, _: &Path) -> Result<(), UnavailableCause> {
                Ok(())
            }
            fn spawn(&self, _: &Path) -> std::io::Result<SpawnedWorker> {
                Ok(self.0.lock().unwrap().take().unwrap())
            }
            fn guard_pipe_writes(&self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let hello = Hello {
            build_id: crate::BuildId::from_embedded("1.0.0", "ab12").unwrap(),
        };
        let mut bytes = wire::encode(
            &Frame::Ready(crate::Ready {
                build_id: hello.build_id.clone(),
            }),
            Direction::WorkerToParent,
        )
        .unwrap();
        bytes.extend(
            wire::encode(
                &Frame::Pcm(Message::Feedback(Feedback {
                    consumed: 1,
                    ..feedback()
                })),
                Direction::WorkerToParent,
            )
            .unwrap(),
        );
        let (start, starting) = mpsc::channel();
        let (drained, draining) = mpsc::channel();
        let client = WorkerClient::new(
            hello.build_id.clone(),
            Box::new(Fake(Mutex::new(Some(SpawnedWorker {
                process: Box::new(DelayedProbe {
                    start: Some(start),
                    drained: draining,
                    killed: false,
                }),
                stdin: Box::new(std::io::sink()),
                stdout: Box::new(QueuedReader {
                    bytes: Cursor::new(bytes),
                    start: Some(starting),
                    drained: Some(drained),
                }),
            })))),
        );
        let control = control();
        assert!(control.offer(block(0)));
        assert!(control.0.queue.lock().unwrap().dispatch().is_some());
        let delivered = Arc::new(Mutex::new(None));
        let saved = Arc::clone(&delivered);
        let cancel = control.clone();
        let sink: FeedbackSink = Box::new(move |feedback| {
            *saved.lock().unwrap() = Some(feedback);
            cancel.close();
        });
        supervise(
            &client.shared,
            &hello,
            Budgets::PRODUCTION,
            &pcm::Open {
                generation: 7,
                start_frame: 0,
                end_frame: 48_000,
                muted: false,
            },
            &control,
            [Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0))],
            &sink,
        );
        let delivered = delivered.lock().unwrap().unwrap();
        assert_eq!(delivered.played_frame, None, "reader-queued clock is stale");
        assert_eq!(delivered.age_us, 0);
        assert_eq!(delivered.consumed, 1);
        assert_eq!(control.0.queue.lock().unwrap().flow.consumed(), 1);
    }

    #[test]
    fn hardware_clock_ages_in_the_parent_queue() {
        let fresh = aged_feedback(feedback(), Duration::from_millis(200));
        assert_eq!(fresh.played_frame, Some(100));
        assert_eq!(fresh.age_us, 250_000);
        let stale = aged_feedback(feedback(), Duration::from_micros(200_001));
        assert_eq!(stale.played_frame, None);
        assert_eq!(stale.consumed, 0);
    }
    #[test]
    fn broker_cannot_credit_blocks_waiting_for_the_writer() {
        let control = control();
        for n in 0..pcm::WINDOW {
            assert!(control.offer(block(n)));
        }
        assert!(
            !control.credit(Feedback {
                consumed: pcm::WINDOW,
                played_frame: None,
                ..feedback()
            }),
            "queued PCM has not been dispatched"
        );
        assert!(control.0.queue.lock().unwrap().dispatch().is_some());
        assert!(control.credit(Feedback {
            consumed: 1,
            played_frame: None,
            ..feedback()
        }));
        assert!(!control.credit(Feedback {
            consumed: 2,
            played_frame: None,
            ..feedback()
        }));
    }

    #[test]
    fn relay_queue_is_bounded_independently_of_flow_credit() {
        let control = control();
        for n in 0..pcm::WINDOW {
            assert!(control.offer(block(n)));
        }
        control
            .0
            .queue
            .lock()
            .unwrap()
            .flow
            .credit(Feedback {
                consumed: pcm::WINDOW,
                played_frame: None,
                ..feedback()
            })
            .unwrap();
        assert!(
            !control.offer(block(pcm::WINDOW)),
            "broker credit cannot grow a stalled relay queue"
        );
        assert_eq!(control.0.queue.lock().unwrap().flow.offered(), pcm::WINDOW);
    }

    #[test]
    fn relay_rejects_excess_credit_old_generations_and_repeated_eof() {
        let control = control();
        for n in 0..pcm::WINDOW {
            assert!(control.offer(block(n)));
        }
        assert!(!control.offer(block(pcm::WINDOW)));
        assert_eq!(
            control.0.queue.lock().unwrap().blocks.len(),
            pcm::WINDOW as usize
        );
        let mut old = feedback();
        old.generation = 6;
        old.consumed = 1;
        assert!(control.0.queue.lock().unwrap().flow.credit(old).is_err());
        assert!(control.offer(Message::End {
            generation: 7,
            sequence: pcm::WINDOW
        }));
        assert!(!control.offer(Message::End {
            generation: 7,
            sequence: pcm::WINDOW
        }));
        assert!(!control.offer(Message::Open(pcm::Open {
            generation: 8,
            start_frame: 0,
            end_frame: 48_000,
            muted: false
        })));
    }
    #[test]
    fn a_retired_broker_drains_only_the_already_credited_window() {
        let control = control();
        assert!(control.offer(block(0)));
        assert!(control.credit(Feedback {
            failed: true,
            played_frame: None,
            ..feedback()
        }));
        control.command(PlayCommand::Close);
        for n in 1..pcm::WINDOW {
            assert!(control.offer(block(n)), "in-flight PCM must not fail video");
        }
        assert!(!control.offer(block(pcm::WINDOW)));
        assert!(control.0.queue.lock().unwrap().blocks.is_empty());
    }

    #[test]
    fn opening_commands_follow_the_new_broker_once_without_becoming_heartbeats() {
        let control = control();
        let mut commands = Commands::default();
        commands.forward(
            None,
            Some(PlayCommand::Play),
            Some(PlayCommand::Mute),
            false,
            false,
        );
        commands.forward(
            None,
            Some(PlayCommand::Pause),
            Some(PlayCommand::Unmute),
            false,
            false,
        );
        commands.forward(Some(&control), None, None, false, false);
        {
            let mut queue = control.0.queue.lock().unwrap();
            assert_eq!(queue.play.take(), Some(PlayCommand::Pause));
            assert_eq!(queue.mute.take(), Some(PlayCommand::Unmute));
            assert!(!queue.heartbeat);
        }
        commands.forward(Some(&control), None, None, false, false);
        assert!(
            control.0.queue.lock().unwrap().play.is_none(),
            "no command replay on feedback"
        );
        commands.forward(Some(&control), Some(PlayCommand::Play), None, true, false);
        assert_eq!(
            control.0.queue.lock().unwrap().play,
            Some(PlayCommand::Play)
        );
        assert!(control.0.queue.lock().unwrap().heartbeat);
        commands.forward(Some(&control), None, None, false, true);
        assert!(control.0.cancel.is_cancelled());
    }

    #[test]
    fn controls_coalesce_and_only_commands_supply_heartbeats() {
        let control = control();
        assert!(control.offer(block(0)));
        control.command(PlayCommand::Play);
        control.command(PlayCommand::Pause);
        control.command(PlayCommand::Mute);
        control.command(PlayCommand::Unmute);
        let queue = control.0.queue.lock().unwrap();
        assert_eq!(queue.play, Some(PlayCommand::Pause));
        assert_eq!(queue.mute, Some(PlayCommand::Unmute));
        assert!(!queue.heartbeat);
        drop(queue);
        control.command(PlayCommand::Heartbeat);
        assert!(control.0.queue.lock().unwrap().heartbeat);
        control.command(PlayCommand::Close);
        assert!(control.0.cancel.is_cancelled());
        assert!(!control.offer(block(pcm::WINDOW)));
    }
}
