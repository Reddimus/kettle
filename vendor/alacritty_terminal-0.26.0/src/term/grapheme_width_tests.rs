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

#[test]
fn grapheme_vs15_narrows_east_asian_wide_variation_bases() {
    for base in ["\u{3030}", "\u{303d}", "\u{3297}", "\u{1f202}"] {
        let (mut term, mut parser) = terminal();
        parser.advance(&mut term, format!("{base}\u{fe0e}X").as_bytes());
        assert_cluster(&term, 0, 0, &format!("{base}\u{fe0e}"), 1);
        assert_eq!(term.grid[Line(0)][Column(1)].c, 'X');
    }
}

#[test]
fn grapheme_prepended_concatenation_marks_take_their_own_cell() {
    // Alone, a number sign takes one cell; it joins the following digit
    // (UAX #29 GB9b) into a two-cell cluster, as wcwidth measures it.
    let (mut term, mut parser) = terminal();
    parser.advance(&mut term, "\u{601}".as_bytes());
    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(1)));
    let (mut term, mut parser) = terminal();
    parser.advance(&mut term, "\u{601}\u{6f1}".as_bytes());
    assert_cluster(&term, 0, 0, "\u{601}\u{6f1}", 2);
    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(2)));
}

#[test]
fn grapheme_selectors_require_an_adjacent_contributing_base() {
    for (text, width) in [
        ("*\u{301}\u{fe0f}", 1),
        ("*\u{200d}\u{fe0f}", 1),
        ("*\u{fe0e}\u{fe0f}", 1),
        ("\u{2764}\u{301}\u{fe0f}", 1),
        ("\u{231a}\u{301}\u{fe0e}", 2),
        ("*\u{fe0f}", 2),
        ("\u{2764}\u{200d}\u{1f525}\u{fe0f}", 1),
    ] {
        for split in 0..=text.len() {
            let (mut term, mut parser) = terminal();
            parser.advance(&mut term, b"\x1b[?2027h");
            parser.advance(&mut term, &text.as_bytes()[..split]);
            parser.advance(&mut term, &text.as_bytes()[split..]);
            assert_cluster(&term, 0, 0, text, width);
            assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(width)));
            assert!(!term.grid.cursor.input_needs_wrap);
            parser.advance(&mut term, b"X");
            assert_eq!(term.grid[Line(0)][Column(width)].c, 'X');
        }
    }
}

#[test]
fn grapheme_interrupted_vs16_keeps_the_right_edge_narrow() {
    for margins in [false, true] {
        for wrap in [false, true] {
            for insert in [false, true] {
                for text in ["*\u{301}\u{fe0f}", "*\u{200d}\u{fe0f}"] {
                    let (mut term, mut parser) = terminal();
                    parser.advance(&mut term, b"\x1b[?2027habcdefgh");
                    let (left, edge) = if margins {
                        parser.advance(&mut term, b"\x1b[?69h\x1b[3;6s\x1b[1;6H");
                        (2, 5)
                    } else {
                        parser.advance(&mut term, b"\x1b[1;8H");
                        (0, 7)
                    };
                    if !wrap {
                        parser.advance(&mut term, b"\x1b[?7l");
                    }
                    if insert {
                        parser.advance(&mut term, b"\x1b[4h");
                    }
                    parser.advance(&mut term, text.as_bytes());
                    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(edge)));
                    assert!(term.grid.cursor.input_needs_wrap);
                    let cell = &term.grid[Line(0)][Column(edge)];
                    assert_eq!(cell.c, '*');
                    assert_eq!(
                        cell.zerowidth().unwrap_or_default(),
                        &text.chars().skip(1).collect::<Vec<_>>()
                    );
                    assert!(!cell.flags.intersects(
                        Flags::WIDE_CHAR
                            | Flags::WIDE_CHAR_SPACER
                            | Flags::LEADING_WIDE_CHAR_SPACER
                            | Flags::WRAPLINE
                            | Flags::PARTIAL_WRAPLINE
                    ));
                    assert_eq!(term.grid[Line(1)][Column(left)].c, ' ');
                    parser.advance(&mut term, b"X");
                    let (line, col) = if wrap { (1, left) } else { (0, edge) };
                    assert_eq!(term.grid[Line(line)][Column(col)].c, 'X');
                    assert_eq!(
                        term.grid.cursor.point,
                        Point::new(Line(line), Column(if wrap { left + 1 } else { edge }))
                    );
                    assert_eq!(term.grid.cursor.input_needs_wrap, !wrap);
                    assert_eq!(
                        term.grid[Line(0)][Column(edge)].flags.contains(if margins {
                            Flags::PARTIAL_WRAPLINE
                        } else {
                            Flags::WRAPLINE
                        }),
                        wrap
                    );
                    if margins {
                        assert_eq!(term.grid[Line(0)][Column(6)].c, 'g');
                        assert_eq!(term.grid[Line(0)][Column(7)].c, 'h');
                    }
                    assert_eq!(term.mode.contains(TermMode::INSERT), insert);
                }
            }
        }
    }
}

