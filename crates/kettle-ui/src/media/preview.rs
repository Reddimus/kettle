//! A lane's silent preview of a video: eight frames from across it, made by
//! the sandboxed worker as one stills sheet and shown in turn, never with
//! sound. Nothing starts one on its own: the user starts it from the lane,
//! or, when `video-preview-hover` is on and motion is allowed, the pointer
//! resting on the lane's picture loops it for a few seconds. It shows only
//! while its lane does, pauses while hidden, and ends when the lane closes,
//! shows another item, or the item changes.
//!
//! This is a sampled loop, not playback: a frame shows for its eighth of the
//! video, at most twelve a second and at least two, so a loop takes at most
//! four seconds. The frames come from one job under a two-second deadline;
//! until they do, the poster stays.

use std::time::{Duration, Instant};

use kettle_media::client::RenderControl;
use kettle_media::video::{edge_for_tiles, sheet_size, tile_origin, tile_size};
use kettle_media::{Rendered, StillsLayout, VideoInfo, VideoStills};

/// Frames a preview shows.
pub(crate) const FRAMES: u8 = 8;
/// How many frames the sheet lays side by side.
const COLUMNS: u8 = 4;
/// The longest side of a preview's frame, in pixels.
pub(crate) const FRAME_EDGE: u32 = 384;
/// How long a preview's frames may take, from the user's gesture.
pub(crate) const FIRST_FRAME_WITHIN: Duration = Duration::from_secs(2);
/// The quickest a frame follows the last: twelve a second.
pub(crate) const FASTEST_FRAME: Duration = Duration::from_nanos(1_000_000_000 / 12);
/// The slowest: two a second, so eight frames loop in four seconds.
pub(crate) const SLOWEST_FRAME: Duration = Duration::from_millis(500);
/// How long a hover loop plays before it stops by itself.
pub(crate) const HOVER_LOOP: Duration = Duration::from_secs(4);
/// How long the pointer rests on the picture before a hover loop starts.
pub(crate) const HOVER_DWELL: Duration = Duration::from_millis(400);
/// What a reply holds beside its sheet's pixels, at most: its header, the
/// video's description and the frames' times.
const REPLY_OVERHEAD: usize = 64 * 1024;

/// The stills job a preview of `info` asks for: eight frames from across the
/// video, laid four by two, each within [`FRAME_EDGE`] and never larger than
/// the picture. `None` for a video without length or picture.
pub(crate) fn stills(info: &VideoInfo) -> Option<VideoStills> {
    if info.duration_ms == 0 {
        return None;
    }
    let layout = VideoStills {
        count: FRAMES,
        max_edge: 0,
        start_s: 0.0,
        end_s: None,
        at_s: None,
        layout: StillsLayout::Sheet {
            cols: COLUMNS,
            labels: false,
        },
    };
    let max_edge = edge_for_tiles(&layout, info.width, info.height, FRAME_EDGE)?;
    let stills = VideoStills { max_edge, ..layout };
    stills.validate().ok()?;
    Some(stills)
}

/// The longest reply a preview job `stills` can send that [`Layout::of`]
/// would take: a sheet of frames [`FRAME_EDGE`] square, the largest it
/// allows whatever the worker finds the video to be, and what comes with it.
pub(crate) fn reply_limit(stills: &VideoStills) -> Option<usize> {
    let (width, height) = sheet_size(stills, (FRAME_EDGE, FRAME_EDGE));
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?
        .checked_add(REPLY_OVERHEAD)
}

/// How long each frame of a video `duration_ms` long shows: its eighth of
/// the video, between [`FASTEST_FRAME`] and [`SLOWEST_FRAME`].
pub(crate) fn interval(duration_ms: u64) -> Duration {
    Duration::from_millis(duration_ms / u64::from(FRAMES)).clamp(FASTEST_FRAME, SLOWEST_FRAME)
}

