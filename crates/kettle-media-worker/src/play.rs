//! A playback session: one video file, from PlayOpen until it ends, is
//! closed, or its parent goes quiet.
//!
//! The worker holds the file and confines itself to it and its decoder
//! before reading a byte of it, then opens it: Opened goes out, then the
//! first frame, and the session waits paused. Play and Pause are answered
//! with the state they leave it in; every frame goes out in order at its
//! time, and one that is late (a slow decoder, or a parent that stopped
//! reading for a while) goes out at once and moves the clock with it, so
//! the session runs behind rather than bursting to catch up: a silent video
//! has nothing to keep up with. The end is the last frame, then Ended, then
//! exit. A seek is a new session, which the parent opens.
//!
//! The parent's commands are read from the moment the worker is confined,
//! on a thread of their own that keeps what the parent last asked rather
//! than queueing it behind frames; once the video is open another thread
//! takes frames from the decoder into a fixed set of buffers. This thread
//! paces them and is the only writer. Every command renews the parent's
//! lease, from when it came; when it lapses the worker kills its process
//! group, decoder and all, needing nothing from the parent. Close, or the
//! parent leaving, ends the session with 0 even while this thread is held.
//! The watchdog ends the process when this thread stops coming round, a
//! frame the parent will not read included.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use kettle_media::audio::{AudioSource, SAMPLE_RATE};
use kettle_media::video::{
    MAX_VIDEO_PREFIX_BYTES, PlayGate, PlayRequest, VideoInput, VideoPlayer, sniff_video_container,
};
use kettle_media::wire::{
    Direction, Frame, HEADER_BYTES, WireError, read_frame_within, write_frame, write_video_frame,
};
use kettle_media::{
    AudioRequest, AudioStatus, FailureCode, Opened, PlayCommand, PlayOpen, PlayState, VideoFrameRef,
};
use kettle_media_render::source::{self, Held};

#[cfg(target_os = "macos")]
use crate::sound::Speaker;
use crate::sound::{Heard, Sound, SoundWish};
use crate::worker::{EXIT_PROTOCOL, Watchdog, refuse, reply};

/// From PlayOpen until the first frame is written: the file held, the worker
/// confined, the video described and its decoder started. Past it the
/// session fails as timed out, and the watchdog ends the process a second
/// later if that answer is held up too.
const OPEN_DEADLINE: Duration = Duration::from_secs(5);
const OPEN_GRACE: Duration = Duration::from_secs(1);
/// How long the parent may go without a command before the session ends.
/// It sends a heartbeat each second.
pub(crate) const LEASE: Duration = Duration::from_secs(5);
/// How long this thread may go without coming round: a write the parent
/// does not read, or anything else holding it, ends the process.
const HANG: Duration = Duration::from_secs(5);
/// How long a playing session may go without a frame before it fails, on
/// top of two of its frames' length.
const STALL: Duration = Duration::from_secs(5);
/// How often this thread comes round when nothing else wakes it.
const TICK: Duration = Duration::from_millis(250);
/// The frames a session holds at once: one being decoded, one waiting for
/// its time, one being written.
const BUFFERS: usize = 3;
/// A command is one byte: nothing larger is read from the parent.
const MAX_COMMAND_FRAME_BYTES: usize = HEADER_BYTES + 1;
/// Most events waiting for this thread: commands queue behind it, and the
/// decoder holds at most its buffers.
const EVENTS: usize = 16;
/// How long the reader gives this thread to end the session once the
/// parent closes it or leaves, before it ends the process itself: this
/// thread may be held in a write the parent no longer reads, or in the
/// decoder's opening.
const CLOSE_GRACE: Duration = Duration::from_millis(500);

/// What the reader and the decoder tell this thread. The parent's commands
/// are not among them: the reader keeps what the parent asked in [`Asked`],
/// and only wakes this thread.
enum Event {
    /// The parent asked something: look at [`Asked`].
    Asked,
    Frame {
        pts_ms: u64,
        rgba: Vec<u8>,
    },
    /// The video has no frame after the last one sent.
    End,
    Failed(FailureCode),
}

/// What the parent last asked of the session, as the command reader heard
/// it. Kept here rather than queued behind the decoder's frames, so no
/// command waits on a write the parent does not read, a full queue or the
/// decoder's opening: this thread reads it on each pass.
struct Asked(std::sync::Mutex<Wishes>);

#[derive(Clone, Copy)]
struct Wishes {
    /// The latest Play (true) or Pause (false) not yet taken.
    play: Option<bool>,
    /// When the reader took that Play or Pause: the first of a run of the
    /// same one.
    play_at: Instant,
    /// When the reader took the parent's last command: the lease runs from
    /// there.
    last: Instant,
    /// The parent closed the session, or left.
    closed: bool,
    /// The parent sent something other than a command (`None`), or a frame
    /// that could not be read.
    broken: Option<Option<WireError>>,
}

