//! Spacer-aware reconstruction of a grid row's text.
//!
//! A wide (CJK / emoji) glyph occupies two cells: the glyph lives in column `N`
//! (`Flags::WIDE_CHAR`) and a literal space is written into column `N+1`
//! (`Flags::WIDE_CHAR_SPACER`). Naively pushing `cell.c` for every column injects
//! that space, so `世界` reconstructs as `世 界` — and search, link/path
//! detection, quick-select hints, and the agent screen-scrape never match across
//! wide text. This single helper skips the spacer cells while keeping the
//! byte→column map exact. Every consumer shares it, so they can't drift apart.

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};

/// True for a cell that only pads out a wide glyph (the trailing space cell, or
/// the leading spacer used when a wide glyph would straddle the last column).
pub(crate) fn is_spacer(flags: Flags) -> bool {
    flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
}

/// A place in a pane's output: a document row (monotonic, see
/// `Grid::history_origin`) and a column on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputCursor {
    row: u64,
    column: usize,
}

impl OutputCursor {
    /// The document row.
    pub fn row(&self) -> u64 {
        self.row
    }
}

/// The output a reader has not had since `seen` (for screen readers).
#[derive(Debug, PartialEq, Eq)]
pub enum NewOutput {
    Lines(Vec<String>),
    /// More rows than are worth reading out; how many.
    Many(u64),
}

/// Rows past which new output is counted rather than read.
const OUTPUT_READ_ROWS: u64 = 200;

/// The output the cursor has moved past since `seen`: the rest of the row
/// it was on, then every row it has left since, never the row it is on now
/// (a prompt, or a line still being typed or written). With no `seen`, or
/// after the screen was cleared back above it, nothing is new.
/// The document row the cursor is on.
pub fn cursor_row(grid: &Grid<Cell>) -> u64 {
    grid.history_origin() + grid.history_size() as u64 + grid.cursor.point.line.0.max(0) as u64
}

/// See [`OutputCursor`]; rows up to `input` (the row a command was entered
/// on, and any it wrapped from) are the user's, not output.
pub fn new_output(
    grid: &Grid<Cell>,
    seen: Option<OutputCursor>,
    input: Option<u64>,
) -> (OutputCursor, NewOutput) {
    let origin = grid.history_origin();
    let history = grid.history_size() as u64;
    let cursor = grid.cursor.point;
    let now = OutputCursor {
        row: origin + history + cursor.line.0.max(0) as u64,
        column: cursor.column.0,
    };
    let Some(seen) = seen.filter(|seen| seen.row < now.row) else {
        return (now, NewOutput::Lines(Vec::new()));
    };
    let first = seen
        .row
        .max(origin)
        .max(input.map_or(0, |row| row.saturating_add(1)));
    if first >= now.row {
        return (now, NewOutput::Lines(Vec::new()));
    }
    if now.row - first > OUTPUT_READ_ROWS {
        return (now, NewOutput::Many(now.row - first));
    }
    let (mut text, mut columns, mut lines) = (String::new(), Vec::new(), Vec::new());
    for row in first..now.row {
        let line = (row - origin) as i64 - history as i64;
        row_text_into(grid, line as i32, grid.columns(), &mut text, &mut columns);
        let from = if row == seen.row { seen.column } else { 0 };
        let start = columns
            .iter()
            .position(|&c| c >= from)
            .unwrap_or(text.len());
        let piece = text[start..].trim();
        if !piece.is_empty() {
            lines.push(piece.to_string());
        }
    }
    (now, NewOutput::Lines(lines))
}

/// Rebuild row `line`'s text into `text` (cleared first), pushing to
/// `col_of_byte` the originating grid column for each UTF-8 byte appended — so a
/// regex match offset maps back to the exact column. Wide-char spacer cells are
/// skipped. Callers reuse both buffers across rows to amortize allocation.
///
/// A cell carrying combining (zero-width) marks — a decomposed `e` + U+0301, an
/// emoji ZWJ sequence, a variation selector — contributes its base char *and*
/// every `zerowidth()` mark, so search / link-detect / scrape see the full
/// grapheme rather than a stripped `e`. Each appended mark maps to the SAME
/// originating grid column as its base cell in `col_of_byte`, so a match offset
/// that lands on a mark still translates back to the base column.
pub fn row_text_into(
    grid: &Grid<Cell>,
    line: i32,
    cols: usize,
    text: &mut String,
    col_of_byte: &mut Vec<usize>,
) {
    text.clear();
    col_of_byte.clear();
    append_row(grid, line, cols, text, &mut |c| col_of_byte.push(c));
}

