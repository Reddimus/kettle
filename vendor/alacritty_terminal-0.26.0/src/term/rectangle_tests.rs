use super::*;
use crate::event::VoidListener;
use crate::term::test::TermSize;

fn terminal(text: &str) -> (Term<VoidListener>, ansi::Processor) {
    let mut term = Term::new(Config::default(), &TermSize::new(8, 4), VoidListener);
    let mut parser: ansi::Processor = ansi::Processor::new();
    parser.advance(&mut term, text.as_bytes());
    (term, parser)
}

fn row(term: &Term<VoidListener>, line: i32) -> String {
    term.grid[Line(line)][..]
        .iter()
        .map(|cell| cell.c)
        .collect()
}

#[test]
fn rectangle_fill_clips_and_keeps_cursor_and_pending_wrap() {
    let (mut term, mut parser) = terminal("abcdefgh\x1b[33m\x1b[90;2;7;99;99$x");
    assert_eq!(row(&term, 0), "abcdefgh");
    assert_eq!(row(&term, 1), "      ZZ");
    assert_eq!(row(&term, 3), "      ZZ");
    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(7)));
    assert!(term.grid.cursor.input_needs_wrap);
    assert_eq!(
        term.grid[Line(1)][Column(6)].fg,
        Color::Named(NamedColor::Yellow)
    );
    parser.advance(&mut term, b"\x1b[65;4;4;2;2$x\x1b[66;9;1;99;99$x\x1b[9$x");
    assert_eq!(row(&term, 3), "BBBBBBBB");
    assert_eq!(term.grid.history_size(), 0);
}

#[test]
fn rectangle_copy_handles_overlap_and_copies_complete_cell_metadata() {
    let (mut term, mut parser) =
        terminal("\x1b[1;31m\x1b[1\"qa\u{301}bcdefgh\x1b[0\"q\x1b[0m\x1b[2;1Hijklmnop");
    let source = term.grid[Line(0)][Column(0)].clone();
    parser.advance(&mut term, b"\x1b[1;1;2;4;1;2;3;1$v");
    assert_eq!(row(&term, 1), "ijabcdop");
    assert_eq!(row(&term, 2), "  ijkl  ");
    assert_eq!(term.grid[Line(1)][Column(2)], source);
}

#[test]
fn rectangle_copy_defaults_clip_destination_and_clamp_page_numbers() {
    let (mut term, mut parser) = terminal("abcdefgh\x1b[2;1Hijklmnop");
    parser.advance(&mut term, b"\x1b[0;0;2;2;0;4;8;0$v");
    assert_eq!(row(&term, 3), "       a");
    parser.advance(&mut term, b"\x1b[1;1;2;2;2;3;1;1$v\x1b[1;1;2;2;1;3;1;2$v");
    assert_eq!(row(&term, 2), "ab      ");
}

#[test]
fn rectangle_coordinates_use_margins_only_in_origin_mode() {
    let (mut term, mut parser) = terminal("\x1b[?69h\x1b[3;6s\x1b[2;4r\x1b[65;1;1;2;2$x");
    assert_eq!(row(&term, 0), "AA      ");
    parser.advance(&mut term, b"\x1b[?6h\x1b[66;1;1;99;99$x");
    assert_eq!(row(&term, 0), "AA      ");
    assert_eq!(row(&term, 1), "AABBBB  ");
    assert_eq!(row(&term, 3), "  BBBB  ");
    parser.advance(&mut term, b"\x1b[1;1;1;2;1;2;2;1$v");
    assert_eq!(row(&term, 2), "  BBBB  ");
}

#[test]
fn rectangle_erase_ignores_iso_while_selective_erase_respects_only_dec() {
    for protect in ["\x1b[1\"q", "\x1bV"] {
        let end = if protect == "\x1bV" {
            "\x1bW"
        } else {
            "\x1b[0\"q"
        };
        let (mut term, mut parser) = terminal(&format!("{protect}AB{end}CD"));
        parser.advance(&mut term, b"\x1b[1;1;1;4${");
        assert_eq!(
            row(&term, 0),
            if protect == "\x1bV" {
                "        "
            } else {
                "AB      "
            }
        );
        parser.advance(&mut term, b"\x1b[41m\x1b[1;1;1;4$z");
        assert_eq!(row(&term, 0), "        ");
        assert_eq!(
            term.grid[Line(0)][Column(0)].bg,
            Color::Named(NamedColor::Red)
        );
        assert!(
            !term.grid[Line(0)][Column(0)]
                .flags
                .contains(Flags::PROTECTED)
        );
    }
}

#[test]
fn rectangle_copy_and_fill_never_create_split_or_false_wide_pairs() {
    let (mut term, mut parser) = terminal("界語abcd\x1b[2;1H12界3456");
    parser.advance(&mut term, b"\x1b[1;2;1;3;1;2;4;1$v");
    assert_eq!(row(&term, 1), "12   456");
    parser.advance(&mut term, b"\x1b[90;1;2;1;2$x");
    assert_eq!(row(&term, 0), " Z語 abcd");
    for line in 0..4 {
        for col in 0..8 {
            let flags = term.grid[Line(line)][Column(col)].flags;
            if flags.contains(Flags::WIDE_CHAR) {
                assert!(col + 1 < 8);
                assert!(
                    term.grid[Line(line)][Column(col + 1)]
                        .flags
                        .contains(Flags::WIDE_CHAR_SPACER)
                );
            }
            if flags.contains(Flags::WIDE_CHAR_SPACER) {
                assert!(col > 0);
                assert!(
                    term.grid[Line(line)][Column(col - 1)]
                        .flags
                        .contains(Flags::WIDE_CHAR)
                );
            }
        }
    }
}

