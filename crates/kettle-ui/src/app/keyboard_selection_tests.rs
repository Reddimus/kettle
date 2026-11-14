use super::{production_source, selection_buffer_bounds};

/// The whole-buffer bounds are grid-absolute: top is the oldest history line
/// (negative), column 0; bottom is the last active row, last column.
#[test]
fn buffer_bounds_span_scrollback_to_last_cell() {
    let (top, bottom) = selection_buffer_bounds(100, 24, 80);
    assert_eq!(top.line.0, -100);
    assert_eq!(top.column.0, 0);
    assert_eq!(bottom.line.0, 23);
    assert_eq!(bottom.column.0, 79);
}

/// Degenerate dims must not underflow (empty scrollback / 1-col / 1-row).
#[test]
fn buffer_bounds_are_saturating() {
    let (top, bottom) = selection_buffer_bounds(0, 1, 1);
    assert_eq!((top.line.0, top.column.0), (0, 0));
    assert_eq!((bottom.line.0, bottom.column.0), (0, 0));
    // No scrollback, single screen line: top == bottom row.
    let (top, bottom) = selection_buffer_bounds(0, 24, 80);
    assert_eq!(top.line.0, 0);
    assert_eq!(bottom.line.0, 23);
}

/// Source-drift guard: the new selection actions must actually mutate
/// `t.selection` (not silently no-op), and the Shift+click extend path must
/// stay wired into the mouse-press handler. A behavioral test would need a
/// live Term + window; pin the wiring at the source level.
#[test]
fn selection_actions_and_shift_click_are_wired() {
    let src = production_source();
    // SelectAll arm builds a full-buffer selection.
    let all = src
        .find("Action::SelectAll =>")
        .expect("SelectAll arm present");
    assert!(
        src[all..all + 1100].contains("t.selection = Some(sel);"),
        "SelectAll must set the term selection"
    );
    // SelectToTop/Bottom arm extends/sets the selection and scrolls.
    let edge = src
        .find("Action::SelectToTop | Action::SelectToBottom =>")
        .expect("SelectToTop/Bottom arm present");
    let edge_body = &src[edge..edge + 2100];
    assert!(
        edge_body.contains("sel.update(edge, side)") && edge_body.contains("t.scroll_display("),
        "SelectToTop/Bottom must extend the selection and reveal it by scrolling"
    );
    // Shift+click extension (existing behavior) must remain wired.
    assert!(
        src.contains("self.extend_selection_to_cursor(ws, area, 0)"),
        "Shift+click must still extend the selection via extend_selection_to_cursor"
    );
    assert!(
        src.contains("self.extend_selection_to_cursor(ws, area, bcode)"),
        "Shift+right-click must record the right button as gesture owner"
    );
}

#[test]
fn vi_visual_keys_select_by_character_line_and_block() {
    use super::{ModalRepeat, modal_repeat_is_inert, vi_visual_kind};
    use kettle_core::SelectionType;
    use winit::keyboard::{Key, ModifiersState};

    let key = |c: &str| Key::Character(c.into());
    let kind = |k: &Key, ch: char, mods: ModifiersState| {
        vi_visual_kind(k, ch, mods).map(|kind| format!("{kind:?}"))
    };
    let name = |kind: SelectionType| Some(format!("{kind:?}"));
    assert_eq!(
        kind(&key("v"), 'v', ModifiersState::empty()),
        name(SelectionType::Simple)
    );
    assert_eq!(
        kind(&key("V"), 'V', ModifiersState::SHIFT),
        name(SelectionType::Lines)
    );
    // Ctrl suppresses the key's text, so Ctrl-v arrives with no character;
    // some platforms report its logical key as the control code itself.
    for ctrl_v in [key("v"), key("\u{16}")] {
        assert_eq!(
            kind(&ctrl_v, '\0', ModifiersState::CONTROL),
            name(SelectionType::Block)
        );
        // Handling it in vi mode is what keeps the key from the program, and
        // a held Ctrl-v must toggle once, not flicker.
        assert!(modal_repeat_is_inert(
            ModalRepeat::ViMode,
            true,
            &ctrl_v,
            true
        ));
    }
    assert!(modal_repeat_is_inert(
        ModalRepeat::ViMode,
        true,
        &key("V"),
        false
    ));
    // Caps Lock reports Ctrl-v's key in upper case.
    assert_eq!(
        kind(&key("V"), '\0', ModifiersState::CONTROL),
        name(SelectionType::Block)
    );
    // Ctrl+Shift+V is the Linux and Windows paste chord, not a block key.
    for ctrl_shift_v in [key("V"), key("v")] {
        assert_eq!(
            kind(
                &ctrl_shift_v,
                '\0',
                ModifiersState::CONTROL | ModifiersState::SHIFT
            ),
            None
        );
    }
    for mods in [
        ModifiersState::CONTROL | ModifiersState::ALT,
        ModifiersState::CONTROL | ModifiersState::SUPER,
        ModifiersState::empty(),
    ] {
        assert_eq!(kind(&key("v"), '\0', mods), None, "{mods:?}");
    }
    assert_eq!(kind(&key("x"), 'x', ModifiersState::empty()), None);
    assert_eq!(kind(&key("x"), '\0', ModifiersState::CONTROL), None);
}

