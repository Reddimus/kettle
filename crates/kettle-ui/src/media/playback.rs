//! A video playing in its lane: a session in a worker of its own (see
//! `kettle_media::session`), the frame it shows, and what the user asked
//! for. Nothing starts one on its own: the user plays it from the lane's
//! button, Space while the lane holds the keyboard, or `video_play`.
//!
//! A playback outlives its session. Pausing keeps the session, paused; the
//! video's end, a failure, a seek, or a third lane starting to play lets the
//! session go and keeps the frame shown and where the video stands, so Play
//! opens a new session there (from the start after the end). A seek is a new
//! session at once, the old frame shown until the new one's first. While its
//! lane cannot be seen the session pauses, and it stays paused once the lane
//! shows again, until the user plays it. The playback, and its session, end
//! with the lane, its item, or its pane.

use std::time::Instant;

use kettle_core::{GraphicsBudget, ImageData, ImageRefusal};
use kettle_media::session::{PlaySession, SessionFrame, SessionStatus};
use kettle_media::{FailureCode, PlayState};

/// Most lanes whose video has a session at once: the sessions the client
/// allows. A lane starting to play past it takes the session of the one
/// played least recently, which keeps its frame.
pub(crate) const MAX_PLAYING: usize = kettle_media::session::MAX_PLAY_SESSIONS;

/// What a playback asks of its session: [`PlaySession`]'s calls, and a
/// stand-in's in tests.
pub(crate) trait Session {
    fn status(&self) -> SessionStatus;
    fn take_frame(&self) -> Option<SessionFrame>;
    fn recycle(&self, rgba: Vec<u8>);
    fn play(&self);
    fn pause(&self);
    fn keep(&self);
}

impl Session for PlaySession {
    fn status(&self) -> SessionStatus {
        PlaySession::status(self)
    }
    fn take_frame(&self) -> Option<SessionFrame> {
        PlaySession::take_frame(self)
    }
    fn recycle(&self, rgba: Vec<u8>) {
        PlaySession::recycle(self, rgba);
    }
    fn play(&self) {
        PlaySession::play(self);
    }
    fn pause(&self) {
        PlaySession::pause(self);
    }
    fn keep(&self) {
        PlaySession::keep(self);
    }
}

/// What the lane's play button does now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Button {
    Play,
    Pause,
}

/// What the lane's footer says about its playback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    /// Opening, before the first frame.
    Opening,
    /// Playing, without sound.
    Playing,
    Failed(FailureCode),
}

/// What a press on the play button asks of the lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Toggled {
    /// Done, by the session it has.
    Done,
    /// It needs a new session, playing from `at_ms`.
    Open { at_ms: u64 },
}

/// A lane's playback of its item as it was when the user played it.
pub(crate) struct LanePlayback {
    pub item: u64,
    pub generation: u64,
    duration_ms: u64,
    session: Option<Box<dyn Session>>,
    /// The frame shown and the moment of the video it is, kept when the
    /// session goes.
    shown: Option<(ImageData, u64)>,
    /// Where the video stands: the frame shown, or where the session that
    /// will show the next one starts.
    position_ms: u64,
    /// Where a drag along the bar would move the video, until its release.
    scrub_ms: Option<u64>,
    /// The user has it playing, rather than paused: only ever while it has a
    /// session, since everything that lets the session go clears it.
    wants_play: bool,
    /// Its lane cannot be seen, so its session is paused for now.
    hidden: bool,
    /// The session played the video to its end.
    ended: bool,
    failure: Option<FailureCode>,
    /// When the user last played, paused or moved it: the playback played
    /// least recently gives its session way first.
    pub touched: Instant,
}

impl std::fmt::Debug for LanePlayback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanePlayback")
            .field("item", &self.item)
            .field("generation", &self.generation)
            .field("session", &self.session.is_some())
            .field("position_ms", &self.position_ms)
            .field("wants_play", &self.wants_play)
            .field("ended", &self.ended)
            .field("failure", &self.failure)
            .finish_non_exhaustive()
    }
}

impl LanePlayback {
    /// A playback of `item` (at `generation`), `duration_ms` long, opening
    /// with `session` from `start_ms`, playing at once when `play`.
    pub(crate) fn new(
        item: u64,
        generation: u64,
        duration_ms: u64,
        session: Box<dyn Session>,
        start_ms: u64,
        play: bool,
        now: Instant,
    ) -> Self {
        let mut playback = Self {
            item,
            generation,
            duration_ms,
            session: None,
            shown: None,
            position_ms: start_ms,
            scrub_ms: None,
            wants_play: false,
            hidden: false,
            ended: false,
            failure: None,
            touched: now,
        };
        playback.reopen(session, start_ms, play, now);
        playback
    }

