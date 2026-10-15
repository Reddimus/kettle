//! Linux playback broker: fixed PCM frames and commands, never media files.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use kettle_media::audio::{AudioBlock, AudioRead, AudioSource, CHANNELS};
use kettle_media::pcm::{self, Feedback, Flow, Message};
use kettle_media::wire::{Direction, Frame, HEADER_BYTES, read_frame_within, write_frame};
use kettle_media::{FailureCode, PlayCommand};

use crate::sound::{Sound, SoundWish, Speaker};
use crate::worker::{EXIT_PROTOCOL, Watchdog};

const TICK: Duration = Duration::from_millis(20);
const LEASE: Duration = Duration::from_secs(5);
const FRAME_LIMIT: usize = HEADER_BYTES + pcm::BYTES + 64;

type Queue = Arc<(Mutex<State>, Condvar)>;
struct State {
    flow: Flow,
    blocks: VecDeque<pcm::Block>,
    ended: bool,
    closed: bool,
    broken: bool,
    last: Instant,
}

pub(crate) fn serve(
    open: pcm::Open,
    input: impl Read + Send + 'static,
    output: &mut impl Write,
    watchdog: &Watchdog,
) -> i32 {
    let Ok(flow) = Flow::new(open.clone()) else {
        return EXIT_PROTOCOL;
    };
    watchdog.arm(Duration::from_secs(4));
    // Only this role establishes the service connection. Its encoded-media
    // decoder has already confined itself without socket access.
    let speaker = match kettle_media_native::pulse::Output::open(open.muted) {
        Ok(output) => output,
        Err(_) => return unavailable(output, &open),
    };
    if !watchdog.confined() || kettle_media_native::sandbox::confine(&Default::default()).is_err() {
        return unavailable(output, &open);
    }
    let duration_ms = (open.end_frame - open.start_frame).div_ceil(48);
    if crate::early_unix::limit_session_cpu(duration_ms).is_err() {
        return unavailable(output, &open);
    }
    let wish = SoundWish::new(open.muted);
    let queue = Arc::new((
        Mutex::new(State {
            flow,
            blocks: VecDeque::new(),
            ended: false,
            closed: false,
            broken: false,
            last: Instant::now(),
        }),
        Condvar::new(),
    ));
    if reader(input, Arc::clone(&queue), wish.clone()).is_err() {
        return unavailable(output, &open);
    }
    let source = Source {
        queue: Arc::clone(&queue),
        generation: open.generation,
        carry: None,
        offset: 0,
    };
    let Ok(sound) = Sound::start(
        Box::new(PulseSpeaker {
            output: speaker,
            failed: false,
        }),
        Box::new(source),
        open.start_frame,
        wish,
    ) else {
        return unavailable(output, &open);
    };
    let mut last_feedback = None;
    loop {
        watchdog.arm(LEASE);
        let (consumed, closed, broken, last) = {
            let state = queue.0.lock().unwrap_or_else(PoisonError::into_inner);
            (
                state.flow.consumed(),
                state.closed,
                state.broken,
                state.last,
            )
        };
        if broken {
            return EXIT_PROTOCOL;
        }
        if closed {
            return 0;
        }
        if last.elapsed() >= LEASE {
            return crate::worker::EXIT_TIMEOUT;
        }
        let feedback = clock_feedback(&open, consumed, sound.heard(), sound.done(), sound.failed());
        if last_feedback != Some(feedback) {
            if write_frame(
                output,
                &Frame::Pcm(Message::Feedback(feedback)),
                Direction::WorkerToParent,
            )
            .is_err()
            {
                return EXIT_PROTOCOL;
            }
            last_feedback = Some(feedback);
        }
        if feedback.failed {
            return 0;
        }
        let state = queue.0.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = queue
            .1
            .wait_timeout(state, TICK)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

fn clock_feedback(
    open: &pcm::Open,
    consumed: u64,
    heard: Option<crate::sound::Heard>,
    done: bool,
    failed: bool,
) -> Feedback {
    let heard = heard.filter(|heard| !failed && heard.at.elapsed() <= Duration::from_millis(250));
    Feedback {
        generation: open.generation,
        consumed,
        played_frame: heard
            .map(|heard| (heard.frame.max(0.0) as u64).min(open.end_frame + pcm::FRAMES as u64)),
        age_us: heard.map_or(0, |heard| {
            heard.at.elapsed().as_micros().min(250_000) as u32
        }),
        done,
        failed,
    }
}

fn unavailable(output: &mut impl Write, open: &pcm::Open) -> i32 {
    let feedback = Feedback {
        generation: open.generation,
        consumed: 0,
        played_frame: None,
        age_us: 0,
        done: false,
        failed: true,
    };
    if write_frame(
        output,
        &Frame::Pcm(Message::Feedback(feedback)),
        Direction::WorkerToParent,
    )
    .is_ok()
    {
        0
    } else {
        EXIT_PROTOCOL
    }
}

fn reader(
    mut input: impl Read + Send + 'static,
    queue: Queue,
    wish: SoundWish,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("kettle-pcm-read".into())
        .spawn(move || {
            loop {
                let read = read_frame_within(&mut input, Direction::ParentToWorker, FRAME_LIMIT);
                let mut state = queue.0.lock().unwrap_or_else(PoisonError::into_inner);
                let accepted = match read {
                    Ok(Some(Frame::Pcm(Message::Block(block)))) => {
                        if state.flow.offer(&block).is_err() {
                            false
                        } else {
                            state.blocks.push_back(block);
                            true
                        }
                    }
                    Ok(Some(Frame::Pcm(Message::End {
                        generation,
                        sequence,
                    }))) => {
                        if state.flow.end(generation, sequence).is_err() {
                            false
                        } else {
                            state.ended = true;
                            true
                        }
                    }
                    Ok(Some(Frame::PlayCommand(command))) => {
                        wish.hear(command);
                        state.last = Instant::now();
                        state.closed |= command == PlayCommand::Close;
                        true
                    }
                    Ok(None) => {
                        state.closed = true;
                        true
                    }
                    _ => false,
                };
                state.broken |= !accepted;
                let stop = state.broken || state.closed;
                queue.1.notify_all();
                if stop {
                    wish.close();
                    return;
                }
            }
        })
        .map(drop)
}

struct Source {
    queue: Queue,
    generation: u64,
    carry: Option<pcm::Block>,
    offset: usize,
}

impl AudioSource for Source {
    fn cancellation(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let queue = Arc::clone(&self.queue);
        Some(Arc::new(move || {
            queue
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .closed = true;
            queue.1.notify_all();
        }))
    }

    fn poll_block(&mut self, samples: &mut [f32]) -> Result<AudioRead, FailureCode> {
        self.read(samples, false)
    }
    fn next_block(&mut self, samples: &mut [f32]) -> Result<Option<AudioBlock>, FailureCode> {
        self.read(samples, true).map(|read| match read {
            AudioRead::Block(block) => Some(block),
            AudioRead::End => None,
            AudioRead::Pending => unreachable!("blocking read waits for PCM"),
        })
    }
}

impl Source {
    fn read(&mut self, samples: &mut [f32], wait: bool) -> Result<AudioRead, FailureCode> {
        if samples.len() < CHANNELS {
            return Err(FailureCode::BadParams);
        }
        if self.carry.is_none() {
            let mut state = self.queue.0.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if state.closed || state.broken {
                    return Err(FailureCode::WorkerUnavailable);
                }
                if let Some(block) = state.blocks.pop_front() {
                    state
                        .flow
                        .credit(Feedback {
                            generation: self.generation,
                            consumed: block.sequence + 1,
                            played_frame: None,
                            age_us: 0,
                            done: false,
                            failed: false,
                        })
                        .map_err(|_| FailureCode::BadParams)?;
                    self.carry = Some(block);
                    self.offset = 0;
                    self.queue.1.notify_all();
                    break;
                }
                if state.ended {
                    return Ok(AudioRead::End);
                }
                if !wait {
                    return Ok(AudioRead::Pending);
                }
                state = self
                    .queue
                    .1
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
        let block = self.carry.as_ref().expect("a block was taken above");
        let frames = (samples.len() / CHANNELS).min(pcm::FRAMES - self.offset);
        let from = self.offset * 8;
        for (sample, bytes) in samples
            .iter_mut()
            .zip(block.samples[from..from + frames * 8].as_chunks::<4>().0)
        {
            *sample = f32::from_le_bytes(*bytes);
        }
        let at_frame = block.at_frame + self.offset as u64;
        self.offset += frames;
        if self.offset == pcm::FRAMES {
            self.carry = None;
        }
        Ok(AudioRead::Block(AudioBlock { at_frame, frames }))
    }
}

struct PulseSpeaker {
    output: kettle_media_native::pulse::Output,
    failed: bool,
}
impl Speaker for PulseSpeaker {
    fn take_free(&mut self) -> Option<usize> {
        (!self.failed).then(|| self.output.take_free()).flatten()
    }
    fn enqueue(&mut self, index: usize, samples: &[f32]) -> Result<(), ()> {
        self.output.enqueue(index, samples).map_err(drop)
    }
    fn start(&mut self) -> Result<(), ()> {
        self.output.start().map_err(drop)
    }
    fn pause(&mut self) -> Result<(), ()> {
        self.output.pause().map_err(drop)
    }
    fn set_muted(&mut self, muted: bool) {
        if self.output.set_muted(muted).is_err() {
            let _ = self.output.pause();
            self.failed = true;
        }
    }
    fn played(&self) -> Option<(f64, Instant)> {
        (!self.failed).then(|| self.output.played()).flatten()
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    #[test]
    fn pcm_data_never_renews_the_command_lease() {
        let open = pcm::Open {
            generation: 7,
            start_frame: 0,
            end_frame: 48_000,
            muted: true,
        };
        let old = Instant::now() - Duration::from_secs(30);
        let queue = Arc::new((
            Mutex::new(State {
                flow: Flow::new(open).unwrap(),
                blocks: VecDeque::new(),
                ended: false,
                closed: false,
                broken: false,
                last: old,
            }),
            Condvar::new(),
        ));
        let (input, mut sent) = std::io::pipe().unwrap();
        reader(input, Arc::clone(&queue), SoundWish::new(true)).unwrap();
        let send = |sent: &mut std::io::PipeWriter, frame: Frame| {
            write_frame(sent, &frame, Direction::ParentToWorker).unwrap();
        };
        let wait_for = |condition: &dyn Fn(&State) -> bool| {
            let state = queue.0.lock().unwrap();
            let (state, timeout) = queue
                .1
                .wait_timeout_while(state, Duration::from_secs(5), |state| !condition(state))
                .unwrap();
            assert!(!timeout.timed_out(), "PCM reader did not accept its frame");
            assert!(!state.broken);
            state.last
        };
        send(
            &mut sent,
            Frame::Pcm(Message::Block(pcm::Block {
                generation: 7,
                sequence: 0,
                at_frame: 0,
                samples: vec![0; pcm::BYTES],
            })),
        );
        assert_eq!(
            wait_for(&|state| state.blocks.len() == 1),
            old,
            "PCM samples are not playback commands"
        );
        send(
            &mut sent,
            Frame::Pcm(Message::End {
                generation: 7,
                sequence: 1,
            }),
        );
        assert_eq!(
            wait_for(&|state| state.ended),
            old,
            "PCM EOF is not a heartbeat"
        );
        send(&mut sent, Frame::PlayCommand(PlayCommand::Heartbeat));
        assert!(wait_for(&|state| state.last != old) > old);
        send(&mut sent, Frame::PlayCommand(PlayCommand::Close));
        wait_for(&|state| state.closed);
    }

    #[test]
    fn feedback_carries_only_a_fresh_bounded_hardware_clock() {
        let open = pcm::Open {
            generation: 7,
            start_frame: 0,
            end_frame: 48_000,
            muted: false,
        };
        let old = crate::sound::Heard {
            frame: 100.0,
            at: Instant::now() - Duration::from_secs(1),
        };
        assert_eq!(
            clock_feedback(&open, 2, Some(old), false, false).played_frame,
            None
        );
        let fresh = crate::sound::Heard {
            frame: 100_000.0,
            at: Instant::now(),
        };
        let heard = clock_feedback(&open, 2, Some(fresh), true, false);
        assert_eq!(
            heard.played_frame,
            Some(open.end_frame + pcm::FRAMES as u64)
        );
        assert_eq!(heard.consumed, 2);
        assert!(heard.done);
        assert_eq!(
            clock_feedback(&open, 2, Some(fresh), false, true).played_frame,
            None
        );
    }
}