impl Asked {
    fn new() -> Self {
        Self(std::sync::Mutex::new(Wishes {
            play: None,
            play_at: Instant::now(),
            last: Instant::now(),
            closed: false,
            broken: None,
        }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Wishes> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The parent sent `command`, now.
    fn hear(&self, command: PlayCommand) {
        let mut heard = self.lock();
        heard.last = Instant::now();
        match command {
            PlayCommand::Play | PlayCommand::Pause => {
                let play = command == PlayCommand::Play;
                // A wish made again keeps when it was first made.
                if heard.play != Some(play) {
                    heard.play_at = heard.last;
                }
                heard.play = Some(play);
            }
            PlayCommand::Close => heard.closed = true,
            // The sound's own: the reader passes them on as they come.
            PlayCommand::Mute | PlayCommand::Unmute => {}
            PlayCommand::Heartbeat => {}
        }
    }

    /// The parent closed its end: it has left.
    fn leave(&self) {
        self.lock().closed = true;
    }

    /// The parent broke the protocol; the first break is the one answered.
    fn break_off(&self, error: Option<WireError>) {
        self.lock().broken.get_or_insert(error);
    }

    /// What the parent asked, the Play or Pause and the Mute or Unmute
    /// taken.
    fn take(&self) -> Wishes {
        let mut heard = self.lock();
        let now = *heard;
        heard.play = None;
        now
    }
}

/// Serve the session `open` begins, reading commands from `input` and
/// writing on `output`; return the exit code.
pub(crate) fn serve(
    open: &PlayOpen,
    input: impl Read + Send + 'static,
    output: &mut impl Write,
    watchdog: &Watchdog,
) -> i32 {
    let opened_by = Instant::now() + OPEN_DEADLINE;
    watchdog.arm(OPEN_DEADLINE + OPEN_GRACE);
    let configured = kettle_media_native::Decoders::configured();
    // Only Apple's decoder plays a session's sound: a session asking for
    // sound is confined for it with that decoder alone, so no external
    // decoder ever runs with the grant. Without it, none is granted.
    let sounding = (open.audio != AudioRequest::Off)
        .then(|| configured.sounding())
        .flatten();
    let sound = sounding.is_some();
    let decoders = sounding.unwrap_or(configured);
    if confine(open, &decoders, sound).is_err() || !watchdog.confined() {
        // A video is never decoded unconfined.
        return reply(output, FailureCode::SandboxUnavailable, 0);
    }
    // The parent is heard from here on, confined, the decoder's opening
    // included.
    let asked = Arc::new(Asked::new());
    let gate = Arc::new(PlayGate::default());
    let sound_wish = SoundWish::new(open.audio == AudioRequest::Muted);
    let (events, received) = mpsc::sync_channel(EVENTS);
    if start_reader(
        input,
        Arc::clone(&asked),
        Arc::clone(&gate),
        sound_wish.clone(),
        events.clone(),
        watchdog.clone(),
    )
    .is_err()
        || start_lease(Arc::clone(&asked)).is_err()
    {
        return reply(output, FailureCode::RenderResource, 0);
    }
    let held = match source::hold(&open.source, MAX_VIDEO_PREFIX_BYTES) {
        Ok(held) => held,
        Err(code) => return reply(output, code, 0),
    };
    let Held::File {
        file,
        identity,
        prefix,
    } = &held
    else {
        return reply(output, FailureCode::UnsupportedMedia, 0);
    };
    let Some(container) = sniff_video_container(prefix, identity.size) else {
        return reply(output, FailureCode::UnsupportedMedia, 0);
    };
    let player: Option<&dyn VideoPlayer> =
        (!decoders.is_empty()).then_some(&decoders as &dyn VideoPlayer);
    #[cfg(feature = "test-faults")]
    let fake = faults::player(prefix);
    #[cfg(feature = "test-faults")]
    let player = fake
        .as_ref()
        .map(|fake| fake as &dyn VideoPlayer)
        .or(player);
    let Some(player) = player else {
        return reply(output, FailureCode::BackendUnavailable, 0);
    };
    let video = VideoInput {
        file,
        identity: *identity,
        container,
    };
    let opening = player.play(video, PlayRequest::of(open), opened_by);
    // A file changed while the decoder opened it is Changed, whatever the
    // decoder made of it.
    if held.unchanged().is_err() {
        return reply(output, FailureCode::Changed, 0);
    }
    let playback = match opening {
        Ok(playback) => playback,
        Err(code) => return reply(output, code, 0),
    };
    let mut opened = playback.opened;
    // The player's decoded sound plays through this machine's output, when
    // it has one the worker may use; else Opened says it is unavailable.
    let sound = playback
        .audio
        .and_then(|source| start_sound(source, &opened, sound_wish));
    if opened.audio == AudioStatus::Ready && sound.is_none() {
        opened.audio = AudioStatus::Unavailable;
    }
    // The worker's own processor time, now the video's length is known.
    let left = opened.info.duration_ms.saturating_sub(opened.start_ms);
    if crate::early_unix::limit_session_cpu(left).is_err() {
        return reply(output, FailureCode::RenderResource, 0);
    }
    if write_frame(output, &Frame::Opened(opened), Direction::WorkerToParent).is_err() {
        return EXIT_PROTOCOL;
    }
    let (free, buffers) = mpsc::sync_channel(BUFFERS);
    let size = opened.width as usize * opened.height as usize * 4;
    for _ in 0..BUFFERS {
        let _ = free.send(vec![0; size]);
    }
    if start_decoder(playback.frames, Arc::clone(&gate), buffers, events).is_err() {
        return reply(output, FailureCode::RenderResource, 0);
    }
    let session = Session {
        opened,
        held: &held,
        asked: &asked,
        sound,
        free,
        output,
        watchdog,
    };
    let code = session.run(&received, opened_by);
    gate.end();
    code
}

/// Play `source`, the sound `opened` starts with, through this machine's
/// output as `wish` says: `None` where there is none or it cannot start.
#[cfg(target_os = "macos")]
fn start_sound(source: Box<dyn AudioSource>, opened: &Opened, wish: SoundWish) -> Option<Sound> {
    let output = kettle_media_native::audio_out::Output::open(wish.muted()).ok()?;
    let start_frame = kettle_media::audio::frame_of_ms(opened.start_ms);
    Sound::start(Box::new(OutputSpeaker(output)), source, start_frame, wish).ok()
}

/// Linux plays no session's sound: its decoder decodes none anyway.
#[cfg(not(target_os = "macos"))]
fn start_sound(_source: Box<dyn AudioSource>, _opened: &Opened, _wish: SoundWish) -> Option<Sound> {
    None
}

/// This machine's output queue, as the sound's speaker.
#[cfg(target_os = "macos")]
struct OutputSpeaker(kettle_media_native::audio_out::Output);

#[cfg(target_os = "macos")]
const _: () = assert!(crate::sound::PIECE_FRAMES == kettle_media_native::audio_out::BUFFER_FRAMES);

#[cfg(target_os = "macos")]
impl Speaker for OutputSpeaker {
    fn take_free(&mut self) -> Option<usize> {
        self.0.take_free()
    }
    fn enqueue(&mut self, index: usize, samples: &[f32]) -> Result<(), ()> {
        self.0.enqueue(index, samples).map_err(drop)
    }
    fn start(&mut self) -> Result<(), ()> {
        self.0.start().map_err(drop)
    }
    fn pause(&mut self) -> Result<(), ()> {
        self.0.pause().map_err(drop)
    }
    fn set_muted(&mut self, muted: bool) {
        self.0.set_muted(muted);
    }
    fn played(&self) -> Option<(f64, Instant)> {
        self.0.played()
    }
}

/// Confine the worker to `open`'s file and what the decoders need, with
/// this machine's sound output when `sound`, before any byte of the file is
/// read.
fn confine(
    open: &PlayOpen,
    decoders: &kettle_media_native::Decoders,
    sound: bool,
) -> Result<(), kettle_media_native::sandbox::SandboxError> {
    let files = source::hold_source(&open.source);
    let (programs, trees) = decoders.sandbox_needs();
    kettle_media_native::sandbox::confine(&kettle_media_native::sandbox::Policy {
        files: files.iter().collect(),
        programs,
        trees,
        audio_output: sound,
    })
}

/// Read the parent's commands into `asked`, waking the session's thread,
/// until the parent leaves, closes the session or breaks the protocol. Play
/// and Pause move the decoder's `gate` as they come, and Close or the parent
/// leaving ends it, whatever the session's thread is doing: the gate always
/// follows the latest command, and after a Pause only a frame already begun
/// finishes. Play, Pause, Mute and Unmute reach the session's `sound` the
/// same way, so a Pause or Mute is heard at once, and Close or the parent
/// leaving ends it, its output disposed before the process exits. Once the parent closes or leaves, the session ends with 0: on
/// the thread's next pass, or, should the thread be held, from here within
/// `CLOSE_GRACE`, the watchdog moved past it first.
fn start_reader(
    mut input: impl Read + Send + 'static,
    asked: Arc<Asked>,
    gate: Arc<PlayGate>,
    sound: SoundWish,
    events: mpsc::SyncSender<Event>,
    watchdog: Watchdog,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("kettle-media-commands".into())
        .spawn(move || {
            loop {
                match read_frame_within(
                    &mut input,
                    Direction::ParentToWorker,
                    MAX_COMMAND_FRAME_BYTES,
                ) {
                    Ok(Some(Frame::PlayCommand(command))) => {
                        asked.hear(command);
                        sound.hear(command);
                        match command {
                            PlayCommand::Play => gate.play(),
                            PlayCommand::Pause => gate.pause(),
                            PlayCommand::Close => {
                                gate.end();
                                break;
                            }
                            _ => {}
                        }
                    }
                    Ok(None) => {
                        asked.leave();
                        gate.end();
                        sound.close();
                        break;
                    }
                    Ok(Some(_)) => {
                        asked.break_off(None);
                        let _ = events.try_send(Event::Asked);
                        return;
                    }
                    Err(error) => {
                        asked.break_off(Some(error));
                        let _ = events.try_send(Event::Asked);
                        return;
                    }
                }
                // A full queue is a thread already coming round.
                let _ = events.try_send(Event::Asked);
            }
            let _ = events.try_send(Event::Asked);
            watchdog.arm(2 * CLOSE_GRACE);
            std::thread::sleep(CLOSE_GRACE);
            std::process::exit(0);
        })
        .map(drop)
}

/// Hold the parent to its lease on a thread of its own, so it runs out on
/// time whatever the session's thread is held on: 5 s after the last
/// command the reader took, the worker kills its process group. Once the
/// parent closes the session or leaves, the reader ends it, and the lease
/// is over.
fn start_lease(asked: Arc<Asked>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("kettle-media-lease".into())
        .spawn(move || {
            loop {
                // Decided under the lock: a command the reader takes
                // meanwhile renews the lease before this looks, or came
                // after it lapsed.
                let left = {
                    let heard = asked.lock();
                    if heard.closed {
                        return;
                    }
                    (heard.last + LEASE).checked_duration_since(Instant::now())
                };
                match left {
                    Some(left) if !left.is_zero() => std::thread::sleep(left.min(TICK)),
                    _ => end_group(),
                }
            }
        })
        .map(drop)
}

/// Take frames from the decoder into free buffers: the first at once, the
/// rest while the session plays.
fn start_decoder(
    mut frames: Box<dyn kettle_media::video::FrameSource>,
    gate: Arc<PlayGate>,
    buffers: mpsc::Receiver<Vec<u8>>,
    events: mpsc::SyncSender<Event>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("kettle-media-decoder".into())
        .spawn(move || {
            let mut first = true;
            loop {
                let Ok(mut rgba) = buffers.recv() else {
                    return;
                };
                // Asked once a buffer is free, so a Pause that came while
                // this waited for one stops the next frame too; the first is
                // taken paused, unless the session has ended (closed while
                // it opened).
                let go = if first {
                    !gate.has_ended()
                } else {
                    gate.wait_to_play(None) == Some(true)
                };
                if !go {
                    return;
                }
                let event = match frames.next_frame(&mut rgba, &gate) {
                    Ok(Some(pts_ms)) => Event::Frame { pts_ms, rgba },
                    Ok(None) => Event::End,
                    Err(code) => Event::Failed(code),
                };
                let last = !matches!(event, Event::Frame { .. });
                if events.send(event).is_err() || last {
                    return;
                }
                first = false;
            }
        })
        .map(drop)
}

/// The pacing thread's state.
struct Session<'a, W: Write> {
    opened: Opened,
    held: &'a Held<'a>,
    asked: &'a Asked,
    /// The session's sound while it has some: its clock then paces frames.
    sound: Option<Sound>,
    free: mpsc::SyncSender<Vec<u8>>,
    output: &'a mut W,
    watchdog: &'a Watchdog,
}

/// How far ahead of its sound a frame goes out: about what the parent takes
/// to show it, so picture and sound arrive together.
const VIDEO_LEAD: Duration = Duration::from_millis(40);
/// How soon this thread looks again for a clock the sound does not have yet.
const SOUND_SOON: Duration = Duration::from_millis(10);

/// When a frame goes out while the session's sound plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Due {
    Now,
    At(Instant),
    /// The sound's clock has no time yet.
    Wait,
    /// The sound has passed it by more than a frame: it is dropped.
    Late,
}

/// A frame's length at `fps_milli`, to the microsecond: a third of a
/// millisecond counts when deciding whether the sound passed a frame by.
fn frame_length(fps_milli: u32) -> Duration {
    Duration::from_micros((1_000_000_000 / u64::from(fps_milli.max(1))).max(1))
}

/// When the frame at `pts_ms`, a `frame` long, goes out as of `now`, with
/// the sound heard where `heard` says: `VIDEO_LEAD` before its sound is
/// heard, and late once the sound has passed it by more than a frame.
fn due_by_sound(heard: Option<Heard>, pts_ms: u64, frame: Duration, now: Instant) -> Due {
    let Some(heard) = heard else {
        return Due::Wait;
    };
    let at_frame = kettle_media::audio::frame_of_ms(pts_ms) as f64;
    let ahead = (at_frame - heard.frame) / f64::from(SAMPLE_RATE);
    // `at` moved by `seconds`, earlier when negative, never before nothing.
    let moved = |at: Instant, seconds: f64| {
        if seconds >= 0.0 {
            at + Duration::from_secs_f64(seconds)
        } else {
            at.checked_sub(Duration::from_secs_f64(-seconds))
                .unwrap_or(at)
        }
    };
    let sounds = moved(heard.at, ahead);
    let goes = moved(sounds, -VIDEO_LEAD.as_secs_f64());
    if goes > now {
        Due::At(goes)
    } else if now.saturating_duration_since(sounds) > frame {
        Due::Late
    } else {
        Due::Now
    }
}

/// Where the session's clock was set: the moment a frame was written after
/// it began playing, and that frame's time.
#[derive(Clone, Copy)]
struct Clock {
    origin: Instant,
    origin_pts: u64,
}

/// When each frame of a playing session goes out. A frame written while it
/// plays sets the clock when none is set, as for the first after Play, and
/// when its write ended more than `late` past its time (a slow decoder, or a
/// parent slow to read it): the frames after keep their spacing from it
/// rather than going out together to catch up.
struct Pacer {
    late: Duration,
    clock: Option<Clock>,
}

impl Pacer {
    fn new(late: Duration) -> Self {
        Self { late, clock: None }
    }

