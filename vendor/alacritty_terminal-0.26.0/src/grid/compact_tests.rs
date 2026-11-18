//! Compact history rows: each test runs the same input through a terminal
//! that compacts its history and one that keeps every row at full width
//! (`COMPACT_ROWS` off), checks that rows really were compacted, and then
//! that the two terminals hold the same cells and give the same answers.

use super::row::COMPACT_ROWS;
use super::*;

use crate::event::VoidListener;
use crate::index::{Direction, Side};
use crate::selection::{Selection, SelectionType};
use crate::term::cell::Cell;
use crate::term::search::RegexSearch;
use crate::term::test::TermSize;
use crate::term::{Config, Term};
use crate::vte::ansi::{self, Color, NamedColor};

/// A terminal that compacts its history and one that does not, fed alike.
struct Pair {
    compact: Term<VoidListener>,
    full: Term<VoidListener>,
    compact_parser: ansi::Processor,
    full_parser: ansi::Processor,
}

impl Pair {
    fn new(columns: usize, lines: usize, history: usize) -> Self {
        let config = || Config {
            scrolling_history: history,
            ..Config::default()
        };
        let size = TermSize::new(columns, lines);
        let compact = Term::new(config(), &size, VoidListener);
        let full = with_full_rows(|| Term::new(config(), &size, VoidListener));
        Self {
            compact,
            full,
            compact_parser: ansi::Processor::new(),
            full_parser: ansi::Processor::new(),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.compact_parser.advance(&mut self.compact, bytes);
        let (full, parser) = (&mut self.full, &mut self.full_parser);
        with_full_rows(|| parser.advance(full, bytes));
    }

    fn resize(&mut self, columns: usize, lines: usize) {
        self.compact.resize(TermSize::new(columns, lines));
        let full = &mut self.full;
        with_full_rows(|| full.resize(TermSize::new(columns, lines)));
    }

    /// Every cell, the geometry and the cursor agree.
    #[track_caller]
    fn assert_same(&self, case: &str) {
        let (a, b) = (self.compact.grid(), self.full.grid());
        assert_eq!(a.columns(), b.columns(), "{case}: columns");
        assert_eq!(a.screen_lines(), b.screen_lines(), "{case}: lines");
        assert_eq!(a.history_size(), b.history_size(), "{case}: history");
        assert_eq!(a.display_offset(), b.display_offset(), "{case}: offset");
        assert_eq!(a.cursor.point, b.cursor.point, "{case}: cursor");
        assert_eq!(
            a.cursor.input_needs_wrap, b.cursor.input_needs_wrap,
            "{case}: pending wrap"
        );
        for line in (a.topmost_line().0..a.screen_lines() as i32).map(Line::from) {
            assert_eq!(a[line].len(), b[line].len(), "{case}: width of {line:?}");
            for column in (0..a.columns()).map(Column::from) {
                assert_eq!(
                    a[line][column], b[line][column],
                    "{case}: cell {line:?} {column:?}"
                );
            }
        }
    }

    fn compact_rows(&self) -> usize {
        compact_rows(self.compact.grid())
    }
}

fn with_full_rows<R>(f: impl FnOnce() -> R) -> R {
    COMPACT_ROWS.with(|on| on.set(false));
    let result = f();
    COMPACT_ROWS.with(|on| on.set(true));
    result
}

fn compact_rows(grid: &Grid<Cell>) -> usize {
    (grid.topmost_line().0..0)
        .filter(|&line| grid[Line(line)].is_compact())
        .count()
}

fn text(grid: &Grid<Cell>, line: i32) -> String {
    grid[Line(line)].into_iter().map(|cell| cell.c).collect()
}

fn seq(from: usize, to: usize) -> Vec<u8> {
    (from..=to)
        .flat_map(|i| format!("{i}\r\n").into_bytes())
        .collect()
}

/// A narrow output in a wide pane keeps only its text in history: the
/// scrollback costs a few cells a row instead of the pane's width.
#[test]
fn history_rows_store_only_their_text() {
    let mut term = Term::new(Config::default(), &TermSize::new(190, 53), VoidListener);
    let mut parser: ansi::Processor = ansi::Processor::new();
    parser.advance(&mut term, &seq(1, 20_000));
    let grid = term.grid();
    assert_eq!(grid.history_size(), 10_000);

    let mut cells = 0;
    for line in grid.topmost_line().0..0 {
        let row = &grid[Line(line)];
        let number = (19_949 + line).to_string();
        assert_eq!(text(grid, line).trim_end(), number, "row {line}");
        assert_eq!(row.len(), 190);
        // The digits, the blank that stands for the rest, and the slack a
        // reused allocation may keep.
        assert!(
            row.capacity() <= number.len() + number.len() / 2 + 8,
            "row {line} allocates {} cells for {number}",
            row.capacity()
        );
        cells += row.capacity();
    }
    assert!(cells <= 10_000 * 12, "history allocates {cells} cells");
    for line in 0..grid.screen_lines() as i32 {
        assert_eq!(grid[Line(line)].capacity(), 190, "screen row {line}");
    }
}

/// Lines of varied length through a full history: each history row keeps
/// an allocation close to its text, and the spares kept for reuse stay few.
#[test]
fn varied_lines_keep_history_and_spares_bounded() {
    let mut term = Term::new(Config::default(), &TermSize::new(190, 10), VoidListener);
    let mut parser: ansi::Processor = ansi::Processor::new();
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut lengths = Vec::new();
    for _ in 0..30_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let len = 1 + (state % 150) as usize;
        lengths.push(len);
        parser.advance(&mut term, format!("{}\r\n", "x".repeat(len)).as_bytes());
    }
    let grid = term.grid();
    assert_eq!(grid.history_size(), 10_000);
    assert!(grid.spare_rows.len() <= super::row::MAX_SPARES);
    // History ends with the 10,000 lines before the nine on screen.
    let expected = &lengths[30_000 - 9 - 10_000..30_000 - 9];
    for (line, &len) in (grid.topmost_line().0..0).zip(expected) {
        let row = &grid[Line(line)];
        assert_eq!(text(grid, line).trim_end().len(), len, "row {line}");
        assert!(
            row.capacity() <= len + len / 2 + 8,
            "row {line} allocates {} cells for {len}",
            row.capacity()
        );
    }
}

