//! Grapheme-aware editor primitives for the scrollback search field.
//!
//! The search bar is a Kettle-owned text surface. Keeping its editing rules in
//! a pure module makes keyboard, IME, clipboard, pointer, and agent-driven UI
//! input share the same byte-limit and selection invariants.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

#[derive(Debug)]
pub(crate) struct SearchState {
    pub(crate) open: bool,
    pub(crate) editor: SearchEditor,
    pub(crate) target_pane: Option<u64>,
    pub(crate) compiled: Option<kettle_core::CompiledSearch>,
    pub(crate) compiled_revision: Option<u64>,
    pub(crate) focused: Option<kettle_core::SearchSpan>,
    pub(crate) revealed_focus: Option<kettle_core::SearchSpan>,
    pub(crate) visible: Vec<kettle_core::SearchSpan>,
    pub(crate) visible_truncated: bool,
    pub(crate) visible_scan: Option<VisibleSearchScan>,
    pub(crate) visible_turn_pending: bool,
    pub(crate) anchor: Option<kettle_core::SearchPoint>,
    pub(crate) typing_scanned_revision: Option<u64>,
    pub(crate) visible_key: Option<VisibleSearchKey>,
    pub(crate) scan_token: Option<kettle_core::SearchScanToken>,
    pub(crate) status: kettle_render::SearchStatus,
    pub(crate) focused_control: kettle_render::SearchControl,
    /// Control under the pointer, for the hover fill.
    pub(crate) hovered_control: Option<kettle_render::SearchControl>,
    /// Button the pointer pressed; it activates on release over the same one.
    pub(crate) pressed_control: Option<kettle_render::SearchControl>,
    pub(crate) dragging_editor: bool,
    pub(crate) wrap: bool,
    pub(crate) case_mode: kettle_config::SearchCaseSensitivity,
    pub(crate) invert: bool,
    pub(crate) revision: u64,
    pub(crate) unlimited_retry_at: Option<std::time::Instant>,
    pub(crate) quiet_retry_pending: bool,
    pub(crate) pre_open_display_offset: Option<usize>,
    pub(crate) background: Option<BackgroundSearch>,
    /// When the current `Searching` status began, and what the bar shows until
    /// it says `Searching…` (see `searching_label_at`).
    pub(crate) searching_since: Option<std::time::Instant>,
    pub(crate) status_before_search: Option<kettle_render::SearchStatus>,
    /// The end of the typing pause the full scan waits for. Only edits set it;
    /// output and retries do not.
    pub(crate) typing_pause_until: Option<std::time::Instant>,
    /// A wake armed for the moment the bar starts saying `Searching…`.
    pub(crate) searching_label_wake: Option<std::time::Instant>,
}

/// How long the full scan waits for typing to pause.
pub(crate) const TYPING_PAUSE: std::time::Duration = std::time::Duration::from_millis(500);

/// How long a scan may run before the bar says `Searching…`.
pub(crate) const SEARCHING_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

impl Default for SearchState {
    fn default() -> Self {
        Self {
            open: false,
            editor: SearchEditor::default(),
            target_pane: None,
            compiled: None,
            compiled_revision: None,
            focused: None,
            revealed_focus: None,
            visible: Vec::new(),
            visible_truncated: false,
            visible_scan: None,
            visible_turn_pending: false,
            anchor: None,
            typing_scanned_revision: None,
            visible_key: None,
            scan_token: None,
            status: kettle_render::SearchStatus::Typing,
            focused_control: kettle_render::SearchControl::Editor,
            hovered_control: None,
            pressed_control: None,
            dragging_editor: false,
            wrap: true,
            case_mode: kettle_config::SearchCaseSensitivity::Smart,
            invert: false,
            revision: 0,
            unlimited_retry_at: None,
            quiet_retry_pending: false,
            pre_open_display_offset: None,
            background: None,
            searching_since: None,
            status_before_search: None,
            typing_pause_until: None,
            searching_label_wake: None,
        }
    }
}

impl SearchState {
    pub(crate) fn query(&self) -> &str {
        self.editor.text()
    }

    /// Whether a gesture that began in the bar owns the pointer: an editor
    /// drag, or a button held down. Motion and the release go to the bar.
    pub(crate) fn pointer_captured(&self) -> bool {
        self.dragging_editor || self.pressed_control.is_some()
    }

    pub(crate) fn note_edit(&mut self, now: std::time::Instant) {
        self.revision = self.revision.wrapping_add(1);
        self.unlimited_retry_at = (!self.editor.text().is_empty()).then_some(now + TYPING_PAUSE);
        self.quiet_retry_pending = false;
        let status = if self.editor.text().is_empty() {
            kettle_render::SearchStatus::Typing
        } else {
            kettle_render::SearchStatus::Searching
        };
        self.start_typing_pause(status, now);
        self.compiled = None;
        self.compiled_revision = None;
        self.focused = None;
        self.revealed_focus = None;
        self.visible.clear();
        self.visible_truncated = false;
        self.visible_scan = None;
        self.visible_turn_pending = false;
        self.typing_scanned_revision = None;
        self.visible_key = None;
        self.scan_token = None;
        self.background = None;
    }

    pub(crate) fn restart_navigation(&mut self, now: std::time::Instant) {
        let compile_error = self.compiled_revision == Some(self.revision)
            && self.compiled.is_none()
            && matches!(
                self.status,
                kettle_render::SearchStatus::Invalid
                    | kettle_render::SearchStatus::TooComplex
                    | kettle_render::SearchStatus::TooLong
            );
        self.focused = None;
        self.revealed_focus = None;
        self.visible.clear();
        self.visible_truncated = false;
        self.visible_scan = None;
        self.visible_turn_pending = false;
        self.typing_scanned_revision = None;
        self.visible_key = None;
        self.background = None;
        self.quiet_retry_pending = false;
        self.unlimited_retry_at =
            (!compile_error && !self.editor.text().is_empty()).then_some(now + TYPING_PAUSE);
        if !compile_error {
            let status = if self.editor.text().is_empty() {
                kettle_render::SearchStatus::Typing
            } else {
                kettle_render::SearchStatus::Searching
            };
            self.start_typing_pause(status, now);
        }
    }