/// Append row `line`'s text to `text`, calling `byte_from` with the
/// originating grid column once per UTF-8 byte appended.
fn append_row(
    grid: &Grid<Cell>,
    line: i32,
    cols: usize,
    text: &mut String,
    byte_from: &mut impl FnMut(usize),
) {
    for c in 0..cols {
        let cell = &grid[Point::new(Line(line), Column(c))];
        if is_spacer(cell.flags) {
            continue;
        }
        let (ch, marks) =
            crate::inline_text::text_parts(cell.c, cell.zerowidth().unwrap_or_default());
        for _ in 0..ch.len_utf8() {
            byte_from(c);
        }
        text.push(ch);
        for &mark in marks {
            for _ in 0..mark.len_utf8() {
                byte_from(c);
            }
            text.push(mark);
        }
    }
}

/// Most viewport rows one soft-wrapped logical line may join, so a pathological
/// screen of wrapped text costs a bounded scan per detection.
pub const MAX_LOGICAL_ROWS: usize = 16;

/// What [`logical_line_into`] read: the index of the first row it did not
/// consume, and whether the logical line goes on past what it read. A line
/// cut before its first row continues from a row above (out of view, or
/// before a bound); one cut after its last row continues below it. A match
/// touching a cut edge may be the fragment of a longer token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalLine {
    pub next: usize,
    pub cut_before: bool,
    pub cut_after: bool,
}

/// Whether grid line `line` exists and ends in a soft wrap into the line below.
fn soft_wraps(grid: &Grid<Cell>, line: i32, cols: usize) -> bool {
    let top = -(grid.history_size() as i32);
    let bottom = grid.screen_lines() as i32 - 1;
    cols > 0
        && (top..=bottom).contains(&line)
        && grid[Point::new(Line(line), Column(cols - 1))]
            .flags
            .contains(Flags::WRAPLINE)
}

/// Rebuild the soft-wrapped logical line that starts at `lines[first]` into
/// `text` (cleared first): that row, then each following row while the row
/// before it ends in a soft wrap (`Flags::WRAPLINE` on its last column), up to
/// [`MAX_LOGICAL_ROWS`]. A hard line break never joins, so text a program
/// wrapped itself is not stitched together. `pos_of_byte` gets, per UTF-8 byte,
/// the index into `lines` and the grid column it came from.
pub fn logical_line_into(
    grid: &Grid<Cell>,
    lines: &[i32],
    first: usize,
    cols: usize,
    text: &mut String,
    pos_of_byte: &mut Vec<(usize, usize)>,
) -> LogicalLine {
    text.clear();
    pos_of_byte.clear();
    let cut_before = lines
        .get(first)
        .is_some_and(|&line| soft_wraps(grid, line - 1, cols));
    let mut index = first;
    let mut wrapped = false;
    while index < lines.len() {
        let line = lines[index];
        let row = index;
        append_row(grid, line, cols, text, &mut |c| pos_of_byte.push((row, c)));
        index += 1;
        wrapped = soft_wraps(grid, line, cols);
        let next_is_below = lines.get(index) == Some(&(line + 1));
        if !wrapped || !next_is_below || index - first >= MAX_LOGICAL_ROWS {
            break;
        }
    }
    LogicalLine {
        next: index,
        cut_before,
        cut_after: wrapped,
    }
}

/// Like [`row_text_into`] but *appends* only the text (no column map) to `out` —
/// for callers that just need the characters (e.g. the agent screen scrape).
/// Combining marks are appended after each base char, identically to
/// [`row_text_into`], so the scrape preserves accented / ZWJ graphemes.
pub fn append_row_text(grid: &Grid<Cell>, line: i32, cols: usize, out: &mut String) {
    append_row(grid, line, cols, out, &mut |_| {});
}

