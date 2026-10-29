use super::*;

/// A sender line naming `program` and its `signer`, with where each sits in
/// it. The line is rendered once more per part with a control character in
/// that part's place, which no sanitized text holds, to find it in any
/// language's word order.
pub(super) fn media_viewer_sender_line(
    line: impl Fn(&str, Option<&str>) -> String,
    program: &str,
    signer: Option<&str>,
) -> kettle_render::MediaLaneSender {
    let text = line(program, signer);
    let find = |marked: String, marker: char, part: &str| {
        marked
            .find(marker)
            .map(|start| start..start + part.len())
            .filter(|range| text.get(range.clone()) == Some(part))
    };
    let program_range = find(line("\u{1}", signer), '\u{1}', program);
    let signer_range =
        signer.and_then(|signer| find(line(program, Some("\u{2}")), '\u{2}', signer));
    kettle_render::MediaLaneSender {
        text,
        program: program_range,
        signer: signer_range,
    }
}

/// The mode a lane shows `item` in: its source only while it has a source to
/// show, as after a replacement by key with a raster it may not.
pub(super) fn lane_mode(
    item: &crate::media::ShelfItem,
    panel: &crate::window_state::PreviewPanel,
) -> kettle_render::MediaLaneMode {
    match item.source.text() {
        Some(_) => panel.mode,
        None => kettle_render::MediaLaneMode::Rendered,
    }
}

/// Whether a lane showing `item` as `panel` says offers a silent preview: a
/// video with a length, shown rendered, whose source the lane can read
/// again.
pub(super) fn lane_previews_item(
    item: &crate::media::ShelfItem,
    panel: &crate::window_state::PreviewPanel,
) -> Option<()> {
    let info = item.video?;
    (info.duration_ms > 0
        && item.kind == crate::media::ItemKind::Media(kettle_media::MediaKind::Video)
        && lane_mode(item, panel) == kettle_render::MediaLaneMode::Rendered
        && !matches!(item.source.spec.input, crate::media::SourceInput::Released))
    .then_some(())
}

pub(super) fn lane_frame(
    item: &crate::media::ShelfItem,
    panel: &crate::window_state::PreviewPanel,
    preview: Option<&crate::media::LanePreview>,
    playback: Option<&crate::media::LanePlayback>,
) -> Option<kettle_render::MediaLaneFrame> {
    lane_previews_item(item, panel)?;
    if let Some(preview) =
        preview.filter(|preview| preview.serves(item.id, item.generation) && preview.active())
    {
        return preview.frame().map(|frame| kettle_render::MediaLaneFrame {
            sheet: frame.sheet.clone(),
            source: frame.source,
            size: item.size,
        });
    }
    let frame = playback
        .filter(|playback| playback.serves(item.id, item.generation))?
        .frame()?;
    Some(kettle_render::MediaLaneFrame {
        sheet: frame.clone(),
        source: (0, 0, frame.width, frame.height),
        size: item.size,
    })
}

pub(super) fn lane_copy_image(
    item: &crate::media::ShelfItem,
    frame: Option<kettle_render::MediaLaneFrame>,
) -> Option<kettle_core::ImageData> {
    let Some(frame) = frame else {
        return item.image().cloned();
    };
    let (x, y, width, height) = frame.source;
    if (x, y, width, height) == (0, 0, frame.sheet.width, frame.sheet.height) {
        Some(frame.sheet)
    } else {
        frame.sheet.crop(x, y, width, height)
    }
}

/// The rest the real pointer makes on a lane's picture after `current`:
/// `hovered` is the lane whose picture it is on, at `at`, at `now`. A rest
/// is a pointer that stays put, so moving starts it again, until a loop
/// started from it; leaving ends it.
pub(super) fn next_hover(
    current: Option<crate::window_state::LaneHover>,
    hovered: Option<u64>,
    at: Option<(f64, f64)>,
    now: std::time::Instant,
) -> Option<crate::window_state::LaneHover> {
    match (hovered, current) {
        (Some(pane), Some(hover)) if hover.pane == pane && (hover.at == at || hover.used) => {
            Some(hover)
        }
        (Some(pane), _) => Some(crate::window_state::LaneHover {
            pane,
            at,
            since: now,
            used: false,
        }),
        (None, _) => None,
    }
}