/// Random output, controls and resizes give identical terminals whether or
/// not history rows are compact, including selection text and search.
#[test]
fn compact_history_matches_full_rows_through_random_output() {
    const PIECES: &[&str] = &[
        "abc",
        "hello world",
        "x",
        "0123456789",
        "  ",
        "\t",
        "\r\n",
        "\r\n",
        "\r\n",
        "\n",
        "\r",
        "\x08",
        "\x1b[31m",
        "\x1b[42m",
        "\x1b[0m",
        "\x1b[7m",
        "\x1b[4:3m",
        "\x1b[58;5;3m",
        "\x1b[K",
        "\x1b[1K",
        "\x1b[2K",
        "\x1b[44m\x1b[K\x1b[0m",
        "\x1b[45m\r\n\x1b[0m",
        "\u{754c}",
        "e\u{301}",
        "\u{1f44d}",
        "\x1b]8;id=a;https://example.com\x1b\\link\x1b]8;;\x1b\\",
        "\x1b[2J",
        "\x1b[3J",
        "\x1b[H",
        "\x1b[5;3H",
        "\x1b[3S",
        "\x1b[2T",
        "\x1bM",
        "\x1bD",
        "\x1bE",
        "\x1b[2;5r",
        "\x1b[r",
        "\x1b[L",
        "\x1b[M",
        "\x1b[P",
        "\x1b[@",
        "\x1b[3X",
        "\x1b[?1049h",
        "\x1b[?1049l",
        "\x1b#8",
        "\x1b[?69h\x1b[3;12s",
        "\x1b[?69l",
        "\x1b[?7l",
        "\x1b[?7h",
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move |bound: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % bound as u64) as usize
    };

    let mut compacted = 0;
    for case in 0..120 {
        let history = [0, 3, 40][case % 3];
        let mut pair = Pair::new(2 + next(60), 1 + next(12), history);
        for step in 0..60 {
            let mut bytes = String::new();
            for _ in 0..next(40) {
                bytes.push_str(PIECES[next(PIECES.len())]);
                if next(8) == 0 {
                    bytes.push_str(&"long".repeat(next(30)));
                }
            }
            pair.feed(bytes.as_bytes());
            if next(6) == 0 {
                pair.resize(2 + next(60), 1 + next(12));
            }
            if next(10) == 0 {
                let delta = next(8) as i32 - 2;
                pair.compact.scroll_display(Scroll::Delta(delta));
                pair.full.scroll_display(Scroll::Delta(delta));
            }
            compacted += pair.compact_rows();
            let label = format!("case {case} step {step}");
            pair.assert_same(&label);
            assert_same_answers(&mut pair, &label);
        }
    }
    assert!(
        compacted > 1_000,
        "only {compacted} compact rows were checked"
    );
}