/// Where a preview's frames sit on the sheet a reply holds, and the moment
/// of the video each shows, checked against the job before its pixels are
/// taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    tile: (u32, u32),
    frames: Vec<((u32, u32), u64)>,
    duration_ms: u64,
}

impl Layout {
    /// The layout of `rendered`, a reply to the job `stills`: a sheet laid
    /// out as asked, a frame for each of its samples (one to [`FRAMES`]),
    /// each within [`FRAME_EDGE`]. `None` for any other reply.
    pub(crate) fn of(stills: &VideoStills, rendered: &Rendered) -> Option<Self> {
        let video = rendered.video.as_ref()?;
        let samples = &video.samples;
        if samples.is_empty() || samples.len() > usize::from(stills.count) {
            return None;
        }
        let tile = tile_size(video.info.width, video.info.height, stills)?;
        if tile.0 > FRAME_EDGE || tile.1 > FRAME_EDGE {
            return None;
        }
        let (width, height) = sheet_size(stills, tile);
        let bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        if (rendered.width, rendered.height) != (width, height)
            || bytes != Some(rendered.rgba.len())
        {
            return None;
        }
        let frames = samples
            .iter()
            .enumerate()
            .map(|(index, sample)| (tile_origin(stills, tile, index as u32), sample.actual_ms))
            .collect();
        Some(Self {
            tile,
            frames,
            duration_ms: video.info.duration_ms,
        })
    }
}

/// A preview's frames: the sheet, charged to the preview account, and where
/// each frame sits on it.
#[derive(Clone)]
pub(crate) struct Frames {
    sheet: kettle_core::ImageData,
    layout: Layout,
}

impl std::fmt::Debug for Frames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frames")
            .field("sheet", &(self.sheet.width, self.sheet.height))
            .field("layout", &self.layout)
            .finish()
    }
}

impl Frames {
    /// `sheet` cut as `layout` says; `None` when the sheet is not the size
    /// the layout's frames need.
    pub(crate) fn new(sheet: kettle_core::ImageData, layout: Layout) -> Option<Self> {
        let fits = layout.frames.iter().all(|&((x, y), _)| {
            x.checked_add(layout.tile.0)
                .is_some_and(|right| right <= sheet.width)
                && y.checked_add(layout.tile.1)
                    .is_some_and(|bottom| bottom <= sheet.height)
        });
        fits.then_some(Self { sheet, layout })
    }

    fn len(&self) -> usize {
        self.layout.frames.len()
    }
}

/// How a preview was started, which says how it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Start {
    /// The user's gesture: it plays until the user stops it.
    User,
    /// The pointer resting on the picture: it loops for [`HOVER_LOOP`], and
    /// stops when the pointer leaves or motion is no longer allowed.
    Hover,
}

/// What a lane's preview is doing.
#[derive(Debug)]
pub(crate) enum State {
    /// Its frames are being made by the job ticketed `ticket`, which must
    /// answer by `deadline`.
    Loading {
        ticket: u64,
        control: RenderControl,
        deadline: Instant,
        start: Start,
    },
    /// Its frames show in turn: `index` now, the next at `next` (`None`
    /// while the lane is hidden), and a hover loop ends at `until`.
    Playing {
        frames: Frames,
        start: Start,
        index: usize,
        interval: Duration,
        next: Option<Instant>,
        until: Option<Instant>,
    },
    /// Its frames, kept for the item while the poster shows.
    Held(Frames),
}

/// A lane's preview of its item as it was when the preview was asked for.
#[derive(Debug)]
pub(crate) struct LanePreview {
    pub item: u64,
    pub generation: u64,
    pub state: State,
}

/// What a tick did to a preview.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tick {
    /// What the lane shows changed.
    pub redraw: bool,
    /// Its frames did not come in time: the job is cancelled, and the
    /// preview should go.
    pub timed_out: bool,
}

/// What a lane can see of its preview's surroundings at a tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    /// The lane shows the item's picture.
    pub visible: bool,
    /// The pointer rests on that picture.
    pub hovered: bool,
    /// Motion Kettle starts on its own is allowed.
    pub motion: bool,
}