#[test]
fn grapheme_prepend_vs15_preserves_width_spacer_and_inserted_suffix() {
    for margins in [false, true] {
        for wrap in [false, true] {
            for insert in [false, true] {
                for at_edge in [false, true] {
                    let (mut term, mut parser) = terminal();
                    parser.advance(&mut term, b"\x1b[?2027habcdefgh");
                    let (left, edge) = if margins {
                        parser.advance(&mut term, b"\x1b[?69h\x1b[3;6s");
                        (2, 5)
                    } else {
                        (0, 7)
                    };
                    let col = if at_edge { edge - 1 } else { left };
                    parser.advance(&mut term, format!("\x1b[1;{}H", col + 1).as_bytes());
                    if !wrap {
                        parser.advance(&mut term, b"\x1b[?7l");
                    }
                    if insert {
                        parser.advance(&mut term, b"\x1b[4h");
                    }
                    parser.advance(&mut term, "\u{600}\u{231a}".as_bytes());
                    let before = term.grid.cursor.point;
                    assert_eq!(term.grid.cursor.input_needs_wrap, at_edge);
                    parser.advance(&mut term, "\u{fe0e}".as_bytes());
                    assert_cluster(&term, 0, col, "\u{600}\u{231a}\u{fe0e}", 2);
                    assert_eq!(term.grid.cursor.point, before);
                    assert_eq!(term.grid.cursor.input_needs_wrap, at_edge);
                    if !at_edge {
                        assert_eq!(
                            term.grid[Line(0)][Column(col + 2)].c,
                            if insert {
                                (b'a' + col as u8) as char
                            } else {
                                (b'a' + col as u8 + 2) as char
                            }
                        );
                    }
                    parser.advance(&mut term, b"X");
                    let (line, next) = if at_edge && wrap {
                        (1, left)
                    } else {
                        (0, if at_edge { edge } else { col + 2 })
                    };
                    assert_eq!(term.grid[Line(line)][Column(next)].c, 'X');
                    assert_eq!(
                        term.grid.cursor.point,
                        Point::new(
                            Line(line),
                            Column(if at_edge && !wrap { edge } else { next + 1 })
                        )
                    );
                    assert_eq!(term.grid.cursor.input_needs_wrap, at_edge && !wrap);
                    if margins {
                        assert_eq!(term.grid[Line(0)][Column(6)].c, 'g');
                        assert_eq!(term.grid[Line(0)][Column(7)].c, 'h');
                    }
                    assert_eq!(term.mode.contains(TermMode::INSERT), insert);
                }
            }
        }
    }
}