/// Selection text and search matches over the whole buffer agree.
#[track_caller]
fn assert_same_answers(pair: &mut Pair, case: &str) {
    let grid = pair.compact.grid();
    let top = Point::new(grid.topmost_line(), Column(0));
    let bottom = Point::new(grid.bottommost_line(), grid.last_column());
    for ty in [SelectionType::Simple, SelectionType::Block] {
        let mut selection = Selection::new(ty, top, Side::Left);
        selection.update(bottom, Side::Right);
        pair.compact.selection = Some(selection.clone());
        pair.full.selection = Some(selection);
        for trim in [false, true] {
            assert_eq!(
                pair.compact.selection_to_string_with_trim(trim),
                pair.full.selection_to_string_with_trim(trim),
                "{case}: {ty:?} selection, trim {trim}"
            );
        }
    }
    pair.compact.selection = None;
    pair.full.selection = None;

    let mut regex = RegexSearch::new("o+ ?w|lon|g\\s+\\S").unwrap();
    for (origin, direction) in [(top, Direction::Right), (bottom, Direction::Left)] {
        assert_eq!(
            pair.compact
                .search_next(&mut regex, origin, direction, Side::Left, None),
            pair.full
                .search_next(&mut regex, origin, direction, Side::Left, None),
            "{case}: search {direction:?}"
        );
    }
}

/// Reflow joins and splits wrapped lines through compact history rows, both
/// ways, and leaves history compact afterwards.
#[test]
fn reflow_through_compact_history_matches_full_rows() {
    let mut pair = Pair::new(20, 4, 100);
    pair.feed(format!("{}\r\nshort\r\n{}\r\n", "w".repeat(50), "tail ".repeat(7)).as_bytes());
    pair.feed(b"a\r\nbb\r\n\x1b[41mred\x1b[0m\r\nccc\r\n\r\n\r\n");
    assert!(pair.compact_rows() >= 4);
    pair.assert_same("before");

    for (columns, lines) in [(9, 4), (7, 6), (33, 3), (60, 8), (20, 4), (3, 2), (20, 4)] {
        pair.resize(columns, lines);
        let case = format!("{columns}x{lines}");
        pair.assert_same(&case);
        assert_same_answers(&mut pair, &case);
        // Every history row that ends in two default cells is compact.
        let grid = pair.compact.grid();
        for line in grid.topmost_line().0..0 {
            let row = &grid[Line(line)];
            let end = row.len() - 1;
            if row[Column(end)] == Cell::default() && row[Column(end - 1)] == Cell::default() {
                assert!(
                    row.is_compact(),
                    "{case}: history row {line} was left at full width"
                );
            }
        }
    }
}

