//! A session's sound out of this machine (macOS): one AudioQueue the worker
//! owns, fed from a fixed set of buffers. Its callback, on the queue's own
//! thread, only marks the buffer it gives back as free: nothing there
//! decodes, allocates, locks or blocks. The queue's timeline says how much
//! of what was queued it has played, which is the clock a session's frames
//! follow once its sound plays.
//!
//! Every call into AudioToolbox here has its own SAFETY comment. The queue
//! is disposed, immediately, before the state its callback writes is
//! freed, so no callback can outlive it.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use kettle_media::audio::{CHANNELS, SAMPLE_RATE};
use objc2_audio_toolbox::{
    AudioQueueAllocateBuffer, AudioQueueBufferRef, AudioQueueCreateTimeline, AudioQueueDispose,
    AudioQueueEnqueueBuffer, AudioQueueGetCurrentTime, AudioQueueNewOutput, AudioQueuePause,
    AudioQueueRef, AudioQueueSetParameter, AudioQueueStart, AudioQueueTimelineRef,
    kAudioQueueParam_Volume,
};
use objc2_core_audio_types::{
    AudioStreamBasicDescription, AudioTimeStamp, AudioTimeStampFlags, kAudioFormatFlagIsFloat,
    kAudioFormatFlagIsPacked, kAudioFormatLinearPCM,
};

/// Frames in each buffer: 20 ms.
pub const BUFFER_FRAMES: usize = 960;
/// Buffers the queue holds at once: one playing, one next, one being
/// filled.
pub const BUFFERS: usize = 3;

/// The output could not be made or could not play: there is no sound, and
/// the session goes on silent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unavailable;

/// What the queue's callback writes: which buffers it has given back.
struct Freed {
    buffers: [AudioQueueBufferRef; BUFFERS],
    free: [AtomicBool; BUFFERS],
}

/// One session's sound output.
pub struct Output {
    queue: AudioQueueRef,
    timeline: AudioQueueTimelineRef,
    /// Boxed so the callback's pointer to it stays put; dropped only after
    /// the queue is disposed.
    freed: Box<Freed>,
    started: bool,
}

// SAFETY: the queue and its timeline are AudioToolbox objects that may be
// called from any one thread at a time; `Output` is owned and called by one
// thread, and the callback touches only the atomics in `freed`.
unsafe impl Send for Output {}

/// The queue's callback: the buffer it gives back is free to fill again.
unsafe extern "C-unwind" fn given_back(
    user: *mut c_void,
    _queue: AudioQueueRef,
    buffer: AudioQueueBufferRef,
) {
    // SAFETY: `user` is the `Freed` the queue was made with, which outlives
    // the queue; only its atomics are written.
    let freed = unsafe { &*user.cast::<Freed>() };
    if let Some(index) = freed.buffers.iter().position(|ours| *ours == buffer) {
        freed.free[index].store(true, Ordering::Release);
    }
}

