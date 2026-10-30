//! Scrollback search state using the shared grapheme-aware text editor.

#[derive(Debug)]
pub(crate) struct SearchState {
    pub(crate) open: bool,
    pub(crate) editor: SearchEditor,
    pub(crate) target_pane: Option<u64>,
    pub(crate) compiled: Option<kettle_core::CompiledSearch>,
    pub(crate) highlight_compiled: Option<kettle_core::CompiledSearch>,
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
            highlight_compiled: None,
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
    pub(crate) fn set_compiled(&mut self, compiled: Option<kettle_core::CompiledSearch>) {
        // Each job owns its resume offsets, even when its bounds coincide with the other's.
        self.highlight_compiled = compiled.clone();
        self.compiled = compiled;
    }

    pub(crate) fn set_scan_token(&mut self, token: kettle_core::SearchScanToken) {
        for compiled in [&mut self.compiled, &mut self.highlight_compiled]
            .into_iter()
            .flatten()
        {
            compiled.set_scan_token(token);
        }
    }

    pub(crate) fn compiled_for_highlights(&mut self) -> &mut kettle_core::CompiledSearch {
        self.highlight_compiled
            .as_mut()
            .expect("compiled search checked")
    }

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
        self.set_compiled(None);
        self.compiled_revision = None;
        self.focused = None;
        self.revealed_focus = None;
        self.visible.clear();
        self.visible_truncated = false;
        self.visible_scan = None;
        self.visible_turn_pending = false;
        self.typing_scanned_revision = None;
        self.visible_key = None;
        self.anchor = None;
        self.scan_token = None;
        self.background = None;
    }

    pub(crate) fn invalidate_stale_navigation(&mut self, token: kettle_core::SearchScanToken) {
        if self.scan_token != Some(token) {
            self.restart_navigation(std::time::Instant::now());
            self.anchor = None;
            self.unlimited_retry_at = None;
        }
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

pub(crate) use crate::text_editor::{
    QueryPoint, TextEditor as SearchEditor, cluster_byte_at, display_width, nearest_stop_byte,
    pointer_query_column,
};

#[cfg(test)]
mod tests {
    use super::{SearchEditor, VisibleSearchKey, VisibleSearchScan};

    fn wrapped_search_term(text: &str) -> alacritty_terminal::Term<kettle_core::EventProxy> {
        use kettle_core::term::TermSize;
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut term = alacritty_terminal::Term::new(
            alacritty_terminal::term::Config::default(),
            &TermSize {
                columns: 80,
                screen_lines: 160,
            },
            kettle_core::EventProxy::new(tx, std::sync::Arc::new(|| {})),
        );
        let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
        parser.advance(&mut term, text.as_bytes());
        term
    }

    #[test]
    fn interleaved_navigation_and_highlights_finish_in_a_wrapped_line() {
        use kettle_core::{
            CaseSensitivity, CompiledSearch, SearchBounds, SearchDirection, SearchPoint,
        };
        let term = wrapped_search_term(&"a".repeat(12_000));
        let start = SearchPoint::new(100, 0);
        let navigation = SearchBounds::new(start, SearchPoint::new(159, 79));
        let mut visible = SearchBounds::new(start, SearchPoint::new(110, 79));
        let mut state = super::SearchState::default();
        state.set_compiled(CompiledSearch::compile("a", CaseSensitivity::Always).unwrap());
        let mut focused = None;
        let mut highlighted = Vec::new();
        let mut visible_done = false;
        for turn in 0..64 {
            if focused.is_none() {
                let batch = state.compiled.as_mut().unwrap().find_in_range(
                    &term,
                    navigation,
                    SearchDirection::Forward,
                    1,
                );
                assert!(!batch.accuracy_limited);
                if turn == 0 {
                    assert!(batch.matches.is_empty());
                    assert!(batch.continuation.is_some());
                }
                focused = batch.matches.first().copied();
            }
            if !visible_done {
                let batch = state.compiled_for_highlights().find_in_range(
                    &term,
                    visible,
                    SearchDirection::Forward,
                    1000 - highlighted.len(),
                );
                assert!(!batch.accuracy_limited);
                if let Some(cursor) = batch.continuation {
                    visible.start = cursor;
                } else {
                    visible_done = true;
                }
                highlighted.extend(batch.matches);
            }
            if focused.is_some() && visible_done {
                break;
            }
        }
        assert_eq!(
            focused.map(|span| span.start),
            Some(start),
            "navigation never reached the viewport"
        );
        assert!(visible_done, "highlights never finished");
        assert_eq!(highlighted.len(), 11 * 80);
        assert_eq!(highlighted.first().unwrap().start, start);
        assert_eq!(highlighted.last().unwrap().end, visible.end);
    }

    #[test]
    fn interleaved_empty_regex_matches_finish_in_a_wrapped_line() {
        use kettle_core::{
            CaseSensitivity, CompiledSearch, SearchBounds, SearchDirection, SearchPoint,
        };
        let term = wrapped_search_term(&"b".repeat(12_000));
        let start = SearchPoint::new(100, 0);
        let navigation = SearchBounds::new(start, SearchPoint::new(149, 79));
        let visible = SearchBounds::new(start, SearchPoint::new(110, 79));
        let mut state = super::SearchState::default();
        state.set_compiled(CompiledSearch::compile("a?", CaseSensitivity::Always).unwrap());
        let mut navigation_done = false;
        let mut visible_done = false;
        for turn in 0..64 {
            if !navigation_done {
                let batch = state.compiled.as_mut().unwrap().find_in_range(
                    &term,
                    navigation,
                    SearchDirection::Forward,
                    1,
                );
                assert!(!batch.accuracy_limited);
                assert!(batch.matches.is_empty());
                navigation_done = batch.exhausted;
                if turn == 0 {
                    assert!(!navigation_done);
                }
            }
            if !visible_done {
                let batch = state.compiled_for_highlights().find_in_range(
                    &term,
                    visible,
                    SearchDirection::Forward,
                    1000,
                );
                assert!(!batch.accuracy_limited);
                assert!(batch.matches.is_empty());
                visible_done = batch.exhausted;
            }
            if navigation_done && visible_done {
                break;
            }
        }
        assert!(navigation_done, "navigation never exhausted the range");
        assert!(visible_done, "highlights never exhausted the range");
    }

    #[test]
    fn output_invalidates_highlight_resume_offsets() {
        use kettle_core::{
            CaseSensitivity, CompiledSearch, SearchBounds, SearchDirection, SearchPoint,
            SearchScanToken,
        };
        let term = wrapped_search_term(&"a".repeat(12_000));
        let bounds = SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(149, 79));
        let mut state = super::SearchState::default();
        state.set_compiled(CompiledSearch::compile("a", CaseSensitivity::Always).unwrap());
        state.set_scan_token(SearchScanToken::capture(&term, 1, 1));
        let first = state.compiled_for_highlights().find_in_range(
            &term,
            bounds,
            SearchDirection::Forward,
            12_000,
        );
        assert!(first.continuation.is_some());
        assert!(!first.matches.is_empty());
        state.set_scan_token(SearchScanToken::capture(&term, 1, 2));
        let restarted = state.compiled_for_highlights().find_in_range(
            &term,
            bounds,
            SearchDirection::Forward,
            1,
        );
        assert_eq!(restarted.matches[0].start, bounds.start);
    }

    #[test]
    fn navigation_invalidates_stale_coordinates_before_reanchoring() {
        let current = kettle_core::SearchScanToken {
            query_revision: 3,
            output_generation: 10,
            layout: kettle_core::SearchLayout {
                columns: 80,
                screen_lines: 24,
                history_size: 200,
            },
        };
        let span = kettle_core::SearchSpan::new(
            kettle_core::SearchPoint::new(-100, 0),
            kettle_core::SearchPoint::new(-100, 2),
        );
        for previous in [
            current,
            kettle_core::SearchScanToken {
                output_generation: 9,
                ..current
            },
            kettle_core::SearchScanToken {
                query_revision: 2,
                ..current
            },
            kettle_core::SearchScanToken {
                layout: kettle_core::SearchLayout {
                    columns: 40,
                    ..current.layout
                },
                ..current
            },
        ] {
            let mut search = super::SearchState {
                editor: SearchEditor::from_text("hit".into(), 64),
                revision: 3,
                scan_token: Some(previous),
                focused: Some(span),
                revealed_focus: Some(span),
                visible: vec![span],
                typing_scanned_revision: Some(3),
                anchor: Some(span.start),
                ..Default::default()
            };
            search.invalidate_stale_navigation(current);
            if previous == current {
                assert_eq!(search.focused, Some(span));
                assert_eq!(search.visible, vec![span]);
            } else {
                assert!(search.focused.is_none());
                assert!(search.revealed_focus.is_none());
                assert!(search.visible.is_empty());
                assert!(search.typing_scanned_revision.is_none());
                assert!(search.anchor.is_none());
                assert!(search.unlimited_retry_at.is_none());
            }
        }
    }

    #[test]
    fn inserting_a_joiner_keeps_the_caret_on_a_grapheme_boundary() {
        let mut editor = SearchEditor::from_text("\u{1f469}\u{1f4bb}".into(), 64);
        editor.set_cursor_at(super::QueryPoint::Byte(4), false);
        assert!(editor.insert("\u{200d}", 64).changed);
        assert_eq!(
            editor.cursor(),
            editor.text().len(),
            "the caret must follow the joined emoji"
        );
        assert!(editor.backspace());
        assert_eq!(
            editor.text(),
            "",
            "Backspace must delete the whole joined emoji"
        );
        assert!(editor.undo());
        assert_eq!(editor.cursor(), editor.text().len());
    }

    #[test]
    fn plain_arrows_collapse_forward_and_reversed_selections() {
        for reversed in [false, true] {
            let mut editor = SearchEditor::from_text("abc".into(), 64);
            editor.set_character_selection(
                if reversed { 3 } else { 0 },
                if reversed { 0 } else { 3 },
            );
            editor.move_left(false, false);
            assert_eq!(
                editor.cursor(),
                0,
                "Left must collapse to the selection start"
            );
            editor.insert("x", 64);
            assert_eq!(editor.text(), "xabc");
            let mut editor = SearchEditor::from_text("abc".into(), 64);
            editor.set_character_selection(
                if reversed { 3 } else { 0 },
                if reversed { 0 } else { 3 },
            );
            editor.move_right(false, false);
            assert_eq!(
                editor.cursor(),
                3,
                "Right must collapse to the selection end"
            );
        }
    }

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
    fn cut_keeps_selection_until_clipboard_write_succeeds() {
        let mut editor = SearchEditor::from_text("needle".into(), 64);
        editor.select_all();
        assert!(!editor.cut_selection(64, |_| false));
        assert_eq!(editor.text(), "needle");
        assert_eq!(editor.selected_text(), Some("needle"));
        assert!(!editor.undo());
        assert!(editor.cut_selection(64, |selected| {
            assert_eq!(selected, "needle");
            true
        }));
        assert_eq!(editor.text(), "");
        assert!(editor.selected_text().is_none());
        assert!(editor.undo());
        assert_eq!(editor.text(), "needle");
        assert_eq!(editor.selected_text(), Some("needle"));
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
    fn double_click_selects_the_punctuation_under_the_pointer() {
        let mut editor = SearchEditor::from_text("ab..cd".into(), 64);
        for byte in [2, 3] {
            editor.select_word_at(super::QueryPoint::Byte(byte));
            assert_eq!(editor.selection(), Some(byte..byte + 1));
        }
        editor.select_word_at(super::QueryPoint::Byte(4));
        assert_eq!(editor.selected_text(), Some("cd"));
        editor.select_word_at(super::QueryPoint::Byte(6));
        assert_eq!(editor.selected_text(), Some("cd"));

        let mut editor = SearchEditor::from_text("ab \u{2764}\u{fe0f}!".into(), 64);
        editor.select_word_at(super::QueryPoint::Byte(2));
        assert_eq!(editor.selected_text(), Some(" "));
        editor.select_word_at(super::QueryPoint::Byte(3));
        assert_eq!(editor.selected_text(), Some("\u{2764}\u{fe0f}"));
        editor.select_word_at(super::QueryPoint::Byte(editor.text().len()));
        assert_eq!(editor.selected_text(), Some("!"));

        let mut empty = SearchEditor::default();
        empty.select_word_at(super::QueryPoint::Byte(0));
        assert_eq!(empty.selection(), None);
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