#[test]
fn grapheme_selector_context_survives_discarded_scalars() {
    for (prefix, selector, width) in [
        (format!("*{}", "\u{301}".repeat(100)), '\u{fe0f}', 1),
        (format!("{}\u{231a}", "\u{600}".repeat(100)), '\u{fe0e}', 2),
    ] {
        let (mut term, mut parser) = terminal();
        parser.advance(&mut term, b"\x1b[?2027h");
        parser.advance(&mut term, prefix.as_bytes());
        parser.advance(&mut term, selector.to_string().as_bytes());
        assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(width)));
        let cell = &term.grid[Line(0)][Column(0)];
        assert_eq!(cell.flags.contains(Flags::WIDE_CHAR), width == 2);
        if prefix.starts_with('*') {
            assert_eq!(
                cell.zerowidth().unwrap().len(),
                cell::MAX_ZEROWIDTH_PER_CELL
            );
        } else {
            let retained = cell.zerowidth().unwrap();
            assert!(retained.len() <= cell::MAX_ZEROWIDTH_PER_CELL);
            assert_eq!(retained.iter().filter(|&&c| c == '\u{231a}').count(), 1);
            assert_eq!(cell.semantic_char(), '\u{231a}');
        }
        assert_eq!(cell.zerowidth().unwrap().last(), Some(&selector));
        assert_eq!(
            term.grid[Line(0)][Column(1)]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER),
            width == 2
        );
        parser.advance(&mut term, b"X");
        assert_eq!(term.grid[Line(0)][Column(width)].c, 'X');
    }
}

// Exact retained UCS2.3.8/wcwidth0.9.4 failures from the 35048 mode-on run.
const FINAL_VIRAMA_CASES: &[&str] = &[
    "\u{a98f}\u{a9c0}",
    "\u{a9a0}\u{a9c0}",
    "\u{a9a2}\u{a9c0}",
    "\u{a9a4}\u{a9c0}",
    "\u{a9a5}\u{a9b3}\u{a9c0}",
    "\u{a9a5}\u{a9c0}",
    "\u{a9a7}\u{a9c0}",
    "\u{a9a9}\u{a9c0}",
    "\u{a9ad}\u{a9c0}",
    "\u{a9b1}\u{a9c0}",
    "\u{11315}\u{1134d}",
    "\u{11317}\u{1134d}",
    "\u{1131a}\u{1134d}",
    "\u{1131c}\u{1134d}",
    "\u{1131e}\u{1134d}",
    "\u{1131f}\u{1134d}",
    "\u{11323}\u{1134d}",
    "\u{11324}\u{1134d}",
    "\u{11326}\u{1134d}",
    "\u{11327}\u{1134d}",
    "\u{11328}\u{1134d}",
    "\u{1132a}\u{1134d}",
    "\u{1132c}\u{1134d}",
    "\u{1132d}\u{1134d}",
    "\u{1132e}\u{1134d}",
    "\u{1132f}\u{1134d}",
    "\u{11330}\u{1134d}",
    "\u{11332}\u{1134d}",
    "\u{11335}\u{1134d}",
    "\u{11336}\u{1134d}",
    "\u{11337}\u{1134d}",
    "\u{11338}\u{1134d}",
];

const FINAL_SINGLETON_CASES: &[char] = &[
    '\u{d4e}',
    '\u{17a4}',
    '\u{17d8}',
    '\u{a8fa}',
    '\u{111c2}',
    '\u{111c3}',
    '\u{113d1}',
    '\u{1193f}',
    '\u{11941}',
    '\u{11a84}',
    '\u{11a85}',
    '\u{11a86}',
    '\u{11a87}',
    '\u{11a88}',
    '\u{11a89}',
    '\u{11d46}',
    '\u{11f02}',
];

#[test]
fn grapheme_final_viramas_keep_all_32_measured_syllables_narrow() {
    for &text in FINAL_VIRAMA_CASES {
        for split in 0..=text.len() {
            let (mut term, mut parser) = terminal();
            parser.advance(&mut term, &text.as_bytes()[..split]);
            parser.advance(&mut term, &text.as_bytes()[split..]);
            assert_cluster(&term, 0, 0, text, 1);
            assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(1)));
            parser.advance(&mut term, b"X");
            assert_eq!(term.grid[Line(0)][Column(1)].c, 'X');
        }
    }
}

