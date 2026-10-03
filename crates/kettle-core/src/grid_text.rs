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
        let ch = cell.c;
        for _ in 0..ch.len_utf8() {
            byte_from(c);
        }
        text.push(ch);
        // Append this cell's combining marks, each mapped back to the base
        // cell's column `c` so downstream offset→column translation is exact.
        if let Some(marks) = cell.zerowidth() {
            for &mark in marks {
                for _ in 0..mark.len_utf8() {
                    byte_from(c);
                }
                text.push(mark);
            }
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
    for c in 0..cols {
        let cell = &grid[Point::new(Line(line), Column(c))];
        if is_spacer(cell.flags) {
            continue;
        }
        out.push(cell.c);
        if let Some(marks) = cell.zerowidth() {
            out.extend(marks.iter().copied());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
