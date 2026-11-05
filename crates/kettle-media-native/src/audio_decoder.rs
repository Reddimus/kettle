use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;

use kettle_media::FailureCode;
use kettle_media::audio::{AudioBlock, AudioRead, AudioSource, CHANNELS};

const BLOCK_FRAMES: usize = 960;
const QUEUED_BLOCKS: usize = 2;

type Decoded = Result<Option<(AudioBlock, Vec<f32>)>, FailureCode>;

pub(crate) struct AudioDecoder {
    receiver: Option<Receiver<Decoded>>,
    cancelled: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pending: Option<(AudioBlock, Vec<f32>, usize)>,
}

impl AudioDecoder {
    pub(crate) fn start(mut source: impl AudioSource + 'static) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(QUEUED_BLOCKS);
        let cancelled = Arc::new(AtomicBool::new(false));
        let stopped = cancelled.clone();
        let thread = std::thread::Builder::new()
            .name("kettle-audio-decoder".into())
            .spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    let mut samples = vec![0.0; BLOCK_FRAMES * CHANNELS];
                    let decoded = source.next_block(&mut samples).and_then(|block| {
                        block
                            .map(|block| {
                                if block.frames == 0 || block.frames > BLOCK_FRAMES {
                                    return Err(FailureCode::RenderParse);
                                }
                                samples.truncate(block.frames * CHANNELS);
                                Ok((block, samples))
                            })
                            .transpose()
                    });
                    let ended = !matches!(decoded, Ok(Some(_)));
                    if stopped.load(Ordering::Acquire) || sender.send(decoded).is_err() || ended {
                        break;
                    }
                }
                // The native reader is cancelled and dropped on its decoding thread.
                drop(source);
            })?;
        Ok(Self {
            receiver: Some(receiver),
            cancelled,
            thread: Some(thread),
            pending: None,
        })
    }
}

impl AudioSource for AudioDecoder {
    fn cancellation(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let cancelled = self.cancelled.clone();
        Some(Arc::new(move || cancelled.store(true, Ordering::Release)))
    }

    fn poll_block(&mut self, samples: &mut [f32]) -> Result<AudioRead, FailureCode> {
        if self.cancelled.load(Ordering::Acquire) {
            return Ok(AudioRead::End);
        }
        if samples.len() < CHANNELS {
            return Err(FailureCode::BadParams);
        }
        if self.pending.is_none() {
            match self
                .receiver
                .as_ref()
                .expect("decoder receiver is present")
                .try_recv()
            {
                Ok(Ok(Some((block, pcm)))) => self.pending = Some((block, pcm, 0)),
                Ok(Ok(None)) | Err(TryRecvError::Disconnected) => return Ok(AudioRead::End),
                Ok(Err(error)) => return Err(error),
                Err(TryRecvError::Empty) => return Ok(AudioRead::Pending),
            }
        }
        let (block, pcm, offset) = self.pending.as_mut().expect("a ready block is present");
        let frames = (samples.len() / CHANNELS).min(block.frames - *offset);
        samples[..frames * CHANNELS]
            .copy_from_slice(&pcm[*offset * CHANNELS..(*offset + frames) * CHANNELS]);
        let read = AudioBlock {
            frames,
            at_frame: block.at_frame + *offset as u64,
        };
        *offset += frames;
        if *offset == block.frames {
            self.pending = None;
        }
        Ok(AudioRead::Block(read))
    }

    fn next_block(&mut self, samples: &mut [f32]) -> Result<Option<AudioBlock>, FailureCode> {
        loop {
            match self.poll_block(samples)? {
                AudioRead::Block(block) => return Ok(Some(block)),
                AudioRead::End => return Ok(None),
                AudioRead::Pending => std::thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
    }
}

impl Drop for AudioDecoder {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.receiver.take();
        if let Some(thread) = self.thread.take() {
            // The worker's close watchdog bounds a native decode that never returns.
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_media::audio::{AudioRead, CHANNELS};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn stalled_decoder_does_not_block_pcm_polling() {
        struct Stalled(mpsc::Receiver<()>);
        impl AudioSource for Stalled {
            fn next_block(&mut self, _: &mut [f32]) -> Result<Option<AudioBlock>, FailureCode> {
                let _ = self.0.recv();
                Ok(None)
            }
        }
        let (release, wait) = mpsc::channel();
        let mut source = AudioDecoder::start(Stalled(wait)).unwrap();
        let (ready, polled) = mpsc::channel();
        let control = std::thread::spawn(move || {
            let result = source.poll_block(&mut [0.0; CHANNELS]);
            ready.send(result).unwrap();
        });
        let result = polled.recv_timeout(Duration::from_millis(100));
        let _ = release.send(());
        control.join().unwrap();
        assert_eq!(
            result.ok(),
            Some(Ok(AudioRead::Pending)),
            "stalled decoder blocked the sound-control thread"
        );
    }
    #[test]
    fn decoded_blocks_keep_their_samples_and_timeline_when_split() {
        struct Once(bool);
        impl AudioSource for Once {
            fn next_block(
                &mut self,
                samples: &mut [f32],
            ) -> Result<Option<AudioBlock>, FailureCode> {
                if std::mem::replace(&mut self.0, true) {
                    return Ok(None);
                }
                samples[..6].copy_from_slice(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
                Ok(Some(AudioBlock {
                    frames: 3,
                    at_frame: 100,
                }))
            }
        }
        let mut source = AudioDecoder::start(Once(false)).unwrap();
        let mut samples = [0.0; 4];
        assert_eq!(
            source.next_block(&mut samples).unwrap(),
            Some(AudioBlock {
                frames: 2,
                at_frame: 100
            })
        );
        assert_eq!(samples, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(
            source.next_block(&mut samples).unwrap(),
            Some(AudioBlock {
                frames: 1,
                at_frame: 102
            })
        );
        assert_eq!(&samples[..2], &[5.0, 6.0]);
        assert_eq!(source.next_block(&mut samples).unwrap(), None);
    }

    #[test]
    fn dropping_a_full_decoder_queue_joins_its_producer() {
        use std::sync::atomic::AtomicUsize;
        struct Endless(Arc<AtomicUsize>, mpsc::Sender<std::thread::ThreadId>);
        impl AudioSource for Endless {
            fn next_block(
                &mut self,
                samples: &mut [f32],
            ) -> Result<Option<AudioBlock>, FailureCode> {
                self.0.fetch_add(1, Ordering::Release);
                samples.fill(0.5);
                Ok(Some(AudioBlock {
                    frames: BLOCK_FRAMES,
                    at_frame: 0,
                }))
            }
        }
        impl Drop for Endless {
            fn drop(&mut self) {
                let _ = self.1.send(std::thread::current().id());
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let (dropped, on_thread) = mpsc::channel();
        let source = AudioDecoder::start(Endless(calls.clone(), dropped)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while calls.load(Ordering::Acquire) < QUEUED_BLOCKS + 1 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(calls.load(Ordering::Acquire), QUEUED_BLOCKS + 1);
        drop(source);
        assert_ne!(
            on_thread.recv_timeout(Duration::from_secs(1)).unwrap(),
            std::thread::current().id()
        );
    }
}
