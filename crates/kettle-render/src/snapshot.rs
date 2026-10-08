//! Decouple PTY parsing from GPU rendering.
//!
//! Rendering from a `&Term<EventProxy>` borrowed through a held `MutexGuard`
//! would keep the pane's Term lock held across the WHOLE GPU frame
//! (cosmic-text shaping, `surface.get_current_texture()` which can block up
//! to a vsync, submit, present). Under output flood, frames fire at the
//! output coalescer's paint budget, so the lock would stay held nearly
//! continuously and starve the PTY reader thread (`processor.advance` blocks
//! on the same lock). Measured that way, throughput was 0.42-0.8 MB/s against
//! 3-9 MB/s for Windows Terminal, Alacritty and WezTerm on the same harness.
//!
//! Instead, capture the pane's renderable state into a [`PaneSnapshot`]
//! while the lock is held (a µs-scale flat copy), then drop the guard and
//! render from the snapshot. Cells are captured RAW (unresolved `AnsiColor`,
//! raw `Flags`) in exact `display_iter` order, so the renderer's per-cell
//! loop (SGR resolution, INVERSE swap, DIM blend, minimum-contrast lift, run
//! merging) runs the same logic without holding the lock.
//!
//! Snapshots are pooled per window (`WindowState::pane_snapshots`): the
//! `cells` Vec keeps its high-water capacity across frames, so steady-state
//! capture does zero allocation.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors as TermColors;
use alacritty_terminal::term::{RenderableCursor, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape};
use kettle_core::EventProxy;

use crate::card_marks::CardMarks;
use crate::inline_cards::MAX_CARD_ROWS;

/// Max combining (zero-width) marks stored inline per [`SnapCell`]. A cell with
/// more than this many marks (pathological) has the excess dropped at capture —
/// the rendered base grapheme stays correct, only an extra accent beyond the
/// fourth is lost. Inline storage keeps `SnapCell: Copy` and the snapshot's
/// zero-allocation pooling (no per-cell `Vec`).
const MAX_ZEROWIDTH: usize = 4;

/// One viewport cell, captured verbatim from the grid's `display_iter`
/// (including wide-char spacers — the renderer's wide-cursor logic depends
/// on seeing them).
#[derive(Clone, Copy)]
pub struct SnapCell {
    /// Grid-absolute line — negative when scrolled into history, exactly as
    /// `display_iter` yields it. The renderer converts to a viewport row
    /// with `line + display_offset` where needed.
    pub line: i32,
    pub col: usize,
    pub c: char,
    /// RAW (unresolved) colors; `color::resolve` runs render-side.
    pub fg: AnsiColor,
    pub bg: AnsiColor,
    pub flags: Flags,
    /// SGR 58 per-cell underline color (neovim spell squiggles).
    pub underline_color: Option<AnsiColor>,
    /// Combining (zero-width) marks layered on `c`, such as a decomposed
    /// accent (`e`+U+0301), an emoji ZWJ sequence, or a variation selector.
    /// Captured so the renderer draws the full grapheme rather than a
    /// stripped base char. Stored inline (see [`MAX_ZEROWIDTH`]); read via
    /// [`SnapCell::zerowidth`], mirroring the grid `Cell::zerowidth`.
    zerowidth: [char; MAX_ZEROWIDTH],
    // MAX_ZEROWIDTH + 1 records overflow without adding storage to SnapCell.
    zerowidth_len: u8,
}

impl SnapCell {
    fn from_grid(point: Point, cell: &Cell) -> Self {
        let mut zerowidth = ['\0'; MAX_ZEROWIDTH];
        let mut zerowidth_len = 0u8;
        if let Some(marks) = cell.zerowidth() {
            for &mark in marks.iter().take(MAX_ZEROWIDTH) {
                zerowidth[zerowidth_len as usize] = mark;
                zerowidth_len += 1;
            }
            if marks.len() > MAX_ZEROWIDTH {
                zerowidth_len = (MAX_ZEROWIDTH + 1) as u8;
            }
        }
        Self {
            line: point.line.0,
            col: point.column.0,
            c: cell.c,
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags,
            underline_color: cell.underline_color(),
            zerowidth,
            zerowidth_len,
        }
    }

