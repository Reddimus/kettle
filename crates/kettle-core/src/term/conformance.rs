use super::*;
use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vi_mode::ViMotion;
use alacritty_terminal::vte::ansi::Processor;

type Rx = crossbeam_channel::Receiver<TermEvent>;

fn harness_rx(cols: usize, rows: usize) -> (Term<EventProxy>, Processor, Rx) {
    let (term, processor, rx, _) = harness_latched(cols, rows);
    (term, processor, rx)
}

/// [`harness_rx`] plus the state events its proxy latched.
fn harness_latched(
    cols: usize,
    rows: usize,
) -> (
    Term<EventProxy>,
    Processor,
    Rx,
    std::sync::Arc<crate::event::LatchedEvents>,
) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let waker: Waker = std::sync::Arc::new(|| {});
    let proxy = EventProxy::new(tx, waker);
    let latched = proxy.latched();
    let term = Term::new(
        TermConfig::default(),
        &TermSize {
            columns: cols,
            screen_lines: rows,
        },
        proxy,
    );
    (term, Processor::new(), rx, latched)
}

/// A one-column grid aborts the whole process on any wide character, and
/// `kettle exec --cols 1`, a narrow enough split, and the `(1, 1, 1, 1)`
/// geometry fallbacks can all request one.
///
/// The engine declares `MIN_COLUMNS = 2` and then never enforces it. On a
/// single-cell row the line-wrap path writes a leading spacer at column 0,
/// wraps, writes the wide char at column 0 of the next row, and then indexes
/// column 1 for the trailing spacer. `Row`'s `Index` is unchecked, so that
/// panics on the PTY reader thread, and `panic = "abort"` in the release
/// profile turns one bad cell into a dead application. A CJK filename in
/// `ls` output is enough.
#[test]
fn a_single_column_request_is_widened_to_what_the_engine_can_represent() {
    let geometry = PtyGeometry::new(1, 1, 1, 1);
    assert_eq!(
        geometry.columns, MIN_COLUMNS,
        "one column is not representable; the clamp has to raise it"
    );

    // PtyGeometry's fields are public and the type is re-exported, so a
    // caller can sidestep the constructor entirely. TermSize::new is the
    // guard that does not depend on how the geometry was built.
    let bypassed = PtyGeometry {
        columns: 1,
        rows: 1,
        pixel_width: 1,
        pixel_height: 1,
    };
    assert_eq!(
        TermSize::new(bypassed.columns, bypassed.rows).columns,
        MIN_COLUMNS
    );

    // Drive the real thing. Without the clamp, these writes panic.
    let (mut term, mut processor) = harness(geometry.columns, geometry.rows);
    feed(&mut term, &mut processor, "\u{754c}".as_bytes());
    feed(&mut term, &mut processor, "\u{1f680}".as_bytes());
    feed(&mut term, &mut processor, "e\u{301}".as_bytes());
}

fn harness(cols: usize, rows: usize) -> (Term<EventProxy>, Processor) {
    let (t, p, _rx) = harness_rx(cols, rows);
    (t, p)
}

fn history_harness(cols: usize, rows: usize, history: usize) -> (Term<EventProxy>, Processor) {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let waker: Waker = std::sync::Arc::new(|| {});
    let proxy = EventProxy::new(tx, waker);
    let config = TermConfig {
        scrolling_history: history,
        ..TermConfig::default()
    };
    let term = Term::new(
        config,
        &TermSize {
            columns: cols,
            screen_lines: rows,
        },
        proxy,
    );
    (term, Processor::new())
}

fn kitty_keyboard_harness(cols: usize, rows: usize) -> (Term<EventProxy>, Processor, Rx) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let waker: Waker = std::sync::Arc::new(|| {});
    let proxy = EventProxy::new(tx, waker);
    let config = TermConfig {
        kitty_keyboard: true,
        ..TermConfig::default()
    };
    let term = Term::new(
        config,
        &TermSize {
            columns: cols,
            screen_lines: rows,
        },
        proxy,
    );
    (term, Processor::new(), rx)
}

/// Concatenate everything the terminal wrote back to the PTY.
fn drain_pty(rx: &Rx) -> String {
    let mut out = String::new();
    while let Ok(ev) = rx.try_recv() {
        if let TermEvent::PtyWrite(s) = ev {
            out.push_str(&s);
        }
    }
    out
}

fn feed(term: &mut Term<EventProxy>, p: &mut Processor, bytes: &[u8]) {
    p.advance(term, bytes);
}

/// Feed bytes through the PTY reader thread's two-stage path,
/// `Extractor::feed` then `Processor::advance` on each `Chunk::Pass`, so a
/// test exercises kettle's real pipeline instead of the alacritty
/// `Processor` alone.
fn feed_ex(term: &mut Term<EventProxy>, p: &mut Processor, ex: &mut Extractor, bytes: &[u8]) {
    for chunk in ex.feed(bytes) {
        if let Chunk::Pass(b) = chunk {
            p.advance(term, &b);
        }
    }
}