/// Selecting and searching across compact rows sees the text and the
/// blanks that end each row.
#[test]
fn selection_and_search_read_compact_rows() {
    let mut pair = Pair::new(40, 3, 100);
    pair.feed(b"first line\r\nsecond\r\n  third   \r\n\r\nlast\r\n\r\n\r\n");
    let grid = pair.compact.grid();
    assert_eq!(grid.history_size(), 5);
    assert_eq!(pair.compact_rows(), 5);

    let mut selection = Selection::new(
        SelectionType::Simple,
        Point::new(Line(-5), Column(6)),
        Side::Left,
    );
    selection.update(Point::new(Line(-3), Column(39)), Side::Right);
    pair.compact.selection = Some(selection.clone());
    pair.full.selection = Some(selection);
    let text = pair.compact.selection_to_string();
    assert_eq!(text.as_deref(), Some("line\nsecond\n  third"));
    assert_eq!(text, pair.full.selection_to_string());

    let mut block = Selection::new(
        SelectionType::Block,
        Point::new(Line(-5), Column(2)),
        Side::Left,
    );
    block.update(Point::new(Line(-1), Column(30)), Side::Right);
    pair.compact.selection = Some(block.clone());
    pair.full.selection = Some(block);
    assert_eq!(
        pair.compact.selection_to_string(),
        pair.full.selection_to_string()
    );

    let mut regex = RegexSearch::new("third\\s+last").unwrap();
    let origin = Point::new(Line(-5), Column(0));
    let found = pair
        .compact
        .search_next(&mut regex, origin, Direction::Right, Side::Left, None);
    assert_eq!(
        found, None,
        "an empty row breaks the line, so no match spans it"
    );
    let mut regex = RegexSearch::new("ird").unwrap();
    let found = pair
        .compact
        .search_next(&mut regex, origin, Direction::Right, Side::Left, None);
    assert_eq!(
        found,
        Some(Point::new(Line(-3), Column(4))..=Point::new(Line(-3), Column(6)))
    );
    let mut regex = RegexSearch::new("ast").unwrap();
    let bottom = Point::new(Line(2), Column(39));
    let found = pair
        .compact
        .search_next(&mut regex, bottom, Direction::Left, Side::Left, None);
    assert_eq!(
        found,
        Some(Point::new(Line(-1), Column(1))..=Point::new(Line(-1), Column(3)))
    );
    pair.assert_same("search");
}

/// Blank cells with a background, or any attribute, are content: rows keep
/// them in history, whether written or left by a coloured erase or scroll.
#[test]
fn styled_blank_cells_stay_in_history() {
    let mut pair = Pair::new(30, 2, 100);
    // An erase to the end in red; blue text whose line feed scrolls in a
    // blue row; inverse and underlined spaces; then plain rows.
    pair.feed(b"red\x1b[41m\x1b[K\x1b[0m\r\n\x1b[44mblue\r\n\x1b[0m\r\n");
    pair.feed(b"x\x1b[7m   \x1b[0m\x1b[4m  \x1b[0m\r\nplain\r\n\r\n\r\n");
    let grid = pair.compact.grid();
    assert_eq!(grid.history_size(), 6);
    let red = Color::Named(NamedColor::Red);
    let blue = Color::Named(NamedColor::Blue);
    let row = |line| &grid[Line(line)];
    let default = Cell::default();

    assert!(
        (3..30).all(|c| row(-6)[Column(c)].bg == red),
        "red erase lost"
    );
    assert!(
        (0..4).all(|c| row(-5)[Column(c)].bg == blue),
        "blue text lost"
    );
    assert!((4..30).all(|c| row(-5)[Column(c)] == default));
    assert!(
        (0..30).all(|c| row(-4)[Column(c)].bg == blue),
        "blue scroll lost"
    );
    assert!((1..4).all(|c| row(-3)[Column(c)].flags.contains(Flags::INVERSE)));
    assert!((4..6).all(|c| row(-3)[Column(c)].flags.contains(Flags::UNDERLINE)));
    assert!((6..30).all(|c| row(-3)[Column(c)] == default));

    assert!(
        !row(-6).is_compact() && !row(-4).is_compact(),
        "coloured ends were dropped"
    );
    assert!(row(-5).is_compact() && row(-3).is_compact() && row(-2).is_compact());
    assert_eq!(
        row(-3).capacity(),
        7,
        "the styled blanks and one default cell"
    );
    pair.assert_same("styled");

    pair.resize(50, 2);
    pair.resize(12, 3);
    pair.assert_same("resized");
}