    /// Reconstruct the grid-absolute `Point` this cell was captured at
    /// (for `SelectionRange::contains`).
    #[inline]
    pub fn point(&self) -> Point {
        Point::new(Line(self.line), Column(self.col))
    }

    /// The combining (zero-width) marks layered on this cell's base char, in
    /// order — empty when the cell carries none. Mirrors the grid
    /// `Cell::zerowidth()` (which returns `Option<&[char]>`); here an empty
    /// slice is returned instead of `None` since the inline array is always
    /// present.
    #[inline]
    pub fn zerowidth(&self) -> &[char] {
        &self.zerowidth[..usize::from(self.zerowidth_len).min(MAX_ZEROWIDTH)]
    }

    /// Long placeholder clusters belong to the card/fallback pass, even when
    /// no active registration requires collecting their complete identity.
    pub fn is_card_placeholder(&self) -> bool {
        self.c == '\u{10eeee}' && usize::from(self.zerowidth_len) > MAX_ZEROWIDTH
    }
}

/// Everything the renderer reads from a pane's `Term`, captured under the
/// Term lock so the GPU frame can run lock-free. Mirrors alacritty's
/// `RenderableContent` plus the grid dimensions the scrollbar / image
/// anchoring need.
pub struct PaneSnapshot {
    /// Viewport cells in `display_iter` order (row-major, all columns).
    pub cells: Vec<SnapCell>,
    /// Full inline-card combining sequences, independent of SnapCell glyph marks.
    pub(crate) card_marks: CardMarks,
    pub(crate) card_marks_collected: bool,
    // Complete rows immediately outside the viewport, for clipped card context.
    pub(crate) card_context: Vec<SnapCell>,
    pub cursor: RenderableCursor,
    /// Whether `cursor` is alacritty_terminal's scrollback-aware vi cursor.
    pub vi_mode: bool,
    /// DEC cursor blink state captured with the cursor so UI-only redraws do
    /// not need to reacquire the terminal lock just to build the overlay.
    pub cursor_blinking: bool,
    /// Grid-absolute selection range (`contains` does the point math).
    pub selection: Option<SelectionRange>,
    /// Full 269-slot color table (OSC 4/10/11/12 overrides + dims) — `Copy`
    /// in alacritty, ~1KB flat.
    pub colors: TermColors,
    pub display_offset: usize,
    pub columns: usize,
    pub screen_lines: usize,
    /// Monotonic document-row id of the oldest retained grid row.
    pub history_origin: u64,
    pub history_size: usize,
}

impl Default for PaneSnapshot {
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            card_marks: CardMarks::default(),
            card_marks_collected: false,
            card_context: Vec::new(),
            cursor: RenderableCursor {
                shape: CursorShape::Hidden,
                point: Point::new(Line(0), Column(0)),
            },
            vi_mode: false,
            cursor_blinking: false,
            selection: None,
            colors: TermColors::default(),
            display_offset: 0,
            columns: 0,
            screen_lines: 0,
            history_origin: 0,
            history_size: 0,
        }
    }
}

impl PaneSnapshot {
    pub fn captures_card_marks(&self) -> bool {
        self.card_marks_collected
    }

    /// Capture `term`'s renderable state into this (pooled) snapshot.
    ///
    /// Called with the Term mutex held; everything here is a flat copy —
    /// no shaping, no resolution, no allocation once `cells` has reached
    /// its high-water capacity.
    pub fn capture(&mut self, term: &Term<EventProxy>) {
        self.capture_with_card_marks(term, false);
    }

