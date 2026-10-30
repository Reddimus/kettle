use super::{
    Align, Attrs, Family, FontSystem, Language, Metrics, Rect4, Renderer, Shaping, Text,
    TextBuffer, Translator, display_width, fit_single_line_label,
};
#[cfg(test)]
use super::{Rgb, color};

/// Case policy shown by the search bar and applied by the UI's search engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchCaseMode {
    /// Match case only when the query contains an uppercase character.
    #[default]
    Smart,
    /// Always match case.
    Match,
    /// Always ignore case.
    Ignore,
}

impl SearchCaseMode {
    pub const ALL: [Self; 3] = [Self::Smart, Self::Match, Self::Ignore];

    /// The mode's name in `tr`'s language, as the Settings row shows it.
    pub fn label(self, tr: &Translator) -> &'static str {
        tr.text(match self {
            Self::Smart => Text::SettingsValueSmart,
            Self::Match => Text::SettingsValueMatchCase,
            Self::Ignore => Text::SettingsValueIgnoreCase,
        })
    }
}

/// Bounded semantic search feedback. Deliberately carries no unbounded regex
/// error string: diagnostic details belong in logs, while chrome stays stable
/// and cannot expose the query through accessibility/automation snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchStatus {
    #[default]
    Typing,
    Searching,
    Match,
    Wrapped,
    Start,
    End,
    NoMatch,
    Limited,
    Invalid,
    TooComplex,
    TooLong,
}

impl SearchStatus {
    pub const ALL: [Self; 11] = [
        Self::Typing,
        Self::Searching,
        Self::Match,
        Self::Wrapped,
        Self::Start,
        Self::End,
        Self::NoMatch,
        Self::Limited,
        Self::Invalid,
        Self::TooComplex,
        Self::TooLong,
    ];

    pub fn label(self, tr: &Translator) -> &'static str {
        tr.text(match self {
            Self::Typing => Text::SearchStatusTyping,
            Self::Searching => Text::SearchStatusSearching,
            Self::Match => Text::SearchStatusMatch,
            Self::Wrapped => Text::SearchStatusWrapped,
            Self::Start => Text::SearchStatusStart,
            Self::End => Text::SearchStatusEnd,
            Self::NoMatch => Text::SearchStatusNoMatch,
            Self::Limited => Text::SearchStatusLimited,
            Self::Invalid => Text::SearchStatusInvalid,
            Self::TooComplex => Text::SearchStatusTooComplex,
            Self::TooLong => Text::SearchStatusTooLong,
        })
    }
}

/// Focusable elements in the search lane. This renderer-owned enum is shared
/// by paint geometry, pointer hit-testing, and the UI's AccessKit projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SearchControl {
    #[default]
    Editor,
    Previous,
    Next,
    Wrap,
    Case,
    Invert,
    Close,
}

impl SearchControl {
    pub const ALL: [Self; 7] = [
        Self::Editor,
        Self::Previous,
        Self::Next,
        Self::Wrap,
        Self::Case,
        Self::Invert,
        Self::Close,
    ];

    pub fn accessible_label(self, tr: &Translator) -> &'static str {
        tr.text(match self {
            Self::Editor => Text::SearchA11yEditor,
            Self::Previous => Text::SearchA11yPrevious,
            Self::Next => Text::SearchA11yNext,
            Self::Wrap => Text::SearchA11yWrap,
            Self::Case => Text::SearchA11yCase,
            Self::Invert => Text::SearchA11yInvert,
            Self::Close => Text::SearchA11yClose,
        })
    }
}

/// Full renderer projection for the in-window search lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOverlay {
    /// The UI's language. The bar's words and widths follow it.
    pub tr: Translator,
    /// Stable pane identity owning the terminal-coordinate highlight spans.
    /// Chrome remains window-local, but a focus change must never project one
    /// pane's signed grid coordinates onto another pane.
    pub target_pane: Option<u64>,
    pub query: String,
    /// UTF-8 byte offset of the editor caret. Invalid offsets are clamped to the
    /// nearest preceding character boundary during paint.
    pub cursor_byte: usize,
    /// Optional UTF-8 byte range selected in the editor.
    pub selection: Option<(usize, usize)>,
    /// Number of display columns intentionally hidden on the left.
    pub horizontal_scroll: usize,
    pub wrap: bool,
    pub case_mode: SearchCaseMode,
    pub invert: bool,
    pub status: SearchStatus,
    pub focused: SearchControl,
    /// Control under the pointer, drawn with a hover fill.
    pub hovered: Option<SearchControl>,
    /// Button held down by the pointer. It activates on release over it.
    pub pressed: Option<SearchControl>,
}

