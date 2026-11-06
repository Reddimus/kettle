use super::*;
use crate::event::VoidListener;
use crate::term::test::TermSize;
use crate::vte::ansi::Processor;

const WIDENING: &[&str] = &[
    "\u{915}\u{93e}",
    "\u{b95}\u{bbe}",
    "\u{c95}\u{cbe}",
    "\u{d9a}\u{dca}\u{200d}\u{dbb}\u{dcf}",
    "\u{915}\u{94d}\u{915}",
    "\u{915}\u{94d}\u{915}\u{93e}",
    "\u{915}\u{94d}\u{924}\u{941}\u{902}",
    "\u{e01}\u{e33}",
    "\u{e81}\u{eb3}",
];

fn terminal() -> (Term<VoidListener>, Processor) {
    let config = Config {
        grapheme_clustering: true,
        ..Config::default()
    };
    (
        Term::new(config, &TermSize::new(8, 3), VoidListener),
        Processor::new(),
    )
}

fn assert_cluster(term: &Term<VoidListener>, line: i32, col: usize, text: &str, width: usize) {
    let cell = &term.grid[Line(line)][Column(col)];
    let stored: String = std::iter::once(cell.c)
        .chain(cell.zerowidth().unwrap_or_default().iter().copied())
        .collect();
    assert_eq!(stored, text);
    assert!(cell.flags.contains(Flags::GRAPHEME));
    assert_eq!(cell.flags.contains(Flags::WIDE_CHAR), width == 2);
    assert!(!cell.flags.contains(Flags::WIDE_CHAR_SPACER));
    let spacer = &term.grid[Line(line)][Column(col + 1)];
    assert_eq!(spacer.flags.contains(Flags::WIDE_CHAR_SPACER), width == 2);
    if width == 2 {
        assert_eq!(spacer.c, ' ');
    }
}

#[test]
fn grapheme_script_widths_follow_every_utf8_split() {
    for &text in WIDENING {
        for split in 0..=text.len() {
            let (mut term, mut parser) = terminal();
            parser.advance(&mut term, &text.as_bytes()[..split]);
            parser.advance(&mut term, &text.as_bytes()[split..]);
            assert_eq!(
                term.grid.cursor.point.column,
                Column(2),
                "{text:?} split {split}"
            );
            assert_cluster(&term, 0, 0, text, 2);
        }
    }
}

#[test]
fn grapheme_script_widening_wraps_at_screen_and_decslrm_edges() {
    for margins in [false, true] {
        for insert in [false, true] {
            for &text in WIDENING {
                let (mut term, mut parser) = terminal();
                parser.advance(&mut term, b"abcdefgh");
                let (edge, left) = if margins {
                    parser.advance(&mut term, b"\x1b[?69h\x1b[3;6s\x1b[1;6H");
                    (5, 2)
                } else {
                    parser.advance(&mut term, b"\x1b[1;8H");
                    (7, 0)
                };
                if insert {
                    parser.advance(&mut term, b"\x1b[4h");
                }
                parser.advance(&mut term, text.as_bytes());
                assert_eq!(
                    term.grid.cursor.point,
                    Point::new(Line(1), Column(left + 2)),
                    "{text:?}"
                );
                assert_cluster(&term, 1, left, text, 2);
                let placeholder = &term.grid[Line(0)][Column(edge)];
                assert_eq!(placeholder.c, ' ');
                assert_eq!(
                    placeholder.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER),
                    !margins
                );
                assert_eq!(placeholder.flags.contains(Flags::PARTIAL_WRAPLINE), margins);
                assert_eq!(term.grid[Line(0)][Column(0)].c, 'a');
                if margins {
                    assert_eq!(term.grid[Line(0)][Column(6)].c, 'g');
                }
                assert!(!term.grid.cursor.input_needs_wrap);
                assert_eq!(term.mode.contains(TermMode::INSERT), insert);
            }
        }
    }
}