/// The viewport's text as assistive technology reads it: rows joined by
/// `\n`, trailing blank rows dropped, each row right-trimmed except that the
/// row holding a visible cursor keeps the blanks before it.
///
/// A screen reader echoes typing by matching each text insertion to the key
/// just pressed. With every row trimmed, a typed space changed nothing until
/// the next character arrived with it (`" e"`), so the reader took both for
/// program output and read fragments aloud. Keeping the cursor row's blanks
/// makes a typed space its own one-character insertion, as other Linux
/// terminals report it. A hidden cursor (full-screen programs) or one
/// scrolled out of view leaves every row trimmed.
pub fn accessible_viewport_text(grid: &Grid<Cell>, cursor_visible: bool) -> String {
    let cols = grid.columns();
    let rows = grid.screen_lines() as i32;
    let offset = grid.display_offset() as i32;
    let cursor = grid.cursor.point;
    let cursor_row =
        Some(cursor.line.0 + offset).filter(|row| cursor_visible && (0..rows).contains(row));
    let mut text = String::with_capacity(rows.max(0) as usize * (cols + 1));
    let mut columns = Vec::with_capacity(cols);
    // Bytes through the last row that has anything to read.
    let mut kept = 0;
    for row in 0..rows {
        if row > 0 {
            text.push('\n');
        }
        let start = text.len();
        columns.clear();
        append_row(grid, row - offset, cols, &mut text, &mut |c| {
            columns.push(c)
        });
        let trimmed = start + text[start..].trim_end().len();
        let before_cursor = if cursor_row == Some(row) {
            start
                + columns
                    .iter()
                    .position(|&column| column >= cursor.column.0)
                    .unwrap_or(columns.len())
        } else {
            start
        };
        text.truncate(trimmed.max(before_cursor));
        if text.len() > start {
            kept = text.len();
        }
    }
    text.truncate(kept);
    text
}

