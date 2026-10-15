//! A playback session in a worker of its own (the worker's `play` serves
//! it): started, fed and watched on threads of its own, so the caller only
//! posts commands and takes the newest frame, and never waits on the worker.
//!
//! The supervising thread starts the worker as a render does, inside the
//! startup deadline, takes Ready, lets the writer send PlayOpen, and waits
//! for Opened; then it watches the worker for the rest of the session,
//! measuring its process group every tick and killing it above the memory
//! limit. The reader takes each frame straight into a buffer of the opened
//! size and keeps only the newest one not yet taken, waking the caller once
//! until it looks, and the session's sound as the worker says it, which
//! only ever goes once open. The writer sends the caller's latest Play or
//! Pause and, apart from it, the latest Mute or Unmute, and a heartbeat
//! each second while the caller keeps looking at the session: one whose
//! caller stops (its event loop stalled) loses its lease, and the worker
//! ends it. Dropping the session closes it: Close, then the worker's exit,
//! or a kill.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::client::{RenderControl, Shared, SpawnedWorker, WorkerExit};
use crate::lifecycle::{self, Budgets, Running, Stopped, Wait};
use crate::wire::{self, Direction, Frame, MAX_READY_FRAME_BYTES, SessionRead};
use crate::{
    AudioStatus, BuildId, FailureCode, HandshakeOutcome, Hello, Opened, PlayCommand, PlayOpen,
    PlayState, Ready, check_ready,
};

/// Most sessions open at once. One being closed no longer counts: its
/// worker is on its way out, or counted as stuck.
pub const MAX_PLAY_SESSIONS: usize = 2;
/// How often the writer sends a heartbeat while the caller keeps the
/// session. The worker ends a session after 5 s without a command.
const HEARTBEAT: Duration = Duration::from_secs(1);
/// How long after the caller last kept the session its heartbeats go on.
const KEPT_FOR: Duration = Duration::from_secs(3);
/// From PlayOpen until Opened: the worker's own 5 s, and a second for the
/// answer to come.
const OPEN_BUDGET: Duration = Duration::from_secs(6);
/// The longest frame of a session other than a video frame: Opened.
const MAX_OTHER_FRAME_BYTES: usize = 256;
/// Buffers the reader keeps for frames to come, beyond the one waiting.
const SPARES: usize = 2;

/// Where a session is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    /// Starting its worker, or opening the video.
    Opening,
    /// Open: what it shows, and whether it plays, is paused or has ended.
    Open { opened: Opened, state: PlayState },
    /// It stopped without ending: why.
    Failed(FailureCode),
}

/// A frame taken from a session: straight RGBA of the size it opened at.
#[derive(Debug, PartialEq, Eq)]
pub struct SessionFrame {
    pub seq: u64,
    pub pts_ms: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// An open playback session. Dropping it closes it.
pub struct PlaySession {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for PlaySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaySession")
            .field("status", &self.inner.lock().status)
            .finish_non_exhaustive()
    }
}

struct Inner {
    #[cfg(target_os = "linux")]
    memory: [Arc<std::sync::atomic::AtomicU64>; 2],
    state: Mutex<State>,
    /// The writer waits on it for commands, closing and heartbeats.
    changed: Condvar,
    /// Cancelled when the session is closed, which cuts every wait short.
    closed: RenderControl,
    /// Whether this session still counts toward [`MAX_PLAY_SESSIONS`].
    counted: AtomicBool,
    shared: Arc<Shared>,
    wake: Box<dyn Fn() + Send + Sync>,
}

struct State {
    #[cfg(target_os = "linux")]
    pcm: Option<(crate::pcm::Feedback, Instant)>,
    #[cfg(target_os = "linux")]
    broker: Option<crate::pcm_client::Control>,
    status: SessionStatus,
    /// The session's sound: Opened's, until the worker says it went.
    audio: Option<AudioStatus>,
    frame: Option<SessionFrame>,
    spares: Vec<Vec<u8>>,
    wishes: Wishes,
    closing: bool,
    kept: Instant,
    /// A wake is out and the caller has not looked since.
    woken: bool,
}

/// What the caller asked that the writer has not sent yet: the latest Play
/// or Pause, and apart from it the latest Mute or Unmute, so neither kind
/// takes the other's place.
#[derive(Default)]
struct Wishes {
    play: Option<PlayCommand>,
    mute: Option<PlayCommand>,
}

impl Wishes {
    fn is_empty(&self) -> bool {
        self.play.is_none() && self.mute.is_none()
    }