/// The earlier of two optional instants.
pub(super) fn earliest(
    a: Option<std::time::Instant>,
    b: Option<std::time::Instant>,
) -> Option<std::time::Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

/// Whether `item` can be read again: it came from a file, or its gallery
/// did, not from bytes a request carried or text copied.
pub(super) fn preview_reloads(item: &crate::media::ShelfItem) -> bool {
    item.source.reload_file().is_some()
}

/// What a reload keeps of the item it replaces.
pub(super) struct ReloadOf {
    pub(super) key: crate::media::ShelfKey,
    pub(super) title: String,
    pub(super) canvas: kettle_media::Canvas,
    /// The gallery or document page shown, which the file is read again at.
    pub(super) page: Option<crate::media::ReadPage>,
}

/// What reloading `item`, shown in `panel`, reads and keeps: its file, and
/// its key, title, the canvas the user chose for its lane, which a file
/// that now holds a diagram is rendered for, and a gallery's page shown.
/// None for bytes a request carried, text copied, or a path this platform
/// cannot name.
pub(super) fn reload_of(
    item: &crate::media::ShelfItem,
    panel: &crate::window_state::PreviewPanel,
) -> Option<(std::path::PathBuf, ReloadOf)> {
    let (path, page) = item.source.reload_file()?;
    let native = native_path_buf(path)?;
    let reload = ReloadOf {
        key: item
            .key
            .clone()
            .unwrap_or_else(|| crate::media::ShelfKey::Path(path.clone())),
        title: item.title.clone(),
        canvas: media_canvas(chosen_canvas(item, panel)),
        page,
    };
    Some((native, reload))
}

/// The path a native path names on this platform.
pub(super) fn native_path_buf(path: &kettle_media::NativePath) -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
            path.as_bytes(),
        )))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt as _;
        let (units, rest) = path.as_bytes().as_chunks::<2>();
        if !rest.is_empty() {
            return None;
        }
        let wide: Vec<u16> = units.iter().map(|unit| u16::from_le_bytes(*unit)).collect();
        Some(std::path::PathBuf::from(std::ffi::OsString::from_wide(
            &wide,
        )))
    }
}

/// The shelf item `pane`'s lane shows in `ws`, if it shows one.
pub(super) fn preview_item(ws: &WindowState, pane: u64) -> Option<&crate::media::ShelfItem> {
    let panel = ws.preview_panels.get(&pane)?;
    ws.mux
        .panes
        .get(&pane)?
        .media_shelf
        .items()
        .iter()
        .find(|item| item.id == panel.item)
}

/// What a lane's render of its item is worth when it comes back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LaneRenderVerdict {
    /// The item left the shelf or was replaced, or the render is of a
    /// gallery page it no longer shows: the render is dropped.
    Gone,
    /// What was read is not what the item was rendered from.
    Changed,
    /// The same item from the same source, or the gallery page asked for:
    /// the render replaces its pixels.
    Same,
}

/// Judge a lane's render, which read a source with `digest`, against the
/// shelf `items` it asked about: the item's own source, or for another page
/// of its gallery, that page. Only a file read again can have changed;
/// bytes kept cannot, so a render of other bytes is of a page gone by.
pub(super) fn lane_render_verdict(
    items: &[crate::media::ShelfItem],
    render: &crate::media::LaneRender,
    digest: &kettle_media::Digest,
) -> LaneRenderVerdict {
    let Some(item) = items
        .iter()
        .find(|item| item.id == render.item && item.generation == render.generation)
    else {
        return LaneRenderVerdict::Gone;
    };
    let expected = match render.page {
        Some(ask) => item.source.page_digest(ask.page),
        None => Some(item.source.digest.clone()),
    };
    if expected.as_ref() == Some(digest) {
        LaneRenderVerdict::Same
    } else if (render.page.is_none() || item.source.is_document()) && item.source.reads_file() {
        // Any page of a document reads the file again: a digest that is not
        // the document's is a file that changed.
        LaneRenderVerdict::Changed
    } else {
        LaneRenderVerdict::Gone
    }
}

