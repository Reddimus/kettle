//! A pane's preview lane: one shelf item, shown in the lane the user opened
//! beside the pane's terminal. A header names the item and offers previous,
//! next, open outside, collapse and close; below it, when the lane is
//! expanded, the item's kind and sender, with a gallery's page before and
//! after it on the same line, the image fitted on its canvas and
//! still images never scaled past their own pixels until the user zooms,
//! videos fitted to the available room, and a footer
//! saying where keys go. A
//! collapsed lane is its header alone. The renderer gets only display text
//! and pixels, never a path or a source.

use glyphon::cosmic_text::{FeatureTag, FontFeatures, Wrap};
use glyphon::{
    Attrs, Buffer as TextBuffer, Color as GColor, Family, FontSystem, Metrics, Shaping, TextArea,
    Weight,
};

use crate::Rect4;

/// Cover the window outside its viewer, after terminal and lane images.
pub(crate) fn viewer_backdrop(viewer: Rect4, surface: (f32, f32)) -> Option<[Rect4; 4]> {
    let (x, y, width, height) = viewer;
    let (sw, sh) = surface;
    let right = x + width;
    let bottom = y + height;
    if ![x, y, width, height, sw, sh, right, bottom]
        .into_iter()
        .all(f32::is_finite)
        || x < 0.0
        || y < 0.0
        || width <= 0.0
        || height <= 0.0
        || sw <= 0.0
        || sh <= 0.0
        || right > sw
        || bottom > sh
    {
        return None;
    }
    Some([
        (0.0, 0.0, sw, y),
        (0.0, y, x, height),
        (right, y, sw - right, height),
        (0.0, bottom, sw, sh - bottom),
    ])
}

/// What shows behind the image's transparent pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCanvas {
    /// The pane's background.
    Theme,
    /// White, as SVG documents expect.
    White,
    /// A checkerboard, so a raster's transparency shows as such.
    Checker,
}

/// What a lane's content area shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaLaneMode {
    /// The rendered item.
    #[default]
    Rendered,
    /// The item's source text, a row per line.
    Source,
}

/// The rows of an item's source a lane shows: display-ready (no control,
/// format or bidirectional characters, tabs expanded), only those in view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaLaneSource {
    /// The rows in view, from `first_row`, each from `first_column`.
    pub rows: Vec<String>,
    pub first_row: usize,
    pub total_rows: usize,
}

/// The edge of an expanded lane that borders its terminal, which a drag
/// moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaLaneEdge {
    /// A lane along the bottom of its pane.
    Top,
    /// A lane along the right of its pane.
    Left,
}

/// Sharper pixels for part of a zoomed item, and the part they cover as
/// fractions of the image's width and height: left, top, right, bottom.
#[derive(Clone)]
pub struct MediaLaneTile {
    pub image: kettle_core::ImageData,
    pub coverage: (f64, f64, f64, f64),
}

/// The play button a video's lane offers: play while the poster or a paused
/// frame shows, pause while the video plays, and stop while a silent
/// preview is made or plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaLanePlay {
    Play,
    Pause,
    Stop,
}

/// The sound button a video's lane offers once its sound can play: the
/// sound heard, or muted. A video with no sound, or whose sound cannot play
/// here, has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaLaneSound {
    Heard,
    Muted,
}

/// Where a playing video is, for the bar under its picture: its place and
/// length in ms, and the time shown beside the bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaLaneScrubber {
    /// Metadata alone can reserve the time row before a session can seek.
    pub seekable: bool,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub label: String,
}

/// A frame of a silent preview or of the video playing, shown in the item's
/// place: part of `sheet` (a preview's sheet, or the whole of a played
/// frame), in its pixels (x, y, width, height), drawn where an image `size`
/// large would be.
#[derive(Clone)]
pub struct MediaLaneFrame {
    pub sheet: kettle_core::ImageData,
    pub source: (u32, u32, u32, u32),
    pub size: (u32, u32),
}

impl std::fmt::Debug for MediaLaneFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaLaneFrame")
            .field("sheet", &(self.sheet.width, self.sheet.height))
            .field("source", &self.source)
            .field("size", &self.size)
            .finish()
    }
}

impl std::fmt::Debug for MediaLaneTile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaLaneTile")
            .field("size", &(self.image.width, self.image.height))
            .field("coverage", &self.coverage)
            .finish()
    }
}

/// One pane's lane, projected for painting.
#[derive(Clone, Debug)]
pub struct MediaLanePanel {
    /// A centered viewer's header action, localized; lanes use collapse.
    pub pin_label: Option<String>,
    pub focused_control: Option<Rect4>,
    /// The pane the lane belongs to, which keys its retained text.
    pub pane: u64,
    /// The lane, in physical pixels: its whole share of the pane.
    pub rect: Rect4,
    /// Collapsed to its header, by the user or for want of room.
    pub collapsed: bool,
    /// Display-ready (sanitized, bounded) title.
    pub title: String,
    /// The kind and size, in the UI language.
    pub detail: String,
    /// Who sent it, on a line of its own.
    pub sender: MediaLaneSender,
    /// Where keys go while the lane shows, in the UI language.
    pub hint: String,
    /// The item's place on its shelf, 1-based, and the shelf's length.
    pub position: (usize, usize),
    /// For a page of a gallery or document: the page shown, 1-based, and how
    /// many there are. The detail line offers the others when there is more
    /// than one.
    pub page: Option<(usize, usize)>,
    /// Whether those are a document's pages rather than a gallery's
    /// diagrams: what paging is called.
    pub document: bool,
    /// The pixels, or `None` when they were released.
    pub image: Option<kettle_core::ImageData>,
    /// Shown in the image's place when there are no pixels.
    pub status: String,
    pub canvas: MediaCanvas,
    /// Whether the header offers to open the item in the image viewer or
    /// video player this platform permits.
    pub open_outside: bool,
    /// Rendered or source.
    pub mode: MediaLaneMode,
    /// The item's source rows in view, when it is text the lane still holds:
    /// then the header offers to switch modes.
    pub source: Option<MediaLaneSource>,
    /// Whether there is something to copy in the current mode.
    pub copy: bool,
    /// Whether the item came from a file the header offers to read again.
    pub reload: bool,
    /// What the lane last did, shown in place of the hint until it changes.
    pub notice: Option<String>,
    /// How the rendered item is zoomed and panned.
    pub view: crate::MediaViewport,
    /// Sharper pixels for the part of the zoomed item in view, when the
    /// lane holds some.
    pub tile: Option<MediaLaneTile>,
    /// The edge a drag resizes the lane by, while it is expanded.
    pub edge: Option<MediaLaneEdge>,
    /// The silent-preview button the header offers, for a video.
    pub play: Option<MediaLanePlay>,
    /// A central play affordance or brief playback feedback; no decoding state.
    pub play_feedback: Option<MediaLanePlay>,
    /// The sound button beside it, while the video's sound can play.
    pub sound: Option<MediaLaneSound>,
    /// While a silent preview or the video plays, the frame shown instead
    /// of the item's pixels: fitted where they would be, and never zoomed.
    pub frame: Option<MediaLaneFrame>,
    /// While the video plays, or is paused in place, the bar under its
    /// picture.
    pub scrubber: Option<MediaLaneScrubber>,
}

/// Who sent an item, in the UI language, with where the sending program's
/// path and its signer sit in that line: the path is what gets shortened, and
/// the signer is kept whole.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaLaneSender {
    pub text: String,
    /// The byte range of the program's path in `text`, when it names one.
    pub program: Option<std::ops::Range<usize>>,
    /// The byte range of who signed the program, when it names them.
    pub signer: Option<std::ops::Range<usize>>,
}

impl MediaLaneSender {
    /// The line fitted to `columns`. A path that does not fit is shortened
    /// from its middle, keeping its last segment; when even that leaves the
    /// line too long, the text around the signer is shortened instead, so who
    /// signed the program stays readable whenever it fits at all.
    pub fn fitted(&self, columns: usize) -> String {
        use crate::{display_width, middle_ellipsis};
        let text = self.text.as_str();
        if display_width(text) <= columns {
            return text.to_owned();
        }
        let valid = |range: &Option<std::ops::Range<usize>>| {
            range
                .clone()
                .filter(|range| text.get(range.clone()).is_some())
        };
        let mut signer = valid(&self.signer);
        // A path that overlaps the signer is not shortened apart from it: the
        // signer is what must stay readable.
        let program = valid(&self.program).filter(|program| {
            signer
                .as_ref()
                .is_none_or(|signer| signer.end <= program.start || program.end <= signer.start)
        });
        let mut line = text.to_owned();
        if let Some(program) = program {
            let path = &text[program.clone()];
            let rest = display_width(text) - display_width(path);
            let shortened = middle_ellipsis(path, columns.saturating_sub(rest).max(1));
            line.replace_range(program.clone(), &shortened);
            if display_width(&line) <= columns {
                return line;
            }
            // A signer after the path starts where the shortened path ends
            // plus whatever lay between them.
            signer = signer.map(|signer| {
                if program.end <= signer.start {
                    let start = program.start + shortened.len() + (signer.start - program.end);
                    start..start + signer.len()
                } else {
                    signer
                }
            });
        }
        // A range that no longer lands on the shortened line, as one that
        // overlapped the path may not, keeps nothing apart.
        if let Some(signer) = signer.filter(|signer| line.get(signer.clone()).is_some()) {
            let kept = &line[signer.clone()];
            let width = display_width(kept);
            if width <= columns {
                let (before, after) = (&line[..signer.start], &line[signer.end..]);
                let room = columns - width;
                let after_room = display_width(after).min(room / 2);
                let before = middle_ellipsis(before, room - after_room);
                let after = middle_ellipsis(after, after_room);
                return format!("{before}{kept}{after}");
            }
        }
        middle_ellipsis(&line, columns)
    }
}

/// Exact paint and input geometry of a lane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaLaneGeometry {
    pub pinning: bool,
    /// The lane.
    pub rect: Rect4,
    pub title: Rect4,
    pub counter: Rect4,
    pub previous: Option<Rect4>,
    pub next: Option<Rect4>,
    /// Opens the item in the permitted image viewer, when it is offered.
    pub open_outside: Option<Rect4>,
    /// Switches between the rendered item and its source, when it has one.
    pub mode: Option<Rect4>,
    /// Shows the item on the next canvas.
    pub canvas: Option<Rect4>,
    /// Copies the image, or the source in source mode.
    pub copy: Option<Rect4>,
    /// Reads the item's file again.
    pub reload: Option<Rect4>,
    /// Plays or pauses a video, or starts or stops its silent preview.
    pub play: Option<Rect4>,
    pub theater: Option<Rect4>,
    pub fullscreen: Option<Rect4>,
    /// The picture itself toggles playback for video only.
    pub video: bool,
    /// Decorative central affordance/feedback, clipped to the picture.
    pub play_feedback: Option<(Rect4, MediaLanePlay)>,
    /// Mutes the video's sound, or lets it be heard again.
    pub sound: Option<Rect4>,
    /// The bar under a playing video's picture, which a press or drag
    /// moves the video along, and the time beside it.
    pub scrubber: Option<Rect4>,
    pub scrub_label: Option<Rect4>,
    /// Dedicated video controls below the picture; absent in compact headers.
    pub transport: Option<Rect4>,
    pub transport_timeline: Option<Rect4>,
    pub transport_text_scale: f32,
    /// Zoom the rendered item out, in, and back to its fit.
    pub zoom_out: Option<Rect4>,
    pub zoom_in: Option<Rect4>,
    pub zoom_fit: Option<Rect4>,
    /// The strip along the edge that resizes the lane: its padding there,
    /// clear of every control and of the content.
    pub resize: Option<Rect4>,
    /// Collapses an expanded lane to its header, or expands a collapsed one.
    pub toggle: Rect4,
    pub close: Rect4,
    /// Whether the lane shows more than its header: the rows below are
    /// empty rectangles when it does not.
    pub full: bool,
    pub detail: Rect4,
    /// A gallery's controls at the end of the detail line, in an expanded
    /// lane with room for them: the page before, which page it is, and the
    /// page after.
    pub page_previous: Option<Rect4>,
    pub page_counter: Option<Rect4>,
    pub page_next: Option<Rect4>,
    pub sender: Rect4,
    pub hint: Rect4,
    /// Where the image area is: the fitted image, or the whole area when
    /// there is no image.
    pub image_area: Rect4,
    /// The image area in whole pixels: what clips the image, and what a
    /// zoomed image keeps covered.
    pub content: Rect4,
    /// The image fitted in `content`, when there are pixels and the lane
    /// shows them.
    pub fit: Option<Rect4>,
    /// Where the image is drawn: `fit` zoomed and panned, reaching past
    /// `content` when zoomed in.
    pub image: Option<Rect4>,
    /// Where the sharper pixels are drawn instead, when they cover all of
    /// the image in view: one image a lane, either way.
    pub tile: Option<Rect4>,
    /// How many source rows fit the content area, in source mode.
    pub source_rows: usize,
}

