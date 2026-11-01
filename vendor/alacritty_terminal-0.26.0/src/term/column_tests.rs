use super::*;
use crate::event::VoidListener;
use crate::term::test::TermSize;

fn terminal() -> (Term<VoidListener>, ansi::Processor) {
    let mut term = Term::new(Config::default(), &TermSize::new(8, 4), VoidListener);
    let mut parser = ansi::Processor::new();
    parser.advance(
        &mut term,
        b"abcdefgh\x1b[2;1Hijklmnop\x1b[3;1Hqrstuvwx\x1b[4;1Hyz012345\x1b[?69h\x1b[3;6s\x1b[2;3r",
    );
    (term, parser)
}

fn row(term: &Term<VoidListener>, line: i32) -> String {
    term.grid[Line(line)][..]
        .iter()
        .map(|cell| cell.c)
        .collect()
}

#[test]
fn column_insert_and_delete_affect_only_the_scroll_rectangle() {
    for (command, expected) in [
        ("\x1b[2'}", ["ijk  lop", "qrs  twx"]),
        ("\x1b[2'~", ["ijkn  op", "qrsv  wx"]),
    ] {
        let (mut term, mut parser) = terminal();
        parser.advance(&mut term, b"\x1b[2;4H\x1b[1;31;44m");
        let cursor = term.grid.cursor.clone();
        parser.advance(&mut term, command.as_bytes());
        assert_eq!(row(&term, 0), "abcdefgh");
        assert_eq!(row(&term, 1), expected[0]);
        assert_eq!(row(&term, 2), expected[1]);
        assert_eq!(row(&term, 3), "yz012345");
        assert_eq!(term.grid.cursor, cursor);
        assert_eq!(term.grid.history_size(), 0);
        let blank = &term.grid[Line(1)][Column(if command.ends_with('}') { 3 } else { 4 })];
        assert!(blank.flags.is_empty());
        assert_eq!(blank.fg, Color::Named(NamedColor::Foreground));
        assert_eq!(blank.bg, Color::Named(NamedColor::Blue));
    }
}

#[test]
fn horizontal_index_moves_outside_margins_and_scrolls_at_the_edges() {
    for (column, command, expected) in [(3, "\x1b6", "ij klmop"), (6, "\x1b9", "ijlmn op")] {
        let (mut term, mut parser) = terminal();
        parser.advance(&mut term, format!("\x1b[2;{column}H{command}").as_bytes());
        assert_eq!(row(&term, 1), expected);
        assert_eq!(
            term.grid.cursor.point,
            Point::new(Line(1), Column(column - 1))
        );
        assert_eq!(row(&term, 0), "abcdefgh");
    }
    let (mut term, mut parser) = terminal();
    parser.advance(&mut term, b"\x1b[1;2H\x1b6");
    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(0)));
    parser.advance(&mut term, b"\x1b[1;7H\x1b9");
    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(7)));
}

#[test]
fn horizontal_index_at_a_margin_outside_the_scroll_rows_is_ignored() {
    for line in [1, 4] {
        for (column, command) in [(3, "\x1b6"), (6, "\x1b9")] {
            let (mut term, mut parser) = terminal();
            parser.advance(&mut term, format!("\x1b[{line};{column}H").as_bytes());
            let cursor = term.grid.cursor.clone();
            let before: Vec<_> = (0..4).map(|line| row(&term, line)).collect();
            parser.advance(&mut term, command.as_bytes());
            assert_eq!(term.grid.cursor, cursor);
            assert_eq!(
                (0..4).map(|line| row(&term, line)).collect::<Vec<_>>(),
                before
            );
        }
    }
}

#[test]
fn column_edits_ignore_outside_cursors_and_clamp_large_counts() {
    let (mut term, mut parser) = terminal();
    parser.advance(&mut term, b"\x1b[1;4H\x1b[65535'}\x1b[2;2H\x1b[65535'~");
    assert_eq!(row(&term, 1), "ijklmnop");
    parser.advance(&mut term, b"\x1b[2;4H\x1b[65535'}");
    assert_eq!(row(&term, 1), "ijk   op");
    parser.advance(&mut term, b"\x1b[2;3H\x1b[65535'~");
    assert_eq!(row(&term, 1), "ij    op");
}

