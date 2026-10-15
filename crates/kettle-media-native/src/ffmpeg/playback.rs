//! One contained tee process, bounded queues and a shared cancellation path.

use std::io::{BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use kettle_media::FailureCode;
use kettle_media::audio::{AudioBlock, AudioSource, CHANNELS};
use kettle_media::video::{FrameSource, PlayGate};

use super::packets::{Metadata, Packet};
use crate::run::{Exit, Running, Triple};

const ROUND: Duration = Duration::from_millis(10);

struct Shared {
    running: Mutex<Running>,
    cancelled: AtomicBool,
    audio_cancelled: AtomicBool,
    clean: AtomicBool,
}

impl Shared {
    fn stop(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stop();
    }
}

struct Bytes {
    at_us: u64,
    data: Vec<u8>,
}

pub(super) struct Video {
    shared: Arc<Shared>,
    packets: mpsc::Receiver<Bytes>,
    size: usize,
    initial_gap: Option<u64>,
}

pub(super) struct Audio {
    shared: Arc<Shared>,
    packets: mpsc::Receiver<Bytes>,
    carry: Option<Bytes>,
    consumed: usize,
}

pub(super) fn split(
    pipes: Triple,
    size: usize,
    start_ms: u64,
    duration_ms: u64,
    initial_gap: bool,
    deadline: Instant,
) -> Result<(Video, Audio), FailureCode> {
    let Triple {
        metadata,
        video,
        audio,
        running,
    } = pipes;
    let shared = Arc::new(Shared {
        running: Mutex::new(running),
        cancelled: AtomicBool::new(false),
        audio_cancelled: AtomicBool::new(false),
        clean: AtomicBool::new(false),
    });
    let raw_video = drain(video, size, Arc::clone(&shared)).inspect_err(|_| shared.stop())?;
    let raw_audio = drain(audio, 7680, Arc::clone(&shared)).inspect_err(|_| shared.stop())?;
    let (video_tx, video_rx) = mpsc::sync_channel(2);
    let (audio_tx, audio_rx) = mpsc::sync_channel(8);
    let (started, startup) = mpsc::sync_channel(1);
    let drained = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("kettle-media-tee".into())
        .spawn(move || {
            let mut metadata = Metadata::new(BufReader::new(metadata), size, start_ms, duration_ms);
            let mut started = Some(started);
            let mut read = || -> Result<(), ()> {
                // Separate readers prevent tee's per-slave interleaver from
                // holding metadata while a large raw packet fills its pipe.
                while let Some(Packet {
                    stream,
                    at_us,
                    bytes,
                }) = metadata.next()?
                {
                    if drained.cancelled.load(Ordering::Acquire) {
                        return Err(());
                    }
                    // A packet proves the muxer opened both codecs. Waiting for
                    // video here would block when audio precedes the picture.
                    if let Some(started) = started.take() {
                        let _ = started.send(());
                    }
                    let raw = if stream == 0 { &raw_video } else { &raw_audio };
                    let data = raw.recv().map_err(|_| ())??;
                    if data.len() != bytes {
                        return Err(());
                    }
                    let mut packet = Bytes { at_us, data };
                    let sender = if stream == 0 { &video_tx } else { &audio_tx };
                    loop {
                        if drained.cancelled.load(Ordering::Acquire) {
                            return Err(());
                        }
                        match sender.try_send(packet) {
                            Ok(()) => break,
                            // Sound may be unavailable. Still drain its pipe so
                            // the same process can continue supplying video.
                            Err(mpsc::TrySendError::Disconnected(_)) if stream == 1 => break,
                            Err(mpsc::TrySendError::Disconnected(_)) => return Err(()),
                            Err(mpsc::TrySendError::Full(returned)) => {
                                packet = returned;
                                std::thread::sleep(ROUND);
                            }
                        }
                    }
                }
                let exit = drained
                    .running
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .exit_within(Duration::from_secs(1));
                if exit != Exit::Clean {
                    return Err(());
                }
                if raw_video.recv().is_ok() || raw_audio.recv().is_ok() {
                    return Err(());
                }
                Ok(())
            };
            if read().is_ok() {
                drained.clean.store(true, Ordering::Release);
            } else {
                drained.stop();
            }
        })
        .map_err(|_| {
            shared.stop();
            FailureCode::RenderResource
        })?;
    startup
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|error| {
            shared.stop();
            match error {
                mpsc::RecvTimeoutError::Timeout => FailureCode::RenderTimeout,
                mpsc::RecvTimeoutError::Disconnected => FailureCode::RenderParse,
            }
        })?;
    Ok((
        Video {
            shared: Arc::clone(&shared),
            packets: video_rx,
            size,
            initial_gap: initial_gap.then_some(start_ms),
        },
        Audio {
            shared,
            packets: audio_rx,
            carry: None,
            consumed: 0,
        },
    ))
}