    /// Collect complete placeholder marks and a fixed band of retained context
    /// when the pane has cards. Decode after releasing its lock.
    pub fn capture_with_card_marks(&mut self, term: &Term<EventProxy>, collect_cards: bool) {
        let grid = term.grid();
        self.columns = grid.columns();
        self.screen_lines = grid.screen_lines();
        self.history_origin = grid.history_origin();
        self.history_size = grid.history_size();

        let content = term.renderable_content();
        self.display_offset = content.display_offset;
        self.cursor = content.cursor;
        self.vi_mode = content.mode.contains(TermMode::VI);
        self.cursor_blinking = term.cursor_style().blinking;
        self.selection = content.selection;
        self.colors = *content.colors;

        self.cells.clear();
        self.card_marks.clear();
        self.card_context.clear();
        self.card_marks_collected = collect_cards;
        self.cells
            .reserve(self.columns.saturating_mul(self.screen_lines));
        for indexed in content.display_iter {
            let cell = indexed.cell;
            if collect_cards {
                self.card_marks.collect(
                    self.cells.len(),
                    cell.c,
                    cell.zerowidth().unwrap_or_default(),
                );
            }
            self.cells.push(SnapCell::from_grid(indexed.point, cell));
        }
        if collect_cards && let (Some(first), Some(last)) = (self.cells.first(), self.cells.last())
        {
            // A card intersects at most MAX_CARD_ROWS and owns one context row
            // beyond its body. Capture a fixed band, never the whole history.
            let band = i32::from(MAX_CARD_ROWS) + 1;
            let start = first.line.saturating_sub(band).max(grid.topmost_line().0);
            let end = last.line.saturating_add(band).min(grid.bottommost_line().0);
            let first_line = first.line;
            let last_line = last.line;
            for line in (start..first_line).chain(last_line + 1..=end) {
                for column in 0..self.columns {
                    let point = Point::new(Line(line), Column(column));
                    let cell = &grid[point];
                    // Visible cells have first claim on the existing mark cap.
                    self.card_marks.collect(
                        self.cells.len() + self.card_context.len(),
                        cell.c,
                        cell.zerowidth().unwrap_or_default(),
                    );
                    self.card_context.push(SnapCell::from_grid(point, cell));
                }
            }
        }
    }

    pub(crate) fn card_cell(&self, index: usize) -> Option<&SnapCell> {
        if index < self.cells.len() {
            self.cells.get(index)
        } else {
            self.card_context.get(index - self.cells.len())
        }
    }

    pub(crate) fn card_row(&self, line: i32) -> &[SnapCell] {
        let cells = if self.cells.first().is_some_and(|cell| line >= cell.line)
            && self.cells.last().is_some_and(|cell| line <= cell.line)
        {
            &self.cells
        } else {
            &self.card_context
        };
        let start = cells.partition_point(|cell| cell.line < line);
        let end = cells[start..].partition_point(|cell| cell.line == line);
        &cells[start..start + end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::Term;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::Config as TermConfig;
    use alacritty_terminal::vte::ansi::Processor;
    use kettle_core::Waker;

    /// Minimal `Dimensions` so the test can build a `Term` without pulling in
    /// kettle-core's (private) `TermSize` test helper.
    struct Size {
        cols: usize,
        rows: usize,
    }
    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.rows
        }
        fn screen_lines(&self) -> usize {
            self.rows
        }
        fn columns(&self) -> usize {
            self.cols
        }
    }

    fn test_term(cols: usize, rows: usize) -> (Term<EventProxy>, Processor) {
        test_term_with_history(cols, rows, TermConfig::default().scrolling_history)
    }

    fn test_term_with_history(
        cols: usize,
        rows: usize,
        scrolling_history: usize,
    ) -> (Term<EventProxy>, Processor) {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let proxy = EventProxy::new(tx, waker);
        let term = Term::new(
            TermConfig {
                scrolling_history,
                ..TermConfig::default()
            },
            &Size { cols, rows },
            proxy,
        );
        (term, Processor::new())
    }

