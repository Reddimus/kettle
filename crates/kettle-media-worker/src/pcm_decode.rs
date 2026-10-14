//! The confined decoder sends bounded PCM; only the other worker plays it.

use std::io::Write;
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use kettle_media::audio::{AudioSource, CHANNELS};
use kettle_media::pcm::{self, Feedback, Flow, Message};
use kettle_media::wire::{Direction, Frame, write_frame};
use kettle_media::{Opened, PlayCommand};

use crate::play::Event;
use crate::sound::{Heard, RemoteClock, Sound, SoundWish};

struct Info {
    flow: Option<Flow>,
    feedback: Option<Feedback>,
    heard: Option<Heard>,
    missing_since: Option<Instant>,
    progress: Option<(u64, Instant)>,
    closed: bool,
    failed: bool,
}

pub(crate) struct State {
    generation: u64,
    wish: SoundWish,
    info: Mutex<Info>,
    changed: Condvar,
}

impl State {
    pub(crate) fn new(wish: SoundWish) -> Arc<Self> {
        Arc::new(Self {
            generation: u64::from(std::process::id()),
            wish,
            info: Mutex::new(Info {
                flow: None,
                feedback: None,
                heard: None,
                missing_since: None,
                progress: None,
                closed: false,
                failed: false,
            }),
            changed: Condvar::new(),
        })
    }

    /// Feedback is not a heartbeat: it never renews the parent's lease.
    pub(crate) fn hear(&self, feedback: Feedback) -> bool {
        let mut info = self.info.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(flow) = &mut info.flow else {
            return false;
        };
        if flow.credit(feedback).is_err() {
            return false;
        }
        info.failed |= feedback.failed;
        if !self.wish.playing() {
            info.progress = None;
        } else if let Some(frame) = feedback.played_frame
            && info.progress.is_none_or(|(last, _)| frame > last)
        {
            info.progress = Some((frame, Instant::now()));
        }
        info.heard = feedback.played_frame.map(|frame| Heard {
            frame: frame as f64,
            at: Instant::now()
                .checked_sub(Duration::from_micros(u64::from(feedback.age_us)))
                .unwrap_or_else(Instant::now),
        });
        if info.heard.is_some() || !self.wish.playing() {
            info.missing_since = None;
        } else {
            info.missing_since.get_or_insert_with(Instant::now);
        }
        info.feedback = Some(feedback);
        self.changed.notify_all();
        true
    }

    pub(crate) fn command(&self, command: PlayCommand) {
        if matches!(command, PlayCommand::Play | PlayCommand::Pause) {
            let mut info = self.info.lock().unwrap_or_else(PoisonError::into_inner);
            info.heard = None;
            info.progress = None;
            info.missing_since =
                (command == PlayCommand::Play && info.feedback.is_some()).then(Instant::now);
        }
    }

    fn fail(&self) {
        self.info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failed = true;
        self.changed.notify_all();
    }
}

impl RemoteClock for State {
    fn heard(&self) -> Option<Heard> {
        self.info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .heard
            .filter(|heard| self.wish.playing() && heard.at.elapsed() <= Duration::from_millis(250))
    }
    fn failed(&self) -> bool {
        let info = self.info.lock().unwrap_or_else(PoisonError::into_inner);
        info.failed
            || info.closed
            || (self.wish.playing()
                && (info
                    .progress
                    .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(1))
                    || info
                        .missing_since
                        .is_some_and(|at| at.elapsed() > Duration::from_secs(1))
                    || info
                        .heard
                        .is_some_and(|heard| heard.at.elapsed() > Duration::from_secs(1))))
    }
    fn done(&self) -> bool {
        self.info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .feedback
            .is_some_and(|feedback| feedback.done)
    }
    fn close(&self) {
        self.info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.changed.notify_all();
    }
}