/// The gallery page request `panel` waits on for its item at `generation`,
/// asked for last and not yet shown: what the lane will show, and where a
/// step counts from.
pub(super) fn page_waited_on(
    panel: &crate::window_state::PreviewPanel,
    generation: u64,
) -> Option<crate::media::PageAsk> {
    panel
        .page
        .filter(|&(asked_of, _)| asked_of == generation)
        .map(|(_, ask)| ask)
}

/// Whether a lane with `panel` takes `render` of gallery page request `ask`,
/// its item showing page `shown`: the request it waits on, or one of the
/// page it shows drawn again on another canvas, for the item and generation
/// it shows. A request for the page it waits on that was asked again since
/// is not taken.
pub(super) fn lane_takes_page(
    panel: &crate::window_state::PreviewPanel,
    render: &crate::media::LaneRender,
    ask: crate::media::PageAsk,
    shown: Option<u32>,
) -> bool {
    panel.item == render.item
        && (page_waited_on(panel, render.generation) == Some(ask) || shown == Some(ask.page))
}

/// The gallery page of `count` that `step` names: its first or last, or so
/// many on from the one asked for and not yet shown, else from the one
/// shown; `None` past the first or last, as a gallery does not wrap.
pub(super) fn gallery_step(
    shown: u32,
    count: usize,
    asked: Option<u32>,
    step: crate::media::PageStep,
) -> Option<u32> {
    use crate::media::PageStep;
    let page = match step {
        PageStep::By(by) => i64::from(asked.unwrap_or(shown)).checked_add(i64::from(by))?,
        PageStep::First => 0,
        PageStep::Last => i64::try_from(count).ok()?.checked_sub(1)?,
    };
    usize::try_from(page)
        .is_ok_and(|page| page < count)
        .then(|| u32::try_from(page).ok())
        .flatten()
}

/// Most columns a source scrolls sideways: past any line a source can hold.
pub(super) const MAX_SOURCE_COLUMN: usize = kettle_media::MAX_EXACT_SOURCE_BYTES;

/// `scroll` moved by `rows` and `columns`, kept on the source: from its
/// first row to its last of `total`, and from its first column.
pub(super) fn scrolled_source(
    scroll: (usize, usize),
    rows: i32,
    columns: i32,
    total: usize,
) -> (usize, usize) {
    let step = |at: usize, by: i32, last: usize| {
        if by < 0 {
            at.saturating_sub(by.unsigned_abs() as usize)
        } else {
            at.saturating_add(by as usize)
        }
        .min(last)
    };
    (
        step(scroll.0, rows, total.saturating_sub(1)),
        step(scroll.1, columns, MAX_SOURCE_COLUMN),
    )
}

/// The rows and columns a lane's source moves for wheel `steps`: back
/// toward its start for the wheel up, and sideways for a horizontal swipe,
/// or the wheel with `shift` held. A positive horizontal step reveals what
/// is to the left, as winit's does; macOS turns a mouse wheel's motion with
/// Shift held into one.
pub(super) fn source_scroll_by(steps: input::WheelSteps, shift: bool) -> (i32, i32) {
    let back = steps.lines.saturating_neg();
    let left = steps.cols.saturating_neg();
    if shift {
        (0, back.saturating_add(left))
    } else {
        (back, left)
    }
}

/// What the wheel does over a lane's rendered item.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum LaneWheel {
    /// Zoom at the pointer by this factor.
    Zoom(f64),
    /// Move the item by these pixels.
    Pan((f64, f64)),
}