impl Output {
    /// Make a queue for interleaved stereo 32-bit floats at 48 kHz, with its
    /// buffers and timeline, playing nothing yet; `muted` sets its volume
    /// to nothing.
    pub fn open(muted: bool) -> Result<Self, Unavailable> {
        let format = AudioStreamBasicDescription {
            mSampleRate: f64::from(SAMPLE_RATE),
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: (CHANNELS * size_of::<f32>()) as u32,
            mFramesPerPacket: 1,
            mBytesPerFrame: (CHANNELS * size_of::<f32>()) as u32,
            mChannelsPerFrame: CHANNELS as u32,
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        let mut freed = Box::new(Freed {
            buffers: [std::ptr::null_mut(); BUFFERS],
            free: [const { AtomicBool::new(true) }; BUFFERS],
        });
        let mut queue: AudioQueueRef = std::ptr::null_mut();
        // SAFETY: the format and the out pointer are valid for the call; the
        // user data is the boxed `Freed`, which outlives the queue (see
        // `Drop`); no run loop: the queue calls back on its own thread.
        let status = unsafe {
            AudioQueueNewOutput(
                NonNull::from(&format),
                Some(given_back),
                std::ptr::from_mut::<Freed>(&mut freed).cast(),
                None,
                None,
                0,
                NonNull::from(&mut queue),
            )
        };
        if status != 0 || queue.is_null() {
            return Err(Unavailable);
        }
        let mut output = Self {
            queue,
            timeline: std::ptr::null_mut(),
            freed,
            started: false,
        };
        let bytes = (BUFFER_FRAMES * CHANNELS * size_of::<f32>()) as u32;
        for index in 0..BUFFERS {
            let mut buffer: AudioQueueBufferRef = std::ptr::null_mut();
            // SAFETY: the queue is live and the out pointer valid.
            let status = unsafe {
                AudioQueueAllocateBuffer(output.queue, bytes, NonNull::from(&mut buffer))
            };
            if status != 0 || buffer.is_null() {
                return Err(Unavailable);
            }
            output.freed.buffers[index] = buffer;
        }
        // SAFETY: as above.
        let status =
            unsafe { AudioQueueCreateTimeline(output.queue, NonNull::from(&mut output.timeline)) };
        if status != 0 || output.timeline.is_null() {
            return Err(Unavailable);
        }
        output.set_muted(muted);
        Ok(output)
    }

    /// A buffer the queue has given back, now taken to be filled.
    pub fn take_free(&self) -> Option<usize> {
        self.freed.free.iter().position(|free| {
            free.compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        })
    }

    /// Fill buffer `index`, taken with [`Self::take_free`], with `samples`
    /// (interleaved stereo, at most `BUFFER_FRAMES` frames), and queue it
    /// after every buffer queued before it.
    pub fn enqueue(&mut self, index: usize, samples: &[f32]) -> Result<(), Unavailable> {
        let buffer = *self.freed.buffers.get(index).ok_or(Unavailable)?;
        let frames = samples.len() / CHANNELS;
        if frames == 0 || frames > BUFFER_FRAMES || !samples.len().is_multiple_of(CHANNELS) {
            return Err(Unavailable);
        }
        let bytes = size_of_val(samples);
        // SAFETY: the buffer is the queue's own, not queued (it was given
        // back and taken), and holds `BUFFER_FRAMES` frames, at least
        // `bytes`.
        unsafe {
            std::ptr::copy_nonoverlapping(
                samples.as_ptr().cast::<u8>(),
                (*buffer).mAudioData.as_ptr().cast::<u8>(),
                bytes,
            );
            (*buffer).mAudioDataByteSize = bytes as u32;
        }
        // SAFETY: as above; linear PCM needs no packet descriptions.
        let status = unsafe { AudioQueueEnqueueBuffer(self.queue, buffer, 0, std::ptr::null()) };
        if status != 0 {
            self.freed.free[index].store(true, Ordering::Release);
            return Err(Unavailable);
        }
        Ok(())
    }

    /// Play what is queued, from the start or from where a pause left it.
    pub fn start(&mut self) -> Result<(), Unavailable> {
        // SAFETY: the queue is live; no start time: as soon as it can.
        if unsafe { AudioQueueStart(self.queue, std::ptr::null()) } != 0 {
            return Err(Unavailable);
        }
        self.started = true;
        Ok(())
    }

    /// Stop playing, keeping what is queued and where it was.
    pub fn pause(&mut self) -> Result<(), Unavailable> {
        if !self.started {
            return Ok(());
        }
        // SAFETY: the queue is live.
        if unsafe { AudioQueuePause(self.queue) } != 0 {
            return Err(Unavailable);
        }
        Ok(())
    }

    /// Silence the sound, or let it be heard: it plays on either way.
    pub fn set_muted(&self, muted: bool) {
        // SAFETY: the queue is live; volume is a parameter of every output
        // queue.
        unsafe {
            AudioQueueSetParameter(
                self.queue,
                kAudioQueueParam_Volume,
                if muted { 0.0 } else { 1.0 },
            );
        }
    }

    /// How many frames of what was queued the queue has played, and when it
    /// was so; `None` before it starts, or when its clock has no time to
    /// give or broke off.
    pub fn played(&self) -> Option<(f64, Instant)> {
        if !self.started {
            return None;
        }
        // SAFETY: a timestamp is plain numbers, all zero a valid value.
        let mut stamp: AudioTimeStamp = unsafe { std::mem::zeroed() };
        let mut broke: u8 = 0;
        let before = Instant::now();
        // SAFETY: the queue and its timeline are live, the out pointers
        // valid.
        let status = unsafe {
            AudioQueueGetCurrentTime(
                self.queue,
                self.timeline,
                &mut stamp,
                std::ptr::from_mut(&mut broke).cast(),
            )
        };
        let after = Instant::now();
        let valid = stamp.mFlags.contains(AudioTimeStampFlags::SampleTimeValid);
        (status == 0 && valid && broke == 0 && stamp.mSampleTime.is_finite())
            .then(|| (stamp.mSampleTime.max(0.0), before + (after - before) / 2))
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        // SAFETY: the queue is live and disposed once, immediately: no
        // callback runs after this returns, so `freed` may then go. Its
        // buffers and timeline go with it.
        unsafe {
            AudioQueueDispose(self.queue, true);
        }
    }
}