#[cfg(any(test, feature = "test-defaults"))]
impl Default for SearchOverlay {
    fn default() -> Self {
        Self {
            tr: Translator::default(),
            target_pane: None,
            query: String::new(),
            cursor_byte: 0,
            selection: None,
            horizontal_scroll: 0,
            wrap: true,
            case_mode: SearchCaseMode::Smart,
            invert: false,
            status: SearchStatus::Typing,
            focused: SearchControl::Editor,
            hovered: None,
            pressed: None,
        }
    }
}

/// Shared paint/input/accessibility geometry for the reserved search lane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchBarGeometry {
    pub rect: Rect4,
    pub rows: usize,
    pub reserved_height: f32,
    pub label: Rect4,
    pub editor: Rect4,
    pub previous: Rect4,
    pub next: Rect4,
    pub wrap: Rect4,
    pub case_mode: Rect4,
    pub invert: Rect4,
    pub status: Rect4,
    pub close: Rect4,
}

impl SearchBarGeometry {
    pub const fn control_rect(self, control: SearchControl) -> Rect4 {
        match control {
            SearchControl::Editor => self.editor,
            SearchControl::Previous => self.previous,
            SearchControl::Next => self.next,
            SearchControl::Wrap => self.wrap,
            SearchControl::Case => self.case_mode,
            SearchControl::Invert => self.invert,
            SearchControl::Close => self.close,
        }
    }

    /// Resolve a pointer position using the exact rectangles used for paint.
    pub fn hit_test(self, x: f32, y: f32) -> Option<SearchControl> {
        SearchControl::ALL.into_iter().find(|control| {
            let (rx, ry, rw, rh) = self.control_rect(*control);
            rw > 0.0 && rh > 0.0 && x >= rx && x < rx + rw && y >= ry && y < ry + rh
        })
    }

    /// Terminal content rectangle after reserving the lane. The x/width are
    /// unchanged; height saturates at zero for extremely short surfaces.
    pub fn content_rect(self, surface_width: f32, surface_height: f32) -> Rect4 {
        (
            0.0,
            0.0,
            surface_width.max(0.0),
            (surface_height - self.reserved_height).max(0.0),
        )
    }
}

impl Renderer {
    /// Glyph boundaries of the search query painted last frame, as (query
    /// byte, window x), sorted by byte. Empty while the search lane is closed.
    /// Pointer input maps a click to the nearest one, so the caret lands where
    /// it is drawn whatever width the font gives each glyph.
    pub fn search_editor_stops(&self) -> &[(usize, f32)] {
        &self.search_editor_stops
    }

    /// The clusters of the search query painted last frame, sorted by byte.
    /// Pointer input uses them to find the character under a click.
    pub fn search_editor_clusters(&self) -> &[SearchEditorCluster] {
        &self.search_editor_clusters
    }

    /// Whether the search query painted last frame reads right to left, so
    /// its start is at its right end.
    pub fn search_editor_rtl(&self) -> bool {
        self.search_editor_rtl
    }
}

/// Column widths of the search bar's fixed controls, from their labels in a
/// language. Each control keeps the fill it has around its longest English
/// label and is never narrower than in English, so English lays out exactly as
/// it always has and a longer translation gets room instead of an ellipsis.
/// Status is text, not a button, and has no fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SearchBarWidths {
    pub(crate) label: usize,
    pub(crate) previous: usize,
    pub(crate) next: usize,
    pub(crate) wrap: usize,
    pub(crate) case: usize,
    pub(crate) invert: usize,
    pub(crate) status: usize,
    pub(crate) close: usize,
}

impl SearchBarWidths {
    /// The English widths: each control's longest label plus its fill.
    pub(crate) const ENGLISH: Self = Self {
        label: 7,
        previous: 8,
        next: 8,
        wrap: 10,
        case: 15,
        invert: 12,
        status: 19,
        close: 8,
    };

    pub(crate) fn new(tr: &Translator) -> Self {
        let en = Translator::new(Language::En);
        let longest = |tr: &Translator| SearchBarLabels::new(tr).widest();
        let (english, here) = (longest(&en), longest(tr));
        let fit = |width: usize, english: usize, here: usize| {
            width.max(here + width.saturating_sub(english))
        };
        let w = Self::ENGLISH;
        Self {
            label: fit(w.label, english.label, here.label),
            previous: fit(w.previous, english.previous, here.previous),
            next: fit(w.next, english.next, here.next),
            wrap: fit(w.wrap, english.wrap, here.wrap),
            case: fit(w.case, english.case, here.case),
            invert: fit(w.invert, english.invert, here.invert),
            status: fit(w.status, english.status, here.status),
            close: fit(w.close, english.close, here.close),
        }
    }
}

