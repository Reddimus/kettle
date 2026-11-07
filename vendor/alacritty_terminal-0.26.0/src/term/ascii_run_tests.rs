//! The ASCII run writer must leave exactly what printing the run one
//! character at a time leaves: cells, cursor and grapheme state.
use super::*;
use crate::event::VoidListener;
use crate::term::test::TermSize;
use crate::vte::ansi::Processor;

const PIECES: &[&str] = &[
    "hello",
    "a",
    " ",
    "0123456789abcdefghijklmnopqrstuvwxyz",
    "\r\n",
    "\n",
    "\r",
    "\t",
    "\x08",
    "\x1b[31m",
    "\x1b[1;4;48;5;17m",
    "\x1b[0m",
    "\x1b[4h",
    "\x1b[4l",
    "\x1b(0",
    "\x1b(B",
    "\x1b[?69h\x1b[3;12s",
    "\x1b[?69l",
    "\x1b[?7l",
    "\x1b[?7h",
    "\x1b[5G",
    "\x1b[2;3H",
    "\x1b[K",
    "\x1b[2@",
    "\x1b]8;id=a;https://x.test\x1b\\",
    "\x1b]8;;\x1b\\",
    "\x1b[58;2;1;2;3m",
    "界",
    "e\u{301}",
    "\u{301}",
    "\u{600}",
    "\u{600}1",
    "👍",
    "\u{2764}\u{fe0f}",
    "#\u{fe0f}\u{20e3}",
    "\u{1f3fb}",
    "\u{915}\u{94d}\u{915}",
    "\x1b[?2027l",
    "\x1b[?2027h",
    "\x1b[3b",
];

fn term(clustering: bool, runs: bool) -> Term<VoidListener> {
    let config = Config {
        grapheme_clustering: clustering,
        ..Config::default()
    };
    let mut term = Term::new(config, &TermSize::new(17, 5), VoidListener);
    term.ascii_runs = runs;
    term
}

fn assert_same(fast: &Term<VoidListener>, slow: &Term<VoidListener>, input: &str) {
    assert_eq!(fast.grid.history_size(), slow.grid.history_size(), "{input:?}");
    let top = -(fast.grid.history_size() as i32);
    for line in top..fast.grid.screen_lines() as i32 {
        for column in 0..fast.grid.columns() {
            let at = (Line(line), Column(column));
            assert_eq!(
                fast.grid[at.0][at.1], slow.grid[at.0][at.1],
                "cell {at:?} after {input:?}"
            );
        }
    }
    assert_eq!(fast.grid.cursor, slow.grid.cursor, "{input:?}");
    assert_eq!(fast.grapheme_state, slow.grapheme_state, "{input:?}");
    assert_eq!(fast.grapheme_width, slow.grapheme_width, "{input:?}");
    assert_eq!(fast.grapheme_anchor, slow.grapheme_anchor, "{input:?}");
    assert_eq!(fast.last_grapheme, slow.last_grapheme, "{input:?}");
    assert_eq!(fast.grapheme_template, slow.grapheme_template, "{input:?}");
}

#[test]
fn ascii_runs_match_character_at_a_time_printing() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for clustering in [true, false] {
        for _ in 0..300 {
            let mut fast = term(clustering, true);
            let mut slow = term(clustering, false);
            let mut fast_parser: Processor = Processor::new();
            let mut slow_parser: Processor = Processor::new();
            let mut input = String::new();
            for _ in 0..(next() % 30) {
                let piece = PIECES[(next() % PIECES.len() as u64) as usize];
                input.push_str(piece);
                fast_parser.advance(&mut fast, piece.as_bytes());
                slow_parser.advance(&mut slow, piece.as_bytes());
                assert_same(&fast, &slow, &input);
            }
            // A mark arriving next joins whatever cluster each one left open.
            fast_parser.advance(&mut fast, "\u{301}".as_bytes());
            slow_parser.advance(&mut slow, "\u{301}".as_bytes());
            assert_same(&fast, &slow, &input);
        }
    }
}

#[test]
fn ascii_grapheme_survives_insertion() {
    for runs in [true, false] {
        let mut term = term(true, runs);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b[?2027h\x1b[4h");
        parser.advance(&mut term, "e\u{301}X".as_bytes());
        let cell = &term.grid[Line(0)][Column(0)];
        assert_eq!(cell.c, 'e');
        assert_eq!(cell.zerowidth(), Some(&['\u{301}'][..]));
        assert!(cell.flags.contains(Flags::GRAPHEME));
        assert_eq!(term.grid[Line(0)][Column(1)].c, 'X');
        assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(2)));
        assert!(!term.grid.cursor.input_needs_wrap);
        assert_eq!(term.grid.history_size(), 0);
        for col in 0..17 {
            assert!(!term.grid[Line(0)][Column(col)].flags.intersects(
                Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER
            ));
        }
    }
}

#[test]
fn ascii_grapheme_survives_bottom_margin_autowrap() {
    for runs in [true, false] {
        let mut term = term(true, runs);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b[?2027h\x1b[5;1H0123456789abcdefg");
        parser.advance(&mut term, "e\u{301}X".as_bytes());
        let cell = &term.grid[Line(4)][Column(0)];
        assert_eq!(cell.c, 'e');
        assert_eq!(cell.zerowidth(), Some(&['\u{301}'][..]));
        assert!(cell.flags.contains(Flags::GRAPHEME));
        assert_eq!(term.grid[Line(4)][Column(1)].c, 'X');
        assert_eq!(term.grid.cursor.point, Point::new(Line(4), Column(2)));
        assert!(!term.grid.cursor.input_needs_wrap);
        assert_eq!(term.grid.history_size(), 1);
        assert!(
            term.grid[Line(3)][Column(16)]
                .flags
                .contains(Flags::WRAPLINE)
        );
        let row: String = (0..17)
            .map(|col| term.grid[Line(3)][Column(col)].c)
            .collect();
        assert_eq!(row, "0123456789abcdefg");
        for col in 0..17 {
            assert!(!term.grid[Line(4)][Column(col)].flags.intersects(
                Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER
            ));
        }
    }
}