/// Right-trimmed grid rows in [first, end), joined within a UTF-8 byte cap.
/// Returns a retained prefix and whether any non-trailing content was omitted.
pub fn rows_text_bounded(grid: &Grid<Cell>, first: i32, end: i32, cap: usize) -> (String, bool) {
    let mut out = String::with_capacity(cap);
    for row in first..end {
        if row != first {
            if out.len() == cap {
                return (out, true);
            }
            out.push('\n');
        }
        let mut trimmed_end = out.len();
        let mut overflow = false;
        for column in 0..grid.columns() {
            let cell = &grid[Point::new(Line(row), Column(column))];
            if is_spacer(cell.flags) {
                continue;
            }
            let (base, marks) =
                crate::inline_text::text_parts(cell.c, cell.zerowidth().unwrap_or_default());
            for ch in std::iter::once(base).chain(marks.iter().copied()) {
                if !overflow && ch.len_utf8() <= cap - out.len() {
                    out.push(ch);
                    if !ch.is_whitespace() {
                        trimmed_end = out.len();
                    }
                } else if !ch.is_whitespace() {
                    return (out, true);
                } else {
                    // Whitespace beyond the cap can still be trimmed at row end.
                    overflow = true;
                }
            }
        }
        out.truncate(trimmed_end);
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each typed character, a space included, reaches a screen reader as
    /// its own insertion at the end of the cursor's row; rows the cursor
    /// left, a hidden cursor and a cursor scrolled out of view keep the
    /// trimmed text.
    #[test]
    fn accessible_text_keeps_the_blanks_typed_before_the_cursor() {
        use alacritty_terminal::event::VoidListener;
        use alacritty_terminal::grid::Scroll;
        use alacritty_terminal::term::{Config, Term, test::TermSize};
        use alacritty_terminal::vte::ansi::Processor;
        let mut term = Term::new(Config::default(), &TermSize::new(20, 4), VoidListener);
        let mut parser: Processor = Processor::new();
        let mut feed =
            |term: &mut Term<VoidListener>, bytes: &str| parser.advance(term, bytes.as_bytes());
        let text = |term: &Term<VoidListener>| accessible_viewport_text(term.grid(), true);

        assert_eq!(text(&term), "", "an empty screen");
        feed(&mut term, "$ ");
        assert_eq!(text(&term), "$ ", "the prompt's trailing blank");
        let mut heard = text(&term);
        for key in ["e", "c", "h", "o", " ", "h", "i", " "] {
            feed(&mut term, key);
            let now = text(&term);
            assert_eq!(
                now.strip_prefix(heard.as_str()),
                Some(key),
                "typing {key:?} inserts exactly it after {heard:?}"
            );
            heard = now;
        }

        feed(&mut term, "\r\nhi\r\n$ ");
        assert_eq!(
            text(&term),
            "$ echo hi\nhi\n$ ",
            "rows the cursor left are trimmed"
        );
        assert_eq!(
            accessible_viewport_text(term.grid(), false),
            "$ echo hi\nhi\n$",
            "a hidden cursor keeps no blanks"
        );

        feed(&mut term, "\r\n");
        assert_eq!(
            text(&term),
            "$ echo hi\nhi\n$",
            "a blank cursor row at column 0 adds nothing"
        );

        feed(&mut term, "世 ");
        assert_eq!(
            text(&term),
            "$ echo hi\nhi\n$\n世 ",
            "a wide glyph's spacer is no blank"
        );

        feed(&mut term, "\r\n1\r\n2\r\n$ ");
        assert_eq!(text(&term), "世\n1\n2\n$ ");
        term.scroll_display(Scroll::Delta(1));
        assert_eq!(
            text(&term),
            "$\n世\n1\n2",
            "a cursor scrolled out of view keeps no blanks"
        );
    }

    /// A screen reader hears output once, as the cursor leaves it: never the
    /// prompt row it is on, never what was typed there before Enter, and a
    /// count rather than the text of a flood.
    #[test]
    fn new_output_is_what_the_cursor_left_since_last_time() {
        use alacritty_terminal::event::VoidListener;
        use alacritty_terminal::term::{Config, Term, test::TermSize};
        use alacritty_terminal::vte::ansi::Processor;
        let mut term = Term::new(Config::default(), &TermSize::new(30, 5), VoidListener);
        let mut parser: Processor = Processor::new();
        let mut feed =
            |term: &mut Term<VoidListener>, bytes: &str| parser.advance(term, bytes.as_bytes());

        feed(&mut term, "$ ");
        let (seen, output) = new_output(term.grid(), None, None);
        assert_eq!(output, NewOutput::Lines(vec![]));
        feed(&mut term, "echo hi");
        let (seen, output) = new_output(term.grid(), Some(seen), None);
        assert_eq!(output, NewOutput::Lines(vec![]), "typing on the prompt row");
        feed(&mut term, "\r\nhi\r\nthere\r\n$ ");
        let (seen, output) = new_output(term.grid(), Some(seen), None);
        assert_eq!(
            output,
            NewOutput::Lines(vec!["hi".into(), "there".into()]),
            "the output, not the command typed before Enter"
        );
        let (seen, output) = new_output(term.grid(), Some(seen), None);
        assert_eq!(output, NewOutput::Lines(vec![]), "heard once");

        // Scrolling past the screen keeps counting rows.
        feed(&mut term, "\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n$ ");
        let (seen, output) = new_output(term.grid(), Some(seen), None);
        assert_eq!(
            output,
            NewOutput::Lines((1..=6).map(|n| n.to_string()).collect())
        );

        // Clearing moves the cursor up: nothing new.
        feed(&mut term, "\x1b[H\x1b[2J$ ");
        let (seen, output) = new_output(term.grid(), Some(seen), None);
        assert_eq!(output, NewOutput::Lines(vec![]));

        // A command entered on a row that its last update caught half-typed:

        // the rest of that row is input, not output.

        feed(&mut term, "echo par");

        let (seen, _) = new_output(term.grid(), Some(seen), None);

        feed(&mut term, "tial");

        let input = cursor_row(term.grid());

        feed(&mut term, "\r\npartial\r\n$ ");

        let (seen, output) = new_output(term.grid(), Some(seen), Some(input));

        assert_eq!(output, NewOutput::Lines(vec!["partial".into()]));

        let flood: String = (0..300).map(|n| format!("{n}\r\n")).collect();
        feed(&mut term, &flood);
        let (_, output) = new_output(term.grid(), Some(seen), None);
        assert_eq!(output, NewOutput::Many(300));
    }

    #[test]
    fn bounded_rows_stop_retaining_combining_text_at_the_byte_cap() {
        let mut grid: Grid<Cell> = Grid::new(200, 80, 0);
        for row in 0..200 {
            for column in 0..80 {
                let cell = &mut grid[Point::new(Line(row), Column(column))];
                cell.c = 'e';
                for _ in 0..30 {
                    cell.push_zerowidth('\u{301}');
                }
            }
        }
        let (output, truncated) = rows_text_bounded(&grid, 0, 200, 1024);
        assert!(truncated);
        assert!(output.len() <= 1024);
        assert!(
            output.capacity() <= 1024,
            "capture must not retain a full snapshot"
        );
        let mut first_row = String::new();
        append_row_text(&grid, 0, 80, &mut first_row);
        assert!(first_row.starts_with(&output));
        assert!(output.len() >= 1022);
    }

    #[test]
    fn bounded_rows_preserve_trimming_spacers_and_requested_range() {
        let mut grid: Grid<Cell> = Grid::new(3, 8, 0);
        grid[Point::new(Line(0), Column(0))].c = 'X';
        let cell = &mut grid[Point::new(Line(1), Column(0))];
        cell.c = '\u{754c}';
        cell.flags.insert(Flags::WIDE_CHAR);
        cell.push_zerowidth('\u{301}');
        grid[Point::new(Line(1), Column(1))]
            .flags
            .insert(Flags::WIDE_CHAR_SPACER);
        grid[Point::new(Line(1), Column(2))].c = 'y';
        assert_eq!(
            rows_text_bounded(&grid, 1, 3, 7),
            ("\u{754c}\u{301}y\n".into(), false)
        );
        assert_eq!(
            rows_text_bounded(&grid, 1, 2, 6),
            ("\u{754c}\u{301}y".into(), false)
        );
        assert_eq!(rows_text_bounded(&grid, 1, 2, 4), ("\u{754c}".into(), true));
    }

    /// A logical line joins rows only across soft wraps, only while the next
    /// row is the grid line below, and never more than `MAX_LOGICAL_ROWS`.
    #[test]
    fn logical_lines_join_soft_wraps_within_bounds() {
        let rows = MAX_LOGICAL_ROWS + 4;
        let mut grid: Grid<Cell> = Grid::new(rows, 2, 0);
        for line in 0..rows {
            grid[Point::new(Line(line as i32), Column(0))].c = 'a';
            grid[Point::new(Line(line as i32), Column(1))].c = 'b';
            grid[Point::new(Line(line as i32), Column(1))]
                .flags
                .insert(Flags::WRAPLINE);
        }
        let lines: Vec<i32> = (0..rows as i32).collect();
        let (mut text, mut pos) = (String::new(), Vec::new());
        let read = logical_line_into(&grid, &lines, 0, 2, &mut text, &mut pos);
        assert_eq!(read.next, MAX_LOGICAL_ROWS, "bounded");
        assert_eq!(text.len(), MAX_LOGICAL_ROWS * 2);
        assert_eq!(pos[2], (1, 0), "the third byte is row 1, column 0");
        // The bound cut it: it goes on below, and the next read starts mid-line.
        assert!(!read.cut_before && read.cut_after);
        let rest = logical_line_into(&grid, &lines, read.next, 2, &mut text, &mut pos);
        assert!(rest.cut_before);

        // A gap in the visible lines stops the join, as does a hard break.
        let gapped = [0, 1, 5];
        let read = logical_line_into(&grid, &gapped, 1, 2, &mut text, &mut pos);
        assert_eq!(read.next, 2);
        grid[Point::new(Line(0), Column(1))]
            .flags
            .remove(Flags::WRAPLINE);
        let read = logical_line_into(&grid, &lines, 0, 2, &mut text, &mut pos);
        assert_eq!(read.next, 1);
        assert_eq!(text, "ab");
        assert!(!read.cut_after, "a hard break ends the line");
    }

    #[test]
    fn skips_wide_char_spacers_and_maps_columns() {
        // "世界": each wide glyph sits in an even column with a literal-space
        // spacer in the odd column after it.
        let mut grid: Grid<Cell> = Grid::new(1, 4, 0);
        let mut set = |col: usize, ch: char, flag: Flags| {
            let cell = &mut grid[Point::new(Line(0), Column(col))];
            cell.c = ch;
            cell.flags.insert(flag);
        };
        set(0, '世', Flags::WIDE_CHAR);
        set(1, ' ', Flags::WIDE_CHAR_SPACER);
        set(2, '界', Flags::WIDE_CHAR);
        set(3, ' ', Flags::WIDE_CHAR_SPACER);

        let mut text = String::new();
        let mut col_of_byte = Vec::new();
        row_text_into(&grid, 0, 4, &mut text, &mut col_of_byte);
        // No spurious space — the two wide glyphs are adjacent so a search for
        // "世界" matches (the bug was reconstructing "世 界").
        assert_eq!(text, "世界");
        // Each glyph is 3 UTF-8 bytes; map back to its own column (0 and 2).
        assert_eq!(col_of_byte, vec![0, 0, 0, 2, 2, 2]);

        let mut out = String::from("> ");
        append_row_text(&grid, 0, 4, &mut out);
        assert_eq!(out, "> 世界");
    }

    #[test]
    fn preserves_combining_marks_and_maps_them_to_base_column() {
        // A decomposed "é": base 'e' in column 0 carrying the combining acute
        // accent U+0301 as a zero-width mark, then a plain 'x' in column 1.
        // Search and scrape must keep the accent rather than see a bare "ex".
        let mut grid: Grid<Cell> = Grid::new(1, 4, 0);
        {
            let base = &mut grid[Point::new(Line(0), Column(0))];
            base.c = 'e';
            base.push_zerowidth('\u{0301}'); // COMBINING ACUTE ACCENT
        }
        grid[Point::new(Line(0), Column(1))].c = 'x';

        let mut text = String::new();
        let mut col_of_byte = Vec::new();
        row_text_into(&grid, 0, 4, &mut text, &mut col_of_byte);

        // The combining mark survives, immediately after its base char.
        assert_eq!(text, "e\u{0301}x  ");
        // 'e' = 1 byte @ col 0; U+0301 = 2 bytes @ col 0 (SAME column as base);
        // 'x' = 1 byte @ col 1; then two trailing spaces @ cols 2,3.
        assert_eq!(col_of_byte, vec![0, 0, 0, 1, 2, 3]);
        // The byte offset of the combining mark maps back to the base column.
        let mark_byte = "e".len();
        assert_eq!(col_of_byte[mark_byte], 0);

        // append_row_text carries the mark too (scrape path).
        let mut out = String::new();
        append_row_text(&grid, 0, 4, &mut out);
        assert_eq!(out, "e\u{0301}x  ");
    }

    #[test]
    fn combining_mark_on_wide_glyph_maps_to_glyph_column() {
        // A wide glyph carrying a variation selector / combining mark: the mark
        // maps to the WIDE_CHAR column, and the spacer is still skipped.
        let mut grid: Grid<Cell> = Grid::new(1, 2, 0);
        {
            let base = &mut grid[Point::new(Line(0), Column(0))];
            base.c = '世';
            base.flags.insert(Flags::WIDE_CHAR);
            base.push_zerowidth('\u{0301}');
        }
        {
            let spacer = &mut grid[Point::new(Line(0), Column(1))];
            spacer.c = ' ';
            spacer.flags.insert(Flags::WIDE_CHAR_SPACER);
        }

        let mut text = String::new();
        let mut col_of_byte = Vec::new();
        row_text_into(&grid, 0, 2, &mut text, &mut col_of_byte);
        assert_eq!(text, "世\u{0301}");
        // '世' = 3 bytes @ col 0; U+0301 = 2 bytes also @ col 0; spacer skipped.
        assert_eq!(col_of_byte, vec![0, 0, 0, 0, 0]);
    }
    #[test]
    fn long_card_cells_are_spaces_in_row_scrape_and_logical_text() {
        let mut grid: Grid<Cell> = Grid::new(1, 3, 0);
        grid[Point::new(Line(0), Column(0))].c = 'a';
        grid[Point::new(Line(0), Column(2))].c = 'b';
        let marker = &mut grid[Point::new(Line(0), Column(1))];
        marker.c = kettle_vt::placeholder::PLACEHOLDER;
        for _ in 0..8 {
            marker.push_zerowidth('\u{0305}');
        }
        let mut text = String::new();
        let mut columns = Vec::new();
        row_text_into(&grid, 0, 3, &mut text, &mut columns);
        assert_eq!(text, "a b");
        assert_eq!(columns, [0, 1, 2]);
        let mut scrape = String::new();
        append_row_text(&grid, 0, 3, &mut scrape);
        assert_eq!(scrape, text);
        let mut positions = Vec::new();
        logical_line_into(&grid, &[0], 0, 3, &mut text, &mut positions);
        assert_eq!(text, "a b");
        assert_eq!(positions, [(0, 0), (0, 1), (0, 2)]);
    }

    #[test]
    fn four_mark_kitty_placeholder_keeps_text_and_full_byte_mapping() {
        let mut grid: Grid<Cell> = Grid::new(1, 1, 0);
        let cell = &mut grid[Point::new(Line(0), Column(0))];
        cell.c = kettle_vt::placeholder::PLACEHOLDER;
        for _ in 0..4 {
            cell.push_zerowidth('\u{0305}');
        }
        let expected: String = std::iter::once(kettle_vt::placeholder::PLACEHOLDER)
            .chain(std::iter::repeat_n('\u{0305}', 4))
            .collect();
        let mut text = String::new();
        let mut columns = Vec::new();
        row_text_into(&grid, 0, 1, &mut text, &mut columns);
        assert_eq!(text, expected);
        assert_eq!(columns, vec![0; expected.len()]);
    }
}