/// The search bar's words in one language.
struct SearchBarLabels {
    label: &'static str,
    previous: &'static str,
    next: &'static str,
    wrap: [&'static str; 2],
    case: Vec<String>,
    invert: [&'static str; 2],
    close: &'static str,
    tr: Translator,
}

impl SearchBarLabels {
    fn new(tr: &Translator) -> Self {
        Self {
            label: tr.text(Text::SearchLabel),
            previous: tr.text(Text::SearchPrev),
            next: tr.text(Text::SearchNext),
            wrap: [tr.text(Text::SearchWrapOn), tr.text(Text::SearchWrapOff)],
            case: SearchCaseMode::ALL
                .iter()
                .map(|mode| search_case_text(tr, *mode))
                .collect(),
            invert: [
                tr.text(Text::SearchEnterNext),
                tr.text(Text::SearchEnterPrev),
            ],
            close: tr.text(Text::SearchClose),
            tr: *tr,
        }
    }

    /// Each control's widest label, in display columns.
    fn widest(&self) -> SearchBarWidths {
        let widest = |labels: &[&str]| labels.iter().map(|l| display_width(l)).max().unwrap_or(0);
        SearchBarWidths {
            label: display_width(self.label),
            previous: display_width(self.previous),
            next: display_width(self.next),
            wrap: widest(&self.wrap),
            case: self
                .case
                .iter()
                .map(|l| display_width(l))
                .max()
                .unwrap_or(0),
            invert: widest(&self.invert),
            status: SearchStatus::ALL
                .iter()
                .map(|status| display_width(status.label(&self.tr)))
                .max()
                .unwrap_or(0),
            close: display_width(self.close),
        }
    }
}

/// The Case button's text. The mode name is padded to the longest mode in the
/// same language, so the chevron stays put as the mode cycles.
fn search_case_text(tr: &Translator, mode: SearchCaseMode) -> String {
    let width = SearchCaseMode::ALL
        .iter()
        .map(|mode| mode.label(tr).chars().count())
        .max()
        .unwrap_or(0);
    tr.search_case(&format!("{:<width$}", mode.label(tr)))
}

/// Compute the responsive, bottom-reserved search lane.
///
/// Wide surfaces use one row. Narrow surfaces keep the editor and Close on the
/// first row and wrap every other control onto as many rows as needed. Nothing
/// is silently omitted, which gives keyboard, pointer, and accessibility users
/// the same control set at every supported window size.
pub fn search_bar_geometry(
    surface_width: f32,
    surface_height: f32,
    cell_width: f32,
    cell_height: f32,
    tr: &Translator,
) -> SearchBarGeometry {
    let zero = (0.0, 0.0, 0.0, 0.0);
    if !surface_width.is_finite()
        || !surface_height.is_finite()
        || !cell_width.is_finite()
        || !cell_height.is_finite()
        || surface_width <= 0.0
        || surface_height <= 0.0
        || cell_width <= 0.0
        || cell_height <= 0.0
    {
        return SearchBarGeometry {
            rect: zero,
            rows: 1,
            reserved_height: 0.0,
            label: zero,
            editor: zero,
            previous: zero,
            next: zero,
            wrap: zero,
            case_mode: zero,
            invert: zero,
            status: zero,
            close: zero,
        };
    }

    // The widths read as the layout's column constants they used to be.
    #[allow(non_snake_case)]
    let SearchBarWidths {
        label: LABEL,
        previous: PREVIOUS,
        next: NEXT,
        wrap: WRAP,
        case: CASE,
        invert: INVERT,
        status: STATUS,
        close: CLOSE,
    } = SearchBarWidths::new(tr);
    const EDITOR_MIN: usize = 12;
    // 8 single-column gaps between the nine controls, plus one extra column
    // for each of the four gaps that separate groups rather than siblings
    // (after the editor, after Next, after Invert, after Status).
    let wide_min =
        2 + LABEL + PREVIOUS + NEXT + WRAP + CASE + INVERT + STATUS + CLOSE + EDITOR_MIN + 8 + 4;

    let columns = (surface_width / cell_width).floor().max(1.0) as usize;
    let pad = usize::from(columns >= 4);
    let right = columns.saturating_sub(pad);
    let (
        label_pos,
        editor_pos,
        previous_pos,
        next_pos,
        wrap_pos,
        case_pos,
        invert_pos,
        status_pos,
        close_pos,
        rows,
    ) = if columns >= wide_min {
        let mut col = pad;
        // `gap` is the run of blank columns AFTER a control. One column
        // separates controls inside a group; two separate the groups
        // themselves, so the row reads as query / navigation / options /
        // outcome / close instead of one undifferentiated strip. Without the
        // wider gap the option group runs straight into the status text and
        // `Enter: Prev Match` reads as a single phrase.
        let mut place = |width: usize, gap: usize| {
            let here = col;
            col += width + gap;
            (here, 0, width)
        };
        const IN_GROUP: usize = 1;
        const BETWEEN_GROUPS: usize = 2;
        let label_pos = place(LABEL, IN_GROUP);
        // All fixed controls plus their inter-control gaps have already been
        // budgeted by WIDE_MIN; the editor gets every surplus column.
        let editor_width = EDITOR_MIN + (columns - wide_min);
        let editor_pos = place(editor_width, BETWEEN_GROUPS);
        let previous_pos = place(PREVIOUS, IN_GROUP);
        let next_pos = place(NEXT, BETWEEN_GROUPS);
        let wrap_pos = place(WRAP, IN_GROUP);
        let case_pos = place(CASE, IN_GROUP);
        let invert_pos = place(INVERT, BETWEEN_GROUPS);
        let status_pos = place(STATUS, BETWEEN_GROUPS);
        let close_pos = (col, 0, CLOSE.min(right.saturating_sub(col)).max(1));
        (
            label_pos,
            editor_pos,
            previous_pos,
            next_pos,
            wrap_pos,
            case_pos,
            invert_pos,
            status_pos,
            close_pos,
            1,
        )
    } else {
        // The editor and Close are invariant. The label collapses before either
        // interactive element; at a physically one-column width Close gets its
        // own row so neither hit target overlaps or disappears.
        let inner = right.saturating_sub(pad).max(1);
        let close_on_own_row = inner == 1;
        let close_width = CLOSE.min((inner / 3).max(1));
        let label_width = if inner >= 24 {
            LABEL
        } else if inner >= 14 {
            2 // compact "? " label, populated by the text builder
        } else {
            0
        };
        let gaps = usize::from(label_width > 0) + usize::from(!close_on_own_row);
        let editor_width = inner
            .saturating_sub(label_width + usize::from(!close_on_own_row) * close_width + gaps)
            .max(1);
        let label_pos = (pad, 0, label_width);
        let editor_col = pad + label_width + usize::from(label_width > 0);
        let editor_pos = (editor_col, 0, editor_width);
        let close_pos = if close_on_own_row {
            (pad, 1, 1)
        } else {
            (
                (editor_col + editor_width + 1).min(right.saturating_sub(1)),
                0,
                close_width.min(right.max(1)),
            )
        };

        // Pack the secondary row(s) with deterministic source order. Each
        // component may shrink on exceptionally narrow windows, but none is
        // hidden. Status is geometry, not a focusable control.
        // Same grouping the wide branch uses: one column between siblings, two
        // where one group ends and the next begins. Without it the narrow rows
        // render `Enter: Next No match`, because `Enter: Next` fills its slot
        // exactly and a single space does not read as a boundary between a
        // control and the outcome text.
        let desired = [
            (PREVIOUS, 1usize),
            (NEXT, 2),
            (WRAP, 1),
            (CASE, 1),
            (INVERT, 2),
            (STATUS, 1),
        ];
        let usable = inner.max(1);
        let mut packed = [(0usize, 0usize, 1usize); 6];
        let mut row = if close_on_own_row { 2 } else { 1 };
        let mut col = pad;
        for (idx, (wanted, gap)) in desired.into_iter().enumerate() {
            let width = wanted.min(usable).max(1);
            if col > pad && col + width > right {
                row += 1;
                col = pad;
            }
            packed[idx] = (col, row, width);
            // A group boundary never justifies pushing a control off the row.
            // `right` is an exclusive end, so a control fits while
            // `col + width <= right`; the next one starts at `col + width +
            // gap` and is already off the row when that equals `right`. Compare
            // with `>=`, not `>`, or the wider gap wraps a control that the
            // single-column gap would have kept.
            let gap = if col + width + gap >= right { 1 } else { gap };
            col = col.saturating_add(width + gap);
        }
        let [
            previous_pos,
            next_pos,
            wrap_pos,
            case_pos,
            invert_pos,
            status_pos,
        ] = packed;
        (
            label_pos,
            editor_pos,
            previous_pos,
            next_pos,
            wrap_pos,
            case_pos,
            invert_pos,
            status_pos,
            close_pos,
            row + 1,
        )
    };

    let natural_row_height = cell_height + 10.0;
    let reserved_height = (natural_row_height * rows as f32).min(surface_height);
    let row_height = reserved_height / rows as f32;
    let top = surface_height - reserved_height;
    let to_rect = |(col, row, width): (usize, usize, usize)| -> Rect4 {
        let x = (col as f32 * cell_width).min(surface_width);
        let max_width = (surface_width - x).max(0.0);
        (
            x,
            top + row as f32 * row_height,
            (width as f32 * cell_width).min(max_width),
            row_height,
        )
    };

    SearchBarGeometry {
        rect: (0.0, top, surface_width, reserved_height),
        rows,
        reserved_height,
        label: to_rect(label_pos),
        editor: to_rect(editor_pos),
        previous: to_rect(previous_pos),
        next: to_rect(next_pos),
        wrap: to_rect(wrap_pos),
        case_mode: to_rect(case_pos),
        invert: to_rect(invert_pos),
        status: to_rect(status_pos),
        close: to_rect(close_pos),
    }
}

/// Recover a control's width in cells from the pixel rect the geometry built.
///
/// `search_bar_geometry` lays the bar out in whole columns and then multiplies
/// by `cell_width` to store pixels, so this has to divide back. A plain
/// `floor` loses a column whenever `n * cell_width / cell_width` lands a few
/// ULP below `n` in f32, which for a cell width like `7.007` turns an 11-column
/// slot into 10 and ellipsizes the label inside it. Sweeping cell widths
/// 6.000–16.000 in 0.001 steps, that bit the widths used by Close (910 of
/// 10001), Case (910) and Invert (415).
///
/// The epsilon is far larger than the f32 error (~1e-7 relative) and far
/// smaller than the fraction of a cell that a genuinely clamped control loses
/// at the surface edge, so a control cut off by the window still floors down.
pub fn search_bar_columns(rect_width: f32, cell_width: f32) -> usize {
    if cell_width <= 0.0 {
        return 0;
    }
    (rect_width / cell_width + 1e-3).floor().max(0.0) as usize
}

/// Number of text segments in the search lane: label, editor, Previous, Next,
/// Wrap, Case, Invert, status and Close.
pub(crate) const SEARCH_SEGMENTS: usize = 9;

/// One piece of search-lane text, placed in pixels and clipped to its slot.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SearchBarSegment {
    /// The control this text labels, if it is one.
    pub(crate) control: Option<SearchControl>,
    /// The slot the text is clipped to.
    pub(crate) rect: Rect4,
    /// Left edge of the text.
    pub(crate) left: f32,
    pub(crate) text: String,
    /// The editor's whole query reads right to left (`starts_right_to_left`),
    /// so its visible slice is shaped as a right-to-left paragraph.
    pub(crate) rtl: bool,
}

/// Bytes of the direction mark that starts the editor's shaped text.
const EDITOR_MARK_LEN: usize = 3;

impl SearchBarSegment {
    /// The text shaped for this segment. The editor's slice starts with a
    /// zero-width direction mark (U+200E or U+200F) taken from the whole
    /// query, so the slice keeps the query's reading order however far it is
    /// scrolled; cosmic-text would otherwise take the direction from the
    /// slice's own first letter.
    pub(crate) fn shaped_text(&self) -> std::borrow::Cow<'_, str> {
        if self.control == Some(SearchControl::Editor) {
            let mark = if self.rtl { '\u{200F}' } else { '\u{200E}' };
            std::borrow::Cow::Owned(format!("{mark}{}", self.text))
        } else {
            std::borrow::Cow::Borrowed(&self.text)
        }
    }
}