#[test]
fn rectangle_selective_erase_preserves_visual_attributes_and_clears_combining_text() {
    let (mut term, mut parser) = terminal("\x1b[1;4;32;44mA\u{301}B\x1b[0m");
    parser.advance(&mut term, b"\x1b[1;1;1;2${");
    let cell = &term.grid[Line(0)][Column(0)];
    assert_eq!(cell.c, ' ');
    assert!(cell.flags.contains(Flags::BOLD | Flags::UNDERLINE));
    assert_eq!(cell.fg, Color::Named(NamedColor::Green));
    assert_eq!(cell.bg, Color::Named(NamedColor::Blue));
    assert!(cell.zerowidth().is_none_or(|chars| chars.is_empty()));
}

#[test]
fn rectangle_controls_are_ignored_below_level_four() {
    let (mut term, mut parser) = terminal("\x1b[63;1\"pABCD");
    parser.advance(&mut term, b"\x1b[90$x\x1b[$z\x1b[${\x1b[1;1;1;4;1;2;1;1$v");
    assert_eq!(row(&term, 0), "ABCD    ");
    assert_eq!(row(&term, 1), "        ");
}

#[test]
fn rectangle_replacement_clears_only_linked_leading_wide_spacers() {
    for command in [
        "\x1b[90;2;1;2;1$x",
        "\x1b[2;1;2;1$z",
        "\x1b[1;1;1;1;1;2;1;1$v",
    ] {
        let mut term = Term::new(Config::default(), &TermSize::new(4, 3), VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, "abc\x1b[1\"q界\x1b[0\"q".as_bytes());
        assert!(
            term.grid[Line(0)][Column(3)]
                .flags
                .contains(Flags::LEADING_WIDE_CHAR_SPACER)
        );
        parser.advance(&mut term, command.as_bytes());
        assert!(
            !term.grid[Line(0)][Column(3)]
                .flags
                .intersects(Flags::LEADING_WIDE_CHAR_SPACER | Flags::PROTECTED)
        );
        let mut term = Term::new(Config::default(), &TermSize::new(4, 3), VoidListener);
        parser.advance(&mut term, "abc\x1b[1\"qP\x1b[2;1H界\x1b[0\"q".as_bytes());
        parser.advance(&mut term, command.as_bytes());
        assert_eq!(term.grid[Line(0)][Column(3)].c, 'P');
        assert!(
            term.grid[Line(0)][Column(3)]
                .flags
                .contains(Flags::PROTECTED)
        );
    }
}

#[test]
fn rectangle_copies_match_whole_glyph_reference_at_every_horizontal_boundary() {
    let fixtures = ["界語abcd", "a界b語cd", "abc界語d", "abcdef界"];
    for text in fixtures {
        let mut glyphs = Vec::new();
        let mut column = 0;
        for character in text.chars() {
            let width = character.width().unwrap();
            glyphs.push((column..column + width, character));
            column += width;
        }
        for source in 0..8 {
            for destination in 0..8 {
                for count in 1..=8 {
                    let width = count.min(8 - source).min(8 - destination);
                    let (mut term, mut parser) = terminal(text);
                    let mut expected: Vec<_> = term.grid[Line(0)][..]
                        .iter()
                        .map(|cell| {
                            (
                                cell.c,
                                cell.flags & (Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER),
                            )
                        })
                        .collect();
                    // Remove complete destination glyphs before placing the immutable source.
                    for (span, _) in &glyphs {
                        if span.start < destination + width && span.end > destination {
                            for cell in &mut expected[span.clone()] {
                                *cell = (' ', Flags::empty());
                            }
                        }
                    }
                    expected[destination..destination + width].fill((' ', Flags::empty()));
                    for (span, character) in &glyphs {
                        if span.start >= source && span.end <= source + width {
                            let target = destination + span.start - source;
                            expected[target] = (
                                *character,
                                if span.len() == 2 {
                                    Flags::WIDE_CHAR
                                } else {
                                    Flags::empty()
                                },
                            );
                            if span.len() == 2 {
                                expected[target + 1] = (' ', Flags::WIDE_CHAR_SPACER);
                            }
                        }
                    }
                    let command = format!(
                        "\x1b[1;{};1;{};1;1;{};1$v",
                        source + 1,
                        source + count,
                        destination + 1
                    );
                    parser.advance(&mut term, command.as_bytes());
                    let actual: Vec<_> = term.grid[Line(0)][..]
                        .iter()
                        .map(|cell| {
                            (
                                cell.c,
                                cell.flags & (Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER),
                            )
                        })
                        .collect();
                    assert_eq!(
                        actual, expected,
                        "{text:?} source={source} destination={destination} count={count}"
                    );
                }
            }
        }
    }
}

#[test]
fn rectangle_replacing_wrapped_glyph_invalidates_a_linked_history_selection() {
    let mut term = Term::new(Config::default(), &TermSize::new(4, 1), VoidListener);
    let mut parser: ansi::Processor = ansi::Processor::new();
    parser.advance(&mut term, "abc界".as_bytes());
    assert_eq!(term.grid.history_size(), 1);
    let point = Point::new(Line(-1), Column(3));
    let mut selection = Selection::new(SelectionType::Simple, point, Side::Left);
    selection.update(point, Side::Right);
    term.selection = Some(selection);
    parser.advance(&mut term, b"\x1b[90;1;1;1;1$x");
    assert!(term.selection.is_none());
}
