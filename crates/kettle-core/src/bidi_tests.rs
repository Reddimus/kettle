use super::*;

/// One narrow cell per character; `W` before a character makes it a
/// two-column character (lead plus spacer).
fn row(text: &str) -> Vec<BidiCell> {
    let mut cells = Vec::new();
    let mut wide = false;
    for c in text.chars() {
        if c == 'W' && !wide {
            wide = true;
            continue;
        }
        if wide {
            cells.push(BidiCell::new(c, CellKind::WideLead, &[]));
            cells.push(BidiCell::new(' ', CellKind::WideSpacer, &[]));
            wide = false;
        } else {
            cells.push(BidiCell::new(c, CellKind::Narrow, &[]));
        }
    }
    cells
}

fn log2vis(map: &RowBidi) -> Vec<usize> {
    (0..map.columns()).map(|col| map.visual(col)).collect()
}

/// The row as it is shown: each visual column's logical cell, with its
/// display substitution.
fn shown(cells: &[BidiCell], map: &RowBidi) -> String {
    (0..map.columns())
        .map(|vcol| {
            let col = map.logical(vcol);
            map.display_char(col).unwrap_or(cells[col].c)
        })
        .collect()
}

#[test]
fn rows_without_right_to_left_text_have_no_map() {
    assert_eq!(row_bidi(&row("plain ASCII, (brackets) <angles> 123")), None);
    assert_eq!(row_bidi(&row("W中W文 and Wé")), None);
    assert_eq!(row_bidi(&row("box ─┼─ and W😀")), None);
    assert_eq!(row_bidi(&[]), None);
}

#[test]
fn a_hebrew_word_reverses_in_place() {
    let cells = row("ab שלום cd");
    let map = row_bidi(&cells).expect("a Hebrew row has a map");
    assert_eq!(log2vis(&map), [0, 1, 2, 6, 5, 4, 3, 7, 8, 9]);
    for vcol in 0..map.columns() {
        assert_eq!(map.visual(map.logical(vcol)), vcol);
    }
    assert_eq!(
        (0..10).filter(|&col| map.is_rtl(col)).collect::<Vec<_>>(),
        [3, 4, 5, 6]
    );
    assert_eq!(shown(&cells, &map), "ab םולש cd");
}

#[test]
fn numbers_and_brackets_inside_right_to_left_text() {
    // The FriBidi reference case: brackets around a number after Hebrew
    // resolve right to left and mirror; the digits keep their order.
    let cells = row("abc שלום (123) def");
    let map = row_bidi(&cells).expect("a Hebrew row has a map");
    assert_eq!(
        log2vis(&map),
        [0, 1, 2, 3, 13, 12, 11, 10, 9, 8, 5, 6, 7, 4, 14, 15, 16, 17]
    );
    assert_eq!(map.display_char(9), Some(')'));
    assert_eq!(map.display_char(13), Some('('));
    assert_eq!(map.display_char(10), None);
    assert_eq!(shown(&cells, &map), "abc (123) םולש def");
}

#[test]
fn brackets_in_left_to_right_text_are_not_mirrored() {
    let cells = row("(a) א");
    let map = row_bidi(&cells).expect("a Hebrew row has a map");
    assert_eq!(map.display_char(0), None);
    assert_eq!(map.display_char(2), None);
    assert_eq!(shown(&cells, &map), "(a) א");
}

#[test]
fn a_two_column_character_keeps_its_lead_on_the_left() {
    let cells = row("אW😀ב");
    let map = row_bidi(&cells).expect("a Hebrew row has a map");
    assert_eq!(log2vis(&map), [3, 1, 2, 0]);
    assert_eq!(map.logical(1), 1);
    assert_eq!(map.logical(2), 2);
}

#[test]
fn marks_move_with_their_cell_and_do_not_change_the_layout() {
    let plain = row("ab שלום cd");
    let mut marked = plain.clone();
    // Hebrew points stored with two letters, and a joiner on a Latin cell.
    marked[3] = BidiCell::new('ש', CellKind::Narrow, &['\u{5BC}', '\u{5C1}']);
    marked[5] = BidiCell::new('ו', CellKind::Narrow, &['\u{5B9}']);
    assert_eq!(row_bidi(&marked), row_bidi(&plain));
}

#[test]
fn arabic_letters_take_their_contextual_forms() {
    let cells = row("hello مرحبا world");
    let map = row_bidi(&cells).expect("an Arabic row has a map");
    let forms: Vec<char> = (6..=10).filter_map(|col| map.display_char(col)).collect();
    // meem initial, reh final, hah initial (reh does not join forward),
    // beh medial, alef final.
    assert_eq!(
        forms,
        ['\u{FEE3}', '\u{FEAE}', '\u{FEA3}', '\u{FE92}', '\u{FE8E}']
    );
    assert_eq!(
        shown(&cells, &map),
        "hello \u{FE8E}\u{FE92}\u{FEA3}\u{FEAE}\u{FEE3} world"
    );
}