/// Whether `text` reads right to left: its first strong character is
/// right-to-left (Unicode bidi rule P2). Digits, marks and punctuation do not
/// decide it, and text with no strong character reads left to right.
pub(crate) fn starts_right_to_left(text: &str) -> bool {
    unicode_bidi::get_base_direction(text) == unicode_bidi::Direction::Rtl
}

/// Opacity of a focused search button's accent fill.
pub(crate) const SEARCH_FOCUSED_FILL_ALPHA: f32 = 0.92;

/// The label color for the focused search button.
///
/// The button sits on the accent, which the theme foreground was never chosen
/// to contrast with. The theme's cursor text is held to AA against the fill
/// as painted: the accent over the bar, then any hover or press `tint`,
/// composited the way the surface blends (`linear` for an sRGB surface).
#[cfg(test)]
pub(crate) fn search_focused_label(
    cursor_text: Rgb,
    bar: Rgb,
    accent: Rgb,
    tint: Option<(Rgb, f32)>,
    linear: bool,
) -> Rgb {
    let fill = color::over(bar, accent, f64::from(SEARCH_FOCUSED_FILL_ALPHA), linear);
    let fill = tint.map_or(fill, |(tint, alpha)| {
        color::over(fill, tint, f64::from(alpha), linear)
    });
    color::with_min_contrast(cursor_text, fill, 4.5)
}