#[test]
fn vi_line_and_block_visual_yank_whole_lines_and_column_slices() {
    use super::toggle_vi_visual;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
    use kettle_core::{Column, Line, Point, SelectionType};

    struct Size;
    impl kettle_core::Dimensions for Size {
        fn total_lines(&self) -> usize {
            4
        }
        fn screen_lines(&self) -> usize {
            4
        }
        fn columns(&self) -> usize {
            12
        }
    }
    let (tx, _rx) = crossbeam_channel::unbounded();
    let proxy = kettle_core::EventProxy::new(tx, std::sync::Arc::new(|| {}));
    let mut term =
        alacritty_terminal::Term::new(alacritty_terminal::term::Config::default(), &Size, proxy);
    Processor::<StdSyncHandler>::new().advance(&mut term, b"abcdefgh\r\nijklmnop\r\nqrstuvwx");
    term.toggle_vi_mode();
    let copied = |term: &alacritty_terminal::Term<kettle_core::EventProxy>| {
        kettle_core::selection_text_for_copy(term, 1 << 20, true)
            .unwrap()
            .unwrap_or_default()
    };

    // `V` on row 0, then down a line: both whole lines, each ending in its
    // line break.
    term.vi_goto_point(Point::new(Line(0), Column(2)));
    let cursor = term.vi_mode_cursor.point;
    let mut visual = toggle_vi_visual(&mut term.selection, false, SelectionType::Lines, cursor);
    assert!(visual);
    term.vi_goto_point(Point::new(Line(1), Column(4)));
    assert_eq!(copied(&term), "abcdefgh\nijklmnop\n");

    // Ctrl-v turns the same selection into a block: columns 2..=4 per row.
    let cursor = term.vi_mode_cursor.point;
    visual = toggle_vi_visual(&mut term.selection, visual, SelectionType::Block, cursor);
    assert!(visual);
    assert_eq!(copied(&term), "cde\nklm");
    // The block keeps following the vi cursor.
    term.vi_goto_point(Point::new(Line(2), Column(5)));
    assert_eq!(copied(&term), "cdef\nklmn\nstuv");

    // `v` makes it a character selection between the same two ends.
    let cursor = term.vi_mode_cursor.point;
    visual = toggle_vi_visual(&mut term.selection, visual, SelectionType::Simple, cursor);
    assert_eq!(copied(&term), "cdefgh\nijklmnop\nqrstuv");

    // The key for the kind shown ends visual mode.
    let cursor = term.vi_mode_cursor.point;
    visual = toggle_vi_visual(&mut term.selection, visual, SelectionType::Simple, cursor);
    assert!(!visual);
    assert!(term.selection.is_none());

    // From no selection, Ctrl-v starts a one-cell block at the vi cursor.
    visual = toggle_vi_visual(&mut term.selection, visual, SelectionType::Block, cursor);
    assert!(visual);
    assert_eq!(copied(&term), "v");
    term.vi_goto_point(Point::new(Line(0), Column(4)));
    assert_eq!(copied(&term), "ef\nmn\nuv");
}