/// How far, in logical pixels, a press on a lane's rendered item moves
/// before it pans the item.
const LANE_DRAG_SLOP: f64 = 3.0;

/// Whether a press at `from` moved to `at`, in physical pixels at `scale`,
/// is a drag.
pub(super) fn past_drag_slop(from: (f64, f64), at: (f64, f64), scale: f64) -> bool {
    (at.0 - from.0).hypot(at.1 - from.1) >= LANE_DRAG_SLOP * scale
}

/// Most zoom steps one wheel event takes, so a burst a control client or a
/// device sends stays a zoom rather than a jump to an end.
const MAX_ZOOM_NOTCHES: f64 = 10.0;

/// What `delta` does over a lane's rendered item: zoom with `zooms` held
/// (Cmd on macOS, Ctrl elsewhere), a step per notch or per notch's worth of
/// a touchpad's pixels; otherwise pan, by a touchpad's pixels as they come
/// and by a wheel's lines a notch, in `cell`s, as the terminal scrolls.
/// Shift turns a wheel's vertical motion sideways.
pub(super) fn lane_wheel(
    delta: &winit::event::MouseScrollDelta,
    zooms: bool,
    shift: bool,
    cell: (f32, f32),
    multiplier: f32,
) -> LaneWheel {
    use winit::event::MouseScrollDelta as D;
    let (notches, pan) = match *delta {
        D::LineDelta(x, y) => {
            let notches = f64::from(y);
            let (x, y) = (f64::from(x), f64::from(y));
            let (x, y) = if shift && x == 0.0 { (y, 0.0) } else { (x, y) };
            let lines = f64::from(input::LINES_PER_NOTCH * multiplier.max(0.0));
            (
                notches,
                (x * lines * f64::from(cell.0), y * lines * f64::from(cell.1)),
            )
        }
        D::PixelDelta(position) => (
            position.y / f64::from(input::PIXELS_PER_NOTCH),
            (position.x, position.y),
        ),
    };
    if zooms {
        LaneWheel::Zoom(
            kettle_render::MEDIA_ZOOM_STEP.powf(notches.clamp(-MAX_ZOOM_NOTCHES, MAX_ZOOM_NOTCHES)),
        )
    } else {
        LaneWheel::Pan(pan)
    }
}

/// Whether `mods` hold the key that turns the wheel over a lane's rendered
/// item into a zoom: Cmd on macOS, Ctrl elsewhere.
pub(super) fn lane_zoom_modifier(mods: winit::keyboard::ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        mods.super_key()
    } else {
        mods.control_key()
    }
}

/// What whole wheel `steps` a control client sent do over a lane's
/// rendered item, as [`lane_wheel`] does with a wheel's lines: zoom a step
/// a notch with `zooms` held, or pan a row a line and three columns a
/// sideways notch, the lines sideways with Shift.
pub(super) fn lane_wheel_steps(
    steps: input::WheelSteps,
    zooms: bool,
    shift: bool,
    cell: (f32, f32),
) -> LaneWheel {
    if zooms {
        let notches = f64::from(steps.notches).clamp(-MAX_ZOOM_NOTCHES, MAX_ZOOM_NOTCHES);
        return LaneWheel::Zoom(kettle_render::MEDIA_ZOOM_STEP.powf(notches));
    }
    let columns = f64::from(steps.cols) * f64::from(input::LINES_PER_NOTCH);
    let lines = f64::from(steps.lines);
    let (x, y) = if shift && steps.cols == 0 {
        (lines, 0.0)
    } else {
        (columns, lines)
    };
    LaneWheel::Pan((x * f64::from(cell.0), y * f64::from(cell.1)))
}

/// The zoom a pinch of `delta` makes, as winit counts it: positive
/// magnifies. Bounded, so no one event shrinks the item to nothing.
pub(super) fn pinch_zoom(delta: f64) -> f64 {
    if delta.is_finite() {
        1.0 + delta.clamp(-0.5, 1.0)
    } else {
        1.0
    }
}