/// What a pointer press at a point in a lane means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaLaneHit {
    Theater,
    Fullscreen,
    Close,
    Previous,
    Next,
    /// A gallery's page before the one shown, or after it.
    PreviousPage,
    NextPage,
    OpenOutside,
    /// Collapse or expand.
    Toggle,
    /// Move the existing viewer into a lane.
    Pin,
    /// Rendered or source.
    Mode,
    /// The next canvas.
    Canvas,
    Copy,
    /// Read the item's file again.
    Reload,
    /// Play or pause a video, or start or stop its silent preview.
    Play,
    /// Mute the video's sound, or let it be heard.
    Sound,
    /// The bar under a playing video, for a press or drag along it.
    Scrub,
    ZoomOut,
    ZoomIn,
    /// Back to the fit.
    Fit,
    /// The edge that resizes the lane, for a drag.
    Resize,
    /// Anywhere else in the lane: nothing happens, and nothing reaches the
    /// terminal.
    Inside,
}

fn contains(rect: Rect4, x: f32, y: f32) -> bool {
    x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
}

impl MediaLaneGeometry {
    /// The part of the image that shows: where it is drawn, clipped to the
    /// content. `None` without an image, or with none of it in view.
    pub fn shown(&self) -> Option<Rect4> {
        let image = self.image?;
        let content = self.content;
        let left = image.0.max(content.0);
        let top = image.1.max(content.1);
        let right = (image.0 + image.2).min(content.0 + content.2);
        let bottom = (image.1 + image.3).min(content.1 + content.3);
        (right > left && bottom > top).then_some((left, top, right - left, bottom - top))
    }

    /// The part of the image in view, as fractions of its width and height:
    /// left, top, right and bottom.
    pub fn visible(&self) -> Option<(f64, f64, f64, f64)> {
        let (image, shown) = (self.image?, self.shown()?);
        let along =
            |at: f32, from: f32, length: f32| (f64::from(at) - f64::from(from)) / f64::from(length);
        Some((
            along(shown.0, image.0, image.2),
            along(shown.1, image.1, image.3),
            along(shown.0 + shown.2, image.0, image.2),
            along(shown.1 + shown.3, image.1, image.3),
        ))
    }

    /// How far along the scrubber `x` is, from 0 at its start to 1 at its
    /// end, held to it: where a press or drag there moves the video.
    pub fn scrub_at(&self, x: f32) -> Option<f64> {
        let bar = self.scrubber?;
        (bar.2 > 0.0)
            .then(|| ((f64::from(x) - f64::from(bar.0)) / f64::from(bar.2)).clamp(0.0, 1.0))
    }

    /// What a press at `(x, y)` does; `None` outside the lane.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<MediaLaneHit> {
        if !contains(self.rect, x, y) {
            None
        } else if contains(self.close, x, y) {
            Some(MediaLaneHit::Close)
        } else if self.resize.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Resize)
        } else if contains(self.toggle, x, y) {
            Some(if self.pinning {
                MediaLaneHit::Pin
            } else {
                MediaLaneHit::Toggle
            })
        } else if self.play.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Play)
        } else if self.theater.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Theater)
        } else if self.fullscreen.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Fullscreen)
        } else if self.sound.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Sound)
        } else if self.scrubber.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Scrub)
        } else if self.reload.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Reload)
        } else if self.mode.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Mode)
        } else if self.canvas.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Canvas)
        } else if self.copy.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Copy)
        } else if self.zoom_fit.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Fit)
        } else if self.zoom_in.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::ZoomIn)
        } else if self.zoom_out.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::ZoomOut)
        } else if self.open_outside.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::OpenOutside)
        } else if self.previous.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Previous)
        } else if self.next.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Next)
        } else if self.page_previous.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::PreviousPage)
        } else if self.page_next.is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::NextPage)
        } else if self.video && self.shown().is_some_and(|rect| contains(rect, x, y)) {
            Some(MediaLaneHit::Play)
        } else {
            Some(MediaLaneHit::Inside)
        }
    }
}

/// The counter's text: the item's place and the shelf's length.
pub fn media_lane_counter(position: (usize, usize)) -> String {
    format!("{}/{}", position.0, position.1)
}

/// The fewest overlay columns a header's title keeps before a control gives
/// way; the collapse and close buttons never do.
const MIN_TITLE_COLUMNS: f32 = 4.0;
/// The fewest columns a playing video's bar keeps beside its time; with
/// fewer the lane shows no bar.
const MIN_SCRUBBER_COLUMNS: f32 = 4.0;
/// Use letterboxing for a useful drag target before falling back to the minimum.
#[cfg(test)]
const PREFERRED_SCRUBBER_COLUMNS: f32 = 24.0;

/// Where a bar's knob `knob` wide sits on `track` with `played` pixels of it
/// played: centered on where the video is, kept on the track, and at its
/// start when the track is narrower than the knob.
pub(crate) fn knob_left(track: Rect4, played: f32, knob: f32) -> f32 {
    (track.0 + played - knob / 2.0)
        .min(track.0 + track.2 - knob)
        .max(track.0)
}

/// Lay a lane out in its rectangle. `cell` is the terminal cell, and
/// `text_cell` the overlay text's column width and line height. An expanded
/// lane with room for its rows shows them all; otherwise it is its header
/// alone, centered down a strip. `None` when even the header does not fit.
pub fn media_lane_geometry(
    lane: &MediaLanePanel,
    cell: (f32, f32),
    text_cell: (f32, f32),
) -> Option<MediaLaneGeometry> {
    media_lane_geometry_scaled(lane, cell, text_cell, 1.0)
}

/// Physical geometry with logical pointer-target sizes scaled exactly once.
pub fn media_lane_geometry_scaled(
    lane: &MediaLanePanel,
    cell: (f32, f32),
    text_cell: (f32, f32),
    scale: f32,
) -> Option<MediaLaneGeometry> {
    let finite = |value: f32| value.is_finite() && value > 0.0;
    let (cw, ch) = cell;
    let (tw, lh) = text_cell;
    if !(finite(cw) && finite(ch) && finite(tw) && finite(lh) && finite(scale)) {
        return None;
    }
    let rect = lane.rect;
    let pad = (tw * 0.75).round().max(4.0);
    let button = 3.0 * tw;
    let toggle_width = lane.pin_label.as_ref().map_or(button, |label| {
        (crate::display_width(label).min(16) as f32 + 2.0).max(3.0) * tw
    });
    if rect.2 < button + toggle_width + MIN_TITLE_COLUMNS * tw + 2.0 * pad || rect.3 < lh {
        return None;
    }
    // Header, detail, sender, a line of image and the hint, with padding
    // above, between the header block and the image, and below.
    let full = !lane.collapsed && rect.3 >= 5.0 * lh + 4.0 * pad;
    let left = rect.0 + pad;
    let right = rect.0 + rect.2 - pad;
    let header_y = if full {
        rect.1 + pad
    } else {
        (rect.1 + (rect.3 - lh) / 2.0).floor()
    };
    // Controls take the header from the right, close first; each one past
    // collapse appears only while the title keeps its few columns, in order
    // of need: a video's play, its sound, reload, the mode, the canvas, browsing,
    // copy, fit, zoom in, zoom out, open outside, then the counter. One that
    // does not fit leaves the rest to try.
    let title_floor = left + MIN_TITLE_COLUMNS * tw + pad;
    let mut edge = right;
    let mut take = |width: f32, always: bool| {
        (always || edge - width >= title_floor).then(|| {
            edge -= width;
            (edge, header_y, width, lh)
        })
    };
    let close = take(button, true).unwrap_or_default();
    let toggle = take(toggle_width, true).unwrap_or_default();
    let play = if lane.play.is_some() {
        take(button, false)
    } else {
        None
    };
    let video = lane.play.is_some() && lane.mode == MediaLaneMode::Rendered;
    let fullscreen = if video && lane.pin_label.is_some() {
        take(button, false)
    } else {
        None
    };
    let theater = if video && lane.pin_label.is_some() {
        take(button, false)
    } else {
        None
    };
    let sound = if lane.sound.is_some() {
        take(button, false)
    } else {
        None
    };
    let reload = if lane.reload {
        take(button, false)
    } else {
        None
    };
    let mode = if lane.source.is_some() {
        take(button, false)
    } else {
        None
    };
    let canvas = if lane.mode == MediaLaneMode::Rendered && lane.image.is_some() {
        take(button, false)
    } else {
        None
    };
    let (previous, next) = if lane.position.1 > 1 {
        match take(2.0 * button, false) {
            Some(pair) => (
                Some((pair.0, pair.1, button, lh)),
                Some((pair.0 + button, pair.1, button, lh)),
            ),
            None => (None, None),
        }
    } else {
        (None, None)
    };
    let copy = if lane.copy { take(button, false) } else { None };
    // Zooming is for a rendered image the lane shows, not a preview's frame.
    let zooms = full
        && lane.mode == MediaLaneMode::Rendered
        && lane.image.is_some()
        && lane.frame.is_none()
        && !video;
    let mut zoom_control = || if zooms { take(button, false) } else { None };
    let zoom_fit = zoom_control();
    let zoom_in = zoom_control();
    let zoom_out = zoom_control();
    let open_outside = if lane.open_outside {
        take(button, false)
    } else {
        None
    };
    let counter_width = (media_lane_counter(lane.position).chars().count() as f32 + 1.0) * tw;
    let counter = take(counter_width, false).unwrap_or((edge, header_y, 0.0, lh));
    let title = (left, header_y, (edge - pad - left).max(0.0), lh);
    let empty = (left, header_y, 0.0, 0.0);
    if !full {
        return Some(MediaLaneGeometry {
            pinning: lane.pin_label.is_some(),
            rect,
            title,
            counter,
            previous,
            next,
            open_outside,
            mode,
            canvas,
            copy,
            reload,
            play,
            theater,
            fullscreen,
            video,
            play_feedback: None,
            sound,
            scrubber: None,
            scrub_label: None,
            transport: None,
            transport_timeline: None,
            transport_text_scale: 1.0,
            zoom_out,
            zoom_in,
            zoom_fit,
            // An expanded lane too short for its rows still resizes by its
            // edge, which its centered header leaves clear.
            resize: lane.edge.map(|edge| match edge {
                MediaLaneEdge::Top => (rect.0, rect.1, rect.2, pad),
                MediaLaneEdge::Left => (rect.0, rect.1, pad, rect.3),
            }),
            toggle,
            close,
            full,
            detail: empty,
            page_previous: None,
            page_counter: None,
            page_next: None,
            sender: empty,
            hint: empty,
            image_area: empty,
            content: empty,
            fit: None,
            image: None,
            tile: None,
            source_rows: 0,
        });
    }
    let mut detail = (left, header_y + lh, right - left, lh);
    // A gallery's page before, its counter and the page after end the detail
    // line, when the detail keeps its few columns beside them.
    let (mut page_previous, mut page_counter, mut page_next) = (None, None, None);
    if let Some(page) = lane.page.filter(|&(_, count)| count > 1) {
        let counter = (media_lane_counter(page).chars().count() as f32 + 1.0) * tw;
        let start = right - 2.0 * button - counter;
        if start - pad - left >= MIN_TITLE_COLUMNS * tw {
            page_previous = Some((start, detail.1, button, lh));
            page_counter = Some((start + button, detail.1, counter, lh));
            page_next = Some((start + button + counter, detail.1, button, lh));
            detail.2 = start - pad - left;
        }
    }
    let sender = (left, detail.1 + lh, right - left, lh);
    let hint = (left, rect.1 + rect.3 - pad - lh, right - left, lh);
    let area_top = sender.1 + lh + pad;
    // A playing video's bar takes a line above the hint, its time at the
    // end, when the picture keeps a line of its own above it and the bar
    // keeps a few columns beside its time.
    let (mut scrubber, mut scrub_label) = (None, None);
    let mut area_bottom = hint.1 - pad;
    let target = (44.0 * scale).max(2.0 * lh);
    let seek_height = (24.0 * scale).max(lh);
    let time_scale = (14.0 * scale / (lh / 1.25)).max(1.0);
    let label_width = lane.scrubber.as_ref().map_or(12.0, |bar| {
        let elapsed = crate::duration_label(bar.position_ms.min(bar.duration_ms));
        let duration = crate::duration_label(bar.duration_ms);
        (crate::display_width(&bar.label) + duration.len().saturating_sub(elapsed.len()) + 1) as f32
    }) * tw
        * time_scale;
    let label_height = lh * time_scale;
    let separate_time = right - left < 4.0 * target + label_width + 2.0 * pad;
    let time_row_height = if separate_time {
        label_height + pad
    } else {
        0.0
    };
    let transport_height = time_row_height + seek_height + target;
    let transport_buttons = 3.0 * target;
    let transport = video
        && lane.pin_label.is_some()
        && area_bottom - transport_height - pad >= area_top + lh
        && right - left >= transport_buttons;
    if transport {
        area_bottom -= transport_height + pad;
    }
    if let Some(bar) = lane.scrubber.as_ref().filter(|bar| bar.seekable)
        && !transport
        && area_bottom - lh - pad >= area_top + lh
    {
        let row = hint.1 - pad - lh;
        let label = (bar.label.chars().count() as f32 + 1.0) * tw;
        let label_left = right - label;
        if label_left - pad - left >= MIN_SCRUBBER_COLUMNS * tw {
            scrub_label = Some((label_left, row, right - label_left, lh));
            scrubber = Some((left, row, label_left - pad - left, lh));
            area_bottom = row - pad;
        }
    }
    let image_area = (
        left,
        area_top,
        right - left,
        (area_bottom - area_top).max(0.0),
    );
    let content = whole_pixels(image_area);
    // A preview's frame takes the place the item's pixels would, unzoomed.
    let (fit, source_rows) = match (lane.mode, &lane.frame) {
        (MediaLaneMode::Rendered, Some(frame)) => (fit_size(frame.size, content, video), 0),
        (MediaLaneMode::Rendered, None) => (
            lane.image
                .as_ref()
                .and_then(|image| fit_size((image.width, image.height), content, video)),
            0,
        ),
        (MediaLaneMode::Source, _) => (None, (image_area.3 / lh).floor() as usize),
    };
    let image = match (&lane.frame, video) {
        (Some(_), _) | (_, true) => fit,
        (None, false) => fit.and_then(|fit| lane.view.place(fit, content)),
    };
    let mut geometry = MediaLaneGeometry {
        pinning: lane.pin_label.is_some(),
        rect,
        title,
        counter,
        previous,
        next,
        open_outside,
        mode,
        canvas,
        copy,
        reload,
        play,
        theater,
        fullscreen,
        video,
        play_feedback: None,
        sound,
        scrubber,
        scrub_label,
        transport: None,
        transport_timeline: None,
        transport_text_scale: 1.0,
        zoom_out,
        zoom_in,
        zoom_fit,
        // The padding along the edge that borders the terminal: nothing
        // else in the lane reaches into it.
        resize: lane.edge.map(|edge| match edge {
            MediaLaneEdge::Top => (rect.0, rect.1, rect.2, pad),
            MediaLaneEdge::Left => (rect.0, rect.1, pad, rect.3),
        }),
        toggle,
        close,
        full,
        detail,
        page_previous,
        page_counter,
        page_next,
        sender,
        hint,
        image_area,
        content,
        fit,
        image,
        tile: None,
        source_rows,
    };
    if transport && let Some(picture) = geometry.image {
        // Use the player's width, including a portrait picture's letterboxing.
        // The timeline has its own row so larger buttons never shorten it.
        let width = content.2;
        if width >= transport_buttons {
            let top = picture.1 + picture.3;
            let seek_row = top + time_row_height;
            let row = seek_row + seek_height;
            let mut left = content.0;
            let mut right = left + width;
            let start = left;
            geometry.transport = Some((left, top, width, transport_height));
            geometry.transport_timeline = Some((left, seek_row, width, seek_height));
            geometry.transport_text_scale = time_scale;
            geometry.play = Some((left, row, target, target));
            left += target;
            if lane.sound.is_some() && width >= 4.0 * target {
                let rect = (left, row, target, target);
                left += target;
                geometry.sound = Some(rect);
            }
            right -= target;
            geometry.fullscreen = Some((right, row, target, target));
            right -= target;
            geometry.theater = Some((right, row, target, target));
            if lane.scrubber.is_some() {
                let label_top = if separate_time {
                    top
                } else {
                    row + (target - label_height) / 2.0
                };
                let label_left = if separate_time { start } else { left + pad };
                geometry.scrub_label =
                    Some((label_left, label_top, label_width.min(width), label_height));
                geometry.scrubber = lane
                    .scrubber
                    .as_ref()
                    .filter(|bar| bar.seekable)
                    .and(geometry.transport_timeline);
            }
        }
        if let Some(feedback) = lane.play_feedback {
            let size = (48.0 * scale).min(picture.2 / 2.0).min(picture.3 / 2.0);
            if size >= 24.0 * scale {
                geometry.play_feedback = Some((
                    (
                        picture.0 + (picture.2 - size) / 2.0,
                        picture.1 + (picture.3 - size) / 2.0,
                        size,
                        size,
                    ),
                    feedback,
                ));
            }
        }
    }
    geometry.tile = lane
        .tile
        .as_ref()
        .filter(|_| lane.frame.is_none())
        .zip(geometry.image)
        .zip(geometry.visible())
        .and_then(|((tile, image), visible)| tile_rect(tile.coverage, visible, image));
    Some(geometry)
}