/// One cluster of the painted search query: the bytes it covers and the x
/// span its glyphs cover.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchEditorCluster {
    pub start: usize,
    pub end: usize,
    pub left: f32,
    pub right: f32,
    /// Right-to-left text starts at `right` and ends at `left`.
    pub rtl: bool,
}

/// Lay out one piece of search-lane text in its own buffer.
///
/// The line is as tall as `metrics` says, and cosmic-text centers glyphs in
/// their line, so text placed at the row's top is centered in the row. Text is
/// set flush left: a query that starts with Hebrew or Arabic is a
/// right-to-left paragraph, which would otherwise be pushed against the far
/// edge of the slot and clipped.
pub(crate) fn shape_search_segment(
    font_system: &mut FontSystem,
    buffer: &mut TextBuffer,
    segment: &SearchBarSegment,
    metrics: Metrics,
    family: Family<'_>,
    text_changed: bool,
) {
    buffer.set_metrics(metrics);
    // No width: a right-to-left line is laid out against the width it is
    // given, so one wider than its slot would start past the slot's far edge.
    // The text area's bounds clip it instead.
    buffer.set_size(None, Some(metrics.line_height));
    if text_changed {
        buffer.set_text(
            &segment.shaped_text(),
            &Attrs::new().family(family),
            Shaping::Advanced,
            Some(Align::Left),
        );
    }
    buffer.shape_until_scroll(font_system, false);
}