impl LanePreview {
    /// A preview whose frames the job ticketed `ticket` is making, under
    /// `control`, from `now`.
    pub(crate) fn loading(
        item: u64,
        generation: u64,
        ticket: u64,
        control: RenderControl,
        now: Instant,
        start: Start,
    ) -> Self {
        Self {
            item,
            generation,
            state: State::Loading {
                ticket,
                control,
                deadline: now + FIRST_FRAME_WITHIN,
                start,
            },
        }
    }

    /// Whether it is for `item` as it is now.
    pub(crate) fn serves(&self, item: u64, generation: u64) -> bool {
        (self.item, self.generation) == (item, generation)
    }

    /// Whether its frames are still being made at `now`, past their
    /// deadline: late frames count as none.
    pub(crate) fn overdue(&self, now: Instant) -> bool {
        matches!(self.state, State::Loading { deadline, .. } if now >= deadline)
    }

    /// The job ticket it waits on, while its frames are being made.
    pub(crate) fn waits_on(&self) -> Option<u64> {
        match self.state {
            State::Loading { ticket, .. } => Some(ticket),
            _ => None,
        }
    }

    /// Whether it is being made or plays, rather than held.
    pub(crate) fn active(&self) -> bool {
        !matches!(self.state, State::Held(_))
    }

    /// Show `frames`, made by the job ticketed `ticket`, from `now`; false,
    /// and nothing changes, when it no longer waits on that job.
    pub(crate) fn ready(&mut self, ticket: u64, frames: Frames, now: Instant) -> bool {
        let State::Loading {
            ticket: waited,
            start,
            deadline,
            ..
        } = self.state
        else {
            return false;
        };
        if waited != ticket || now >= deadline {
            return false;
        }
        self.state = playing(frames, start, now);
        true
    }

    /// Play held frames again from `now`; false when there are none.
    pub(crate) fn replay(&mut self, now: Instant, start: Start) -> bool {
        let State::Held(frames) = &self.state else {
            return false;
        };
        self.state = playing(frames.clone(), start, now);
        true
    }

    /// Stop: a job still making frames is cancelled, and frames shown are
    /// held. Returns whether frames are held, to keep it.
    pub(crate) fn stop(&mut self) -> bool {
        match &self.state {
            State::Loading { control, .. } => {
                control.cancel();
                false
            }
            State::Playing { frames, .. } => {
                self.state = State::Held(frames.clone());
                true
            }
            State::Held(_) => true,
        }
    }

    /// Move on to `now`: a job past its deadline times out; frames pause
    /// while the lane cannot show them and go on, one a step, at most one a
    /// frame's interval, when it can; a hover loop ends when its time is
    /// up, the pointer leaves or motion is no longer allowed.
    pub(crate) fn tick(&mut self, now: Instant, seen: Seen) -> Tick {
        if let State::Playing {
            frames,
            start: Start::Hover,
            until,
            ..
        } = &self.state
            && (until.is_some_and(|until| now >= until) || !seen.hovered || !seen.motion)
        {
            let held = frames.clone();
            self.state = State::Held(held);
            return Tick {
                redraw: true,
                ..Tick::default()
            };
        }
        match &mut self.state {
            State::Loading {
                control, deadline, ..
            } => {
                if now >= *deadline {
                    control.cancel();
                    return Tick {
                        timed_out: true,
                        ..Tick::default()
                    };
                }
                Tick::default()
            }
            State::Held(_) => Tick::default(),
            State::Playing {
                frames,
                index,
                interval,
                next,
                ..
            } => {
                if !seen.visible {
                    *next = None;
                    return Tick::default();
                }
                match *next {
                    None => {
                        *next = Some(now + *interval);
                        Tick::default()
                    }
                    Some(due) if now >= due => {
                        *index = (*index + 1) % frames.len().max(1);
                        // No catching up: the next frame is an interval
                        // from now, whatever was missed.
                        *next = Some(now + *interval);
                        Tick {
                            redraw: true,
                            ..Tick::default()
                        }
                    }
                    Some(_) => Tick::default(),
                }
            }
        }
    }