/// The part of `source` a lane shows from `scroll`
/// in `rows` rows of `columns` columns. A source replaced under its key may
/// be shorter than the one scrolled, so the scroll is kept on it.
pub(super) fn lane_source(
    source: &crate::media::ItemSource,
    scroll: (usize, usize),
    rows: usize,
    columns: usize,
) -> kettle_render::MediaLaneSource {
    let total = source.text_rows();
    let (first_row, first_column) = scrolled_source(scroll, 0, 0, total);
    kettle_render::MediaLaneSource {
        rows: source.display_rows(first_row, rows, first_column, columns),
        first_row,
        total_rows: total,
    }
}

/// The worker's canvas for a lane's.
pub(super) fn media_canvas(canvas: kettle_render::MediaCanvas) -> kettle_media::Canvas {
    match canvas {
        kettle_render::MediaCanvas::Theme => kettle_media::Canvas::Theme,
        kettle_render::MediaCanvas::White => kettle_media::Canvas::White,
        kettle_render::MediaCanvas::Checker => kettle_media::Canvas::Checker,
    }
}

/// The canvas a lane shows `item` on. The user's choice for a raster or an
/// SVG is drawn behind its pixels; a diagram's is the one its pixels were
/// rendered for, as its colors follow the canvas, until a render for the
/// choice comes back. Otherwise the kind's own: white for SVG, a checkerboard
/// for a raster's transparency, the pane's background for a diagram.
pub(super) fn preview_canvas(
    item: &crate::media::ShelfItem,
    panel: &crate::window_state::PreviewPanel,
) -> kettle_render::MediaCanvas {
    use kettle_render::MediaCanvas as C;
    match item.kind {
        kind if crate::media::draws_as_diagram(kind) => match item.source.spec.canvas {
            kettle_media::Canvas::Theme => C::Theme,
            kettle_media::Canvas::White => C::White,
            kettle_media::Canvas::Checker => C::Checker,
        },
        crate::media::ItemKind::Media(kettle_media::MediaKind::Svg) => {
            panel.canvas.unwrap_or(C::White)
        }
        crate::media::ItemKind::Media(kettle_media::MediaKind::Raster) => {
            panel.canvas.unwrap_or(C::Checker)
        }
        _ => panel.canvas.unwrap_or(C::Theme),
    }
}

/// The canvas the user chose for `item`'s lane, or else the one it shows.
pub(super) fn chosen_canvas(
    item: &crate::media::ShelfItem,
    panel: &crate::window_state::PreviewPanel,
) -> kettle_render::MediaCanvas {
    panel.canvas.unwrap_or_else(|| preview_canvas(item, panel))
}

/// What a lane says in place of its hint after `notice`.
pub(super) fn lane_notice_text(
    notice: crate::window_state::LaneNotice,
    tr: &kettle_i18n::Translator,
) -> &'static str {
    use crate::window_state::LaneNotice as N;
    use kettle_i18n::Text as T;
    tr.text(match notice {
        N::ImageCopied => T::MediaLaneNoticeImageCopied,
        N::SourceCopied => T::MediaLaneNoticeSourceCopied,
        N::CopyFailed => T::MediaLaneNoticeCopyFailed,
        N::CopyBusy => T::MediaLaneNoticeCopyBusy,
        N::Rendering => T::MediaLaneNoticeRendering,
        N::RenderingPage => T::MediaLaneNoticeRenderingPage,
        N::RenderingDocumentPage => T::MediaLaneNoticeRenderingDocumentPage,
        N::RenderFailed => T::MediaLaneNoticeRenderFailed,
        N::Changed => T::MediaLaneNoticeChanged,
        N::PreviewFailed => T::MediaLaneNoticePreviewFailed,
        N::PreviewTimedOut => T::MediaLaneNoticePreviewTimedOut,
        N::PreviewNeedsDecoder => T::MediaLaneNoticePreviewNeedsDecoder,
        N::PreviewNeedsSandbox => T::MediaLaneNoticePreviewNeedsSandbox,
        N::VideoFailed => T::MediaLaneNoticeVideoFailed,
        N::VideoTimedOut => T::MediaLaneNoticeVideoTimedOut,
        N::VideoNeedsDecoder => T::MediaLaneNoticeVideoNeedsDecoder,
        N::VideoNeedsSandbox => T::MediaLaneNoticeVideoNeedsSandbox,
    })
}