/// Where pixels covering `coverage` of an image drawn at `image` go, when
/// they cover all of it in view, `visible`.
fn tile_rect(
    coverage: (f64, f64, f64, f64),
    visible: (f64, f64, f64, f64),
    image: Rect4,
) -> Option<Rect4> {
    const SLACK: f64 = 1e-6;
    let finite = [coverage.0, coverage.1, coverage.2, coverage.3]
        .iter()
        .all(|value| value.is_finite());
    let covers = visible.0 >= coverage.0 - SLACK
        && visible.1 >= coverage.1 - SLACK
        && visible.2 <= coverage.2 + SLACK
        && visible.3 <= coverage.3 + SLACK;
    if !(finite && covers && coverage.2 > coverage.0 && coverage.3 > coverage.1) {
        return None;
    }
    let (x, y, w, h) = (
        f64::from(image.0),
        f64::from(image.1),
        f64::from(image.2),
        f64::from(image.3),
    );
    Some((
        (x + coverage.0 * w) as f32,
        (y + coverage.1 * h) as f32,
        ((coverage.2 - coverage.0) * w) as f32,
        ((coverage.3 - coverage.1) * h) as f32,
    ))
}

/// The whole pixels inside `area`, so an image lands on pixel boundaries
/// and never past its edges.
fn whole_pixels(area: Rect4) -> Rect4 {
    let (left, top) = (area.0.ceil(), area.1.ceil());
    (
        left,
        top,
        ((area.0 + area.2).floor() - left).max(0.0),
        ((area.1 + area.3).floor() - top).max(0.0),
    )
}

/// Where an image `size` large sits fitted in `content`, which is in whole
/// pixels.
fn fit_size(size: (u32, u32), content: Rect4, upscale: bool) -> Option<Rect4> {
    let (width, height) = (size.0 as f32, size.1 as f32);
    let (left, top, room) = (content.0, content.1, (content.2, content.3));
    if width <= 0.0 || height <= 0.0 || room.0 < 1.0 || room.1 < 1.0 {
        return None;
    }
    // Fit inside that room, with still images capped at their own pixels, at one
    // scale. The longer edge is floored to stay inside and the shorter
    // one follows from the image's shape, rounded, so the shape is off by
    // under half a pixel. When rounding would take the shorter edge past
    // the room, it takes the whole room instead and the longer edge
    // follows it. An image too thin to keep its shape and fit still keeps
    // one pixel, so it stays visible.
    let scale = (room.0 / width).min(room.1 / height);
    let scale = if upscale { scale } else { scale.min(1.0) };
    let fit = |long: f32, short: f32, short_room: f32| {
        let long_edge = (long * scale).floor().max(1.0);
        let short_edge = (long_edge * short / long).round().max(1.0);
        if short_edge <= short_room {
            (long_edge, short_edge)
        } else {
            ((short_room * long / short).round().max(1.0), short_room)
        }
    };
    let (w, h) = if width >= height {
        fit(width, height, room.1)
    } else {
        let (h, w) = fit(height, width, room.0);
        (w, h)
    };
    Some((
        left + ((room.0 - w) / 2.0).floor(),
        top + ((room.1 - h) / 2.0).floor(),
        w,
        h,
    ))
}

/// A lane's text, shaped once per change: its seven lines, the source rows
/// in view and the control glyphs.
pub(crate) struct LaneText {
    title: TextBuffer,
    detail: TextBuffer,
    sender: TextBuffer,
    counter: TextBuffer,
    hint: TextBuffer,
    status: TextBuffer,
    page_counter: TextBuffer,
    scrub_label: TextBuffer,
    /// Previous, next, open outside, collapse, expand, close, show source,
    /// show rendered, canvas, copy, reload, zoom out, zoom in, fit, a
    /// gallery's page before and after, and a video's play, stop, pause and
    /// sound.
    controls: [TextBuffer; 22],
    pin_label: Option<String>,
    /// What each line buffer was last shaped with; `None` until it is, or
    /// once a font change means it must be again.
    shaped: [Option<String>; 8],
    /// One buffer per source row in view, and what each was shaped with.
    rows: Vec<TextBuffer>,
    rows_shaped: Vec<String>,
}

impl LaneText {
    pub(crate) fn new(font_system: &mut FontSystem, metrics: Metrics) -> Self {
        let line = |font_system: &mut FontSystem| {
            let mut buffer = TextBuffer::new(font_system, metrics);
            buffer.set_wrap(Wrap::None);
            buffer
        };
        let control = |font_system: &mut FontSystem, glyph: &str| {
            let mut buffer = line(font_system);
            buffer.set_text(
                glyph,
                &Attrs::new().weight(Weight::BOLD),
                Shaping::Advanced,
                None,
            );
            buffer
        };
        Self {
            title: line(font_system),
            detail: line(font_system),
            sender: line(font_system),
            counter: line(font_system),
            hint: line(font_system),
            status: line(font_system),
            page_counter: line(font_system),
            scrub_label: line(font_system),
            controls: [
                control(font_system, "‹"),
                control(font_system, "›"),
                control(font_system, "↗"),
                control(font_system, "▾"),
                control(font_system, "▴"),
                control(font_system, "×"),
                control(font_system, "≡"),
                control(font_system, "▣"),
                control(font_system, "◐"),
                control(font_system, "⧉"),
                control(font_system, "↻"),
                control(font_system, "−"),
                control(font_system, "+"),
                control(font_system, "⤢"),
                control(font_system, "‹"),
                control(font_system, "›"),
                control(font_system, "▶"),
                control(font_system, "■"),
                control(font_system, "‖"),
                control(font_system, "♪"),
                control(font_system, "T"),
                control(font_system, "F"),
            ],
            pin_label: None,
            shaped: Default::default(),
            rows: Vec::new(),
            rows_shaped: Vec::new(),
        }
    }

    /// What each line and source row was last shaped with, for the
    /// renderer's text damage key: buffers keep their addresses when the
    /// lane changes item.
    pub(crate) fn shaped(&self) -> (&[Option<String>; 8], &[String], Option<&str>) {
        (&self.shaped, &self.rows_shaped, self.pin_label.as_deref())
    }

    /// Reshape every line on the next fill, after the font family or the
    /// loaded faces change: the text alone would not say so.
    pub(crate) fn invalidate(&mut self) {
        self.shaped = Default::default();
        self.pin_label = None;
        self.rows.clear();
        self.rows_shaped.clear();
    }