/// Each reader holds at most three whole packets: two queued and one
/// being read. Cancellation releases both queue pressure and blocked reads.
fn drain(
    mut pipe: std::io::PipeReader,
    size: usize,
    shared: Arc<Shared>,
) -> Result<mpsc::Receiver<Result<Vec<u8>, ()>>, FailureCode> {
    let (sender, received) = mpsc::sync_channel(2);
    std::thread::Builder::new()
        .name("kettle-media-pipe".into())
        .spawn(move || {
            loop {
                if shared.cancelled.load(Ordering::Acquire) {
                    return;
                }
                let mut bytes = vec![0; size];
                let mut filled = 0;
                let mut failed = false;
                while filled < size {
                    match pipe.read(&mut bytes[filled..]) {
                        Ok(0) if filled == 0 => return,
                        Ok(0) => {
                            failed = true;
                            break;
                        }
                        Ok(n) => filled += n,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => {
                            failed = true;
                            break;
                        }
                    }
                }
                let mut packet = if failed { Err(()) } else { Ok(bytes) };
                loop {
                    if shared.cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    match sender.try_send(packet) {
                        Ok(()) if failed => return,
                        Ok(()) => break,
                        Err(mpsc::TrySendError::Disconnected(_)) => return,
                        Err(mpsc::TrySendError::Full(returned)) => {
                            packet = returned;
                            std::thread::sleep(ROUND);
                        }
                    }
                }
            }
        })
        .map_err(|_| FailureCode::RenderResource)?;
    Ok(received)
}

fn receive(
    shared: &Shared,
    packets: &mpsc::Receiver<Bytes>,
    audio: bool,
) -> Result<Option<Bytes>, FailureCode> {
    loop {
        if shared.cancelled.load(Ordering::Acquire)
            || (audio && shared.audio_cancelled.load(Ordering::Acquire))
        {
            return Err(FailureCode::RenderParse);
        }
        match packets.recv_timeout(ROUND) {
            Ok(packet) => return Ok(Some(packet)),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return if shared.clean.load(Ordering::Acquire) {
                    Ok(None)
                } else {
                    Err(FailureCode::RenderParse)
                };
            }
        }
    }
}

impl FrameSource for Video {
    fn next_frame(&mut self, rgba: &mut [u8], gate: &PlayGate) -> Result<Option<u64>, FailureCode> {
        if rgba.len() != self.size {
            return Err(FailureCode::BadParams);
        }
        if gate.has_ended() {
            self.shared.stop();
            return Ok(None);
        }
        if let Some(at_ms) = self.initial_gap.take() {
            for pixel in rgba.as_chunks_mut::<4>().0 {
                pixel.copy_from_slice(&[0, 0, 0, 255]);
            }
            return Ok(Some(at_ms));
        }
        let Some(packet) = receive(&self.shared, &self.packets, false)? else {
            return Ok(None);
        };
        rgba.copy_from_slice(&packet.data);
        Ok(Some(packet.at_us / 1000))
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        if !self.shared.clean.load(Ordering::Acquire) {
            self.shared.stop();
        }
    }
}

impl AudioSource for Audio {
    fn cancellation(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let shared = Arc::clone(&self.shared);
        Some(Arc::new(move || {
            shared.audio_cancelled.store(true, Ordering::Release)
        }))
    }

    fn next_block(&mut self, samples: &mut [f32]) -> Result<Option<AudioBlock>, FailureCode> {
        if samples.len() < CHANNELS {
            return Err(FailureCode::BadParams);
        }
        if self.carry.is_none() {
            self.carry = receive(&self.shared, &self.packets, true)?;
            self.consumed = 0;
        }
        let Some(packet) = &self.carry else {
            return Ok(None);
        };
        let frames = (samples.len() / CHANNELS).min((packet.data.len() - self.consumed) / 8);
        let end = self.consumed + frames * 8;
        for (sample, bytes) in samples
            .iter_mut()
            .zip(packet.data[self.consumed..end].as_chunks::<4>().0)
        {
            let value = f32::from_le_bytes(*bytes);
            if !value.is_finite() {
                return Err(FailureCode::RenderParse);
            }
            *sample = value;
        }
        let at_frame = ((u128::from(packet.at_us) * 48_000 + 500_000) / 1_000_000) as u64
            + (self.consumed / 8) as u64;
        self.consumed = end;
        if end == packet.data.len() {
            self.carry = None;
        }
        Ok(Some(AudioBlock { frames, at_frame }))
    }
}
