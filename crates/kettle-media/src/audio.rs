//! A playback session's sound, as a player decodes it for the worker:
//! interleaved stereo 32-bit float samples at 48 kHz, each block placed on
//! the video's timeline. The worker plays it; a player only decodes.

use crate::FailureCode;

/// Frames a second of every session's sound.
pub const SAMPLE_RATE: u32 = 48_000;
/// Samples in each frame: left, then right.
pub const CHANNELS: usize = 2;
/// The most frames a source keeps between blocks: a quarter of a second,
/// more than any compressed packet decodes to.
pub const MAX_PENDING_FRAMES: usize = 12_000;

/// A block a source decoded: how many frames it wrote, and where the first
/// plays, in frames from the start of the video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioBlock {
    pub frames: usize,
    pub at_frame: u64,
}

/// A nonblocking source distinguishes a temporary lack of PCM from EOF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioRead {
    Block(AudioBlock),
    Pending,
    End,
}

/// A session's sound, decoded in order a block at a time.
pub trait AudioSource: Send {
    /// An interrupt for a source that can block on another process. Closing
    /// sound calls it before joining the feeding thread; video keeps running.
    fn cancellation(&self) -> Option<std::sync::Arc<dyn Fn() + Send + Sync>> {
        None
    }

    /// IPC sources override this so pause/mute can run while awaiting PCM.
    fn poll_block(&mut self, samples: &mut [f32]) -> Result<AudioRead, FailureCode> {
        self.next_block(samples)
            .map(|block| block.map_or(AudioRead::End, AudioRead::Block))
    }

    /// Write the next frames into `samples`, `CHANNELS` samples each, as
    /// many as fit, and say how many and where they play; `Ok(None)` once
    /// the sound has ended. A block never starts before the one before it
    /// ended.
    fn next_block(&mut self, samples: &mut [f32]) -> Result<Option<AudioBlock>, FailureCode>;
}

/// `value` units of `timescale` a second, as frames of the sound from the
/// start, rounded to the nearest; `None` for a time before the start, a
/// timescale that is not one, or a time past any video's length.
pub fn frames_at(value: i64, timescale: i32) -> Option<u64> {
    if timescale <= 0 || value < 0 {
        return None;
    }
    let scale = i128::from(timescale);
    let frames = (i128::from(value) * i128::from(SAMPLE_RATE) + scale / 2) / scale;
    let max = u128::from(crate::MAX_VIDEO_DURATION_MS) * u128::from(SAMPLE_RATE) / 1000;
    u64::try_from(frames)
        .ok()
        .filter(|frames| u128::from(*frames) <= max)
}

/// Drop the frames of `samples` (interleaved, `CHANNELS` each), the first
/// of which plays at `at`, that play before `floor`, and say where the
/// first left plays: a decoder may give a little before the start asked
/// for, or overlap the block before.
pub fn trim(samples: &mut Vec<f32>, at: u64, floor: u64) -> u64 {
    let frames = samples.len() / CHANNELS;
    let skip = usize::try_from(floor.saturating_sub(at))
        .unwrap_or(usize::MAX)
        .min(frames);
    samples.drain(..skip * CHANNELS);
    at + skip as u64
}

/// The frame `ms` into the video.
pub fn frame_of_ms(ms: u64) -> u64 {
    ms.saturating_mul(u64::from(SAMPLE_RATE)) / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Times map to frames to the nearest, whatever their scale, and a time
    /// that is not one maps to none.
    #[test]
    fn times_map_to_frames_to_the_nearest() {
        assert_eq!(frames_at(0, 44_100), Some(0));
        assert_eq!(frames_at(44_100, 44_100), Some(48_000));
        assert_eq!(frames_at(1, 44_100), Some(1));
        assert_eq!(frames_at(1_001, 30_000), Some(1_602));
        assert_eq!(frames_at(600, 600), Some(48_000));
        assert_eq!(frames_at(-1, 600), None);
        assert_eq!(frames_at(1, 0), None);
        assert_eq!(frames_at(i64::MAX, 1), None);
        assert_eq!(frame_of_ms(2_500), 120_000);
        assert_eq!(frame_of_ms(u64::MAX), u64::MAX / 1000);
    }

    /// Frames before the floor go, and where the rest plays moves with them;
    /// a block past the floor keeps every frame, and one wholly before it
    /// keeps none.
    #[test]
    fn frames_before_the_floor_are_dropped() {
        let block = |frames: usize| (0..frames * CHANNELS).map(|n| n as f32).collect::<Vec<_>>();
        let mut samples = block(50);
        assert_eq!(trim(&mut samples, 100, 120), 120);
        assert_eq!(samples.len(), 30 * CHANNELS);
        assert_eq!(samples[0], (20 * CHANNELS) as f32);
        let mut samples = block(50);
        assert_eq!(trim(&mut samples, 100, 90), 100);
        assert_eq!(samples.len(), 50 * CHANNELS);
        let mut samples = block(50);
        assert_eq!(trim(&mut samples, 100, 1_000), 150);
        assert!(samples.is_empty());
    }
}