    /// Play: the next frame written sets the clock.
    fn reset(&mut self) {
        self.clock = None;
    }

    /// When the frame at `pts_ms` is due; with no clock set, at once.
    fn due(&self, pts_ms: u64) -> Option<Instant> {
        self.clock
            .map(|set| set.origin + Duration::from_millis(pts_ms.saturating_sub(set.origin_pts)))
    }

    /// The frame at `pts_ms` was written, by `done`, while playing.
    fn written(&mut self, pts_ms: u64, done: Instant) {
        if self
            .due(pts_ms)
            .is_none_or(|due| done.saturating_duration_since(due) > self.late)
        {
            self.clock = Some(Clock {
                origin: done,
                origin_pts: pts_ms,
            });
        }
    }
}

impl<W: Write> Session<'_, W> {
    fn run(mut self, events: &mpsc::Receiver<Event>, opened_by: Instant) -> i32 {
        let frame_ms = (1_000_000 / u64::from(self.opened.fps_milli)).max(1);
        // A frame written this far past its time sets the clock again.
        let mut pacer = Pacer::new(Duration::from_millis(frame_ms.max(20)));
        let stall = STALL + Duration::from_millis(2 * frame_ms);
        let frame_len = frame_length(self.opened.fps_milli);
        let mut pending: VecDeque<(u64, Vec<u8>)> = VecDeque::with_capacity(BUFFERS);
        let mut playing = false;
        let mut shown_first = false;
        // The first frame, when it was written paused: its time, and when
        // its write ended.
        let mut first_paused: Option<(u64, Instant)> = None;
        let mut decoded_all = false;
        let mut seq = 0u64;
        let mut last_frame = Instant::now();
        loop {
            let asked = self.asked.take();
            if asked.closed {
                return 0;
            }
            // A frame out of place is refused as `refuse` refuses any.
            if let Some(error) = asked.broken {
                return refuse(self.output, error.map_or(Ok(None), Err));
            }
            match asked.play {
                Some(true) if !playing => {
                    playing = true;
                    pacer.reset();
                    // A Play the reader took before the first frame's write
                    // ended came before that frame: its write sets the clock.
                    if let Some((pts_ms, done)) = first_paused.take()
                        && asked.play_at <= done
                    {
                        pacer.written(pts_ms, done);
                    }
                    last_frame = Instant::now();
                    if shown_first && !self.state(PlayState::Playing) {
                        return EXIT_PROTOCOL;
                    }
                }
                Some(false) if playing => {
                    playing = false;
                    if shown_first && !self.state(PlayState::Paused) {
                        return EXIT_PROTOCOL;
                    }
                }
                _ => {}
            }
            // The lease is the lease thread's to hold, and the sound's
            // wishes the reader's to pass on.
            // A speaker that fails leaves the session silent, paced by its
            // own clock from here, and the parent told.
            if self.sound.as_ref().is_some_and(Sound::failed) {
                self.sound = None;
                pacer.reset();
                if write_frame(
                    self.output,
                    &Frame::AudioState(AudioStatus::Unavailable),
                    Direction::WorkerToParent,
                )
                .is_err()
                {
                    return EXIT_PROTOCOL;
                }
            }
            let mut now = Instant::now();
            if shown_first {
                self.watchdog.arm(HANG);
            }
            if !shown_first && now >= opened_by {
                return self.fail(FailureCode::RenderTimeout);
            }
            let mut sound_due = None;
            while let Some(&(pts_ms, _)) = pending.front() {
                if shown_first {
                    if !playing {
                        break;
                    }
                    match &self.sound {
                        Some(sound) => match due_by_sound(sound.heard(), pts_ms, frame_len, now) {
                            Due::Wait => {
                                sound_due = Some(now + SOUND_SOON);
                                break;
                            }
                            Due::At(at) => {
                                sound_due = Some(at);
                                break;
                            }
                            // Dropped for the newer frame waiting behind it,
                            // so never the video's last, which the lane
                            // holds.
                            Due::Late if pending.len() > 1 => {
                                let (_, rgba) = pending.pop_front().expect("a waiting frame");
                                let _ = self.free.send(rgba);
                                last_frame = now;
                                continue;
                            }
                            Due::Late | Due::Now => {}
                        },
                        None => {
                            if pacer.due(pts_ms).is_some_and(|due| due > now) {
                                break;
                            }
                        }
                    }
                }
                let (_, rgba) = pending.pop_front().expect("a waiting frame");
                let frame = VideoFrameRef {
                    seq,
                    pts_ms,
                    width: self.opened.width,
                    height: self.opened.height,
                    rgba: &rgba,
                };
                if write_video_frame(self.output, frame).is_err() {
                    return EXIT_PROTOCOL;
                }
                seq += 1;
                now = Instant::now();
                last_frame = now;
                if !playing {
                    if !shown_first {
                        first_paused = Some((pts_ms, now));
                    }
                } else if self.sound.is_none() {
                    pacer.written(pts_ms, now);
                }
                let _ = self.free.send(rgba);
                if !shown_first {
                    // A first frame whose write ended past the deadline did
                    // not open the session in time, whatever the parent read.
                    if now > opened_by {
                        return self.fail(FailureCode::RenderTimeout);
                    }
                    shown_first = true;
                    // Open: the opening's allowance ends with its first
                    // frame, before anything more is written.
                    self.watchdog.arm(HANG);
                    let state = if playing {
                        PlayState::Playing
                    } else {
                        PlayState::Paused
                    };
                    if !self.state(state) {
                        return EXIT_PROTOCOL;
                    }
                }
            }
            // Opening, paused, playing or past its last frame: a session
            // never outlives a change to its file, nor ends as if it had not
            // changed.
            if self.held.unchanged().is_err() {
                return reply(self.output, FailureCode::Changed, 0);
            }
            // The end is the last frame and the last of the sound, both.
            let sounding = self.sound.as_ref().is_some_and(|sound| !sound.done());
            if decoded_all && pending.is_empty() && !sounding {
                return if self.state(PlayState::Ended) {
                    0
                } else {
                    EXIT_PROTOCOL
                };
            }
            // Past the video's last frame only its sound plays: no stall.
            if shown_first && playing && !decoded_all && now.duration_since(last_frame) > stall {
                return self.fail(FailureCode::RenderTimeout);
            }
            let mut wake = now + TICK;
            if !shown_first {
                wake = wake.min(opened_by);
            }
            if playing
                && self.sound.is_none()
                && let Some(due) = pending.front().and_then(|&(pts_ms, _)| pacer.due(pts_ms))
            {
                wake = wake.min(due);
            }
            if let Some(due) = sound_due {
                wake = wake.min(due);
            }
            if playing && decoded_all && sounding {
                wake = wake.min(now + SOUND_SOON);
            }
            match events.recv_timeout(wake.saturating_duration_since(Instant::now())) {
                Ok(Event::Asked) | Err(RecvTimeoutError::Timeout) => {}
                Ok(Event::Frame { pts_ms, rgba }) => pending.push_back((pts_ms, rgba)),
                Ok(Event::End) => decoded_all = true,
                Ok(Event::Failed(code)) => return self.fail(code),
                // The reader and the decoder have both gone: the reader
                // only after the parent broke the protocol, which is
                // answered first.
                Err(RecvTimeoutError::Disconnected) => {
                    let asked = self.asked.take();
                    if let Some(error) = asked.broken {
                        return refuse(self.output, error.map_or(Ok(None), Err));
                    }
                    return if asked.closed { 0 } else { EXIT_PROTOCOL };
                }
            }
        }
    }