    /// Set the status after an edit, which defers the full scan by the
    /// typing pause. Once the bar says `Searching…`, a further edit leaves it
    /// saying so instead of going back to an older status.
    fn start_typing_pause(&mut self, status: kettle_render::SearchStatus, now: std::time::Instant) {
        let label_shown = self.searching_label_at().is_some_and(|at| now >= at);
        self.set_status_at(status, now);
        if !label_shown && status == kettle_render::SearchStatus::Searching {
            self.typing_pause_until = Some(now + TYPING_PAUSE);
        }
    }

    /// Set the status, remembering when a search began and what to show
    /// until it says `Searching…`. A scan that re-enters `Searching` keeps its
    /// start time, so a genuinely slow search still says so.
    pub(crate) fn set_status_at(
        &mut self,
        status: kettle_render::SearchStatus,
        now: std::time::Instant,
    ) {
        if status == kettle_render::SearchStatus::Searching
            && self.status != kettle_render::SearchStatus::Searching
        {
            // The status stays `Searching` only while the scan near the
            // viewport has found nothing, so `No match` still holds. Every
            // other status described the old query or a step through it, and
            // the slot stays blank instead (`Match` paints no label).
            self.status_before_search = Some(match self.status {
                kettle_render::SearchStatus::NoMatch => kettle_render::SearchStatus::NoMatch,
                _ => kettle_render::SearchStatus::Match,
            });
            self.searching_since = Some(now);
        }
        self.status = status;
    }

    /// When the bar starts saying `Searching…`: once the scan has run for
    /// the grace period, not counting the typing pause before it starts.
    /// Output and retries never extend it. `None` unless searching.
    pub(crate) fn searching_label_at(&self) -> Option<std::time::Instant> {
        if self.status != kettle_render::SearchStatus::Searching {
            return None;
        }
        let since = self.searching_since?;
        let started = self
            .typing_pause_until
            .map_or(since, |until| until.max(since));
        Some(started + SEARCHING_GRACE)
    }