/// The clusters of a shaped segment in the segment's own text bytes: the
/// editor's direction mark is dropped.
pub(crate) fn segment_clusters(
    buffer: &TextBuffer,
    segment: &SearchBarSegment,
) -> Vec<SearchEditorCluster> {
    let clusters = glyph_clusters(buffer);
    if segment.control != Some(SearchControl::Editor) {
        return clusters;
    }
    let clusters = clusters
        .into_iter()
        .filter(|cluster| cluster.end > EDITOR_MARK_LEN)
        .map(|cluster| SearchEditorCluster {
            start: cluster.start.max(EDITOR_MARK_LEN) - EDITOR_MARK_LEN,
            end: cluster.end - EDITOR_MARK_LEN,
            ..cluster
        })
        .collect();
    grapheme_clusters(&segment.text, clusters)
}

pub(crate) fn grapheme_clusters(
    text: &str,
    clusters: Vec<SearchEditorCluster>,
) -> Vec<SearchEditorCluster> {
    use unicode_segmentation::UnicodeSegmentation as _;
    let mut result = Vec::new();
    for cluster in clusters {
        let Some(text) = text.get(cluster.start..cluster.end) else {
            result.push(cluster);
            continue;
        };
        let count = text.graphemes(true).count();
        if count <= 1 {
            result.push(cluster);
            continue;
        }
        let count = count as f32;
        for (index, (byte, grapheme)) in text.grapheme_indices(true).enumerate() {
            let width = (cluster.right - cluster.left) / count;
            let left = if cluster.rtl {
                cluster.right - (index + 1) as f32 * width
            } else {
                cluster.left + index as f32 * width
            };
            result.push(SearchEditorCluster {
                start: cluster.start + byte,
                end: cluster.start + byte + grapheme.len(),
                left,
                right: left + width,
                rtl: cluster.rtl,
            });
        }
    }
    result
}

/// The clusters in the first line of `buffer`, sorted by byte. The glyphs of
/// one cluster (a split vowel, an emoji sequence the font does not ligate)
/// share its bytes and sit next to each other, so the cluster spans them all.
fn glyph_clusters(buffer: &TextBuffer) -> Vec<SearchEditorCluster> {
    let glyphs = buffer.layout_runs().next().into_iter().flat_map(|run| {
        run.glyphs.iter().map(|glyph| SearchEditorCluster {
            start: glyph.start,
            end: glyph.end,
            left: glyph.x,
            right: glyph.x + glyph.w,
            rtl: glyph.level.is_rtl(),
        })
    });
    merge_clusters(glyphs)
}

/// Merge neighbouring glyphs that share their bytes into one cluster, and
/// sort the clusters by byte.
pub(crate) fn merge_clusters(
    glyphs: impl Iterator<Item = SearchEditorCluster>,
) -> Vec<SearchEditorCluster> {
    let mut clusters: Vec<SearchEditorCluster> = Vec::new();
    for glyph in glyphs {
        match clusters.last_mut() {
            Some(last) if last.start == glyph.start && last.end == glyph.end => {
                last.left = last.left.min(glyph.left);
                last.right = last.right.max(glyph.right);
            }
            _ => clusters.push(glyph),
        }
    }
    clusters.sort_by_key(|cluster| cluster.start);
    clusters
}

/// Every caret position among `clusters`, as (byte, x), sorted by byte.
///
/// A byte sits at the leading edge of the cluster it starts, or, at the end
/// of the text, at the trailing edge of the cluster it ends. Right-to-left
/// clusters lead from their right edge.
pub(crate) fn cluster_stops(clusters: &[SearchEditorCluster]) -> Vec<(usize, f32)> {
    let leading = clusters.iter().map(|cluster| {
        let x = if cluster.rtl {
            cluster.right
        } else {
            cluster.left
        };
        (cluster.start, x)
    });
    let trailing = clusters.iter().map(|cluster| {
        let x = if cluster.rtl {
            cluster.left
        } else {
            cluster.right
        };
        (cluster.end, x)
    });
    let mut stops: Vec<(usize, f32)> = leading.chain(trailing).collect();
    // The sort is stable, so a leading edge stays ahead of a trailing edge at
    // the same byte and survives the dedup.
    stops.sort_by_key(|stop| stop.0);
    stops.dedup_by_key(|stop| stop.0);
    if stops.is_empty() {
        stops.push((0, 0.0));
    }
    stops
}