    #[test]
    fn snapcell_zerowidth_roundtrips_combining_marks() {
        // Feed a base 'e' immediately followed by COMBINING ACUTE ACCENT
        // (U+0301): the alacritty engine attaches the accent as a zero-width
        // mark on the base cell. capture() must carry it into SnapCell.
        let (mut term, mut proc) = test_term(8, 2);
        proc.advance(&mut term, "e\u{0301}x".as_bytes());

        let mut snap = PaneSnapshot::default();
        snap.capture(&term);

        // Column 0 holds the base 'e' with the accent as its only mark.
        let base = snap
            .cells
            .iter()
            .find(|c| c.col == 0 && c.line == 0)
            .expect("base cell present");
        assert_eq!(base.c, 'e');
        assert_eq!(base.zerowidth(), &['\u{0301}']);

        // Column 1 holds plain 'x' with no marks (empty slice, not None).
        let next = snap
            .cells
            .iter()
            .find(|c| c.col == 1 && c.line == 0)
            .expect("next cell present");
        assert_eq!(next.c, 'x');
        assert!(next.zerowidth().is_empty());
    }

    #[test]
    fn snapcell_zerowidth_empty_for_plain_cells() {
        let (mut term, mut proc) = test_term(8, 2);
        proc.advance(&mut term, b"ab");
        let mut snap = PaneSnapshot::default();
        snap.capture(&term);
        assert!(snap.cells.iter().all(|c| c.zerowidth().is_empty()));
    }