    /// The status the bar paints at `now`, given the status automation
    /// reports. A search that finishes within the grace period never flashes
    /// `Searching…`.
    pub(crate) fn painted_status(
        &self,
        status: kettle_render::SearchStatus,
        now: std::time::Instant,
    ) -> kettle_render::SearchStatus {
        match self.searching_label_at() {
            Some(at) if status == kettle_render::SearchStatus::Searching && now < at => self
                .status_before_search
                .unwrap_or(kettle_render::SearchStatus::Match),
            _ => status,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VisibleSearchKey {
    pub(crate) revision: u64,
    pub(crate) pane: u64,
    pub(crate) output_generation: u64,
    pub(crate) columns: usize,
    pub(crate) screen_lines: usize,
    pub(crate) history_size: usize,
    pub(crate) display_offset: usize,
    pub(crate) focused: Option<kettle_core::SearchSpan>,
}

#[derive(Debug)]
pub(crate) struct VisibleSearchScan {
    pub(crate) key: VisibleSearchKey,
    pub(crate) ranges: Vec<kettle_core::SearchBounds>,
    pub(crate) range_index: usize,
    pub(crate) cursor: kettle_core::SearchPoint,
    pub(crate) matches: Vec<kettle_core::SearchSpan>,
    pub(crate) seen: std::collections::HashSet<kettle_core::SearchSpan>,
    pub(crate) truncated: bool,
}

impl VisibleSearchScan {
    pub(crate) fn new(
        key: VisibleSearchKey,
        ranges: Vec<kettle_core::SearchBounds>,
        focused: Option<kettle_core::SearchSpan>,
        truncated: bool,
    ) -> Self {
        let cursor = ranges
            .first()
            .map_or(kettle_core::SearchPoint::default(), |range| range.start);
        let mut matches = Vec::with_capacity(256);
        let mut seen = std::collections::HashSet::with_capacity(256);
        if let Some(focused) = focused {
            matches.push(focused);
            seen.insert(focused);
        }
        Self {
            key,
            ranges,
            range_index: 0,
            cursor,
            matches,
            seen,
            truncated,
        }
    }

    pub(crate) fn current_bounds(&self) -> Option<kettle_core::SearchBounds> {
        self.ranges
            .get(self.range_index)
            .map(|range| kettle_core::SearchBounds::new(self.cursor, range.end))
    }

    pub(crate) fn advance_range(&mut self) -> bool {
        self.range_index += 1;
        let Some(range) = self.ranges.get(self.range_index) else {
            return false;
        };
        self.cursor = range.start;
        true
    }

    /// Merge one exact core work slice. Returns true only when projection is complete or hit an
    /// accuracy/match-cap barrier; ordinary continuations remain resumable and non-limited.
    pub(crate) fn apply_batch(&mut self, batch: kettle_core::SearchBatch) -> bool {
        for span in batch.matches {
            if self.seen.insert(span) {
                self.matches.push(span);
            }
        }
        self.truncated |= batch.truncated;
        if batch.accuracy_limited
            || batch.truncated
            || self.matches.len() >= kettle_core::MAX_SEARCH_MATCHES
        {
            self.truncated = true;
            return true;
        }
        if let Some(continuation) = batch.continuation {
            self.cursor = continuation;
            return false;
        }
        !self.advance_range()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BackgroundSearch {
    pub(crate) token: kettle_core::SearchScanToken,
    pub(crate) direction: kettle_core::SearchDirection,
    pub(crate) cursor: kettle_core::SearchPoint,
    pub(crate) edge: kettle_core::SearchPoint,
    /// Point that bounds the second phase when this job wraps.
    pub(crate) wrap_anchor: kettle_core::SearchPoint,
    pub(crate) wrapped: bool,
    /// Continuation of the immediate 1,000-line nearby phase after an exact work-budget yield.
    pub(crate) nearby: bool,
    /// Explicit Enter/F3/Previous/Next navigation, rather than the idle initial scan.
    pub(crate) navigation: bool,
    pub(crate) had_focus: bool,
    /// Output changed between chunks. Idle initial scans need one quiet-period verification before
    /// claiming no match; explicit navigation remains Limited until the user retries because its
    /// original ordering boundary cannot be reconstructed after rows drift.
    pub(crate) output_drifted: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct EditOutcome {
    pub(crate) changed: bool,
    pub(crate) truncated: bool,
}

/// The grapheme boundary at or before `byte`.
fn grapheme_boundary_at_or_before(text: &str, byte: usize) -> usize {
    if byte >= text.len() {
        return text.len();
    }
    let mut boundary = 0;
    for (start, _) in text.grapheme_indices(true) {
        if start > byte {
            break;
        }
        boundary = start;
    }
    boundary
}

/// The query byte of the painted caret position nearest `x`, from the
/// renderer's (query byte, window x) stops.
pub(crate) fn nearest_stop_byte(stops: &[(usize, f32)], x: f32) -> Option<usize> {
    stops
        .iter()
        .min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs()))
        .map(|stop| stop.0)
}

/// The first query byte of the character painted under `x`: the cluster that
/// covers it, else the nearest one.
pub(crate) fn cluster_byte_at(
    clusters: &[kettle_render::SearchEditorCluster],
    x: f32,
) -> Option<usize> {
    let distance = |cluster: &kettle_render::SearchEditorCluster| {
        (cluster.left - x).max(x - cluster.right).max(0.0)
    };
    clusters
        .iter()
        .find(|cluster| cluster.left <= x && x < cluster.right)
        .or_else(|| {
            clusters
                .iter()
                .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        })
        .map(|cluster| cluster.start)
}

/// Map a signed editor-cell offset to a query display column.
///
/// Cell zero is the editor's left padding and cell one is the first visible query column. Keeping
/// the offset signed lets a drag beyond the left edge walk backward through a scrolled query. The
/// caret is painted between cells, so it never occupies a column of its own.
pub(crate) fn pointer_query_column(horizontal_scroll: usize, relative_cell: isize) -> usize {
    if relative_cell >= 1 {
        horizontal_scroll.saturating_add(relative_cell.saturating_sub(1) as usize)
    } else {
        horizontal_scroll.saturating_sub(1isize.saturating_sub(relative_cell) as usize)
    }
}

/// How many edits the query can undo.
const UNDO_LIMIT: usize = 100;

/// The query and caret as they were before an edit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct EditorSnapshot {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SearchEditor {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
    horizontal_scroll: usize,
    /// Earlier states, newest last, and the states undone since the last
    /// edit.
    undo: Vec<EditorSnapshot>,
    redo: Vec<EditorSnapshot>,
    /// The run of edits the next one can join, so typed text or a held
    /// Backspace undoes in one step, as in a native text field.
    run: Option<EditRun>,
}

/// A run of like edits at a caret that has not moved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditRun {
    Typing,
    DeletingBack,
    DeletingForward,
}

/// A place in the query found from a pointer: a byte from the painted glyphs,
/// or a display column counted in cells where nothing is painted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QueryPoint {
    Byte(usize),
    Column(usize),
}

impl QueryPoint {
    /// A key that is equal for two presses on the same spot, for counting
    /// double and triple clicks.
    pub(crate) fn click_key(self) -> usize {
        match self {
            Self::Byte(byte) => byte,
            Self::Column(column) => column,
        }
    }
}

/// Display width as the editor counts it: the sum of its graphemes' widths.
/// Paint and scrolling count the same way, so a column means one thing.
pub(crate) fn display_width(text: &str) -> usize {
    text.graphemes(true).map(|grapheme| grapheme.width()).sum()
}

impl SearchEditor {
    pub(crate) fn from_text(text: String, max_bytes: usize) -> Self {
        let mut editor = Self::default();
        editor.replace_all(&text, max_bytes);
        editor.undo.clear();
        editor
    }

    fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
        }
    }

    /// Record `before` as an undo step when an edit changed the query's text.
    /// An edit that continues the last one's `run` joins its step. Returns
    /// whether the text changed.
    fn edited(&mut self, before: EditorSnapshot, run: Option<EditRun>) -> bool {
        if before.text == self.text {
            return false;
        }
        if run.is_none() || run != self.run {
            self.undo.push(before);
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.run = run;
        true
    }

    /// Put back `snapshot`. Returns whether the text changed, which is when
    /// the search has to run again.
    fn restore(&mut self, snapshot: EditorSnapshot) -> bool {
        let changed = snapshot.text != self.text;
        self.text = snapshot.text;
        self.cursor = snapshot.cursor;
        self.anchor = snapshot.anchor;
        self.run = None;
        changed
    }

    /// Undo the last edit. Returns whether the text changed.
    pub(crate) fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.restore(previous)
    }

    /// Redo the last undone edit. Returns whether the text changed.
    pub(crate) fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.restore(next)
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn horizontal_scroll(&self) -> usize {
        self.horizontal_scroll
    }

    pub(crate) fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    pub(crate) fn directed_selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then_some((anchor, self.cursor))
    }

    pub(crate) fn selected_text(&self) -> Option<&str> {
        self.selection().map(|range| &self.text[range])
    }

    pub(crate) fn replace_all(&mut self, value: &str, max_bytes: usize) -> EditOutcome {
        let mut replacement = Self::default();
        let mut outcome = replacement.insert(value, max_bytes);
        if outcome.truncated {
            outcome.changed = false;
            return outcome;
        }
        outcome.changed = replacement.text != self.text;
        let before = self.snapshot();
        self.text = replacement.text;
        self.cursor = replacement.cursor;
        self.anchor = None;
        self.horizontal_scroll = 0;
        self.edited(before, None);
        self.run = None;
        outcome
    }

    /// Insert `value` over the selection or at the caret, as one undo step.
    pub(crate) fn insert(&mut self, value: &str, max_bytes: usize) -> EditOutcome {
        let before = self.snapshot();
        let outcome = self.insert_text(value, max_bytes);
        self.edited(before, None);
        outcome
    }

    /// Insert typed text. Consecutive typing, including the character that
    /// replaced a selection, undoes as one step.
    pub(crate) fn type_text(&mut self, value: &str, max_bytes: usize) -> EditOutcome {
        let before = self.snapshot();
        let outcome = self.insert_text(value, max_bytes);
        self.edited(before, Some(EditRun::Typing));
        outcome
    }

    fn insert_text(&mut self, value: &str, max_bytes: usize) -> EditOutcome {
        let selected = self.selection();
        let selected_len = selected.as_ref().map_or(0, Range::len);
        let retained = self.text.len().saturating_sub(selected_len);
        let available = max_bytes.saturating_sub(retained);
        // Reject by raw UTF-8 size before normalization. Clipboard/AccessKit
        // callers can supply large strings; never duplicate or scan an
        // unbounded payload for a 4 KiB editor.
        if value.len() > available {
            return EditOutcome {
                changed: false,
                truncated: true,
            };
        }
        let normalized = normalize_input(value, available);
        let accepted = normalized.as_str();

        if accepted.is_empty() && selected.is_none() {
            return EditOutcome {
                changed: false,
                truncated: false,
            };
        }

        let range = selected.unwrap_or(self.cursor..self.cursor);
        self.text.replace_range(range.clone(), accepted);
        self.cursor = range.start + accepted.len();
        self.anchor = None;
        EditOutcome {
            changed: range.start != range.end || !accepted.is_empty(),
            truncated: false,
        }
    }

    /// Delete the character before the caret. A held Backspace undoes in
    /// one step.
    pub(crate) fn backspace(&mut self) -> bool {
        let previous = previous_grapheme_boundary(&self.text, self.cursor);
        self.delete_back_to(previous, Some(EditRun::DeletingBack))
    }

    pub(crate) fn delete(&mut self) -> bool {
        let next = next_grapheme_boundary(&self.text, self.cursor);
        self.delete_forward_to(next, Some(EditRun::DeletingForward))
    }

    pub(crate) fn delete_word_backward(&mut self) -> bool {
        let previous = previous_word_boundary(&self.text, self.cursor);
        self.delete_back_to(previous, None)
    }

    pub(crate) fn delete_word_forward(&mut self) -> bool {
        let next = next_word_boundary(&self.text, self.cursor);
        self.delete_forward_to(next, None)
    }

    /// Delete from the caret to the start of the query (`Cmd+Backspace`).
    pub(crate) fn delete_to_start(&mut self) -> bool {
        self.delete_back_to(0, None)
    }

    /// Delete from the caret to the end of the query (`Cmd+Delete`, `Ctrl+K`).
    pub(crate) fn delete_to_end(&mut self) -> bool {
        self.delete_forward_to(self.text.len(), None)
    }

    /// Delete the selection if there is one, as its own step, else the text
    /// from `start` to the caret.
    fn delete_back_to(&mut self, start: usize, run: Option<EditRun>) -> bool {
        let before = self.snapshot();
        if self.delete_selection() {
            return self.edited(before, None);
        }
        if start < self.cursor {
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
        }
        self.edited(before, run)
    }

    /// Delete the selection if there is one, as its own step, else the text
    /// from the caret to `end`.
    fn delete_forward_to(&mut self, end: usize, run: Option<EditRun>) -> bool {
        let before = self.snapshot();
        if self.delete_selection() {
            return self.edited(before, None);
        }
        if end > self.cursor {
            self.text.replace_range(self.cursor..end, "");
        }
        self.edited(before, run)
    }

    pub(crate) fn move_left(&mut self, selecting: bool, by_word: bool) {
        let next = if by_word {
            previous_word_boundary(&self.text, self.cursor)
        } else {
            previous_grapheme_boundary(&self.text, self.cursor)
        };
        self.move_cursor(next, selecting);
    }

    pub(crate) fn move_right(&mut self, selecting: bool, by_word: bool) {
        let next = if by_word {
            next_word_boundary(&self.text, self.cursor)
        } else {
            next_grapheme_boundary(&self.text, self.cursor)
        };
        self.move_cursor(next, selecting);
    }

    pub(crate) fn move_home(&mut self, selecting: bool) {
        self.move_cursor(0, selecting);
    }

    pub(crate) fn move_end(&mut self, selecting: bool) {
        self.move_cursor(self.text.len(), selecting);
    }

    pub(crate) fn select_all(&mut self) {
        self.run = None;
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    #[cfg(test)]
    pub(crate) fn set_cursor_column(&mut self, column: usize, selecting: bool) {
        self.set_cursor_at(QueryPoint::Column(column), selecting);
    }

    /// The query byte at `point`, on a grapheme boundary.
    fn byte_at(&self, point: QueryPoint) -> usize {
        match point {
            QueryPoint::Byte(byte) => grapheme_boundary_at_or_before(&self.text, byte),
            QueryPoint::Column(column) => byte_at_display_column(&self.text, column),
        }
    }

    /// Put the caret at `point`, extending the selection when `selecting`.
    pub(crate) fn set_cursor_at(&mut self, point: QueryPoint, selecting: bool) {
        let byte = self.byte_at(point);
        self.move_cursor(byte, selecting);
    }

    pub(crate) fn set_character_selection(&mut self, anchor: usize, focus: usize) {
        self.run = None;
        let byte_at_character = |index: usize| {
            self.text
                .char_indices()
                .nth(index)
                .map_or(self.text.len(), |(byte, _)| byte)
        };
        let anchor = byte_at_character(anchor);
        let focus = byte_at_character(focus);
        self.anchor = (anchor != focus).then_some(anchor);
        self.cursor = focus;
    }

    #[cfg(test)]
    pub(crate) fn select_word_at_column(&mut self, column: usize) {
        self.select_word_at(QueryPoint::Column(column));
    }

    /// Select the word at `point`, or the character there if it is not in a
    /// word.
    pub(crate) fn select_word_at(&mut self, point: QueryPoint) {
        self.run = None;
        let byte = self.byte_at(point);
        if let Some((start, word)) = self
            .text
            .unicode_word_indices()
            .find(|(start, word)| byte >= *start && byte <= start.saturating_add(word.len()))
        {
            self.anchor = Some(start);
            self.cursor = start + word.len();
            return;
        }
        let start = previous_grapheme_boundary(&self.text, byte);
        let end = next_grapheme_boundary(&self.text, start);
        self.anchor = Some(start);
        self.cursor = end;
    }

    pub(crate) fn clear_selection(&mut self) {
        self.run = None;
        self.anchor = None;
    }

    pub(crate) fn ensure_cursor_visible(&mut self, visible_columns: usize) {
        self.horizontal_scroll = Self::visible_scroll_for(
            &self.text,
            self.cursor,
            self.horizontal_scroll,
            visible_columns,
        );
    }

    /// Return a grapheme-aligned horizontal offset that keeps `cursor_byte` visible.
    ///
    /// This is also used for the transient IME projection: the preedit text must be able to
    /// scroll without mutating the committed editor state.
    pub(crate) fn visible_scroll_for(
        text: &str,
        mut cursor_byte: usize,
        current_scroll: usize,
        visible_columns: usize,
    ) -> usize {
        cursor_byte = cursor_byte.min(text.len());
        while cursor_byte > 0 && !text.is_char_boundary(cursor_byte) {
            cursor_byte -= 1;
        }
        let cursor = display_width(&text[..cursor_byte]);
        if visible_columns == 0 {
            return cursor;
        }
        let minimum = display_column_boundary_at_or_after(
            text,
            cursor.saturating_sub(visible_columns.saturating_sub(1)),
        );
        let maximum = display_column_boundary_at_or_after(
            text,
            display_width(text).saturating_sub(visible_columns.saturating_sub(1)),
        )
        .min(cursor);
        current_scroll.clamp(minimum.min(maximum), maximum)
    }

    #[cfg(test)]
    pub(crate) fn cursor_column(&self) -> usize {
        display_width(&self.text[..self.cursor])
    }

    fn move_cursor(&mut self, next: usize, selecting: bool) {
        debug_assert!(self.text.is_char_boundary(next));
        // Editing somewhere else starts a new undo step.
        self.run = None;
        if selecting {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = next;
    }

    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            return false;
        };
        self.text.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.anchor = None;
        true
    }
}