    /// Whether this plays `item` as it is at `generation`.
    pub(crate) fn serves(&self, item: u64, generation: u64) -> bool {
        self.item == item && self.generation == generation
    }

    /// Open the video again with `session` from `start_ms`, playing at
    /// once when `play` and the lane can be seen; the frame shown stays
    /// until the new one's first.
    pub(crate) fn reopen(
        &mut self,
        session: Box<dyn Session>,
        start_ms: u64,
        play: bool,
        now: Instant,
    ) {
        let play = play && !self.hidden;
        if play {
            session.play();
        }
        self.session = Some(session);
        self.position_ms = start_ms.min(self.duration_ms);
        self.wants_play = play;
        self.ended = false;
        self.failure = None;
        self.touched = now;
    }

    /// Whether it has a session now: one opening, playing or paused.
    pub(crate) fn has_session(&self) -> bool {
        self.session.is_some()
    }

    /// Take what the session has for the lane: its newest frame, charged to
    /// `budget` (the frame it replaces gives its buffer back for the next
    /// one), and where it is. A session that ended or failed goes. Whether
    /// anything the lane shows changed.
    pub(crate) fn poll(&mut self, budget: &GraphicsBudget) -> bool {
        let Some(session) = self.session.as_deref() else {
            return false;
        };
        // The state first: a session publishes its last frame before it
        // says it ended, so a frame taken after is never one lost.
        let status = session.status();
        let mut changed = false;
        if let Some(frame) = session.take_frame() {
            match ImageData::try_with_budget(frame.width, frame.height, frame.rgba, budget) {
                Ok(image) => {
                    if let Some((old, _)) = self.shown.replace((image, frame.pts_ms))
                        && let Ok(pixels) = old.into_pixels()
                    {
                        session.recycle(pixels);
                    }
                    self.position_ms = frame.pts_ms.min(self.duration_ms);
                    changed = true;
                }
                // No room for it now: the frame shown stays.
                Err(ImageRefusal::NoRoom(pixels)) => session.recycle(pixels),
                Err(ImageRefusal::Invalid) => {}
            }
        }
        match status {
            SessionStatus::Open {
                state: PlayState::Ended,
                ..
            } => {
                self.ended = true;
                self.wants_play = false;
                self.session = None;
                changed = true;
            }
            SessionStatus::Failed(code) => {
                self.failure = Some(code);
                self.wants_play = false;
                self.session = None;
                changed = true;
            }
            SessionStatus::Opening | SessionStatus::Open { .. } => {}
        }
        changed
    }

    /// Say the lane still looks at its session, so its worker keeps it.
    pub(crate) fn keep(&self) {
        if let Some(session) = &self.session {
            session.keep();
        }
    }

    /// The play button's press: pause what plays, play what is paused, or
    /// ask for a session to play where the video stands (from the start
    /// once it ended).
    pub(crate) fn toggle(&mut self, now: Instant) -> Toggled {
        self.touched = now;
        match self.session.as_deref() {
            Some(session) if self.wants_play => {
                session.pause();
                self.wants_play = false;
                Toggled::Done
            }
            Some(session) => {
                if !self.hidden {
                    session.play();
                    self.wants_play = true;
                }
                Toggled::Done
            }
            None => Toggled::Open {
                at_ms: if self.ended { 0 } else { self.position_ms },
            },
        }
    }

    /// Let the session go, keeping the frame shown and where the video
    /// stands: a third lane playing takes it.
    pub(crate) fn park(&mut self) {
        self.session = None;
        self.wants_play = false;
    }

    /// Pause the session once the lane cannot be seen. It stays paused when
    /// the lane shows again: only the user's own Play plays, and a control
    /// client or a script can be what showed it.
    pub(crate) fn set_hidden(&mut self, hidden: bool) {
        if hidden == self.hidden {
            return;
        }
        self.hidden = hidden;
        if hidden && self.wants_play {
            self.wants_play = false;
            if let Some(session) = self.session.as_deref() {
                session.pause();
            }
        }
    }