/// The x of `byte` among `stops`: exact when it is a caret position, else the
/// position before it (a byte inside a cluster), else the end of the text.
pub(crate) fn stop_x(stops: &[(usize, f32)], byte: usize) -> f32 {
    match stops.binary_search_by_key(&byte, |stop| stop.0) {
        Ok(index) => stops[index].1,
        Err(0) => stops.first().map_or(0.0, |stop| stop.1),
        Err(index) => stops[index - 1].1,
    }
}

/// The x ranges covered by the clusters that overlap bytes `start..end`,
/// merged where they touch. Text that mixes directions can need several.
pub(crate) fn cluster_spans(
    clusters: &[SearchEditorCluster],
    start: usize,
    end: usize,
) -> Vec<(f32, f32)> {
    if start >= end {
        return Vec::new();
    }
    let mut spans: Vec<(f32, f32)> = clusters
        .iter()
        .filter(|cluster| cluster.start < end && cluster.end > start)
        .map(|cluster| (cluster.left, cluster.right))
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f32, f32)> = Vec::with_capacity(spans.len());
    for (left, right) in spans {
        match merged.last_mut() {
            Some(last) if left <= last.1 + 0.5 => last.1 = last.1.max(right),
            _ => merged.push((left, right)),
        }
    }
    merged
}

/// Lay out every piece of search-lane text in its own slot.
///
/// Each control is placed from its own rectangle instead of padding one line
/// per row with spaces, so a glyph whose advance is not a whole cell (a CJK
/// fallback in the query, for one) cannot push the controls after it. Button
/// labels are centered as a block as wide as the control's widest label, so a
/// toggle such as `Wrap: On` / `Wrap: Off` changes its value without moving
/// its name.
pub(crate) fn search_bar_segments(
    search: &SearchOverlay,
    geometry: SearchBarGeometry,
    cell_width: f32,
) -> Vec<SearchBarSegment> {
    let mut segments = Vec::with_capacity(SEARCH_SEGMENTS);
    let rtl = starts_right_to_left(&search.query);
    let mut push = |control: Option<SearchControl>, rect: Rect4, text: String, left: f32| {
        segments.push(SearchBarSegment {
            control,
            rect,
            left,
            text,
            rtl: rtl && control == Some(SearchControl::Editor),
        });
    };
    let cols = |rect: Rect4| search_bar_columns(rect.2, cell_width);
    // Centre `text` in `rect` as part of a block `block` columns wide.
    let centered = |rect: Rect4, block: usize| -> f32 {
        let block = block.min(cols(rect)) as f32 * cell_width;
        rect.0 + ((rect.2 - block) * 0.5).max(0.0).round()
    };
    let button = |rect: Rect4, text: String, variants: &[&str]| -> (String, f32) {
        let block = variants
            .iter()
            .map(|variant| display_width(variant))
            .max()
            .unwrap_or(0);
        let fitted = fit_single_line_label(&text, cols(rect).max(1));
        (fitted, centered(rect, block))
    };

    let labels = SearchBarLabels::new(&search.tr);
    let label_cols = cols(geometry.label);
    push(
        None,
        geometry.label,
        if label_cols >= display_width(labels.label) {
            labels.label
        } else {
            "?"
        }
        .to_string(),
        geometry.label.0,
    );
    let editor_cols = search_bar_columns(geometry.editor.2, cell_width).max(1);
    push(
        Some(SearchControl::Editor),
        geometry.editor,
        search_editor_body(search, editor_cols).0,
        geometry.editor.0 + cell_width,
    );
    let (text, left) = button(
        geometry.previous,
        labels.previous.to_string(),
        &[labels.previous],
    );
    push(Some(SearchControl::Previous), geometry.previous, text, left);
    let (text, left) = button(geometry.next, labels.next.to_string(), &[labels.next]);
    push(Some(SearchControl::Next), geometry.next, text, left);
    // Say the state, not a checkbox. `[x] Wrap` makes someone decode a TUI
    // idiom to learn whether wrapping is on; `Wrap: On` just tells them. The
    // trailing chevron on Case is the affordance for "clicking cycles this",
    // which nothing else in the bar communicated.
    let (text, left) = button(
        geometry.wrap,
        labels.wrap[usize::from(!search.wrap)].to_string(),
        &labels.wrap,
    );
    push(Some(SearchControl::Wrap), geometry.wrap, text, left);
    // The mode name is padded to the longest one, so the chevron stays put as
    // the mode cycles.
    let case_variants: Vec<&str> = labels.case.iter().map(String::as_str).collect();
    let (text, left) = button(
        geometry.case_mode,
        search_case_text(&search.tr, search.case_mode),
        &case_variants,
    );
    push(Some(SearchControl::Case), geometry.case_mode, text, left);
    // The invert toggle flips which direction Enter searches, so its label
    // names that direction. It also reminds users of the Enter keybinding.
    let (text, left) = button(
        geometry.invert,
        labels.invert[usize::from(search.invert)].to_string(),
        &labels.invert,
    );
    push(Some(SearchControl::Invert), geometry.invert, text, left);
    // `Match` is the one status the user can already see: the hit is
    // highlighted in the pane and the viewport has jumped to it. Printing it
    // too spends the widest slot in the bar restating the obvious, and it
    // makes the states that DO need attention — `No match`, `Invalid pattern`,
    // `Results limited` — look like just another word in the strip rather than
    // an answer. The slot keeps its width either way, so nothing shifts when
    // the status appears and disappears.
    let status = if search.status == SearchStatus::Match {
        String::new()
    } else {
        fit_single_line_label(
            search.status.label(&search.tr),
            cols(geometry.status).max(1),
        )
    };
    push(None, geometry.status, status, geometry.status.0);
    let (text, left) = button(geometry.close, labels.close.to_string(), &[labels.close]);
    push(Some(SearchControl::Close), geometry.close, text, left);
    segments
}