#[test]
fn extended_reverse_wrap_respects_margins_and_pending_wrap() {
    let (mut term, mut parser) = terminal();
    parser.advance(&mut term, b"\x1b[?1045h\x1b[2;3H\x08");
    assert_eq!(term.grid.cursor.point, Point::new(Line(2), Column(5)));
    parser.advance(&mut term, b"\x1b[3;3H\x1b[5D");
    assert_eq!(term.grid.cursor.point, Point::new(Line(2), Column(5)));
    parser.advance(&mut term, b"\x1b[3;6HX\x08Y");
    assert_eq!(term.grid[Line(2)][Column(5)].c, 'Y');
    parser.advance(&mut term, b"\x1b[?7l\x1b[3;3H\x08");
    assert_eq!(term.grid.cursor.point, Point::new(Line(2), Column(2)));
}

#[test]
fn ordinary_reverse_wrap_stops_at_unwrapped_lines_and_resets() {
    let mut term = Term::new(Config::default(), &TermSize::new(8, 4), VoidListener);
    let mut parser: ansi::Processor = ansi::Processor::new();
    parser.advance(&mut term, b"abcdefghXY\x1b[?45h\x1b[2;1H\x08");
    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(7)));
    parser.advance(&mut term, b"\x1b[3;1H\x08");
    assert_eq!(term.grid.cursor.point, Point::new(Line(2), Column(0)));
    parser.advance(&mut term, b"\x1b[?1045h\x1b[!p\x1b[2;1H\x08");
    assert_eq!(term.grid.cursor.point, Point::new(Line(1), Column(0)));
}

#[test]
fn ordinary_reverse_wrap_tracks_partial_width_wraps_without_joining_full_rows() {
    let (mut term, mut parser) = terminal();
    parser.advance(&mut term, b"\x1b[r\x1b[1;3HABCDE\x1b[?45h\x1b[2;3H\x08");
    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(5)));
    assert!(
        !term.grid[Line(0)][Column(7)]
            .flags
            .contains(Flags::WRAPLINE)
    );
}

#[test]
fn rectangle_copy_never_transplants_a_partial_wrap_marker() {
    let (mut term, mut parser) = terminal();
    parser.advance(
        &mut term,
        b"\x1b[r\x1b[1;3HABCDE\x1b[1;3;1;6;1;3;3;1$v\x1b[?45h\x1b[4;3H\x08",
    );
    assert_eq!(term.grid.cursor.point, Point::new(Line(3), Column(2)));
}

#[test]
fn rectangle_replacements_preserve_destination_partial_wrap_state() {
    for command in [
        "\x1b[90;1;5;1;6$x",
        "\x1b[1;5;1;6$z",
        "\x1b[3;3;3;6;1;1;3;1$v",
    ] {
        let (mut term, mut parser) = terminal();
        parser.advance(&mut term, b"\x1b[r\x1b[1;3HABCDE");
        parser.advance(&mut term, command.as_bytes());
        parser.advance(&mut term, b"\x1b[?45h\x1b[2;3H\x08");
        assert_eq!(
            term.grid.cursor.point,
            Point::new(Line(0), Column(5)),
            "{command:?}"
        );
    }
}

#[test]
fn column_edits_retire_a_linked_leading_spacer_and_history_selection() {
    let mut term = Term::new(Config::default(), &TermSize::new(4, 1), VoidListener);
    let mut parser: ansi::Processor = ansi::Processor::new();
    parser.advance(&mut term, "abc\x1b[1\"q界\x1b[0\"q\x1b[1;1H".as_bytes());
    let point = Point::new(Line(-1), Column(3));
    let mut selection = Selection::new(SelectionType::Simple, point, Side::Left);
    selection.update(point, Side::Right);
    term.selection = Some(selection);
    parser.advance(&mut term, b"\x1b['}");
    assert!(
        !term.grid[Line(-1)][Column(3)]
            .flags
            .intersects(Flags::LEADING_WIDE_CHAR_SPACER | Flags::PROTECTED)
    );
    assert!(term.selection.is_none());
}

#[test]
fn reverse_wrap_recognizes_existing_full_row_wraps_after_margins_change() {
    let mut term = Term::new(Config::default(), &TermSize::new(8, 4), VoidListener);
    let mut parser: ansi::Processor = ansi::Processor::new();
    parser.advance(
        &mut term,
        b"abcdefghX\x1b[?69h\x1b[3;6s\x1b[?45h\x1b[2;3H\x08",
    );
    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(5)));
}

