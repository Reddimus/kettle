//! A session's sound in the worker. A thread of its own feeds a speaker (an
//! output queue on macOS) from the player's decoded sound, a fixed 20 ms
//! at a time, filling any gap in the sound and everything after its end
//! with silence so the speaker's clock runs as long as the session does;
//! plays, pauses and mutes it as the session asks; and keeps the clock the
//! session's frames follow: where the sound being heard is in the video.
//! Before the first Play it fills the speaker, so Play starts at once.
//! macOS owns its device queue directly; Linux uses this feeder only in
//! the PCM broker, which never accepts encoded media.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kettle_media::PlayCommand;
use kettle_media::audio::{AudioBlock, AudioRead, AudioSource, CHANNELS};

/// Frames in each piece the speaker is given: 20 ms.
pub(crate) const PIECE_FRAMES: usize = 960;
/// How often the feeding thread comes round: well inside a piece.
const ROUND: Duration = Duration::from_millis(5);
/// How long a started speaker may go without a clock before the sound is
/// given up: a device that never starts leaves the session silent, not
/// waiting.
const CLOCK_WITHIN: Duration = Duration::from_secs(1);

/// What plays a session's sound: a fixed set of buffers it gives back once
/// played, and its own clock.
pub(crate) trait Speaker: Send {
    /// A buffer it has given back, now taken to be filled.
    fn take_free(&mut self) -> Option<usize>;
    /// Queue `samples` (interleaved, at most `PIECE_FRAMES` frames) in
    /// buffer `index`, after everything queued before.
    fn enqueue(&mut self, index: usize, samples: &[f32]) -> Result<(), ()>;
    fn start(&mut self) -> Result<(), ()>;
    fn pause(&mut self) -> Result<(), ()>;
    fn set_muted(&mut self, muted: bool);
    /// Frames played of what was queued, and when that was so.
    fn played(&self) -> Option<(f64, Instant)>;
}

/// Where the sound being heard is in the video, in frames from its start,
/// and when that was so.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Heard {
    pub(crate) frame: f64,
    pub(crate) at: Instant,
}

/// What the session wants of its sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Wish {
    playing: bool,
    muted: bool,
    closed: bool,
}

const RUNNING: u8 = 0;
/// The speaker refused a piece, or to play: the session goes on silent.
const FAILED: u8 = 1;
/// Every frame of the sound has been heard.
const DONE: u8 = 2;

/// What the session wants of its sound, shared by the thread that hears
/// the parent and the sound's own: each command takes effect as it comes,
/// so a Pause or a Mute is heard at once whatever the session's thread is
/// held on.
#[derive(Clone)]
pub(crate) struct SoundWish(Arc<Mutex<Wish>>);

impl SoundWish {
    /// Paused, muted when asked.
    pub(crate) fn new(muted: bool) -> Self {
        Self(Arc::new(Mutex::new(Wish {
            playing: false,
            muted,
            closed: false,
        })))
    }

