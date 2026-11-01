//! Two contained stream decoders, bounded queues and shared cancellation.

use std::io::{BufReader, Read};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use kettle_media::audio::{AudioBlock, AudioSource, CHANNELS};
use kettle_media::video::{FrameSource, PlayGate};
use kettle_media::{FailureCode, pcm};

use super::packets::{Metadata, Packet, Stream};
use crate::run::{Exit, Pair, Running};

const ROUND: Duration = Duration::from_millis(10);

struct Shared {
    running: [Mutex<Running>; 2],
    cancelled: AtomicBool,
    audio_cancelled: AtomicBool,
    clean: [AtomicBool; 2],
}

impl Shared {
    fn stop(&self) {
        self.cancelled.store(true, Ordering::Release);
        for running in &self.running {
            running
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .stop();
        }
    }

    fn stop_and_reap(&self) -> bool {
        self.stop();
        let deadline = Instant::now() + Duration::from_secs(1);
        self.running.iter().fold(true, |reaped, running| {
            let mut running = running.lock().unwrap_or_else(PoisonError::into_inner);
            if !running.is_reaped() {
                running.exit_within(deadline.saturating_duration_since(Instant::now()));
            }
            running.is_reaped() && reaped
        })
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
    reached_eof: bool,
}

pub(super) struct Audio {
    shared: Arc<Shared>,
    packets: mpsc::Receiver<Bytes>,
    leading_silence: Range<u64>,
    carry: Option<Bytes>,
    consumed: usize,
}

pub(super) fn split(
    streams: [Pair; 2],
    size: usize,
    start_ms: u64,
    duration_ms: u64,
    audio_start_frame: u64,
    initial_gap: bool,
    deadline: Instant,
) -> Result<(Video, Audio), FailureCode> {
    let [video, audio] = streams;
    let Pair {
        metadata: video_metadata,
        raw: video_raw,
        running: video_running,
    } = video;
    let Pair {
        metadata: audio_metadata,
        raw: audio_raw,
        running: audio_running,
    } = audio;
    let shared = Arc::new(Shared {
        running: [Mutex::new(video_running), Mutex::new(audio_running)],
        cancelled: AtomicBool::new(false),
        audio_cancelled: AtomicBool::new(false),
        clean: [AtomicBool::new(false), AtomicBool::new(false)],
    });
    let raw_video = drain(video_raw, size, Arc::clone(&shared)).inspect_err(|_| shared.stop())?;
    let raw_audio = drain(audio_raw, 7680, Arc::clone(&shared)).inspect_err(|_| shared.stop())?;
    let (video_tx, video_rx) = mpsc::sync_channel(2);
    let (audio_tx, audio_rx) = mpsc::sync_channel(8);
    let (started, startup) = mpsc::sync_channel(2);
    for (stream, metadata, raw, sender) in [
        (Stream::Video, video_metadata, raw_video, video_tx),
        (Stream::Audio, audio_metadata, raw_audio, audio_tx),
    ] {
        let drained = Arc::clone(&shared);
        let started = started.clone();
        std::thread::Builder::new()
            .name(
                match stream {
                    Stream::Video => "kettle-media-video",
                    Stream::Audio => "kettle-media-audio",
                }
                .into(),
            )
            .spawn(move || {
                let index = stream.index();
                let mut metadata = Metadata::new(
                    BufReader::new(metadata),
                    stream,
                    size,
                    start_ms,
                    duration_ms,
                );
                let mut started = Some(started);
                let mut read = || -> Result<(), ()> {
                    while let Some(Packet {
                        stream: packet_stream,
                        at_us,
                        bytes,
                    }) = metadata.next()?
                    {
                        if packet_stream != index
                            || drained.cancelled.load(Ordering::Acquire)
                            || (stream == Stream::Audio
                                && drained.audio_cancelled.load(Ordering::Acquire))
                        {
                            return Err(());
                        }
                        if let Some(started) = started.take() {
                            let _ = started.send(());
                        }
                        let data = raw.recv().map_err(|_| ())??;
                        if data.len() != bytes {
                            return Err(());
                        }
                        let mut packet = Bytes { at_us, data };
                        loop {
                            if drained.cancelled.load(Ordering::Acquire)
                                || (stream == Stream::Audio
                                    && drained.audio_cancelled.load(Ordering::Acquire))
                            {
                                return Err(());
                            }
                            match sender.try_send(packet) {
                                Ok(()) => break,
                                Err(mpsc::TrySendError::Disconnected(_))
                                    if stream == Stream::Audio =>
                                {
                                    break;
                                }
                                Err(mpsc::TrySendError::Disconnected(_)) => return Err(()),
                                Err(mpsc::TrySendError::Full(returned)) => {
                                    packet = returned;
                                    std::thread::sleep(ROUND);
                                }
                            }
                        }
                    }
                    let exit = drained.running[index]
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .exit_within(Duration::from_secs(1));
                    if exit != Exit::Clean || raw.recv().is_ok() {
                        return Err(());
                    }
                    Ok(())
                };
                if read().is_ok() {
                    drained.clean[index].store(true, Ordering::Release);
                    if let Some(started) = started.take() {
                        let _ = started.send(());
                    }
                } else if stream == Stream::Audio && drained.audio_cancelled.load(Ordering::Acquire)
                {
                    let mut running = drained.running[index]
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    running.stop();
                    running.exit_within(Duration::from_secs(1));
                } else {
                    drained.stop();
                }
            })
            .map_err(|_| {
                shared.stop();
                FailureCode::RenderResource
            })?;
    }
    drop(started);
    for _ in 0..2 {
        if let Err(error) = startup.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            if !shared.stop_and_reap() {
                return Err(FailureCode::RenderResource);
            }
            return Err(match error {
                mpsc::RecvTimeoutError::Timeout => FailureCode::RenderTimeout,
                mpsc::RecvTimeoutError::Disconnected => FailureCode::RenderParse,
            });
        }
    }
    let start_frame = start_ms * 48;
    let gap = audio_start_frame.saturating_sub(start_frame);
    // Prime with whole PCM packets; the feeder fills the remaining gap
    // from the real packet timestamp without discarding audio samples.
    let silence_frames = gap / pcm::FRAMES as u64 * pcm::FRAMES as u64;
    Ok((
        Video {
            shared: Arc::clone(&shared),
            packets: video_rx,
            size,
            initial_gap: initial_gap.then_some(start_ms),
            reached_eof: false,
        },
        Audio {
            shared,
            packets: audio_rx,
            leading_silence: start_frame..start_frame + silence_frames,
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
                return if shared.clean[usize::from(audio)].load(Ordering::Acquire) {
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
            self.reached_eof = true;
            return Ok(None);
        };
        rgba.copy_from_slice(&packet.data);
        Ok(Some(packet.at_us / 1000))
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        if !self.reached_eof {
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
        if !self.leading_silence.is_empty() {
            if self.shared.cancelled.load(Ordering::Acquire)
                || self.shared.audio_cancelled.load(Ordering::Acquire)
            {
                return Err(FailureCode::RenderParse);
            }
            let frames = usize::try_from(self.leading_silence.end - self.leading_silence.start)
                .unwrap_or(usize::MAX)
                .min(pcm::FRAMES)
                .min(samples.len() / CHANNELS);
            samples[..frames * CHANNELS].fill(0.0);
            let at_frame = self.leading_silence.start;
            self.leading_silence.start += frames as u64;
            return Ok(Some(AudioBlock { frames, at_frame }));
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

impl Drop for Audio {
    fn drop(&mut self) {
        self.shared.audio_cancelled.store(true, Ordering::Release);
        self.shared.running[1]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn dropping_queued_video_stops_audio_after_video_helper_eof() {
        let directory = kettle_test_support::private_tempdir("kettle-queued-video-drop-");
        let video_tool = crate::run::tests::script(directory.path(), "video", "exit 0");
        let audio_tool =
            crate::run::tests::script(directory.path(), "audio", "while :; do :; done");
        let Pair {
            running: mut video_running,
            ..
        } = crate::run::pair(
            &video_tool,
            |_| Vec::new(),
            File::open("/dev/null").unwrap(),
            1,
        )
        .unwrap();
        assert_eq!(
            video_running.exit_within(Duration::from_secs(1)),
            Exit::Clean
        );
        let Pair {
            running: audio_running,
            ..
        } = crate::run::pair(
            &audio_tool,
            |_| Vec::new(),
            File::open("/dev/null").unwrap(),
            1,
        )
        .unwrap();
        let shared = Arc::new(Shared {
            running: [Mutex::new(video_running), Mutex::new(audio_running)],
            cancelled: AtomicBool::new(false),
            audio_cancelled: AtomicBool::new(false),
            clean: [AtomicBool::new(true), AtomicBool::new(false)],
        });
        let (sent, received) = mpsc::sync_channel(2);
        sent.send(Bytes {
            at_us: 0,
            data: vec![0; 4],
        })
        .unwrap();
        drop(sent);
        let video = Video {
            shared: Arc::clone(&shared),
            packets: received,
            size: 4,
            initial_gap: None,
            reached_eof: false,
        };
        drop(video);
        assert!(shared.cancelled.load(Ordering::Acquire));
        assert!(shared.stop_and_reap());
    }
}