    /// Shape what `lane` shows into its rectangles; `glyph_width` is the
    /// overlay text's column width, which fits the sender line. A lane
    /// showing only its header leaves the other lines empty.
    pub(crate) fn shape(
        &mut self,
        font_system: &mut FontSystem,
        metrics: Metrics,
        family: &str,
        lane: &MediaLanePanel,
        geometry: &MediaLaneGeometry,
        glyph_width: f32,
    ) {
        let counter = media_lane_counter(lane.position);
        let page_counter = match (lane.page, geometry.page_counter) {
            (Some(page), Some(_)) => media_lane_counter(page),
            _ => String::new(),
        };
        let full = |text: &'_ str| {
            if geometry.full {
                text.to_owned()
            } else {
                String::new()
            }
        };
        let status = if lane.image.is_some() || lane.frame.is_some() {
            String::new()
        } else {
            full(&lane.status)
        };
        let columns = (geometry.sender.2 / glyph_width.max(1.0)).floor() as usize;
        let sender = full(&lane.sender.fitted(columns));
        let detail = full(&lane.detail);
        let hint = full(lane.notice.as_deref().unwrap_or(&lane.hint));
        // The source rows in view, in source mode; none otherwise.
        let rows: &[String] = match (&lane.source, lane.mode, geometry.full) {
            (Some(source), MediaLaneMode::Source, true) => {
                &source.rows[..source.rows.len().min(geometry.source_rows)]
            }
            _ => &[],
        };
        self.rows.truncate(rows.len());
        self.rows_shaped.truncate(rows.len());
        while self.rows.len() < rows.len() {
            let mut buffer = TextBuffer::new(font_system, metrics);
            buffer.set_wrap(Wrap::None);
            self.rows.push(buffer);
            self.rows_shaped.push(String::new());
        }
        for ((buffer, shaped), row) in self.rows.iter_mut().zip(&mut self.rows_shaped).zip(rows) {
            buffer.set_metrics(metrics);
            buffer.set_size(Some(geometry.image_area.2), Some(metrics.line_height));
            if *shaped != *row {
                // A source shows the characters it holds: no ligature may
                // join `-->` into an arrow, whatever the terminal's font
                // settings.
                let mut features = FontFeatures::new();
                for tag in [b"liga", b"clig", b"calt", b"dlig"] {
                    features.disable(FeatureTag::new(tag));
                }
                let attrs = Attrs::new()
                    .family(Family::Name(family))
                    .font_features(features);
                buffer.set_text(row, &attrs, Shaping::Advanced, None);
                row.clone_into(shaped);
            }
            buffer.shape_until_scroll(font_system, false);
        }
        let page_rect = geometry.page_counter.unwrap_or_default();
        let scrub_label = match (&lane.scrubber, geometry.scrub_label) {
            (Some(bar), Some(_)) => bar.label.clone(),
            _ => String::new(),
        };
        let scrub_rect = geometry.scrub_label.unwrap_or_default();
        let lines: [(&mut TextBuffer, &str, Rect4, bool); 8] = [
            (&mut self.title, &lane.title, geometry.title, true),
            (&mut self.detail, &detail, geometry.detail, false),
            (&mut self.counter, &counter, geometry.counter, false),
            (&mut self.hint, &hint, geometry.hint, false),
            (&mut self.status, &status, geometry.image_area, false),
            (&mut self.sender, &sender, geometry.sender, false),
            (&mut self.page_counter, &page_counter, page_rect, false),
            (&mut self.scrub_label, &scrub_label, scrub_rect, false),
        ];
        for ((buffer, text, rect, bold), shaped) in lines.into_iter().zip(&mut self.shaped) {
            buffer.set_metrics(metrics);
            buffer.set_size(Some(rect.2), Some(rect.3));
            if shaped.as_deref() != Some(text) {
                let attrs = Attrs::new().family(Family::Name(family));
                let attrs = if bold {
                    attrs.weight(Weight::BOLD)
                } else {
                    attrs
                };
                buffer.set_text(text, &attrs, Shaping::Advanced, None);
                text.clone_into(shaped.get_or_insert_default());
            }
            buffer.shape_until_scroll(font_system, false);
        }
        let pin_label = lane.pin_label.as_deref().unwrap_or("▾");
        if self.pin_label.as_deref() != Some(pin_label) {
            self.controls[3].set_text(
                pin_label,
                &Attrs::new()
                    .family(Family::Name(family))
                    .weight(Weight::BOLD),
                Shaping::Advanced,
                None,
            );
            self.pin_label = Some(pin_label.to_owned());
        }
        for buffer in &mut self.controls {
            buffer.set_metrics(metrics);
            buffer.shape_until_scroll(font_system, false);
        }
    }

    /// The text areas to paint, in `label`, `description` and `emphasis`
    /// colors. Control glyphs are centered in their cells by `glyph_width`;
    /// a gallery's page control with no page that way, at `page`'s first or
    /// last, and the sound button while muted, are in the description color.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn areas<'a>(
        &'a self,
        geometry: &MediaLaneGeometry,
        collapsed: bool,
        mode: MediaLaneMode,
        page: Option<(usize, usize)>,
        play: Option<MediaLanePlay>,
        sound: Option<MediaLaneSound>,
        colors: (GColor, GColor, GColor),
        glyph_width: f32,
        line_height: f32,
    ) -> Vec<TextArea<'a>> {
        let (label, description, emphasis) = colors;
        let area = |buffer: &'a TextBuffer, rect: Rect4, color: GColor| TextArea {
            buffer,
            left: rect.0,
            top: rect.1,
            scale: 1.0,
            bounds: crate::text_bounds_for_rect(rect),
            default_color: color,
            custom_glyphs: &[],
        };
        let mut areas = vec![
            area(&self.title, geometry.title, label),
            area(&self.counter, geometry.counter, description),
        ];
        if geometry.full {
            areas.extend([
                area(&self.detail, geometry.detail, description),
                area(&self.sender, geometry.sender, description),
                area(&self.hint, geometry.hint, description),
            ]);
        }
        if let Some(rect) = geometry.page_counter {
            areas.push(area(&self.page_counter, rect, description));
        }
        if let Some(rect) = geometry.scrub_label {
            let mut time = area(
                &self.scrub_label,
                rect,
                if geometry.transport.is_some() {
                    GColor::rgb(255, 255, 255)
                } else {
                    label
                },
            );
            time.scale = geometry.transport_text_scale;
            areas.push(time);
        }
        if let Some(status) = self.shaped[4]
            .as_deref()
            .filter(|status| !status.is_empty())
        {
            let (x, y, w, h) = geometry.image_area;
            let text = status.chars().count() as f32 * glyph_width;
            let left = x + ((w - text) / 2.0).max(0.0);
            let top = y + ((h - line_height) / 2.0).max(0.0);
            areas.push(area(
                &self.status,
                (left, top, w - (left - x), line_height),
                description,
            ));
        }
        let (x, y, _, _) = geometry.image_area;
        for (row, buffer) in self.rows.iter().enumerate() {
            let top = y + row as f32 * line_height;
            areas.push(area(
                buffer,
                (x, top, geometry.image_area.2, line_height),
                label,
            ));
        }
        // The toggle collapses an expanded lane and expands a collapsed one;
        // the mode control offers whichever mode is not showing.
        let source = mode == MediaLaneMode::Source;
        let buttons = [
            geometry.previous,
            geometry.next,
            geometry.open_outside,
            (!collapsed).then_some(geometry.toggle),
            collapsed.then_some(geometry.toggle),
            Some(geometry.close),
            geometry.mode.filter(|_| !source),
            geometry.mode.filter(|_| source),
            geometry.canvas,
            geometry.copy,
            geometry.reload,
            geometry.zoom_out,
            geometry.zoom_in,
            geometry.zoom_fit,
            geometry.page_previous,
            geometry.page_next,
            geometry.play.filter(|_| play == Some(MediaLanePlay::Play)),
            geometry.play.filter(|_| play == Some(MediaLanePlay::Stop)),
            geometry.play.filter(|_| play == Some(MediaLanePlay::Pause)),
            geometry.sound,
            geometry.theater,
            geometry.fullscreen,
        ];
        let (first, last) =
            page.map_or((true, true), |(shown, count)| (shown <= 1, shown >= count));
        for (at, (buffer, rect)) in self.controls.iter().zip(buttons).enumerate() {
            if let Some(rect) = rect {
                let idle = (at == 14 && first)
                    || (at == 15 && last)
                    || (at == 19 && sound == Some(MediaLaneSound::Muted));
                let mut glyph = area(buffer, rect, if idle { description } else { emphasis });
                if geometry.transport.is_some()
                    && (16..=21).contains(&at)
                    && rect.1 >= geometry.transport.unwrap().1
                {
                    // Player icons are vector shapes; retain semantic controls and
                    // their larger pointer targets without terminal-font glyphs.
                    continue;
                }
                let control_width = if at == 3 && geometry.pinning {
                    self.pin_label.as_deref().map_or(1, crate::display_width) as f32 * glyph_width
                } else {
                    glyph_width
                };
                glyph.left = rect.0 + ((rect.2 - control_width * glyph.scale) / 2.0).max(0.0);
                glyph.top = rect.1 + ((rect.3 - line_height * glyph.scale) / 2.0).max(0.0);
                areas.push(glyph);
            }
        }
        areas
    }
}

/// Most squares a checkerboard is drawn with; larger images get larger
/// squares.
const MAX_CHECKER_SQUARES: usize = 2048;