    /// Follow a drag along the bar to `fraction` of the video, shown until
    /// its release.
    pub(crate) fn scrub(&mut self, fraction: f64, now: Instant) {
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.scrub_ms = Some((self.duration_ms as f64 * fraction).round() as u64);
        self.touched = now;
    }

    /// A move straight to `at_ms`, held to the video: where to open it, and
    /// whether it plays there, as it did before. Nothing moves until a
    /// session opens there (`reopen`): an open refused (a control client's
    /// or a Lua script's) or failed leaves the bar where the video is.
    pub(crate) fn seek_target(&self, at_ms: u64) -> (u64, bool) {
        (at_ms.min(self.duration_ms), self.wants_play)
    }

    /// A drag along the bar that ended without its release: the video stays
    /// where it was.
    pub(crate) fn cancel_scrub(&mut self) {
        self.scrub_ms = None;
    }

    /// The drag's release: where to open the video, and whether it plays
    /// there, as it did before; `None` without a drag. The drag ends, and
    /// as with [`Self::seek_target`] the bar shows where the video is until
    /// a session opens where the drag ended.
    pub(crate) fn release_scrub(&mut self) -> Option<(u64, bool)> {
        let at_ms = self.scrub_ms.take()?;
        Some((at_ms, self.wants_play))
    }

    pub(crate) fn button(&self) -> Button {
        if self.wants_play {
            Button::Pause
        } else {
            Button::Play
        }
    }

    pub(crate) fn frame(&self) -> Option<&ImageData> {
        self.shown.as_ref().map(|(image, _)| image)
    }

    /// Where the bar says the video is: a drag's place while it is held.
    pub(crate) fn position_ms(&self) -> u64 {
        self.scrub_ms.unwrap_or(self.position_ms)
    }

