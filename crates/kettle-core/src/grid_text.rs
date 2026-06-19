//! Spacer-aware reconstruction of a grid row's text.
//!
//! A wide (CJK / emoji) glyph occupies two cells: the glyph lives in column `N`
//! (`Flags::WIDE_CHAR`) and a literal space is written into column `N+1`
//! (`Flags::WIDE_CHAR_SPACER`). Naively pushing `cell.c` for every column injects
//! that space, so `世界` reconstructs as `世 界` — and search, link/path
//! detection, quick-select hints, and the agent screen-scrape never match across
//! wide text. This single helper skips the spacer cells while keeping the
//! byte→column map exact, and is shared by every consumer so the fix can't drift
//! (audit, v2.26.0).

use alacritty_terminal::grid::Grid;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};

/// True for a cell that only pads out a wide glyph (the trailing space cell, or
/// the leading spacer used when a wide glyph would straddle the last column).
fn is_spacer(cell: &Cell) -> bool {
    cell.flags
        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
}

/// Rebuild row `line`'s text into `text` (cleared first), pushing to
/// `col_of_byte` the originating grid column for each UTF-8 byte appended — so a
/// regex match offset maps back to the exact column. Wide-char spacer cells are
/// skipped. Callers reuse both buffers across rows to amortize allocation.
pub fn row_text_into(
    grid: &Grid<Cell>,
    line: i32,
    cols: usize,
    text: &mut String,
    col_of_byte: &mut Vec<usize>,
) {
    text.clear();
    col_of_byte.clear();
    for c in 0..cols {
        let cell = &grid[Point::new(Line(line), Column(c))];
        if is_spacer(cell) {
            continue;
        }
        let ch = cell.c;
        for _ in 0..ch.len_utf8() {
            col_of_byte.push(c);
        }
        text.push(ch);
    }
}

/// Like [`row_text_into`] but *appends* only the text (no column map) to `out` —
/// for callers that just need the characters (e.g. the agent screen scrape).
pub fn append_row_text(grid: &Grid<Cell>, line: i32, cols: usize, out: &mut String) {
    for c in 0..cols {
        let cell = &grid[Point::new(Line(line), Column(c))];
        if is_spacer(cell) {
            continue;
        }
        out.push(cell.c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
