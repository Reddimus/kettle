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
    pub(crate) dragging_editor: bool,
    pub(crate) wrap: bool,
    pub(crate) case_mode: kettle_config::SearchCaseSensitivity,
    pub(crate) invert: bool,
    pub(crate) revision: u64,
    pub(crate) unlimited_retry_at: Option<std::time::Instant>,
    pub(crate) quiet_retry_pending: bool,
    pub(crate) pre_open_display_offset: Option<usize>,
    pub(crate) background: Option<BackgroundSearch>,
}

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
            dragging_editor: false,
            wrap: true,
            case_mode: kettle_config::SearchCaseSensitivity::Smart,
            invert: false,
            revision: 0,
            unlimited_retry_at: None,
            quiet_retry_pending: false,
            pre_open_display_offset: None,
            background: None,
        }
    }
}

impl SearchState {
    pub(crate) fn query(&self) -> &str {
        self.editor.text()
    }

    pub(crate) fn note_edit(&mut self, now: std::time::Instant) {
        self.revision = self.revision.wrapping_add(1);
        self.unlimited_retry_at =
            (!self.editor.text().is_empty()).then_some(now + std::time::Duration::from_millis(500));
        self.quiet_retry_pending = false;
        self.status = if self.editor.text().is_empty() {
            kettle_render::SearchStatus::Typing
        } else {
            kettle_render::SearchStatus::Searching
        };
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
        self.unlimited_retry_at = (!compile_error && !self.editor.text().is_empty())
            .then_some(now + std::time::Duration::from_millis(500));
        if !compile_error {
            self.status = if self.editor.text().is_empty() {
                kettle_render::SearchStatus::Typing
            } else {
                kettle_render::SearchStatus::Searching
            };
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

/// Map a signed editor-cell offset to a query display column.
///
/// Cell zero is the opening bracket and cell one is the first visible query column. Keeping the
/// offset signed lets a drag beyond the left edge walk backward through a scrolled query.
pub(crate) fn pointer_query_column(horizontal_scroll: usize, relative_cell: isize) -> usize {
    if relative_cell >= 1 {
        horizontal_scroll.saturating_add(relative_cell.saturating_sub(1) as usize)
    } else {
        horizontal_scroll.saturating_sub(1isize.saturating_sub(relative_cell) as usize)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SearchEditor {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
    horizontal_scroll: usize,
}

impl SearchEditor {
    pub(crate) fn from_text(text: String, max_bytes: usize) -> Self {
        let mut editor = Self::default();
        editor.replace_all(&text, max_bytes);
        editor
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
        *self = replacement;
        outcome
    }

    pub(crate) fn insert(&mut self, value: &str, max_bytes: usize) -> EditOutcome {
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

    pub(crate) fn backspace(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let previous = previous_grapheme_boundary(&self.text, self.cursor);
        if previous == self.cursor {
            return false;
        }
        self.text.replace_range(previous..self.cursor, "");
        self.cursor = previous;
        true
    }

    pub(crate) fn delete(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let next = next_grapheme_boundary(&self.text, self.cursor);
        if next == self.cursor {
            return false;
        }
        self.text.replace_range(self.cursor..next, "");
        true
    }

    pub(crate) fn delete_word_backward(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let previous = previous_word_boundary(&self.text, self.cursor);
        if previous == self.cursor {
            return false;
        }
        self.text.replace_range(previous..self.cursor, "");
        self.cursor = previous;
        true
    }

    pub(crate) fn delete_word_forward(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let next = next_word_boundary(&self.text, self.cursor);
        if next == self.cursor {
            return false;
        }
        self.text.replace_range(self.cursor..next, "");
        true
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
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    pub(crate) fn set_cursor_column(&mut self, column: usize, selecting: bool) {
        let byte = byte_at_display_column(&self.text, column);
        self.move_cursor(byte, selecting);
    }

    pub(crate) fn set_character_selection(&mut self, anchor: usize, focus: usize) {
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

    pub(crate) fn select_word_at_column(&mut self, column: usize) {
        let byte = byte_at_display_column(&self.text, column);
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
        let cursor = text[..cursor_byte].width();
        if visible_columns == 0 {
            return cursor;
        }
        let minimum = display_column_boundary_at_or_after(
            text,
            cursor.saturating_sub(visible_columns.saturating_sub(1)),
        );
        let maximum = display_column_boundary_at_or_after(
            text,
            text.width()
                .saturating_sub(visible_columns.saturating_sub(1)),
        )
        .min(cursor);
        current_scroll.clamp(minimum.min(maximum), maximum)
    }

    #[cfg(test)]
    fn cursor_column(&self) -> usize {
        self.text[..self.cursor].width()
    }

    fn move_cursor(&mut self, next: usize, selecting: bool) {
        debug_assert!(self.text.is_char_boundary(next));
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
