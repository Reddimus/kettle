//! Decoded sound only, relayed between one decoder and its playback broker.

use crate::{MAX_VIDEO_DURATION_MS, ValidationError};

pub const FRAMES: usize = 960;
pub const BYTES: usize = FRAMES * 2 * size_of::<f32>();
pub const WINDOW: u64 = 4;
const MAX_BLOCKS: u64 = MAX_VIDEO_DURATION_MS.div_ceil(20) + 2;
const MAX_FRAME: u64 = MAX_VIDEO_DURATION_MS * 48 + FRAMES as u64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Open {
    pub generation: u64,
    pub start_frame: u64,
    pub end_frame: u64,
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub generation: u64,
    pub sequence: u64,
    pub at_frame: u64,
    /// Exactly 20 ms, stereo f32le/48 kHz. No encoded format is accepted.
    pub samples: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feedback {
    pub generation: u64,
    pub consumed: u64,
    pub played_frame: Option<u64>,
    pub age_us: u32,
    pub done: bool,
    pub failed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Open(Open),
    Block(Block),
    End { generation: u64, sequence: u64 },
    Feedback(Feedback),
}

impl Message {
    pub fn validate(&self) -> Result<(), ValidationError> {
        let valid = match self {
            Self::Open(open) => {
                open.generation != 0
                    && open.start_frame <= open.end_frame
                    && open.end_frame <= MAX_FRAME - FRAMES as u64
            }
            Self::Block(block) => valid_block(block),
            Self::End {
                generation,
                sequence,
            } => *generation != 0 && *sequence <= MAX_BLOCKS,
            Self::Feedback(feedback) => {
                feedback.generation != 0
                    && feedback.consumed <= MAX_BLOCKS
                    && feedback.played_frame.is_none_or(|frame| frame <= MAX_FRAME)
                    && feedback.age_us <= 250_000
                    && (!feedback.failed || feedback.played_frame.is_none())
            }
        };
        if valid {
            Ok(())
        } else {
            Err(ValidationError::BadParams)
        }
    }
}

fn valid_block(block: &Block) -> bool {
    valid_block_fields(
        block.generation,
        block.sequence,
        block.at_frame,
        &block.samples,
    )
}

pub(crate) fn valid_block_fields(
    generation: u64,
    sequence: u64,
    at_frame: u64,
    samples: &[u8],
) -> bool {
    generation != 0
        && sequence <= MAX_BLOCKS
        && at_frame <= MAX_FRAME - FRAMES as u64
        && valid_samples(samples)
}

pub fn valid_samples(bytes: &[u8]) -> bool {
    bytes.len() == BYTES
        && bytes
            .as_chunks::<4>()
            .0
            .iter()
            .all(|n| f32::from_le_bytes(*n).is_finite())
}

/// Cumulative credits avoid duplicated buffer identifiers. Every broker is
/// a new generation; retiring a seek never resets a live stream's clock.
#[derive(Debug)]
pub struct Flow {
    open: Open,
    sent: u64,
    consumed: u64,
    next_frame: u64,
    ended: bool,
    played: Option<u64>,
}

impl Flow {
    pub fn new(open: Open) -> Result<Self, ValidationError> {
        Message::Open(open.clone()).validate()?;
        Ok(Self {
            next_frame: open.start_frame,
            open,
            sent: 0,
            consumed: 0,
            ended: false,
            played: None,
        })
    }

    pub fn offer(&mut self, block: &Block) -> Result<(), ValidationError> {
        if self.ended
            || !valid_block(block)
            || block.generation != self.open.generation
            || block.sequence != self.sent
            || self.sent - self.consumed >= WINDOW
            || block.at_frame < self.next_frame
            || block.at_frame > self.open.end_frame
        {
            return Err(ValidationError::BadParams);
        }
        self.sent += 1;
        self.next_frame = block.at_frame + FRAMES as u64;
        Ok(())
    }

    pub fn credit(&mut self, feedback: Feedback) -> Result<(), ValidationError> {
        Message::Feedback(feedback).validate()?;
        if feedback
            .played_frame
            .zip(self.played)
            .is_some_and(|(now, last)| now < last)
            || feedback.generation != self.open.generation
            || feedback.consumed < self.consumed
            || feedback.consumed > self.sent
            || feedback.played_frame.is_some_and(|frame| {
                frame < self.open.start_frame || frame > self.open.end_frame + FRAMES as u64
            })
        {
            return Err(ValidationError::BadParams);
        }
        if feedback.played_frame.is_some() {
            self.played = feedback.played_frame;
        }
        self.consumed = feedback.consumed;
        self.ended |= feedback.failed;
        Ok(())
    }

    pub fn end(&mut self, generation: u64, sequence: u64) -> Result<(), ValidationError> {
        if self.ended || generation != self.open.generation || sequence != self.sent {
            return Err(ValidationError::BadParams);
        }
        self.ended = true;
        Ok(())
    }

    pub fn offered(&self) -> u64 {
        self.sent
    }
    pub fn consumed(&self) -> u64 {
        self.consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Open {
        Open {
            generation: 7,
            start_frame: 48_000,
            end_frame: 96_000,
            muted: false,
        }
    }
    fn block(n: u64) -> Block {
        Block {
            generation: 7,
            sequence: n,
            at_frame: 48_000 + n * FRAMES as u64,
            samples: vec![0; BYTES],
        }
    }
    fn feedback(consumed: u64) -> Feedback {
        Feedback {
            generation: 7,
            consumed,
            played_frame: None,
            age_us: 0,
            done: false,
            failed: false,
        }
    }

    #[test]
    fn a_generation_clock_never_runs_backwards_even_across_pause() {
        let mut flow = Flow::new(open()).unwrap();
        let mut heard = Feedback {
            generation: 7,
            consumed: 0,
            played_frame: Some(49_000),
            age_us: 0,
            done: false,
            failed: false,
        };
        flow.credit(heard).unwrap();
        heard.played_frame = None;
        flow.credit(heard).unwrap();
        heard.played_frame = Some(48_999);
        assert!(flow.credit(heard).is_err());
        heard.played_frame = Some(49_000);
        flow.credit(heard).unwrap();
    }

    #[test]
    fn credits_bound_pcm_and_do_not_allow_replay() {
        let mut flow = Flow::new(open()).unwrap();
        for n in 0..WINDOW {
            flow.offer(&block(n)).unwrap();
        }
        assert!(flow.offer(&block(WINDOW)).is_err(), "unbounded PCM queue");
        flow.credit(feedback(1)).unwrap();
        flow.offer(&block(WINDOW)).unwrap();
        assert!(flow.offer(&block(WINDOW)).is_err(), "replayed packet");
        assert!(
            flow.credit(feedback(WINDOW + 2)).is_err(),
            "invented credit"
        );
        assert!(flow.credit(feedback(0)).is_err(), "credit moved backwards");
    }

    #[test]
    fn seek_generations_and_packet_times_cannot_be_reused() {
        let mut flow = Flow::new(open()).unwrap();
        flow.offer(&block(0)).unwrap();
        assert!(
            flow.offer(&Block {
                generation: 6,
                ..block(1)
            })
            .is_err()
        );
        assert!(
            flow.credit(Feedback {
                generation: 8,
                ..feedback(1)
            })
            .is_err()
        );
        assert!(
            flow.offer(&Block {
                at_frame: 48_000,
                ..block(1)
            })
            .is_err(),
            "overlapping sound"
        );
        assert!(
            flow.offer(&Block {
                at_frame: 96_001,
                ..block(1)
            })
            .is_err(),
            "past the video"
        );
        assert!(flow.end(7, 0).is_err(), "lost packet at EOF");
        flow.end(7, 1).unwrap();
        assert!(
            flow.offer(&block(1)).is_err(),
            "retired stream accepted PCM"
        );
        assert!(flow.end(7, 1).is_err(), "repeated EOF");
    }

    #[test]
    fn only_fixed_finite_pcm_blocks_are_accepted() {
        let mut flow = Flow::new(open()).unwrap();
        flow.offer(&block(0)).unwrap();
        for bytes in [
            vec![],
            vec![0; BYTES - 4],
            vec![0; BYTES + 4],
            f32::NAN.to_le_bytes().repeat(BYTES / 4),
            f32::INFINITY.to_le_bytes().repeat(BYTES / 4),
        ] {
            assert!(
                flow.offer(&Block {
                    samples: bytes,
                    ..block(1)
                })
                .is_err()
            );
        }
        assert!(
            Message::Feedback(Feedback {
                played_frame: Some(1),
                failed: true,
                ..feedback(1)
            })
            .validate()
            .is_err()
        );
    }
}