    pub(crate) fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    pub(crate) fn notice(&self) -> Option<Notice> {
        if let Some(code) = self.failure {
            return Some(Notice::Failed(code));
        }
        self.session.as_ref()?;
        if self.shown.is_none() {
            Some(Notice::Opening)
        } else if self.wants_play {
            Some(Notice::Playing)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use kettle_media::{Opened, VideoCodec, VideoInfo};

    /// What a stand-in session was told, and what it has to give.
    #[derive(Default)]
    struct Script {
        commands: Vec<&'static str>,
        frames: Vec<SessionFrame>,
        status: Option<SessionStatus>,
        recycled: usize,
        /// A frame and the state after it, which the session's reader
        /// publishes while the GUI is taking a frame.
        arriving: Option<(SessionFrame, SessionStatus)>,
    }

    struct Stand(Rc<RefCell<Script>>);

    impl Session for Stand {
        fn status(&self) -> SessionStatus {
            self.0.borrow().status.unwrap_or(SessionStatus::Opening)
        }
        fn take_frame(&self) -> Option<SessionFrame> {
            let mut script = self.0.borrow_mut();
            let taken = script.frames.pop();
            if let Some((frame, status)) = script.arriving.take() {
                script.frames.push(frame);
                script.status = Some(status);
            }
            taken
        }
        fn recycle(&self, _rgba: Vec<u8>) {
            self.0.borrow_mut().recycled += 1;
        }
        fn play(&self) {
            self.0.borrow_mut().commands.push("play");
        }
        fn pause(&self) {
            self.0.borrow_mut().commands.push("pause");
        }
        fn keep(&self) {
            self.0.borrow_mut().commands.push("keep");
        }
    }

    fn stand() -> (Box<dyn Session>, Rc<RefCell<Script>>) {
        let script = Rc::new(RefCell::new(Script::default()));
        (Box::new(Stand(Rc::clone(&script))), script)
    }

    fn frame(pts_ms: u64) -> SessionFrame {
        SessionFrame {
            seq: pts_ms / 100,
            pts_ms,
            width: 2,
            height: 1,
            rgba: vec![9; 8],
        }
    }

    fn open(state: PlayState) -> SessionStatus {
        SessionStatus::Open {
            opened: Opened {
                info: VideoInfo {
                    duration_ms: 9000,
                    width: 2,
                    height: 1,
                    rotation: 0,
                    codec: VideoCodec::H264,
                    fps_milli: Some(10_000),
                    has_audio: false,
                    container: None,
                },
                audio: kettle_media::AudioStatus::NoTrack,
                width: 2,
                height: 1,
                fps_milli: 10_000,
                start_ms: 0,
            },
            state,
        }
    }

    fn budget() -> GraphicsBudget {
        GraphicsBudget::default()
    }

    /// A playback opens playing, says so until its first frame, shows each
    /// newest frame and gives the one it replaced back, and the button
    /// pauses it; its end lets the session go and keeps the frame, and Play
    /// then asks for a session from the start.
    #[test]
    fn a_playback_shows_its_frames_and_outlives_its_end() {
        let now = Instant::now();
        let (session, script) = stand();
        let mut playback = LanePlayback::new(1, 2, 9000, session, 0, true, now);
        assert_eq!(script.borrow().commands, ["play"]);
        assert_eq!(playback.notice(), Some(Notice::Opening));
        assert_eq!(playback.button(), Button::Pause);
        assert!(!playback.poll(&budget()));
        script.borrow_mut().frames.push(frame(100));
        script.borrow_mut().status = Some(open(PlayState::Playing));
        assert!(playback.poll(&budget()));
        assert_eq!(playback.position_ms(), 100);
        assert_eq!(playback.notice(), Some(Notice::Playing));
        script.borrow_mut().frames.push(frame(200));
        assert!(playback.poll(&budget()));
        assert_eq!(
            script.borrow().recycled,
            1,
            "the frame replaced served again"
        );
        assert_eq!(playback.toggle(now), Toggled::Done);
        assert_eq!(script.borrow().commands, ["play", "pause"]);
        assert_eq!((playback.button(), playback.notice()), (Button::Play, None));
        assert_eq!(playback.toggle(now), Toggled::Done);
        assert_eq!(script.borrow().commands, ["play", "pause", "play"]);
        script.borrow_mut().status = Some(open(PlayState::Ended));
        assert!(playback.poll(&budget()));
        assert!(!playback.has_session());
        assert!(playback.frame().is_some(), "the last frame stays");
        assert_eq!(playback.button(), Button::Play);
        assert_eq!(playback.toggle(now), Toggled::Open { at_ms: 0 });
    }

    /// The last frame and Ended can land while a frame is being taken: the
    /// playback still shows that frame before it lets the session go.
    #[test]
    fn the_last_frame_comes_with_the_end() {
        let now = Instant::now();
        let (session, script) = stand();
        let mut playback = LanePlayback::new(1, 2, 9000, session, 0, true, now);
        script.borrow_mut().status = Some(open(PlayState::Playing));
        script.borrow_mut().frames.push(frame(8800));
        assert!(playback.poll(&budget()));
        script.borrow_mut().arriving = Some((frame(8900), open(PlayState::Ended)));
        playback.poll(&budget());
        playback.poll(&budget());
        assert_eq!(playback.position_ms(), 8900);
        assert!(!playback.has_session());
    }

    /// A parked or failed playback keeps its frame and where it stands, and
    /// Play asks for a session there; a failure says why.
    #[test]
    fn a_parked_or_failed_playback_reopens_where_it_stands() {
        let now = Instant::now();
        let (session, script) = stand();
        let mut playback = LanePlayback::new(1, 2, 9000, session, 3000, true, now);
        script.borrow_mut().frames.push(frame(3500));
        playback.poll(&budget());
        playback.park();
        assert!(!playback.has_session());
        assert_eq!(playback.button(), Button::Play);
        assert_eq!(playback.toggle(now), Toggled::Open { at_ms: 3500 });
        let (session, script) = stand();
        playback.reopen(session, 3500, true, now);
        script.borrow_mut().status = Some(SessionStatus::Failed(FailureCode::CodecUnavailable));
        assert!(playback.poll(&budget()));
        assert_eq!(
            playback.notice(),
            Some(Notice::Failed(FailureCode::CodecUnavailable))
        );
        assert_eq!(playback.toggle(now), Toggled::Open { at_ms: 3500 });
        let (session, _) = stand();
        playback.reopen(session, 3500, true, now);
        assert_eq!(
            playback.notice(),
            Some(Notice::Playing),
            "the old frame shows"
        );
    }

    /// A hidden lane pauses a playing session, which stays paused once the
    /// lane shows again, a control client or a script being able to show
    /// it; a paused one stays paused either way, and nothing plays while
    /// hidden, Play or a new session included.
    #[test]
    fn a_hidden_lane_pauses_and_stays_paused() {
        let now = Instant::now();
        let (session, script) = stand();
        let mut playback = LanePlayback::new(1, 2, 9000, session, 0, true, now);
        playback.set_hidden(true);
        playback.set_hidden(true);
        playback.set_hidden(false);
        assert_eq!(script.borrow().commands, ["play", "pause"]);
        assert_eq!(playback.button(), Button::Play);
        // The user's Play plays it again.
        playback.toggle(now);
        assert_eq!(script.borrow().commands, ["play", "pause", "play"]);
        playback.toggle(now);
        playback.set_hidden(true);
        playback.set_hidden(false);
        assert_eq!(script.borrow().commands, ["play", "pause", "play", "pause"]);
        playback.set_hidden(true);
        playback.toggle(now);
        playback.set_hidden(false);
        assert_eq!(
            script.borrow().commands,
            ["play", "pause", "play", "pause"],
            "played while hidden: nothing"
        );
        assert_eq!(playback.button(), Button::Play);
        // Opened again while hidden: it waits, paused, for the user.
        playback.set_hidden(true);
        let (session, again) = stand();
        playback.reopen(session, 4000, true, now);
        playback.set_hidden(false);
        assert!(again.borrow().commands.is_empty());
        assert_eq!(playback.button(), Button::Play);
    }

    /// A drag along the bar moves where the bar says the video is, held to
    /// the video, and its release says where to open it and whether it
    /// plays there. The video, and the bar after the release, move only
    /// when a session opens there.
    #[test]
    fn a_drag_along_the_bar_moves_the_video_on_release() {
        let now = Instant::now();
        let (session, _) = stand();
        let mut playback = LanePlayback::new(1, 2, 9000, session, 0, false, now);
        assert_eq!(playback.release_scrub(), None);
        playback.scrub(0.5, now);
        assert_eq!(playback.position_ms(), 4500);
        playback.scrub(2.0, now);
        assert_eq!(playback.position_ms(), 9000);
        playback.scrub(f64::NAN, now);
        assert_eq!(playback.position_ms(), 0);
        playback.scrub(0.25, now);
        assert_eq!(playback.release_scrub(), Some((2250, false)));
        assert_eq!(playback.position_ms(), 0, "no session opened there yet");
        let (session, _) = stand();
        playback.reopen(session, 2250, false, now);
        assert_eq!(playback.position_ms(), 2250);
        playback.toggle(now);
        playback.scrub(0.75, now);
        assert_eq!(playback.release_scrub(), Some((6750, true)));
    }

    /// A seek says where to open the video and whether it plays there, and
    /// moves nothing itself: when the open is refused (a control client's
    /// or a Lua script's) or fails, the bar stays where the video is, and
    /// the video goes on from there.
    #[test]
    fn a_seek_moves_nothing_until_its_session_opens() {
        let now = Instant::now();
        let (session, script) = stand();
        let mut playback = LanePlayback::new(1, 2, 9000, session, 3000, true, now);
        assert_eq!(playback.seek_target(8000), (8000, true));
        assert_eq!(
            playback.seek_target(20_000),
            (9000, true),
            "held to the video"
        );
        assert_eq!(playback.position_ms(), 3000);
        assert_eq!(playback.button(), Button::Pause);
        assert!(playback.has_session());
        assert_eq!(script.borrow().commands, ["play"]);
        let (session, again) = stand();
        let (at_ms, play) = playback.seek_target(8000);
        playback.reopen(session, at_ms, play, now);
        assert_eq!(playback.position_ms(), 8000);
        assert_eq!(again.borrow().commands, ["play"]);
    }

    /// A frame with no room in the account is given back, and the one shown
    /// stays.
    #[test]
    fn a_frame_without_room_is_given_back() {
        let now = Instant::now();
        let (session, script) = stand();
        let mut playback = LanePlayback::new(1, 2, 9000, session, 0, true, now);
        script.borrow_mut().frames.push(frame(100));
        playback.poll(&budget());
        // An account a frame fits, with its room already taken.
        let full = GraphicsBudget::independent(kettle_core::GraphicsLimits {
            retained_bytes: 8,
            image_bytes: 8,
            ..kettle_core::GraphicsLimits::default()
        })
        .expect("a small account");
        let _taken = full.reserve_retained_cpu(4).expect("half of it");
        script.borrow_mut().frames.push(frame(200));
        playback.poll(&full);
        assert_eq!(playback.position_ms(), 100);
        assert_eq!(script.borrow().recycled, 1, "the pixels went back");
        playback.keep();
        assert!(script.borrow().commands.contains(&"keep"));
    }
}