    /// Take `command` in place of the last of its kind: Play then Pause is
    /// Pause, and Mute then Unmute is Unmute, whatever came between.
    fn add(&mut self, command: PlayCommand) {
        match command {
            PlayCommand::Play | PlayCommand::Pause => self.play = Some(command),
            PlayCommand::Mute | PlayCommand::Unmute => self.mute = Some(command),
            // Not wishes: the writer sends these itself.
            PlayCommand::Heartbeat | PlayCommand::Close => {}
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Change the state, then wake the caller unless a wake is already out.
    fn publish(&self, change: impl FnOnce(&mut State)) {
        let wake = {
            let mut state = self.lock();
            change(&mut state);
            !std::mem::replace(&mut state.woken, true)
        };
        if wake {
            (self.wake)();
        }
    }

    /// The session's final word, unless it already had one or is closed.
    fn fail(&self, code: FailureCode) {
        self.publish(|state| {
            if !matches!(
                state.status,
                SessionStatus::Failed(_)
                    | SessionStatus::Open {
                        state: PlayState::Ended,
                        ..
                    }
            ) {
                state.status = SessionStatus::Failed(code);
            }
        });
    }

    fn uncount(&self) {
        if self.counted.swap(false, Ordering::AcqRel) {
            self.shared.sessions.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl PlaySession {
    /// Open `open` in a fresh worker, on threads of its own; `wake` is
    /// called (from them) when there is something new to look at, once
    /// until the caller looks. Refused at once when the request is not
    /// valid or [`MAX_PLAY_SESSIONS`] are open.
    pub(crate) fn start(
        shared: &Arc<Shared>,
        build_id: &BuildId,
        budgets: Budgets,
        open: &PlayOpen,
        wake: Box<dyn Fn() + Send + Sync>,
    ) -> Result<Self, FailureCode> {
        let hello = Hello {
            build_id: build_id.clone(),
        };
        let (Ok(hello_bytes), Ok(open_bytes)) = (
            wire::encode(&Frame::Hello(hello.clone()), Direction::ParentToWorker),
            wire::encode(&Frame::PlayOpen(open.clone()), Direction::ParentToWorker),
        ) else {
            return Err(FailureCode::BadParams);
        };
        // Rust 1.99 renames fetch_update to try_update; the MSRV (1.95) predates it.
        #[allow(deprecated)]
        let admitted = shared
            .sessions
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |open| {
                (open < MAX_PLAY_SESSIONS).then_some(open + 1)
            });
        if admitted.is_err() {
            return Err(FailureCode::WorkerUnavailable);
        }
        let inner = Arc::new(Inner {
            #[cfg(target_os = "linux")]
            memory: [Arc::default(), Arc::default()],
            state: Mutex::new(State {
                #[cfg(target_os = "linux")]
                pcm: None,
                #[cfg(target_os = "linux")]
                broker: None,
                status: SessionStatus::Opening,
                audio: None,
                frame: None,
                spares: Vec::new(),
                wishes: Wishes::default(),
                closing: false,
                kept: Instant::now(),
                woken: false,
            }),
            changed: Condvar::new(),
            closed: RenderControl::default(),
            counted: AtomicBool::new(true),
            shared: Arc::clone(shared),
            wake,
        });
        let supervised = Arc::clone(&inner);
        let started = std::thread::Builder::new()
            .name("kettle-media-play".into())
            .spawn(move || {
                supervise(&supervised, &hello, hello_bytes, open_bytes, budgets);
                supervised.uncount();
            });
        if started.is_err() {
            inner.uncount();
            return Err(FailureCode::WorkerUnavailable);
        }
        Ok(Self { inner })
    }

    /// Where the session is.
    pub fn status(&self) -> SessionStatus {
        let mut state = self.inner.lock();
        state.woken = false;
        state.status
    }

    /// The newest frame not taken yet. Frames not taken in time are dropped
    /// for newer ones.
    pub fn take_frame(&self) -> Option<SessionFrame> {
        let mut state = self.inner.lock();
        state.woken = false;
        state.frame.take()
    }

    /// Give a taken frame's buffer back for the frames to come.
    pub fn recycle(&self, rgba: Vec<u8>) {
        let mut state = self.inner.lock();
        if state.spares.len() < SPARES {
            state.spares.push(rgba);
        }
    }

    /// The session's sound once it opened: what Opened said, until the
    /// worker says it went (its output failed); `None` before.
    pub fn audio(&self) -> Option<AudioStatus> {
        self.inner.lock().audio
    }

    pub fn play(&self) {
        self.wish(PlayCommand::Play);
    }

    pub fn pause(&self) {
        self.wish(PlayCommand::Pause);
    }

    /// Silence the session's sound, or let it be heard: it neither plays
    /// nor pauses.
    pub fn set_muted(&self, muted: bool) {
        self.wish(if muted {
            PlayCommand::Mute
        } else {
            PlayCommand::Unmute
        });
    }

    /// Say the caller still looks at the session. Without this for a few
    /// seconds the heartbeats stop, and the worker ends the session.
    pub fn keep(&self) {
        self.inner.lock().kept = Instant::now();
    }

    /// Only the latest of each wish counts (see [`Wishes::add`]).
    fn wish(&self, command: PlayCommand) {
        let mut state = self.inner.lock();
        state.kept = Instant::now();
        state.wishes.add(command);
        drop(state);
        self.inner.changed.notify_all();
    }
}

impl Drop for PlaySession {
    fn drop(&mut self) {
        self.inner.lock().closing = true;
        self.inner.closed.cancel();
        self.inner.changed.notify_all();
        self.inner.uncount();
    }
}

/// What the reader tells the supervising thread.
enum Said {
    Ready(Box<Ready>),
    Opened,
    Ended,
    /// The worker refused or failed the session.
    Refused(FailureCode),
    /// The worker closed its output.
    Closed,
    /// A frame out of place, or one that could not be read.
    Broken,
}

/// Start the worker, open the session, and watch it until it ends.
fn supervise(
    inner: &Arc<Inner>,
    hello: &Hello,
    hello_bytes: Vec<u8>,
    open_bytes: Vec<u8>,
    budgets: Budgets,
) {
    let shared = Arc::clone(&inner.shared);
    let ready_until = Instant::now() + budgets.ready;
    let SpawnedWorker {
        process,
        stdin,
        stdout,
    } = match lifecycle::spawn_within(&shared, ready_until, budgets, Some(&inner.closed)) {
        Ok(spawned) => spawned,
        Err(code) => return inner.fail(code),
    };
    let mut worker = Running::new(process, &shared, budgets);
    #[cfg(target_os = "linux")]
    {
        worker = worker.with_memory(inner.memory.clone());
    }
    let (said, heard) = mpsc::channel();
    let (go, opening) = mpsc::channel();
    if start_reader(stdout, said, Arc::clone(inner), hello.clone(), budgets).is_err()
        || start_writer(Arc::clone(inner), stdin, hello_bytes, open_bytes, opening).is_err()
    {
        worker.kill();
        return inner.fail(FailureCode::WorkerUnavailable);
    }
    match worker.next(&heard, ready_until, Some(&inner.closed)) {
        Ok(Said::Ready(ready)) if check_ready(hello, &ready) == HandshakeOutcome::Compatible => {}
        Ok(Said::Ready(_)) => {
            worker.kill();
            return inner.fail(FailureCode::RestartRequired);
        }
        Ok(Said::Refused(code)) => {
            return inner.fail(lifecycle::startup_refusal_end(code, worker.stop()));
        }
        other => return end(inner, &mut worker, other),
    }
    if go.send(()).is_err() {
        return inner.fail(worker.stop_failure());
    }
    match worker.next(&heard, Instant::now() + OPEN_BUDGET, Some(&inner.closed)) {
        Ok(Said::Opened) => {}
        other => return end(inner, &mut worker, other),
    }
    loop {
        let event = worker.next(
            &heard,
            Instant::now() + Duration::from_secs(3600),
            Some(&inner.closed),
        );
        match event {
            Err(Wait::Timeout) if worker.own_exit().is_none() => {}
            other => return end(inner, &mut worker, other),
        }
    }
}

/// End the session on `event`, the worker stopped and what it said kept.
fn end(inner: &Inner, worker: &mut Running<'_>, event: Result<Said, Wait>) {
    match event {
        // Closed: Close is on its way; the worker gets the cleanup budget.
        Err(Wait::Interrupted) => {
            worker.stop();
        }
        Ok(Said::Ended) => match worker.stop() {
            Stopped::Exited(WorkerExit::Code(0)) => inner.publish(|state| {
                if let SessionStatus::Open { state, .. } = &mut state.status {
                    *state = PlayState::Ended;
                }
            }),
            Stopped::Exited(exit) => inner.fail(lifecycle::exit_failure(exit)),
            Stopped::OverFootprint => inner.fail(FailureCode::RenderResource),
            Stopped::Killed | Stopped::Stuck => inner.fail(FailureCode::WorkerUnavailable),
        },
        Ok(Said::Refused(code)) => inner.fail(lifecycle::refusal_end(code, worker.stop())),
        Err(Wait::OverFootprint) => {
            worker.kill();
            inner.fail(FailureCode::RenderResource);
        }
        Err(Wait::Unmeasured) => {
            worker.kill();
            inner.fail(FailureCode::WorkerUnavailable);
        }
        Err(Wait::Timeout) => {
            let code = match worker.own_exit() {
                Some(exit) => lifecycle::exit_failure(exit),
                None => {
                    worker.kill();
                    FailureCode::RenderTimeout
                }
            };
            inner.fail(code);
        }
        // Output closed before the session ended, even with exit 0, leaves
        // it failed: what the worker would have said never came.
        Ok(Said::Closed | Said::Ready(_) | Said::Opened | Said::Broken) | Err(Wait::ReaderGone) => {
            inner.fail(worker.stop_failure());
        }
    }
}

fn opening_frame(stdout: &mut Box<dyn Read + Send>) -> Result<Frame, Said> {
    match wire::read_frame_within(stdout, Direction::WorkerToParent, MAX_OTHER_FRAME_BYTES) {
        Ok(Some(Frame::Failure(failure))) => Err(refusal(stdout, failure.code)),
        Ok(Some(frame)) => Ok(frame),
        Ok(None) => Err(Said::Closed),
        Err(_) => Err(Said::Broken),
    }
}

/// Read Ready, then Opened, then the session's frames.
fn start_reader(
    mut stdout: Box<dyn Read + Send>,
    said: Sender<Said>,
    inner: Arc<Inner>,
    _hello: Hello,
    _budgets: Budgets,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("kettle-media-play-read".into())
        .spawn(move || {
            let first = match wire::read_frame_within(
                &mut stdout,
                Direction::WorkerToParent,
                MAX_READY_FRAME_BYTES,
            ) {
                Ok(Some(Frame::Ready(ready))) => Said::Ready(Box::new(ready)),
                Ok(Some(Frame::Failure(failure))) => refusal(&mut stdout, failure.code),
                Ok(None) => Said::Closed,
                _ => Said::Broken,
            };
            let ready = matches!(first, Said::Ready(_));
            if said.send(first).is_err() || !ready {
                return;
            }
            #[cfg(target_os = "linux")]
            let mut broker = None;
            #[cfg(target_os = "linux")]
            let opening = loop {
                match opening_frame(&mut stdout) {
                    Ok(Frame::Opened(opened)) => break Ok(opened),
                    #[cfg(target_os = "linux")]
                    Ok(Frame::Pcm(crate::pcm::Message::Open(open))) if broker.is_none() => {
                        let target = Arc::downgrade(&inner);
                        let feedback = Box::new(move |feedback| {
                            if let Some(inner) = target.upgrade() {
                                inner.lock().pcm = Some((feedback, Instant::now()));
                                inner.changed.notify_all();
                            }
                        });
                        match crate::pcm_client::Broker::start(
                            Arc::clone(&inner.shared),
                            _hello.clone(),
                            _budgets,
                            open,
                            [Arc::clone(&inner.memory[1]), Arc::clone(&inner.memory[0])],
                            feedback,
                        ) {
                            Ok(started) => {
                                inner.lock().broker = Some(started.0.clone());
                                broker = Some(started);
                                continue;
                            }
                            Err(_) => break Err(Said::Broken),
                        }
                    }
                    Err(error) => break Err(error),
                    _ => break Err(Said::Broken),
                }
            };
            #[cfg(not(target_os = "linux"))]
            let opening = match opening_frame(&mut stdout) {
                Ok(Frame::Opened(opened)) => Ok(opened),
                Err(error) => Err(error),
                _ => Err(Said::Broken),
            };
            let opened = match opening {
                Ok(opened) => opened,
                Err(error) => {
                    let _ = said.send(error);
                    return;
                }
            };
            inner.publish(|state| {
                // Never over the failure the supervisor gave the session
                // while this thread was taking Opened.
                if state.status == SessionStatus::Opening {
                    state.status = SessionStatus::Open {
                        opened,
                        state: PlayState::Paused,
                    };
                    state.audio = Some(opened.audio);
                }
            });
            if said.send(Said::Opened).is_err() {
                return;
            }
            let mut rgba = Vec::new();
            loop {
                if rgba.capacity() == 0 {
                    rgba = inner.lock().spares.pop().unwrap_or_default();
                }
                let read = wire::read_session_frame(
                    &mut stdout,
                    opened.width,
                    opened.height,
                    &mut rgba,
                    if cfg!(target_os = "linux") {
                        session_frame_limit()
                    } else {
                        MAX_OTHER_FRAME_BYTES
                    },
                );
                let next = match read {
                    Ok(Some(SessionRead::Pixels { seq, pts_ms })) => {
                        let frame = SessionFrame {
                            seq,
                            pts_ms,
                            width: opened.width,
                            height: opened.height,
                            rgba: std::mem::take(&mut rgba),
                        };
                        inner.publish(|state| {
                            if let Some(old) = state.frame.replace(frame)
                                && state.spares.len() < SPARES
                            {
                                state.spares.push(old.rgba);
                            }
                        });
                        continue;
                    }
                    Ok(Some(SessionRead::Frame(frame))) => match *frame {
                        #[cfg(target_os = "linux")]
                        Frame::Pcm(message) => {
                            if broker
                                .as_ref()
                                .is_some_and(|broker| broker.0.offer(message))
                            {
                                continue;
                            }
                            Said::Broken
                        }
                        Frame::PlayState(PlayState::Ended) => Said::Ended,
                        Frame::PlayState(now) => {
                            inner.publish(|state| {
                                if let SessionStatus::Open { state: was, .. } = &mut state.status {
                                    *was = now;
                                }
                            });
                            continue;
                        }
                        // Sound only ever goes: a session without it never
                        // gains it.
                        Frame::AudioState(now)
                            if now != AudioStatus::Ready
                                && inner.lock().audio == Some(AudioStatus::Ready) =>
                        {
                            inner.publish(|state| state.audio = Some(now));
                            continue;
                        }
                        Frame::Failure(failure) => refusal(&mut stdout, failure.code),
                        _ => Said::Broken,
                    },
                    Ok(None) => Said::Closed,
                    _ => Said::Broken,
                };
                let _ = said.send(next);
                return;
            }
        })
        .map(drop)
}

/// A refusal the worker said: kept only with nothing after it, as a
/// render's is; anything more makes the stream broken.
fn refusal(stdout: &mut dyn Read, code: FailureCode) -> Said {
    if lifecycle::at_end(stdout) {
        Said::Refused(code)
    } else {
        Said::Broken
    }
}

fn session_frame_limit() -> usize {
    #[cfg(target_os = "linux")]
    {
        crate::pcm_client::FRAME_LIMIT
    }
    #[cfg(not(target_os = "linux"))]
    {
        MAX_OTHER_FRAME_BYTES
    }
}

fn pcm_waiting(state: &State) -> bool {
    #[cfg(target_os = "linux")]
    {
        state.pcm.is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = state;
        false
    }
}

/// Write Hello, PlayOpen once Ready has matched, then the caller's commands
/// and heartbeats, and Close when the session is closed. The platform first
/// guards this thread's pipe writes.
fn start_writer(
    inner: Arc<Inner>,
    mut stdin: Box<dyn Write + Send>,
    hello: Vec<u8>,
    open: Vec<u8>,
    opening: Receiver<()>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("kettle-media-play-write".into())
        .spawn(move || {
            if inner.shared.platform().guard_pipe_writes().is_err() {
                return;
            }
            let mut send = |bytes: &[u8]| stdin.write_all(bytes).and_then(|()| stdin.flush());
            if send(&hello).is_err() || opening.recv().is_err() || send(&open).is_err() {
                return;
            }
            let encode = |command| {
                wire::encode(&Frame::PlayCommand(command), Direction::ParentToWorker)
                    .unwrap_or_default()
            };
            let mut beat = Instant::now() + HEARTBEAT;
            #[cfg(target_os = "linux")]
            let mut pcm_commands = crate::pcm_client::Commands::default();
            loop {
                let (wishes, closing, kept) = {
                    let mut state = inner.lock();
                    while state.wishes.is_empty()
                        && !state.closing
                        && !pcm_waiting(&state)
                        && Instant::now() < beat
                    {
                        let left = beat.saturating_duration_since(Instant::now());
                        state = inner
                            .changed
                            .wait_timeout(state, left)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0;
                    }
                    (std::mem::take(&mut state.wishes), state.closing, state.kept)
                };
                #[cfg(target_os = "linux")]
                {
                    let (feedback, broker) = {
                        let mut state = inner.lock();
                        (state.pcm.take(), state.broker.clone())
                    };
                    if let Some((feedback, received)) = feedback {
                        let frame = Frame::Pcm(crate::pcm_client::aged_feedback(
                            feedback,
                            received.elapsed(),
                        ));
                        if !wire::encode(&frame, Direction::ParentToWorker)
                            .is_ok_and(|bytes| send(&bytes).is_ok())
                        {
                            return;
                        }
                    }
                    let now = Instant::now();
                    pcm_commands.forward(
                        broker.as_ref(),
                        wishes.play,
                        wishes.mute,
                        now >= beat && now.duration_since(kept) < KEPT_FOR,
                        closing,
                    );
                }
                if closing {
                    let _ = send(&encode(PlayCommand::Close));
                    return;
                }
                for command in wishes.play.into_iter().chain(wishes.mute) {
                    if send(&encode(command)).is_err() {
                        return;
                    }
                }
                let now = Instant::now();
                if now >= beat {
                    if now.duration_since(kept) < KEPT_FOR
                        && send(&encode(PlayCommand::Heartbeat)).is_err()
                    {
                        return;
                    }
                    beat = now + HEARTBEAT;
                }
            }
        })
        .map(drop)
}

#[cfg(test)]
mod tests {
    use std::io::{PipeReader, PipeWriter};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::client::{
        FileIdentity, UnavailableCause, WorkerClient, WorkerPlatform, WorkerProcess,
    };
    use crate::wire::{read_frame, write_frame};
    use crate::{
        AudioRequest, AudioStatus, BuildId, Failure, GuiActionWitness, NativePath, Ready, Source,
        VideoCodec, VideoFrame, VideoInfo,
    };

    fn build_id() -> BuildId {
        BuildId::from_embedded("1.0.0", &"a".repeat(64)).unwrap()
    }

    /// How the fake worker behaves.
    #[derive(Clone, Copy, Debug)]
    struct Behavior {
        /// Frames it plays after the first.
        frames: u64,
        interleaves_states: bool,
        /// Between frames while playing.
        every: Duration,
        /// Refuses Hello with this, in place of Ready.
        refuses_hello: Option<FailureCode>,
        /// Writes a byte after its refusal.
        trails: bool,
        /// Refuses the session with this, in place of Opened.
        refuses: Option<FailureCode>,
        /// Sends frames of the wrong size.
        misframes: bool,
        /// Never answers Hello.
        silent: bool,
        /// Closes its output and exits 0 in place of Ready.
        leaves_at_hello: bool,
        /// Closes its output and exits 0 once open and paused, never Ended.
        leaves_open: bool,
        /// Waits this long after PlayOpen before it says Opened.
        opens_after: Duration,
        /// Its group's memory.
        footprint: u64,
        /// Dies by a signal after this many frames.
        dies_after: Option<u64>,
        /// How it exits after a refusal.
        refused_exit: WorkerExit,
        ended_exit: WorkerExit,
        /// Opens a video whose sound plays.
        sounds: bool,
        /// Says its sound is this as it starts playing.
        says_sound: Option<AudioStatus>,
    }

    const PLAYS: Behavior = Behavior {
        frames: 5,
        interleaves_states: false,
        every: Duration::from_millis(10),
        refuses_hello: None,
        trails: false,
        refuses: None,
        misframes: false,
        silent: false,
        leaves_at_hello: false,
        leaves_open: false,
        opens_after: Duration::ZERO,
        footprint: 1 << 20,
        dies_after: None,
        refused_exit: WorkerExit::Code(0),
        ended_exit: WorkerExit::Code(0),
        sounds: false,
        says_sound: None,
    };

    /// What the fake worker saw, for the test to look at.
    #[derive(Default)]
    struct Seen {
        commands: Mutex<Vec<PlayCommand>>,
        exited: AtomicBool,
        killed: AtomicBool,
        reaped: AtomicUsize,
    }

    struct Fake {
        behavior: Behavior,
        seen: Arc<Seen>,
    }

    struct FakeProcess {
        seen: Arc<Seen>,
        exit: Arc<Mutex<Option<WorkerExit>>>,
        footprint: u64,
    }

    impl WorkerProcess for FakeProcess {
        fn try_wait(&mut self) -> std::io::Result<Option<WorkerExit>> {
            let exit = *self.exit.lock().unwrap();
            if exit.is_some() {
                self.seen.reaped.fetch_add(1, Ordering::SeqCst);
            }
            Ok(exit)
        }
        fn kill(&mut self) {
            self.seen.killed.store(true, Ordering::SeqCst);
            self.exit
                .lock()
                .unwrap()
                .get_or_insert(WorkerExit::Signal(9));
        }
        fn footprint(&mut self) -> std::io::Result<u64> {
            Ok(self.footprint)
        }
    }

    impl WorkerPlatform for Fake {
        fn worker_path(&self) -> Result<&Path, UnavailableCause> {
            Ok(Path::new("/install/kettle-media-worker"))
        }
        fn inspect(&self, _: &Path) -> Result<FileIdentity, UnavailableCause> {
            Ok(FileIdentity {
                dev: 1,
                ino: 2,
                size: 3,
                mtime_seconds: 4,
                mtime_nanos: 5,
                ctime_seconds: 6,
                ctime_nanos: 7,
            })
        }
        fn verify(&self, _: &Path) -> Result<(), UnavailableCause> {
            Ok(())
        }
        fn guard_pipe_writes(&self) -> std::io::Result<()> {
            Ok(())
        }
        fn spawn(&self, _: &Path) -> std::io::Result<SpawnedWorker> {
            let (stdin_reader, stdin_writer) = std::io::pipe()?;
            let (stdout_reader, stdout_writer) = std::io::pipe()?;
            let exit = Arc::new(Mutex::new(None));
            let behavior = self.behavior;
            let seen = Arc::clone(&self.seen);
            let exited = Arc::clone(&exit);
            std::thread::spawn(move || {
                let code = serve(behavior, stdin_reader, stdout_writer, &seen);
                seen.exited.store(true, Ordering::SeqCst);
                exited.lock().unwrap().get_or_insert(code);
            });
            Ok(SpawnedWorker {
                process: Box::new(FakeProcess {
                    seen: Arc::clone(&self.seen),
                    exit,
                    footprint: self.behavior.footprint,
                }),
                stdin: Box::new(stdin_writer),
                stdout: Box::new(stdout_reader),
            })
        }
    }

    const OPENED: Opened = Opened {
        info: VideoInfo {
            duration_ms: 1000,
            width: 4,
            height: 2,
            rotation: 0,
            codec: VideoCodec::H264,
            fps_milli: Some(10_000),
            has_audio: false,
            container: None,
        },
        audio: AudioStatus::NoTrack,
        width: 4,
        height: 2,
        fps_milli: 10_000,
        start_ms: 0,
    };

    /// [`OPENED`], of a video whose sound plays.
    const SOUNDING: Opened = Opened {
        info: VideoInfo {
            has_audio: true,
            ..OPENED.info
        },
        audio: AudioStatus::Ready,
        ..OPENED
    };

    fn frame(seq: u64, misframed: bool) -> Frame {
        let (width, height) = if misframed { (2, 2) } else { (4, 2) };
        Frame::VideoFrame(VideoFrame {
            seq,
            pts_ms: seq * 100,
            width,
            height,
            rgba: vec![seq as u8; (width * height * 4) as usize],
        })
    }

    /// The fake worker's side: the protocol as the real one speaks it.
    fn serve(
        behavior: Behavior,
        mut stdin: PipeReader,
        mut stdout: PipeWriter,
        seen: &Seen,
    ) -> WorkerExit {
        let out = |stdout: &mut PipeWriter, frame: &Frame| {
            write_frame(stdout, frame, Direction::WorkerToParent).is_ok()
        };
        let Ok(Some(Frame::Hello(_))) = read_frame(&mut stdin, Direction::ParentToWorker) else {
            return WorkerExit::Code(2);
        };
        if behavior.leaves_at_hello {
            return WorkerExit::Code(0);
        }
        if let Some(code) = behavior.refuses_hello {
            out(&mut stdout, &Frame::Failure(Failure { code }));
            if behavior.trails {
                let _ = stdout.write_all(&[0]);
            }
            return behavior.refused_exit;
        }
        if behavior.silent {
            // Holds its pipes until killed.
            let _ = read_frame(&mut stdin, Direction::ParentToWorker);
            return WorkerExit::Signal(9);
        }
        out(
            &mut stdout,
            &Frame::Ready(Ready {
                build_id: build_id(),
            }),
        );
        let Ok(Some(Frame::PlayOpen(_))) = read_frame(&mut stdin, Direction::ParentToWorker) else {
            return WorkerExit::Code(2);
        };
        if let Some(code) = behavior.refuses {
            out(&mut stdout, &Frame::Failure(Failure { code }));
            if behavior.trails {
                let _ = stdout.write_all(&[0]);
            }
            return behavior.refused_exit;
        }
        std::thread::sleep(behavior.opens_after);
        out(
            &mut stdout,
            &Frame::Opened(if behavior.sounds { SOUNDING } else { OPENED }),
        );
        out(&mut stdout, &frame(0, false));
        out(&mut stdout, &Frame::PlayState(PlayState::Paused));
        if behavior.leaves_open {
            return WorkerExit::Code(0);
        }
        let mut sent = 0;
        loop {
            let command = match read_frame(&mut stdin, Direction::ParentToWorker) {
                Ok(Some(Frame::PlayCommand(command))) => command,
                _ => return WorkerExit::Code(0),
            };
            seen.commands.lock().unwrap().push(command);
            match command {
                PlayCommand::Play => {
                    out(&mut stdout, &Frame::PlayState(PlayState::Playing));
                    if let Some(status) = behavior.says_sound {
                        out(&mut stdout, &Frame::AudioState(status));
                    }
                    while sent < behavior.frames {
                        sent += 1;
                        if behavior.dies_after == Some(sent) {
                            return WorkerExit::Signal(9);
                        }
                        std::thread::sleep(behavior.every);
                        if behavior.interleaves_states {
                            out(&mut stdout, &Frame::PlayState(PlayState::Paused));
                            out(&mut stdout, &Frame::PlayState(PlayState::Playing));
                        }
                        if !out(&mut stdout, &frame(sent, behavior.misframes)) {
                            return WorkerExit::Code(2);
                        }
                    }
                    out(&mut stdout, &Frame::PlayState(PlayState::Ended));
                    return behavior.ended_exit;
                }
                PlayCommand::Pause => {
                    out(&mut stdout, &Frame::PlayState(PlayState::Paused));
                }
                PlayCommand::Heartbeat | PlayCommand::Mute | PlayCommand::Unmute => {}
                PlayCommand::Close => return WorkerExit::Code(0),
            }
        }
    }

    fn client(behavior: Behavior) -> (WorkerClient, Arc<Seen>) {
        let seen = Arc::new(Seen::default());
        let platform = Fake {
            behavior,
            seen: Arc::clone(&seen),
        };
        (
            WorkerClient::with(
                build_id(),
                Box::new(platform),
                Budgets {
                    ready: Duration::from_secs(1),
                    ..Budgets::PRODUCTION
                },
            ),
            seen,
        )
    }

    fn open() -> PlayOpen {
        PlayOpen {
            // Absolute on every platform; the fake worker never opens it.
            source: Source::user_pull(
                NativePath::from_path(&std::env::temp_dir().join("kettle-session-clip.mp4"))
                    .unwrap(),
                GuiActionWitness::from_explicit_gui_action(),
            ),
            max_width: 640,
            max_height: 360,
            start_ms: 0,
            max_fps_milli: 30_000,
            audio: AudioRequest::On,
        }
    }

    /// Wakes counted, for a test to wait on.
    fn counter() -> (Arc<AtomicUsize>, Box<dyn Fn() + Send + Sync>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        (
            wakes,
            Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    /// Wait until `done` says so, within two seconds.
    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !done() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn ended_followed_by_a_crash_fails() {
        let (client, seen) = client(Behavior {
            frames: 0,
            ended_exit: WorkerExit::Signal(6),
            ..PLAYS
        });
        let session = client.play(&open(), Box::new(|| {})).unwrap();
        session.play();
        until("supervisor finished", || {
            client.shared.sessions.load(Ordering::SeqCst) == 0
        });
        assert!(seen.reaped.load(Ordering::SeqCst) > 0);
        assert_eq!(
            session.status(),
            SessionStatus::Failed(FailureCode::RenderResource)
        );
    }

    /// A session opens paused on its first frame, plays its frames, and
    /// ends; its worker is reaped and it no longer counts.
    #[test]
    fn a_session_opens_plays_and_ends() {
        let (client, seen) = client(PLAYS);
        let (wakes, wake) = counter();
        let session = client.play(&open(), wake).unwrap();
        until("opened", || {
            matches!(
                session.status(),
                SessionStatus::Open {
                    state: PlayState::Paused,
                    ..
                }
            )
        });
        let SessionStatus::Open { opened, .. } = session.status() else {
            unreachable!()
        };
        assert_eq!(opened, OPENED);
        until("the first frame", || {
            session
                .take_frame()
                .is_some_and(|frame| frame.seq == 0 && frame.rgba.len() == 4 * 2 * 4)
        });
        // The wake follows the frame it is for.
        until("a wake", || wakes.load(Ordering::SeqCst) >= 1);
        session.play();
        let mut last = 0;
        until("the end", || {
            if let Some(frame) = session.take_frame() {
                assert!(frame.seq > last, "frames go forward");
                last = frame.seq;
                session.recycle(frame.rgba);
            }
            matches!(
                session.status(),
                SessionStatus::Open {
                    state: PlayState::Ended,
                    ..
                }
            )
        });
        until("reaped", || seen.reaped.load(Ordering::SeqCst) > 0);
        assert!(!seen.killed.load(Ordering::SeqCst), "it ended by itself");
        // The supervisor frees the slot once it has reaped the worker.
        until("the slot freed", || {
            client.shared.sessions.load(Ordering::SeqCst) == 0
        });
        // Heartbeats come with time, as many as the run took.
        let commands = seen.commands.lock().unwrap();
        let asked: Vec<_> = commands
            .iter()
            .filter(|command| **command != PlayCommand::Heartbeat)
            .collect();
        assert_eq!(asked, [&PlayCommand::Play]);
    }

    #[test]
    fn non_video_messages_preserve_recycled_pixels() {
        let (client, _seen) = client(Behavior {
            frames: 8,
            every: Duration::ZERO,
            interleaves_states: true,
            ..PLAYS
        });
        let session = client.play(&open(), Box::new(|| {})).unwrap();
        let mut first = None;
        until("first frame", || {
            first = session.take_frame();
            first.is_some()
        });
        let mut first = first.unwrap();
        first.rgba.reserve(4096);
        session.recycle(first.rgba);
        session.recycle(Vec::with_capacity(4096));
        session.play();
        until("ended", || {
            client.shared.sessions.load(Ordering::SeqCst) == 0
        });
        let last = session.take_frame().unwrap();
        assert_eq!(last.seq, 8);
        assert!(
            last.rgba.capacity() >= 4096,
            "recycled pixel buffer was discarded"
        );
    }

    /// Only the newest frame waits for the caller, and one wake stands for
    /// everything new until the caller looks.
    #[test]
    fn only_the_newest_frame_waits_and_one_wake_stands() {
        let (client, _seen) = client(Behavior {
            frames: 8,
            every: Duration::from_millis(1),
            ..PLAYS
        });
        let (wakes, wake) = counter();
        let session = client.play(&open(), wake).unwrap();
        // Nobody looks until the end: the first news wakes the caller, and
        // nothing after it does.
        until("woken", || wakes.load(Ordering::SeqCst) == 1);
        session.play();
        until("ended", || {
            client.shared.sessions.load(Ordering::SeqCst) == 0
        });
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert_eq!(session.take_frame().map(|frame| frame.seq), Some(8));
        assert_eq!(session.take_frame(), None);
    }

    /// Dropping a session closes it: Close goes out, its worker exits by
    /// itself and is reaped, and it stops counting at once.
    #[test]
    fn dropping_a_session_closes_it() {
        let (client, seen) = client(PLAYS);
        let (_wakes, wake) = counter();
        let session = client.play(&open(), wake).unwrap();
        until("opened", || {
            matches!(session.status(), SessionStatus::Open { .. })
        });
        assert_eq!(client.shared.sessions.load(Ordering::SeqCst), 1);
        drop(session);
        assert_eq!(client.shared.sessions.load(Ordering::SeqCst), 0);
        until("closed", || seen.exited.load(Ordering::SeqCst));
        until("reaped", || seen.reaped.load(Ordering::SeqCst) > 0);
        assert_eq!(*seen.commands.lock().unwrap(), [PlayCommand::Close]);
        assert!(!seen.killed.load(Ordering::SeqCst));
    }

    /// At most two sessions are open at once; closing one makes room.
    #[test]
    fn two_sessions_at_most() {
        let (client, _seen) = client(PLAYS);
        let quiet = || -> Box<dyn Fn() + Send + Sync> { Box::new(|| {}) };
        let first = client.play(&open(), quiet()).unwrap();
        let _second = client.play(&open(), quiet()).unwrap();
        assert_eq!(
            client.play(&open(), quiet()).err(),
            Some(FailureCode::WorkerUnavailable)
        );
        drop(first);
        assert!(client.play(&open(), quiet()).is_ok());
        let mut bad = open();
        bad.max_width = 0;
        assert_eq!(
            client.play(&bad, quiet()).err(),
            Some(FailureCode::BadParams)
        );
    }

    /// Only the latest of each wish reaches the worker: Play then Pause is
    /// Pause, Unmute then Mute is Mute, and neither kind takes the other's
    /// place.
    #[test]
    fn only_the_latest_of_each_wish_is_sent() {
        let (client, seen) = client(PLAYS);
        let session = client.play(&open(), Box::new(|| {})).unwrap();
        until("opened", || {
            matches!(session.status(), SessionStatus::Open { .. })
        });
        // Each run of wishes taken before the writer can look, then sent.
        let wish = |wishes: &[PlayCommand], count: usize| {
            {
                let mut state = session.inner.lock();
                for &command in wishes {
                    state.wishes.add(command);
                }
            }
            session.inner.changed.notify_all();
            until("sent", || seen.commands.lock().unwrap().len() >= count);
            std::thread::sleep(Duration::from_millis(50));
            seen.commands.lock().unwrap().clone()
        };
        use PlayCommand::{Mute, Pause, Play, Unmute};
        assert_eq!(wish(&[Pause, Unmute, Mute], 2), [Pause, Mute]);
        assert_eq!(
            wish(&[Play, Unmute, Pause], 4),
            [Pause, Mute, Pause, Unmute]
        );
    }

    /// A session's sound is Opened's until the worker says it went, and
    /// none before it opened.
    #[test]
    fn a_session_s_sound_is_what_the_worker_says() {
        let (sounding, _seen) = client(Behavior {
            sounds: true,
            says_sound: Some(AudioStatus::Unavailable),
            ..PLAYS
        });
        let session = sounding.play(&open(), Box::new(|| {})).unwrap();
        until("opened", || {
            matches!(session.status(), SessionStatus::Open { .. })
        });
        assert_eq!(session.audio(), Some(AudioStatus::Ready));
        session.play();
        until("its sound went", || {
            session.audio() == Some(AudioStatus::Unavailable)
        });
        until("the end", || {
            matches!(
                session.status(),
                SessionStatus::Open {
                    state: PlayState::Ended,
                    ..
                }
            )
        });
        let (silent, _seen) = client(Behavior {
            silent: true,
            ..PLAYS
        });
        let opening = silent.play(&open(), Box::new(|| {})).unwrap();
        assert_eq!(opening.audio(), None);
    }

    /// A session the worker refuses says why, though before Ready only a
    /// handshake refusal does and only with nothing after it, as for a
    /// render; one whose worker leaves without ending it, exit 0 or not,
    /// fails; one whose frames are not the size it opened at, whose sound
    /// the worker says came where it had none, whose worker dies by a
    /// signal, or that holds too much memory, fails as the worker's fault;
    /// one that never becomes ready times out.
    #[test]
    fn a_session_that_cannot_go_on_says_why() {
        for (behavior, why) in [
            (
                Behavior {
                    refuses_hello: Some(FailureCode::BackendUnavailable),
                    ..PLAYS
                },
                FailureCode::WorkerUnavailable,
            ),
            (
                Behavior {
                    refuses_hello: Some(FailureCode::RestartRequired),
                    ..PLAYS
                },
                FailureCode::RestartRequired,
            ),
            (
                Behavior {
                    refuses_hello: Some(FailureCode::UnknownMethod),
                    ..PLAYS
                },
                FailureCode::UnknownMethod,
            ),
            (
                Behavior {
                    refuses_hello: Some(FailureCode::UnknownMethod),
                    trails: true,
                    ..PLAYS
                },
                FailureCode::WorkerUnavailable,
            ),
            (
                Behavior {
                    refuses: Some(FailureCode::BackendUnavailable),
                    trails: true,
                    ..PLAYS
                },
                FailureCode::WorkerUnavailable,
            ),
            (
                Behavior {
                    leaves_at_hello: true,
                    ..PLAYS
                },
                FailureCode::WorkerUnavailable,
            ),
            (
                Behavior {
                    leaves_open: true,
                    ..PLAYS
                },
                FailureCode::WorkerUnavailable,
            ),
            (
                Behavior {
                    refuses: Some(FailureCode::BackendUnavailable),
                    ..PLAYS
                },
                FailureCode::BackendUnavailable,
            ),
            (
                Behavior {
                    refuses: Some(FailureCode::DisplayDisabled),
                    ..PLAYS
                },
                FailureCode::WorkerUnavailable,
            ),
            // A refusal is kept only as a render's is: after Ready from a
            // worker that then exits 0, before it from one that exits at
            // all; else the exit says why.
            (
                Behavior {
                    refuses: Some(FailureCode::BackendUnavailable),
                    refused_exit: WorkerExit::Code(4),
                    ..PLAYS
                },
                FailureCode::RenderTimeout,
            ),
            (
                Behavior {
                    refuses: Some(FailureCode::BackendUnavailable),
                    refused_exit: WorkerExit::Code(9),
                    ..PLAYS
                },
                FailureCode::RestartRequired,
            ),
            (
                Behavior {
                    refuses_hello: Some(FailureCode::UnknownMethod),
                    refused_exit: WorkerExit::Code(4),
                    ..PLAYS
                },
                FailureCode::UnknownMethod,
            ),
            (
                Behavior {
                    refuses_hello: Some(FailureCode::RestartRequired),
                    refused_exit: WorkerExit::Signal(6),
                    ..PLAYS
                },
                FailureCode::RenderResource,
            ),
            (
                Behavior {
                    misframes: true,
                    ..PLAYS
                },
                FailureCode::WorkerUnavailable,
            ),
            // Sound only goes: a silent session never gains it, and a sound
            // that goes never comes back as Ready.
            (
                Behavior {
                    says_sound: Some(AudioStatus::Unavailable),
                    ..PLAYS
                },
                FailureCode::WorkerUnavailable,
            ),
            (
                Behavior {
                    sounds: true,
                    says_sound: Some(AudioStatus::Ready),
                    ..PLAYS
                },
                FailureCode::WorkerUnavailable,
            ),
            (
                Behavior {
                    dies_after: Some(2),
                    ..PLAYS
                },
                FailureCode::RenderResource,
            ),
            (
                // Playing long enough for a footprint sample, every 25 ms,
                // to come before a short video would end by itself.
                Behavior {
                    footprint: u64::MAX,
                    frames: 1_000,
                    ..PLAYS
                },
                FailureCode::RenderResource,
            ),
            (
                Behavior {
                    silent: true,
                    ..PLAYS
                },
                FailureCode::RenderTimeout,
            ),
        ] {
            let (client, _seen) = client(behavior);
            let session = client.play(&open(), Box::new(|| {})).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                match session.status() {
                    SessionStatus::Failed(code) => {
                        assert_eq!(code, why, "{behavior:?}");
                        break;
                    }
                    SessionStatus::Open {
                        state: PlayState::Paused,
                        ..
                    } => session.play(),
                    _ => {}
                }
                assert!(Instant::now() < deadline, "{behavior:?} never failed");
                std::thread::sleep(Duration::from_millis(5));
            }
            // It stops counting as its supervisor finishes, just after.
            until("uncounted", || {
                client.shared.sessions.load(Ordering::SeqCst) == 0
            });
        }
    }

    /// Opened that comes after the session gave up opening leaves the
    /// failure standing: never an open session with no worker.
    #[test]
    fn a_late_opened_leaves_the_failure_standing() {
        let (client, _seen) = client(Behavior {
            opens_after: OPEN_BUDGET + Duration::from_millis(500),
            ..PLAYS
        });
        let session = client.play(&open(), Box::new(|| {})).unwrap();
        let started = Instant::now();
        let deadline = started + OPEN_BUDGET + Duration::from_secs(2);
        while session.status() != SessionStatus::Failed(FailureCode::RenderTimeout) {
            assert!(Instant::now() < deadline, "{:?}", session.status());
            std::thread::sleep(Duration::from_millis(10));
        }
        // Past the moment the worker says Opened.
        std::thread::sleep(
            OPEN_BUDGET + Duration::from_millis(1_500) - started.elapsed().min(OPEN_BUDGET),
        );
        assert_eq!(
            session.status(),
            SessionStatus::Failed(FailureCode::RenderTimeout)
        );
    }

    /// Heartbeats go out each second while the caller keeps the session,
    /// and stop once it has not for three seconds.
    #[test]
    fn heartbeats_follow_the_caller() {
        let (client, seen) = client(PLAYS);
        let session = client.play(&open(), Box::new(|| {})).unwrap();
        until("opened", || {
            matches!(session.status(), SessionStatus::Open { .. })
        });
        let beats = || {
            seen.commands
                .lock()
                .unwrap()
                .iter()
                .filter(|command| **command == PlayCommand::Heartbeat)
                .count()
        };
        for _ in 0..5 {
            session.keep();
            std::thread::sleep(Duration::from_millis(500));
        }
        let kept = beats();
        assert!((2..=3).contains(&kept), "{kept} heartbeats in 2.5 s");
        std::thread::sleep(Duration::from_millis(4500));
        let after = beats();
        assert!(after <= kept + 3, "heartbeats went on: {after}");
        let settled = after;
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(beats(), settled, "heartbeats without the caller");
    }
}