#[test]
fn grapheme_script_widening_clips_without_fabricating_vs16() {
    for margins in [false, true] {
        for insert in [false, true] {
            for &text in WIDENING {
                let (mut term, mut parser) = terminal();
                parser.advance(&mut term, b"abcdefgh\x1b[?7l");
                let edge = if margins {
                    parser.advance(&mut term, b"\x1b[?69h\x1b[3;6s\x1b[1;6H");
                    5
                } else {
                    parser.advance(&mut term, b"\x1b[1;8H");
                    7
                };
                if insert {
                    parser.advance(&mut term, b"\x1b[4h");
                }
                parser.advance(&mut term, text.as_bytes());
                assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(edge)));
                let cell = &term.grid[Line(0)][Column(edge)];
                let stored: String = std::iter::once(cell.c)
                    .chain(cell.zerowidth().unwrap_or_default().iter().copied())
                    .collect();
                assert_eq!(stored, text);
                assert!(!cell.flags.contains(Flags::WIDE_CHAR));
                assert!(term.grid.cursor.input_needs_wrap);
                assert_eq!(term.mode.contains(TermMode::INSERT), insert);
                if margins {
                    assert_eq!(term.grid[Line(0)][Column(6)].c, 'g');
                }
            }
        }
    }
}

#[test]
fn grapheme_script_widening_inserts_only_its_extra_spacer() {
    for margins in [false, true] {
        for &text in WIDENING {
            let (mut term, mut parser) = terminal();
            parser.advance(&mut term, b"abcdefgh");
            if margins {
                parser.advance(&mut term, b"\x1b[?69h\x1b[2;7s");
            }
            parser.advance(&mut term, b"\x1b[1;2H\x1b[4h");
            parser.advance(&mut term, text.as_bytes());
            assert_cluster(&term, 0, 1, text, 2);
            assert_eq!(term.grid.cursor.point.column, Column(3));
            let suffix: String = (3..if margins { 7 } else { 8 })
                .map(|col| term.grid[Line(0)][Column(col)].c)
                .collect();
            assert_eq!(suffix, if margins { "bcde" } else { "bcdef" });
            if margins {
                assert_eq!(term.grid[Line(0)][Column(7)].c, 'h');
            }
        }
    }
}

#[test]
fn grapheme_vs15_narrows_and_clears_spacer_even_at_pending_wrap() {
    for margins in [false, true] {
        for wrap in [false, true] {
            for insert in [false, true] {
                for at_edge in [false, true] {
                    let (mut term, mut parser) = terminal();
                    if margins {
                        parser.advance(&mut term, b"\x1b[?69h\x1b[3;6s");
                    }
                    if !wrap {
                        parser.advance(&mut term, b"\x1b[?7l");
                    }
                    let col = if at_edge {
                        if margins { 4 } else { 6 }
                    } else {
                        2
                    };
                    parser.advance(&mut term, format!("\x1b[1;{}H", col + 1).as_bytes());
                    if insert {
                        parser.advance(&mut term, b"\x1b[4h");
                    }
                    parser.advance(&mut term, "\u{231a}".as_bytes());
                    assert_eq!(term.grid.cursor.input_needs_wrap, at_edge);
                    parser.advance(&mut term, "\u{fe0e}".as_bytes());
                    assert_cluster(&term, 0, col, "\u{231a}\u{fe0e}", 1);
                    assert_eq!(term.grid[Line(0)][Column(col + 1)].c, ' ');
                    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(col + 1)));
                    assert!(!term.grid.cursor.input_needs_wrap);
                    assert_eq!(term.mode.contains(TermMode::INSERT), insert);
                    parser.advance(&mut term, b"X");
                    assert_eq!(term.grid[Line(0)][Column(col + 1)].c, 'X');
                }
            }
        }
    }
}

#[test]
fn grapheme_vs15_narrowing_undoes_only_extra_inserted_cell() {
    for margins in [false, true] {
        for wrap in [false, true] {
            let (mut term, mut parser) = terminal();
            parser.advance(&mut term, b"abcdefgh");
            if margins {
                parser.advance(&mut term, b"\x1b[?69h\x1b[2;7s");
            }
            if !wrap {
                parser.advance(&mut term, b"\x1b[?7l");
            }
            parser.advance(&mut term, b"\x1b[1;2H\x1b[4h");
            parser.advance(&mut term, "\u{231a}\u{fe0e}\u{301}".as_bytes());
            assert_cluster(&term, 0, 1, "\u{231a}\u{fe0e}\u{301}", 1);
            assert_eq!(term.grid.cursor.point.column, Column(2));
            assert_eq!(term.grid[Line(0)][Column(2)].c, 'b');
            assert_eq!(term.grid[Line(0)][Column(5)].c, 'e');
            assert!(term.mode.contains(TermMode::INSERT));
            if margins {
                assert_eq!(term.grid[Line(0)][Column(7)].c, 'h');
            }
        }
    }
}