#[test]
fn lam_alef_stays_two_joined_cells() {
    let map = row_bidi(&row("لا")).expect("an Arabic row has a map");
    assert_eq!(map.display_char(0), Some('\u{FEDF}'));
    assert_eq!(map.display_char(1), Some('\u{FE8E}'));
    let map = row_bidi(&row("سلام")).expect("an Arabic row has a map");
    let forms: Vec<char> = (0..4).filter_map(|col| map.display_char(col)).collect();
    assert_eq!(forms, ['\u{FEB3}', '\u{FEE0}', '\u{FE8E}', '\u{FEE1}']);
}

#[test]
fn joiners_stored_as_marks_force_or_break_a_join() {
    let cells = [
        BidiCell::new('ب', CellKind::Narrow, &['\u{200C}']),
        BidiCell::new('ب', CellKind::Narrow, &[]),
    ];
    let map = row_bidi(&cells).expect("an Arabic row has a map");
    assert_eq!(map.display_char(0), Some('\u{FE8F}'));
    assert_eq!(map.display_char(1), Some('\u{FE8F}'));
    let lone = [BidiCell::new('ب', CellKind::Narrow, &['\u{200D}'])];
    let map = row_bidi(&lone).expect("an Arabic row has a map");
    assert_eq!(map.display_char(0), Some('\u{FE91}'));
}

#[test]
fn explicit_bidi_controls_cannot_reorder_a_row() {
    // An override stored as a mark is not a base character at all.
    let marked = [
        BidiCell::new('a', CellKind::Narrow, &['\u{202E}']),
        BidiCell::new('b', CellKind::Narrow, &[]),
    ];
    assert_eq!(row_bidi(&marked), None);
    // As a base character it is laid out as a blank: the Latin text after
    // it keeps its order instead of being reversed.
    let map = row_bidi(&row("ab\u{202E}cd א")).expect("a Hebrew row has a map");
    assert_eq!(log2vis(&map), [0, 1, 2, 3, 4, 5, 6]);
}

#[test]
fn trailing_blanks_stay_at_the_end() {
    let map = row_bidi(&row("שלום   ")).expect("a Hebrew row has a map");
    assert_eq!(log2vis(&map), [3, 2, 1, 0, 4, 5, 6]);
    let mut cells = row("א");
    cells.extend([BidiCell::new('\0', CellKind::Narrow, &[]); 3]);
    let map = row_bidi(&cells).expect("a Hebrew row has a map");
    assert_eq!(log2vis(&map), [0, 1, 2, 3]);
}

#[test]
fn a_pointer_maps_to_the_logical_cell_and_side() {
    let map = row_bidi(&row("ab שלום")).expect("a Hebrew row has a map");
    // The left half of visual column 3 shows ם, logical 6, whose logical
    // end is on the left.
    assert_eq!(map.logical_point(3, Side::Left), (6, Side::Right));
    assert_eq!(map.logical_point(1, Side::Right), (1, Side::Right));
    // Past the row's end: the right half of the rightmost visual cell, ש,
    // which is its logical start.
    assert_eq!(map.logical_point(40, Side::Left), (3, Side::Left));
}

#[test]
fn a_logical_span_splits_into_visual_runs() {
    let map = row_bidi(&row("ab שלום")).expect("a Hebrew row has a map");
    assert_eq!(map.visual_runs(0..=4), [0..3, 5..7]);
    assert_eq!(
        map.visual_runs(3..=6),
        std::iter::once(3..7).collect::<Vec<_>>()
    );
    assert_eq!(
        map.visual_runs(5..=99),
        std::iter::once(3..5).collect::<Vec<_>>()
    );
}

#[test]
fn a_ligature_guard_surrounds_right_to_left_cells() {
    let map = row_bidi(&row("ab שלום cd")).expect("a Hebrew row has a map");
    assert!(!map.guard_after(0));
    assert!(map.guard_after(2));
    assert!(map.guard_after(5));
    assert!(map.guard_after(6));
    assert!(!map.guard_after(8));
}

#[test]
fn tables_match_the_bidi_classes_they_extend() {
    assert_eq!(tables::UCD_VERSION, unicode_bidi::UNICODE_VERSION);
    assert_eq!(tables::MIRRORS.len(), 428);
    assert!(tables::MIRRORS.windows(2).all(|pair| pair[0].0 < pair[1].0));
    for (open, close) in [('(', ')'), ('<', '>'), ('«', '»'), ('≤', '≥'), ('[', ']')] {
        assert_eq!(mirror(open), Some(close));
        assert_eq!(mirror(close), Some(open));
    }
    // Box drawing is never mirrored, so a frame keeps its corners.
    assert!((0x2500..=0x257F).all(|code| char::from_u32(code).and_then(mirror).is_none()));
    assert!(tables::FORMS.windows(2).all(|pair| pair[0].0 < pair[1].0));
    for &(base, _) in tables::FORMS {
        assert_eq!(bidi_class(base), BidiClass::AL, "{base:?}");
    }
    assert_eq!(joining('ا'), Some(Joining::Right));
    assert_eq!(joining('ب'), Some(Joining::Dual));
    assert_eq!(joining('ـ'), Some(Joining::Causing));
    assert_eq!(joining('a'), None);
}