#[test]
fn grapheme_mc_virama_exception_keeps_the_nonvirama_neighbor_wide() {
    for mark in [
        '\u{1b44}',
        '\u{a9c0}',
        '\u{111c0}',
        '\u{11235}',
        '\u{1134d}',
        '\u{116b6}',
    ] {
        let text = format!("A{mark}");
        let (mut term, mut parser) = terminal();
        parser.advance(&mut term, text.as_bytes());
        assert_cluster(&term, 0, 0, &text, 1);
        assert_eq!(term.grid.cursor.point.column, Column(1));
    }
    let (mut term, mut parser) = terminal();
    parser.advance(&mut term, "A\u{1b43}".as_bytes());
    assert_cluster(&term, 0, 0, "A\u{1b43}", 2);
    // Grantha virama is InCb=Extend, not a linker: these are two narrow cells.
    let (mut term, mut parser) = terminal();
    parser.advance(&mut term, "\u{11315}\u{1134d}\u{11315}".as_bytes());
    assert_cluster(&term, 0, 0, "\u{11315}\u{1134d}", 1);
    assert_eq!(term.grid[Line(0)][Column(1)].c, '\u{11315}');
    assert!(
        !term.grid[Line(0)][Column(1)]
            .flags
            .contains(Flags::WIDE_CHAR)
    );
    assert_eq!(term.grid.cursor.point.column, Column(2));
}

#[test]
fn grapheme_final_singletons_take_one_cell_and_keep_scalar_mode_unchanged() {
    for &c in FINAL_SINGLETON_CASES {
        let text = c.to_string();
        for split in 0..=text.len() {
            let (mut term, mut parser) = terminal();
            parser.advance(&mut term, &text.as_bytes()[..split]);
            parser.advance(&mut term, &text.as_bytes()[split..]);
            assert_cluster(&term, 0, 0, &text, 1);
            assert_eq!(term.grid.cursor.point.column, Column(1), "{c:?}");
            // A control commits the prefix, so this checks a separate suffix cell.
            parser.advance(&mut term, b"\x1b[0m\r\x1b[CX");
            assert_eq!(term.grid[Line(0)][Column(1)].c, 'X');
        }
        let (mut term, mut parser) = terminal();
        parser.advance(&mut term, b"\x1b[?2027l");
        parser.advance(&mut term, text.as_bytes());
        let expected = if matches!(c, '\u{17a4}' | '\u{17d8}') {
            2
        } else {
            0
        };
        assert_eq!(term.grid.cursor.point.column, Column(expected), "{c:?}");
        assert_eq!(
            term.grid[Line(0)][Column(0)]
                .flags
                .contains(Flags::WIDE_CHAR),
            expected == 2
        );
    }
}

#[test]
fn grapheme_tailored_singletons_preserve_edges_margins_and_insertions() {
    for &c in FINAL_SINGLETON_CASES {
        for margins in [false, true] {
            for wrap in [false, true] {
                for insert in [false, true] {
                    let (mut term, mut parser) = terminal();
                    parser.advance(&mut term, b"abcdefgh");
                    let (left, edge) = if margins {
                        parser.advance(&mut term, b"\x1b[?69h\x1b[3;6s");
                        (2, 5)
                    } else {
                        (0, 7)
                    };
                    parser.advance(&mut term, format!("\x1b[1;{}H", edge + 1).as_bytes());
                    if !wrap {
                        parser.advance(&mut term, b"\x1b[?7l");
                    }
                    if insert {
                        parser.advance(&mut term, b"\x1b[4h");
                    }
                    parser.advance(&mut term, c.to_string().as_bytes());
                    assert_eq!(term.grid[Line(0)][Column(edge)].c, c);
                    assert!(
                        !term.grid[Line(0)][Column(edge)]
                            .flags
                            .intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER)
                    );
                    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(edge)));
                    assert!(term.grid.cursor.input_needs_wrap);
                    parser.advance(&mut term, b"X");
                    let prepend = !matches!(c, '\u{a8fa}' | '\u{17a4}' | '\u{17d8}');
                    let (line, col) = if wrap { (1, left) } else { (0, edge) };
                    let cell = &term.grid[Line(line)][Column(col)];
                    if prepend {
                        assert_eq!(cell.c, c);
                        assert_eq!(cell.zerowidth(), Some(&['X'][..]));
                        assert_eq!(cell.flags.contains(Flags::WIDE_CHAR), wrap);
                    } else {
                        assert_eq!(cell.c, 'X');
                        assert!(!cell.flags.contains(Flags::WIDE_CHAR));
                    }
                    assert_eq!(
                        term.grid.cursor.point,
                        Point::new(
                            Line(line),
                            Column(if wrap {
                                left + if prepend { 2 } else { 1 }
                            } else {
                                edge
                            })
                        )
                    );
                    assert_eq!(term.grid.cursor.input_needs_wrap, !wrap);
                    assert_eq!(term.mode.contains(TermMode::INSERT), insert);
                    if margins {
                        assert_eq!(term.grid[Line(0)][Column(6)].c, 'g');
                        assert_eq!(term.grid[Line(0)][Column(7)].c, 'h');
                    }
                }
            }
        }
    }
}