    /// When it next needs a tick: a job's deadline, the next frame or a
    /// hover loop's end; `None` while held or paused.
    pub(crate) fn wake(&self) -> Option<Instant> {
        match &self.state {
            State::Loading { deadline, .. } => Some(*deadline),
            State::Playing { next, until, .. } => match (*next, *until) {
                (Some(next), Some(until)) => Some(next.min(until)),
                (next, None) => next,
                (None, Some(until)) => Some(until),
            },
            State::Held(_) => None,
        }
    }

    /// The frame showing, while it plays.
    pub(crate) fn frame(&self) -> Option<ShownFrame<'_>> {
        let State::Playing { frames, index, .. } = &self.state else {
            return None;
        };
        let &((x, y), at_ms) = frames.layout.frames.get(*index)?;
        let (width, height) = frames.layout.tile;
        Some(ShownFrame {
            sheet: &frames.sheet,
            source: (x, y, width, height),
            at_ms,
            length_ms: frames.layout.duration_ms,
        })
    }
}

impl Drop for LanePreview {
    /// A preview let go while its frames are made cancels their job, however
    /// it goes: with its lane, its pane, or its window.
    fn drop(&mut self) {
        if let State::Loading { control, .. } = &self.state {
            control.cancel();
        }
    }
}

/// The frame a playing preview shows: its sheet, its place on it (x, y,
/// width, height), the moment of the video it shows and the video's length,
/// both in milliseconds.
pub(crate) struct ShownFrame<'a> {
    pub sheet: &'a kettle_core::ImageData,
    pub source: (u32, u32, u32, u32),
    pub at_ms: u64,
    pub length_ms: u64,
}