/// The search lane as text, one line per row, each segment at its column.
/// Tests read layout invariants from it; paint uses the segments directly.
#[cfg(test)]
pub(crate) fn search_bar_text(
    search: &SearchOverlay,
    geometry: SearchBarGeometry,
    cell_width: f32,
) -> String {
    let row_height = geometry.reserved_height / geometry.rows.max(1) as f32;
    let mut rows = vec![Vec::<(usize, usize, String)>::new(); geometry.rows];
    for segment in search_bar_segments(search, geometry, cell_width) {
        let rect = segment.rect;
        if rect.2 <= 0.0 || rect.3 <= 0.0 {
            continue;
        }
        let row = if row_height > 0.0 {
            ((rect.1 - geometry.rect.1) / row_height).round().max(0.0) as usize
        } else {
            0
        }
        .min(rows.len().saturating_sub(1));
        let col = (segment.left / cell_width).round().max(0.0) as usize;
        let width = search_bar_columns(rect.2, cell_width).max(1);
        rows[row].push((col, width, segment.text));
    }
    rows.iter_mut()
        .map(|segments| {
            segments.sort_by_key(|segment| segment.0);
            let mut line = String::new();
            let mut column = 0usize;
            for (start, width, text) in segments {
                if *start > column {
                    line.extend(std::iter::repeat_n(' ', *start - column));
                    column = *start;
                }
                if *start < column {
                    continue;
                }
                let fitted = fit_single_line_label(text, *width);
                column += display_width(&fitted);
                line.push_str(&fitted);
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The editor's visible query text, without padding or caret, and the query
/// byte it starts at.
///
/// The caret is painted as a bar between glyphs, so moving it or focusing
/// another control never shifts the characters. Text past the right edge is
/// clipped, as in any text field: an ellipsis there would cover the character
/// next to the caret.
pub(crate) fn search_editor_body(search: &SearchOverlay, max_cols: usize) -> (String, usize) {
    use unicode_segmentation::UnicodeSegmentation as _;
    use unicode_width::UnicodeWidthStr as _;
    let inner = max_cols.saturating_sub(2);
    if inner == 0 {
        return (String::new(), 0);
    }
    // Do not let a pasted line break create extra chrome rows. The UI also
    // normalizes paste, but paint is a trust boundary for callers of this crate.
    // Each control character becomes as many spaces as it has bytes, so body
    // bytes still map one to one onto query bytes.
    let mut query = String::with_capacity(search.query.len().min(4096));
    for ch in search.query.chars() {
        if query.len() + ch.len_utf8() > 4096 {
            break;
        }
        if ch.is_control() {
            query.extend(std::iter::repeat_n(' ', ch.len_utf8()));
        } else {
            query.push(ch);
        }
    }
    let visible = drop_cols_front(&query, search.horizontal_scroll);
    let skip = query.len() - visible.len();
    let mut body = String::with_capacity(visible.len());
    let mut cols = 0usize;
    for grapheme in visible.graphemes(true) {
        let width = grapheme.width();
        if cols + width > inner {
            break;
        }
        cols += width;
        body.push_str(grapheme);
    }
    (body, skip)
}

/// The editor text with its one-cell padding on each side, as tests see it.
#[cfg(test)]
pub(crate) fn search_editor_label(search: &SearchOverlay, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    let label = format!(" {} ", search_editor_body(search, max_cols).0);
    fit_single_line_label(&label, max_cols)
}

pub(crate) fn drop_cols_front(s: &str, cols: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation as _;
    use unicode_width::UnicodeWidthStr as _;
    let mut skipped = 0usize;
    let mut byte = s.len();
    // Stop at the first grapheme that is not wholly inside the dropped
    // columns, so a zero-width one at the boundary (a leading combining mark,
    // a zero-width space) stays.
    for (idx, grapheme) in s.grapheme_indices(true) {
        let width = grapheme.width();
        if skipped >= cols || skipped + width > cols {
            byte = idx;
            break;
        }
        skipped += width;
    }
    s[byte..].to_string()
}