    /// End the session with `code`, unless the file changed meanwhile: a
    /// decoder that fails or stalls on a file changed under it has failed
    /// for the change.
    fn fail(&mut self, code: FailureCode) -> i32 {
        let code = if self.held.unchanged().is_err() {
            FailureCode::Changed
        } else {
            code
        };
        reply(self.output, code, 0)
    }

    /// Say where the session is; false when the parent cannot be told.
    fn state(&mut self, state: PlayState) -> bool {
        write_frame(
            self.output,
            &Frame::PlayState(state),
            Direction::WorkerToParent,
        )
        .is_ok()
    }
}

/// End the whole process group, the decoder with it: the parent has gone
/// quiet, and nothing it might still do can be waited for. Only a worker
/// that leads its group, as Kettle starts one, signals it; any other just
/// exits, and its decoder goes with the pipe it writes to.
fn end_group() -> ! {
    // SAFETY: getpgrp, getpid and kill take no pointers. Both sandboxes let
    // a worker signal its own group.
    let killed = unsafe { libc::getpgrp() == libc::getpid() && libc::kill(0, libc::SIGKILL) == 0 };
    if killed {
        // The kill is on its way here too, but may land after kill returns:
        // wait for it, so the group ends by the kill and never by an exit
        // that beat it.
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    std::process::exit(crate::worker::EXIT_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use std::io::{PipeReader, PipeWriter};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};

    use kettle_media::video::FrameSource;
    use kettle_media::wire::read_frame;
    use kettle_media::{GuiActionWitness, NativePath, Source, VideoCodec, VideoInfo};

    use super::*;
    use crate::worker::Watchdog;

    const SECOND: Duration = Duration::from_secs(1);

    fn opened(fps_milli: u32, width: u32, height: u32) -> Opened {
        Opened {
            info: VideoInfo {
                duration_ms: 60_000,
                width,
                height,
                rotation: 0,
                codec: VideoCodec::H264,
                fps_milli: Some(fps_milli),
                has_audio: false,
                container: None,
            },
            width,
            height,
            fps_milli,
            start_ms: 0,
            audio: kettle_media::AudioStatus::NoTrack,
        }
    }

    fn frame(opened: &Opened, pts_ms: u64) -> Event {
        Event::Frame {
            pts_ms,
            rgba: vec![pts_ms as u8; opened.width as usize * opened.height as usize * 4],
        }
    }

    /// What a test gives a session, in order: the parent's commands, as
    /// the command reader leaves them, and the decoder's events.
    enum Step {
        Ask(PlayCommand),
        Send(Event),
    }

    /// What the session wrote and when: the end of every write, as bytes
    /// written so far, and whether a write has begun.
    #[derive(Default)]
    struct Written {
        ends: Mutex<Vec<(u64, Instant)>>,
        begun: Mutex<bool>,
        changed: Condvar,
    }

    impl Written {
        /// When the byte at `offset` (counted from 1) was written.
        fn at(&self, offset: u64) -> Instant {
            self.ends
                .lock()
                .unwrap()
                .iter()
                .find(|(total, _)| *total >= offset)
                .map(|(_, at)| *at)
                .expect("every byte read was written")
        }
    }

    /// The session's output, timing each write as it ends. A frame's time is
    /// then when its last byte was written, which is what the session's
    /// clock goes by, however late a test thread reads it.
    struct Timed<'a> {
        pipe: PipeWriter,
        written: &'a Written,
        total: u64,
    }

    impl Write for Timed<'_> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            *self.written.begun.lock().unwrap() = true;
            self.written.changed.notify_all();
            let n = self.pipe.write(buf)?;
            self.total += n as u64;
            self.written
                .ends
                .lock()
                .unwrap()
                .push((self.total, Instant::now()));
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.pipe.flush()
        }
    }

    /// The parent's side of a session under test, and its decoder's.
    struct Parent<'a> {
        asked: &'a Asked,
        /// The session's sound's wishes, which the reader passes commands
        /// on to.
        sound: Option<SoundWish>,
        events: mpsc::SyncSender<Event>,
        written: &'a Written,
    }

    impl Parent<'_> {
        /// Wait until the session has begun its first write.
        fn first_write(&self) {
            let begun = self.written.begun.lock().unwrap();
            let (begun, timeout) = self
                .written
                .changed
                .wait_timeout_while(begun, 10 * SECOND, |begun| !*begun)
                .unwrap();
            assert!(*begun && !timeout.timed_out(), "the session never wrote");
        }

        fn ask(&self, command: PlayCommand) {
            self.asked.hear(command);
            if let Some(sound) = &self.sound {
                sound.hear(command);
            }
            let _ = self.events.try_send(Event::Asked);
        }

        fn send(&self, event: Event) {
            let _ = self.events.send(event);
        }
    }

    /// Run a session over `held` on a thread of its own, giving it `steps`
    /// in order, and read what it writes as the parent does until it ends:
    /// each frame, with when its last byte was written. `before_read` is
    /// told how many frames came before each read, so a test can stop
    /// reading for a while; `later` runs beside the reading.
    fn run(
        opened: Opened,
        held: &Held<'_>,
        steps: Vec<Step>,
        before_read: impl Fn(usize),
        later: impl FnOnce(&Parent) + Send,
    ) -> Vec<(Instant, Frame)> {
        run_with(opened, held, None, steps, before_read, later)
    }

    /// [`run`], with `sound` as the session's sound.
    fn run_with(
        opened: Opened,
        held: &Held<'_>,
        sound: Option<Sound>,
        steps: Vec<Step>,
        before_read: impl Fn(usize),
        later: impl FnOnce(&Parent) + Send,
    ) -> Vec<(Instant, Frame)> {
        let asked = Asked::new();
        let written = Written::default();
        let wish = sound.as_ref().map(Sound::wish);
        let read = std::thread::scope(|scope| {
            let (send, received) = mpsc::sync_channel(EVENTS);
            let (mut reader, writer) = std::io::pipe().unwrap();
            let asked = &asked;
            let written = &written;
            scope.spawn(move || {
                // Taken buffers go nowhere: no decoder runs here.
                let (free, _) = mpsc::sync_channel(BUFFERS);
                let watchdog = Watchdog::unwatched();
                let mut output = Timed {
                    pipe: writer,
                    written,
                    total: 0,
                };
                let session = Session {
                    opened,
                    held,
                    asked,
                    sound,
                    free,
                    output: &mut output,
                    watchdog: &watchdog,
                };
                session.run(&received, Instant::now() + OPEN_DEADLINE)
            });
            let parent = Parent {
                asked,
                sound: wish,
                events: send.clone(),
                written,
            };
            for step in steps {
                match step {
                    Step::Ask(command) => parent.ask(command),
                    Step::Send(event) => parent.send(event),
                }
            }
            scope.spawn(move || later(&parent));
            // Kept as the command reader keeps its own: a session whose
            // events all end has lost its parent.
            let read = read_all(&mut reader, before_read);
            drop(send);
            read
        });
        // The session has ended and every write with it.
        read.into_iter()
            .map(|(end, frame)| (written.at(end), frame))
            .collect()
    }

    /// Each frame read, with the offset of its last byte in the stream.
    fn read_all(reader: &mut PipeReader, before_read: impl Fn(usize)) -> Vec<(u64, Frame)> {
        struct Counted<'a>(&'a mut PipeReader, u64);
        impl Read for Counted<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.0.read(buf)?;
                self.1 += n as u64;
                Ok(n)
            }
        }
        let mut counted = Counted(reader, 0);
        let mut read = Vec::new();
        loop {
            before_read(read.len());
            match read_frame(&mut counted, Direction::WorkerToParent) {
                Ok(Some(frame)) => read.push((counted.1, frame)),
                Ok(None) => return read,
                Err(error) => panic!("{error:?} after {} frames", read.len()),
            }
        }
    }

    /// Each video frame's time, and when it came.
    fn shown(read: &[(Instant, Frame)]) -> Vec<(Instant, u64)> {
        read.iter()
            .filter_map(|(at, frame)| match frame {
                Frame::VideoFrame(frame) => Some((*at, frame.pts_ms)),
                _ => None,
            })
            .collect()
    }

    /// Every frame but the video's.
    fn said(read: &[(Instant, Frame)]) -> Vec<&Frame> {
        read.iter()
            .map(|(_, frame)| frame)
            .filter(|frame| !matches!(frame, Frame::VideoFrame(_)))
            .collect()
    }

    /// The clock keeps the frames after a frame's time, and one written
    /// late moves it.
    #[test]
    fn the_pacer_sets_its_clock_at_the_first_frame_and_a_late_one() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut pacer = Pacer::new(ms(100));
        assert_eq!(pacer.due(0), None);
        pacer.written(0, t0);
        assert_eq!(pacer.due(100), Some(t0 + ms(100)));
        // On time, within `late`: the clock stays.
        pacer.written(100, t0 + ms(190));
        assert_eq!(pacer.due(200), Some(t0 + ms(200)));
        // Done past it: the next frames keep their spacing from this one.
        pacer.written(200, t0 + ms(301));
        assert_eq!(pacer.due(300), Some(t0 + ms(401)));
        pacer.reset();
        assert_eq!(pacer.due(400), None);
    }

    /// Play before the first frame: that frame sets the clock, and the next
    /// waits its time after it.
    #[test]
    fn play_before_the_first_frame_sets_the_clock_there() {
        let opened = opened(5_000, 4, 2);
        let read = run(
            opened,
            &Held::Bytes(&[]),
            vec![
                Step::Ask(PlayCommand::Play),
                Step::Send(frame(&opened, 0)),
                Step::Send(frame(&opened, 200)),
                Step::Send(Event::End),
            ],
            |_| {},
            |_| {},
        );
        let shown = shown(&read);
        assert_eq!(
            shown.iter().map(|(_, pts)| *pts).collect::<Vec<_>>(),
            [0, 200]
        );
        let gap = shown[1].0 - shown[0].0;
        assert!(gap >= Duration::from_millis(150), "{gap:?}");
        assert_eq!(
            said(&read),
            [
                &Frame::PlayState(PlayState::Playing),
                &Frame::PlayState(PlayState::Ended)
            ]
        );
    }

    /// Play that comes while the first frame is being written, after the
    /// session last looked, still came before that frame: its write sets
    /// the clock, and the next frame waits its time after it.
    #[test]
    fn play_during_the_first_write_sets_the_clock_there() {
        // A first frame far larger than a pipe holds, so its write waits on
        // the reading, which waits for Play.
        let opened = opened(10_000, 512, 512);
        let (played, heard) = mpsc::channel();
        let read = run(
            opened,
            &Held::Bytes(&[]),
            vec![
                Step::Send(frame(&opened, 0)),
                Step::Send(frame(&opened, 100)),
                Step::Send(Event::End),
            ],
            move |count| {
                if count == 0 {
                    heard.recv_timeout(10 * SECOND).expect("Play was asked");
                }
            },
            move |parent| {
                parent.first_write();
                parent.ask(PlayCommand::Play);
                let _ = played.send(());
            },
        );
        let shown = shown(&read);
        assert_eq!(
            shown.iter().map(|(_, pts)| *pts).collect::<Vec<_>>(),
            [0, 100]
        );
        let gap = shown[1].0 - shown[0].0;
        assert!(gap >= Duration::from_millis(80), "{gap:?}");
    }

    /// Play that comes after the first frame was written paused, even
    /// within its length, leaves the clock to the next frame: it goes out
    /// at once rather than at the first frame's time.
    #[test]
    fn play_after_the_first_write_leaves_the_clock_to_the_next() {
        // A second a frame.
        let opened = opened(1_000, 4, 2);
        let (first, after) = mpsc::channel();
        let read = run(
            opened,
            &Held::Bytes(&[]),
            vec![
                Step::Send(frame(&opened, 0)),
                Step::Send(frame(&opened, 1_000)),
                Step::Send(Event::End),
            ],
            // Play once the first frame has been read, so written.
            move |count| {
                if count == 1 {
                    let _ = first.send(());
                }
            },
            move |parent| {
                after
                    .recv_timeout(10 * SECOND)
                    .expect("the first frame came");
                parent.ask(PlayCommand::Play);
            },
        );
        let shown = shown(&read);
        assert_eq!(
            shown.iter().map(|(_, pts)| *pts).collect::<Vec<_>>(),
            [0, 1_000]
        );
        let gap = shown[1].0 - shown[0].0;
        assert!(gap < Duration::from_millis(800), "{gap:?}");
    }

    /// A frame whose write the parent holds up past its time moves the
    /// clock when it is done: the next keeps its spacing from it instead of
    /// following it straight out.
    #[test]
    fn a_frame_held_up_in_its_write_moves_the_clock() {
        // A frame far larger than a pipe holds, a tenth of a second long.
        let opened = opened(10_000, 512, 512);
        let read = run(
            opened,
            &Held::Bytes(&[]),
            vec![
                Step::Ask(PlayCommand::Play),
                Step::Send(frame(&opened, 0)),
                Step::Send(frame(&opened, 100)),
                Step::Send(frame(&opened, 200)),
                Step::Send(frame(&opened, 300)),
                Step::Send(Event::End),
            ],
            // Frame 0 and Playing read, the parent looks away.
            |read| {
                if read == 2 {
                    std::thread::sleep(Duration::from_millis(600));
                }
            },
            |_| {},
        );
        let shown = shown(&read);
        assert_eq!(
            shown.iter().map(|(_, pts)| *pts).collect::<Vec<_>>(),
            [0, 100, 200, 300]
        );
        // The frame after the held one keeps its spacing from it. Later
        // gaps are pacing as usual, which a slow reader can bunch up within
        // a frame's lateness.
        let gap = shown[2].0 - shown[1].0;
        assert!(
            gap >= Duration::from_millis(60),
            "{gap:?} after the held frame"
        );
    }

    /// A paused session ends as Changed when its file changes.
    #[test]
    fn a_paused_session_ends_when_its_file_changes() {
        let dir = kettle_test_support::private_tempdir("kettle-worker-play-");
        let path = dir.path().join("clip.mp4");
        std::fs::write(&path, b"a video").unwrap();
        let source = Source::user_pull(
            NativePath::from_path(&path).unwrap(),
            GuiActionWitness::from_explicit_gui_action(),
        );
        let held = source::hold(&source, MAX_VIDEO_PREFIX_BYTES).unwrap();
        let opened = opened(10_000, 4, 2);
        let read = run(
            opened,
            &held,
            vec![Step::Send(frame(&opened, 0))],
            |read| {
                // Frame 0 and Paused read, the file changes.
                if read == 2 {
                    use std::io::Write as _;
                    let mut file = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&path)
                        .unwrap();
                    file.write_all(b" changed").unwrap();
                }
            },
            // Otherwise closed in a while, so the test ends either way.
            |parent| {
                std::thread::sleep(SECOND);
                parent.ask(PlayCommand::Close);
            },
        );
        assert_eq!(shown(&read).len(), 1);
        assert_eq!(
            said(&read),
            [
                &Frame::PlayState(PlayState::Paused),
                &Frame::Failure(kettle_media::Failure {
                    code: FailureCode::Changed
                })
            ]
        );
    }

    /// A session still waiting for its first frame ends as Changed when its
    /// file changes.
    #[test]
    fn an_opening_session_ends_when_its_file_changes() {
        let dir = kettle_test_support::private_tempdir("kettle-worker-play-");
        let path = dir.path().join("clip.mp4");
        std::fs::write(&path, b"a video").unwrap();
        let source = Source::user_pull(
            NativePath::from_path(&path).unwrap(),
            GuiActionWitness::from_explicit_gui_action(),
        );
        let held = source::hold(&source, MAX_VIDEO_PREFIX_BYTES).unwrap();
        {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(b" changed").unwrap();
        }
        let read = run(
            opened(10_000, 4, 2),
            &held,
            Vec::new(),
            |_| {},
            // Otherwise closed in a while, so the test ends either way.
            |parent| {
                std::thread::sleep(SECOND);
                parent.ask(PlayCommand::Close);
            },
        );
        assert_eq!(
            said(&read),
            [&Frame::Failure(kettle_media::Failure {
                code: FailureCode::Changed
            })]
        );
    }

    /// A file changed while the last frame's write is held up: the session
    /// ends as Changed, not Ended.
    #[test]
    fn a_change_during_the_last_frame_is_not_an_end() {
        let dir = kettle_test_support::private_tempdir("kettle-worker-play-");
        let path = dir.path().join("clip.mp4");
        std::fs::write(&path, b"a video").unwrap();
        let source = Source::user_pull(
            NativePath::from_path(&path).unwrap(),
            GuiActionWitness::from_explicit_gui_action(),
        );
        let held = source::hold(&source, MAX_VIDEO_PREFIX_BYTES).unwrap();
        // Frames far larger than a pipe holds, a tenth of a second apart.
        let opened = opened(10_000, 512, 512);
        let read = run(
            opened,
            &held,
            vec![
                Step::Ask(PlayCommand::Play),
                Step::Send(frame(&opened, 0)),
                Step::Send(frame(&opened, 100)),
                Step::Send(Event::End),
            ],
            |read| {
                // Frame 0 and Playing read, the worker comes to write the
                // last frame and waits on the pipe while the file changes.
                if read == 2 {
                    std::thread::sleep(Duration::from_millis(400));
                    use std::io::Write as _;
                    let mut file = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&path)
                        .unwrap();
                    file.write_all(b" changed").unwrap();
                }
            },
            |_| {},
        );
        assert_eq!(shown(&read).len(), 2);
        assert_eq!(
            said(&read),
            [
                &Frame::PlayState(PlayState::Playing),
                &Frame::Failure(kettle_media::Failure {
                    code: FailureCode::Changed
                })
            ]
        );
    }

    /// A decoder that fails just after the file changes, before the loop
    /// looks at the file again, fails the session as Changed.
    #[test]
    fn a_decoder_failure_on_a_changed_file_is_changed() {
        use std::sync::atomic::AtomicBool;
        let dir = kettle_test_support::private_tempdir("kettle-worker-play-");
        let path = dir.path().join("clip.mp4");
        std::fs::write(&path, b"a video").unwrap();
        let source = Source::user_pull(
            NativePath::from_path(&path).unwrap(),
            GuiActionWitness::from_explicit_gui_action(),
        );
        let held = source::hold(&source, MAX_VIDEO_PREFIX_BYTES).unwrap();
        let opened = opened(10_000, 4, 2);
        let changed = AtomicBool::new(false);
        let read = run(
            opened,
            &held,
            vec![Step::Send(frame(&opened, 0))],
            |read| {
                // Frame 0 and Paused read, the file changes.
                if read == 2 {
                    use std::io::Write as _;
                    let mut file = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&path)
                        .unwrap();
                    file.write_all(b" changed").unwrap();
                    changed.store(true, Ordering::SeqCst);
                }
            },
            // The decoder fails at once, well inside the loop's tick.
            |parent| {
                while !changed.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                parent.send(Event::Failed(FailureCode::RenderParse));
            },
        );
        assert_eq!(
            said(&read),
            [
                &Frame::PlayState(PlayState::Paused),
                &Frame::Failure(kettle_media::Failure {
                    code: FailureCode::Changed
                })
            ]
        );
    }

    /// The latest Play or Pause is the one taken, once; every command moves
    /// the lease; the first break is the one kept, and leaving closes.
    #[test]
    fn what_the_parent_asked_is_its_latest_wish() {
        let asked = Asked::new();
        let started = asked.take().last;
        asked.hear(PlayCommand::Play);
        asked.hear(PlayCommand::Pause);
        std::thread::sleep(Duration::from_millis(5));
        asked.hear(PlayCommand::Heartbeat);
        let heard = asked.take();
        assert_eq!(heard.play, Some(false));
        assert!(heard.last > started);
        assert!(!heard.closed && heard.broken.is_none());
        assert_eq!(asked.take().play, None, "taken once");
        asked.hear(PlayCommand::Pause);
        asked.hear(PlayCommand::Play);
        assert_eq!(asked.take().play, Some(true));
        // Play made again keeps when it was first made; a Pause between
        // starts it over.
        asked.hear(PlayCommand::Play);
        let first = asked.lock().play_at;
        std::thread::sleep(Duration::from_millis(5));
        asked.hear(PlayCommand::Play);
        assert_eq!(asked.take().play_at, first);
        asked.hear(PlayCommand::Play);
        let played = asked.lock().play_at;
        std::thread::sleep(Duration::from_millis(5));
        asked.hear(PlayCommand::Pause);
        asked.hear(PlayCommand::Play);
        assert!(asked.take().play_at > played);
        asked.break_off(Some(WireError::Truncated));
        asked.break_off(None);
        assert_eq!(asked.take().broken, Some(Some(WireError::Truncated)));
        asked.leave();
        assert!(asked.take().closed);
        let closing = Asked::new();
        closing.hear(PlayCommand::Close);
        assert!(closing.take().closed);
    }

    /// The decoder's gate follows each Play and Pause as the reader hears
    /// it, and the sound each Play, Pause, Mute and Unmute, with no session
    /// thread coming round at all: a Pause stops both at once, a Pause then
    /// a Play leaves them playing, and a Mute is heard at once.
    #[test]
    fn the_gate_and_the_sound_follow_the_latest_command() {
        let (input, mut parent) = std::io::pipe().unwrap();
        let gate = Arc::new(PlayGate::default());
        let sound = SoundWish::new(false);
        let (events, _received) = mpsc::sync_channel(EVENTS);
        start_reader(
            input,
            Arc::new(Asked::new()),
            Arc::clone(&gate),
            sound.clone(),
            events,
            Watchdog::unwatched(),
        )
        .unwrap();
        let mut send = |commands: &[PlayCommand], playing: bool, muted: bool| {
            for &command in commands {
                write_frame(
                    &mut parent,
                    &Frame::PlayCommand(command),
                    Direction::ParentToWorker,
                )
                .unwrap();
            }
            let deadline = Instant::now() + SECOND;
            while (gate.is_playing(), sound.playing(), sound.muted()) != (playing, playing, muted) {
                assert!(
                    Instant::now() < deadline,
                    "after {commands:?}, playing should be {playing} and muted {muted}"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        send(&[PlayCommand::Play], true, false);
        send(&[PlayCommand::Mute], true, true);
        send(&[PlayCommand::Pause], false, true);
        send(&[PlayCommand::Pause, PlayCommand::Play], true, true);
        send(
            &[PlayCommand::Unmute, PlayCommand::Mute, PlayCommand::Unmute],
            true,
            false,
        );
        send(&[PlayCommand::Play, PlayCommand::Pause], false, false);
        // Left open: the reader ends the process once its parent leaves.
        std::mem::forget(parent);
    }

    /// Close and the parent leaving both end the sound before the reader's
    /// grace runs out and the process exits, so its output is disposed even
    /// while the session's thread is held: Close through the same `hear` as
    /// every command (see `close_ends_the_sound_and_lets_its_speaker_go`),
    /// the parent leaving by closing it.
    #[test]
    fn leaving_ends_the_sound_before_the_process_exits() {
        let source = kettle_test_support::production_source(include_str!("play.rs"));
        let reader = source
            .split("fn start_reader(")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("start_reader");
        let heard = reader
            .find("sound.hear(command);")
            .expect("commands reach the sound");
        assert!(heard < reader.find("PlayCommand::Close => {").unwrap());
        let left = reader
            .find("asked.leave();\n                        gate.end();\n                        sound.close();")
            .expect("the parent leaving ends the sound");
        assert!(left < reader.find("std::process::exit(0);").unwrap());
    }

    /// A frame goes out `VIDEO_LEAD` before its sound is heard: held while
    /// early, out within a frame of it, dropped once the sound passed it by
    /// more, and held while the sound has no clock.
    #[test]
    fn frames_follow_the_sound_s_clock() {
        let now = Instant::now();
        let ms = Duration::from_millis;
        let frame = ms(100);
        let heard = |frame: f64| Some(Heard { frame, at: now });
        assert_eq!(due_by_sound(None, 0, frame, now), Due::Wait);
        // Heard at 1 s: the frame at 1.5 s goes out 460 ms from now.
        assert_eq!(
            due_by_sound(heard(48_000.0), 1_500, frame, now),
            Due::At(now + ms(460))
        );
        assert_eq!(due_by_sound(heard(48_000.0), 1_040, frame, now), Due::Now);
        assert_eq!(due_by_sound(heard(48_000.0), 1_000, frame, now), Due::Now);
        // Passed by a frame is still on time; by more, late.
        assert_eq!(due_by_sound(heard(48_000.0), 900, frame, now), Due::Now);
        assert_eq!(due_by_sound(heard(48_000.0), 850, frame, now), Due::Late);
        // Lateness counts from the sound, not from the lead before it: at
        // thirty a second, a frame whose sound is heard now goes out now,
        // and one passed by less than its third-of-a-millisecond-long frame
        // is not late either.
        let thirtieth = frame_length(30_000);
        assert_eq!(thirtieth, Duration::from_micros(33_333));
        assert_eq!(frame_length(1), Duration::from_secs(1_000));
        assert_eq!(
            due_by_sound(heard(49_590.0), 1_000, thirtieth, now),
            Due::Now
        );
        assert_eq!(
            due_by_sound(heard(48_000.0), 1_000, thirtieth, now),
            Due::Now
        );
        assert_eq!(
            due_by_sound(heard(48_000.0), 960, thirtieth, now),
            Due::Late
        );
    }

    /// A session with sound waits for its sound's clock: each frame goes
    /// out as its sound comes due, one the sound passed is dropped, Mute
    /// and Play reach the speaker, and the end waits for the sound's own.
    #[test]
    fn a_sounding_session_follows_its_sound() {
        use crate::sound::testing::{Blocks, stand};
        use std::collections::VecDeque;
        let (speaker, told) = stand(3);
        // Two seconds of sound from the start.
        let source = Box::new(Blocks(VecDeque::from([(0.5, 96_000, 0)])));
        let sound = Sound::start(speaker, source, 0, SoundWish::new(false)).unwrap();
        let opened = opened(10_000, 4, 2);
        let clock = Arc::clone(&told);
        let all_heard = Arc::new(std::sync::Mutex::new(None));
        let heard_at = Arc::clone(&all_heard);
        let read = run_with(
            opened,
            &Held::Bytes(&[]),
            Some(sound),
            vec![
                Step::Ask(PlayCommand::Play),
                Step::Ask(PlayCommand::Mute),
                Step::Send(frame(&opened, 0)),
                Step::Send(frame(&opened, 100)),
                Step::Send(frame(&opened, 200)),
                Step::Send(frame(&opened, 300)),
                Step::Send(Event::End),
            ],
            |_| {},
            move |_| {
                // Each stage long enough that the session cannot miss it.
                let set = |frames: f64| {
                    std::thread::sleep(Duration::from_millis(400));
                    clock.lock().unwrap().played = Some(frames);
                };
                // 62.5 ms heard: the frame at 100 ms is due.
                set(3_000.0);
                // 350 ms: the frame at 200 ms is passed, at 300 ms due.
                set(16_800.0);
                // All of the sound heard: the session ends. Timed before it
                // is told, so the End can only come after.
                std::thread::sleep(Duration::from_millis(400));
                *heard_at.lock().unwrap() = Some(Instant::now());
                clock.lock().unwrap().played = Some(96_000.0);
            },
        );
        let ended = read
            .iter()
            .find(|(_, frame)| *frame == Frame::PlayState(PlayState::Ended))
            .map(|(at, _)| *at)
            .unwrap();
        let all_heard = all_heard.lock().unwrap().unwrap();
        assert!(
            ended >= all_heard,
            "ended before all of its sound was heard"
        );
        assert_eq!(
            shown(&read).iter().map(|(_, pts)| *pts).collect::<Vec<_>>(),
            [0, 100, 300]
        );
        assert_eq!(
            said(&read),
            [
                &Frame::PlayState(PlayState::Playing),
                &Frame::PlayState(PlayState::Ended)
            ]
        );
        let told = told.lock().unwrap();
        assert_eq!(told.started, 1);
        assert_eq!(told.muted, [false, true]);
    }

    /// A frame the sound has passed is dropped only for a newer one waiting
    /// behind it: a late last frame, its End not yet taken, still goes out,
    /// so the lane holds the video's own last frame.
    #[test]
    fn a_late_last_frame_is_never_dropped() {
        use crate::sound::testing::{Blocks, stand};
        use std::collections::VecDeque;
        let (speaker, told) = stand(3);
        // Two seconds of sound, all of it heard at once.
        told.lock().unwrap().played = Some(96_000.0);
        let source = Box::new(Blocks(VecDeque::from([(0.5, 96_000, 0)])));
        let sound = Sound::start(speaker, source, 0, SoundWish::new(false)).unwrap();
        let opened = opened(10_000, 4, 2);
        let read = run_with(
            opened,
            &Held::Bytes(&[]),
            Some(sound),
            vec![
                Step::Ask(PlayCommand::Play),
                Step::Send(frame(&opened, 0)),
                Step::Send(frame(&opened, 100)),
            ],
            |_| {},
            // The end comes once the last frame was looked at.
            |parent| {
                std::thread::sleep(Duration::from_millis(200));
                parent.send(Event::End);
            },
        );
        assert_eq!(
            shown(&read).iter().map(|(_, pts)| *pts).collect::<Vec<_>>(),
            [0, 100]
        );
        assert_eq!(
            said(&read).last(),
            Some(&&Frame::PlayState(PlayState::Ended))
        );
    }

    /// A speaker that fails leaves the session silent: the parent is told,
    /// and its frames go out on the session's own clock to the end.
    #[test]
    fn a_session_whose_speaker_fails_plays_on_silent() {
        use crate::sound::testing::{Blocks, stand};
        use std::collections::VecDeque;
        let (speaker, told) = stand(3);
        told.lock().unwrap().refuse = true;
        let source = Box::new(Blocks(VecDeque::from([(0.5, 9_600, 0)])));
        let sound = Sound::start(speaker, source, 0, SoundWish::new(false)).unwrap();
        let opened = opened(10_000, 4, 2);
        let read = run_with(
            opened,
            &Held::Bytes(&[]),
            Some(sound),
            vec![
                Step::Ask(PlayCommand::Play),
                Step::Send(frame(&opened, 0)),
                Step::Send(frame(&opened, 100)),
                Step::Send(Event::End),
            ],
            |_| {},
            |_| {},
        );
        assert_eq!(
            shown(&read).iter().map(|(_, pts)| *pts).collect::<Vec<_>>(),
            [0, 100]
        );
        let said = said(&read);
        assert!(
            said.contains(&&Frame::AudioState(AudioStatus::Unavailable)),
            "{said:?}"
        );
        assert_eq!(said.last(), Some(&&Frame::PlayState(PlayState::Ended)));
    }

    /// Counts the frames it is asked for, giving each at once.
    struct Counted(Arc<AtomicUsize>);

    impl FrameSource for Counted {
        fn next_frame(
            &mut self,
            _rgba: &mut [u8],
            _gate: &PlayGate,
        ) -> Result<Option<u64>, FailureCode> {
            Ok(Some(self.0.fetch_add(1, Ordering::SeqCst) as u64 * 100))
        }
    }

    /// The opening's allowance ends with the first frame: by the time
    /// anything after it is written, the watchdog holds the session's
    /// thread to `HANG`, a state the parent will not read included.
    #[test]
    fn the_first_frame_ends_the_opening_s_allowance() {
        /// Notes how long the watchdog has left when the first write past
        /// the first frame begins.
        struct Watching {
            watchdog: Watchdog,
            first: usize,
            written: usize,
            left: Option<Duration>,
        }
        impl Write for Watching {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.written >= self.first {
                    self.left.get_or_insert(self.watchdog.left());
                }
                self.written += bytes.len();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let opened = opened(10_000, 4, 2);
        let first = kettle_media::wire::encode(
            &Frame::VideoFrame(kettle_media::VideoFrame {
                seq: 0,
                pts_ms: 0,
                width: 4,
                height: 2,
                rgba: vec![0; 32],
            }),
            Direction::WorkerToParent,
        )
        .unwrap()
        .len();
        let watchdog = Watchdog::unwatched();
        let mut output = Watching {
            watchdog: watchdog.clone(),
            first,
            written: 0,
            left: None,
        };
        let asked = Asked::new();
        let (events, received) = mpsc::sync_channel(EVENTS);
        events.send(frame(&opened, 0)).unwrap();
        events.send(Event::End).unwrap();
        let (free, _) = mpsc::sync_channel(BUFFERS);
        let session = Session {
            opened,
            held: &Held::Bytes(&[]),
            asked: &asked,
            sound: None,
            free,
            output: &mut output,
            watchdog: &watchdog,
        };
        assert_eq!(session.run(&received, Instant::now() + OPEN_DEADLINE), 0);
        let left = output.left.expect("written past the first frame");
        assert!(left > HANG - SECOND, "{left:?} left");
    }

    /// A session closed while its decoder opened takes not even its first
    /// frame: the decoder ends at once.
    #[test]
    fn a_closed_session_s_decoder_takes_no_frame() {
        let asked = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(PlayGate::default());
        gate.end();
        let (free, buffers) = mpsc::sync_channel(BUFFERS);
        let (events, received) = mpsc::sync_channel(EVENTS);
        start_decoder(Box::new(Counted(Arc::clone(&asked))), gate, buffers, events).unwrap();
        free.send(vec![0; 8]).unwrap();
        assert!(matches!(
            received.recv_timeout(SECOND),
            Err(RecvTimeoutError::Disconnected)
        ));
        assert_eq!(asked.load(Ordering::SeqCst), 0);
    }

    /// Past the first frame the decoder takes none while paused, a Pause
    /// that comes while it waits for a buffer included.
    #[test]
    fn a_paused_decoder_takes_no_frame_past_the_first() {
        let asked = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(PlayGate::default());
        let (free, buffers) = mpsc::sync_channel(BUFFERS);
        let (events, received) = mpsc::sync_channel(EVENTS);
        start_decoder(
            Box::new(Counted(Arc::clone(&asked))),
            Arc::clone(&gate),
            buffers,
            events,
        )
        .unwrap();
        free.send(vec![0; 8]).unwrap();
        assert!(matches!(
            received.recv_timeout(SECOND),
            Ok(Event::Frame { pts_ms: 0, .. })
        ));
        // Playing, it waits for a buffer; paused, it is given one.
        gate.play();
        std::thread::sleep(Duration::from_millis(50));
        gate.pause();
        free.send(vec![0; 8]).unwrap();
        assert!(matches!(
            received.recv_timeout(Duration::from_millis(200)),
            Err(RecvTimeoutError::Timeout)
        ));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        gate.play();
        assert!(matches!(
            received.recv_timeout(SECOND),
            Ok(Event::Frame { pts_ms: 100, .. })
        ));
        gate.end();
        drop(free);
    }
}

/// Test fixture only (feature `test-faults`): a held file whose MP4 box is
/// followed by `kettle-media-worker-test-video FRAMES FPS_MILLI WIDTH
/// HEIGHT` plays with a stand-in decoder, frame N filled with N's low byte
/// and timed N frame lengths in; `stall N` makes it give no frame from the
/// Nth on, and `fail N` fail there; `open MS` makes opening take that long,
/// and `refuse` then fail.
#[cfg(feature = "test-faults")]
mod faults {
    use std::time::{Duration, Instant};

    use kettle_media::video::{
        FrameSource, PlayGate, PlayRequest, Playback, VideoInput, VideoPlayer,
    };
    use kettle_media::{FailureCode, Opened, VideoCodec, VideoInfo, play_size};

    const MARK: &[u8] = b"kettle-media-worker-test-video ";
    /// Where the mark sits: after the 24-byte MP4 box that makes the file
    /// sniff as a video.
    const AT: usize = 24;

    pub(super) struct Fake {
        frames: u64,
        fps_milli: u32,
        width: u32,
        height: u32,
        stall: Option<u64>,
        fail: Option<u64>,
        open_ms: Option<u64>,
        refuse: bool,
    }

    pub(super) fn player(prefix: &[u8]) -> Option<Fake> {
        let rest = prefix.get(AT..)?.strip_prefix(MARK)?;
        let line = rest.split(|byte| *byte == b'\n').next()?;
        let words: Vec<&str> = std::str::from_utf8(line).ok()?.split_whitespace().collect();
        let number = |at: usize| words.get(at)?.parse::<u64>().ok();
        let after = |name: &str| {
            let at = words.iter().position(|word| *word == name)?;
            number(at + 1)
        };
        Some(Fake {
            frames: number(0)?,
            fps_milli: u32::try_from(number(1)?).ok()?,
            width: u32::try_from(number(2)?).ok()?,
            height: u32::try_from(number(3)?).ok()?,
            stall: after("stall"),
            fail: after("fail"),
            open_ms: after("open"),
            refuse: words.contains(&"refuse"),
        })
    }

    impl VideoPlayer for Fake {
        fn play(
            &self,
            input: VideoInput<'_>,
            request: PlayRequest,
            _deadline: Instant,
        ) -> Result<Playback, FailureCode> {
            if let Some(open_ms) = self.open_ms {
                std::thread::sleep(Duration::from_millis(open_ms));
            }
            if self.refuse {
                return Err(FailureCode::RenderParse);
            }
            let fps_milli = u64::from(self.fps_milli);
            let info = VideoInfo {
                duration_ms: self.frames * 1_000_000 / fps_milli,
                width: self.width,
                height: self.height,
                rotation: 0,
                codec: VideoCodec::H264,
                fps_milli: Some(self.fps_milli),
                has_audio: false,
                container: Some(input.container),
            };
            let (width, height) = play_size(
                self.width,
                self.height,
                request.max_width,
                request.max_height,
            )
            .ok_or(FailureCode::RenderResource)?;
            let next = request.start_ms * fps_milli / 1_000_000;
            let opened = Opened {
                info,
                width,
                height,
                fps_milli: self.fps_milli,
                start_ms: next * 1_000_000 / fps_milli,
                audio: kettle_media::AudioStatus::silent(request.audio, info.has_audio),
            };
            opened.validate().map_err(|_| FailureCode::BadParams)?;
            Ok(Playback {
                opened,
                frames: Box::new(Frames {
                    next,
                    taken: 0,
                    frames: self.frames,
                    fps_milli,
                    stall: self.stall,
                    fail: self.fail,
                }),
                audio: None,
            })
        }
    }

    struct Frames {
        next: u64,
        taken: u64,
        frames: u64,
        fps_milli: u64,
        stall: Option<u64>,
        fail: Option<u64>,
    }

    impl FrameSource for Frames {
        fn next_frame(
            &mut self,
            rgba: &mut [u8],
            gate: &PlayGate,
        ) -> Result<Option<u64>, FailureCode> {
            if self.fail == Some(self.taken) {
                return Err(FailureCode::RenderParse);
            }
            if self.stall == Some(self.taken) {
                while !gate.has_ended() {
                    std::thread::sleep(Duration::from_millis(20));
                }
                return Ok(None);
            }
            if self.next >= self.frames {
                return Ok(None);
            }
            rgba.fill(self.next as u8);
            let pts = self.next * 1_000_000 / self.fps_milli;
            self.next += 1;
            self.taken += 1;
            Ok(Some(pts))
        }
    }
}