#[test]
fn grapheme_saturated_prefix_preserves_the_first_base_and_later_presentation() {
    for prefix in ['\u{600}', '\u{d4e}'] {
        for (base, tail) in [
            ('A', "\u{fe0e}\u{fe0f}".repeat(100)),
            ('\u{231a}', "\u{fe0e}".to_owned()),
            (
                '\u{1f469}',
                format!("{}\u{200d}\u{1f4bb}", "\u{301}".repeat(100)),
            ),
        ] {
            let (mut term, mut parser) = terminal();
            parser.advance(
                &mut term,
                b"\x1b[31m\x1b]8;;https://example.invalid/base\x07",
            );
            parser.advance(&mut term, prefix.to_string().repeat(100).as_bytes());
            let first = term.grid[Line(0)][Column(0)].clone();
            assert!(first.hyperlink().is_some());
            assert!(first.zerowidth().unwrap().len() <= cell::MAX_ZEROWIDTH_PER_CELL);
            assert!(first.zerowidth().unwrap().iter().all(|&c| c == prefix));
            parser.advance(&mut term, b"\x1b[32m");
            parser.advance(&mut term, base.to_string().as_bytes());
            assert_eq!(term.grid[Line(0)][Column(0)].semantic_char(), base);
            parser.advance(&mut term, tail.as_bytes());
            let cell = &term.grid[Line(0)][Column(0)];
            let retained: Vec<_> = std::iter::once(cell.c)
                .chain(cell.zerowidth().unwrap().iter().copied())
                .collect();
            assert_eq!(retained.iter().filter(|&&c| c == base).count(), 1);
            let base_at = retained.iter().position(|&c| c == base).unwrap();
            assert!(retained[..base_at].iter().all(|&c| c == prefix));
            assert!(retained.len() <= cell::MAX_ZEROWIDTH_PER_CELL + 1);
            assert_eq!(cell.semantic_char(), base);
            assert_eq!(cell.fg, first.fg);
            assert_eq!(cell.hyperlink(), first.hyperlink());
            if base == '\u{1f469}' {
                assert_eq!(&retained[retained.len() - 2..], &['\u{200d}', '\u{1f4bb}']);
            } else {
                assert_eq!(retained.last(), tail.chars().last().as_ref());
            }
            assert_eq!(term.grid.cursor.point.column, Column(2));
            parser.advance(&mut term, b"X");
            assert_eq!(term.grid[Line(0)][Column(2)].c, 'X');
        }
    }
}