pub(crate) fn start(
    mut source: Box<dyn AudioSource>,
    opened: &Opened,
    wish: SoundWish,
    state: Arc<State>,
    events: mpsc::SyncSender<Event>,
    output: &mut impl Write,
    deadline: Instant,
) -> Option<Sound> {
    let cancel = source.cancellation()?;
    let open = pcm::Open {
        generation: state.generation,
        start_frame: opened.start_ms * 48,
        end_frame: opened.info.duration_ms * 48,
        muted: wish.muted(),
    };
    state
        .info
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .flow = Some(Flow::new(open.clone()).ok()?);
    write_frame(
        output,
        &Frame::Pcm(Message::Open(open)),
        Direction::WorkerToParent,
    )
    .ok()?;
    let mut info = state.info.lock().unwrap_or_else(PoisonError::into_inner);
    let until = deadline.min(Instant::now() + Duration::from_secs(3));
    while info.feedback.is_none() && !info.closed && !info.failed {
        let left = until.checked_duration_since(Instant::now())?;
        info = state
            .changed
            .wait_timeout(info, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    if info.failed || info.closed {
        return None;
    }
    drop(info);
    let producer = Arc::clone(&state);
    let thread = std::thread::Builder::new()
        .name("kettle-pcm-decode".into())
        .spawn(move || {
            let mut samples = vec![0.0; pcm::FRAMES * CHANNELS];
            loop {
                let mut info = producer.info.lock().unwrap_or_else(PoisonError::into_inner);
                while !info.closed
                    && !info.failed
                    && info
                        .flow
                        .as_ref()
                        .is_some_and(|flow| flow.offered() - flow.consumed() >= pcm::WINDOW)
                {
                    info = producer
                        .changed
                        .wait(info)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                if info.closed || info.failed {
                    return;
                }
                let sequence = info.flow.as_ref().expect("opened PCM flow").offered();
                drop(info);
                let message = match source.next_block(&mut samples) {
                    Ok(Some(block)) if block.frames == pcm::FRAMES => Message::Block(pcm::Block {
                        generation: producer.generation,
                        sequence,
                        at_frame: block.at_frame,
                        samples: samples.iter().flat_map(|n| n.to_le_bytes()).collect(),
                    }),
                    Ok(None) => Message::End {
                        generation: producer.generation,
                        sequence,
                    },
                    _ => {
                        producer.fail();
                        let _ = events.try_send(Event::Asked);
                        return;
                    }
                };
                let ended = matches!(message, Message::End { .. });
                let mut info = producer.info.lock().unwrap_or_else(PoisonError::into_inner);
                let flow = info.flow.as_mut().expect("opened PCM flow");
                let accepted = match &message {
                    Message::Block(block) => flow.offer(block),
                    Message::End {
                        generation,
                        sequence,
                    } => flow.end(*generation, *sequence),
                    _ => unreachable!("only PCM or EOF from decoder"),
                };
                if accepted.is_err() {
                    info.failed = true;
                    drop(info);
                    let _ = events.try_send(Event::Asked);
                    return;
                }
                drop(info);
                let mut event = Event::Pcm(message);
                loop {
                    let info = producer.info.lock().unwrap_or_else(PoisonError::into_inner);
                    if info.closed || info.failed {
                        return;
                    }
                    drop(info);
                    match events.try_send(event) {
                        Ok(()) => break,
                        Err(mpsc::TrySendError::Disconnected(_)) => return,
                        Err(mpsc::TrySendError::Full(returned)) => {
                            event = returned;
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                }
                if ended {
                    return;
                }
            }
        })
        .ok()?;
    Some(Sound::remote(wish, state, thread, cancel))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened() -> Arc<State> {
        let state = State::new(SoundWish::new(false));
        state.info.lock().unwrap().flow = Some(
            Flow::new(pcm::Open {
                generation: state.generation,
                start_frame: 0,
                end_frame: 48_000,
                muted: false,
            })
            .unwrap(),
        );
        state
    }
    fn feedback(state: &State) -> Feedback {
        Feedback {
            generation: state.generation,
            consumed: 0,
            played_frame: Some(100),
            age_us: 0,
            done: false,
            failed: false,
        }
    }
    #[test]
    fn remote_clock_is_fresh_only_while_playing_and_times_out_when_lost() {
        let state = opened();
        assert!(state.hear(feedback(&state)));
        assert!(state.heard().is_none(), "paused clock must not pace frames");
        state.wish.hear(PlayCommand::Play);
        state.command(PlayCommand::Play);
        assert!(
            state.heard().is_none(),
            "resume needs a new hardware timestamp"
        );
        assert!(state.hear(feedback(&state)));
        assert!(state.heard().is_some());
        state.info.lock().unwrap().heard.as_mut().unwrap().at =
            Instant::now() - Duration::from_millis(300);
        assert!(state.heard().is_none());
        assert!(!state.failed());
        state.info.lock().unwrap().heard.as_mut().unwrap().at =
            Instant::now() - Duration::from_secs(2);
        assert!(state.failed());
        state.wish.hear(PlayCommand::Pause);
        state.command(PlayCommand::Pause);
        assert!(!state.failed(), "paused sessions need no running clock");
    }
    #[test]
    fn a_fresh_but_nonadvancing_hardware_clock_is_not_progress() {
        let state = opened();
        state.wish.hear(PlayCommand::Play);
        state.command(PlayCommand::Play);
        assert!(state.hear(feedback(&state)));
        state.info.lock().unwrap().progress.as_mut().unwrap().1 =
            Instant::now() - Duration::from_secs(2);
        assert!(state.hear(feedback(&state)));
        assert!(state.failed(), "repeated timestamps cannot renew progress");
        let mut next = feedback(&state);
        next.played_frame = Some(200);
        assert!(state.hear(next));
        assert!(!state.failed());
        state.wish.hear(PlayCommand::Pause);
        state.command(PlayCommand::Pause);
        assert!(!state.failed());
    }

    #[test]
    fn feedback_cannot_invent_credit_or_use_another_generation() {
        let state = opened();
        let mut wrong = feedback(&state);
        wrong.generation += 1;
        assert!(!state.hear(wrong));
        wrong = feedback(&state);
        wrong.consumed = 1;
        assert!(!state.hear(wrong));
        wrong = feedback(&state);
        wrong.failed = true;
        wrong.played_frame = None;
        assert!(state.hear(wrong));
        assert!(state.failed());
        assert!(state.heard().is_none());
        state.close();
        assert!(state.info.lock().unwrap().closed);
    }
}