/// Frames playing from their first, `start`ed at `now`.
fn playing(frames: Frames, start: Start, now: Instant) -> State {
    let interval = interval(frames.layout.duration_ms);
    State::Playing {
        frames,
        start,
        index: 0,
        interval,
        next: Some(now + interval),
        until: (start == Start::Hover).then_some(now + HOVER_LOOP),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_media::video::VideoContainer;
    use kettle_media::{StillSample, VideoCodec, VideoStillsResult};

    fn info(width: u32, height: u32, duration_ms: u64) -> VideoInfo {
        VideoInfo {
            duration_ms,
            width,
            height,
            rotation: 0,
            codec: VideoCodec::H264,
            fps_milli: Some(30_000),
            has_audio: false,
            container: Some(VideoContainer::IsoBmff),
        }
    }

    /// A reply to `stills` for `info`, with `samples` frames, the sheet
    /// `width` by `height`.
    fn reply(info: VideoInfo, samples: usize, width: u32, height: u32) -> Rendered {
        let whole = kettle_media::Crop {
            x: 0,
            y: 0,
            width,
            height,
        };
        Rendered {
            width,
            height,
            rgba: vec![0; width as usize * height as usize * 4],
            digest: kettle_media::Digest {
                sha256: [0; 32],
                path_identity: None,
            },
            layout: kettle_media::RenderLayout {
                source_width: f64::from(width),
                source_height: f64::from(height),
                image_in_target: whole,
                result_in_target: whole,
            },
            exact_source: None,
            source_text: Vec::new(),
            fence_sources: Vec::new(),
            fence_count: 0,
            fence_index: None,
            uncovered_scripts: Vec::new(),
            warnings: Vec::new(),
            video: Some(VideoStillsResult {
                info,
                samples: (0..samples as u64)
                    .map(|index| StillSample {
                        requested_ms: index * 1000 + 500,
                        actual_ms: index * 1000 + 480,
                    })
                    .collect(),
                tolerance_ms: 40,
            }),
        }
    }

    /// A video's preview job asks for eight frames from across it, four by
    /// two, each fitted within the frame edge, never past the picture; one
    /// without length asks for nothing.
    #[test]
    fn a_preview_asks_for_eight_frames_within_the_edge() {
        let landscape = stills(&info(1920, 1080, 9000)).unwrap();
        assert_eq!(landscape.count, 8);
        assert_eq!(
            landscape.layout,
            StillsLayout::Sheet {
                cols: 4,
                labels: false
            }
        );
        assert_eq!(
            (landscape.start_s, landscape.end_s, landscape.at_s),
            (0.0, None, None)
        );
        assert_eq!(tile_size(1920, 1080, &landscape), Some((384, 216)));
        let portrait = stills(&info(1080, 1920, 9000)).unwrap();
        assert_eq!(tile_size(1080, 1920, &portrait), Some((216, 384)));
        let small = stills(&info(160, 90, 9000)).unwrap();
        assert_eq!(tile_size(160, 90, &small), Some((160, 90)));
        assert_eq!(stills(&info(1920, 1080, 0)), None);
        assert_eq!(stills(&info(0, 1080, 9000)), None);
        // The reply limit holds the largest sheet a reply may be, whatever
        // the worker finds the video to be, and a little.
        let limit = reply_limit(&landscape).unwrap();
        assert_eq!(limit, 1556 * 780 * 4 + REPLY_OVERHEAD);
        // A worker that finds the video a little taller than the poster did.
        let tile = tile_size(1920, 1120, &landscape).unwrap();
        let (width, height) = sheet_size(&landscape, tile);
        assert!(width as usize * height as usize * 4 <= limit, "{tile:?}");
    }

    /// A frame shows for its eighth of the video, never quicker than twelve
    /// a second or slower than two.
    #[test]
    fn frames_show_for_their_share_within_the_rates() {
        assert_eq!(interval(2000), Duration::from_millis(250));
        assert_eq!(interval(100), FASTEST_FRAME);
        assert_eq!(interval(0), FASTEST_FRAME);
        assert_eq!(interval(60_000), SLOWEST_FRAME);
        assert!(FASTEST_FRAME >= Duration::from_secs(1) / 12);
        assert!(SLOWEST_FRAME * u32::from(FRAMES) <= Duration::from_secs(4));
    }

    /// Only a sheet laid out as asked, with one to eight samples, each frame
    /// within the edge, is a preview's.
    #[test]
    fn only_a_sheet_laid_out_as_asked_is_a_preview() {
        let video = info(1920, 1080, 9000);
        let asked = stills(&video).unwrap();
        let layout = Layout::of(&asked, &reply(video, 8, 1556, 444)).unwrap();
        assert_eq!(layout.tile, (384, 216));
        assert_eq!(layout.frames[0], ((4, 4), 480));
        assert_eq!(layout.frames[7], ((4 + 3 * 388, 4 + 220), 7480));
        assert!(Layout::of(&asked, &reply(video, 3, 1556, 444)).is_some());
        for wrong in [
            reply(video, 9, 1556, 444),
            reply(video, 0, 1556, 444),
            reply(video, 8, 1556, 445),
            reply(video, 8, 1557, 444),
            // As many pixels, another shape.
            reply(video, 8, 444, 1556),
            reply(video, 8, 778, 888),
            reply(info(1000, 1000, 9000), 8, 1556, 444),
        ] {
            assert_eq!(Layout::of(&asked, &wrong), None);
        }
        let mut short = reply(video, 8, 1556, 444);
        short.rgba.pop();
        assert_eq!(Layout::of(&asked, &short), None);
        let mut silent = reply(video, 8, 1556, 444);
        silent.video = None;
        assert_eq!(Layout::of(&asked, &silent), None);
        // A frame past the edge, however it was asked for.
        let wide = VideoStills {
            max_edge: 4096,
            ..asked
        };
        let big = tile_size(1920, 1080, &wide).unwrap();
        let (width, height) = sheet_size(&wide, big);
        assert_eq!(Layout::of(&wide, &reply(video, 8, width, height)), None);
    }

    fn frames(count: usize) -> Frames {
        let video = info(640, 360, 4000);
        let asked = stills(&video).unwrap();
        let tile = tile_size(640, 360, &asked).unwrap();
        let (width, height) = sheet_size(&asked, tile);
        let rendered = reply(video, count, width, height);
        let layout = Layout::of(&asked, &rendered).unwrap();
        let sheet = kettle_core::ImageData::new(width, height, rendered.rgba).unwrap();
        Frames::new(sheet, layout).unwrap()
    }

    const SHOWN: Seen = Seen {
        visible: true,
        hovered: false,
        motion: true,
    };

    /// Frames go on one a step, an interval apart, never catching up on
    /// what was missed, and loop.
    #[test]
    fn frames_step_an_interval_apart_without_catching_up() {
        let now = Instant::now();
        let mut preview = LanePreview::loading(1, 2, 3, RenderControl::default(), now, Start::User);
        assert!(preview.ready(3, frames(8), now));
        let step = interval(4000);
        assert_eq!(preview.wake(), Some(now + step));
        assert_eq!(preview.tick(now + step / 2, SHOWN), Tick::default());
        assert_eq!(preview.frame().unwrap().at_ms, 480);
        let late = now + step * 5;
        assert!(preview.tick(late, SHOWN).redraw);
        assert_eq!(preview.frame().unwrap().at_ms, 1480, "one step, not five");
        assert_eq!(preview.wake(), Some(late + step));
        let mut at = late;
        for _ in 0..7 {
            at += step;
            assert!(preview.tick(at, SHOWN).redraw);
        }
        assert_eq!(preview.frame().unwrap().at_ms, 480, "it loops");
    }

    /// While the lane cannot show them, frames pause and need no tick; shown
    /// again they wait an interval before the next.
    #[test]
    fn hidden_frames_pause() {
        let now = Instant::now();
        let mut preview = LanePreview::loading(1, 2, 3, RenderControl::default(), now, Start::User);
        preview.ready(3, frames(8), now);
        let hidden = Seen {
            visible: false,
            ..SHOWN
        };
        let later = now + Duration::from_secs(10);
        assert_eq!(preview.tick(later, hidden), Tick::default());
        assert_eq!(preview.wake(), None);
        assert_eq!(preview.frame().unwrap().at_ms, 480);
        assert_eq!(preview.tick(later, SHOWN), Tick::default());
        assert_eq!(preview.wake(), Some(later + interval(4000)));
    }

    /// Frames not made by the deadline time out and cancel their job; frames
    /// for another ticket are not taken.
    #[test]
    fn a_preview_not_ready_in_time_times_out() {
        let now = Instant::now();
        let control = RenderControl::default();
        let mut preview = LanePreview::loading(1, 2, 3, control.clone(), now, Start::User);
        assert_eq!(preview.waits_on(), Some(3));
        assert_eq!(preview.wake(), Some(now + FIRST_FRAME_WITHIN));
        assert!(!preview.ready(4, frames(8), now));
        let before = now + FIRST_FRAME_WITHIN - Duration::from_millis(1);
        assert!(!preview.tick(before, SHOWN).timed_out);
        assert!(!control.is_cancelled());
        assert!(preview.tick(now + FIRST_FRAME_WITHIN, SHOWN).timed_out);
        assert!(control.is_cancelled());
    }

    /// Frames that come after their deadline are not taken, and a preview
    /// let go while its frames are made cancels their job.
    #[test]
    fn late_frames_are_refused_and_a_dropped_preview_cancels() {
        let now = Instant::now();
        let mut late = LanePreview::loading(1, 2, 3, RenderControl::default(), now, Start::User);
        let deadline = now + FIRST_FRAME_WITHIN;
        assert!(!late.overdue(deadline - Duration::from_millis(1)));
        assert!(late.overdue(deadline));
        assert!(!late.ready(3, frames(8), deadline));
        assert!(late.active() && late.frame().is_none());
        let control = RenderControl::default();
        drop(LanePreview::loading(
            1,
            2,
            3,
            control.clone(),
            now,
            Start::User,
        ));
        assert!(control.is_cancelled());
        let shown = RenderControl::default();
        let mut playing = LanePreview::loading(1, 2, 3, shown.clone(), now, Start::User);
        playing.ready(3, frames(8), now);
        drop(playing);
        assert!(!shown.is_cancelled(), "only a job still making frames");
    }

    /// Stopping cancels a job making frames and holds frames shown, which
    /// play again from the first.
    #[test]
    fn stopping_cancels_or_holds() {
        let now = Instant::now();
        let control = RenderControl::default();
        let mut loading = LanePreview::loading(1, 2, 3, control.clone(), now, Start::User);
        assert!(!loading.stop());
        assert!(control.is_cancelled());
        let mut playing = LanePreview::loading(1, 2, 3, RenderControl::default(), now, Start::User);
        playing.ready(3, frames(8), now);
        playing.tick(now + Duration::from_secs(1), SHOWN);
        assert!(playing.stop());
        assert!(!playing.active());
        assert!(playing.frame().is_none());
        assert_eq!(playing.wake(), None);
        let later = now + Duration::from_secs(5);
        assert!(playing.replay(later, Start::User));
        assert_eq!(playing.frame().unwrap().at_ms, 480);
        assert!(
            !LanePreview::loading(1, 2, 3, RenderControl::default(), now, Start::User)
                .replay(later, Start::User)
        );
    }

    /// A hover loop ends after its few seconds, when the pointer leaves, or
    /// when motion is no longer allowed, keeping its frames; a preview the
    /// user started plays on.
    #[test]
    fn a_hover_loop_ends_by_itself() {
        let now = Instant::now();
        let hovered = Seen {
            hovered: true,
            ..SHOWN
        };
        let hover = || {
            let mut preview =
                LanePreview::loading(1, 2, 3, RenderControl::default(), now, Start::Hover);
            preview.ready(3, frames(8), now);
            preview
        };
        let mut timed = hover();
        assert_eq!(timed.wake(), Some(now + interval(4000)));
        assert!(!timed.tick(now + Duration::from_millis(100), hovered).redraw);
        assert!(timed.active());
        assert!(timed.tick(now + HOVER_LOOP, hovered).redraw);
        assert!(!timed.active());
        let mut left = hover();
        left.tick(now + Duration::from_millis(100), SHOWN);
        assert!(!left.active());
        let mut still = hover();
        still.tick(
            now + Duration::from_millis(100),
            Seen {
                motion: false,
                ..hovered
            },
        );
        assert!(!still.active());
        let mut user = LanePreview::loading(1, 2, 3, RenderControl::default(), now, Start::User);
        user.ready(3, frames(8), now);
        user.tick(now + HOVER_LOOP * 3, SHOWN);
        assert!(user.active());
    }

    /// Frames are cut only from a sheet large enough for them all.
    #[test]
    fn frames_need_a_sheet_that_holds_them() {
        let video = info(640, 360, 4000);
        let asked = stills(&video).unwrap();
        let tile = tile_size(640, 360, &asked).unwrap();
        let (width, height) = sheet_size(&asked, tile);
        let layout = Layout::of(&asked, &reply(video, 8, width, height)).unwrap();
        // The margin past the last frame may go; a pixel of the frame not.
        let sheet = |width: u32| {
            kettle_core::ImageData::new(width, height, vec![0; (width * height * 4) as usize])
                .unwrap()
        };
        assert!(Frames::new(sheet(width - 4), layout.clone()).is_some());
        assert!(Frames::new(sheet(width - 5), layout).is_none());
    }
}