fn row_text(term: &Term<EventProxy>, row: i32) -> String {
    let g = term.grid();
    (0..g.columns())
        .map(|c| g[Point::new(Line(row), Column(c))].c)
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn text_newline_and_cursor_addressing() {
    let (mut t, mut p) = harness(20, 5);
    feed(&mut t, &mut p, b"hello\r\nworld");
    assert_eq!(row_text(&t, 0), "hello");
    assert_eq!(row_text(&t, 1), "world");
    // CUP: ESC[3;2H then write — 1-based row/col.
    feed(&mut t, &mut p, b"\x1b[3;2HX");
    assert_eq!(row_text(&t, 2), " X");
}

/// Kettle's vi UI is deliberately backed by alacritty_terminal's native
/// grid coordinates. Pin the property that motivated that integration:
/// output rotation moves the cursor and visual selection with their text,
/// then clears the selection instead of silently aliasing it to unrelated
/// content once the bounded history evicts the selected row.
#[test]
fn vi_cursor_and_selection_follow_scrollback_then_clear_on_eviction() {
    let (mut term, mut processor) = history_harness(24, 3, 4);
    for index in 0..7 {
        feed(
            &mut term,
            &mut processor,
            format!("stable-row-{index:02}\r\n").as_bytes(),
        );
    }
    assert_eq!(term.grid().history_size(), 4);

    term.toggle_vi_mode();
    let start = Point::new(Line(-2), Column(0));
    let selected_row = row_text(&term, start.line.0);
    assert!(
        selected_row.starts_with("stable-row-"),
        "fixture must select a populated history row: {selected_row:?}"
    );
    term.vi_goto_point(start);
    let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
    selection.update(start, Side::Right);
    term.selection = Some(selection);
    term.vi_motion(ViMotion::Last);
    let selected_before = term
        .selection_to_string()
        .expect("visual selection is materialized");
    assert!(selected_before.contains(&selected_row));

    // One output scroll rotates the grid coordinate while preserving the
    // selected content and keeps the vi cursor in the visible scrollback.
    feed(&mut term, &mut processor, b"one-more-row\r\n");
    let selected_after = term
        .selection_to_string()
        .expect("surviving visual selection follows output");
    assert_eq!(selected_after, selected_before);
    let vi_line = term.vi_mode_cursor.point.line.0;
    assert!(
        (-4..=2).contains(&vi_line),
        "vi cursor must stay inside the bounded grid, got {vi_line}"
    );

    // Advance beyond the entire history budget. The engine drops the
    // selection when its anchor rotates out instead of letting it point at
    // a different row.
    for index in 0..8 {
        feed(
            &mut term,
            &mut processor,
            format!("replacement-{index:02}\r\n").as_bytes(),
        );
    }
    assert!(
        term.selection.is_none(),
        "evicted vi selection must be cleared instead of aliasing replacement text"
    );
    let top = -(term.grid().history_size() as i32);
    let vi_line = term.vi_mode_cursor.point.line.0;
    assert!(
        (top..term.grid().screen_lines() as i32).contains(&vi_line),
        "vi cursor must remain in the resized/rotated grid: {vi_line}, top={top}"
    );
}

#[test]
fn vi_cursor_stays_bounded_and_selection_is_invalidated_by_reflow() {
    let (mut term, mut processor) = history_harness(24, 4, 12);
    for index in 0..10 {
        feed(
            &mut term,
            &mut processor,
            format!("reflow-row-{index:02}\r\n").as_bytes(),
        );
    }

    term.toggle_vi_mode();
    let point = Point::new(Line(-3), Column(2));
    term.vi_goto_point(point);
    let mut selection = Selection::new(SelectionType::Simple, point, Side::Left);
    selection.update(point, Side::Right);
    term.selection = Some(selection);
    term.vi_motion(ViMotion::WordRight);
    assert!(term.selection.is_some());

    term.resize(TermSize {
        columns: 13,
        screen_lines: 6,
    });
    assert!(
        term.selection.is_none(),
        "column reflow must invalidate a selection whose endpoints changed shape"
    );
    let top = -(term.grid().history_size() as i32);
    let vi_point = term.vi_mode_cursor.point;
    assert!(
        (top..term.grid().screen_lines() as i32).contains(&vi_point.line.0)
            && vi_point.column.0 < term.grid().columns(),
        "native vi cursor must be clamped after reflow: {vi_point:?}"
    );
}

#[test]
fn kitty_keyboard_stack_caps_and_evicts_its_oldest_mode() {
    let (mut term, mut processor, rx) = kitty_keyboard_harness(20, 5);

    // Overflow must evict the oldest keyboard entry, leaving the unrelated
    // title stack alone even when no title was ever saved.
    feed(&mut term, &mut processor, b"\x1b[>1u");
    for _ in 0..4096 {
        feed(&mut term, &mut processor, b"\x1b[>2u");
    }
    feed(&mut term, &mut processor, b"\x1b[<4095u\x1b[?u");

    assert_eq!(drain_pty(&rx), "\x1b[?2u");
    assert!(term.mode().contains(TermMode::REPORT_EVENT_TYPES));
    assert!(!term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
    feed(&mut term, &mut processor, b"\x1b[<1u\x1b[?u");
    assert_eq!(drain_pty(&rx), "\x1b[?0u");
}

#[test]
fn kitty_keyboard_negotiation_applies_flags_and_is_screen_local() {
    let (mut term, mut processor, rx) = kitty_keyboard_harness(20, 5);

    // Query starts at zero. Replace with disambiguation, union event types,
    // then remove disambiguation using the protocol's 1/2/3 apply modes.
    feed(
        &mut term,
        &mut processor,
        b"\x1b[?u\x1b[=1;1u\x1b[=2;2u\x1b[=1;3u\x1b[?u",
    );
    assert_eq!(drain_pty(&rx), "\x1b[?0u\x1b[?2u");
    assert!(!term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
    assert!(term.mode().contains(TermMode::REPORT_EVENT_TYPES));

    // Push flag 4, then mutate the active stack entry with union mode.
    // Queries must report the resulting flag set rather than stale stack
    // state.
    feed(&mut term, &mut processor, b"\x1b[>4u\x1b[=1;2u\x1b[?u");
    assert_eq!(drain_pty(&rx), "\x1b[?5u");

    // Main and alternate screens have independent keyboard modes and
    // stacks. Main retains flags 1|4 while alternate starts empty,
    // receives flag 8, and is discarded when DECRST 1049 returns to main.
    feed(
        &mut term,
        &mut processor,
        b"\x1b[?1049h\x1b[?u\x1b[>8u\x1b[?u\x1b[?1049l\x1b[?u",
    );
    assert_eq!(drain_pty(&rx), "\x1b[?0u\x1b[?8u\x1b[?5u");
    assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
    assert!(term.mode().contains(TermMode::REPORT_ALTERNATE_KEYS));
    assert!(!term.mode().contains(TermMode::REPORT_ALL_KEYS_AS_ESC));

    // Popping the final entry resets all flags, as required by the
    // protocol, even if a direct mode was active before the first push.
    feed(&mut term, &mut processor, b"\x1b[<u\x1b[?u");
    assert_eq!(drain_pty(&rx), "\x1b[?0u");
    assert!(!term.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
}

/// The widest scroll deltas, which a wheel event sent over the control
/// socket can carry, stop at the ends of history instead of overflowing
/// the offset arithmetic.
#[test]
fn extreme_scroll_deltas_stop_at_the_ends_of_history() {
    use alacritty_terminal::grid::Scroll;
    let (mut t, mut p) = harness(20, 4);
    feed(&mut t, &mut p, b"L0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5");
    let history = t.grid().history_size();
    assert_eq!(history, 2);
    t.scroll_display(Scroll::Delta(1));
    t.scroll_display(Scroll::Delta(i32::MAX));
    assert_eq!(t.grid().display_offset(), history);
    t.scroll_display(Scroll::Delta(i32::MIN));
    assert_eq!(t.grid().display_offset(), 0);
}

/// A block selection whose end lies past the last column clamps onto it,
/// and trimming its edge cells can then leave no column at all. Copying
/// it read a column past the row and aborted.
#[test]
fn a_block_selection_clamped_onto_the_last_column_copies_nothing() {
    use alacritty_terminal::index::Side;
    use alacritty_terminal::selection::{Selection, SelectionType};
    let (mut t, mut p) = harness(80, 10);
    feed(&mut t, &mut p, b"x");
    for side in [Side::Left, Side::Right] {
        let mut selection = Selection::new(
            SelectionType::Block,
            Point::new(Line(5), Column(79)),
            Side::Right,
        );
        selection.update(Point::new(Line(6), Column(81)), side);
        t.selection = Some(selection);
        assert_eq!(t.selection_to_string(), None);
    }
}

/// A selection made while scrolled back must read the VISIBLE (history)
/// row, not the active-screen row at the same viewport index. This guards
/// alacritty's `Selection` coordinate contract, which expects GRID-ABSOLUTE
/// points (viewport − display_offset, via `viewport_to_point`). If kettle-ui
/// stores the raw viewport line, copying while scrolled returns the wrong or
/// empty text. The two branches below show that the raw viewport line and
/// `viewport_to_point` select different rows.
#[test]
fn selection_while_scrolled_reads_visible_row_not_active_screen() {
    use alacritty_terminal::grid::Scroll;
    use alacritty_terminal::index::Side;
    use alacritty_terminal::selection::{Selection, SelectionType};
    use alacritty_terminal::term::viewport_to_point;
    // 4 visible rows; feed 8 lines so the first 4 spill into scrollback.
    let (mut t, mut p) = harness(20, 4);
    feed(
        &mut t,
        &mut p,
        b"L0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6\r\nL7",
    );
    // Bottom: visible rows are L4..L7. Scroll back 3 → visible top = L1.
    t.scroll_display(Scroll::Delta(3));
    let off = t.grid().display_offset();
    assert_eq!(off, 3, "scrolled back 3 lines");

    // FIXED: convert viewport row 0 (showing "L1") to its grid-absolute line.
    let a = viewport_to_point(off, Point::new(0usize, Column(0)));
    let mut s = Selection::new(SelectionType::Lines, a, Side::Left);
    s.update(a, Side::Right);
    t.selection = Some(s);
    let fixed = t.selection_to_string().unwrap_or_default();
    assert!(
        fixed.contains("L1"),
        "fixed reads the visible row: {fixed:?}"
    );
    assert!(
        !fixed.contains("L4"),
        "fixed must not read the active screen: {fixed:?}"
    );

    // BUGGY: the raw viewport line used as absolute reads the active-screen
    // row "L4" instead — the regression this conversion prevents.
    let b = Point::new(Line(0), Column(0));
    let mut s2 = Selection::new(SelectionType::Lines, b, Side::Left);
    s2.update(b, Side::Right);
    t.selection = Some(s2);
    let buggy = t.selection_to_string().unwrap_or_default();
    assert!(buggy.contains("L4"), "buggy reads active screen: {buggy:?}");
    assert_ne!(
        fixed.trim(),
        buggy.trim(),
        "display_offset conversion must change which row is copied"
    );
}

/// The pointer's sub-cell `Side` (which half of a cell the cursor is in) must
/// change which boundary cells a Simple drag includes. kettle's `px_to_cell`
/// computes this side. This pins the alacritty `Selection::to_range`
/// (`range_simple`) contract kettle relies on, so an alacritty bump that
/// changes the trimming fails here instead of silently causing an "off by
/// one letter" selection.
#[test]
fn selection_side_trims_inclusive_range_per_alacritty() {
    use alacritty_terminal::index::Side;
    use alacritty_terminal::selection::{Selection, SelectionType};
    let (mut t, mut p) = harness(20, 2);
    feed(&mut t, &mut p, b"ABCDEFGH");
    let a = Point::new(Line(0), Column(2)); // 'C'
    let b = Point::new(Line(0), Column(5)); // 'F'

    // Anchor on the LEFT of 'C', end on the RIGHT of 'F' → the full inclusive
    // span C..F is copied.
    let mut wide = Selection::new(SelectionType::Simple, a, Side::Left);
    wide.update(b, Side::Right);
    t.selection = Some(wide);
    let wide_s = t.selection_to_string().unwrap_or_default();
    assert_eq!(
        wide_s, "CDEF",
        "Left-anchor/Right-end selects the full span"
    );

    // Anchor on the RIGHT of 'C', end on the LEFT of 'F' → alacritty trims BOTH
    // boundary cells (C..F → D..E). This is exactly the cell-by-cell behavior a
    // drag must reproduce as the pointer crosses each cell's midpoint.
    let mut narrow = Selection::new(SelectionType::Simple, a, Side::Right);
    narrow.update(b, Side::Left);
    t.selection = Some(narrow);
    let narrow_s = t.selection_to_string().unwrap_or_default();
    assert_eq!(
        narrow_s, "DE",
        "Right-anchor/Left-end trims both boundary cells"
    );

    assert!(
        narrow_s.len() < wide_s.len(),
        "the sub-cell side must change which boundary cells are copied"
    );
}

/// `screen_text_of` is the single sanctioned grid scrape behind the
/// control server's `read_screen` and `run_command` output slicing. Pin:
/// document order (history first, then active screen), per-line right-trim,
/// the scrollback request capped by available history, and the reported
/// geometry/cursor.
#[test]
fn screen_text_returns_history_then_screen_with_geometry() {
    use crate::term::screen_text_of;
    use alacritty_terminal::grid::Scroll;
    // 4 visible rows; feed 8 lines so 4 spill into scrollback.
    let (mut t, mut p) = harness(20, 4);
    feed(
        &mut t,
        &mut p,
        b"h0\r\nh1   \r\nh2\r\nh3\r\nv0\r\nv1\r\nv2\r\nv3",
    );
    // No scrollback requested → active screen only, right-trimmed lines.
    let s = screen_text_of(&t, 0);
    assert_eq!((s.cols, s.rows), (20, 4));
    assert_eq!(s.history_size, 4);
    assert_eq!(s.display_offset, 0, "not scrolled");
    assert_eq!(s.text, "v0\nv1\nv2\nv3\n");
    // Cursor sits after "v3" on the last active row.
    assert_eq!(s.cursor, (3, 2));

    // Request more history than exists → clamped to the 4 available, with
    // history in document order BEFORE the active screen, trailing spaces
    // trimmed ("h1   " → "h1").
    let s = screen_text_of(&t, 999);
    assert_eq!(s.text, "h0\nh1\nh2\nh3\nv0\nv1\nv2\nv3\n");

    // A partial request returns only the NEWEST history tail.
    let s = screen_text_of(&t, 2);
    assert_eq!(s.text, "h2\nh3\nv0\nv1\nv2\nv3\n");

    // Default read_screen follows the visible viewport when the user or an
    // agent scrolls back. This is distinct from explicit scrollback capture
    // above, which remains active-screen based for run_command slicing.
    t.scroll_display(Scroll::Delta(3));
    let s = screen_text_of(&t, 0);
    assert_eq!(s.display_offset, 3, "scrolled back three lines");
    assert_eq!(s.text, "h1\nh2\nh3\nv0\n");

    let s = screen_text_of(&t, 2);
    assert_eq!(
        s.text, "h2\nh3\nv0\nv1\nv2\nv3\n",
        "explicit scrollback capture keeps history + active screen semantics"
    );
}

/// Hyperlink/URL detection must scan the VISIBLE viewport, not the active
/// screen, when scrolled back. `grid[Line(row)]` for `row in 0..screen_lines`
/// is always the active (bottom) screen regardless of `display_offset`, so
/// reading it would paint the active screen's link underlines over
/// scrolled-back history ("leftover/ghost underlines from another scroll
/// position"). `links()` reads `Line(row - display_offset)`, matching the
/// decoration/selection `display_offset` conversion.
#[test]
fn links_while_scrolled_read_visible_viewport_not_active_screen() {
    use alacritty_terminal::grid::Scroll;
    // 4 visible rows; feed 8 lines so 4 spill into scrollback. A URL sits in
    // history (L1) and a DIFFERENT URL on the active screen (L5).
    let (mut t, mut p) = harness(40, 4);
    feed(
        &mut t,
        &mut p,
        b"top\r\nx http://hist.test/1 y\r\nl2\r\nl3\r\nl4\r\nz http://active.test/2 w\r\nl6\r\nl7",
    );
    // Bottom (offset 0): visible rows are l4 / active / l6 / l7.
    let bottom = crate::links::links(&t);
    assert!(
        bottom.iter().any(|k| k.uri.contains("active.test")),
        "at the bottom the active-screen URL is the visible one: {bottom:?}"
    );

    // Scroll back 3 → visible top = L1 ("x http://hist.test/1 y").
    t.scroll_display(Scroll::Delta(3));
    assert_eq!(t.grid().display_offset(), 3, "scrolled back 3 lines");
    let scrolled = crate::links::links(&t);
    // The VISIBLE history link is found, at viewport row 0...
    assert!(
        scrolled
            .iter()
            .any(|k| k.uri.contains("hist.test") && k.row == 0),
        "visible history link must be detected at viewport row 0: {scrolled:?}"
    );
    // ...and the now-offscreen active-screen link is NOT reported, so no
    // ghost underline is drawn over history.
    assert!(
        !scrolled.iter().any(|k| k.uri.contains("active.test")),
        "offscreen active-screen URL must not be underlined over history: {scrolled:?}"
    );
}

/// Agent CLIs and editors print project-relative file locations constantly
/// (`crates/foo/src/lib.rs:12:3`). Kettle should make those clickable using
/// the pane cwd while still letting URL detection own URL-shaped text.
#[test]
fn links_with_cwd_detects_file_paths_without_splitting_urls() {
    let (mut t, mut p) = harness(100, 3);
    feed(
            &mut t,
            &mut p,
            b"err crates/kettle-core/src/links.rs:12:3 and https://example.test/a/b\r\nabs /etc/hosts:4",
        );

    let links = crate::links::links_with_cwd(&t, Some("/home/me/kettle"));
    assert!(
        links
            .iter()
            .any(|k| k.uri == "file:///home/me/kettle/crates/kettle-core/src/links.rs"),
        "project-relative file path should resolve against pane cwd: {links:?}"
    );
    assert!(
        links.iter().any(|k| k.uri == "file:///etc/hosts"),
        "absolute file path should become a local file URI: {links:?}"
    );
    let web_links: Vec<_> = links
        .iter()
        .filter(|k| k.uri.contains("example.test"))
        .collect();
    assert_eq!(
        web_links.len(),
        1,
        "path detection must not split a URL into an extra file link: {links:?}"
    );
    assert_eq!(web_links[0].uri, "https://example.test/a/b");
}

/// A real drag-select (Simple selection spanning rows) made while
/// scrolled to the top of history must copy the VISIBLE history rows, not
/// the active screen. Users do exactly this to copy an earlier chunk of a
/// long Claude Code / Codex conversation.
#[test]
fn simple_drag_selection_while_scrolled_copies_visible_rows() {
    use alacritty_terminal::grid::Scroll;
    use alacritty_terminal::index::Side;
    use alacritty_terminal::selection::{Selection, SelectionType};
    use alacritty_terminal::term::viewport_to_point;
    let (mut t, mut p) = harness(12, 3);
    feed(
        &mut t,
        &mut p,
        b"row-A\r\nrow-B\r\nrow-C\r\nrow-D\r\nrow-E\r\nrow-F",
    );
    // active screen = row-D/E/F; history = row-A/B/C. Scroll to the top.
    t.scroll_display(Scroll::Delta(3));
    let off = t.grid().display_offset();
    assert_eq!(off, 3, "scrolled to the top of a 3-line history");
    // Drag from viewport (0,0) down to (1, end): copies "row-A" + "row-B".
    let start = viewport_to_point(off, Point::new(0usize, Column(0)));
    let end = viewport_to_point(off, Point::new(1usize, Column(11)));
    let mut s = Selection::new(SelectionType::Simple, start, Side::Left);
    s.update(end, Side::Right);
    t.selection = Some(s);
    let copied = t.selection_to_string().unwrap_or_default();
    assert!(
        copied.contains("row-A"),
        "copied the visible top rows: {copied:?}"
    );
    assert!(copied.contains("row-B"), "{copied:?}");
    assert!(
        !copied.contains("row-D"),
        "must not read the active screen while scrolled: {copied:?}"
    );
}

/// E2e harness: replay an asciicast v2 trace — the format
/// `record.rs` writes — through the REAL VT pipeline and assert the grid
/// reflects it. This is the `.cast` record→replay regression path: a captured
/// Claude Code / Codex / tmux session can be re-fed deterministically (no
/// PTY, no auth) to guard rendering, selection, and SGR handling.
#[test]
fn replays_asciicast_v2_output_into_grid() {
    // A minimal hand-authored trace (no real session data): plain text, an
    // SGR-bold run, and a CRLF — the shapes a Claude Code frame emits.
    let cast = concat!(
        "{\"version\":2,\"width\":20,\"height\":4}\n",
        "[0.10, \"o\", \"hello \"]\n",
        "[0.20, \"o\", \"\\u001b[1mworld\\u001b[0m\"]\n",
        "[0.30, \"o\", \"\\r\\nsecond line\"]\n",
    );
    let (mut t, mut p) = harness(20, 4);
    for line in cast.lines().skip(1) {
        let v: serde_json::Value = serde_json::from_str(line).expect("event is valid JSON");
        if v[1] == "o" {
            feed(&mut t, &mut p, v[2].as_str().unwrap_or("").as_bytes());
        }
    }
    assert_eq!(row_text(&t, 0), "hello world");
    assert_eq!(row_text(&t, 1), "second line");
    // The SGR bold from the trace applied to "world".
    let g = t.grid();
    assert!(
        g[Point::new(Line(0), Column(6))]
            .flags
            .contains(Flags::BOLD),
        "replayed SGR bold must reach the grid"
    );
}

/// The full record→replay round trip through `kettle_core::record::Recorder`,
/// the recorder behind `kettle exec --record` and the GUI's `--record`.
/// Record output via the real Recorder to a `.cast`, parse it back, replay
/// through the grid, and assert it reconstructs. Pins that the recorder's
/// on-disk format stays replayable (sibling of
/// `replays_asciicast_v2_output_into_grid`).
#[test]
fn recorder_output_round_trips_through_replay() {
    use crate::record::Recorder;
    use std::io::Read;
    let temp = crate::record::test_tempdir();
    let path = temp.path().join("round-trip.cast");
    {
        let mut rec = Recorder::start(&path, 20, 4, false).expect("start recorder");
        rec.record_output(b"hello ");
        rec.record_output(b"\x1b[1mworld\x1b[0m");
        rec.record_output(b"\r\nsecond line");
        rec.finish();
    }
    let mut cast = String::new();
    std::fs::File::open(&path)
        .unwrap()
        .read_to_string(&mut cast)
        .unwrap();
    let (mut t, mut p) = harness(20, 4);
    for line in cast.lines().skip(1) {
        let v: serde_json::Value = serde_json::from_str(line).expect("event is valid JSON");
        if v[1] == "o" {
            feed(&mut t, &mut p, v[2].as_str().unwrap_or("").as_bytes());
        }
    }
    assert_eq!(row_text(&t, 0), "hello world");
    assert_eq!(row_text(&t, 1), "second line");
    assert!(
        t.grid()[Point::new(Line(0), Column(6))]
            .flags
            .contains(Flags::BOLD),
        "recorded+replayed SGR bold must reach the grid"
    );
    let _ = std::fs::remove_file(&path);
}

/// Locks the contract kettle-render relies on to position per-cell bg /
/// underline / strikeout / selection quads. `display_iter` yields
/// GRID-ABSOLUTE lines (negative when scrolled into history), and
/// `viewport_row = grid_line + display_offset` recovers the 0-based viewport
/// row. Using the raw grid-absolute line as the viewport Y detaches
/// decorations from text while scrolled and drops a scrolled-back
/// selection's highlight.
#[test]
fn display_iter_is_grid_absolute_so_render_adds_display_offset() {
    use alacritty_terminal::grid::Scroll;
    let (mut t, mut p) = harness(12, 4);
    feed(
        &mut t,
        &mut p,
        b"A0\r\nA1\r\nA2\r\nA3\r\nA4\r\nA5\r\nA6\r\nA7",
    );
    t.scroll_display(Scroll::Delta(3));
    let off = t.grid().display_offset() as i32;
    assert_eq!(off, 3, "scrolled back 3");
    let first = t
        .grid()
        .display_iter()
        .next()
        .expect("at least one visible cell");
    // The top visible cell is grid-absolute line -off (in history), and
    // grid_line + display_offset == 0 == the pane-top viewport row.
    assert_eq!(
        first.point.line.0, -off,
        "display_iter is grid-absolute (negative in history)"
    );
    assert_eq!(
        first.point.line.0 + off,
        0,
        "grid_line + display_offset = viewport row 0 (what kettle-render must use)"
    );
}

#[test]
fn erase_display_above_on_second_row_clears_first_row() {
    let (mut t, mut p) = harness(10, 3);
    feed(&mut t, &mut p, b"TOP\r\nBOTTOM\r\nKEEP");
    feed(&mut t, &mut p, b"\x1b[2;1H\x1b[1J");
    assert_eq!(row_text(&t, 0), "");
    assert_eq!(row_text(&t, 1), " OTTOM");
    assert_eq!(row_text(&t, 2), "KEEP");
    assert_eq!(t.grid().cursor.point, Point::new(Line(1), Column(0)));
}

#[test]
fn erase_line_and_display() {
    let (mut t, mut p) = harness(10, 3);
    feed(&mut t, &mut p, b"ABCDEFG");
    feed(&mut t, &mut p, b"\x1b[1;4H\x1b[K"); // cursor col4, erase to EOL
    assert_eq!(row_text(&t, 0), "ABC");
    feed(&mut t, &mut p, b"\x1b[2J"); // erase whole display
    assert_eq!(row_text(&t, 0), "");
}

#[test]
fn sgr_truecolor_bold_and_reset() {
    use alacritty_terminal::vte::ansi::Color;
    let (mut t, mut p) = harness(8, 2);
    feed(&mut t, &mut p, b"\x1b[1;38;2;10;20;30mZ\x1b[0mz");
    let g = t.grid();
    let z = &g[Point::new(Line(0), Column(0))];
    assert!(z.flags.contains(Flags::BOLD));
    match z.fg {
        Color::Spec(rgb) => assert_eq!((rgb.r, rgb.g, rgb.b), (10, 20, 30)),
        other => panic!("expected truecolor, got {other:?}"),
    }
    // After SGR reset, the next cell is back to default fg + no bold.
    let z2 = &g[Point::new(Line(0), Column(1))];
    assert!(!z2.flags.contains(Flags::BOLD));
}

#[test]
fn tab_stops_and_carriage_return() {
    let (mut t, mut p) = harness(20, 2);
    feed(&mut t, &mut p, b"a\tb");
    let s = row_text(&t, 0);
    assert_eq!(&s[..1], "a");
    assert_eq!(s.chars().nth(8), Some('b')); // default tab stop at col 8
    feed(&mut t, &mut p, b"\rZ");
    assert_eq!(row_text(&t, 0).chars().next(), Some('Z'));
}

#[test]
fn alt_screen_and_bracketed_paste_modes() {
    let (mut t, mut p) = harness(10, 3);
    feed(&mut t, &mut p, b"\x1b[?1049h");
    assert!(t.mode().contains(TermMode::ALT_SCREEN));
    feed(&mut t, &mut p, b"\x1b[?2004h");
    assert!(t.mode().contains(TermMode::BRACKETED_PASTE));
    feed(&mut t, &mut p, b"\x1b[?1049l");
    assert!(!t.mode().contains(TermMode::ALT_SCREEN));
}

#[test]
fn scroll_region_and_index() {
    let (mut t, mut p) = harness(6, 4);
    // Restrict scrolling to rows 1..=2 (DECSTBM, 1-based), then make it
    // scroll: cursor to row2, newlines push row1 content up within region.
    feed(
        &mut t,
        &mut p,
        b"\x1b[1;2r\x1b[1;1Hone\x1b[2;1Htwo\r\n\r\nthree",
    );
    // Row 3 (outside the region) stays empty; region scrolled.
    assert_eq!(row_text(&t, 3), "");
}

#[test]
fn dec_special_graphics_charset() {
    let (mut t, mut p) = harness(6, 2);
    // ESC ( 0 = DEC line-drawing into G0; q->─ x->│; ESC ( B back to ASCII.
    feed(&mut t, &mut p, b"\x1b(0qx\x1b(By");
    let s = row_text(&t, 0);
    let cs: Vec<char> = s.chars().collect();
    assert_eq!(cs[0], '\u{2500}', "q -> light horizontal");
    assert_eq!(cs[1], '\u{2502}', "x -> light vertical");
    assert_eq!(cs[2], 'y', "ASCII restored after ESC(B");
}

#[test]
fn insert_and_delete_char() {
    let (mut t, mut p) = harness(8, 2);
    feed(&mut t, &mut p, b"abcde");
    // Cursor to col 2 ('b'), delete it (DCH).
    feed(&mut t, &mut p, b"\x1b[1;2H\x1b[P");
    assert_eq!(row_text(&t, 0), "acde");
    // Insert a blank at col 1 (ICH), then type at it.
    feed(&mut t, &mut p, b"\x1b[1;1H\x1b[@Z");
    assert_eq!(row_text(&t, 0), "Zacde");
}

/// DCH deletes from the cursor to the end of the line at most, whatever
/// its count: the cells before the cursor stay.
#[test]
fn delete_char_never_clears_cells_before_the_cursor() {
    for (column, expected) in [(4, "ABC"), (8, "ABCDEFG")] {
        let remaining = 9 - column;
        for count in [remaining, remaining + 1, 65535] {
            let (mut t, mut p) = harness(8, 2);
            feed(&mut t, &mut p, b"ABCDEFGH");
            feed(
                &mut t,
                &mut p,
                format!("\x1b[1;{column}H\x1b[{count}P").as_bytes(),
            );
            assert_eq!(row_text(&t, 0), expected, "column {column}, count {count}");
        }
    }
}

#[test]
fn insert_and_delete_line() {
    let (mut t, mut p) = harness(6, 4);
    feed(&mut t, &mut p, b"r0\r\nr1\r\nr2");
    // Delete line 2 (DL): r2 moves up into row 1.
    feed(&mut t, &mut p, b"\x1b[2;1H\x1b[M");
    assert_eq!(row_text(&t, 0), "r0");
    assert_eq!(row_text(&t, 1), "r2");
    // Insert a line at row 1 (IL): pushes r2 back down.
    feed(&mut t, &mut p, b"\x1b[2;1H\x1b[L");
    assert_eq!(row_text(&t, 1), "");
    assert_eq!(row_text(&t, 2), "r2");
}

#[test]
fn save_restore_cursor_and_autowrap() {
    let (mut t, mut p) = harness(3, 3);
    // DECSC at row1col1, move away, write, DECRC back, overwrite.
    feed(&mut t, &mut p, b"\x1b7\x1b[3;3HX\x1b8A");
    assert_eq!(row_text(&t, 0).chars().next(), Some('A'));
    // Autowrap (DECAWM, on by default): 4 chars into 3 columns wraps.
    let (mut t2, mut p2) = harness(3, 3);
    feed(&mut t2, &mut p2, b"abcd");
    assert_eq!(row_text(&t2, 0), "abc");
    assert_eq!(row_text(&t2, 1), "d");
}

#[test]
fn origin_mode_relative_moves_do_not_reapply_top_margin() {
    let (mut t, mut p) = harness(10, 24);
    feed(&mut t, &mut p, b"\x1b[5;20r\x1b[?6h");
    assert_eq!(t.grid().cursor.point, Point::new(Line(4), Column(0)));

    let cases: &[(&[u8], i32, usize)] = &[
        (b"\x1b[2G", 4, 1),
        (b"\x1b[2B", 6, 1),
        (b"\x1b[A", 5, 1),
        (b"\x1b[2E", 7, 0),
        (b"\x1b[F", 6, 0),
        (b"\x1b[999A", 4, 0),
        (b"\x1b[999B", 19, 0),
        (b"\x1b[999E", 19, 0),
        (b"\x1b[999F", 4, 0),
        (b"\x1b[999G", 4, 9),
    ];
    for &(sequence, line, column) in cases {
        feed(&mut t, &mut p, sequence);
        assert_eq!(
            t.grid().cursor.point,
            Point::new(Line(line), Column(column))
        );
    }

    feed(&mut t, &mut p, b"\x1b[?6l\x1b[3;3H\x1b[999A");
    assert_eq!(t.grid().cursor.point, Point::new(Line(0), Column(2)));
    // From above the region, CUD stops at the bottom margin, as in xterm.
    feed(&mut t, &mut p, b"\x1b[999B");
    assert_eq!(t.grid().cursor.point, Point::new(Line(19), Column(2)));

    // DECRC restores DECOM with the cursor: saved outside origin mode, the
    // cursor comes back above the margin with origin mode off again.
    feed(&mut t, &mut p, b"\x1b[2;1H\x1b7\x1b[?6h\x1b8");
    assert!(!t.mode().contains(TermMode::ORIGIN));
    assert_eq!(t.grid().cursor.point, Point::new(Line(1), Column(0)));
    feed(&mut t, &mut p, b"\x1b[A");
    assert_eq!(t.grid().cursor.point, Point::new(Line(0), Column(0)));
}

#[test]
fn decsc_and_scosc_save_and_restore_origin_mode() {
    for (save, restore) in [(&b"\x1b7"[..], &b"\x1b8"[..]), (b"\x1b[s", b"\x1b[u")] {
        let (mut t, mut p) = harness(10, 24);
        feed(&mut t, &mut p, b"\x1b[5;20r");
        // Saved with origin mode on, restored after it was turned off.
        feed(&mut t, &mut p, b"\x1b[?6h\x1b[3;2H");
        feed(&mut t, &mut p, save);
        feed(&mut t, &mut p, b"\x1b[?6l\x1b[1;1H");
        feed(&mut t, &mut p, restore);
        assert!(t.mode().contains(TermMode::ORIGIN));
        assert_eq!(t.grid().cursor.point, Point::new(Line(6), Column(1)));
        // Saved with origin mode off, restored after it was turned on.
        feed(&mut t, &mut p, b"\x1b[?6l\x1b[2;3H");
        feed(&mut t, &mut p, save);
        feed(&mut t, &mut p, b"\x1b[?6h");
        feed(&mut t, &mut p, restore);
        assert!(!t.mode().contains(TermMode::ORIGIN));
        assert_eq!(t.grid().cursor.point, Point::new(Line(1), Column(2)));
    }
}

#[test]
fn mode_1049_saves_origin_mode_for_a_later_decrc() {
    let (mut t, mut p) = harness(10, 24);
    // Entering the alternate screen saves the cursor as DECSC does, so a
    // DECRC after leaving it restores origin mode as it was on entry.
    feed(
        &mut t,
        &mut p,
        b"\x1b[5;20r\x1b[?6h\x1b[3;2H\x1b[?1049h\x1b[?1049l\x1b8",
    );
    assert!(t.mode().contains(TermMode::ORIGIN));
    assert_eq!(t.grid().cursor.point, Point::new(Line(6), Column(1)));
    // Leaving it restores origin mode too, so a program that turns origin
    // mode on in the alternate screen does not leave it on for the shell.
    feed(&mut t, &mut p, b"\x1b[?6l\x1b[?1049h\x1b[?6h\x1b[?1049l");
    assert!(!t.mode().contains(TermMode::ORIGIN));
    // Restoring origin mode on the way out keeps the cursor inside the
    // margins as they are then, as DECRC does.
    feed(
        &mut t,
        &mut p,
        b"\x1b[5;20r\x1b[?6h\x1b[?1049h\x1b[?6l\x1b[10;20r\x1b[?1049l",
    );
    assert!(t.mode().contains(TermMode::ORIGIN));
    assert_eq!(t.grid().cursor.point, Point::new(Line(9), Column(0)));
}

#[test]
fn a_scroll_region_past_the_screen_is_clipped_or_refused() {
    let (mut t, mut p) = harness(10, 24);
    // Both margins past the last line leave nothing once clipped, so the
    // region is refused, as xterm refuses it, and the old one stays.
    feed(&mut t, &mut p, b"\x1b[5;20r\x1b[25;26r\x1b[10;1H\x1b[99B");
    assert_eq!(t.grid().cursor.point.line, Line(19));
    // A bottom margin past the screen is clipped to its last line.
    feed(&mut t, &mut p, b"\x1b[20;30r\x1b[22;1H\x1b[99A");
    assert_eq!(t.grid().cursor.point.line, Line(19));
    // Restoring an origin-mode cursor after such a region, through DECRC
    // and by leaving 1049, keeps it on the screen.
    feed(&mut t, &mut p, b"\x1b[r\x1b[?6h\x1b7\x1b[25;26r\x1b8");
    feed(&mut t, &mut p, b"\x1b[?1049h\x1b[?6l\x1b[25;26r\x1b[?1049l");
    assert!(t.mode().contains(TermMode::ORIGIN));
    assert!(t.grid().cursor.point.line < Line(24));
}

#[test]
fn decrc_keeps_an_origin_mode_cursor_inside_new_margins() {
    let (mut t, mut p) = harness(10, 24);
    // Saved at the top of rows 5..=20 under origin mode, then the margins
    // move down to rows 10..=20: the restored row is clamped into them.
    feed(
        &mut t,
        &mut p,
        b"\x1b[5;20r\x1b[?6h\x1b[1;1H\x1b7\x1b[10;20r\x1b8",
    );
    assert!(t.mode().contains(TermMode::ORIGIN));
    assert_eq!(t.grid().cursor.point, Point::new(Line(9), Column(0)));
}

#[test]
fn private_mode_1048_saves_and_restores_the_cursor() {
    let (mut t, mut p) = harness(10, 8);
    feed(&mut t, &mut p, b"\x1b[3;4H\x1b[?1048h\x1b[6;7H\x1b[?1048l");
    assert_eq!(t.grid().cursor.point, Point::new(Line(2), Column(3)));
    // It shares DECSC's saved state, origin mode included.
    feed(&mut t, &mut p, b"\x1b[5;8r\x1b[?6h\x1b[?1048h\x1b[?6l\x1b8");
    assert!(t.mode().contains(TermMode::ORIGIN));
    assert_eq!(t.grid().cursor.point, Point::new(Line(4), Column(0)));
}

#[test]
fn linefeed_newline_mode_returns_to_the_first_column() {
    let (mut t, mut p) = harness(10, 8);
    // With LNM set, LF, VT and FF each add a carriage return.
    feed(&mut t, &mut p, b"\x1b[20h");
    for (line, control) in [b'\n', 0x0b, 0x0c].into_iter().enumerate() {
        feed(&mut t, &mut p, b"ab");
        feed(&mut t, &mut p, &[control]);
        assert_eq!(
            t.grid().cursor.point,
            Point::new(Line(line as i32 + 1), Column(0)),
            "control {control:#04x}"
        );
    }
    // IND is a pure index under LNM, and LF is one again once LNM is reset.
    feed(&mut t, &mut p, b"ab\x1bD");
    assert_eq!(t.grid().cursor.point, Point::new(Line(4), Column(2)));
    feed(&mut t, &mut p, b"\x1b[20l\n");
    assert_eq!(t.grid().cursor.point, Point::new(Line(5), Column(2)));
}

#[test]
fn relative_moves_stop_at_margins_without_origin_mode() {
    let (mut t, mut p) = harness(10, 24);
    // Rows 5..=20 scroll; origin mode stays off.
    feed(&mut t, &mut p, b"\x1b[5;20r");
    let cases: &[(&[u8], i32)] = &[
        // From inside the region the margins stop CUU, CUD, CPL and CNL.
        (b"\x1b[10;3H\x1b[99A", 4),
        (b"\x1b[10;3H\x1b[99B", 19),
        (b"\x1b[10;3H\x1b[99F", 4),
        (b"\x1b[10;3H\x1b[99E", 19),
        // From above the region CUU reaches the top line, and CUD stops at
        // the bottom margin.
        (b"\x1b[3;3H\x1b[99A", 0),
        (b"\x1b[3;3H\x1b[99B", 19),
        // From below the region CUD reaches the bottom line, and CUU stops
        // at the top margin.
        (b"\x1b[22;3H\x1b[99B", 23),
        (b"\x1b[22;3H\x1b[99A", 4),
    ];
    for &(sequence, line) in cases {
        feed(&mut t, &mut p, sequence);
        assert_eq!(
            t.grid().cursor.point.line,
            Line(line),
            "{}",
            String::from_utf8_lossy(sequence).escape_debug()
        );
    }
}

#[test]
fn decaln_resets_margins_and_homes_cursor() {
    let (mut t, mut p) = harness(10, 8);
    feed(&mut t, &mut p, b"\x1b[3;5r\x1b[4;4H\x1b#8");
    assert_eq!(t.grid().cursor.point, Point::new(Line(0), Column(0)));
    assert_eq!(row_text(&t, 7), "EEEEEEEEEE");
    // The region is the whole screen again, so CUD runs to the last line.
    feed(&mut t, &mut p, b"\x1b[4;4H\x1b[99B");
    assert_eq!(t.grid().cursor.point.line, Line(7));
}

#[test]
fn origin_mode_addresses_within_margins() {
    let (mut t, mut p) = harness(6, 5);
    // Scroll region rows 2..=4, enable origin mode, then home (1;1) is
    // the region top (absolute row index 1).
    feed(&mut t, &mut p, b"\x1b[2;4r\x1b[?6h\x1b[1;1HO");
    assert_eq!(row_text(&t, 0), "", "row 0 is above the margin");
    assert_eq!(row_text(&t, 1), "O", "origin-mode home = top margin");
}

#[test]
fn dsr_cursor_position_report() {
    let (mut t, mut p, rx) = harness_rx(40, 10);
    // Move to row 3, col 5 (1-based), then DSR 6n.
    feed(&mut t, &mut p, b"\x1b[3;5H\x1b[6n");
    let reply = drain_pty(&rx);
    assert_eq!(reply, "\x1b[3;5R", "CPR must echo the 1-based cursor");
}

#[test]
fn device_attributes_reply() {
    let (mut t, mut p, rx) = harness_rx(10, 3);
    feed(&mut t, &mut p, b"\x1b[c"); // Primary DA
    let reply = drain_pty(&rx);
    assert!(
        reply.starts_with("\x1b[?"),
        "DA1 reply should be a CSI ? … c, got {reply:?}"
    );
    assert!(reply.ends_with('c'));
}

#[test]
fn sgr_underline_dim_strike() {
    let (mut t, mut p) = harness(8, 2);
    // dim + single underline + strikeout, then a curly underline cell.
    feed(&mut t, &mut p, b"\x1b[2;4;9mA\x1b[0m\x1b[4:3mB");
    let g = t.grid();
    let a = &g[Point::new(Line(0), Column(0))];
    assert!(a.flags.contains(Flags::DIM));
    assert!(a.flags.contains(Flags::UNDERLINE));
    assert!(a.flags.contains(Flags::STRIKEOUT));
    let b = &g[Point::new(Line(0), Column(1))];
    assert!(
        b.flags.contains(Flags::UNDERCURL),
        "SGR 4:3 = curly underline"
    );
    assert!(!b.flags.contains(Flags::DIM), "SGR 0 reset cleared dim");
}

#[test]
fn decaln_fills_screen_with_e() {
    let (mut t, mut p) = harness(4, 3);
    feed(&mut t, &mut p, b"\x1b#8"); // DEC screen alignment test
    for r in 0..3 {
        assert_eq!(row_text(&t, r), "EEEE", "DECALN fills row {r}");
    }
}

#[test]
fn rep_repeats_last_graphic_char() {
    let (mut t, mut p) = harness(8, 2);
    // 'A' then REP 3 -> "AAAA".
    feed(&mut t, &mut p, b"A\x1b[3b");
    assert_eq!(row_text(&t, 0), "AAAA");
}

#[test]
fn charset_g1_via_so_si() {
    let (mut t, mut p) = harness(6, 2);
    // Designate DEC special graphics into G1, SO -> G1, SI -> back to G0.
    feed(&mut t, &mut p, b"\x1b)0\x0eqx\x0fy");
    let cs: Vec<char> = row_text(&t, 0).chars().collect();
    assert_eq!(cs[0], '\u{2500}', "G1 q -> horizontal line");
    assert_eq!(cs[1], '\u{2502}', "G1 x -> vertical line");
    assert_eq!(cs[2], 'y', "SI returned to ASCII G0");
}

#[test]
fn ris_full_reset_clears_origin_mode() {
    let (mut t, mut p) = harness(6, 4);
    // Origin mode on + scroll region, then RIS (ESC c) — a full reset —
    // so 1;1 is absolute home again.
    feed(&mut t, &mut p, b"\x1b[2;4r\x1b[?6hzz\x1bc\x1b[1;1HX");
    assert_eq!(row_text(&t, 0), "X", "RIS cleared origin mode + region");
}

#[test]
fn el_erase_to_left() {
    let (mut t, mut p) = harness(6, 2);
    feed(&mut t, &mut p, b"ABCDE");
    // Cursor to col 3 (1-based), EL 1 = erase start..=cursor.
    feed(&mut t, &mut p, b"\x1b[1;3H\x1b[1K");
    // cols 0..=2 cleared; "DE" remains at cols 3,4.
    assert_eq!(row_text(&t, 0), "   DE");
}

#[test]
fn ed_erase_below() {
    let (mut t, mut p) = harness(4, 3);
    feed(&mut t, &mut p, b"r0\r\nr1\r\nr2");
    // Cursor to row 2 col 1 (1-based), ED 0 = erase cursor..=end.
    feed(&mut t, &mut p, b"\x1b[2;1H\x1b[0J");
    assert_eq!(row_text(&t, 0), "r0", "row above the cursor kept");
    assert_eq!(row_text(&t, 1), "", "cursor row erased");
    assert_eq!(row_text(&t, 2), "", "rows below erased");
}

#[test]
fn da2_secondary_device_attributes() {
    let (mut t, mut p, rx) = harness_rx(10, 3);
    feed(&mut t, &mut p, b"\x1b[>c"); // Secondary DA
    let reply = drain_pty(&rx);
    assert!(
        reply.starts_with("\x1b[>") && reply.ends_with('c'),
        "DA2 reply should be CSI > … c, got {reply:?}"
    );
}

#[test]
fn ech_erases_in_place() {
    let (mut t, mut p) = harness(6, 2);
    feed(&mut t, &mut p, b"ABCDE");
    // Cursor col 2 (1-based), ECH 2 clears 2 cells, cursor unmoved.
    feed(&mut t, &mut p, b"\x1b[1;2H\x1b[2X");
    assert_eq!(row_text(&t, 0), "A  DE");
}

#[test]
fn ich_shifts_right_off_edge() {
    let (mut t, mut p) = harness(5, 2);
    feed(&mut t, &mut p, b"abcde");
    // ICH 2 at home pushes cells right; the line is 5 wide so d,e fall off.
    feed(&mut t, &mut p, b"\x1b[1;1H\x1b[2@");
    assert_eq!(row_text(&t, 0), "  abc");
}

#[test]
fn absolute_cursor_moves_cha_hpa_vpa() {
    let (mut t, mut p) = harness(6, 4);
    // CHA: column-absolute to col 3 then write.
    feed(&mut t, &mut p, b"abcde\x1b[3GZ");
    assert_eq!(row_text(&t, 0), "abZde");
    // HPA (ESC[`) col 2 on row 1, VPA (ESC[d) row 3.
    feed(&mut t, &mut p, b"\x1b[1;1H\x1b[2`Q\x1b[3dW");
    assert_eq!(row_text(&t, 0).chars().nth(1), Some('Q'), "HPA col 2");
    // VPA changes the row only; column stays where it was (col 3 after Q).
    assert_eq!(row_text(&t, 2).chars().nth(2), Some('W'), "VPA row 3");
}

#[test]
fn decsc_restores_sgr_attributes() {
    let (mut t, mut p) = harness(6, 3);
    // Bold on, DECSC (saves cursor + pen), reset SGR + move away,
    // DECRC restores both, then write — must be bold at the saved cell.
    feed(&mut t, &mut p, b"\x1b[1m\x1b7\x1b[0m\x1b[3;4HZ\x1b8A");
    let a = &t.grid()[Point::new(Line(0), Column(0))];
    assert_eq!(a.c, 'A');
    assert!(
        a.flags.contains(Flags::BOLD),
        "DECRC must restore the saved SGR pen"
    );
}

/// Synchronized output (DEC private mode 2026 / BSU·ESU). While a
/// sync block is open the engine MUST buffer mutations so a renderer that
/// locks the grid never samples a half-drawn frame; the buffered changes
/// apply atomically on close. This lets well-behaved TUIs repaint without
/// tearing. The bytes go through kettle's REAL pipeline (`feed_ex` →
/// Extractor → Processor), so this also guards that a future `Extractor`
/// change cannot swallow the `?2026` toggles.
#[test]
fn synchronized_update_defers_grid_mutation_until_close() {
    let (mut t, mut p) = harness(6, 2);
    let mut ex = Extractor::new();
    feed_ex(&mut t, &mut p, &mut ex, b"A");
    assert_eq!(t.grid()[Point::new(Line(0), Column(0))].c, 'A');
    // Open a synchronized update, return to col 0 and overwrite with 'B',
    // but DO NOT close the block yet.
    feed_ex(&mut t, &mut p, &mut ex, b"\x1b[?2026h\rB");
    assert_eq!(
        t.grid()[Point::new(Line(0), Column(0))].c,
        'A',
        "grid mutated mid-synchronized-update (mode 2026 not honored)"
    );
    // Close the block — the buffered write now applies atomically.
    feed_ex(&mut t, &mut p, &mut ex, b"\x1b[?2026l");
    assert_eq!(
        t.grid()[Point::new(Line(0), Column(0))].c,
        'B',
        "synchronized update not flushed on close"
    );
}

#[test]
fn su_sd_scroll_up_and_down() {
    let (mut t, mut p) = harness(4, 3);
    feed(&mut t, &mut p, b"r0\r\nr1\r\nr2");
    feed(&mut t, &mut p, b"\x1b[1S"); // SU 1: content moves up
    assert_eq!(row_text(&t, 0), "r1");
    assert_eq!(row_text(&t, 1), "r2");
    assert_eq!(row_text(&t, 2), "");

    let (mut t2, mut p2) = harness(4, 3);
    feed(&mut t2, &mut p2, b"a\r\nb\r\nc");
    feed(&mut t2, &mut p2, b"\x1b[1T"); // SD 1: content moves down
    assert_eq!(row_text(&t2, 0), "");
    assert_eq!(row_text(&t2, 1), "a");
    assert_eq!(row_text(&t2, 2), "b");
}

#[test]
fn decscusr_sets_cursor_shape() {
    use alacritty_terminal::vte::ansi::CursorShape;
    let (mut t, mut p) = harness(6, 2);
    feed(&mut t, &mut p, b"\x1b[3 q"); // DECSCUSR 3 = (blinking) underline
    assert_eq!(t.renderable_content().cursor.shape, CursorShape::Underline);
    feed(&mut t, &mut p, b"\x1b[5 q"); // 5 = (blinking) bar/beam
    assert_eq!(t.renderable_content().cursor.shape, CursorShape::Beam);
    feed(&mut t, &mut p, b"\x1b[1 q"); // 1 = (blinking) block
    assert_eq!(t.renderable_content().cursor.shape, CursorShape::Block);
}

#[test]
fn dec_mode_25_hide_collapses_renderable_cursor_to_hidden() {
    // Cursor visibility (DEC ?25) and cursor shape (DECSCUSR `q`) are
    // tracked in different places in the engine; `RenderableContent`
    // *folds* them so the renderer only has to look at one field. This
    // test pins that contract — what the renderer reads is `Hidden` the
    // moment a program clears ?25, even if the shape was set to
    // something else first. Otherwise we'd silently keep drawing a
    // cursor at TUI apps that asked us not to (less, fzf full-screen).
    use alacritty_terminal::vte::ansi::CursorShape;
    let (mut t, mut p) = harness(6, 2);
    feed(&mut t, &mut p, b"\x1b[1 q"); // shape = block (visible default)
    assert_eq!(t.renderable_content().cursor.shape, CursorShape::Block);
    feed(&mut t, &mut p, b"\x1b[?25l"); // ?25 cleared = hide cursor
    assert_eq!(
        t.renderable_content().cursor.shape,
        CursorShape::Hidden,
        "DEC ?25 l must collapse the renderable cursor to Hidden"
    );
    feed(&mut t, &mut p, b"\x1b[?25h"); // ?25 set = show again
    assert_eq!(
        t.renderable_content().cursor.shape,
        CursorShape::Block,
        "DEC ?25 h restores the previous shape"
    );
}

#[test]
fn partial_erases_clear_both_halves_of_wide_characters() {
    let sequences: &[&[u8]] = &[
        b"\r\x1b[X",
        b"\x1b[2G\x1b[X",
        b"\r\x1b[1K",
        b"\x1b[2G\x1b[K",
        b"\r\x1b[1J",
        b"\x1b[2G\x1b[J",
    ];
    for &sequence in sequences {
        let (mut t, mut p) = harness(4, 2);
        feed(&mut t, &mut p, "\u{4e16}".as_bytes());
        feed(&mut t, &mut p, sequence);
        for column in 0..2 {
            let cell = &t.grid()[Point::new(Line(0), Column(column))];
            assert_eq!(cell.c, ' ');
            assert!(
                !cell
                    .flags
                    .intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER)
            );
        }
        feed(&mut t, &mut p, b"\rab");
        assert_eq!(row_text(&t, 0), "ab");
    }
}

#[test]
fn wide_cjk_char_occupies_two_cells() {
    let (mut t, mut p) = harness(8, 2);
    feed(&mut t, &mut p, "世A".as_bytes());
    let g = t.grid();
    let c0 = &g[Point::new(Line(0), Column(0))];
    assert_eq!(c0.c, '世');
    assert!(c0.flags.contains(Flags::WIDE_CHAR), "CJK = wide");
    assert!(
        g[Point::new(Line(0), Column(1))]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER),
        "second cell is the wide spacer"
    );
    assert_eq!(g[Point::new(Line(0), Column(2))].c, 'A');
}

#[test]
fn wide_char_wraps_when_it_does_not_fit() {
    let (mut t, mut p) = harness(3, 3);
    // 2 narrow + 1 wide: the wide char can't fit in the last column,
    // so it wraps to the next row.
    feed(&mut t, &mut p, "ab世".as_bytes());
    assert_eq!(row_text(&t, 0), "ab");
    assert_eq!(t.grid()[Point::new(Line(1), Column(0))].c, '世');
}

#[test]
fn combining_mark_is_zero_width() {
    let (mut t, mut p) = harness(6, 2);
    // 'e' + combining acute accent: one cell, mark stored as zerowidth.
    feed(&mut t, &mut p, "e\u{0301}X".as_bytes());
    let g = t.grid();
    let base = &g[Point::new(Line(0), Column(0))];
    assert_eq!(base.c, 'e');
    assert_eq!(
        base.zerowidth(),
        Some(&['\u{0301}'][..]),
        "combining mark attaches to the base cell"
    );
    assert_eq!(
        g[Point::new(Line(0), Column(1))].c,
        'X',
        "next glyph is in the very next cell (mark took no column)"
    );
}

#[test]
fn osc4_palette_query_emits_color_request() {
    let (mut t, mut p, rx) = harness_rx(8, 2);
    // Query palette entry 1.
    feed(&mut t, &mut p, b"\x1b]4;1;?\x07");
    let got_idx = rx.try_iter().find_map(|ev| match ev {
        TermEvent::ColorRequest(idx, _) => Some(idx),
        _ => None,
    });
    assert_eq!(got_idx, Some(1), "OSC 4 ; 1 ; ? requests palette index 1");
}

#[test]
fn sgr_underline_style_variants_set_distinct_flags() {
    // Five style bits in the engine: `\e[4m` (single), `\e[21m` or
    // `\e[4:2m` (double), `\e[4:3m` (curl), `\e[4:4m` (dotted),
    // `\e[4:5m` (dashed). The renderer reads these and
    // draws differently per style — this pins each one reaching the
    // engine's cell flags so a future engine bump can't silently
    // drop a variant.
    let (mut t, mut p) = harness(20, 2);
    feed(
        &mut t,
        &mut p,
        b"\x1b[4ma\x1b[4:2mb\x1b[4:3mc\x1b[4:4md\x1b[4:5me",
    );
    let g = t.grid();
    let f = |c: usize| g[Point::new(Line(0), Column(c))].flags;
    assert!(f(0).contains(Flags::UNDERLINE), "[4m → UNDERLINE on `a`");
    assert!(
        f(1).contains(Flags::DOUBLE_UNDERLINE),
        "[4:2m → DOUBLE_UNDERLINE on `b`"
    );
    assert!(f(2).contains(Flags::UNDERCURL), "[4:3m → UNDERCURL on `c`");
    assert!(
        f(3).contains(Flags::DOTTED_UNDERLINE),
        "[4:4m → DOTTED_UNDERLINE on `d`"
    );
    assert!(
        f(4).contains(Flags::DASHED_UNDERLINE),
        "[4:5m → DASHED_UNDERLINE on `e`"
    );
    // Each variant is mutually-exclusive: setting DOUBLE clears the
    // previous UNDERLINE bit (alacritty single-underline-flag model).
    // Confirm by checking `b` doesn't still carry plain UNDERLINE.
    assert!(
        !f(1).contains(Flags::UNDERLINE),
        "[4:2m must clear plain UNDERLINE"
    );
}

#[test]
fn sgr_58_sets_per_cell_underline_color() {
    // Neovim spell-check / git diff / lsp diagnostics emit per-cell
    // underline color via SGR 58. The engine stores it on the cell;
    // the renderer reads it so the squiggle color follows
    // the request instead of using the text fg. Confirms truecolor
    // form `\e[58;2;r;g;bm` reaches `cell.underline_color()`.
    use alacritty_terminal::vte::ansi::{Color as AnsiColor, Rgb as AnsiRgb};
    let (mut t, mut p) = harness(8, 2);
    // Underline on + red underline color, then write a glyph.
    feed(&mut t, &mut p, b"\x1b[4m\x1b[58;2;200;30;30mX");
    let grid = t.grid();
    let cell = &grid[Point::new(Line(0), Column(0))];
    assert_eq!(cell.c, 'X');
    assert!(
        cell.flags.contains(Flags::UNDERLINE),
        "SGR 4 must set UNDERLINE"
    );
    assert_eq!(
        cell.underline_color(),
        Some(AnsiColor::Spec(AnsiRgb {
            r: 200,
            g: 30,
            b: 30
        })),
        "SGR 58 must store the per-cell underline color"
    );
    // SGR 59 resets to default (None), leaving UNDERLINE intact.
    feed(&mut t, &mut p, b"\x1b[59mY");
    let cell2 = &t.grid()[Point::new(Line(0), Column(1))];
    assert_eq!(cell2.c, 'Y');
    assert!(cell2.flags.contains(Flags::UNDERLINE));
    assert_eq!(
        cell2.underline_color(),
        None,
        "SGR 59 must clear the per-cell underline color"
    );
}

#[test]
fn osc4_multi_index_query_emits_one_request_per_index() {
    // vte's OSC 4 handler chunks the params in pairs (`;idx;val`), so
    // a single `OSC 4 ; 1 ; ? ; 7 ; ?` should ask for *two* colors in
    // one go. tmux, neovim 0.10+ and base16-shell-hook all batch
    // palette probes this way — without per-pair dispatch they'd see
    // only the first reply and assume the rest of the palette equals
    // the engine default, breaking the dark/light detection they rely
    // on.
    let (mut t, mut p, rx) = harness_rx(8, 2);
    feed(&mut t, &mut p, b"\x1b]4;1;?;7;?\x07");
    let mut indices: Vec<usize> = rx
        .try_iter()
        .filter_map(|ev| match ev {
            TermEvent::ColorRequest(idx, _) => Some(idx),
            _ => None,
        })
        .collect();
    indices.sort_unstable();
    assert_eq!(
        indices,
        vec![1, 7],
        "multi-index OSC 4 must fire one ColorRequest per `;idx;?` pair"
    );
}

#[test]
fn osc_10_11_12_set_populate_default_color_slots() {
    // OSC 10/11/12 SET should populate the engine's `Colors[256..=258]`
    // slots (default fg, default bg, cursor) so the renderer's
    // `resolve_query` reflects the override on the next frame. Without
    // this round-trip, the render path silently drops OSC 12 (set cursor
    // color). `osc_color_set_query_reset_round_trip_through_engine` covers
    // OSC 4 set; OSC 10/11/12 are the close siblings that use the same
    // Colors slots.
    for (input, idx) in &[
        (b"\x1b]10;rgb:11/22/33\x07" as &[u8], 256usize),
        (b"\x1b]11;rgb:44/55/66\x07", 257),
        (b"\x1b]12;rgb:77/88/99\x07", 258),
    ] {
        let (mut t, mut p, _rx) = harness_rx(8, 2);
        assert!(t.colors()[*idx].is_none(), "slot {idx} clean pre-set");
        feed(&mut t, &mut p, input);
        let c = t.colors()[*idx].unwrap_or_else(|| panic!("slot {idx} unset after {input:?}"));
        // The exact values from the xparsecolor input (engine packs
        // each `RR` byte pair into a single u8).
        let want = match idx {
            256 => (0x11, 0x22, 0x33),
            257 => (0x44, 0x55, 0x66),
            258 => (0x77, 0x88, 0x99),
            _ => unreachable!(),
        };
        assert_eq!((c.r, c.g, c.b), want, "wrong color for slot {idx}");
    }
}

#[test]
fn osc_104_no_params_resets_all_256_palette_slots() {
    // OSC 104 with no parameters (just `\e]104\a` or `\e]104;\a`)
    // resets *every* palette index (0..256), not just one. xterm
    // documents this: "OSC 104 ; c → reset color number c (default
    // restore palette)." Tools like `colorls`/`zsh-colorize`'s
    // theme-changers emit it to undo their session-wide palette
    // overrides on exit. The `osc_color_set_query_reset_round_trip_through_engine`
    // test covered only the indexed form (`\e]104;1\a`); pin the
    // no-arg-resets-all branch too so it can't quietly regress
    // (e.g. if alacritty/vte upstream change the dispatch table).
    let (mut t, mut p, _rx) = harness_rx(8, 2);
    // Populate three slots so we have something to confirm reset against.
    feed(&mut t, &mut p, b"\x1b]4;1;rgb:11/22/33\x07");
    feed(&mut t, &mut p, b"\x1b]4;2;rgb:44/55/66\x07");
    feed(&mut t, &mut p, b"\x1b]4;200;rgb:77/88/99\x07");
    assert!(t.colors()[1].is_some(), "slot 1 should be set");
    assert!(t.colors()[2].is_some(), "slot 2 should be set");
    assert!(t.colors()[200].is_some(), "slot 200 should be set");
    // OSC 104 with no parameters → reset all 256 palette indices.
    feed(&mut t, &mut p, b"\x1b]104\x07");
    for idx in 0..256 {
        assert!(
            t.colors()[idx].is_none(),
            "slot {idx} should be cleared after OSC 104 (no params)"
        );
    }
}

#[test]
fn osc_110_111_112_reset_default_fg_bg_cursor_slots() {
    // OSC 110 / 111 / 112 are the reset siblings of OSC 10/11/12 (set
    // default fg/bg/cursor). They tell the engine to throw away any
    // override the user-program set so the renderer falls back to the
    // theme's defaults. Kettle's render path reads `t.colors()[256..=258]`
    // each frame, so if the engine ignored these resets, a program that
    // sent `\e]10;rgb:11/22/33\a` then `\e]110\a` would keep the override.
    // One table covers all three indices.
    for (idx, set, reset) in &[
        (
            256usize,
            &b"\x1b]10;rgb:11/22/33\x07"[..],
            &b"\x1b]110\x07"[..],
        ),
        (
            257usize,
            &b"\x1b]11;rgb:44/55/66\x07"[..],
            &b"\x1b]111\x07"[..],
        ),
        (
            258usize,
            &b"\x1b]12;rgb:77/88/99\x07"[..],
            &b"\x1b]112\x07"[..],
        ),
    ] {
        let (mut t, mut p, _rx) = harness_rx(8, 2);
        assert!(t.colors()[*idx].is_none(), "slot {idx} clean pre-set");
        feed(&mut t, &mut p, set);
        assert!(
            t.colors()[*idx].is_some(),
            "slot {idx} should be populated after OSC set"
        );
        feed(&mut t, &mut p, reset);
        assert!(
            t.colors()[*idx].is_none(),
            "slot {idx} should be cleared after the OSC reset sibling"
        );
    }
}

#[test]
fn osc_color_set_query_reset_round_trip_through_engine() {
    // Round-trip companion to the OSC query test: confirm OSC 4 set +
    // OSC 104 reset actually move the engine's `Colors` slot (so our
    // `kettle_render::resolve_query` will reflect changes live —
    // tested separately in kettle-render). This guards against an
    // upstream regression silently disconnecting the set/reset path
    // from the OSC 4 query path we ship.
    let (mut t, mut p, _rx) = harness_rx(8, 2);
    // Initially the engine has no override for palette 1.
    assert!(t.colors()[1].is_none(), "expected no override pre-set");
    // OSC 4 ; 1 ; rgb:11/22/33  → set.
    feed(&mut t, &mut p, b"\x1b]4;1;rgb:11/22/33\x07");
    let after_set = t.colors()[1].expect("OSC 4 set must populate slot 1");
    assert_eq!(
        (after_set.r, after_set.g, after_set.b),
        (0x11, 0x22, 0x33),
        "engine must store the override exactly"
    );
    // OSC 104 ; 1  → reset that index only.
    feed(&mut t, &mut p, b"\x1b]104;1\x07");
    assert!(t.colors()[1].is_none(), "OSC 104 ; 1 must clear slot 1");
}

#[test]
fn xtwinops_text_area_size_pixels_formats_csi4_reply() {
    // CSI 14 t — text-area pixel size. The engine raises
    // TextAreaSizeRequest(fmt) and expects the caller to plug in cell
    // dimensions + grid size; the formatter then produces the standard
    // `CSI 4 ; h ; w t` xtwinops reply (h = rows × cell_h, w = cols ×
    // cell_w). Sixel/kitty/iTerm2 image apps depend on this to compute
    // pixel-accurate placements.
    let (mut t, mut p, rx) = harness_rx(40, 10);
    feed(&mut t, &mut p, b"\x1b[14t");
    let fmt = rx
        .try_iter()
        .find_map(|ev| match ev {
            TermEvent::TextAreaSizeRequest(f) => Some(f),
            _ => None,
        })
        .expect("CSI 14 t must raise a TextAreaSizeRequest");
    // 9 px wide × 18 px tall cells on a 40×10 grid → 360 × 180 px.
    let reply = fmt(alacritty_terminal::event::WindowSize {
        num_lines: 10,
        num_cols: 40,
        cell_width: 9,
        cell_height: 18,
    });
    assert_eq!(
        reply, "\x1b[4;180;360t",
        "CSI 14 t reply must be CSI 4 ; <height-px> ; <width-px> t"
    );
}

#[test]
fn osc_color_queries_carry_index_and_format_xparsecolor_reply() {
    // OSC 10 / 11 / 12 (default fg / bg / cursor) and OSC 4 ; n ; ? are
    // the four queries shells and TUIs use to detect light-vs-dark and
    // theme colors. Each must (a) emit a `ColorRequest` carrying the
    // correct index — 256 / 257 / 258 / palette-idx — and (b) hand back
    // an engine-supplied formatter that renders the canonical xparsecolor
    // reply `\e]<prefix>;rgb:RRRR/GGGG/BBBB\` so apps that probe for the
    // exact wire format (mc / neovim / gnome-terminal probes) accept it.
    let cases: &[(&[u8], usize, &str)] = &[
        (b"\x1b]10;?\x07", 256, "\x1b]10;rgb:"), // OSC 10 — fg
        (b"\x1b]11;?\x07", 257, "\x1b]11;rgb:"), // OSC 11 — bg
        (b"\x1b]12;?\x07", 258, "\x1b]12;rgb:"), // OSC 12 — cursor
        (b"\x1b]4;7;?\x07", 7, "\x1b]4;7;rgb:"), // OSC 4 ; 7 — palette
    ];
    for (input, want_idx, want_prefix) in cases {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, input);
        let (idx, fmt) = rx
            .try_iter()
            .find_map(|ev| match ev {
                TermEvent::ColorRequest(i, f) => Some((i, f)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no ColorRequest for {input:?}"));
        assert_eq!(idx, *want_idx, "wrong index for {input:?}");

        // Format with a known value and verify the wire shape. The
        // 8-bit channels are doubled to 16-bit per xparsecolor.
        let reply = fmt(alacritty_terminal::vte::ansi::Rgb {
            r: 0x12,
            g: 0x34,
            b: 0x56,
        });
        let want_payload = format!("{want_prefix}1212/3434/5656");
        assert!(
            reply.starts_with(&want_payload),
            "{input:?} reply must start with {want_payload:?}, got {reply:?}"
        );
    }
}

#[test]
fn decrqm_reports_mode_state() {
    let (mut t, mut p, rx) = harness_rx(8, 2);
    // Enable bracketed paste, then DECRQM-query it (CSI ? 2004 $ p).
    feed(&mut t, &mut p, b"\x1b[?2004h\x1b[?2004$p");
    let reply = drain_pty(&rx);
    assert!(
        reply.contains("2004;1") && reply.ends_with("$y"),
        "DECRPM should report mode 2004 as set, got {reply:?}"
    );
}

#[test]
fn osc52_wire_reads_preserve_intervening_stores() {
    let (mut term, mut parser, rx, journal) = harness_latched(8, 2);
    term.set_options(TermConfig {
        osc52: alacritty_terminal::term::Osc52::CopyPaste,
        ..TermConfig::default()
    });
    feed(
        &mut term,
        &mut parser,
        b"\x1b]52;c;QQ==\x07\x1b]52;c;?\x07\x1b]52;c;Qg==\x07\x1b]52;c;?\x07",
    );
    let mut clipboard = String::new();
    let mut replies = Vec::new();
    for event in journal.take_clipboard_events() {
        match event {
            TermEvent::ClipboardStore(_, text) => clipboard = text,
            TermEvent::ClipboardLoad(_, format) => replies.push(format(&clipboard)),
            _ => panic!("unexpected clipboard operation"),
        }
    }
    assert_eq!(replies, ["\x1b]52;c;QQ==\x07", "\x1b]52;c;Qg==\x07"]);
    assert!(rx.is_empty());
}

#[test]
fn osc52_copy_emits_clipboard_store() {
    let (mut t, mut p, _rx, latched) = harness_latched(8, 2);
    // base64("hi") = "aGk=" ; OSC 52 ; c ; <b64> ST
    feed(&mut t, &mut p, b"\x1b]52;c;aGk=\x07");
    assert_eq!(
        latched
            .take_clipboard_events()
            .into_iter()
            .map(|event| match event {
                TermEvent::ClipboardStore(target, text) => (target, text),
                _ => panic!("expected clipboard store"),
            })
            .collect::<Vec<_>>(),
        vec![(crate::ClipboardType::Clipboard, "hi".to_string())],
        "OSC 52 c sets clipboard"
    );
}

#[test]
fn osc8_hyperlink_carries_on_cells() {
    let (mut t, mut p) = harness(8, 2);
    feed(
        &mut t,
        &mut p,
        b"\x1b]8;;https://x.example\x07Z\x1b]8;;\x07W",
    );
    let g = t.grid();
    let z = &g[Point::new(Line(0), Column(0))];
    assert_eq!(z.c, 'Z');
    assert_eq!(
        z.hyperlink().map(|h| h.uri().to_string()).as_deref(),
        Some("https://x.example"),
        "OSC 8 URI attaches to the cell"
    );
    // After the closing OSC 8 ; ; the link is cleared.
    assert!(g[Point::new(Line(0), Column(1))].hyperlink().is_none());
}

#[test]
fn alt_screen_preserves_primary_content() {
    let (mut t, mut p) = harness(8, 3);
    feed(&mut t, &mut p, b"main");
    feed(&mut t, &mut p, b"\x1b[?1049h"); // enter alt screen
    assert_eq!(row_text(&t, 0), "", "alt screen starts blank");
    feed(&mut t, &mut p, b"\x1b[2J\x1b[1;1Halt");
    assert_eq!(row_text(&t, 0), "alt");
    feed(&mut t, &mut p, b"\x1b[?1049l"); // back to primary
    assert_eq!(row_text(&t, 0), "main", "primary content restored");
}

#[test]
fn synchronized_output_applies_content() {
    let (mut t, mut p) = harness(8, 2);
    // DECSET 2026 brackets an atomic update; the content must be present
    // and correct once the synchronized update ends.
    feed(&mut t, &mut p, b"\x1b[?2026hhello\x1b[?2026l");
    assert_eq!(row_text(&t, 0), "hello");
}

#[test]
fn decrqm_reports_synchronized_output_mode() {
    let (mut t, mut p, rx) = harness_rx(8, 2);
    feed(&mut t, &mut p, b"\x1b[?2026$p"); // DECRQM query of mode 2026
    let reply = drain_pty(&rx);
    assert!(
        reply.contains("2026;") && reply.ends_with("$y"),
        "DECRPM should report mode 2026, got {reply:?}"
    );
}

#[test]
fn nel_index_reverse_index() {
    // NEL (ESC E): CR+LF to the next line.
    let (mut t, mut p) = harness(6, 3);
    feed(&mut t, &mut p, b"ab\x1bEcd");
    assert_eq!(row_text(&t, 0), "ab");
    assert_eq!(row_text(&t, 1), "cd");

    // IND (ESC D): down one line, column preserved.
    let (mut t2, mut p2) = harness(6, 3);
    feed(&mut t2, &mut p2, b"X\x1bDY");
    assert_eq!(row_text(&t2, 0), "X");
    assert_eq!(row_text(&t2, 1), " Y", "IND keeps the column");

    // RI (ESC M): up one line, column preserved.
    let (mut t3, mut p3) = harness(6, 3);
    feed(&mut t3, &mut p3, b"\x1b[2;1Hb\x1bMa");
    assert_eq!(row_text(&t3, 0), " a", "RI moved up, kept column");
    assert_eq!(row_text(&t3, 1), "b");
}

#[test]
fn decid_replies_like_da1() {
    let (mut t, mut p, rx) = harness_rx(8, 2);
    feed(&mut t, &mut p, b"\x1bZ"); // DECID
    let reply = drain_pty(&rx);
    assert!(
        reply.starts_with("\x1b[?") && reply.ends_with('c'),
        "DECID should reply like DA1 (CSI ? … c), got {reply:?}"
    );
}

#[test]
fn cursor_blink_mode_emits_event() {
    let (mut t, mut p, _rx, latched) = harness_latched(8, 2);
    feed(&mut t, &mut p, b"\x1b[?12h"); // DECSET 12 = cursor blink on
    assert!(
        latched.take_cursor_blinking_changed(),
        "?12h should signal a cursor-blink change"
    );
}

#[test]
fn dec_mode_12_toggles_engine_cursor_blink_state() {
    // The companion to the event test above: confirm the engine actually
    // tracks the blink state on `cursor_style().blinking` so the UI can
    // read it live (we honor the *running* program's wish for solid vs.
    // blinking cursor, not just the static config). This is what
    // `Terminal::cursor_blinking()` returns — exercised through the real
    // vte parser so the mode-flip path is real.
    let (mut t, mut p) = harness(8, 2);
    let initial = t.cursor_style().blinking;
    feed(&mut t, &mut p, b"\x1b[?12h"); // request blink
    assert!(
        t.cursor_style().blinking,
        "DEC mode 12 set must turn cursor blink on (was {initial})"
    );
    feed(&mut t, &mut p, b"\x1b[?12l"); // request solid
    assert!(
        !t.cursor_style().blinking,
        "DEC mode 12 reset must turn cursor blink off"
    );
}

#[test]
fn cht_cbt_tab_navigation() {
    // CHT (CSI I): forward N tab stops (default stops every 8).
    let (mut t, mut p) = harness(40, 2);
    feed(&mut t, &mut p, b"\x1b[3I*");
    assert_eq!(
        row_text(&t, 0).chars().nth(24),
        Some('*'),
        "CHT 3 → column 24"
    );
    // CBT (CSI Z): backward N tab stops.
    let (mut t2, mut p2) = harness(40, 2);
    feed(&mut t2, &mut p2, b"\x1b[1;21H\x1b[1ZB");
    assert_eq!(
        row_text(&t2, 0).chars().nth(16),
        Some('B'),
        "CBT 1 from col 20 → column 16"
    );
}

#[test]
fn xtwinops_text_area_size_chars() {
    // XTWINOPS CSI 18 t → report text area size in characters as
    // CSI 8 ; rows ; cols t (DA-style, deterministic — no window needed).
    let (mut t, mut p, rx) = harness_rx(40, 10);
    feed(&mut t, &mut p, b"\x1b[18t");
    assert_eq!(
        drain_pty(&rx),
        "\x1b[8;10;40t",
        "CSI 18 t must report 8;<rows>;<cols>t"
    );
}

#[test]
fn dsr_device_status_ok() {
    // DSR CSI 5 n → "terminal OK" = CSI 0 n (no malfunction).
    let (mut t, mut p, rx) = harness_rx(8, 3);
    feed(&mut t, &mut p, b"\x1b[5n");
    assert_eq!(
        drain_pty(&rx),
        "\x1b[0n",
        "CSI 5 n must reply CSI 0 n (ready)"
    );
}

#[test]
fn da1_primary_attributes_exact_params() {
    // Primary DA (CSI c) must reply exactly with Kettle's truthful feature
    // set: VT2xx-class id + shipped sixel + shipped OSC 52 clipboard.
    let (mut t, mut p, rx) = harness_rx(10, 3);
    feed(&mut t, &mut p, b"\x1b[c");
    assert_eq!(
        drain_pty(&rx),
        crate::event::PRIMARY_DA_REPLY,
        "DA1 reply must advertise VT2xx + sixel + OSC 52"
    );
    // CSI 0 c is an explicit-parameter alias for the same query.
    let (mut t2, mut p2, rx2) = harness_rx(10, 3);
    feed(&mut t2, &mut p2, b"\x1b[0c");
    assert_eq!(
        drain_pty(&rx2),
        crate::event::PRIMARY_DA_REPLY,
        "CSI 0 c == CSI c"
    );
    // DECID is the older ESC Z spelling of Primary DA and should stay in
    // lockstep with CSI c.
    let (mut t3, mut p3, rx3) = harness_rx(10, 3);
    feed(&mut t3, &mut p3, b"\x1bZ");
    assert_eq!(
        drain_pty(&rx3),
        crate::event::PRIMARY_DA_REPLY,
        "DECID == CSI c"
    );
}

#[test]
fn irm_insert_mode_shifts_right() {
    // Default (replace) vs IRM (CSI 4 h): inserting pushes text right.
    let (mut t, mut p) = harness(10, 2);
    feed(&mut t, &mut p, b"ABCD\x1b[1;1H\x1b[4hX");
    assert_eq!(row_text(&t, 0), "XABCD", "IRM inserts, shifting right");
    feed(&mut t, &mut p, b"\x1b[4l\x1b[1;1HZ");
    assert_eq!(row_text(&t, 0), "ZABCD", "4 l → back to replace");
}

#[test]
fn dectcem_cursor_visibility_mode() {
    let (mut t, mut p) = harness(6, 2);
    assert!(t.mode().contains(TermMode::SHOW_CURSOR), "shown by default");
    feed(&mut t, &mut p, b"\x1b[?25l");
    assert!(!t.mode().contains(TermMode::SHOW_CURSOR), "?25 l hides");
    feed(&mut t, &mut p, b"\x1b[?25h");
    assert!(t.mode().contains(TermMode::SHOW_CURSOR), "?25 h shows");
}

#[test]
fn lnm_newline_mode_sets_flag() {
    // CSI 20 h sets LNM; 20 l clears it. (alacritty_terminal tracks the
    // mode but does not itself translate LF→CRLF on output, so only the
    // mode bit — the conformant, observable part — is asserted here.)
    let (mut t, mut p) = harness(8, 2);
    assert!(!t.mode().contains(TermMode::LINE_FEED_NEW_LINE));
    feed(&mut t, &mut p, b"\x1b[20h");
    assert!(t.mode().contains(TermMode::LINE_FEED_NEW_LINE), "20 h sets");
    feed(&mut t, &mut p, b"\x1b[20l");
    assert!(
        !t.mode().contains(TermMode::LINE_FEED_NEW_LINE),
        "20 l clears LNM"
    );
}

#[test]
fn sgr_individual_attribute_resets() {
    // SGR `set` codes are tested elsewhere (`sgr_truecolor_bold_and_reset`,
    // `sgr_underline_dim_strike`, …). This pins the individual
    // attribute-*off* codes:
    //   * SGR 22 — normal intensity (clears bold *and* dim)
    //   * SGR 23 — not italic
    //   * SGR 24 — not underlined (clears all underline styles)
    //   * SGR 27 — not reversed
    //   * SGR 29 — not strikethrough
    // These matter for tools that emit nested styling: nvim /
    // tmux / less / `git diff --color` set an attribute, write,
    // unset just that attribute, and continue with the rest of
    // their accumulated SGR state. Without these we'd silently
    // diverge from xterm behavior (cells AFTER the `not X`
    // would carry residual flags).
    //
    // Blink rendition is retained for protocol operations; text stays steady.
    let (mut t, mut p) = harness(20, 2);
    // Stack: bold + dim + italic + underline + reverse + strike.
    // Blink has its own round-trip test below.
    feed(&mut t, &mut p, b"\x1b[1;2;3;4;7;9mA");
    let a = &t.grid()[Point::new(Line(0), Column(0))];
    assert!(a.flags.contains(Flags::BOLD), "SGR 1 set");
    assert!(a.flags.contains(Flags::DIM), "SGR 2 set");
    assert!(a.flags.contains(Flags::ITALIC), "SGR 3 set");
    assert!(a.flags.contains(Flags::UNDERLINE), "SGR 4 set");
    assert!(a.flags.contains(Flags::INVERSE), "SGR 7 set");
    assert!(a.flags.contains(Flags::STRIKEOUT), "SGR 9 set");

    // SGR 22 → clears BOTH bold and dim (normal intensity).
    feed(&mut t, &mut p, b"\x1b[22mB");
    let b = &t.grid()[Point::new(Line(0), Column(1))];
    assert!(!b.flags.contains(Flags::BOLD), "SGR 22 clears bold");
    assert!(!b.flags.contains(Flags::DIM), "SGR 22 clears dim");
    // The other flags must still be set.
    assert!(b.flags.contains(Flags::ITALIC), "SGR 22 keeps italic");
    assert!(b.flags.contains(Flags::UNDERLINE), "SGR 22 keeps underline");
    assert!(b.flags.contains(Flags::INVERSE), "SGR 22 keeps inverse");
    assert!(b.flags.contains(Flags::STRIKEOUT), "SGR 22 keeps strikeout");

    // SGR 23 → italic off only.
    feed(&mut t, &mut p, b"\x1b[23mC");
    let c = &t.grid()[Point::new(Line(0), Column(2))];
    assert!(!c.flags.contains(Flags::ITALIC), "SGR 23 clears italic");
    assert!(c.flags.contains(Flags::UNDERLINE), "SGR 23 keeps underline");

    // SGR 24 → underline off (any style).
    feed(&mut t, &mut p, b"\x1b[24mD");
    let d = &t.grid()[Point::new(Line(0), Column(3))];
    assert!(
        !d.flags.contains(Flags::UNDERLINE),
        "SGR 24 clears underline"
    );
    assert!(d.flags.contains(Flags::INVERSE), "SGR 24 keeps inverse");

    // SGR 27 → inverse off.
    feed(&mut t, &mut p, b"\x1b[27mE");
    let e = &t.grid()[Point::new(Line(0), Column(4))];
    assert!(!e.flags.contains(Flags::INVERSE), "SGR 27 clears inverse");
    assert!(e.flags.contains(Flags::STRIKEOUT), "SGR 27 keeps strikeout");

    // SGR 29 → strikeout off.
    feed(&mut t, &mut p, b"\x1b[29mF");
    let f = &t.grid()[Point::new(Line(0), Column(5))];
    assert!(
        !f.flags.contains(Flags::STRIKEOUT),
        "SGR 29 clears strikeout"
    );
}

#[test]
fn app_cursor_and_keypad_modes() {
    let (mut t, mut p) = harness(6, 2);
    feed(&mut t, &mut p, b"\x1b[?1h");
    assert!(t.mode().contains(TermMode::APP_CURSOR), "DECCKM set");
    feed(&mut t, &mut p, b"\x1b=");
    assert!(t.mode().contains(TermMode::APP_KEYPAD), "DECKPAM set");
    feed(&mut t, &mut p, b"\x1b[?1l\x1b>");
    assert!(!t.mode().contains(TermMode::APP_CURSOR));
    assert!(!t.mode().contains(TermMode::APP_KEYPAD), "DECKPNM clears");
}

#[test]
fn blink_rendition_is_retained_and_cleared_without_affecting_other_attributes() {
    let (mut t, mut p) = harness(20, 2);
    feed(&mut t, &mut p, b"\x1b[1;5mA\x1b[25mB\x1b[6mC\x1b[0mD");
    let grid = t.grid();
    assert!(
        grid[Point::new(Line(0), Column(0))]
            .flags
            .contains(Flags::BOLD | Flags::BLINK)
    );
    assert!(
        !grid[Point::new(Line(0), Column(1))]
            .flags
            .contains(Flags::BLINK)
    );
    assert!(
        grid[Point::new(Line(0), Column(1))]
            .flags
            .contains(Flags::BOLD)
    );
    assert!(
        grid[Point::new(Line(0), Column(2))]
            .flags
            .contains(Flags::BLINK)
    );
    assert!(
        !grid[Point::new(Line(0), Column(3))]
            .flags
            .intersects(Flags::BOLD | Flags::BLINK)
    );
}

#[test]
fn mouse_tracking_modes_set_and_clear_flags() {
    let (mut t, mut p) = harness(6, 2);
    feed(&mut t, &mut p, b"\x1b[?1000h");
    assert!(t.mode().contains(TermMode::MOUSE_REPORT_CLICK));
    feed(&mut t, &mut p, b"\x1b[?1002h\x1b[?1006h");
    assert!(t.mode().contains(TermMode::MOUSE_DRAG), "?1002 = drag");
    assert!(t.mode().contains(TermMode::SGR_MOUSE), "?1006 = SGR enc");
    feed(&mut t, &mut p, b"\x1b[?1003h");
    assert!(
        t.mode().contains(TermMode::MOUSE_MOTION),
        "?1003 = any-motion"
    );
    feed(&mut t, &mut p, b"\x1b[?1000l\x1b[?1002l\x1b[?1003l");
    assert!(
        !t.mode().intersects(TermMode::MOUSE_MODE),
        "all tracking off"
    );
}

#[test]
fn inline_card_cells_do_not_consume_the_kitty_placement_scan_budget() {
    let (mut term, mut processor) = harness(300, 2);
    let card = "\u{10eeee}\u{0305}\u{030d}\u{030e}\u{0310}\u{0312}\u{033d}\u{033e}\u{033f}";
    let mut text = card.repeat(260);
    text.push('\u{10eeee}');
    feed(&mut term, &mut processor, text.as_bytes());
    assert_eq!(
        term.grid()[Point::new(Line(0), Column(0))]
            .zerowidth()
            .unwrap()
            .len(),
        8
    );
    let cells = Terminal::placeholder_cells_from_term(&term);
    assert_eq!(
        cells.len(),
        1,
        "only the ordinary Kitty placeholder is a placement candidate"
    );
    assert_eq!(
        cells[0].1, 260,
        "cards must not starve the real image after the placement cap"
    );
}

#[test]
fn inline_card_breaks_kitty_left_inheritance() {
    let (mut term, mut processor) = harness(8, 2);
    feed(&mut term, &mut processor,
            "\u{10eeee}\u{0305}\u{030d}\u{0305}\u{10eeee}\u{0305}\u{030d}\u{030e}\u{0310}\u{0312}\u{033d}\u{033e}\u{033f}\u{10eeee}".as_bytes());
    let cells = Terminal::placeholder_cells_from_term(&term);
    assert_eq!(cells.len(), 2);
    assert_eq!((cells[0].1, cells[0].2.col), (0, 1));
    assert_eq!(
        (cells[1].1, cells[1].2.col),
        (2, 0),
        "left inheritance cannot cross a card cell"
    );
}

#[test]
fn four_marks_keep_existing_kitty_placeholder_behavior() {
    let (mut term, mut processor) = harness(8, 2);
    feed(
        &mut term,
        &mut processor,
        "\u{10eeee}\u{0305}\u{030d}\u{030e}\u{0310}".as_bytes(),
    );
    let cells = Terminal::placeholder_cells_from_term(&term);
    assert_eq!(cells.len(), 1);
    assert_eq!(
        (cells[0].2.row, cells[0].2.col, cells[0].2.image_id),
        (0, 1, 2 << 24)
    );
}

#[test]
fn placeholder_document_row_applies_scrollback_offset_once() {
    use alacritty_terminal::grid::Scroll;

    let (mut term, mut processor) = history_harness(8, 2, 4);
    feed(
        &mut term,
        &mut processor,
        b"0\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6",
    );
    assert_eq!(term.grid().history_size(), 4);
    assert!(term.grid().history_origin() > 0);

    term.grid_mut()[Point::new(Line(-2), Column(0))].c = placeholder::PLACEHOLDER;
    term.scroll_display(Scroll::Delta(2));
    let expected =
        stable_grid_line_id(term.grid().history_origin(), term.grid().history_size(), -2);
    let cells = Terminal::placeholder_cells_from_term(&term);

    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].0, expected);
    assert_eq!(cells[0].1, 0);
    assert_ne!(
        cells[0].0,
        expected.saturating_sub(term.grid().display_offset() as u64),
        "display_iter already reports a grid-relative negative line"
    );
}

// SS2/SS3 single-shift (ESC N / ESC O), HTS (ESC H, custom tab
// stops), DECSCA/DECSEL selective-erase and LNM LF→CRLF *output*
// translation are not applied by alacritty_terminal, so no conformance
// test asserts those behaviors (only LNM's mode bit) — see ROADMAP.

/// A selection fits a byte limit or is refused before its text is
/// built; nothing selected is no text.
#[test]
fn a_selection_is_read_only_within_its_limit() {
    let (mut t, mut p) = harness(20, 4);
    feed(&mut t, &mut p, b"hello world\r\nsecond");
    assert_eq!(selection_text_bounded(&t, 1024), Ok(None));
    let mut selection = Selection::new(
        SelectionType::Simple,
        Point::new(Line(0), Column(0)),
        Side::Left,
    );
    selection.update(Point::new(Line(1), Column(5)), Side::Right);
    t.selection = Some(selection);
    assert_eq!(
        selection_text_bounded(&t, 1024),
        Ok(Some("hello world\nsecond".to_owned()))
    );
    assert_eq!(selection_text_bounded(&t, 8), Err(SelectionTextTooLarge));
}

/// A diagram selected from a reply is joined back across the wraps at
/// the terminal's width; a selection starting inside its first row
/// keeps that row's indentation.
#[test]
fn a_selected_diagram_is_joined_back_at_the_terminal_width() {
    let (mut t, mut p) = harness(30, 8);
    feed(
            &mut t,
            &mut p,
            b"\xe2\x8f\xba Here:\r\n  mermaid\r\n  flowchart TD\r\n      A[Receive the pull\r\n  request] --> B\r\n      B --> C",
        );
    assert_eq!(selected_diagram_sources(&t), Ok(None));
    let select = |t: &mut Term<EventProxy>, from: (i32, usize)| {
        let mut selection = Selection::new(
            SelectionType::Simple,
            Point::new(Line(from.0), Column(from.1)),
            Side::Left,
        );
        selection.update(Point::new(Line(5), Column(29)), Side::Right);
        t.selection = Some(selection);
    };
    let want = vec!["flowchart TD\n    A[Receive the pull request] --> B\n    B --> C".to_owned()];
    select(&mut t, (0, 0));
    assert_eq!(selected_diagram_sources(&t), Ok(Some(want.clone())));
    select(&mut t, (2, 2));
    assert_eq!(selected_diagram_sources(&t), Ok(Some(want)));
}

/// A rectangle selected over a diagram takes the lines it spans whole:
/// cut at its left side, a wrapped row would look like source at the
/// left edge, and a comment's wrap would become a statement.
#[test]
fn a_rectangle_over_a_diagram_takes_its_lines_whole() {
    let (mut t, mut p) = harness(30, 8);
    feed(
            &mut t,
            &mut p,
            b"\xe2\x8f\xba Here:\r\n  flowchart TD\r\n      %% This is an example of\r\n  A --> B\r\n      C --> D",
        );
    let mut selection = Selection::new(
        SelectionType::Block,
        Point::new(Line(1), Column(2)),
        Side::Left,
    );
    selection.update(Point::new(Line(4), Column(20)), Side::Right);
    t.selection = Some(selection);
    assert_eq!(
        selected_diagram_sources(&t),
        Ok(Some(vec![
            "flowchart TD\n    %% This is an example of A --> B\n    C --> D".to_owned()
        ]))
    );
}

#[test]
fn dec_keypad_mode_alias_and_saved_state_follow_the_live_keypad() {
    let (mut term, mut parser, rx) = harness_rx(8, 3);
    feed(&mut term, &mut parser, b"\x1b[?66h\x1b[?66$p");
    assert!(term.mode().contains(TermMode::APP_KEYPAD));
    assert_eq!(drain_pty(&rx), "\x1b[?66;1$y");
    feed(&mut term, &mut parser, b"\x1b[?66s\x1b>\x1b[?66$p");
    assert!(!term.mode().contains(TermMode::APP_KEYPAD));
    assert_eq!(drain_pty(&rx), "\x1b[?66;2$y");
    feed(&mut term, &mut parser, b"\x1b[?66r");
    assert!(term.mode().contains(TermMode::APP_KEYPAD));
    feed(&mut term, &mut parser, b"\x1b[?66l\x1b=\x1b[?66$p");
    assert_eq!(drain_pty(&rx), "\x1b[?66;1$y");
    feed(&mut term, &mut parser, b"\x1b[!p\x1b[?66$p");
    assert_eq!(drain_pty(&rx), "\x1b[?66;2$y");
}

#[test]
fn dec_no_clear_preserves_cells_but_still_homes_and_resets_margins() {
    for level in [4, 5] {
        for column_change in ["\x1b[?3h", "\x1b[?3l"] {
            let prefix = format!(
                "\x1b[{};1\"pABC\x1b[?95h\x1b[?69h\x1b[2;7s\x1b[2;3r\x1b[?6h",
                60 + level
            );
            let bytes = format!("{prefix}{column_change}\x1b[?95$p").into_bytes();
            for split in 0..=bytes.len() {
                let (mut term, mut parser, rx) = harness_rx(8, 3);
                feed(&mut term, &mut parser, &bytes[..split]);
                feed(&mut term, &mut parser, &bytes[split..]);
                assert_eq!(term.columns(), 8, "programs cannot resize Kettle");
                assert_eq!(term.grid().cursor.point, Point::new(Line(0), Column(0)));
                assert!(!term.mode().contains(TermMode::LEFT_RIGHT_MARGIN));
                let row: String = term.grid()[Line(0)][..].iter().map(|c| c.c).collect();
                assert_eq!(row, if level == 5 { "ABC     " } else { "        " });
                assert_eq!(
                    drain_pty(&rx),
                    if level == 5 {
                        "\x1b[?95;1$y"
                    } else {
                        "\x1b[?95;0$y"
                    }
                );
                feed(&mut term, &mut parser, b"\x1b[?95l\x1b[?3h");
                assert!(term.grid()[Line(0)][..].iter().all(|c| c.c == ' '));
            }
        }
    }
}

#[test]
fn dec_no_clear_tracks_graphics_selection_alternate_screen_and_resets() {
    use alacritty_terminal::term::GraphicsEvent;
    let (mut term, mut parser, rx) = harness_rx(8, 3);
    feed(&mut term, &mut parser, "\x1b[?95h\x1b[?95s界X".as_bytes());
    let mut selection = Selection::new(
        SelectionType::Simple,
        Point::new(Line(0), Column(0)),
        Side::Left,
    );
    selection.update(Point::new(Line(0), Column(2)), Side::Right);
    term.selection = Some(selection);
    term.take_graphics_events();
    feed(&mut term, &mut parser, b"\x1b[?3h");
    assert_eq!(term.selection_to_string().as_deref(), Some("界X"));
    assert!(
        term.grid()[Line(0)][Column(0)]
            .flags
            .contains(Flags::WIDE_CHAR)
    );
    assert!(
        term.grid()[Line(0)][Column(1)]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
    );
    assert!(term.take_graphics_events().events.is_empty());
    feed(
        &mut term,
        &mut parser,
        b"\x1b[?95l\x1b[?95r\x1b[?1049hALT\x1b[?3l",
    );
    assert_eq!(term.grid()[Line(0)][Column(0)].c, 'A');
    feed(&mut term, &mut parser, b"\x1b[?1049l\x1b[?95l");
    term.take_graphics_events();
    feed(&mut term, &mut parser, b"\x1b[?3h");
    assert!(term.selection.is_none());
    assert_eq!(
        term.take_graphics_events().events,
        [GraphicsEvent::EraseDisplay]
    );
    for reset in ["\x1b[!p", "\x1bc"] {
        feed(&mut term, &mut parser, b"\x1b[?95h\x1b[?95s");
        feed(&mut term, &mut parser, reset.as_bytes());
        feed(&mut term, &mut parser, b"\x1b[?95r\x1b[?95$p");
        assert_eq!(drain_pty(&rx), "\x1b[?95;2$y");
    }
}

#[test]
fn dec_backarrow_reports_negotiation_and_restores_its_saved_state() {
    let (mut term, mut parser, rx) = harness_rx(8, 3);
    feed(
        &mut term,
        &mut parser,
        b"\x1b[?67$p\x1b[?67h\x1b[?67$p\x1b[?67s\x1b[?67l\x1b[?67$p\x1b[?67r\x1b[?67$p",
    );
    assert_eq!(
        drain_pty(&rx),
        "\x1b[?67;2$y\x1b[?67;1$y\x1b[?67;2$y\x1b[?67;1$y"
    );
}

#[test]
fn dec_column_change_homes_even_in_a_single_row_pane() {
    for keep in [false, true] {
        let (mut term, mut parser) = harness(8, 1);
        feed(&mut term, &mut parser, b"ABC\x1b[?69h\x1b[2;7s\x1b[?6h");
        if keep {
            feed(&mut term, &mut parser, b"\x1b[?95h");
        }
        feed(&mut term, &mut parser, b"\x1b[?3h");
        assert_eq!(term.grid().cursor.point, Point::new(Line(0), Column(0)));
    }
}

#[test]
fn grapheme_mode_2027_is_opt_in_and_keeps_emoji_in_its_leading_cell() {
    let sequence = "\u{1f469}\u{200d}\u{1f4bb}";
    let (mut legacy, mut lp) = harness(12, 3);
    feed(&mut legacy, &mut lp, sequence.as_bytes());
    assert_eq!(legacy.grid().cursor.point.column, Column(4));
    let (mut term, mut parser, rx) = harness_rx(12, 3);
    feed(&mut term, &mut parser, b"\x1b[?2027h\x1b[?2027$p");
    for c in sequence.chars() {
        let mut encoded = [0; 4];
        feed(
            &mut term,
            &mut parser,
            c.encode_utf8(&mut encoded).as_bytes(),
        );
    }
    assert_eq!(drain_pty(&rx), "\x1b[?2027;1$y");
    assert_eq!(term.grid().cursor.point.column, Column(2));
    let cell = &term.grid()[Line(0)][Column(0)];
    assert_eq!(cell.c, '\u{1f469}');
    assert_eq!(cell.zerowidth(), Some(&['\u{200d}', '\u{1f4bb}'][..]));
}

#[test]
fn grapheme_late_vs16_widens_once_and_preserves_first_attributes() {
    let (mut term, mut parser) = harness(8, 3);
    feed(&mut term, &mut parser, b"\x1b[?2027h\x1b[31m");
    feed(&mut term, &mut parser, "\u{2764}".as_bytes());
    assert_eq!(term.grid().cursor.point.column, Column(1));
    feed(&mut term, &mut parser, b"\x1b[32m");
    feed(
        &mut term,
        &mut parser,
        "\u{fe0f}\u{fe0f}\u{fe0e}".as_bytes(),
    );
    assert_eq!(term.grid().cursor.point.column, Column(2));
    let cell = &term.grid()[Line(0)][Column(0)];
    assert!(cell.flags.contains(Flags::WIDE_CHAR));
    assert_eq!(
        cell.fg,
        alacritty_terminal::vte::ansi::Color::Named(alacritty_terminal::vte::ansi::NamedColor::Red)
    );
    assert_eq!(
        cell.zerowidth(),
        Some(&['\u{fe0f}', '\u{fe0f}', '\u{fe0e}'][..])
    );
    assert!(
        term.grid()[Line(0)][Column(1)]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
    );
    feed(&mut term, &mut parser, b"X");
    assert_eq!(term.grid()[Line(0)][Column(2)].c, 'X');
    assert_eq!(
        term.grid()[Line(0)][Column(2)].fg,
        alacritty_terminal::vte::ansi::Color::Named(
            alacritty_terminal::vte::ansi::NamedColor::Green
        )
    );
}

#[test]
fn grapheme_late_vs16_wraps_the_whole_cluster_and_inserts_only_one_extra_cell() {
    let (mut term, mut parser) = harness(6, 3);
    feed(&mut term, &mut parser, b"\x1b[?2027habcde");
    feed(&mut term, &mut parser, "\u{2764}\u{fe0f}X".as_bytes());
    assert_eq!(term.grid()[Line(0)][Column(5)].c, ' ');
    assert!(
        term.grid()[Line(0)][Column(5)]
            .flags
            .contains(Flags::LEADING_WIDE_CHAR_SPACER)
    );
    assert_eq!(term.grid()[Line(1)][Column(0)].c, '\u{2764}');
    assert_eq!(term.grid()[Line(1)][Column(2)].c, 'X');
    let (mut term, mut parser) = harness(8, 3);
    feed(
        &mut term,
        &mut parser,
        b"abcdefgh\x1b[1;2H\x1b[4h\x1b[?2027h",
    );
    feed(&mut term, &mut parser, "\u{2764}\u{fe0f}".as_bytes());
    let row: String = (3..8)
        .map(|col| term.grid()[Line(0)][Column(col)].c)
        .collect();
    assert_eq!(row, "bcdef");
    assert_eq!(term.grid().cursor.point.column, Column(3));
}

#[test]
fn grapheme_sequences_survive_every_utf8_split_and_keep_one_leading_cell() {
    for (text, width) in [
        ("\u{1f1fa}\u{1f1f8}", 2),
        ("\u{1f44d}\u{1f3fb}", 2),
        ("\u{915}\u{94d}\u{937}", 1),
        ("\u{94d}\u{915}", 1),
        ("\u{600}A", 1),
        ("\u{1100}\u{1161}\u{11a8}", 2),
        ("e\u{301}", 1),
    ] {
        for split in 0..=text.len() {
            let (mut term, mut parser) = harness(12, 3);
            feed(&mut term, &mut parser, b"\x1b[?2027h");
            feed(&mut term, &mut parser, &text.as_bytes()[..split]);
            feed(&mut term, &mut parser, &text.as_bytes()[split..]);
            assert_eq!(
                term.grid().cursor.point.column,
                Column(width),
                "{text:?} split {split}"
            );
            let cell = &term.grid()[Line(0)][Column(0)];
            let stored: String = std::iter::once(cell.c)
                .chain(cell.zerowidth().unwrap_or_default().iter().copied())
                .collect();
            assert_eq!(stored, text, "split {split}");
        }
    }
}

#[test]
fn grapheme_rep_repeats_complete_clusters_and_mode_is_not_saved_with_cursor() {
    let (mut term, mut parser, rx) = harness_rx(20, 3);
    feed(&mut term, &mut parser, b"\x1b7\x1b[?2027h\x1b8\x1b[?2027$p");
    assert_eq!(drain_pty(&rx), "\x1b[?2027;1$y");
    feed(
        &mut term,
        &mut parser,
        "\u{1f469}\u{200d}\u{1f4bb}".as_bytes(),
    );
    feed(&mut term, &mut parser, b"\x1b[2b");
    assert_eq!(term.grid().cursor.point.column, Column(6));
    for col in [0, 2, 4] {
        let cell = &term.grid()[Line(0)][Column(col)];
        assert_eq!(cell.c, '\u{1f469}');
        assert_eq!(cell.zerowidth(), Some(&['\u{200d}', '\u{1f4bb}'][..]));
    }
    for reset in [b"\x1b[!p".as_slice(), b"\x1bc"] {
        feed(&mut term, &mut parser, reset);
        feed(&mut term, &mut parser, b"\x1b[?2027$p");
        assert_eq!(drain_pty(&rx), "\x1b[?2027;2$y");
        feed(&mut term, &mut parser, b"\x1b[?2027h");
    }
}

#[test]
fn grapheme_overwrite_of_wide_spacer_erases_all_cluster_metadata() {
    let (mut term, mut parser) = harness(10, 3);
    feed(&mut term, &mut parser, b"\x1b[?2027h");
    feed(
        &mut term,
        &mut parser,
        "\u{1f469}\u{200d}\u{1f4bb}".as_bytes(),
    );
    feed(&mut term, &mut parser, b"\x1b[1;2HX");
    let erased = &term.grid()[Line(0)][Column(0)];
    assert_eq!(erased.c, ' ');
    assert!(erased.zerowidth().unwrap_or_default().is_empty());
    assert!(!erased.flags.intersects(Flags::WIDE_CHAR | Flags::GRAPHEME));
    assert_eq!(term.grid()[Line(0)][Column(1)].c, 'X');
}

#[test]
fn grapheme_storage_cap_keeps_boundary_context_after_discarded_scalars() {
    let (mut term, mut parser) = harness(12, 3);
    feed(&mut term, &mut parser, b"\x1b[?2027h");
    feed(&mut term, &mut parser, "\u{1f469}".as_bytes());
    for _ in 0..1000 {
        feed(&mut term, &mut parser, "\u{301}".as_bytes());
    }
    feed(&mut term, &mut parser, "\u{200d}\u{1f4bb}X".as_bytes());
    assert_eq!(
        term.grid()[Line(0)][Column(0)].zerowidth().unwrap().len(),
        30
    );
    assert_eq!(term.grid()[Line(0)][Column(2)].c, 'X');
    assert_eq!(term.grid().cursor.point.column, Column(3));
}

#[test]
fn grapheme_copy_search_and_reflow_preserve_the_complete_cluster() {
    let cluster = "\u{1f469}\u{200d}\u{1f4bb}";
    let (mut term, mut parser) = harness(12, 3);
    feed(&mut term, &mut parser, b"\x1b[?2027hA");
    feed(&mut term, &mut parser, cluster.as_bytes());
    feed(&mut term, &mut parser, b"BCDE");
    let matches = crate::search::search(&term, cluster);
    assert_eq!(matches.len(), 1);
    assert_eq!(
        (matches[0].line, matches[0].start_col, matches[0].end_col),
        (0, 1, 1)
    );
    let mut selection = Selection::new(
        SelectionType::Simple,
        Point::new(Line(0), Column(1)),
        Side::Left,
    );
    selection.update(Point::new(Line(0), Column(2)), Side::Right);
    term.selection = Some(selection);
    assert_eq!(term.selection_to_string().as_deref(), Some(cluster));
    feed(&mut term, &mut parser, b"\x1b[?2027l");
    term.resize(TermSize {
        columns: 4,
        screen_lines: 3,
    });
    term.resize(TermSize {
        columns: 12,
        screen_lines: 3,
    });
    let matches = crate::search::search(&term, cluster);
    assert_eq!(matches.len(), 1);
    let cell = &term.grid()[Line(matches[0].line)][Column(matches[0].start_col)];
    assert_eq!(cell.c, '\u{1f469}');
    assert!(cell.flags.contains(Flags::WIDE_CHAR | Flags::GRAPHEME));
    assert_eq!(cell.zerowidth(), Some(&['\u{200d}', '\u{1f4bb}'][..]));
}

#[test]
fn grapheme_late_widening_respects_horizontal_margins_and_autowrap_off() {
    let (mut term, mut parser) = harness(8, 3);
    feed(
        &mut term,
        &mut parser,
        b"abcdefgh\x1b[?69h\x1b[3;6s\x1b[1;6H\x1b[?2027h",
    );
    feed(&mut term, &mut parser, "\u{2764}\u{fe0f}".as_bytes());
    assert_eq!(term.grid()[Line(0)][Column(5)].c, ' ');
    assert_eq!(term.grid()[Line(0)][Column(6)].c, 'g');
    assert_eq!(term.grid()[Line(1)][Column(2)].c, '\u{2764}');
    assert!(
        term.grid()[Line(1)][Column(3)]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
    );
    let (mut term, mut parser) = harness(2, 1);
    feed(&mut term, &mut parser, b"\x1b[?2027h\x1b[?7lA");
    feed(&mut term, &mut parser, "\u{2764}\u{fe0f}".as_bytes());
    assert_eq!(term.grid()[Line(0)][Column(0)].c, 'A');
    assert_eq!(term.grid()[Line(0)][Column(1)].c, '\u{2764}');
    assert!(
        !term.grid()[Line(0)][Column(1)]
            .flags
            .contains(Flags::WIDE_CHAR)
    );
    assert_eq!(term.grid().cursor.point.column, Column(1));
}

#[test]
fn grapheme_rep_leaves_the_last_repeated_cell_open_for_combining_input() {
    let (mut term, mut parser) = harness(10, 3);
    feed(&mut term, &mut parser, b"\x1b[?2027hA\x1b[2b");
    feed(&mut term, &mut parser, "\u{301}".as_bytes());
    assert_eq!(term.grid().cursor.point.column, Column(3));
    assert_eq!(
        term.grid()[Line(0)][Column(2)].zerowidth(),
        Some(&['\u{301}'][..])
    );
    feed(&mut term, &mut parser, b"B");
    assert_eq!(term.grid()[Line(0)][Column(3)].c, 'B');
}

#[test]
fn grapheme_full_cells_still_apply_variation_selectors() {
    let (mut term, mut parser) = harness(12, 3);
    feed(&mut term, &mut parser, b"\x1b[?2027h");
    feed(&mut term, &mut parser, "\u{2764}".as_bytes());
    for _ in 0..100 {
        feed(&mut term, &mut parser, "\u{301}".as_bytes());
    }
    feed(&mut term, &mut parser, "\u{fe0f}\u{fe0e}".as_bytes());
    assert_eq!(term.grid().cursor.point.column, Column(2));
    let cell = &term.grid()[Line(0)][Column(0)];
    assert_eq!(cell.zerowidth().unwrap().len(), 30);
    assert_eq!(cell.zerowidth().unwrap().last(), Some(&'\u{fe0e}'));
}

#[test]
fn grapheme_full_zero_width_prefix_still_retains_first_spacing_scalar() {
    let (mut term, mut parser) = harness(12, 3);
    feed(&mut term, &mut parser, b"\x1b[?2027h");
    for _ in 0..100 {
        feed(&mut term, &mut parser, "\u{94d}".as_bytes());
    }
    feed(&mut term, &mut parser, "\u{915}".as_bytes());
    assert_eq!(term.grid().cursor.point.column, Column(1));
    let cell = &term.grid()[Line(0)][Column(0)];
    assert_eq!(cell.zerowidth().unwrap().len(), 30);
    assert_eq!(cell.zerowidth().unwrap().last(), Some(&'\u{915}'));
}

#[test]
fn grapheme_semantic_selection_and_brackets_use_the_spacing_base() {
    let (mut term, mut parser) = harness(12, 3);
    feed(&mut term, &mut parser, "\x1b[?2027hA\u{605} B".as_bytes());
    assert_eq!(term.grid()[Line(0)][Column(1)].c, '\u{605}');
    assert_eq!(
        term.semantic_search_right(Point::new(Line(0), Column(0))),
        Point::new(Line(0), Column(0))
    );
    let (mut term, mut parser) = harness(12, 3);
    feed(
        &mut term,
        &mut parser,
        "\x1b[?2027h\u{605}(X\u{605})".as_bytes(),
    );
    assert_eq!(term.grid()[Line(0)][Column(0)].c, '\u{605}');
    assert_eq!(
        term.bracket_search(Point::new(Line(0), Column(0))),
        Some(Point::new(Line(0), Column(2)))
    );
    assert_eq!(
        term.bracket_search(Point::new(Line(0), Column(2))),
        Some(Point::new(Line(0), Column(0)))
    );
}

#[test]
fn grapheme_append_context_does_not_cross_alternate_screens() {
    let (mut term, mut parser, rx) = harness_rx(10, 3);
    feed(&mut term, &mut parser, b"\x1b[?2027hA\x1b[?1049h");
    feed(&mut term, &mut parser, "\u{301}".as_bytes());
    assert!(
        term.grid()[Line(0)][Column(0)]
            .zerowidth()
            .unwrap_or_default()
            .is_empty()
    );
    feed(&mut term, &mut parser, b"B\x1b[?1049l");
    feed(&mut term, &mut parser, "\u{301}".as_bytes());
    let original = &term.grid()[Line(0)][Column(0)];
    assert_eq!(original.c, 'A');
    assert!(original.flags.contains(Flags::GRAPHEME));
    assert!(original.zerowidth().unwrap_or_default().is_empty());
    feed(&mut term, &mut parser, b"C\x1b[?2027$p");
    assert_eq!(term.grid()[Line(0)][Column(1)].c, 'C');
    assert_eq!(drain_pty(&rx), "\x1b[?2027;1$y");
}

#[test]
fn grapheme_rectangles_copy_the_whole_cell_and_erase_both_wide_halves() {
    let (mut term, mut parser) = harness(12, 3);
    feed(
        &mut term,
        &mut parser,
        "\x1b[?2027h\u{1f469}\u{200d}\u{1f4bb}X".as_bytes(),
    );
    let original = term.grid()[Line(0)][Column(0)].clone();
    assert!(original.flags.contains(Flags::GRAPHEME | Flags::WIDE_CHAR));
    feed(&mut term, &mut parser, b"\x1b[1;1;1;2;1;2;3;1$v");
    assert_eq!(term.grid()[Line(1)][Column(2)], original);
    feed(&mut term, &mut parser, b"\x1b[2;4;2;4$z");
    for column in [2, 3] {
        let cell = &term.grid()[Line(1)][Column(column)];
        assert_eq!(cell.c, ' ');
        assert!(
            !cell
                .flags
                .intersects(Flags::GRAPHEME | Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER)
        );
        assert!(cell.zerowidth().unwrap_or_default().is_empty());
    }
    assert_eq!(term.grid()[Line(0)][Column(0)], original);
}

#[test]
fn grapheme_late_widening_scrolls_once_and_publishes_the_graphics_boundary() {
    use alacritty_terminal::term::{GraphicsEvent, GraphicsScrollDirection};
    let (mut term, mut parser) = harness(4, 2);
    feed(&mut term, &mut parser, b"abcd\r\nefgh\x1b[2;4H\x1b[?2027h");
    feed(&mut term, &mut parser, "\u{2764}".as_bytes());
    term.take_graphics_events();
    feed(&mut term, &mut parser, "\u{fe0f}".as_bytes());
    assert_eq!(term.history_size(), 1);
    assert_eq!(term.grid()[Line(1)][Column(0)].c, '\u{2764}');
    assert_eq!(
        term.grid()[Line(1)][Column(0)].zerowidth(),
        Some(&['\u{fe0f}'][..])
    );
    assert_eq!(term.grid().cursor.point, Point::new(Line(1), Column(2)));
    let events = term.take_graphics_events().events;
    assert!(matches!(
        events.as_slice(),
        [GraphicsEvent::Scroll {
            direction: GraphicsScrollDirection::Up,
            lines: 1,
            ..
        }]
    ));
}

#[test]
fn grapheme_ignored_unicode_controls_break_the_append_context() {
    let text = "\x1b[?2027h\u{1f469}\u{200d}\u{80}\u{1f4bb}X".as_bytes();
    for split in 0..=text.len() {
        let (mut term, mut parser) = harness(12, 3);
        feed(&mut term, &mut parser, &text[..split]);
        feed(&mut term, &mut parser, &text[split..]);
        assert_eq!(term.grid().cursor.point.column, Column(5), "split {split}");
        assert_eq!(
            term.grid()[Line(0)][Column(0)].zerowidth(),
            Some(&['\u{200d}'][..])
        );
        assert_eq!(term.grid()[Line(0)][Column(2)].c, '\u{1f4bb}');
    }
}

#[test]
fn grapheme_storage_cap_retains_the_last_zwj_and_its_spacing_scalar() {
    let (mut term, mut parser) = harness(12, 3);
    feed(&mut term, &mut parser, "\x1b[?2027h\u{1f469}".as_bytes());
    for _ in 0..30 {
        feed(&mut term, &mut parser, "\u{301}".as_bytes());
    }
    feed(&mut term, &mut parser, "\u{200d}\u{1f408}X".as_bytes());
    let marks = term.grid()[Line(0)][Column(0)].zerowidth().unwrap();
    assert_eq!(marks.len(), 30);
    assert_eq!(&marks[28..], &['\u{200d}', '\u{1f408}']);
    assert_eq!(term.grid()[Line(0)][Column(2)].c, 'X');
}

#[test]
fn special_color_aliases_roundtrip_without_touching_dynamic_colors() {
    let (mut term, mut parser, rx) = harness_rx(12, 3);
    feed(
        &mut term,
        &mut parser,
        b"\x1b]10;#102030\x07\x1b]4;256;#123456;260;#abcdef\x07\x1b]5;0;?;4;?\x1b\\",
    );
    let foreground = term.colors()[256];
    assert_eq!(
        foreground,
        Some(alacritty_terminal::vte::ansi::Rgb {
            r: 0x10,
            g: 0x20,
            b: 0x30
        })
    );
    let queried: Vec<_> = rx
        .try_iter()
        .filter_map(|event| match event {
            TermEvent::ColorRequest(index, format) => Some(format(term.colors()[index].unwrap())),
            _ => None,
        })
        .collect();
    assert_eq!(
        queried,
        [
            "\x1b]5;0;rgb:1212/3434/5656\x1b\\",
            "\x1b]5;4;rgb:abab/cdcd/efef\x1b\\"
        ]
    );
    feed(&mut term, &mut parser, b"\x1b]105;0\x07");
    assert_eq!(term.colors()[269], None);
    assert!(term.colors()[273].is_some());
    assert_eq!(term.colors()[256], foreground);
    feed(&mut term, &mut parser, b"\x1b]105\x07");
    assert_eq!(term.colors()[273], None);
    assert_eq!(term.colors()[256], foreground);
}

#[test]
fn special_color_modes_are_bounded_and_persist_through_terminal_resets() {
    let (mut term, mut parser) = harness(12, 3);
    feed(
        &mut term,
        &mut parser,
        b"\x1b]6;0;1\x07\x1b]106;5;300\x07\x1b]6;6;1\x07",
    );
    assert!(term.colors().special_mode(0));
    assert!(term.colors().special_mode(5));
    assert!(!term.colors().special_mode(6));
    assert!(!term.colors().special_mode(usize::MAX));
    feed(&mut term, &mut parser, b"\x1b]6;0;0\x07");
    assert!(!term.colors().special_mode(0));
    assert!(term.colors().special_mode(5));
    feed(&mut term, &mut parser, b"\x1bc");
    assert!(term.colors().special_mode(5));
    feed(&mut term, &mut parser, b"\x1b[!p");
    assert!(term.colors().special_mode(5));
}

#[test]
fn screen_reverse_mode_queries_save_restore_and_resets_across_chunks() {
    let bytes = b"\x1b[?5$p\x1b[?5h\x1b[?5$p\x1b[!p\x1b[?5$p\x1b[?5s\x1b[?5l\x1b[?5$p\x1b[?5r\x1b[?5$p\x1bc\x1b[?5$p";
    for split in 0..=bytes.len() {
        let (mut term, mut parser, rx) = harness_rx(12, 3);
        feed(&mut term, &mut parser, &bytes[..split]);
        feed(&mut term, &mut parser, &bytes[split..]);
        assert_eq!(
            drain_pty(&rx),
            "\x1b[?5;2$y\x1b[?5;1$y\x1b[?5;1$y\x1b[?5;2$y\x1b[?5;1$y\x1b[?5;2$y",
            "split {split}"
        );
    }
}

#[test]
fn screen_reverse_palette_updates_follow_displayed_slots_without_changing_other_colors() {
    use alacritty_terminal::vte::ansi::Rgb;
    let (mut term, mut parser) = harness(12, 3);
    feed(
        &mut term,
        &mut parser,
        b"\x1b]4;0;#ff0000;7;#00ff00;1;#123456\x07\x1b[?5h",
    );
    assert_eq!(term.colors()[0], Some(Rgb { r: 0, g: 255, b: 0 }));
    assert_eq!(term.colors()[7], Some(Rgb { r: 255, g: 0, b: 0 }));
    assert_eq!(
        term.colors()[1],
        Some(Rgb {
            r: 0x12,
            g: 0x34,
            b: 0x56
        })
    );
    feed(&mut term, &mut parser, b"\x1b]4;0;#0000ff\x07\x1b[?5l");
    assert_eq!(term.colors()[0], Some(Rgb { r: 255, g: 0, b: 0 }));
    assert_eq!(term.colors()[7], Some(Rgb { r: 0, g: 0, b: 255 }));
    assert!(!term.colors().has_special_modes());
}

#[test]
fn ansi_keyboard_modes_report_defaults_changes_and_reset_boundaries() {
    let sequence = b"\x1b[2$p\x1b[12$p\x1b[2h\x1b[12l\x1b[2$p\x1b[12$p\x1b[!p\x1b[2$p\x1b[12$p\x1bc\x1b[2$p\x1b[12$p";
    for split in 0..=sequence.len() {
        let (mut term, mut parser, rx) = harness_rx(12, 3);
        term.set_options(TermConfig {
            keyboard_input_modes: true,
            ..TermConfig::default()
        });
        feed(&mut term, &mut parser, &sequence[..split]);
        feed(&mut term, &mut parser, &sequence[split..]);
        let writes: Vec<_> = rx
            .try_iter()
            .filter_map(|event| match event {
                TermEvent::PtyWrite(bytes) => Some(bytes),
                _ => None,
            })
            .collect();
        assert_eq!(
            writes,
            [
                "\x1b[2;2$y",
                "\x1b[12;1$y",
                "\x1b[2;1$y",
                "\x1b[12;2$y",
                "\x1b[2;2$y",
                "\x1b[12;2$y",
                "\x1b[2;2$y",
                "\x1b[12;1$y"
            ],
            "split {split}"
        );
    }
}

#[test]
fn keyboard_hardware_status_is_refused_at_every_declared_level() {
    for level in 62..=65 {
        let (mut term, mut parser, rx) = harness_rx(12, 3);
        feed(
            &mut term,
            &mut parser,
            format!("\x1b[{level};1\"p\x1b[?26n").as_bytes(),
        );
        assert_eq!(
            drain_pty(&rx),
            "",
            "level {level}: no physical DEC keyboard is modeled"
        );
    }
}

#[test]
fn a_raw_input_transport_reports_fixed_keyboard_modes_and_does_not_mutate_them() {
    let (mut term, mut parser, rx) = harness_rx(12, 3);
    feed(&mut term, &mut parser, b"\x1b[2h\x1b[12l\x1b[2$p\x1b[12$p");
    assert_eq!(drain_pty(&rx), "\x1b[2;4$y\x1b[12;3$y");
    assert!(!term.keyboard_locked());
    assert!(!term.local_echo());
}

#[test]
fn withdrawing_keyboard_input_capability_clears_its_mutable_modes() {
    let (mut term, mut parser, rx) = harness_rx(12, 3);
    term.set_options(TermConfig {
        keyboard_input_modes: true,
        ..TermConfig::default()
    });
    feed(&mut term, &mut parser, b"\x1b[2h\x1b[12l");
    assert!(term.keyboard_locked() && term.local_echo());
    term.set_options(TermConfig::default());
    feed(&mut term, &mut parser, b"\x1b[2$p\x1b[12$p");
    assert_eq!(drain_pty(&rx), "\x1b[2;4$y\x1b[12;3$y");
    assert!(!term.keyboard_locked() && !term.local_echo());
}