#[test]
fn partial_wrap_state_survives_margin_changes_and_soft_reset() {
    for (change, column, expected) in [("\x1b[2;5s", 2, 4), ("\x1b[?69l", 1, 7), ("\x1b[!p", 1, 7)]
    {
        let (mut term, mut parser) = terminal();
        parser.advance(&mut term, b"\x1b[r\x1b[1;3HABCDE");
        parser.advance(&mut term, change.as_bytes());
        parser.advance(
            &mut term,
            format!("\x1b[?45h\x1b[2;{column}H\x08").as_bytes(),
        );
        assert_eq!(
            term.grid.cursor.point,
            Point::new(Line(0), Column(expected)),
            "{change:?}"
        );
    }
}

#[test]
fn column_deletion_retires_spacers_before_earlier_rows_shift() {
    let mut term = Term::new(Config::default(), &TermSize::new(4, 2), VoidListener);
    let mut parser: ansi::Processor = ansi::Processor::new();
    parser.advance(&mut term, "abc界\x1b[1;1H\x1b['~".as_bytes());
    for line in 0..2 {
        assert!(
            term.grid[Line(line)][..]
                .iter()
                .all(|cell| !cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER))
        );
    }
}

#[test]
fn column_edits_retire_a_spacer_linked_below_the_scroll_region() {
    for command in ["\x1b['~", "\x1b['}"] {
        let mut term = Term::new(Config::default(), &TermSize::new(4, 4), VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(
            &mut term,
            "\x1b[2;1Habc\x1b[1\"q界\x1b[0\"q\x1b[1;2r\x1b[1;1H".as_bytes(),
        );
        parser.advance(&mut term, command.as_bytes());
        assert!(term.grid[Line(1)][..].iter().all(|cell| {
            !cell
                .flags
                .intersects(Flags::LEADING_WIDE_CHAR_SPACER | Flags::PROTECTED)
        }));
        assert_eq!(term.grid[Line(2)][Column(0)].c, '界');
        assert!(
            term.grid[Line(2)][Column(0)]
                .flags
                .contains(Flags::PROTECTED)
        );
    }
}

#[test]
fn column_deletion_retires_a_spacer_when_its_glyph_precedes_the_edit() {
    let mut term = Term::new(Config::default(), &TermSize::new(4, 4), VoidListener);
    let mut parser: ansi::Processor = ansi::Processor::new();
    parser.advance(
        &mut term,
        "abc\x1b[1\"q界\x1b[0\"q\x1b[1;3H\x1b['~".as_bytes(),
    );
    assert!(term.grid[Line(0)][..].iter().all(|cell| {
        !cell
            .flags
            .intersects(Flags::LEADING_WIDE_CHAR_SPACER | Flags::PROTECTED)
    }));
    assert_eq!(term.grid[Line(1)][Column(0)].c, '界');
    assert!(
        term.grid[Line(1)][Column(0)]
            .flags
            .contains(Flags::PROTECTED)
    );
}

#[test]
fn back_index_does_not_reverse_wrap_or_consume_pending_wrap() {
    let (mut term, mut parser) = terminal();
    parser.advance(&mut term, b"\x1b[?1045h\x1b[2;1H\x1b6");
    assert_eq!(term.grid.cursor.point, Point::new(Line(1), Column(0)));
    parser.advance(&mut term, b"\x1b[2;6HX\x1b6");
    assert_eq!(term.grid.cursor.point, Point::new(Line(1), Column(4)));
    assert!(!term.grid.cursor.input_needs_wrap);
}

#[test]
fn column_edit_keeps_a_selection_on_untouched_history_text() {
    let mut term = Term::new(Config::default(), &TermSize::new(4, 1), VoidListener);
    let mut parser: ansi::Processor = ansi::Processor::new();
    parser.advance(&mut term, "abc界\x1b[1;1H".as_bytes());
    let point = Point::new(Line(-1), Column(0));
    let mut selection = Selection::new(SelectionType::Simple, point, Side::Left);
    selection.update(point, Side::Right);
    term.selection = Some(selection.clone());
    parser.advance(&mut term, b"\x1b['}");
    assert_eq!(term.selection, Some(selection));
}

#[test]
fn rectangle_fill_keeps_selections_outside_affected_columns() {
    let (mut term, mut parser) = terminal();
    let point = Point::new(Line(1), Column(0));
    let mut selection = Selection::new(SelectionType::Simple, point, Side::Left);
    selection.update(point, Side::Right);
    term.selection = Some(selection.clone());
    parser.advance(&mut term, b"\x1b[90;2;4;2;5$x");
    assert_eq!(term.selection, Some(selection));
}
