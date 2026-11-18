//! Scrollback rows are stored without their blank ends. These tests drive a
//! pane's output pipeline and check that what a pane reads from history,
//! and the line count the byte budget allows, are what full-width rows give,
//! and that reading history does not store its rows at full width again.

use super::fuzzing::SyncGraphicsHarness;
use super::*;
use alacritty_terminal::grid::{Grid, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};

fn harness(columns: usize, rows: usize, history: usize) -> SyncGraphicsHarness {
    let config = TermConfig {
        scrolling_history: history,
        ..TermConfig::default()
    };
    SyncGraphicsHarness::sized(columns, rows, config, false)
}

fn locked(pane: &SyncGraphicsHarness) -> std::sync::MutexGuard<'_, Term<EventProxy>> {
    pane.term.lock().unwrap()
}

fn seq(from: usize, to: usize) -> Vec<u8> {
    (from..=to)
        .flat_map(|i| format!("{i}\r\n").into_bytes())
        .collect()
}

/// History rows that store fewer cells than they have.
fn compact_rows(grid: &Grid<Cell>) -> usize {
    (grid.topmost_line().0..0)
        .filter(|&line| {
            let row = &grid[Line(line)];
            row.capacity() < row.len()
        })
        .count()
}

fn row_text(grid: &Grid<Cell>, line: i32) -> String {
    (0..grid.columns())
        .map(|col| grid[Point::new(Line(line), Column(col))].c)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// Search, selection copy, session snapshots, links, rendering a scrolled
/// view, height-only resizes and the alternate screen all read history
/// without storing its rows at full width again, and see the text they hold.
#[test]
fn pane_reads_keep_history_compact() {
    let mut pane = harness(190, 40, 10_000);
    pane.feed(&seq(1, 20_000));
    pane.feed(b"see https://example.com/page here\r\n\x1b[41mred\x1b[K\x1b[0m\r\n");
    pane.feed(&seq(1, 50));

    let mut term = locked(&pane);
    let grid = term.grid();
    assert_eq!(grid.history_size(), 10_000);
    let compact = compact_rows(grid);
    assert_eq!(compact, 10_000 - 1, "every row but the red one is compact");
    let allocated: usize = (grid.topmost_line().0..0)
        .map(|line| grid[Line(line)].capacity())
        .sum();
    assert!(
        allocated < 10_000 * 20,
        "history allocates {allocated} cells"
    );
    assert_eq!(row_text(grid, grid.topmost_line().0), "10014");

    let matches = crate::search::search(&term, "19937");
    assert_eq!(matches.len(), 1);
    assert_eq!(row_text(term.grid(), matches[0].line), "19937");
    assert_eq!(matches[0].start_col, 0);

    let mut budget = usize::MAX;
    let restored = crate::restore::snapshot(&term, 100, &mut budget);
    assert!(restored.iter().any(|(text, ..)| text.contains("19999")));
    assert!(restored.iter().any(|(text, ..)| text.contains("red")));

    let link_line = (term.grid().topmost_line().0..0)
        .find(|&line| row_text(term.grid(), line).starts_with("see "))
        .expect("the link row is in history");
    let mut selection = Selection::new(
        SelectionType::Simple,
        Point::new(Line(link_line - 2), Column(0)),
        Side::Left,
    );
    selection.update(Point::new(Line(link_line), Column(189)), Side::Right);
    term.selection = Some(selection);
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some("19999\n20000\nsee https://example.com/page here")
    );
    term.selection = None;

    // Scroll the link row to the top of the view, as the renderer and the
    // link scan see it.
    term.scroll_display(Scroll::Delta(-link_line));
    let links = crate::links::links(&term);
    assert!(
        links
            .iter()
            .any(|link| link.row == 0 && link.uri == "https://example.com/page"),
        "the link in history is found"
    );
    let first: String = term
        .renderable_content()
        .display_iter
        .take(4)
        .map(|cell| cell.c)
        .collect();
    assert_eq!(first, "see ");
    term.scroll_display(Scroll::Bottom);
    assert_eq!(
        compact_rows(term.grid()),
        compact,
        "reads stored rows in full"
    );
    drop(term);

    // Shrinking pushes ten rows into history, evicting the ten oldest;
    // growing pulls fifteen back onto the screen.
    pane.resize(190, 30, 10_000);
    pane.resize(190, 45, 10_000);
    pane.feed(b"\x1b[?1049halternate\r\n\r\n\x1b[?1049l");
    let term = locked(&pane);
    let grid = term.grid();
    assert_eq!(grid.history_size(), 10_000 - 15);
    assert_eq!(
        compact_rows(grid),
        10_000 - 15 - 1,
        "resizes keep history compact"
    );
    assert_eq!(row_text(grid, grid.topmost_line().0), "10024");
    for line in 0..grid.screen_lines() as i32 {
        assert_eq!(grid[Line(line)].capacity(), 190, "screen row {line}");
    }
}

/// The byte budget still counts every history row at full width, so it
/// keeps the same number of lines, which now use far less than the budget.
#[test]
fn scrollback_bytes_keeps_its_line_count() {
    let budget = 1_000_000;
    let lines = effective_scrollback_lines(10_000, budget, 190, 40);
    assert_eq!(lines, 176, "the per-line estimate is the full row width");

    let mut pane = harness(190, 40, lines);
    pane.feed(&seq(1, 5_000));
    let term = locked(&pane);
    let grid = term.grid();
    assert_eq!(grid.history_size(), 176);
    assert_eq!(compact_rows(grid), 176);
    let history_bytes: usize = (grid.topmost_line().0..0)
        .map(|line| grid[Line(line)].capacity() * std::mem::size_of::<Cell>())
        .sum();
    assert!(
        history_bytes * 20 < budget,
        "history holds {history_bytes} bytes of cells"
    );
}

/// A session snapshot of rows in history matches one of the same rows on
/// screen, styled ends and wide characters included.
#[test]
fn session_snapshot_of_compact_rows_matches_screen_rows() {
    let content = concat!(
        "plain text\r\n",
        "\x1b[1;31mbold red\x1b[0m tail\r\n",
        "\x1b[44mblue end\x1b[K\x1b[0m\r\n",
        "wide \u{754c}\u{754c} x\r\n",
        "\x1b]8;id=r;https://example.com\x1b\\link\x1b]8;;\x1b\\\r\n",
        "  indented  \r\n",
        "\r\n",
        "last\r\n",
    );
    let mut history = harness(60, 3, 100);
    history.feed(content.as_bytes());
    let mut screen = harness(60, 20, 100);
    screen.feed(content.as_bytes());

    let history_term = locked(&history);
    let screen_term = locked(&screen);
    assert_eq!(history_term.grid().history_size(), 6);
    assert_eq!(screen_term.grid().history_size(), 0);
    assert_eq!(compact_rows(history_term.grid()), 5, "all but the blue row");

    let (mut history_budget, mut screen_budget) = (usize::MAX, usize::MAX);
    let from_history = crate::restore::snapshot(&history_term, 100, &mut history_budget);
    let from_screen = crate::restore::snapshot(&screen_term, 100, &mut screen_budget);
    assert_eq!(from_history.len(), 9);
    assert_eq!(from_history, from_screen);
}