/// What a screen reader hears that a lane last did, given its `preview` and
/// its `notice`: that a preview is being made, else the notice. A preview
/// that plays is said by its button's name, never frame by frame.
pub(super) fn lane_announced(
    preview: Option<&crate::media::LanePreview>,
    notice: Option<crate::window_state::LaneNotice>,
    tr: &kettle_i18n::Translator,
) -> Option<String> {
    if preview.is_some_and(|preview| preview.waits_on().is_some()) {
        return Some(
            tr.text(kettle_i18n::Text::MediaLaneNoticePreviewLoading)
                .to_string(),
        );
    }
    notice.map(|notice| lane_notice_text(notice, tr).to_string())
}

/// Whether a container's video may go to Apple's decoder, the only one
/// that plays sound: MP4 and QuickTime on macOS, or a container not known
/// yet; elsewhere there is no Apple decoder to keep it from.
pub(super) fn apple_reads(container: Option<kettle_media::video::VideoContainer>) -> bool {
    use kettle_media::video::VideoContainer as C;
    !cfg!(target_os = "macos")
        || container.is_none_or(|container| matches!(container, C::IsoBmff | C::QuickTime))
}

/// A playing video's sound in ctl `ui_geometry`: heard, muted, or why there
/// is none.
pub(super) fn playback_sound_name(sound: crate::media::PlaybackSound) -> &'static str {
    use crate::media::PlaybackSound as S;
    use kettle_media::AudioStatus as A;
    match sound {
        S::Heard => "heard",
        S::Muted => "muted",
        S::Silent(A::NoTrack) => "no_track",
        S::Silent(A::DecoderSilent) => "decoder_silent",
        S::Silent(A::Unsupported) => "unsupported",
        S::Silent(A::Unavailable) => "unavailable",
        S::Silent(A::Off | A::Ready) => "off",
    }
}

/// What a lane's footer says of a video playing without sound, by why.
pub(super) fn silence_text(why: kettle_media::AudioStatus) -> kettle_i18n::Text {
    use kettle_i18n::Text as T;
    use kettle_media::AudioStatus as A;
    match why {
        A::NoTrack => T::MediaLaneNoticeVideoNoSoundTrack,
        A::DecoderSilent => T::MediaLaneNoticeVideoSilentDecoder,
        A::Unsupported => T::MediaLaneNoticeVideoSoundUnsupported,
        A::Unavailable => T::MediaLaneNoticeVideoSoundUnavailable,
        // Sound that can play is heard or muted, never silent.
        A::Off | A::Ready => T::MediaLaneNoticeVideoPlaying,
    }
}

/// What a lane says when its video stops playing, or cannot open, with
/// `failure`: which decoder to install, that there is no sandbox, that the
/// file changed, that it took too long, or that it stopped.
pub(super) fn playback_failure_notice(
    failure: kettle_media::FailureCode,
) -> crate::window_state::LaneNotice {
    use crate::window_state::LaneNotice as N;
    use kettle_media::FailureCode as F;
    match failure {
        F::UnsupportedContainer | F::CodecUnavailable | F::BackendUnavailable => {
            N::VideoNeedsDecoder
        }
        F::SandboxUnavailable => N::VideoNeedsSandbox,
        F::Changed => N::Changed,
        F::RenderTimeout => N::VideoTimedOut,
        _ => N::VideoFailed,
    }
}

