//! Bounded grapheme-aware single-line editor shared by search and modal fields.

use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

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
pub(crate) struct TextEditor {
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

impl TextEditor {
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
        let boundary = grapheme_boundary_at_or_before(&self.text, self.cursor);
        if boundary != self.cursor {
            self.cursor = next_grapheme_boundary(&self.text, boundary);
        }
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

    /// Cut only after the clipboard writer accepts the selected text.
    pub(crate) fn cut_selection(
        &mut self,
        max_bytes: usize,
        write: impl FnOnce(&str) -> bool,
    ) -> bool {
        let Some(selected) = self.selected_text() else {
            return false;
        };
        if !write(selected) {
            return false;
        }
        self.insert("", max_bytes).changed
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
        if !selecting
            && !by_word
            && let Some(selection) = self.selection()
        {
            self.move_cursor(selection.start, false);
            return;
        }
        let next = if by_word {
            previous_word_boundary(&self.text, self.cursor)
        } else {
            previous_grapheme_boundary(&self.text, self.cursor)
        };
        self.move_cursor(next, selecting);
    }

    pub(crate) fn move_right(&mut self, selecting: bool, by_word: bool) {
        if !selecting
            && !by_word
            && let Some(selection) = self.selection()
        {
            self.move_cursor(selection.end, false);
            return;
        }
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
        let anchor = grapheme_boundary_at_or_before(&self.text, byte_at_character(anchor));
        let focus = grapheme_boundary_at_or_before(&self.text, byte_at_character(focus));
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
        let byte = if byte == self.text.len() {
            previous_grapheme_boundary(&self.text, byte)
        } else {
            byte
        };
        if let Some((start, word)) = self
            .text
            .unicode_word_indices()
            .find(|(start, word)| byte >= *start && byte < start.saturating_add(word.len()))
        {
            self.anchor = Some(start);
            self.cursor = start + word.len();
            return;
        }
        let start = byte;
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

impl std::ops::Deref for TextEditor {
    type Target = str;
    fn deref(&self) -> &str {
        self.text()
    }
}
impl From<String> for TextEditor {
    fn from(text: String) -> Self {
        Self::from_text(text, kettle_core::MAX_SEARCH_QUERY_BYTES)
    }
}
impl From<&str> for TextEditor {
    fn from(text: &str) -> Self {
        text.to_string().into()
    }
}