    fn get(&self) -> Wish {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn change(&self, change: impl FnOnce(&mut Wish)) {
        change(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// Whether the sound is to be muted.
    pub(crate) fn muted(&self) -> bool {
        self.get().muted
    }

    /// Follow `command`: Play and Pause, Mute and Unmute, and Close, which
    /// ends the sound; a heartbeat is not the sound's.
    pub(crate) fn hear(&self, command: PlayCommand) {
        match command {
            PlayCommand::Play => self.change(|wish| wish.playing = true),
            PlayCommand::Pause => self.change(|wish| wish.playing = false),
            PlayCommand::Mute => self.change(|wish| wish.muted = true),
            PlayCommand::Unmute => self.change(|wish| wish.muted = false),
            PlayCommand::Close => self.close(),
            PlayCommand::Heartbeat => {}
        }
    }

    /// End the sound: its thread stops and lets its speaker go, the output
    /// disposed, within a round.
    pub(crate) fn close(&self) {
        self.change(|wish| wish.closed = true);
    }

    /// Whether the sound is to play.
    #[cfg(any(test, target_os = "linux"))]
    pub(crate) fn playing(&self) -> bool {
        self.get().playing
    }
}

struct Shared {
    wish: SoundWish,
    heard: Mutex<Option<Heard>>,
    state: AtomicU8,
}

#[cfg(target_os = "linux")]
pub(crate) trait RemoteClock: Send + Sync {
    fn heard(&self) -> Option<Heard>;
    fn failed(&self) -> bool;
    fn done(&self) -> bool;
    fn close(&self);
}

/// A session's sound, played on a thread of its own until dropped.
pub(crate) struct Sound {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    cancel_source: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(target_os = "linux")]
    remote: Option<Arc<dyn RemoteClock>>,
}

impl Sound {
    /// Feed `speaker` from `source`, whose sound starts at `start_frame`,
    /// as `wish` says: it plays once the session plays.
    pub(crate) fn start(
        speaker: Box<dyn Speaker>,
        source: Box<dyn AudioSource>,
        start_frame: u64,
        wish: SoundWish,
    ) -> std::io::Result<Self> {
        let cancel_source = source.cancellation();
        let muted = wish.muted();
        let shared = Arc::new(Shared {
            wish,
            heard: Mutex::new(None),
            state: AtomicU8::new(RUNNING),
        });
        let fed = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("kettle-media-sound".into())
            .spawn(move || Feeder::new(speaker, source, start_frame, muted).run(&fed))?;
        Ok(Self {
            shared,
            thread: Some(thread),
            cancel_source,
            #[cfg(target_os = "linux")]
            remote: None,
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn remote(
        wish: SoundWish,
        remote: Arc<dyn RemoteClock>,
        thread: JoinHandle<()>,
        cancel: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                wish,
                heard: Mutex::new(None),
                state: AtomicU8::new(RUNNING),
            }),
            thread: Some(thread),
            cancel_source: Some(cancel),
            remote: Some(remote),
        }
    }

    /// What it follows: for a test, the parent's commands as the reader
    /// would pass them on.
    #[cfg(test)]
    pub(crate) fn wish(&self) -> SoundWish {
        self.shared.wish.clone()
    }

    /// Where the sound being heard is, once the speaker has a clock.
    pub(crate) fn heard(&self) -> Option<Heard> {
        #[cfg(target_os = "linux")]
        if let Some(remote) = &self.remote {
            return remote.heard();
        }
        *self
            .shared
            .heard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The speaker failed, or the feeding thread stopped on its own: there
    /// is no sound from here on.
    pub(crate) fn failed(&self) -> bool {
        #[cfg(target_os = "linux")]
        if let Some(remote) = &self.remote {
            return remote.failed();
        }
        self.shared.state.load(Ordering::Acquire) == FAILED
            || self.thread.as_ref().is_some_and(JoinHandle::is_finished)
    }

    /// Every frame of the sound has been heard.
    pub(crate) fn done(&self) -> bool {
        #[cfg(target_os = "linux")]
        if let Some(remote) = &self.remote {
            return remote.done();
        }
        self.shared.state.load(Ordering::Acquire) == DONE
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        self.shared.wish.close();
        #[cfg(target_os = "linux")]
        if let Some(remote) = &self.remote {
            remote.close();
        }
        if let Some(cancel) = &self.cancel_source {
            cancel();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The feeding thread's state.
struct Feeder {
    speaker: Box<dyn Speaker>,
    source: Box<dyn AudioSource>,
    start_frame: u64,
    /// The video's frame the next sample queued plays at.
    next_frame: u64,
    /// Decoded frames not yet queued, and where the first plays.
    carry: Vec<f32>,
    carry_at: u64,
    /// The sound has no more frames; past `end_frame` is silence.
    source_ended: bool,
    end_frame: u64,
    scratch: Vec<f32>,
    piece: Vec<f32>,
    piece_filled: usize,
    pending_index: Option<usize>,
    applied: Wish,
    /// When the speaker was first started, until it had a clock.
    awaiting_clock: Option<Instant>,
}

impl Feeder {
    fn new(
        speaker: Box<dyn Speaker>,
        source: Box<dyn AudioSource>,
        start_frame: u64,
        muted: bool,
    ) -> Self {
        Self {
            speaker,
            source,
            start_frame,
            next_frame: start_frame,
            carry: Vec::new(),
            carry_at: start_frame,
            source_ended: false,
            end_frame: start_frame,
            scratch: vec![0.0; 2048 * CHANNELS],
            piece: vec![0.0; PIECE_FRAMES * CHANNELS],
            piece_filled: 0,
            pending_index: None,
            applied: Wish {
                playing: false,
                muted,
                closed: false,
            },
            awaiting_clock: None,
        }
    }

    fn run(mut self, shared: &Shared) {
        self.speaker.set_muted(self.applied.muted);
        loop {
            let wish = shared.wish.get();
            if wish.closed {
                return;
            }
            if self.round(wish, shared).is_err() {
                shared.state.store(FAILED, Ordering::Release);
                // Silent from here: the speaker goes with this thread.
                return;
            }
            std::thread::sleep(ROUND);
        }
    }

    /// One round: what the session wants, the speaker filled, the clock
    /// kept.
    fn round(&mut self, wish: Wish, shared: &Shared) -> Result<(), ()> {
        if wish.muted != self.applied.muted {
            self.speaker.set_muted(wish.muted);
        }
        if !wish.playing && self.applied.playing {
            self.speaker.pause()?;
            self.awaiting_clock = None;
        }
        while let Some(index) = self
            .pending_index
            .take()
            .or_else(|| self.speaker.take_free())
        {
            if !self.fill_piece()? {
                self.pending_index = Some(index);
                break;
            }
            self.speaker.enqueue(index, &self.piece)?;
        }
        if wish.playing && !self.applied.playing {
            self.speaker.start()?;
        }
        self.applied = wish;
        let played = self.speaker.played();
        if !wish.playing {
            // Paused, the sound has no clock to go by: frames played again
            // wait for a fresh one, never one from before the pause.
            *shared.heard.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
        if played.is_some() {
            self.awaiting_clock = None;
        } else if wish.playing {
            // No clock while playing, from the start or once it was lost:
            // the frames wait for one, and a second of playing without it
            // gives the sound up.
            *shared.heard.lock().unwrap_or_else(PoisonError::into_inner) = None;
            let since = *self.awaiting_clock.get_or_insert_with(Instant::now);
            if since.elapsed() > CLOCK_WITHIN {
                return Err(());
            }
        }
        if let Some((played, at)) = played {
            let frame = self.start_frame as f64 + played;
            if wish.playing {
                *shared.heard.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(Heard { frame, at });
            }
            if self.source_ended && frame >= self.end_frame as f64 {
                shared.state.store(DONE, Ordering::Release);
            }
        }
        Ok(())
    }

    /// Fill `piece` with the next `PIECE_FRAMES` frames: the sound where
    /// there is some, silence in its gaps and after its end.
    fn fill_piece(&mut self) -> Result<bool, ()> {
        let mut filled = self.piece_filled;
        while filled < PIECE_FRAMES {
            if self.carry.is_empty() && !self.source_ended {
                match self.source.poll_block(&mut self.scratch) {
                    Ok(AudioRead::Block(AudioBlock { frames, at_frame })) => {
                        self.carry.clear();
                        self.carry
                            .extend_from_slice(&self.scratch[..frames * CHANNELS]);
                        // A block before what is queued plays no more.
                        self.carry_at =
                            kettle_media::audio::trim(&mut self.carry, at_frame, self.next_frame);
                        self.end_frame = self.end_frame.max(self.carry_at + frames as u64);
                    }
                    Ok(AudioRead::Pending) => {
                        self.piece_filled = filled;
                        return Ok(false);
                    }
                    Ok(AudioRead::End) => self.source_ended = true,
                    // A sound that fails to decode leaves silence.
                    Err(_) => self.source_ended = true,
                }
                continue;
            }
            let room = PIECE_FRAMES - filled;
            let into = &mut self.piece[filled * CHANNELS..PIECE_FRAMES * CHANNELS];
            if !self.carry.is_empty() && self.carry_at <= self.next_frame {
                let take = room.min(self.carry.len() / CHANNELS);
                into[..take * CHANNELS].copy_from_slice(&self.carry[..take * CHANNELS]);
                self.carry.drain(..take * CHANNELS);
                self.carry_at += take as u64;
                filled += take;
                self.next_frame += take as u64;
            } else {
                // Silence up to the next block, or to the end of the piece.
                let gap = if self.carry.is_empty() {
                    room
                } else {
                    usize::try_from(self.carry_at - self.next_frame)
                        .unwrap_or(usize::MAX)
                        .min(room)
                };
                into[..gap * CHANNELS].fill(0.0);
                filled += gap;
                self.next_frame += gap as u64;
            }
        }
        self.piece_filled = 0;
        Ok(true)
    }
}

/// A stand-in speaker and sound for tests, here and in the session's.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use kettle_media::FailureCode;
    use kettle_media::audio::{AudioBlock, AudioSource, CHANNELS};

    use super::Speaker;

    /// What a stand-in speaker was told, and its clock, which the test
    /// moves.
    #[derive(Default)]
    pub(crate) struct Told {
        pub(crate) queued: Vec<f32>,
        pub(crate) free: VecDeque<usize>,
        pub(crate) started: usize,
        pub(crate) paused: usize,
        pub(crate) muted: Vec<bool>,
        pub(crate) played: Option<f64>,
        pub(crate) refuse: bool,
        /// Its buffers, and how many pieces it was given.
        pub(crate) buffers: usize,
        pub(crate) enqueued: usize,
        /// It was let go, as an output is disposed.
        pub(crate) released: bool,
    }

    struct Stand(Arc<Mutex<Told>>);

    impl Drop for Stand {
        fn drop(&mut self) {
            if let Ok(mut told) = self.0.lock() {
                told.released = true;
            }
        }
    }

    impl Speaker for Stand {
        /// As a queue does: each buffer back once what it held has played.
        fn take_free(&mut self) -> Option<usize> {
            let mut told = self.0.lock().unwrap();
            if let Some(index) = told.free.pop_front() {
                return Some(index);
            }
            let played = told.played.unwrap_or(0.0) as usize / super::PIECE_FRAMES;
            (told.enqueued < told.buffers + played).then(|| told.enqueued % told.buffers.max(1))
        }
        fn enqueue(&mut self, _index: usize, samples: &[f32]) -> Result<(), ()> {
            let mut told = self.0.lock().unwrap();
            if told.refuse {
                return Err(());
            }
            told.queued.extend_from_slice(samples);
            told.enqueued += 1;
            Ok(())
        }
        fn start(&mut self) -> Result<(), ()> {
            self.0.lock().unwrap().started += 1;
            Ok(())
        }
        fn pause(&mut self) -> Result<(), ()> {
            self.0.lock().unwrap().paused += 1;
            Ok(())
        }
        fn set_muted(&mut self, muted: bool) {
            self.0.lock().unwrap().muted.push(muted);
        }
        fn played(&self) -> Option<(f64, Instant)> {
            self.0
                .lock()
                .unwrap()
                .played
                .map(|played| (played, Instant::now()))
        }
    }

    /// A sound of `blocks`, each its frames' value and where it plays.
    pub(crate) struct Blocks(pub(crate) VecDeque<(f32, usize, u64)>);

    impl AudioSource for Blocks {
        /// As a decoder does: as many frames as fit, the rest next time.
        fn next_block(&mut self, samples: &mut [f32]) -> Result<Option<AudioBlock>, FailureCode> {
            let Some((value, frames, at_frame)) = self.0.pop_front() else {
                return Ok(None);
            };
            let fit = frames.min(samples.len() / CHANNELS);
            samples[..fit * CHANNELS].fill(value);
            if fit < frames {
                self.0
                    .push_front((value, frames - fit, at_frame + fit as u64));
            }
            Ok(Some(AudioBlock {
                frames: fit,
                at_frame,
            }))
        }
    }

    pub(crate) fn stand(free: usize) -> (Box<dyn Speaker>, Arc<Mutex<Told>>) {
        let told = Arc::new(Mutex::new(Told {
            free: (0..free).collect(),
            buffers: free,
            ..Told::default()
        }));
        (Box::new(Stand(Arc::clone(&told))), told)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::testing::{Blocks, stand};
    use super::*;

    fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !done() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Before Play the speaker is filled but not started; the sound is
    /// queued in place, its gap and what follows its end silence; Play,
    /// Pause and Mute reach the speaker once each.
    #[test]
    fn the_speaker_is_filled_in_place_and_follows_the_session() {
        let (speaker, told) = stand(3);
        // 600 frames of 1 from frame 1000, a gap of 400, 500 frames of 2.
        let source = Box::new(Blocks(VecDeque::from([
            (1.0, 600, 1_000),
            (2.0, 500, 2_000),
        ])));
        let sound = Sound::start(speaker, source, 1_000, SoundWish::new(true)).unwrap();
        until("filled", || {
            told.lock().unwrap().queued.len() == 3 * PIECE_FRAMES * CHANNELS
        });
        assert_eq!(told.lock().unwrap().started, 0, "nothing plays before Play");
        {
            let told = told.lock().unwrap();
            let left: Vec<f32> = told.queued.iter().step_by(CHANNELS).copied().collect();
            assert!(left[..600].iter().all(|v| *v == 1.0));
            assert!(
                left[600..1_000].iter().all(|v| *v == 0.0),
                "the gap is silence"
            );
            assert!(left[1_000..1_500].iter().all(|v| *v == 2.0));
            assert!(
                left[1_500..].iter().all(|v| *v == 0.0),
                "after the end, silence"
            );
            assert_eq!(told.muted, [true]);
        }
        sound.wish().hear(PlayCommand::Play);
        until("started", || told.lock().unwrap().started == 1);
        sound.wish().hear(PlayCommand::Unmute);
        until("unmuted", || told.lock().unwrap().muted == [true, false]);
        sound.wish().hear(PlayCommand::Pause);
        until("paused", || told.lock().unwrap().paused == 1);
        drop(sound);
    }

    /// The clock is the speaker's, from where the sound starts; once every
    /// frame of it has been heard the sound is done; a speaker that
    /// refuses a piece leaves the session silent.
    #[test]
    fn the_clock_follows_the_speaker_to_the_end() {
        let (speaker, told) = stand(3);
        let source = Box::new(Blocks(VecDeque::from([(1.0, 1_200, 48_000)])));
        let sound = Sound::start(speaker, source, 48_000, SoundWish::new(false)).unwrap();
        assert_eq!(sound.heard(), None);
        sound.wish().hear(PlayCommand::Play);
        told.lock().unwrap().played = Some(600.0);
        until("a clock", || {
            sound.heard().is_some_and(|heard| heard.frame == 48_600.0)
        });
        assert!(!sound.done());
        told.lock().unwrap().played = Some(1_200.0);
        until("done", || sound.done());
        drop(sound);

        let (speaker, told) = stand(0);
        told.lock().unwrap().refuse = true;
        let sound = Sound::start(
            speaker,
            Box::new(Blocks(VecDeque::new())),
            0,
            SoundWish::new(false),
        )
        .unwrap();
        told.lock().unwrap().free.push_back(0);
        until("failed", || sound.failed());
    }

    /// A speaker started that never gives a clock, or a feeding thread
    /// that stops on its own, leaves the session silent rather than
    /// waiting for a clock that will not come.
    #[test]
    fn a_sound_without_a_clock_is_given_up() {
        let (speaker, _told) = stand(3);
        let sound = Sound::start(
            speaker,
            Box::new(Blocks(VecDeque::new())),
            0,
            SoundWish::new(false),
        )
        .unwrap();
        sound.wish().hear(PlayCommand::Play);
        std::thread::sleep(CLOCK_WITHIN / 2);
        assert!(!sound.failed(), "given up too soon");
        until("given up", || sound.failed());

        /// A sound whose decoding panics.
        struct Breaks;
        impl AudioSource for Breaks {
            fn next_block(
                &mut self,
                _: &mut [f32],
            ) -> Result<Option<AudioBlock>, kettle_media::FailureCode> {
                panic!("a decoder's bug")
            }
        }
        let (speaker, _told) = stand(3);
        let sound = Sound::start(speaker, Box::new(Breaks), 0, SoundWish::new(false)).unwrap();
        until("a stopped thread", || sound.failed());
    }

    /// Close, as the reader passes it on, ends the sound however the
    /// session's thread is held: its thread stops and lets its speaker go,
    /// the output disposed, while the Sound itself is still held.
    #[test]
    fn pending_pcm_does_not_hold_pause_or_consume_silence() {
        use kettle_media::audio::AudioRead;
        use std::sync::atomic::{AtomicBool, AtomicUsize};
        struct Pending {
            polls: Arc<AtomicUsize>,
            available: Arc<AtomicBool>,
        }
        impl AudioSource for Pending {
            fn next_block(
                &mut self,
                _: &mut [f32],
            ) -> Result<Option<AudioBlock>, kettle_media::FailureCode> {
                panic!("blocking source read used instead of PCM polling")
            }
            fn poll_block(
                &mut self,
                samples: &mut [f32],
            ) -> Result<AudioRead, kettle_media::FailureCode> {
                self.polls.fetch_add(1, Ordering::Release);
                if self.available.swap(false, Ordering::AcqRel) {
                    samples[..PIECE_FRAMES * CHANNELS].fill(0.25);
                    Ok(AudioRead::Block(AudioBlock {
                        frames: PIECE_FRAMES,
                        at_frame: 0,
                    }))
                } else {
                    Ok(AudioRead::Pending)
                }
            }
        }
        let polls = Arc::new(AtomicUsize::new(0));
        let available = Arc::new(AtomicBool::new(false));
        let (speaker, told) = stand(1);
        let sound = Sound::start(
            speaker,
            Box::new(Pending {
                polls: polls.clone(),
                available: available.clone(),
            }),
            0,
            SoundWish::new(false),
        )
        .unwrap();
        until("nonblocking PCM poll", || polls.load(Ordering::Acquire) > 0);
        sound.wish().hear(PlayCommand::Play);
        until("play while PCM pending", || {
            told.lock().unwrap().started == 1
        });
        sound.wish().hear(PlayCommand::Pause);
        until("pause while PCM pending", || {
            told.lock().unwrap().paused == 1
        });
        assert!(
            told.lock().unwrap().queued.is_empty(),
            "missing PCM became silence"
        );
        available.store(true, Ordering::Release);
        sound.wish().hear(PlayCommand::Play);
        until("real PCM queued", || {
            told.lock().unwrap().queued.len() == PIECE_FRAMES * CHANNELS
        });
        assert!(
            told.lock()
                .unwrap()
                .queued
                .iter()
                .all(|sample| *sample == 0.25)
        );
        assert!(!sound.failed());
    }

    #[test]
    fn a_partial_piece_survives_pending_pcm() {
        struct Partial(u8);
        impl AudioSource for Partial {
            fn next_block(
                &mut self,
                _: &mut [f32],
            ) -> Result<Option<AudioBlock>, kettle_media::FailureCode> {
                panic!("must poll asynchronous PCM")
            }
            fn poll_block(
                &mut self,
                samples: &mut [f32],
            ) -> Result<AudioRead, kettle_media::FailureCode> {
                self.0 += 1;
                match self.0 {
                    1 | 3 => {
                        samples[..PIECE_FRAMES / 2 * CHANNELS].fill(f32::from(self.0));
                        Ok(AudioRead::Block(AudioBlock {
                            frames: PIECE_FRAMES / 2,
                            at_frame: if self.0 == 1 {
                                0
                            } else {
                                PIECE_FRAMES as u64 / 2
                            },
                        }))
                    }
                    2 => Ok(AudioRead::Pending),
                    _ => Ok(AudioRead::End),
                }
            }
        }
        let (speaker, _) = stand(1);
        let mut feeder = Feeder::new(speaker, Box::new(Partial(0)), 0, false);
        assert!(!feeder.fill_piece().unwrap());
        assert_eq!(feeder.piece_filled, PIECE_FRAMES / 2);
        assert!(feeder.fill_piece().unwrap());
        assert!(
            feeder.piece[..PIECE_FRAMES / 2 * CHANNELS]
                .iter()
                .all(|n| *n == 1.0)
        );
        assert!(
            feeder.piece[PIECE_FRAMES / 2 * CHANNELS..]
                .iter()
                .all(|n| *n == 3.0)
        );
        assert_eq!(feeder.next_frame, PIECE_FRAMES as u64);
    }

    #[test]
    fn closing_interrupts_a_source_waiting_for_another_process() {
        use std::sync::atomic::AtomicBool;
        struct Waiting {
            entered: Arc<AtomicBool>,
            stopped: Arc<(Mutex<bool>, std::sync::Condvar)>,
        }
        impl AudioSource for Waiting {
            fn cancellation(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
                let stopped = Arc::clone(&self.stopped);
                Some(Arc::new(move || {
                    *stopped.0.lock().unwrap() = true;
                    stopped.1.notify_all();
                }))
            }
            fn next_block(
                &mut self,
                _: &mut [f32],
            ) -> Result<Option<AudioBlock>, kettle_media::FailureCode> {
                self.entered.store(true, Ordering::Release);
                let guard = self.stopped.0.lock().unwrap();
                let _ = self
                    .stopped
                    .1
                    .wait_timeout_while(guard, Duration::from_secs(1), |done| !*done)
                    .unwrap();
                Ok(None)
            }
        }
        let entered = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (speaker, _) = stand(1);
        let sound = Sound::start(
            speaker,
            Box::new(Waiting {
                entered: Arc::clone(&entered),
                stopped,
            }),
            0,
            SoundWish::new(false),
        )
        .unwrap();
        until("source waiting", || entered.load(Ordering::Acquire));
        let before = Instant::now();
        drop(sound);
        assert!(
            before.elapsed() < Duration::from_millis(250),
            "closing waited for decoded audio"
        );
    }

    #[test]
    fn close_ends_the_sound_and_lets_its_speaker_go() {
        let (speaker, told) = stand(3);
        let source = Box::new(Blocks(VecDeque::from([(0.5, 96_000, 0)])));
        let sound = Sound::start(speaker, source, 0, SoundWish::new(false)).unwrap();
        told.lock().unwrap().played = Some(4_800.0);
        sound.wish().hear(PlayCommand::Play);
        until("playing", || told.lock().unwrap().started == 1);
        assert!(!told.lock().unwrap().released);
        sound.wish().hear(PlayCommand::Close);
        until("its speaker let go", || told.lock().unwrap().released);
        drop(sound);
    }

    /// Time spent paused without a clock is not counted: only a second of
    /// playing without one gives the sound up.
    #[test]
    fn a_pause_without_a_clock_counts_for_nothing() {
        let (speaker, _told) = stand(3);
        let sound = Sound::start(
            speaker,
            Box::new(Blocks(VecDeque::new())),
            0,
            SoundWish::new(false),
        )
        .unwrap();
        sound.wish().hear(PlayCommand::Play);
        std::thread::sleep(CLOCK_WITHIN / 10);
        sound.wish().hear(PlayCommand::Pause);
        std::thread::sleep(CLOCK_WITHIN * 2);
        assert!(!sound.failed(), "given up while paused");
        sound.wish().hear(PlayCommand::Play);
        std::thread::sleep(CLOCK_WITHIN / 2);
        assert!(!sound.failed(), "the pause counted");
        until("given up", || sound.failed());
    }

    /// Paused, the sound has no clock: played again, its frames wait for
    /// a fresh one, never one kept from before the pause.
    #[test]
    fn a_paused_sound_has_no_clock() {
        let (speaker, told) = stand(3);
        let source = Box::new(Blocks(VecDeque::from([(0.5, 96_000, 0)])));
        let sound = Sound::start(speaker, source, 0, SoundWish::new(false)).unwrap();
        told.lock().unwrap().played = Some(4_800.0);
        sound.wish().hear(PlayCommand::Play);
        until("a clock", || sound.heard().is_some());
        sound.wish().hear(PlayCommand::Pause);
        told.lock().unwrap().played = None;
        until("no clock while paused", || sound.heard().is_none());
        sound.wish().hear(PlayCommand::Play);
        std::thread::sleep(ROUND * 4);
        assert_eq!(sound.heard(), None, "a clock from before the pause");
        told.lock().unwrap().played = Some(9_600.0);
        until("a fresh clock", || {
            sound.heard().is_some_and(|heard| heard.frame == 9_600.0)
        });
    }

    /// A clock that goes away while the sound plays is no clock: the
    /// sound says it has none, so frames wait, and a second without one
    /// gives the sound up.
    #[test]
    fn a_clock_lost_while_playing_is_none() {
        let (speaker, told) = stand(3);
        let source = Box::new(Blocks(VecDeque::from([(0.5, 96_000, 0)])));
        let sound = Sound::start(speaker, source, 0, SoundWish::new(false)).unwrap();
        told.lock().unwrap().played = Some(4_800.0);
        sound.wish().hear(PlayCommand::Play);
        until("a clock", || sound.heard().is_some());
        told.lock().unwrap().played = None;
        until("the clock gone", || sound.heard().is_none());
        std::thread::sleep(CLOCK_WITHIN / 2);
        assert!(!sound.failed(), "given up too soon");
        until("given up", || sound.failed());
    }
}