/// The dark squares of a checkerboard over `rect`, whose light squares are
/// the background beneath. Clipped to `rect` and bounded in number.
pub fn checker_squares(rect: Rect4, square: f32) -> Vec<Rect4> {
    let (x, y, w, h) = rect;
    if !(square.is_finite() && square >= 1.0 && w >= 1.0 && h >= 1.0) {
        return Vec::new();
    }
    let mut size = square;
    while ((w / size).ceil() * (h / size).ceil()) as usize > 2 * MAX_CHECKER_SQUARES {
        size *= 2.0;
    }
    let (columns, rows) = ((w / size).ceil() as usize, (h / size).ceil() as usize);
    let mut squares = Vec::with_capacity(columns * rows / 2 + 1);
    for row in 0..rows {
        for column in (row % 2..columns).step_by(2) {
            let sx = x + column as f32 * size;
            let sy = y + row as f32 * size;
            squares.push((sx, sy, size.min(x + w - sx), size.min(y + h - sy)));
        }
    }
    squares
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_poster_and_frame_fill_available_room_without_cropping() {
        let mut lane = viewer((1, 1), Some((160, 90)));
        lane.play = Some(MediaLanePlay::Play);
        let geometry = media_lane_geometry(&lane, (8.0, 16.0), (8.0, 16.0)).unwrap();
        let fit = geometry.fit.unwrap();
        assert!(fit.2 > 160.0 && fit.3 > 90.0);
        assert!((fit.2 / fit.3 - 16.0 / 9.0).abs() < 0.02);
        assert!(fit.2 <= geometry.content.2 && fit.3 <= geometry.content.3);
        assert_eq!(
            geometry.hit_test(fit.0 + fit.2 / 2.0, fit.1 + fit.3 / 2.0),
            Some(MediaLaneHit::Play)
        );
        lane.frame = Some(MediaLaneFrame {
            sheet: lane.image.clone().unwrap(),
            source: (0, 0, 160, 90),
            size: (160, 90),
        });
        assert_eq!(
            media_lane_geometry(&lane, (8.0, 16.0), (8.0, 16.0))
                .unwrap()
                .fit,
            Some(fit)
        );
        lane.play = None;
        lane.frame = None;
        assert_eq!(
            media_lane_geometry(&lane, (8.0, 16.0), (8.0, 16.0))
                .unwrap()
                .fit
                .unwrap()
                .2,
            160.0
        );
    }

    #[test]
    fn video_fit_stays_inside_landscape_portrait_and_narrow_windows() {
        for size in [(1920, 1080), (1080, 1920), (1, 20000), (20000, 1)] {
            for room in [(1920.0, 1080.0), (1080.0, 1920.0), (80.0, 40.0)] {
                let content = (11.0, 13.0, room.0, room.1);
                let fit = fit_size(size, content, true).unwrap();
                assert!(fit.2 >= 1.0 && fit.3 >= 1.0);
                assert!(contains(content, fit.0, fit.1));
                assert!(fit.0 + fit.2 <= content.0 + content.2);
                assert!(fit.1 + fit.3 <= content.1 + content.3);
            }
        }
    }

    #[test]
    fn video_size_controls_are_separate_hit_targets_and_keep_play_priority() {
        let mut lane = viewer((1, 1), Some((160, 90)));
        lane.play = Some(MediaLanePlay::Play);
        lane.pin_label = Some("Pin".into());
        let geometry = media_lane_geometry(&lane, CELL, CELL).unwrap();
        for (rect, hit) in [
            (geometry.theater, MediaLaneHit::Theater),
            (geometry.fullscreen, MediaLaneHit::Fullscreen),
        ] {
            let (x, y, w, h) = rect.unwrap();
            assert_eq!(geometry.hit_test(x + w / 2.0, y + h / 2.0), Some(hit));
        }
        assert!(geometry.zoom_in.is_none());
        lane.pin_label = None;
        let pinned = media_lane_geometry(&lane, CELL, CELL).unwrap();
        assert!(pinned.theater.is_none() && pinned.fullscreen.is_none());
        assert!(pinned.play.is_some());
    }

    #[test]
    fn video_viewer_centers_play_and_keeps_transport_beside_the_picture() {
        let mut lane = viewer((1, 1), Some((160, 90)));
        lane.pin_label = Some("Pin".into());
        lane.play = Some(MediaLanePlay::Play);
        lane.play_feedback = Some(MediaLanePlay::Play);
        let poster = media_lane_geometry(&lane, CELL, CELL).unwrap();
        let picture = poster.image.unwrap();
        let (center, glyph) = poster.play_feedback.unwrap();
        assert_eq!(glyph, MediaLanePlay::Play);
        assert!((center.0 + center.2 / 2.0 - picture.0 - picture.2 / 2.0).abs() < 0.01);
        assert!((center.1 + center.3 / 2.0 - picture.1 - picture.3 / 2.0).abs() < 0.01);
        assert!(poster.play.unwrap().1 >= picture.1 + picture.3);
        assert!(
            poster.fullscreen.unwrap().0 + poster.fullscreen.unwrap().2
                <= poster.content.0 + poster.content.2
        );
        lane.play = Some(MediaLanePlay::Pause);
        lane.play_feedback = None;
        lane.scrubber = Some(MediaLaneScrubber {
            seekable: true,
            position_ms: 1000,
            duration_ms: 12000,
            label: "0:01 / 0:12".into(),
        });
        let playing = media_lane_geometry(&lane, CELL, CELL).unwrap();
        assert_eq!(
            playing.image, poster.image,
            "starting playback does not move the picture"
        );
        assert!(playing.play_feedback.is_none());
        let bar = playing.scrubber.unwrap();
        assert!(bar.1 + bar.3 <= playing.play.unwrap().1);
        assert_eq!(bar.0, playing.play.unwrap().0);
        assert!(bar.0 + bar.2 >= playing.fullscreen.unwrap().0 + playing.fullscreen.unwrap().2);
        assert!(
            playing.scrub_label.unwrap().0 + playing.scrub_label.unwrap().2
                < playing.theater.unwrap().0
        );
    }

    #[test]
    fn video_transport_has_large_targets_and_a_separate_wide_timeline() {
        let mut lane = viewer((1, 1), Some((480, 240)));
        lane.rect = (0.0, 0.0, 486.0, 790.0);
        lane.pin_label = Some("Pin".into());
        lane.play = Some(MediaLanePlay::Pause);
        lane.sound = Some(MediaLaneSound::Heard);
        lane.scrubber = Some(MediaLaneScrubber {
            seekable: true,
            position_ms: 83000,
            duration_ms: 754000,
            label: "1:23 / 12:34".into(),
        });
        let geometry = media_lane_geometry(&lane, (8.0, 16.25), (8.0, 16.25)).unwrap();
        let play = geometry.play.unwrap();
        let track = geometry.scrubber.unwrap();
        assert!(
            play.2 >= 44.0 && play.3 >= 44.0,
            "Play needs a pointer-sized target"
        );
        assert!(
            track.3 >= 24.0,
            "timeline needs a generous vertical hit area"
        );
        assert!(track.1 + track.3 <= play.1, "timeline has its own row");
        assert!(
            track.2 >= geometry.content.2 * 0.8,
            "timeline uses the player's width"
        );
        assert_eq!(
            geometry.hit_test(play.0 + play.2 / 2.0, play.1 + play.3 / 2.0),
            Some(MediaLaneHit::Play)
        );
        assert_eq!(
            geometry.hit_test(track.0 + track.2 / 2.0, track.1 + 1.0),
            Some(MediaLaneHit::Scrub)
        );
    }

    #[test]
    fn video_time_row_stays_fixed_at_timestamp_digit_boundaries() {
        for width in [304.0, 312.0, 320.0] {
            let mut lane = viewer((1, 1), Some((480, 240)));
            lane.rect = (0.0, 0.0, width, 600.0);
            lane.pin_label = Some("Pin".into());
            lane.play = Some(MediaLanePlay::Play);
            lane.scrubber = Some(MediaLaneScrubber {
                seekable: false,
                position_ms: 0,
                duration_ms: 754000,
                label: "0:00 / 12:34".into(),
            });
            let before = media_lane_geometry(&lane, (7.8, 16.25), (7.8, 16.25)).unwrap();
            lane.play = Some(MediaLanePlay::Pause);
            lane.scrubber = Some(MediaLaneScrubber {
                seekable: true,
                position_ms: 600000,
                duration_ms: 754000,
                label: "10:00 / 12:34".into(),
            });
            let after = media_lane_geometry(&lane, (7.8, 16.25), (7.8, 16.25)).unwrap();
            assert_eq!(
                before.image, after.image,
                "timestamp must not shift picture at width {width}"
            );
            assert!(before.scrubber.is_none());
            assert!(after.scrubber.is_some());
            assert_eq!(before.transport, after.transport);
            assert_eq!(before.play, after.play);
            assert_eq!(before.scrub_label, after.scrub_label);
        }
    }

    #[test]
    fn video_icon_canvas_is_independent_of_large_text_targets() {
        for scale in [1.0, 1.25, 2.0] {
            let mut lane = viewer((1, 1), Some((480, 240)));
            lane.pin_label = Some("Pin".into());
            lane.play = Some(MediaLanePlay::Play);
            lane.rect = (0.0, 0.0, 600.0 * scale, 800.0 * scale);
            let geometry = media_lane_geometry_scaled(
                &lane,
                (12.0 * scale, 30.0 * scale),
                (12.0 * scale, 30.0 * scale),
                scale,
            )
            .unwrap();
            assert_eq!(geometry.play.unwrap().3, 60.0 * scale);
            let cfg = kettle_config::Config::default();
            let paint = crate::media_transport::paint(
                &lane,
                &geometry,
                crate::completion_palette(&cfg.theme, cfg.theme.palette[4]),
                None,
                false,
                scale,
            )
            .unwrap();
            let icon = paint
                .shapes
                .iter()
                .find(|shape| shape.params[2] == 1.0)
                .unwrap();
            assert_eq!(
                icon.size,
                [24.0 * scale; 2],
                "visual canvas stays independent of the enlarged hit target"
            );
        }
    }

    #[test]
    fn short_video_viewers_keep_the_picture_and_header_controls() {
        let mut lane = viewer((1, 1), Some((160, 90)));
        lane.rect.3 = 140.0;
        lane.pin_label = Some("Pin".into());
        lane.play = Some(MediaLanePlay::Play);
        let geometry = media_lane_geometry(&lane, CELL, CELL).unwrap();
        let picture = geometry.image.expect("short viewer retains its picture");
        assert!(picture.3 >= CELL.1);
        assert_eq!(geometry.play.unwrap().1, geometry.title.1);
        assert!(picture.1 + picture.3 <= geometry.hint.1);
    }

    #[test]
    fn portrait_video_transport_uses_letterbox_width_to_keep_seeking() {
        for (height, cell) in [(600.0, CELL), (500.0, (8.2, 20.0)), (600.0, (7.8, 16.25))] {
            let mut lane = viewer((1, 1), Some((1080, 1920)));
            lane.pin_label = Some("Pin".into());
            lane.play = Some(MediaLanePlay::Pause);
            lane.sound = Some(MediaLaneSound::Heard);
            lane.scrubber = Some(MediaLaneScrubber {
                seekable: true,
                position_ms: 1000,
                duration_ms: 12000,
                label: "0:01 / 0:12".into(),
            });
            lane.rect.3 = height;
            let geometry = media_lane_geometry(&lane, cell, cell).unwrap();
            let picture = geometry.image.unwrap();
            let bar = geometry.scrubber.expect("portrait video keeps seeking");
            let label = geometry.scrub_label.unwrap();
            let play = geometry.play.unwrap();
            let fullscreen = geometry.fullscreen.unwrap();
            assert!(bar.2 >= PREFERRED_SCRUBBER_COLUMNS * cell.0 - 0.001);
            assert!(bar.1 >= picture.1 + picture.3);
            assert!(bar.1 + bar.3 <= play.1);
            assert!(label.1 >= play.1);
            assert!(label.0 + label.2 < geometry.theater.unwrap().0);
            assert!(play.0 < picture.0 && fullscreen.0 + fullscreen.2 > picture.0 + picture.2);
            assert!(play.0 >= geometry.content.0);
            assert!(fullscreen.0 + fullscreen.2 <= geometry.content.0 + geometry.content.2);
            assert!(
                (play.0 + (fullscreen.0 + fullscreen.2 - play.0) / 2.0
                    - geometry.content.0
                    - geometry.content.2 / 2.0)
                    .abs()
                    < 0.01
            );
        }
    }

    #[test]
    fn video_transport_scales_once_and_keeps_narrow_targets_disjoint() {
        for scale in [1.0, 1.25, 2.0] {
            for width in [192.0, 280.0, 486.0, 1000.0] {
                let mut lane = viewer((1, 1), Some((240, 480)));
                lane.rect = (0.0, 0.0, width * scale, 790.0 * scale);
                lane.pin_label = Some("Pin".into());
                lane.play = Some(MediaLanePlay::Pause);
                lane.sound = Some(MediaLaneSound::Heard);
                lane.scrubber = Some(MediaLaneScrubber {
                    seekable: true,
                    position_ms: 1000,
                    duration_ms: 12000,
                    label: "0:01 / 0:12".into(),
                });
                let cell = (7.8 * scale, 16.25 * scale);
                let geometry = media_lane_geometry_scaled(&lane, cell, cell, scale).unwrap();
                let play = geometry.play.unwrap();
                let bar = geometry
                    .scrubber
                    .expect("narrow video keeps a full-width timeline");
                assert!((play.3 - 44.0 * scale).abs() < 0.001);
                assert!((bar.3 - 24.0 * scale).abs() < 0.001);
                for r in [
                    play,
                    geometry.theater.unwrap(),
                    geometry.fullscreen.unwrap(),
                    bar,
                    geometry.scrub_label.unwrap(),
                ] {
                    assert!(
                        r.0 >= -0.001
                            && r.1 >= -0.001
                            && r.0 + r.2 <= lane.rect.2 + 0.001
                            && r.1 + r.3 <= lane.rect.3 + 0.001
                    );
                }
                assert!(bar.1 + bar.3 <= play.1);
                let time = geometry.scrub_label.unwrap();
                assert!(
                    time.1 + time.3 <= bar.1
                        || (time.0 >= play.0 + play.2
                            && time.0 + time.2 <= geometry.theater.unwrap().0)
                );
                assert!(
                    geometry.image.unwrap().1 + geometry.image.unwrap().3
                        <= geometry.transport.unwrap().1
                );
            }
        }
    }

    const CELL: (f32, f32) = (10.0, 20.0);

    fn viewer(position: (usize, usize), image: Option<(u32, u32)>) -> MediaLanePanel {
        MediaLanePanel {
            focused_control: None,
            pin_label: None,
            play: None,
            play_feedback: None,
            sound: None,
            frame: None,
            scrubber: None,
            pane: 1,
            rect: (0.0, 0.0, 800.0, 600.0),
            collapsed: false,
            title: "Plot".into(),
            detail: "Raster 640×480".into(),
            sender: MediaLaneSender {
                text: "from this pane".into(),
                program: None,
                signer: None,
            },
            hint: "Esc closes".into(),
            position,
            page: None,
            document: false,
            image: image.map(|(w, h)| {
                kettle_core::ImageData::new(w, h, vec![0; (w * h * 4) as usize]).unwrap()
            }),
            status: String::new(),
            canvas: MediaCanvas::Theme,
            open_outside: false,
            mode: MediaLaneMode::Rendered,
            source: None,
            copy: false,

            reload: false,

            notice: None,
            view: crate::MediaViewport::FIT,
            tile: None,
            edge: None,
        }
    }

    #[test]
    fn viewer_backdrop_covers_the_window_without_dimming_the_viewer() {
        let shade = viewer_backdrop((10.0, 20.0, 60.0, 40.0), (100.0, 80.0)).unwrap();
        assert_eq!(
            shade,
            [
                (0.0, 0.0, 100.0, 20.0),
                (0.0, 20.0, 10.0, 40.0),
                (70.0, 20.0, 30.0, 40.0),
                (0.0, 60.0, 100.0, 20.0)
            ]
        );
        assert_eq!(
            shade.iter().map(|r| r.2 * r.3).sum::<f32>(),
            100.0 * 80.0 - 60.0 * 40.0
        );
        assert!(viewer_backdrop((f32::NAN, 0.0, 1.0, 1.0), (100.0, 80.0)).is_none());
    }

    #[test]
    fn overlay_header_action_pins_and_keeps_close_available() {
        let mut lane = viewer((1, 1), Some((64, 48)));
        lane.pin_label = Some("Pin".into());
        let geometry = media_lane_geometry(&lane, CELL, CELL).unwrap();
        let center = |r: Rect4| (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        let (x, y) = center(geometry.toggle);
        assert_eq!(geometry.hit_test(x, y), Some(MediaLaneHit::Pin));
        let (x, y) = center(geometry.close);
        assert_eq!(geometry.hit_test(x, y), Some(MediaLaneHit::Close));
    }

    #[test]
    fn localized_pin_labels_fit_without_overlapping_close_or_title() {
        for label in ["Pin", "Fijar"] {
            let mut lane = viewer((1, 1), None);
            lane.pin_label = Some(label.into());
            let geometry = media_lane_geometry(&lane, CELL, CELL).unwrap();
            assert!(geometry.toggle.2 >= label.len() as f32 * CELL.0);
            assert!(geometry.toggle.0 + geometry.toggle.2 <= geometry.close.0);
            assert!(geometry.title.0 + geometry.title.2 <= geometry.toggle.0);
        }
    }

    #[test]
    fn the_lane_fills_its_rect_with_its_controls_on_one_row() {
        let geometry = media_lane_geometry(&viewer((2, 5), Some((640, 480))), CELL, CELL).unwrap();
        assert_eq!(geometry.rect, (0.0, 0.0, 800.0, 600.0));
        assert!(geometry.full);
        let previous = geometry.previous.unwrap();
        let next = geometry.next.unwrap();
        assert!(previous.0 + previous.2 <= next.0 && next.0 + next.2 <= geometry.toggle.0);
        assert!(geometry.toggle.0 + geometry.toggle.2 <= geometry.close.0);
        assert!(geometry.counter.0 + geometry.counter.2 <= previous.0);
        assert!(geometry.title.0 + geometry.title.2 <= geometry.counter.0);
        for row in [
            geometry.counter,
            previous,
            next,
            geometry.toggle,
            geometry.close,
        ] {
            assert_eq!(row.1, geometry.title.1);
        }
        assert!(geometry.detail.1 > geometry.title.1);
        assert!(geometry.sender.1 > geometry.detail.1);
        assert!(geometry.image_area.1 > geometry.sender.1 + geometry.sender.3 - 1.0);
        assert!(geometry.hint.1 > geometry.image_area.1 + geometry.image_area.3);
    }

    /// The open-outside button, when offered, sits on the header row left of
    /// browsing, the last control before the counter, and a press on it says
    /// so; without it the header is as before.
    #[test]
    fn the_open_outside_button_sits_before_close() {
        let mut offered = viewer((2, 5), Some((640, 480)));
        offered.open_outside = true;
        let geometry = media_lane_geometry(&offered, CELL, CELL).unwrap();
        let open = geometry.open_outside.expect("offered");
        let previous = geometry.previous.unwrap();
        assert!(open.0 + open.2 <= previous.0 && geometry.counter.0 + geometry.counter.2 <= open.0);
        assert_eq!(open.1, geometry.close.1);
        let center = (open.0 + open.2 / 2.0, open.1 + open.3 / 2.0);
        assert_eq!(
            geometry.hit_test(center.0, center.1),
            Some(MediaLaneHit::OpenOutside)
        );
        let single = MediaLanePanel {
            play: None,
            play_feedback: None,
            frame: None,
            position: (1, 1),
            page: None,
            ..offered.clone()
        };
        let geometry = media_lane_geometry(&single, CELL, CELL).unwrap();
        let open = geometry.open_outside.unwrap();
        assert!(geometry.counter.0 + geometry.counter.2 <= open.0);
        // Without it, browsing sits against the canvas, which sits against
        // the toggle.
        let plain = media_lane_geometry(&viewer((2, 5), Some((640, 480))), CELL, CELL).unwrap();
        assert_eq!(plain.open_outside, None);
        let canvas = plain.canvas.unwrap();
        assert_eq!(plain.next.unwrap().0 + plain.next.unwrap().2, canvas.0);
        assert_eq!(canvas.0 + canvas.2, plain.toggle.0);
    }

    /// A video's lane offers its preview button right of the rest, after
    /// collapse, and a press on it says so; without one the header is as
    /// before.
    #[test]
    fn a_video_lane_offers_its_preview_button_first() {
        let mut video = viewer((2, 5), Some((640, 360)));
        video.reload = true;
        video.play = Some(MediaLanePlay::Play);
        let geometry = media_lane_geometry(&video, CELL, CELL).unwrap();
        let play = geometry.play.expect("offered");
        let reload = geometry.reload.unwrap();
        assert!(reload.0 + reload.2 <= play.0 && play.0 + play.2 <= geometry.toggle.0);
        assert_eq!(play.1, geometry.close.1);
        let center = (play.0 + play.2 / 2.0, play.1 + play.3 / 2.0);
        assert_eq!(
            geometry.hit_test(center.0, center.1),
            Some(MediaLaneHit::Play)
        );
        video.play = None;
        assert_eq!(media_lane_geometry(&video, CELL, CELL).unwrap().play, None);
    }

    /// A video whose sound can play offers its sound button just left of
    /// play, and a press on it says so; without sound there is none, and a
    /// header too narrow for it gives it up before the title's columns.
    #[test]
    fn a_video_lane_offers_its_sound_button_beside_play() {
        let mut video = viewer((1, 1), Some((640, 360)));
        video.play = Some(MediaLanePlay::Pause);
        video.sound = Some(MediaLaneSound::Heard);
        let geometry = media_lane_geometry(&video, CELL, CELL).unwrap();
        let (play, sound) = (geometry.play.unwrap(), geometry.sound.expect("offered"));
        assert_eq!(sound.0 + sound.2, play.0);
        assert_eq!(sound.1, play.1);
        let center = (sound.0 + sound.2 / 2.0, sound.1 + sound.3 / 2.0);
        assert_eq!(
            geometry.hit_test(center.0, center.1),
            Some(MediaLaneHit::Sound)
        );
        video.sound = None;
        assert_eq!(media_lane_geometry(&video, CELL, CELL).unwrap().sound, None);
        // As the header narrows the sound button goes before play, and
        // never takes the title's few columns.
        video.sound = Some(MediaLaneSound::Muted);
        let mut gone = false;
        for width in (40..=800).rev().step_by(5) {
            video.rect.2 = width as f32;
            let Some(header) = media_lane_geometry(&video, CELL, CELL) else {
                break;
            };
            if let Some(sound) = header.sound {
                assert!(header.play.is_some(), "sound without play at {width}");
                assert!(sound.0 >= header.title.0 + MIN_TITLE_COLUMNS * CELL.0);
            }
            gone |= header.sound.is_none() && header.play.is_some();
        }
        assert!(gone, "never shown without its sound button");
    }

    /// A preview's frame is drawn where the item's pixels would be, at
    /// their size whatever its own, unzoomed, with no zoom controls and no
    /// sharper tile; with the pixels released it still shows.
    #[test]
    fn a_preview_frame_takes_the_items_place_unzoomed() {
        let mut lane = viewer((1, 1), Some((640, 360)));
        lane.play = Some(MediaLanePlay::Play);
        let poster = media_lane_geometry(&lane, CELL, CELL).unwrap();
        lane.view = crate::MediaViewport::FIT.zoomed(2.0, poster.fit.unwrap(), poster.content);
        lane.tile = Some(MediaLaneTile {
            image: kettle_core::ImageData::new(2, 2, vec![0; 16]).unwrap(),
            coverage: (0.0, 0.0, 1.0, 1.0),
        });
        lane.play = Some(MediaLanePlay::Stop);
        lane.frame = Some(MediaLaneFrame {
            sheet: kettle_core::ImageData::new(1556, 444, vec![0; 1556 * 444 * 4]).unwrap(),
            source: (4, 4, 384, 216),
            size: (640, 360),
        });
        let playing = media_lane_geometry(&lane, CELL, CELL).unwrap();
        assert_eq!(playing.fit, poster.fit);
        assert_eq!(playing.image, poster.fit, "never zoomed or panned");
        assert_eq!(playing.tile, None);
        assert_eq!(
            (playing.zoom_in, playing.zoom_out, playing.zoom_fit),
            (None, None, None)
        );
        lane.image = None;
        assert_eq!(
            media_lane_geometry(&lane, CELL, CELL).unwrap().image,
            poster.fit
        );
    }

    /// A playing video's bar takes a line above the hint, its time at the
    /// end and the picture's room above it; a press on it is a scrub, and
    /// where along it says where to; a lane too short for it has none.
    #[test]
    fn a_playing_video_lane_has_its_bar_above_the_hint() {
        let mut lane = viewer((1, 1), Some((640, 360)));
        let poster = media_lane_geometry(&lane, CELL, CELL).unwrap();
        lane.play = Some(MediaLanePlay::Pause);
        lane.scrubber = Some(MediaLaneScrubber {
            seekable: true,
            position_ms: 3000,
            duration_ms: 9000,
            label: "0:03 / 0:09".into(),
        });
        let geometry = media_lane_geometry(&lane, CELL, CELL).unwrap();
        let (bar, label) = (geometry.scrubber.unwrap(), geometry.scrub_label.unwrap());
        assert_eq!(bar.1, label.1);
        assert_eq!(bar.3, CELL.1);
        assert!(bar.1 + bar.3 <= geometry.hint.1, "above the hint");
        assert!(bar.0 + bar.2 < label.0, "the time after the bar");
        assert_eq!(label.2, 12.0 * CELL.0, "the time and a column");
        assert_eq!(label.0 + label.2, geometry.hint.0 + geometry.hint.2);
        assert!(geometry.image_area.1 + geometry.image_area.3 <= bar.1);
        assert!(
            geometry.image_area.3 < poster.image_area.3,
            "the picture gives the room"
        );
        let middle = bar.1 + bar.3 / 2.0;
        assert_eq!(
            geometry.hit_test(bar.0 + 1.0, middle),
            Some(MediaLaneHit::Scrub)
        );
        assert_eq!(
            geometry.hit_test(label.0 + 1.0, middle),
            Some(MediaLaneHit::Inside)
        );
        assert_eq!(geometry.scrub_at(bar.0), Some(0.0));
        assert_eq!(geometry.scrub_at(bar.0 + bar.2 / 4.0), Some(0.25));
        assert_eq!(geometry.scrub_at(bar.0 + bar.2), Some(1.0));
        assert_eq!(
            geometry.scrub_at(bar.0 - 50.0),
            Some(0.0),
            "held to the bar"
        );
        assert_eq!(geometry.scrub_at(bar.0 + bar.2 + 50.0), Some(1.0));
        assert_eq!(poster.scrub_at(bar.0), None);
        // Too narrow for a few columns of bar beside its time: no bar.
        let mut narrow = lane.clone();
        narrow.rect.2 = 2.0 * 8.0 + 15.0 * CELL.0;
        let narrow = media_lane_geometry(&narrow, CELL, CELL).unwrap();
        assert_eq!((narrow.scrubber, narrow.scrub_label), (None, None));
        // A knob stays on its track, and at its start when the track is
        // narrower than it, never past either end.
        assert_eq!(knob_left((10.0, 0.0, 100.0, 4.0), 50.0, 6.0), 57.0);
        assert_eq!(knob_left((10.0, 0.0, 100.0, 4.0), 0.0, 6.0), 10.0);
        assert_eq!(knob_left((10.0, 0.0, 100.0, 4.0), 100.0, 6.0), 104.0);
        assert_eq!(knob_left((10.0, 0.0, 3.0, 4.0), 3.0, 6.0), 10.0);
        assert_eq!(knob_left((10.0, 0.0, 0.0, 4.0), 0.0, 6.0), 10.0);
        // Full, but with only a line of picture: no bar under it.
        lane.rect.3 = 5.0 * CELL.1 + 4.0 * 8.0;
        let short = media_lane_geometry(&lane, CELL, CELL).unwrap();
        assert!(short.full);
        assert_eq!((short.scrubber, short.scrub_label), (None, None));
    }

    #[test]
    fn a_long_sender_line_shortens_the_path_and_keeps_the_signer() {
        let path = "/Users/someone/.local/share/claude/versions/2.1.289";
        let signer = "Anthropic PBC (Q6L2SF6YDW)";
        let text = format!("Unverified sender: {path} (pid 56662), signed by {signer}");
        let at = |part: &str| {
            let start = text.find(part).unwrap();
            Some(start..start + part.len())
        };
        let sender = MediaLaneSender {
            program: at(path),
            signer: at(signer),
            text: text.clone(),
        };
        assert_eq!(sender.fitted(200), text, "a line that fits is whole");
        let fitted = sender.fitted(85);
        assert!(crate::display_width(&fitted) <= 85, "{fitted}");
        assert!(fitted.starts_with("Unverified sender: /"), "{fitted}");
        assert!(
            fitted.ends_with("…/2.1.289 (pid 56662), signed by Anthropic PBC (Q6L2SF6YDW)"),
            "{fitted}"
        );
        // Too narrow even for the path's last segment: the signer stays whole.
        for columns in [30, 40, 50] {
            let narrow = sender.fitted(columns);
            assert!(crate::display_width(&narrow) <= columns, "{narrow}");
            assert!(narrow.contains(signer), "{columns}: {narrow}");
        }
        // Narrower than the signer itself: the line still fits.
        assert!(crate::display_width(&sender.fitted(12)) <= 12);
        // A path shorter than the ellipsis, a signer exactly the width left,
        // and ranges that overlap: each fits, keeps what it can, and never
        // panics.
        let short = "Unverified sender: /a (pid 7), signed by Apple";
        let short_sender = MediaLaneSender {
            program: Some(19..21),
            signer: Some(short.len() - 5..short.len()),
            text: short.into(),
        };
        for columns in [5, 20, 40, 45] {
            let fitted = short_sender.fitted(columns);
            assert!(crate::display_width(&fitted) <= columns, "{fitted}");
            assert!(fitted.contains("Apple"), "{columns}: {fitted}");
        }
        let overlapping = MediaLaneSender {
            text: "abcdef XYZ".into(),
            program: Some(0..6),
            signer: Some(0..10),
        };
        assert!(crate::display_width(&overlapping.fitted(4)) <= 4);
        let inside = MediaLaneSender {
            text: "abcdef XYZ".into(),
            program: Some(0..6),
            signer: Some(1..4),
        };
        assert_eq!(
            inside.fitted(3),
            "bcd",
            "a signer inside the path still fits"
        );
        // A line without a path shortens as a whole.
        let plain = MediaLaneSender {
            text: "x".repeat(50),
            program: None,
            signer: None,
        };
        assert_eq!(crate::display_width(&plain.fitted(10)), 10);
    }

    #[test]
    fn one_item_needs_no_browsing_buttons() {
        let geometry = media_lane_geometry(&viewer((1, 1), None), CELL, CELL).unwrap();
        assert_eq!((geometry.previous, geometry.next), (None, None));
        assert_eq!(geometry.image, None);
        assert_eq!(
            geometry.hit_test(geometry.close.0 + 1.0, geometry.close.1 + 1.0),
            Some(MediaLaneHit::Close)
        );
    }

    #[test]
    fn the_image_is_fitted_centered_and_never_enlarged() {
        let geometry =
            media_lane_geometry(&viewer((1, 1), Some((4000, 1000))), CELL, CELL).unwrap();
        let (ix, iy, iw, ih) = geometry.image.unwrap();
        let (ax, ay, aw, ah) = geometry.image_area;
        assert!(iw <= aw && ih <= ah);
        assert!((iw / ih - 4.0).abs() < 0.05, "keeps its aspect");
        assert!(
            ((ix - ax) - (ax + aw - ix - iw)).abs() <= 1.0,
            "centered across"
        );
        assert!(
            ((iy - ay) - (ay + ah - iy - ih)).abs() <= 1.0,
            "centered down"
        );
        let small = media_lane_geometry(&viewer((1, 1), Some((64, 48))), CELL, CELL).unwrap();
        assert_eq!(
            (small.image.unwrap().2, small.image.unwrap().3),
            (64.0, 48.0)
        );
    }

    #[test]
    fn the_fitted_image_keeps_its_shape_within_half_a_pixel() {
        // Whole and fractional overlay metrics: a fractional line height
        // leaves an image area with a fractional height.
        let layouts = [
            ((0.0, 0.0, 800.0, 600.0), CELL, CELL),
            ((0.0, 0.0, 800.0, 600.0), (10.0, 20.25), (10.0, 20.25)),
            ((0.0, 0.0, 800.0, 300.0), (9.0, 19.0), (9.0, 18.75)),
            ((0.0, 0.0, 333.0, 777.0), (7.5, 15.5), (7.25, 15.3)),
        ];
        let sizes = (1..=40).flat_map(|step| {
            [
                (997, step * 37),
                (step * 37, 997),
                (4096, step),
                (step, 4096),
                (1000, 300 + step),
            ]
        });
        for (pane_rect, cell, text_cell) in layouts {
            for (width, height) in sizes.clone() {
                let mut overlay = viewer((1, 1), Some((width, height)));
                overlay.rect = pane_rect;
                let geometry = media_lane_geometry(&overlay, cell, text_cell).unwrap();
                let (_, _, iw, ih) = geometry.image.unwrap();
                let (_, _, aw, ah) = geometry.image_area;
                let (ix, iy) = (geometry.image.unwrap().0, geometry.image.unwrap().1);
                let (ax, ay) = (geometry.image_area.0, geometry.image_area.1);
                let case = format!("{width}x{height} in {aw}x{ah}: {iw}x{ih} at {ix},{iy}");
                assert!(iw >= 1.0 && ih >= 1.0, "{case}");
                assert!(
                    ix >= ax && iy >= ay && ix + iw <= ax + aw && iy + ih <= ay + ah,
                    "{case}: inside the area"
                );
                for value in [ix, iy, iw, ih] {
                    assert_eq!(value, value.floor(), "{case}: on pixel boundaries");
                }
                let (w, h) = (width as f32, height as f32);
                let (high, wide) = ((iw * h / w - ih).abs(), (ih * w / h - iw).abs());
                if iw * h / w >= 1.0 && ih * w / h >= 1.0 {
                    assert!(high <= 0.5 || wide <= 0.5, "{case}");
                } else {
                    assert!(iw == 1.0 || ih == 1.0, "{case}: one pixel keeps it visible");
                }
            }
        }
        let thin = media_lane_geometry(&viewer((1, 1), Some((4096, 1))), CELL, CELL).unwrap();
        let (_, _, aw, _) = thin.image_area;
        assert_eq!(
            (thin.image.unwrap().2, thin.image.unwrap().3),
            (aw.floor(), 1.0)
        );
        let mut short_room = viewer((1, 1), Some((1000, 300)));
        short_room.rect = (0.0, 0.0, 800.0, 300.0);
        let geometry = media_lane_geometry(&short_room, (9.0, 19.0), (9.0, 18.75)).unwrap();
        let (_, iy, iw, ih) = geometry.image.unwrap();
        let (_, ay, _, ah) = geometry.image_area;
        assert!(iy >= ay && iy + ih <= ay + ah, "{iw}x{ih} at {iy}");
        assert!(
            (ih * 1000.0 / 300.0 - iw).abs() <= 0.5 || (iw * 300.0 / 1000.0 - ih).abs() <= 0.5,
            "{iw}x{ih}"
        );
    }

    #[test]
    fn an_invalidated_line_takes_the_new_family_though_its_text_is_the_same() {
        let mut font_system = FontSystem::new();
        let metrics = Metrics::new(14.0, 18.0);
        let mut text = LaneText::new(&mut font_system, metrics);
        let viewer = viewer((1, 2), Some((64, 48)));
        let geometry = media_lane_geometry(&viewer, CELL, CELL).unwrap();
        text.shape(&mut font_system, metrics, "Menlo", &viewer, &geometry, 8.0);
        text.shape(
            &mut font_system,
            metrics,
            "Courier",
            &viewer,
            &geometry,
            8.0,
        );
        fn family(text: &LaneText) -> Family<'_> {
            text.title.lines[0].attrs_list().defaults().family
        }
        assert_eq!(
            family(&text),
            Family::Name("Menlo"),
            "same text, no reshape"
        );
        text.invalidate();
        assert!(text.shaped().0.iter().all(Option::is_none));
        text.shape(
            &mut font_system,
            metrics,
            "Courier",
            &viewer,
            &geometry,
            8.0,
        );
        assert_eq!(family(&text), Family::Name("Courier"));
    }

    /// The play button shows the glyph for what a press does: play,
    /// pause, or stop a preview; a playing video's time is shaped beside
    /// its bar.
    #[test]
    fn the_play_button_shows_what_a_press_does() {
        let mut font_system = FontSystem::new();
        let metrics = Metrics::new(14.0, 18.0);
        let mut text = LaneText::new(&mut font_system, metrics);
        let mut lane = viewer((1, 1), Some((640, 360)));
        lane.scrubber = Some(MediaLaneScrubber {
            seekable: true,
            position_ms: 0,
            duration_ms: 9000,
            label: "0:00 / 0:09".into(),
        });
        let colors = (
            GColor::rgb(1, 1, 1),
            GColor::rgb(2, 2, 2),
            GColor::rgb(3, 3, 3),
        );
        for (play, glyph) in [
            (MediaLanePlay::Play, 16),
            (MediaLanePlay::Stop, 17),
            (MediaLanePlay::Pause, 18),
        ] {
            lane.play = Some(play);
            lane.sound = None;
            let geometry = media_lane_geometry(&lane, CELL, CELL).unwrap();
            text.shape(&mut font_system, metrics, "Menlo", &lane, &geometry, 8.0);
            let areas = text.areas(
                &geometry,
                false,
                MediaLaneMode::Rendered,
                None,
                Some(play),
                None,
                colors,
                8.0,
                18.0,
            );
            let shown: Vec<usize> = (16..20)
                .filter(|&at| {
                    areas
                        .iter()
                        .any(|area| std::ptr::eq(area.buffer, &text.controls[at]))
                })
                .collect();
            assert_eq!(shown, [glyph], "{play:?}");
            assert!(
                areas
                    .iter()
                    .any(|area| std::ptr::eq(area.buffer, &text.scrub_label))
            );
        }
        assert_eq!(text.shaped().0[7].as_deref(), Some("0:00 / 0:09"));
        // The sound button: in the emphasis color while heard, and the
        // description color while muted.
        lane.play = Some(MediaLanePlay::Pause);
        for (sound, color) in [
            (MediaLaneSound::Heard, colors.2),
            (MediaLaneSound::Muted, colors.1),
        ] {
            lane.sound = Some(sound);
            let geometry = media_lane_geometry(&lane, CELL, CELL).unwrap();
            text.shape(&mut font_system, metrics, "Menlo", &lane, &geometry, 8.0);
            let areas = text.areas(
                &geometry,
                false,
                MediaLaneMode::Rendered,
                None,
                lane.play,
                Some(sound),
                colors,
                8.0,
                18.0,
            );
            let glyph: Vec<_> = areas
                .iter()
                .filter(|area| std::ptr::eq(area.buffer, &text.controls[19]))
                .map(|area| area.default_color)
                .collect();
            assert_eq!(glyph, [color], "{sound:?}");
        }
    }

    #[test]
    fn presses_map_to_controls_inside_and_nothing_outside() {
        let geometry = media_lane_geometry(&viewer((2, 3), Some((64, 48))), CELL, CELL).unwrap();
        let at = |rect: Rect4| geometry.hit_test(rect.0 + 1.0, rect.1 + 1.0);
        assert_eq!(at(geometry.previous.unwrap()), Some(MediaLaneHit::Previous));
        assert_eq!(at(geometry.next.unwrap()), Some(MediaLaneHit::Next));
        assert_eq!(at(geometry.toggle), Some(MediaLaneHit::Toggle));
        assert_eq!(at(geometry.close), Some(MediaLaneHit::Close));
        assert_eq!(at(geometry.image.unwrap()), Some(MediaLaneHit::Inside));
        assert_eq!(geometry.hit_test(1.0, 1.0), Some(MediaLaneHit::Inside));
        assert_eq!(geometry.hit_test(-1.0, 1.0), None, "outside the lane");
        assert_eq!(geometry.hit_test(1.0, 600.0), None, "below the lane");
    }

    #[test]
    fn a_lane_too_narrow_or_a_degenerate_cell_has_no_panel() {
        let mut small = viewer((1, 1), None);
        small.rect = (0.0, 0.0, 110.0, 600.0);
        assert!(media_lane_geometry(&small, CELL, CELL).is_none());
        assert!(media_lane_geometry(&viewer((1, 1), None), (0.0, 20.0), CELL).is_none());
        assert!(media_lane_geometry(&viewer((1, 1), None), CELL, (f32::NAN, 20.0)).is_none());
        let mut short = viewer((1, 1), None);
        short.rect = (0.0, 0.0, 800.0, 19.0);
        assert!(
            media_lane_geometry(&short, CELL, CELL).is_none(),
            "shorter than a line"
        );
    }

    /// The mode control comes with a source, the canvas with an image shown,
    /// and copy when there is something to copy; each answers its press.
    /// Source mode lays out rows in the content area instead of the image.
    #[test]
    fn source_canvas_and_copy_appear_when_offered_and_answer_presses() {
        let plain = media_lane_geometry(&viewer((1, 1), Some((64, 48))), CELL, CELL).unwrap();
        assert_eq!((plain.mode, plain.copy, plain.reload), (None, None, None));
        assert!(plain.canvas.is_some(), "an image has a canvas");
        assert!(
            media_lane_geometry(&viewer((1, 1), None), CELL, CELL)
                .unwrap()
                .canvas
                .is_none(),
            "no pixels, no canvas"
        );
        let mut lane = viewer((1, 1), Some((64, 48)));
        lane.source = Some(MediaLaneSource::default());
        lane.copy = true;
        lane.reload = true;
        let rendered = media_lane_geometry(&lane, CELL, CELL).unwrap();
        // Reload comes first after collapse, as what a changed file needs.
        assert_eq!(
            rendered.reload.unwrap().0 + rendered.reload.unwrap().2,
            rendered.toggle.0
        );
        let center = |rect: Rect4| (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0);
        for (rect, hit) in [
            (rendered.reload, MediaLaneHit::Reload),
            (rendered.mode, MediaLaneHit::Mode),
            (rendered.canvas, MediaLaneHit::Canvas),
            (rendered.copy, MediaLaneHit::Copy),
        ] {
            let (x, y) = center(rect.expect("offered"));
            assert_eq!(rendered.hit_test(x, y), Some(hit));
        }
        assert!(rendered.image.is_some());
        assert_eq!(rendered.source_rows, 0);
        lane.mode = MediaLaneMode::Source;
        let source = media_lane_geometry(&lane, CELL, CELL).unwrap();
        assert_eq!(source.image, None, "source mode shows rows, not the image");
        assert_eq!(source.canvas, None, "a canvas is for the image");
        assert_eq!(
            source.source_rows,
            (source.image_area.3 / CELL.1).floor() as usize
        );
        assert!(source.source_rows > 0);
    }

    /// A rendered image in an expanded lane offers zoom out, zoom in and fit,
    /// in that order left of copy, each answering its press; a source, a
    /// lane without pixels and a collapsed one offer none. Zoomed, the image
    /// is drawn larger than its fit, about the same center, and shows only
    /// within the content.
    #[test]
    fn a_rendered_image_zooms_within_its_content() {
        let mut lane = viewer((1, 1), Some((64, 48)));
        lane.copy = true;
        let geometry = media_lane_geometry(&lane, CELL, CELL).unwrap();
        let (out, zoom_in, fit) = (
            geometry.zoom_out.expect("offered"),
            geometry.zoom_in.expect("offered"),
            geometry.zoom_fit.expect("offered"),
        );
        let copy = geometry.copy.unwrap();
        assert_eq!(out.0 + out.2, zoom_in.0);
        assert_eq!(zoom_in.0 + zoom_in.2, fit.0);
        assert_eq!(fit.0 + fit.2, copy.0);
        let center = |rect: Rect4| (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0);
        for (rect, hit) in [
            (out, MediaLaneHit::ZoomOut),
            (zoom_in, MediaLaneHit::ZoomIn),
            (fit, MediaLaneHit::Fit),
        ] {
            let (x, y) = center(rect);
            assert_eq!(geometry.hit_test(x, y), Some(hit));
        }
        // Fitted, the image is its fit and shows whole.
        let fitted = geometry.fit.unwrap();
        assert_eq!(geometry.image, Some(fitted));
        assert_eq!(geometry.shown(), Some(fitted));
        assert_eq!(fitted, (368.0, 296.0, 64.0, 48.0));
        // Zoomed, it is larger, about the same center, and shows only within
        // the content.
        lane.view = crate::MediaViewport::FIT.zoomed(32.0, fitted, geometry.content);
        let zoomed = media_lane_geometry(&lane, CELL, CELL).unwrap();
        assert_eq!(zoomed.fit, Some(fitted));
        let image = zoomed.image.unwrap();
        assert_eq!((image.2, image.3), (2048.0, 1536.0));
        assert_eq!(center(image), center(fitted));
        assert_eq!(zoomed.shown(), Some(zoomed.content));
        for offered in [
            MediaLanePanel {
                focused_control: None,
                pin_label: None,
                play: None,
                play_feedback: None,
                frame: None,
                mode: MediaLaneMode::Source,
                source: Some(MediaLaneSource::default()),
                ..lane.clone()
            },
            MediaLanePanel {
                focused_control: None,
                pin_label: None,
                play: None,
                play_feedback: None,
                frame: None,
                image: None,
                ..lane.clone()
            },
            MediaLanePanel {
                focused_control: None,
                pin_label: None,
                play: None,
                play_feedback: None,
                frame: None,
                collapsed: true,
                ..lane.clone()
            },
        ] {
            let geometry = media_lane_geometry(&offered, CELL, CELL).unwrap();
            assert_eq!(
                (geometry.zoom_out, geometry.zoom_in, geometry.zoom_fit),
                (None, None, None)
            );
            assert_eq!((geometry.image, geometry.shown()), (None, None));
        }
    }

    /// Sharper pixels are drawn in the part of the zoomed image they cover,
    /// and only while that holds all of it in view; the part in view is the
    /// shown image as fractions of the whole.
    #[test]
    fn a_tile_is_drawn_only_while_it_covers_the_view() {
        let mut lane = viewer((1, 1), Some((64, 48)));
        let fitted = media_lane_geometry(&lane, CELL, CELL).unwrap();
        lane.view = crate::MediaViewport::FIT.zoomed(32.0, fitted.fit.unwrap(), fitted.content);
        let zoomed = media_lane_geometry(&lane, CELL, CELL).unwrap();
        let (image, shown) = (zoomed.image.unwrap(), zoomed.shown().unwrap());
        let visible = zoomed.visible().unwrap();
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(
            visible.0,
            f64::from(shown.0 - image.0) / f64::from(image.2)
        ));
        assert!(visible.0 > 0.0 && visible.2 < 1.0, "{visible:?}");
        assert_eq!(zoomed.tile, None, "no tile, none drawn");
        let tile = |coverage| MediaLaneTile {
            image: kettle_core::ImageData::new(1, 1, vec![0; 4]).unwrap(),
            coverage,
        };
        lane.tile = Some(tile((0.25, 0.0, 0.75, 1.0)));
        let covered = media_lane_geometry(&lane, CELL, CELL).unwrap();
        let rect = covered.tile.expect("it covers the view");
        assert_eq!(rect.0, image.0 + 0.25 * image.2);
        assert_eq!(rect.2, 0.5 * image.2);
        assert_eq!((rect.1, rect.3), (image.1, image.3));
        lane.tile = Some(tile((0.45, 0.0, 0.75, 1.0)));
        assert_eq!(
            media_lane_geometry(&lane, CELL, CELL).unwrap().tile,
            None,
            "the view reaches past it"
        );
        lane.tile = Some(tile((f64::NAN, 0.0, 1.0, 1.0)));
        assert_eq!(media_lane_geometry(&lane, CELL, CELL).unwrap().tile, None);
        lane.tile = Some(tile((0.0, 0.0, 1.0, 1.0)));
        lane.mode = MediaLaneMode::Source;
        lane.source = Some(MediaLaneSource::default());
        assert_eq!(
            media_lane_geometry(&lane, CELL, CELL).unwrap().tile,
            None,
            "a source shows no image"
        );
    }

    /// An expanded lane resizes by the padding along the edge it shares with
    /// its terminal: the top of a bottom lane, the left of a right one. A
    /// press there is a resize, and the strip reaches no control or content.
    #[test]
    fn a_lane_resizes_by_the_padding_along_its_terminal_edge() {
        let mut lane = viewer((2, 5), Some((64, 48)));
        lane.copy = true;
        lane.open_outside = true;
        assert_eq!(media_lane_geometry(&lane, CELL, CELL).unwrap().resize, None);
        // Too short for its rows, it still has its edge, clear of the
        // header centered down it.
        let short = MediaLanePanel {
            play: None,
            play_feedback: None,
            frame: None,
            rect: (0.0, 0.0, 800.0, 74.0),
            edge: Some(MediaLaneEdge::Top),
            ..lane.clone()
        };
        let geometry = media_lane_geometry(&short, CELL, CELL).unwrap();
        assert!(!geometry.full);
        let strip = geometry.resize.expect("an edge");
        assert!(
            strip.1 + strip.3 <= geometry.title.1,
            "{strip:?} {:?}",
            geometry.title
        );
        assert_eq!(
            geometry.hit_test(strip.0 + 1.0, strip.1 + 1.0),
            Some(MediaLaneHit::Resize)
        );
        for edge in [MediaLaneEdge::Top, MediaLaneEdge::Left] {
            lane.edge = Some(edge);
            let geometry = media_lane_geometry(&lane, CELL, CELL).unwrap();
            let strip = geometry.resize.expect("an edge");
            match edge {
                MediaLaneEdge::Top => {
                    assert_eq!(
                        (strip.0, strip.1, strip.2),
                        (lane.rect.0, lane.rect.1, lane.rect.2)
                    );
                    assert!(strip.3 > 0.0 && strip.3 <= geometry.title.1 - lane.rect.1);
                }
                MediaLaneEdge::Left => {
                    assert_eq!(
                        (strip.0, strip.1, strip.3),
                        (lane.rect.0, lane.rect.1, lane.rect.3)
                    );
                    assert!(strip.2 > 0.0 && strip.2 <= geometry.title.0 - lane.rect.0);
                }
            }
            assert_eq!(
                geometry.hit_test(strip.0 + strip.2 / 2.0, strip.1 + strip.3 / 2.0),
                Some(MediaLaneHit::Resize)
            );
            let overlaps = |a: Rect4, b: Rect4| {
                a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
            };
            for part in [
                Some(geometry.close),
                Some(geometry.toggle),
                Some(geometry.title),
                Some(geometry.content),
                geometry.copy,
                geometry.open_outside,
                geometry.previous,
            ]
            .into_iter()
            .flatten()
            {
                assert!(!overlaps(strip, part), "{edge:?}: {strip:?} {part:?}");
            }
        }
    }

    /// A gallery's page before and after, with its counter between, end the
    /// detail line of an expanded lane, which keeps a few columns before
    /// them; a press on either says which, and one on the counter does
    /// nothing. One page, a collapsed lane or too little room offer none.
    #[test]
    fn a_gallery_pages_from_the_end_of_the_detail_line() {
        let mut lane = viewer((1, 1), Some((640, 480)));
        lane.page = Some((2, 5));
        let geometry = media_lane_geometry(&lane, CELL, CELL).unwrap();
        let (previous, counter, next) = (
            geometry.page_previous.unwrap(),
            geometry.page_counter.unwrap(),
            geometry.page_next.unwrap(),
        );
        let (_, line, _, _) = geometry.detail;
        assert!([previous, counter, next].iter().all(|rect| rect.1 == line));
        assert_eq!(previous.0 + previous.2, counter.0);
        assert_eq!(counter.0 + counter.2, next.0);
        assert_eq!(next.0 + next.2, geometry.sender.0 + geometry.sender.2);
        assert_eq!(counter.2, 4.0 * CELL.0, "\"2/5\" and a column");
        assert!(geometry.detail.0 + geometry.detail.2 < previous.0);
        let at = |rect: Rect4| geometry.hit_test(rect.0 + 1.0, rect.1 + 1.0);
        assert_eq!(at(previous), Some(MediaLaneHit::PreviousPage));
        assert_eq!(at(next), Some(MediaLaneHit::NextPage));
        assert_eq!(at(counter), Some(MediaLaneHit::Inside));

        let none = |lane: &MediaLanePanel| {
            let geometry = media_lane_geometry(lane, CELL, CELL).unwrap();
            (
                geometry.page_previous,
                geometry.page_counter,
                geometry.page_next,
            ) == (None, None, None)
        };
        assert!(none(&MediaLanePanel {
            play: None,
            play_feedback: None,
            frame: None,
            page: Some((1, 1)),
            ..lane.clone()
        }));
        assert!(none(&MediaLanePanel {
            play: None,
            play_feedback: None,
            frame: None,
            collapsed: true,
            ..lane.clone()
        }));
        assert!(none(&MediaLanePanel {
            play: None,
            play_feedback: None,
            frame: None,
            page: None,
            ..lane.clone()
        }));
        // The detail keeps four columns beside the controls, or they go.
        let room = |width: f32| MediaLanePanel {
            play: None,
            play_feedback: None,
            frame: None,
            rect: (0.0, 0.0, width, 600.0),
            ..lane.clone()
        };
        let pad = (CELL.0 * 0.75).round().max(4.0);
        let least = 2.0 * pad + 6.0 * CELL.0 + 4.0 * CELL.0 + pad + MIN_TITLE_COLUMNS * CELL.0;
        assert!(!none(&room(least)));
        assert!(none(&room(least - 1.0)));
    }

    /// However narrow the lane, every control and the counter lie inside it:
    /// controls past close and collapse give way, in order, so the title
    /// keeps a few columns, and a lane too narrow even for those has none.
    #[test]
    fn a_narrow_header_keeps_every_control_inside_the_lane() {
        let inside = |outer: Rect4, inner: Rect4| {
            inner.0 >= outer.0
                && inner.1 >= outer.1
                && inner.0 + inner.2 <= outer.0 + outer.2
                && inner.1 + inner.3 <= outer.1 + outer.3
        };
        let mut previous_controls = usize::MAX;
        for width in (60..=900).rev().step_by(10) {
            let mut lane = viewer((2, 5), Some((64, 48)));
            lane.page = Some((3, 12));
            lane.open_outside = true;
            lane.copy = true;
            lane.reload = true;
            lane.source = Some(MediaLaneSource::default());
            lane.rect = (210.0, 40.0, width as f32, 300.0);
            let Some(geometry) = media_lane_geometry(&lane, CELL, CELL) else {
                assert!(width < 120, "{width} px holds close, collapse and a title");
                continue;
            };
            let optional = [
                geometry.reload,
                geometry.mode,
                geometry.canvas,
                geometry.previous,
                geometry.next,
                geometry.copy,
                geometry.zoom_fit,
                geometry.zoom_in,
                geometry.zoom_out,
                geometry.open_outside,
            ];
            let controls = optional.iter().flatten().count();
            // They give way from the end of that order: no single button
            // shows while one before it does not, and browsing, two wide,
            // shows only after the mode and canvas; a single button may still
            // fit where browsing does not.
            let shown: Vec<bool> = optional.iter().map(Option::is_some).collect();
            assert_eq!(shown[3], shown[4], "{width}: browsing comes as a pair");
            let singles = [
                shown[0], shown[1], shown[2], shown[5], shown[6], shown[7], shown[8], shown[9],
            ];
            assert!(
                singles.windows(2).all(|pair| pair[0] || !pair[1]),
                "{width}: {singles:?}"
            );
            assert!(
                !shown[3] || (shown[0] && shown[1] && shown[2]),
                "{width}: {shown:?}"
            );
            assert!(
                controls <= previous_controls,
                "fewer controls as it narrows"
            );
            previous_controls = controls;
            for part in [
                Some(geometry.close),
                Some(geometry.toggle),
                Some(geometry.counter),
                geometry.page_previous,
                geometry.page_counter,
                geometry.page_next,
            ]
            .into_iter()
            .chain(optional)
            .flatten()
            {
                assert!(
                    inside(lane.rect, part),
                    "{width}: {part:?} in {:?}",
                    lane.rect
                );
            }
            assert!(geometry.title.2 >= 0.0);
            assert!(geometry.title.0 + geometry.title.2 <= geometry.toggle.0);
        }
        assert_eq!(
            previous_controls, 0,
            "the narrowest keep only close and collapse"
        );
    }

    /// A collapsed lane, or one too short for its rows, is its header alone,
    /// centered down the strip, with the same controls and nothing below.
    #[test]
    fn a_collapsed_or_short_lane_is_its_header_alone() {
        let mut collapsed = viewer((2, 3), Some((64, 48)));
        collapsed.collapsed = true;
        let short = MediaLanePanel {
            play: None,
            play_feedback: None,
            frame: None,
            rect: (0.0, 100.0, 800.0, 30.0),
            collapsed: false,
            ..collapsed.clone()
        };
        for lane in [collapsed, short] {
            let geometry = media_lane_geometry(&lane, CELL, CELL).unwrap();
            assert!(!geometry.full);
            assert_eq!(geometry.image, None);
            assert_eq!(geometry.detail.2 * geometry.detail.3, 0.0);
            let (_, y, _, h) = lane.rect;
            let middle = geometry.title.1 + geometry.title.3 / 2.0;
            assert!((middle - (y + h / 2.0)).abs() <= 1.0, "{geometry:?}");
            assert!(geometry.next.is_some() && geometry.close.2 > 0.0);
            assert_eq!(
                geometry.hit_test(geometry.toggle.0 + 1.0, geometry.toggle.1 + 1.0),
                Some(MediaLaneHit::Toggle)
            );
        }
    }

    #[test]
    fn checkerboards_are_clipped_alternating_and_bounded() {
        let squares = checker_squares((10.0, 10.0, 30.0, 20.0), 10.0);
        assert_eq!(
            squares,
            vec![
                (10.0, 10.0, 10.0, 10.0),
                (30.0, 10.0, 10.0, 10.0),
                (20.0, 20.0, 10.0, 10.0)
            ]
        );
        let huge = checker_squares((0.0, 0.0, 4096.0, 4096.0), 1.0);
        assert!(huge.len() <= MAX_CHECKER_SQUARES + 64, "{}", huge.len());
        assert!(checker_squares((0.0, 0.0, 10.0, 10.0), f32::NAN).is_empty());
    }
}