/// What a lane says when its video's preview fails with `failure`: which
/// decoder to install, that there is no sandbox, that the file changed, or
/// that it could not be made.
pub(super) fn preview_failure_notice(
    failure: kettle_media::FailureCode,
) -> crate::window_state::LaneNotice {
    use crate::window_state::LaneNotice as N;
    use kettle_media::FailureCode as F;
    match failure {
        F::UnsupportedContainer | F::CodecUnavailable | F::BackendUnavailable => {
            N::PreviewNeedsDecoder
        }
        F::SandboxUnavailable => N::PreviewNeedsSandbox,
        F::Changed => N::Changed,
        F::RenderTimeout => N::PreviewTimedOut,
        _ => N::PreviewFailed,
    }
}

/// Preview lanes and their controls, each by pane: the lane at part 0 and
/// its controls after it. Pane ids at or past [`MAX_LANE_ACCESSIBILITY_PANE`]
/// have no lane nodes rather than ones that could collide.
pub(super) const ACCESSIBILITY_LANE_ID_MASK: u64 = 1 << 56;
pub(super) const MAX_LANE_ACCESSIBILITY_PANE: u64 = 1 << 51;
pub(super) const LANE_ACCESSIBILITY_PARTS: u64 = 32;
/// A lane's controls, at parts 1 and on (see [`lane_controls`]).
pub(super) const LANE_CONTROLS: usize = 18;
/// A playing video's bar, a slider: the part after the controls', below
/// the stride.
pub(super) const LANE_SCRUBBER_PART: u64 = 17;
const LANE_PIN_PART: u64 = LANE_SCRUBBER_PART + 1;
const _: () = assert!(LANE_PIN_PART < LANE_ACCESSIBILITY_PARTS);

/// How far a step along a playing video's bar moves it, from the keyboard
/// or an assistive technology.
pub(super) const LANE_SEEK_STEP: std::time::Duration = std::time::Duration::from_secs(5);

/// How often a window whose lanes have video sessions comes round to keep
/// them: each session's worker ends it after five seconds without word, and
/// its client sends word only while the lane keeps it.
pub(super) const LANE_PLAYBACK_KEEP: std::time::Duration = std::time::Duration::from_secs(1);

/// A lane's controls in order, with the part each answers to.
pub(super) fn lane_controls(
    geometry: &kettle_render::MediaLaneGeometry,
) -> [(
    u64,
    Option<kettle_render::Rect4>,
    kettle_render::MediaLaneHit,
); LANE_CONTROLS] {
    use kettle_render::MediaLaneHit as Hit;
    [
        (1, geometry.previous, Hit::Previous),
        (2, geometry.next, Hit::Next),
        (3, geometry.open_outside, Hit::OpenOutside),
        (
            if geometry.pinning { LANE_PIN_PART } else { 4 },
            Some(geometry.toggle),
            if geometry.pinning {
                Hit::Pin
            } else {
                Hit::Toggle
            },
        ),
        (5, Some(geometry.close), Hit::Close),
        (6, geometry.mode, Hit::Mode),
        (7, geometry.canvas, Hit::Canvas),
        (8, geometry.copy, Hit::Copy),
        (9, geometry.reload, Hit::Reload),
        (10, geometry.zoom_out, Hit::ZoomOut),
        (11, geometry.zoom_in, Hit::ZoomIn),
        (12, geometry.zoom_fit, Hit::Fit),
        (13, geometry.page_previous, Hit::PreviousPage),
        (14, geometry.page_next, Hit::NextPage),
        (15, geometry.play, Hit::Play),
        (16, geometry.sound, Hit::Sound),
        (19, geometry.theater, Hit::Theater),
        (20, geometry.fullscreen, Hit::Fullscreen),
    ]
}