/// A wide character that does not fit leaves a spacer in the last column,
/// and one that does fit can end the row: both survive compaction and
/// reflow, as do wrapped rows.
#[test]
fn wide_chars_at_the_edge_survive_compaction() {
    let mut pair = Pair::new(9, 2, 100);
    pair.feed("abcdefgh\u{754c}x\r\nabcdefg\u{754c}\r\nab\u{754c}\r\n\r\n\r\n".as_bytes());
    let grid = pair.compact.grid();
    assert!(
        grid[Line(-5)][Column(8)]
            .flags
            .contains(Flags::LEADING_WIDE_CHAR_SPACER)
    );
    assert!(grid[Line(-5)][Column(8)].flags.contains(Flags::WRAPLINE));
    assert!(grid[Line(-4)][Column(0)].flags.contains(Flags::WIDE_CHAR));
    assert!(grid[Line(-3)][Column(7)].flags.contains(Flags::WIDE_CHAR));
    assert!(
        grid[Line(-3)][Column(8)]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
    );
    assert!(
        grid[Line(-2)][Column(3)]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
    );
    assert!(grid[Line(-4)].is_compact() && grid[Line(-2)].is_compact());
    pair.assert_same("written");

    for (columns, lines) in [(10, 2), (8, 2), (3, 4), (16, 2), (9, 2)] {
        pair.resize(columns, lines);
        pair.assert_same(&format!("{columns}x{lines}"));
    }
}

/// Combining marks and hyperlinks are stored with their cells.
#[test]
fn zero_width_and_hyperlinks_survive_compaction() {
    let mut pair = Pair::new(30, 2, 100);
    pair.feed("e\u{301}\u{302}x\r\n".as_bytes());
    pair.feed(b"\x1b]8;id=k;https://example.com\x1b\\site\x1b]8;;\x1b\\ ok\r\n\r\n\r\n");
    let grid = pair.compact.grid();
    assert_eq!(grid.history_size(), 3);
    assert_eq!(
        grid[Line(-3)][Column(0)].zerowidth(),
        Some(&['\u{301}', '\u{302}'][..])
    );
    let link = grid[Line(-2)][Column(3)].hyperlink().expect("link kept");
    assert_eq!(link.uri(), "https://example.com");
    assert_eq!(grid[Line(-2)][Column(4)].hyperlink(), None);
    assert!(grid[Line(-3)].is_compact() && grid[Line(-2)].is_compact());
    pair.assert_same("extras");
    pair.resize(4, 2);
    pair.resize(30, 2);
    pair.assert_same("resized");
}

/// Scrolls that add history in odd ways: none kept, less kept than
/// scrolled, several lines at once, and a margin above fixed bottom rows.
#[test]
fn unusual_scrolls_compact_only_retained_rows() {
    for history in [0, 1, 2] {
        let mut pair = Pair::new(30, 4, history);
        pair.feed(b"one\r\ntwo\r\nthree\r\nfour\x1b[H\x1b[3S");
        pair.feed(b"\x1b[2J\x1b[Hfive\r\nsix\r\n\x1b[3J\x1b[Hseven\x1b[2J");
        pair.assert_same(&format!("history {history}"));
        assert_eq!(pair.compact_rows(), pair.compact.grid().history_size());
    }

    let mut pair = Pair::new(30, 6, 50);
    // Scroll a top margin over the two fixed bottom rows.
    pair.feed(b"\x1b[5;1Hfixed one\r\nfixed two\x1b[1;4r\x1b[H");
    for i in 0..12 {
        pair.feed(format!("row {i}\x1bD\r").as_bytes());
    }
    pair.feed(b"\x1b[r");
    let grid = pair.compact.grid();
    assert_eq!(grid.history_size(), 9);
    assert_eq!(pair.compact_rows(), 9);
    assert_eq!(text(grid, 4).trim_end(), "fixed one");
    assert_eq!(text(grid, 5).trim_end(), "fixed two");
    pair.assert_same("margin");
}