    #[test]
    fn card_context_is_bounded_and_pooled_independently_of_scrollback_depth() {
        let (mut term, mut processor) = test_term_with_history(40, 4, 5000);
        processor.advance(&mut term, "ordinary\r\n".repeat(1000).as_bytes());
        assert!(term.grid().history_size() > 500);
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(200));
        let mut snap = PaneSnapshot::default();
        snap.capture_with_card_marks(&term, true);
        assert_eq!(snap.cells.len(), 4 * 40);
        assert_eq!(snap.card_context.len(), 2 * 13 * 40);
        assert_eq!(snap.card_context[0].line, snap.cells[0].line - 13);
        assert_eq!(
            snap.card_context.last().unwrap().line,
            snap.cells.last().unwrap().line + 13
        );
        assert!(
            snap.card_context
                .windows(2)
                .all(|pair| (pair[0].line, pair[0].col) < (pair[1].line, pair[1].col))
        );
        let pointer = snap.card_context.as_ptr();
        let capacity = snap.card_context.capacity();
        snap.capture_with_card_marks(&term, true);
        assert_eq!(snap.card_context.as_ptr(), pointer);
        assert_eq!(snap.card_context.capacity(), capacity);
        snap.capture_with_card_marks(&term, false);
        assert!(snap.card_context.is_empty());
        assert_eq!(snap.card_context.capacity(), capacity);
        assert_eq!(snap.cells.len(), 4 * 40);
        assert_eq!(snap.card_marks.cells().len(), 0);
        term.scroll_display(alacritty_terminal::grid::Scroll::Top);
        snap.capture_with_card_marks(&term, true);
        assert_eq!(snap.card_context.len(), 13 * 40);
        assert!(
            snap.card_context
                .iter()
                .all(|cell| cell.line > snap.cells.last().unwrap().line)
        );
        term.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
        snap.capture_with_card_marks(&term, true);
        assert_eq!(snap.card_context.len(), 13 * 40);
        assert!(
            snap.card_context
                .iter()
                .all(|cell| cell.line < snap.cells[0].line)
        );
    }

    #[test]
    fn capture_carries_cursor_blink_state_for_lock_free_ui_redraws() {
        let (mut term, mut proc) = test_term(8, 2);
        let mut snap = PaneSnapshot::default();

        proc.advance(&mut term, b"\x1b[?12h");
        snap.capture(&term);
        assert!(snap.cursor_blinking);

        proc.advance(&mut term, b"\x1b[?12l");
        snap.capture(&term);
        assert!(!snap.cursor_blinking);
    }

    #[test]
    fn capture_carries_monotonic_history_origin_after_ring_eviction() {
        let (mut term, mut proc) = test_term_with_history(8, 2, 2);
        proc.advance(&mut term, b"0\r\n1\r\n2\r\n3\r\n4\r\n5");
        assert_eq!(term.grid().history_size(), 2);
        assert!(term.grid().history_origin() > 0);

        let mut snap = PaneSnapshot::default();
        snap.capture(&term);

        assert_eq!(snap.history_origin, term.grid().history_origin());
        assert_eq!(snap.history_size, 2);
    }
    #[test]
    fn card_capture_preserves_full_marks_without_widening_snapcell() {
        let (mut term, mut proc) = test_term(8, 2);
        let marks = [
            '\u{0305}', '\u{030d}', '\u{030e}', '\u{0310}', '\u{0312}', '\u{033d}', '\u{033e}',
            '\u{033f}',
        ];
        let text: String = std::iter::once('\u{10eeee}').chain(marks).collect();
        proc.advance(&mut term, text.as_bytes());
        let mut snap = PaneSnapshot::default();
        snap.capture_with_card_marks(&term, true);
        let (snapshot_index, full_marks) = snap.card_marks.cells().next().unwrap();
        assert_eq!(snap.cells[snapshot_index].c, '\u{10eeee}');
        assert_eq!(snap.cells[snapshot_index].zerowidth(), &marks[..4]);
        assert_eq!(full_marks, marks);
        proc.advance(&mut term, b"\rX");
        snap.capture_with_card_marks(&term, true);
        assert!(snap.card_marks.cells().len() == 0);
    }

    #[test]
    fn ordinary_snapshot_clears_previous_card_sequences() {
        let (mut term, mut proc) = test_term(8, 2);
        proc.advance(
            &mut term,
            "\u{10eeee}\u{0305}\u{030d}\u{030e}\u{0310}\u{0312}\u{033d}\u{033e}\u{033f}".as_bytes(),
        );
        let mut snap = PaneSnapshot::default();
        snap.capture_with_card_marks(&term, true);
        assert_eq!(snap.card_marks.cells().len(), 1);
        snap.capture(&term);
        assert!(snap.card_marks.cells().len() == 0);
        assert_eq!(
            snap.cells
                .iter()
                .find(|c| c.col == 0 && c.line == 0)
                .unwrap()
                .zerowidth()
                .len(),
            4
        );
    }

    #[test]
    fn long_card_cluster_is_identifiable_without_registration_or_full_collection() {
        let (mut term, mut proc) = test_term(8, 2);
        proc.advance(&mut term,
            "\u{10eeee}\u{0305}\u{030d}\u{030e}\u{0310}\u{0312}\u{033d}\u{033e}\u{033f}e\u{0305}\u{030d}\u{030e}\u{0310}\u{0312}\u{033d}\u{033e}\u{033f}\u{10eeee}\u{0305}\u{030d}\u{030e}\u{0310}".as_bytes());
        let mut snap = PaneSnapshot::default();
        snap.capture(&term);
        assert!(snap.card_marks.cells().len() == 0);
        let row: Vec<_> = snap.cells.iter().filter(|c| c.line == 0).collect();
        assert!(row[0].is_card_placeholder());
        assert!(
            !row[1].is_card_placeholder(),
            "ordinary combining text retains its glyph path"
        );
        assert!(
            !row[2].is_card_placeholder(),
            "four-mark Kitty cells retain existing semantics"
        );
        assert_eq!(row[0].zerowidth().len(), 4);
        assert_eq!(row[1].zerowidth().len(), 4);
        assert_eq!(row[2].zerowidth().len(), 4);
    }
}