#[test]
fn grapheme_final_viramas_stay_narrow_at_edges_with_insert_and_margins() {
    for &text in FINAL_VIRAMA_CASES {
        for margins in [false, true] {
            for wrap in [false, true] {
                for insert in [false, true] {
                    let (mut term, mut parser) = terminal();
                    parser.advance(&mut term, b"abcdefgh");
                    let (left, edge) = if margins {
                        parser.advance(&mut term, b"\x1b[?69h\x1b[3;6s");
                        (2, 5)
                    } else {
                        (0, 7)
                    };
                    parser.advance(&mut term, format!("\x1b[1;{}H", edge + 1).as_bytes());
                    if !wrap {
                        parser.advance(&mut term, b"\x1b[?7l");
                    }
                    if insert {
                        parser.advance(&mut term, b"\x1b[4h");
                    }
                    parser.advance(&mut term, text.as_bytes());
                    let cell = &term.grid[Line(0)][Column(edge)];
                    let stored: String = std::iter::once(cell.c)
                        .chain(cell.zerowidth().unwrap().iter().copied())
                        .collect();
                    assert_eq!(stored, text);
                    assert!(
                        !cell
                            .flags
                            .intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER)
                    );
                    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(edge)));
                    assert!(term.grid.cursor.input_needs_wrap);
                    parser.advance(&mut term, b"X");
                    let (line, col) = if wrap { (1, left) } else { (0, edge) };
                    assert_eq!(term.grid[Line(line)][Column(col)].c, 'X');
                    assert_eq!(term.grid.cursor.input_needs_wrap, !wrap);
                    assert_eq!(term.mode.contains(TermMode::INSERT), insert);
                    if margins {
                        assert_eq!(term.grid[Line(0)][Column(6)].c, 'g');
                        assert_eq!(term.grid[Line(0)][Column(7)].c, 'h');
                    }
                }
            }
        }
    }
}

#[test]
fn grapheme_reserved_first_base_survives_edge_widening_and_tail_replacement() {
    for (prefix, base) in [('\u{600}', 'A'), ('\u{94d}', '\u{915}')] {
        for margins in [false, true] {
            for wrap in [false, true] {
                for insert in [false, true] {
                    let (mut term, mut parser) = terminal();
                    parser.advance(&mut term, b"abcdefgh");
                    let (left, edge) = if margins {
                        parser.advance(&mut term, b"\x1b[?69h\x1b[3;6s");
                        (2, 5)
                    } else {
                        (0, 7)
                    };
                    parser.advance(&mut term, format!("\x1b[1;{}H", edge).as_bytes());
                    if !wrap {
                        parser.advance(&mut term, b"\x1b[?7l");
                    }
                    if insert {
                        parser.advance(&mut term, b"\x1b[4h");
                    }
                    parser.advance(&mut term, prefix.to_string().repeat(100).as_bytes());
                    parser.advance(&mut term, base.to_string().as_bytes());
                    if prefix == '\u{94d}' {
                        parser.advance(&mut term, "\u{93e}".as_bytes());
                    }
                    parser.advance(&mut term, "\u{fe0f}\u{fe0e}".repeat(100).as_bytes());
                    let cell = &term.grid[Line(0)][Column(edge - 1)];
                    assert_eq!(cell.semantic_char(), base);
                    assert_eq!(
                        cell.zerowidth()
                            .unwrap()
                            .iter()
                            .filter(|&&c| c == base)
                            .count(),
                        1
                    );
                    assert_eq!(cell.zerowidth().unwrap().last(), Some(&'\u{fe0e}'));
                    assert!(cell.flags.contains(Flags::WIDE_CHAR));
                    assert!(
                        term.grid[Line(0)][Column(edge)]
                            .flags
                            .contains(Flags::WIDE_CHAR_SPACER)
                    );
                    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(edge)));
                    assert!(term.grid.cursor.input_needs_wrap);
                    parser.advance(&mut term, b"X");
                    let (line, col) = if wrap { (1, left) } else { (0, edge) };
                    assert_eq!(term.grid[Line(line)][Column(col)].c, 'X');
                    assert_eq!(term.mode.contains(TermMode::INSERT), insert);
                    if margins {
                        assert_eq!(term.grid[Line(0)][Column(6)].c, 'g');
                        assert_eq!(term.grid[Line(0)][Column(7)].c, 'h');
                    }
                }
            }
        }
    }
}