/// Switching to the alternate screen and back leaves primary history
/// compact and intact.
#[test]
fn alternate_screen_leaves_history_compact() {
    let mut pair = Pair::new(40, 3, 100);
    pair.feed(&seq(1, 30));
    let before = pair.compact_rows();
    assert_eq!(before, 28);
    pair.feed(b"\x1b[?1049halt\r\nscreen\r\n\r\n\r\n\x1b[?1049l");
    pair.resize(41, 3);
    pair.feed(b"\x1b[?1049hagain\x1b[?1049l");
    assert_eq!(pair.compact_rows(), before);
    assert_eq!(text(pair.compact.grid(), -28).trim_end(), "1");
    pair.assert_same("alternate");
}

/// Widening a row whose blank end has a background keeps that background:
/// the new cells are default, so the old ones count as occupied.
#[test]
fn widening_a_coloured_row_keeps_its_colour_when_compacted() {
    let mut row = Row::<Cell>::new(4);
    let template = Cell {
        bg: Color::Named(NamedColor::Red),
        ..Cell::default()
    };
    row.reset(&template);
    row.grow(8);
    row.compact();
    assert!(row.is_compact());
    assert!((0..4).all(|c| row[Column(c)].bg == template.bg));
    assert!((4..8).all(|c| row[Column(c)] == Cell::default()));
}

/// Cells move a run with one copy and refill it with defaults, at every
/// length, and a moved link is moved, not shared.
#[test]
fn cells_take_runs_at_every_length() {
    let mut linked = Cell {
        c: 'l',
        ..Cell::default()
    };
    linked.set_hyperlink(Some(crate::term::cell::Hyperlink::new(
        Some("k"),
        "https://e".into(),
    )));
    let shared = linked.extra.clone().expect("a link is extra storage");
    for len in 0..70 {
        let mut cells: Vec<Cell> = (0..len)
            .map(|i| Cell {
                c: char::from(b'a' + (i % 26) as u8),
                ..Cell::default()
            })
            .collect();
        if len > 0 {
            cells[len / 2] = linked.clone();
        }
        let expected = cells.clone();
        let mut into = vec![linked.clone()];
        Cell::take_cells(&mut cells, &mut into);
        assert_eq!(into[1..], expected[..], "length {len}");
        assert!(
            cells.iter().all(|cell| *cell == Cell::default()),
            "length {len}"
        );
        drop(into);
        drop(expected);
        assert_eq!(std::sync::Arc::strong_count(&shared), 2, "length {len}");
    }
}

/// Emptying cells skips their drops only when none owns extra storage, so
/// links and combining marks are still released.
#[test]
fn clearing_cells_releases_extra_storage() {
    let mut linked = Cell::default();
    linked.set_hyperlink(Some(crate::term::cell::Hyperlink::new(
        Some("k"),
        "https://e".into(),
    )));
    let shared = linked.extra.clone().expect("a link is extra storage");
    let mut cells = vec![Cell::default(); 40];
    cells.extend(std::iter::repeat_n(linked, 3));
    assert_eq!(std::sync::Arc::strong_count(&shared), 4);
    Cell::clear_cells(&mut cells);
    assert!(cells.is_empty());
    assert_eq!(
        std::sync::Arc::strong_count(&shared),
        1,
        "a link was leaked"
    );

    let mut plain = vec![Cell::default(); 40];
    Cell::clear_cells(&mut plain);
    assert!(plain.is_empty());
}