fn normalize_input(value: &str, max_bytes: usize) -> String {
    let mut normalized = String::with_capacity(value.len().min(max_bytes));
    for ch in value.chars() {
        let ch = match ch {
            '\t' | '\r' | '\n' => ' ',
            ch if ch.is_control() => continue,
            ch => ch,
        };
        debug_assert!(normalized.len() + ch.len_utf8() <= max_bytes);
        normalized.push(ch);
    }
    normalized
}

fn byte_at_display_column(value: &str, target: usize) -> usize {
    let mut column = 0usize;
    for (byte, grapheme) in value.grapheme_indices(true) {
        let width = grapheme.width();
        if target < column.saturating_add(width.max(1)) {
            return byte;
        }
        column = column.saturating_add(width);
    }
    value.len()
}

fn display_column_boundary_at_or_after(value: &str, target: usize) -> usize {
    let mut column = 0usize;
    for grapheme in value.graphemes(true) {
        let next = column.saturating_add(grapheme.width());
        if target <= column {
            return column;
        }
        if target <= next {
            return next;
        }
        column = next;
    }
    column
}

pub(crate) fn previous_grapheme_boundary(value: &str, cursor: usize) -> usize {
    value[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map_or(cursor, |(index, _)| index)
}

fn next_grapheme_boundary(value: &str, cursor: usize) -> usize {
    value[cursor..]
        .graphemes(true)
        .next()
        .map_or(cursor, |grapheme| cursor + grapheme.len())
}

fn previous_word_boundary(value: &str, cursor: usize) -> usize {
    value[..cursor]
        .unicode_word_indices()
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn next_word_boundary(value: &str, cursor: usize) -> usize {
    let mut words = value[cursor..].unicode_word_indices();
    match words.next() {
        Some((0, _)) => words
            .next()
            .map_or(value.len(), |(index, _)| cursor + index),
        Some((index, _)) => cursor + index,
        None => value.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::{SearchEditor, VisibleSearchKey, VisibleSearchScan};

    #[test]
    fn deletes_one_extended_grapheme_at_a_time() {
        let mut editor = SearchEditor::from_text("a\u{301}👩‍💻z".into(), 64);
        assert!(editor.backspace());
        assert_eq!(editor.text(), "a\u{301}👩‍💻");
        assert!(editor.backspace());
        assert_eq!(editor.text(), "a\u{301}");
        assert!(editor.backspace());
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn selection_replacement_and_navigation_keep_utf8_boundaries() {
        let mut editor = SearchEditor::from_text("one café three".into(), 64);
        editor.move_left(false, true);
        editor.move_left(true, true);
        assert_eq!(editor.selected_text(), Some("café "));
        let result = editor.insert("two ", 64);
        assert!(result.changed);
        assert!(!result.truncated);
        assert_eq!(editor.text(), "one two three");
        assert!(editor.selection().is_none());
    }

    #[test]
    fn reversed_character_selection_replaces_complete_unicode_range() {
        let mut editor = SearchEditor::from_text("one café three".into(), 64);
        editor.set_character_selection(9, 4);
        assert_eq!(editor.selected_text(), Some("café "));
        let result = editor.insert("two ", 64);
        assert!(result.changed);
        assert_eq!(editor.text(), "one two three");
        assert!(editor.selection().is_none());
    }

    #[test]
    fn input_is_normalized_and_capped_on_utf8_boundaries() {
        let mut editor = SearchEditor::default();
        let result = editor.insert("ab\tcd\n💻", 8);
        assert!(!result.changed);
        assert!(result.truncated);
        assert_eq!(editor.text(), "");

        assert!(editor.insert("éé", 4).changed);
        let result = editor.insert("💻", 6);
        assert!(!result.changed);
        assert!(result.truncated);
        assert_eq!(editor.text(), "éé");

        let huge = "x".repeat(1_000_000);
        let result = editor.insert(&huge, 4096);
        assert!(!result.changed);
        assert!(result.truncated);
        assert_eq!(editor.text(), "éé");
    }

    #[test]
    fn replace_all_reports_normalized_clear_as_a_change() {
        let mut editor = SearchEditor::from_text("old".into(), 64);
        let result = editor.replace_all("\0\n", 64);
        assert!(result.changed);
        assert_eq!(editor.text(), " ");

        let result = editor.replace_all("\0", 64);
        assert!(result.changed);
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn word_right_stops_at_the_immediately_next_unicode_word() {
        let mut editor = SearchEditor::from_text("one café three".into(), 64);
        editor.move_home(false);
        editor.move_right(false, true);
        assert_eq!(editor.cursor(), 4);
        editor.move_right(false, true);
        assert_eq!(&editor.text()[editor.cursor()..], "three");

        editor.set_character_selection(3, 3);
        editor.move_right(false, true);
        assert_eq!(editor.cursor(), 4);
    }

    #[test]
    fn word_deletion_preserves_utf8_boundaries() {
        let mut editor = SearchEditor::from_text("one café three".into(), 64);
        assert!(editor.delete_word_backward());
        assert_eq!(editor.text(), "one café ");
        editor.move_home(false);
        assert!(editor.delete_word_forward());
        assert_eq!(editor.text(), "café ");
    }

    #[test]
    fn horizontal_scroll_keeps_cursor_in_the_editor_view() {
        let mut editor = SearchEditor::from_text("abcdefghij".into(), 64);
        editor.ensure_cursor_visible(4);
        assert_eq!(editor.horizontal_scroll(), 7);
        editor.move_home(false);
        editor.ensure_cursor_visible(4);
        assert_eq!(editor.horizontal_scroll(), 0);
    }

    #[test]
    fn horizontal_scroll_snaps_before_wide_graphemes() {
        let mut editor = SearchEditor::from_text("界界界界".into(), 64);
        editor.ensure_cursor_visible(4);
        assert_eq!(editor.cursor_column(), 8);
        assert_eq!(editor.horizontal_scroll(), 6);

        editor.ensure_cursor_visible(2);
        assert_eq!(editor.horizontal_scroll(), 8);

        editor.select_all();
        assert!(editor.insert("x", 64).changed);
        editor.ensure_cursor_visible(4);
        assert_eq!(editor.horizontal_scroll(), 0);
    }

    #[test]
    fn transient_ime_projection_gets_its_own_grapheme_aligned_scroll() {
        let projected = "abc界界";
        assert_eq!(
            SearchEditor::visible_scroll_for(projected, projected.len(), 0, 3),
            5
        );
        // The committed editor offset remains an input, not mutable state owned by the preedit.
        assert_eq!(SearchEditor::visible_scroll_for("x", 1, 5, 4), 0);
    }

    #[test]
    fn pointer_columns_select_unicode_words_without_splitting_graphemes() {
        let mut editor = SearchEditor::from_text("one café 👩‍💻".into(), 64);
        editor.select_word_at_column(5);
        assert_eq!(editor.selected_text(), Some("café"));
        editor.set_cursor_column(10, false);
        assert!(editor.text().is_char_boundary(editor.cursor()));
    }

    #[test]
    fn pointer_drag_can_walk_left_of_a_scrolled_editor() {
        assert_eq!(super::pointer_query_column(7, 1), 7);
        assert_eq!(super::pointer_query_column(7, 0), 6);
        assert_eq!(super::pointer_query_column(7, -2), 4);
        assert_eq!(super::pointer_query_column(0, -20), 0);
        assert_eq!(super::pointer_query_column(7, 4), 10);
    }

    /// Typing undoes as one step; a paste, a deletion and a jump of the caret
    /// each start a new one; redo replays what undo took back.
    #[test]
    fn undo_steps_back_through_edits_like_a_text_field() {
        let mut editor = SearchEditor::default();
        for ch in ["g", "a", "m"] {
            editor.type_text(ch, 64);
        }
        editor.insert(" pasted", 64);
        editor.move_home(false);
        editor.type_text(">", 64);
        assert_eq!(editor.text(), ">gam pasted");
        assert!(editor.undo());
        assert_eq!(editor.text(), "gam pasted");
        assert!(editor.undo());
        assert_eq!(editor.text(), "gam");
        assert!(editor.undo());
        assert_eq!(editor.text(), "");
        assert!(!editor.undo());
        assert!(editor.redo());
        assert_eq!(editor.text(), "gam");
        assert_eq!(editor.cursor(), 3);
        // A new edit drops the redo history.
        editor.backspace();
        assert!(!editor.redo());
        assert!(editor.undo());
        assert_eq!(editor.text(), "gam");
        // An edit that changes nothing is not a step.
        let mut empty = SearchEditor::default();
        assert!(!empty.backspace());
        assert!(!empty.undo());
        // A query restored from a pane starts with no history.
        assert!(!SearchEditor::from_text("kept".into(), 64).undo());
        // A held Backspace undoes in one step, however long the query.
        let long = "a".repeat(150);
        let mut editor = SearchEditor::default();
        editor.insert(&long, 4096);
        while editor.backspace() {}
        assert!(editor.undo());
        assert_eq!(editor.text(), long);
        // A caret moved by accessibility ends a typing run.
        let mut editor = SearchEditor::default();
        editor.type_text("foo", 64);
        editor.set_character_selection(0, 0);
        editor.type_text("x", 64);
        assert!(editor.undo());
        assert_eq!(editor.text(), "foo");
        // Replacing the query with the same text is not a step, and undo
        // reports no change, so the search is not restarted.
        let mut editor = SearchEditor::from_text("foo".into(), 64);
        editor.select_all();
        editor.insert("foo", 64);
        assert!(!editor.undo());
        // Typing over a selection undoes back to the selection in one step.
        let mut editor = SearchEditor::from_text("shalom".into(), 64);
        editor.select_all();
        for ch in ["x", "y", "z"] {
            editor.type_text(ch, 64);
        }
        assert!(editor.undo());
        assert_eq!(editor.text(), "shalom");
        assert_eq!(editor.selected_text(), Some("shalom"));
    }

    /// `Cmd+Backspace`, `Cmd+Delete` and `Ctrl+K` delete to the ends, or the
    /// selection when there is one.
    #[test]
    fn line_deletes_reach_the_ends_of_the_query() {
        let mut editor = SearchEditor::from_text("alpha beta".into(), 64);
        editor.move_left(false, true);
        assert!(editor.delete_to_start());
        assert_eq!((editor.text(), editor.cursor()), ("beta", 0));
        editor.move_right(false, false);
        assert!(editor.delete_to_end());
        assert_eq!(editor.text(), "b");
        assert!(!editor.delete_to_end());
        let mut editor = SearchEditor::from_text("alpha beta".into(), 64);
        editor.set_character_selection(1, 3);
        assert!(editor.delete_to_start());
        assert_eq!(editor.text(), "aha beta");
        assert!(editor.undo());
        assert_eq!(editor.text(), "alpha beta");
    }

    /// A click maps to what was painted, not to the cell grid: a fallback
    /// font here draws each CJK glyph 1.5 cells wide instead of 2, so by cells
    /// "b" would sit at column 6 while it is painted at 3.5 to 4.5.
    #[test]
    fn pointer_mapping_follows_the_painted_glyphs() {
        let cluster = |start, end, left, right| kettle_render::SearchEditorCluster {
            start,
            end,
            left,
            right,
            rtl: false,
        };
        let clusters = [
            cluster(0, 3, 10.0, 25.0),
            cluster(3, 6, 25.0, 40.0),
            cluster(6, 7, 40.0, 50.0),
        ];
        let stops = [(0, 10.0), (3, 25.0), (6, 40.0), (7, 50.0)];
        // Caret: the nearest boundary.
        assert_eq!(super::nearest_stop_byte(&stops, 11.0), Some(0));
        assert_eq!(super::nearest_stop_byte(&stops, 30.0), Some(3));
        assert_eq!(super::nearest_stop_byte(&stops, 44.0), Some(6));
        assert_eq!(super::nearest_stop_byte(&stops, 99.0), Some(7));
        // Character under the pointer, for word and line selection.
        assert_eq!(super::cluster_byte_at(&clusters, 41.0), Some(6));
        assert_eq!(super::cluster_byte_at(&clusters, 25.0), Some(3));
        assert_eq!(super::cluster_byte_at(&clusters, 24.9), Some(0));
        assert_eq!(super::cluster_byte_at(&clusters, 80.0), Some(6));
        assert_eq!(super::cluster_byte_at(&[], 5.0), None);
        let mut editor = SearchEditor::from_text("日本b".into(), 64);
        editor.set_cursor_at(super::QueryPoint::Byte(6), false);
        assert_eq!(editor.cursor(), 6);
        // A painted byte past the text, from a stale frame, lands on the end.
        editor.set_cursor_at(super::QueryPoint::Byte(99), false);
        assert_eq!(editor.cursor(), 7);
    }

    /// A painted byte is used as is. Converting it to a column and back
    /// counted lam-alef as one column one way and two the other, so a click
    /// at the end of an Arabic word landed a letter early.
    #[test]
    fn a_click_lands_on_the_painted_byte_in_arabic() {
        let mut editor = SearchEditor::from_text("السلام".into(), 64);
        editor.set_cursor_at(super::QueryPoint::Byte(12), false);
        assert_eq!(editor.cursor(), 12);
        editor.set_cursor_at(super::QueryPoint::Byte(10), false);
        assert_eq!(editor.cursor(), 10);
        // A byte inside a grapheme snaps back to its start.
        let mut editor = SearchEditor::from_text("e\u{301}x".into(), 64);
        editor.set_cursor_at(super::QueryPoint::Byte(2), false);
        assert_eq!(editor.cursor(), 0);
        editor.select_word_at(super::QueryPoint::Byte(3));
        assert_eq!(editor.selected_text(), Some("e\u{301}x"));
    }

    #[test]
    fn a_new_search_remembers_the_status_shown_before_it() {
        let now = std::time::Instant::now();
        let mut state = super::SearchState {
            editor: SearchEditor::from_text("zz".into(), 64),
            status: kettle_render::SearchStatus::NoMatch,
            ..Default::default()
        };
        state.note_edit(now);
        assert_eq!(state.status, kettle_render::SearchStatus::Searching);
        assert_eq!(
            state.status_before_search,
            Some(kettle_render::SearchStatus::NoMatch)
        );
        assert_eq!(state.searching_since, Some(now));
        // Another edit while the search is still running keeps its start.
        state.note_edit(now + std::time::Duration::from_millis(50));
        assert_eq!(
            state.status_before_search,
            Some(kettle_render::SearchStatus::NoMatch)
        );
        assert_eq!(state.searching_since, Some(now));
        // Once it finishes, the next edit starts a new grace period.
        state.status = kettle_render::SearchStatus::Match;
        let later = now + std::time::Duration::from_millis(400);
        state.note_edit(later);
        assert_eq!(
            state.status_before_search,
            Some(kettle_render::SearchStatus::Match)
        );
        assert_eq!(state.searching_since, Some(later));
    }

    /// Only `No match` is held over a new search. A prompt, an error or a
    /// step result belongs to the old query, so the slot stays blank.
    #[test]
    fn only_no_match_is_held_over_a_new_search() {
        use kettle_render::SearchStatus as S;
        let now = std::time::Instant::now();
        for (before, held) in [
            (S::NoMatch, S::NoMatch),
            (S::Match, S::Match),
            (S::Typing, S::Match),
            (S::Invalid, S::Match),
            (S::TooComplex, S::Match),
            (S::TooLong, S::Match),
            (S::End, S::Match),
            (S::Start, S::Match),
            (S::Wrapped, S::Match),
            (S::Limited, S::Match),
        ] {
            let mut state = super::SearchState {
                editor: SearchEditor::from_text("zz".into(), 64),
                status: before,
                ..Default::default()
            };
            state.note_edit(now);
            assert_eq!(state.painted_status(S::Searching, now), held, "{before:?}");
        }
    }

    /// The grace runs from the end of the typing pause, stays over once the
    /// bar says `Searching…`, and never stretches with output or retries.
    #[test]
    fn the_searching_label_waits_for_a_slow_scan_and_then_stays() {
        use kettle_render::SearchStatus as S;
        let ms = std::time::Duration::from_millis;
        let t0 = std::time::Instant::now();
        let mut state = super::SearchState {
            editor: SearchEditor::from_text("zz".into(), 64),
            status: S::NoMatch,
            ..Default::default()
        };
        state.note_edit(t0);
        // Typing again inside the pause moves the pause.
        state.note_edit(t0 + ms(300));
        assert_eq!(state.searching_label_at(), Some(t0 + ms(1000)));
        // The full scan starts at the end of the pause and clears the retry
        // deadline; the grace still runs from there.
        state.unlimited_retry_at = None;
        assert_eq!(state.painted_status(S::Searching, t0 + ms(950)), S::NoMatch);
        assert_eq!(
            state.painted_status(S::Searching, t0 + ms(1000)),
            S::Searching
        );
        // Output re-arms a quiet retry: the label does not go back.
        state.quiet_retry_pending = true;
        state.unlimited_retry_at = Some(t0 + ms(1600));
        assert_eq!(
            state.painted_status(S::Searching, t0 + ms(1100)),
            S::Searching
        );
        // Nor does another edit once the label is up.
        state.note_edit(t0 + ms(1200));
        assert_eq!(
            state.painted_status(S::Searching, t0 + ms(1250)),
            S::Searching
        );
        // A finished search paints its own status.
        state.status = S::Match;
        assert_eq!(state.searching_label_at(), None);
        assert_eq!(state.painted_status(S::Match, t0 + ms(1250)), S::Match);
    }

    /// Output that arrives after a finished search starts a search with no
    /// typing pause: its grace runs from the output.
    #[test]
    fn a_search_after_output_says_searching_once_its_grace_ends() {
        use kettle_render::SearchStatus as S;
        let ms = std::time::Duration::from_millis;
        let t0 = std::time::Instant::now();
        let mut state = super::SearchState {
            editor: SearchEditor::from_text("zz".into(), 64),
            status: S::NoMatch,
            ..Default::default()
        };
        state.note_edit(t0);
        state.status = S::Match;
        state.set_status_at(S::Searching, t0 + ms(2000));
        state.unlimited_retry_at = Some(t0 + ms(2500));
        assert_eq!(state.searching_label_at(), Some(t0 + ms(2200)));
        assert_eq!(state.painted_status(S::Searching, t0 + ms(2100)), S::Match);
        assert_eq!(
            state.painted_status(S::Searching, t0 + ms(2200)),
            S::Searching
        );
    }

    #[test]
    fn large_visible_projection_resumes_without_becoming_limited() {
        let key = VisibleSearchKey {
            revision: 7,
            pane: 2,
            output_generation: 11,
            columns: 480,
            screen_lines: 216,
            history_size: 1_000,
            display_offset: 400,
            focused: None,
        };
        let viewport = kettle_core::SearchBounds::new(
            kettle_core::SearchPoint::new(-400, 0),
            kettle_core::SearchPoint::new(-185, 479),
        );
        let context = kettle_core::SearchBounds::new(
            kettle_core::SearchPoint::new(-184, 0),
            kettle_core::SearchPoint::new(-85, 479),
        );
        let mut scan = VisibleSearchScan::new(key, vec![viewport, context], None, false);
        let continuation = kettle_core::SearchPoint::new(-264, 0);
        assert!(!scan.apply_batch(kettle_core::SearchBatch {
            matches: vec![kettle_core::SearchSpan::new(
                kettle_core::SearchPoint::new(-300, 4),
                kettle_core::SearchPoint::new(-300, 9),
            )],
            cancelled: false,
            exhausted: false,
            truncated: false,
            accuracy_limited: false,
            continuation: Some(continuation),
        }));
        assert_eq!(scan.current_bounds().unwrap().start, continuation);
        assert!(!scan.truncated);

        assert!(!scan.apply_batch(kettle_core::SearchBatch {
            matches: Vec::new(),
            cancelled: false,
            exhausted: true,
            truncated: false,
            accuracy_limited: false,
            continuation: None,
        }));
        assert_eq!(scan.current_bounds().unwrap(), context);
        assert!(!scan.apply_batch(kettle_core::SearchBatch {
            matches: Vec::new(),
            cancelled: false,
            exhausted: false,
            truncated: false,
            accuracy_limited: false,
            continuation: Some(kettle_core::SearchPoint::new(-120, 0)),
        }));
        assert!(scan.apply_batch(kettle_core::SearchBatch {
            matches: Vec::new(),
            cancelled: false,
            exhausted: true,
            truncated: false,
            accuracy_limited: false,
            continuation: None,
        }));
        assert!(!scan.truncated);
        assert_eq!(scan.matches.len(), 1);
    }
}