pub(super) fn lane_interactive_controls(
    lane: &kettle_render::MediaLanePanel,
    geometry: &kettle_render::MediaLaneGeometry,
) -> Vec<(u64, Rect, kettle_render::MediaLaneHit)> {
    lane_controls(geometry)
        .into_iter()
        .chain(geometry.scrubber.map(|rect| {
            (
                LANE_SCRUBBER_PART,
                Some(rect),
                kettle_render::MediaLaneHit::Scrub,
            )
        }))
        .filter_map(|(part, rect, hit)| Some((part, rect?, hit)))
        .filter(|(_, _, hit)| lane_control_enabled(lane, *hit))
        .collect()
}

pub(super) fn lane_control_enabled(
    lane: &kettle_render::MediaLanePanel,
    hit: kettle_render::MediaLaneHit,
) -> bool {
    use kettle_render::MediaLaneHit as Hit;
    match hit {
        Hit::Previous => lane.position.0 > 1,
        Hit::Next => lane.position.0 < lane.position.1,
        Hit::PreviousPage => lane.page.is_some_and(|(page, _)| page > 1),
        Hit::NextPage => lane.page.is_some_and(|(page, count)| page < count),
        _ => true,
    }
}

/// A lane control's name in ctl `ui_geometry`.
pub(super) fn lane_control_name(hit: kettle_render::MediaLaneHit) -> &'static str {
    use kettle_render::MediaLaneHit as Hit;
    match hit {
        Hit::Previous => "previous",
        Hit::Next => "next",
        Hit::PreviousPage => "previous_page",
        Hit::NextPage => "next_page",
        Hit::OpenOutside => "open_outside",
        Hit::Toggle => "toggle",
        Hit::Pin => "pin",
        Hit::Close => "close",
        Hit::Mode => "mode",
        Hit::Canvas => "canvas",
        Hit::Copy => "copy",
        Hit::Reload => "reload",
        Hit::Play => "play",
        Hit::Sound => "sound",
        Hit::Theater => "theater",
        Hit::Fullscreen => "fullscreen",
        Hit::Scrub => "scrubber",
        Hit::ZoomOut => "zoom_out",
        Hit::ZoomIn => "zoom_in",
        Hit::Fit => "fit",
        Hit::Resize => "resize",
        Hit::Inside => "inside",
    }
}

/// What the clipboard names for a lane to show.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CopiedPreview {
    /// A file a file manager copied: on this computer, as no pane's output
    /// can put a file on the clipboard.
    File(std::path::PathBuf),
    /// A copied path or `file://` link, as a `file://` URL. It names a file
    /// wherever the pane it was copied from runs, so the pane's gate decides.
    Link(String),
}

/// The lane control a lane action works as, for the focused pane's lane.
pub(super) fn lane_action_hit(action: &Action) -> Option<kettle_render::MediaLaneHit> {
    use kettle_render::MediaLaneHit as Hit;
    match action {
        Action::PreviewNext => Some(Hit::Next),
        Action::PreviewPrevious => Some(Hit::Previous),
        Action::PreviewNextDiagram => Some(Hit::NextPage),
        Action::PreviewPreviousDiagram => Some(Hit::PreviousPage),
        Action::ClosePreview => Some(Hit::Close),
        Action::PreviewSource => Some(Hit::Mode),
        Action::PreviewCanvas => Some(Hit::Canvas),
        Action::PreviewCopy => Some(Hit::Copy),
        Action::PreviewReload => Some(Hit::Reload),
        Action::VideoPlay => Some(Hit::Play),
        Action::VideoMute => Some(Hit::Sound),
        Action::PreviewZoomIn => Some(Hit::ZoomIn),
        Action::PreviewZoomOut => Some(Hit::ZoomOut),
        Action::PreviewFit => Some(Hit::Fit),
        _ => None,
    }
}

/// Why a control client's keys or text are refused while a preview lane in
/// the pane's window holds the keyboard.
pub(super) const PREVIEW_HOLDS_KEYBOARD: &str =
    "a preview lane in this pane's window holds the keyboard; nothing is typed until Esc";