/// Rows serialize their cells at full width, so the format does not change.
#[cfg(feature = "serde")]
#[test]
fn compact_rows_serialize_at_full_width() {
    let mut row = Row::<Cell>::new(6);
    row[Column(0)].c = 'a';
    let full = row.clone();
    row.compact();
    assert!(row.is_compact());
    let json = serde_json::to_string(&row).unwrap();
    assert_eq!(json, serde_json::to_string(&full).unwrap());
    let back: Row<Cell> = serde_json::from_str(&json).unwrap();
    assert!(!back.is_compact());
    assert_eq!(back, full);
    assert_eq!(back, row);
}

/// A row read back trusts none of its `occ`: a row whose coloured cells lie
/// past `occ`, as an older version could leave one, keeps them when it is
/// compacted.
#[cfg(feature = "serde")]
#[test]
fn rows_read_back_keep_cells_past_their_occ() {
    let red = Cell {
        bg: Color::Named(NamedColor::Red),
        ..Cell::default()
    };
    let mut cells = vec![red; 4];
    cells.resize(8, Cell::default());
    let json = serde_json::to_string(&Row::from_vec(cells.clone(), 0)).unwrap();
    let mut row: Row<Cell> = serde_json::from_str(&json).unwrap();
    row.compact();
    assert!(row.is_compact());
    assert!(row.into_iter().eq(&cells));

    // `occ` is still read, so formats without field names stay aligned.
    let without_occ = serde_json::to_string(&serde_json::json!({ "inner": [] })).unwrap();
    assert!(serde_json::from_str::<Row<Cell>>(&without_occ).is_err());
}

/// A row built from cells counts all of them as occupied, so a wrong `occ`
/// cannot make compaction drop a cell.
#[test]
fn rows_from_cells_keep_cells_past_their_occ() {
    let mut cells = vec![Cell::default(); 8];
    cells[2].c = 'x';
    let mut row = Row::from_vec(cells.clone(), 0);
    row.compact();
    assert!(row.is_compact());
    assert!(row.into_iter().eq(&cells));
}

/// A scroll by more than its region clears history rows as well; the rows
/// it pairs for compaction are only those on screen.
#[test]
fn scrolling_past_the_region_compacts_without_pairing_a_row_with_itself() {
    let mut grid = Grid::<Cell>::new(2, 8, 3);
    for _ in 0..3 {
        grid[Line(0)][Column(0)].c = 'a';
        grid.scroll_up::<Color>(&(Line(0)..Line(2)), 1);
    }
    assert_eq!(compact_rows(&grid), 3);
    grid.scroll_up::<Color>(&(Line(0)..Line(2)), 3);
    assert_eq!(grid.history_size(), 3);
    for line in grid.topmost_line().0..2 {
        assert_eq!(grid[Line(line)].len(), 8, "row {line}");
    }
    assert!(!grid[Line(0)].is_compact() && !grid[Line(1)].is_compact());
}

/// Reading or iterating a compact row sees every column; writing to one
/// stores its full width first.
#[test]
fn compact_rows_read_and_write_like_full_rows() {
    let mut row = Row::<Cell>::new(10);
    row[Column(1)].c = 'b';
    let full = row.clone();
    row.compact();
    assert_eq!(row.capacity(), 3);
    assert_eq!(row.len(), 10);
    assert_eq!(row, full);
    assert!(row.into_iter().eq(&full));
    assert!(row.into_iter().rev().eq(full.into_iter().rev()));
    assert_eq!(row.into_iter().len(), 10);
    assert_eq!(row[Column(9)], Cell::default());
    assert_eq!(row.last(), full.last());
    assert!(!row.is_clear());

    row[Column(9)].c = 'z';
    assert!(!row.is_compact());
    assert_eq!(row.capacity(), 10);
    assert_eq!(row[Column(9)].c, 'z');
    assert_eq!(row[Column(1)].c, 'b');
}