#[test]
fn grapheme_standalone_modifiers_start_a_new_cluster() {
    for modifier in '\u{1f3fb}'..='\u{1f3ff}' {
        for prefix in [" ", "A", "e\u{301}"] {
            let (mut term, mut parser) = terminal();
            parser.advance(&mut term, format!("{prefix}{modifier}\u{301}X").as_bytes());
            assert_cluster(&term, 0, 1, &format!("{modifier}\u{301}"), 2);
            assert_eq!(term.grid.cursor.point.column, Column(4));
            assert_eq!(term.grid[Line(0)][Column(3)].c, 'X');
        }
    }
}

#[test]
fn grapheme_modifier_boundary_wraps_and_inserts_as_a_wide_base() {
    for margins in [false, true] {
        for wrap in [false, true] {
            for insert in [false, true] {
                let (mut term, mut parser) = terminal();
                parser.advance(&mut term, b"abcdefgh");
                let left = if margins {
                    parser.advance(&mut term, b"\x1b[?69h\x1b[3;6s\x1b[1;5H");
                    2
                } else {
                    parser.advance(&mut term, b"\x1b[1;7H");
                    0
                };
                if insert {
                    parser.advance(&mut term, b"\x1b[4h");
                }
                if !wrap {
                    parser.advance(&mut term, b"\x1b[?7l");
                }
                parser.advance(&mut term, " \u{1f3fb}".as_bytes());
                if !wrap {
                    let column = if margins { 4 } else { 6 };
                    assert_eq!(
                        term.grid.cursor.point,
                        Point::new(Line(0), Column(column + 1))
                    );
                    assert!(
                        term.grid[Line(0)][Column(column)]
                            .zerowidth()
                            .unwrap_or_default()
                            .is_empty()
                    );
                    continue;
                }
                assert_cluster(&term, 1, left, "\u{1f3fb}", 2);
                assert_eq!(
                    term.grid.cursor.point,
                    Point::new(Line(1), Column(left + 2))
                );
            }
        }
    }
}

#[test]
fn grapheme_width_changes_survive_the_retained_scalar_cap() {
    for text in [
        "\u{915}\u{94d}\u{915}",
        "\u{915}\u{93e}",
        "\u{e01}\u{e33}",
        "\u{231a}\u{fe0e}",
    ] {
        let (mut term, mut parser) = terminal();
        let mut chars = text.chars();
        let base = chars.next().unwrap();
        parser.advance(&mut term, base.to_string().as_bytes());
        // VS15 must immediately follow its presentation base; the other width
        // changes also work after more marks than the cell can retain.
        if base != '\u{231a}' {
            parser.advance(&mut term, "\u{301}".repeat(100).as_bytes());
        }
        parser.advance(&mut term, chars.collect::<String>().as_bytes());
        let expected = if base == '\u{231a}' { 1 } else { 2 };
        assert_eq!(term.grid.cursor.point.column, Column(expected));
        assert_eq!(
            term.grid[Line(0)][Column(0)]
                .flags
                .contains(Flags::WIDE_CHAR),
            expected == 2
        );
    }
}

#[test]
fn grapheme_width_rules_and_scalar_opt_out_stay_distinct() {
    for (text, clustered, scalar) in [
        ("\u{915}\u{93e}", 2, 2),
        ("\u{915}\u{94d}\u{915}", 2, 2),
        ("\u{231a}\u{fe0e}", 1, 2),
        (" \u{1f3fb}", 3, 3),
        ("\u{1f44d}\u{1f3fb}", 2, 4),
        ("\u{1f469}\u{200d}\u{1f4bb}", 2, 4),
        ("\u{1f1fa}\u{1f1f8}", 2, 2),
    ] {
        for (enabled, expected) in [(true, clustered), (false, scalar)] {
            let (mut term, mut parser) = terminal();
            if !enabled {
                parser.advance(&mut term, b"\x1b[?2027l");
            }
            parser.advance(&mut term, text.as_bytes());
            assert_eq!(
                term.grid.cursor.point.column,
                Column(expected),
                "{text:?} mode {enabled}"
            );
        }
    }
}
