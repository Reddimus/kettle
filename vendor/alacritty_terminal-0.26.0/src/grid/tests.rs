//! Tests for the Grid.

use super::*;

use crate::term::cell::Cell;

impl GridCell for usize {
    fn is_empty(&self) -> bool {
        *self == 0
    }

    fn reset(&mut self, template: &Self) {
        *self = *template;
    }

    fn flags(&self) -> &Flags {
        unimplemented!();
    }

    fn flags_mut(&mut self) -> &mut Flags {
        unimplemented!();
    }
}

// Scroll up moves lines upward.
#[test]
fn scroll_up() {
    let mut grid = Grid::<usize>::new(10, 1, 0);
    for i in 0..10 {
        grid[Line(i as i32)][Column(0)] = i;
    }

    grid.scroll_up::<usize>(&(Line(0)..Line(10)), 2);

    assert_eq!(grid[Line(0)][Column(0)], 2);
    assert_eq!(grid[Line(0)].occ, 1);
    assert_eq!(grid[Line(1)][Column(0)], 3);
    assert_eq!(grid[Line(1)].occ, 1);
    assert_eq!(grid[Line(2)][Column(0)], 4);
    assert_eq!(grid[Line(2)].occ, 1);
    assert_eq!(grid[Line(3)][Column(0)], 5);
    assert_eq!(grid[Line(3)].occ, 1);
    assert_eq!(grid[Line(4)][Column(0)], 6);
    assert_eq!(grid[Line(4)].occ, 1);
    assert_eq!(grid[Line(5)][Column(0)], 7);
    assert_eq!(grid[Line(5)].occ, 1);
    assert_eq!(grid[Line(6)][Column(0)], 8);
    assert_eq!(grid[Line(6)].occ, 1);
    assert_eq!(grid[Line(7)][Column(0)], 9);
    assert_eq!(grid[Line(7)].occ, 1);
    assert_eq!(grid[Line(8)][Column(0)], 0); // was 0.
    assert_eq!(grid[Line(8)].occ, 0);
    assert_eq!(grid[Line(9)][Column(0)], 0); // was 1.
    assert_eq!(grid[Line(9)].occ, 0);
}

#[test]
fn history_origin_advances_only_when_rows_are_irreversibly_removed() {
    let mut grid = Grid::<usize>::new(2, 1, 2);
    let region = Line(0)..Line(2);

    grid.scroll_up::<usize>(&region, 1);
    assert_eq!(grid.history_size(), 1);
    assert_eq!(grid.history_origin(), 0);

    grid.scroll_up::<usize>(&region, 1);
    assert_eq!(grid.history_size(), 2);
    assert_eq!(grid.history_origin(), 0);

    // At capacity, the next row reuses the oldest grid-relative coordinate.
    // The origin distinguishes that replacement from the evicted row.
    grid.scroll_up::<usize>(&region, 1);
    assert_eq!(grid.history_size(), 2);
    assert_eq!(grid.history_origin(), 1);

    // Purging history preserves active-row identifiers by moving the origin
    // forward by exactly the number of removed history rows.
    grid.clear_history();
    assert_eq!(grid.history_size(), 0);
    assert_eq!(grid.history_origin(), 3);
}

/// Reflow is the other way rows leave the document for good. Narrowing wraps
/// long lines into more rows than the history can hold, and the excess is
/// dropped from the OLDEST end — so the origin has to move with it, exactly as
/// it does for eviction and history purges. Before this, a column resize left
/// the origin untouched while the rows it counted were gone, so a retained
/// anchor pointed at a coordinate unrelated content had taken over.
#[test]
fn history_origin_advances_when_reflow_discards_oldest_rows() {
    // A FULL row is essential: narrowing a sparsely occupied row wraps nothing
    // and discards nothing. An earlier version of this test used one occupied
    // column and passed with the fix reverted — vacuous, which is worse than
    // no test at all.
    let mut grid = Grid::<Cell>::new(1, 8, 1);
    for column in 0..8 {
        grid[Line(0)][Column(column)] = cell(char::from(b'a' + column as u8));
    }
    let origin_before = grid.history_origin();

    // Eight cells at one column each become eight rows, well past the two
    // (`max_scroll_limit + lines`) this grid can hold, so reflow truncates the
    // oldest end.
    grid.resize(true, 1, 1);

    assert!(
        grid.history_origin() > origin_before,
        "reflow overflowed the history but the origin never moved \
         (origin {origin_before} -> {}, total lines {})",
        grid.history_origin(),
        grid.total_lines()
    );
}

#[test]
fn history_origin_tracks_limit_reduction_and_full_reset() {
    let mut grid = Grid::<usize>::new(3, 1, 4);
    let region = Line(0)..Line(3);
    for _ in 0..4 {
        grid.scroll_up::<usize>(&region, 1);
    }
    assert_eq!(grid.history_size(), 4);

    grid.update_history(1);
    assert_eq!(grid.history_size(), 1);
    assert_eq!(grid.history_origin(), 3);

    grid.reset::<usize>();
    assert_eq!(grid.history_size(), 0);
    assert_eq!(
        grid.history_origin(),
        7,
        "reset removes one history row and invalidates all three visible rows"
    );
}

// Scroll down moves lines downward.
#[test]
fn scroll_down() {
    let mut grid = Grid::<usize>::new(10, 1, 0);
    for i in 0..10 {
        grid[Line(i as i32)][Column(0)] = i;
    }

    grid.scroll_down::<usize>(&(Line(0)..Line(10)), 2);

    assert_eq!(grid[Line(0)][Column(0)], 0); // was 8.
    assert_eq!(grid[Line(0)].occ, 0);
    assert_eq!(grid[Line(1)][Column(0)], 0); // was 9.
    assert_eq!(grid[Line(1)].occ, 0);
    assert_eq!(grid[Line(2)][Column(0)], 0);
    assert_eq!(grid[Line(2)].occ, 1);
    assert_eq!(grid[Line(3)][Column(0)], 1);
    assert_eq!(grid[Line(3)].occ, 1);
    assert_eq!(grid[Line(4)][Column(0)], 2);
    assert_eq!(grid[Line(4)].occ, 1);
    assert_eq!(grid[Line(5)][Column(0)], 3);
    assert_eq!(grid[Line(5)].occ, 1);
    assert_eq!(grid[Line(6)][Column(0)], 4);
    assert_eq!(grid[Line(6)].occ, 1);
    assert_eq!(grid[Line(7)][Column(0)], 5);
    assert_eq!(grid[Line(7)].occ, 1);
    assert_eq!(grid[Line(8)][Column(0)], 6);
    assert_eq!(grid[Line(8)].occ, 1);
    assert_eq!(grid[Line(9)][Column(0)], 7);
    assert_eq!(grid[Line(9)].occ, 1);
}

#[test]
fn scroll_down_with_history() {
    let mut grid = Grid::<usize>::new(10, 1, 1);
    grid.increase_scroll_limit(1);
    for i in 0..10 {
        grid[Line(i as i32)][Column(0)] = i;
    }

    grid.scroll_down::<usize>(&(Line(0)..Line(10)), 2);

    assert_eq!(grid[Line(0)][Column(0)], 0); // was 8.
    assert_eq!(grid[Line(0)].occ, 0);
    assert_eq!(grid[Line(1)][Column(0)], 0); // was 9.
    assert_eq!(grid[Line(1)].occ, 0);
    assert_eq!(grid[Line(2)][Column(0)], 0);
    assert_eq!(grid[Line(2)].occ, 1);
    assert_eq!(grid[Line(3)][Column(0)], 1);
    assert_eq!(grid[Line(3)].occ, 1);
    assert_eq!(grid[Line(4)][Column(0)], 2);
    assert_eq!(grid[Line(4)].occ, 1);
    assert_eq!(grid[Line(5)][Column(0)], 3);
    assert_eq!(grid[Line(5)].occ, 1);
    assert_eq!(grid[Line(6)][Column(0)], 4);
    assert_eq!(grid[Line(6)].occ, 1);
    assert_eq!(grid[Line(7)][Column(0)], 5);
    assert_eq!(grid[Line(7)].occ, 1);
    assert_eq!(grid[Line(8)][Column(0)], 6);
    assert_eq!(grid[Line(8)].occ, 1);
    assert_eq!(grid[Line(9)][Column(0)], 7);
    assert_eq!(grid[Line(9)].occ, 1);
}

// Test that GridIterator works.
#[test]
fn test_iter() {
    let assert_indexed = |value: usize, indexed: Option<Indexed<&usize>>| {
        assert_eq!(Some(&value), indexed.map(|indexed| indexed.cell));
    };

    let mut grid = Grid::<usize>::new(5, 5, 0);
    for i in 0..5 {
        for j in 0..5 {
            grid[Line(i)][Column(j)] = i as usize * 5 + j;
        }
    }

    let mut iter = grid.iter_from(Point::new(Line(0), Column(0)));

    assert_eq!(None, iter.prev());
    assert_indexed(1, iter.next());
    assert_eq!(Column(1), iter.point().column);
    assert_eq!(0, iter.point().line);

    assert_indexed(2, iter.next());
    assert_indexed(3, iter.next());
    assert_indexed(4, iter.next());

    // Test line-wrapping.
    assert_indexed(5, iter.next());
    assert_eq!(Column(0), iter.point().column);
    assert_eq!(1, iter.point().line);

    assert_indexed(4, iter.prev());
    assert_eq!(Column(4), iter.point().column);
    assert_eq!(0, iter.point().line);

    // Make sure iter.cell() returns the current iterator position.
    assert_eq!(&4, iter.cell());

    // Test that iter ends at end of grid.
    let mut final_iter = grid.iter_from(Point {
        line: Line(4),
        column: Column(4),
    });
    assert_eq!(None, final_iter.next());
    assert_indexed(23, final_iter.prev());
}

#[test]
fn shrink_reflow() {
    let mut grid = Grid::<Cell>::new(1, 5, 2);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = cell('2');
    grid[Line(0)][Column(2)] = cell('3');
    grid[Line(0)][Column(3)] = cell('4');
    grid[Line(0)][Column(4)] = cell('5');

    grid.resize(true, 1, 2);

    assert_eq!(grid.total_lines(), 3);

    assert_eq!(grid[Line(-2)].len(), 2);
    assert_eq!(grid[Line(-2)][Column(0)], cell('1'));
    assert_eq!(grid[Line(-2)][Column(1)], wrap_cell('2'));

    assert_eq!(grid[Line(-1)].len(), 2);
    assert_eq!(grid[Line(-1)][Column(0)], cell('3'));
    assert_eq!(grid[Line(-1)][Column(1)], wrap_cell('4'));

    assert_eq!(grid[Line(0)].len(), 2);
    assert_eq!(grid[Line(0)][Column(0)], cell('5'));
    assert_eq!(grid[Line(0)][Column(1)], Cell::default());
}

#[test]
fn shrink_reflow_twice() {
    let mut grid = Grid::<Cell>::new(1, 5, 2);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = cell('2');
    grid[Line(0)][Column(2)] = cell('3');
    grid[Line(0)][Column(3)] = cell('4');
    grid[Line(0)][Column(4)] = cell('5');

    grid.resize(true, 1, 4);
    grid.resize(true, 1, 2);

    assert_eq!(grid.total_lines(), 3);

    assert_eq!(grid[Line(-2)].len(), 2);
    assert_eq!(grid[Line(-2)][Column(0)], cell('1'));
    assert_eq!(grid[Line(-2)][Column(1)], wrap_cell('2'));

    assert_eq!(grid[Line(-1)].len(), 2);
    assert_eq!(grid[Line(-1)][Column(0)], cell('3'));
    assert_eq!(grid[Line(-1)][Column(1)], wrap_cell('4'));

    assert_eq!(grid[Line(0)].len(), 2);
    assert_eq!(grid[Line(0)][Column(0)], cell('5'));
    assert_eq!(grid[Line(0)][Column(1)], Cell::default());
}

#[test]
fn shrink_reflow_empty_cell_inside_line() {
    let mut grid = Grid::<Cell>::new(1, 5, 3);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = Cell::default();
    grid[Line(0)][Column(2)] = cell('3');
    grid[Line(0)][Column(3)] = cell('4');
    grid[Line(0)][Column(4)] = Cell::default();

    grid.resize(true, 1, 2);

    assert_eq!(grid.total_lines(), 2);

    assert_eq!(grid[Line(-1)].len(), 2);
    assert_eq!(grid[Line(-1)][Column(0)], cell('1'));
    assert_eq!(grid[Line(-1)][Column(1)], wrap_cell(' '));

    assert_eq!(grid[Line(0)].len(), 2);
    assert_eq!(grid[Line(0)][Column(0)], cell('3'));
    assert_eq!(grid[Line(0)][Column(1)], cell('4'));

    grid.resize(true, 1, 1);

    assert_eq!(grid.total_lines(), 4);

    assert_eq!(grid[Line(-3)].len(), 1);
    assert_eq!(grid[Line(-3)][Column(0)], wrap_cell('1'));

    assert_eq!(grid[Line(-2)].len(), 1);
    assert_eq!(grid[Line(-2)][Column(0)], wrap_cell(' '));

    assert_eq!(grid[Line(-1)].len(), 1);
    assert_eq!(grid[Line(-1)][Column(0)], wrap_cell('3'));

    assert_eq!(grid[Line(0)].len(), 1);
    assert_eq!(grid[Line(0)][Column(0)], cell('4'));
}

#[test]
fn grow_reflow() {
    let mut grid = Grid::<Cell>::new(2, 2, 0);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = wrap_cell('2');
    grid[Line(1)][Column(0)] = cell('3');
    grid[Line(1)][Column(1)] = Cell::default();

    grid.resize(true, 2, 3);

    assert_eq!(grid.total_lines(), 2);

    assert_eq!(grid[Line(0)].len(), 3);
    assert_eq!(grid[Line(0)][Column(0)], cell('1'));
    assert_eq!(grid[Line(0)][Column(1)], cell('2'));
    assert_eq!(grid[Line(0)][Column(2)], cell('3'));

    // Make sure rest of grid is empty.
    assert_eq!(grid[Line(1)].len(), 3);
    assert_eq!(grid[Line(1)][Column(0)], Cell::default());
    assert_eq!(grid[Line(1)][Column(1)], Cell::default());
    assert_eq!(grid[Line(1)][Column(2)], Cell::default());
}

#[test]
fn grow_reflow_multiline() {
    let mut grid = Grid::<Cell>::new(3, 2, 0);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = wrap_cell('2');
    grid[Line(1)][Column(0)] = cell('3');
    grid[Line(1)][Column(1)] = wrap_cell('4');
    grid[Line(2)][Column(0)] = cell('5');
    grid[Line(2)][Column(1)] = cell('6');

    grid.resize(true, 3, 6);

    assert_eq!(grid.total_lines(), 3);

    assert_eq!(grid[Line(0)].len(), 6);
    assert_eq!(grid[Line(0)][Column(0)], cell('1'));
    assert_eq!(grid[Line(0)][Column(1)], cell('2'));
    assert_eq!(grid[Line(0)][Column(2)], cell('3'));
    assert_eq!(grid[Line(0)][Column(3)], cell('4'));
    assert_eq!(grid[Line(0)][Column(4)], cell('5'));
    assert_eq!(grid[Line(0)][Column(5)], cell('6'));

    // Make sure rest of grid is empty.
    for r in (1..3).map(Line::from) {
        assert_eq!(grid[r].len(), 6);
        for c in 0..6 {
            assert_eq!(grid[r][Column(c)], Cell::default());
        }
    }
}

#[test]
fn grow_reflow_disabled() {
    let mut grid = Grid::<Cell>::new(2, 2, 0);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = wrap_cell('2');
    grid[Line(1)][Column(0)] = cell('3');
    grid[Line(1)][Column(1)] = Cell::default();

    grid.resize(false, 2, 3);

    assert_eq!(grid.total_lines(), 2);

    assert_eq!(grid[Line(0)].len(), 3);
    assert_eq!(grid[Line(0)][Column(0)], cell('1'));
    assert_eq!(grid[Line(0)][Column(1)], wrap_cell('2'));
    assert_eq!(grid[Line(0)][Column(2)], Cell::default());

    assert_eq!(grid[Line(1)].len(), 3);
    assert_eq!(grid[Line(1)][Column(0)], cell('3'));
    assert_eq!(grid[Line(1)][Column(1)], Cell::default());
    assert_eq!(grid[Line(1)][Column(2)], Cell::default());
}

#[test]
fn shrink_reflow_disabled() {
    let mut grid = Grid::<Cell>::new(1, 5, 2);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = cell('2');
    grid[Line(0)][Column(2)] = cell('3');
    grid[Line(0)][Column(3)] = cell('4');
    grid[Line(0)][Column(4)] = cell('5');

    grid.resize(false, 1, 2);

    assert_eq!(grid.total_lines(), 1);

    assert_eq!(grid[Line(0)].len(), 2);
    assert_eq!(grid[Line(0)][Column(0)], cell('1'));
    assert_eq!(grid[Line(0)][Column(1)], cell('2'));
}

#[test]
fn accurate_size_hint() {
    let grid = Grid::<Cell>::new(5, 5, 2);

    size_hint_matches_count(grid.iter_from(Point::new(Line(0), Column(0))));
    size_hint_matches_count(grid.iter_from(Point::new(Line(2), Column(3))));
    size_hint_matches_count(grid.iter_from(Point::new(Line(4), Column(4))));
    size_hint_matches_count(grid.iter_from(Point::new(Line(4), Column(2))));
    size_hint_matches_count(grid.iter_from(Point::new(Line(10), Column(10))));
    size_hint_matches_count(grid.iter_from(Point::new(Line(2), Column(10))));

    let mut iterator = grid.iter_from(Point::new(Line(3), Column(1)));
    iterator.next();
    iterator.next();
    size_hint_matches_count(iterator);

    size_hint_matches_count(grid.display_iter());
}

fn size_hint_matches_count<T>(iter: impl Iterator<Item = T>) {
    let iterator = iter.into_iter();
    let (lower, upper) = iterator.size_hint();
    let count = iterator.count();
    assert_eq!(lower, count);
    assert_eq!(upper, Some(count));
}

// https://github.com/rust-lang/rust-clippy/pull/6375
#[allow(clippy::all)]
fn cell(c: char) -> Cell {
    let mut cell = Cell::default();
    cell.c = c;
    cell
}

fn wrap_cell(c: char) -> Cell {
    let mut cell = cell(c);
    cell.flags.insert(Flags::WRAPLINE);
    cell
}

/// `scroll_up` as it was before regions moved as one slice rotation.
fn reference_scroll_up(grid: &mut Grid<usize>, region: &Range<Line>, positions: usize) {
    if region.end - region.start <= positions && region.start != 0 {
        for i in (region.start.0..region.end.0).map(Line::from) {
            grid.raw[i].reset(&grid.cursor.template);
        }
        return;
    }
    if grid.display_offset != 0 {
        grid.display_offset = min(grid.display_offset + positions, grid.max_scroll_limit);
    }
    if region.start == 0 {
        let history_before = grid.history_size();
        grid.increase_scroll_limit(positions);
        let history_added = grid.history_size().saturating_sub(history_before);
        let evicted = positions.saturating_sub(history_added);
        grid.history_origin = grid.history_origin.saturating_add(evicted as u64);
        for i in (0..region.start.0).rev().map(Line::from) {
            grid.raw.swap(i, i + positions);
        }
        grid.raw.rotate(-(positions as isize));
        let screen_lines = grid.screen_lines() as i32;
        for i in (region.end.0..screen_lines).rev().map(Line::from) {
            grid.raw.swap(i, i - positions);
        }
    } else {
        for i in (region.start.0..region.end.0 - positions as i32).map(Line::from) {
            grid.raw.swap(i, i + positions);
        }
    }
    for i in (region.end.0 - positions as i32..region.end.0).map(Line::from) {
        grid.raw[i].reset(&grid.cursor.template);
    }
}

/// `scroll_down` as it was before regions moved as one slice rotation.
fn reference_scroll_down(grid: &mut Grid<usize>, region: &Range<Line>, positions: usize) {
    if region.end - region.start <= positions {
        for i in (region.start.0..region.end.0).map(Line::from) {
            grid.raw[i].reset(&grid.cursor.template);
        }
        return;
    }
    if grid.max_scroll_limit == 0 {
        let screen_lines = grid.screen_lines() as i32;
        for i in (region.end.0..screen_lines).map(Line::from) {
            grid.raw.swap(i, i - positions as i32);
        }
        grid.raw.rotate_down(positions);
        for i in (0..positions).map(Line::from) {
            grid.raw[i].reset(&grid.cursor.template);
        }
        for i in (0..region.start.0).map(Line::from) {
            grid.raw.swap(i, i + positions);
        }
    } else {
        let range = (region.start + positions).0..region.end.0;
        for line in range.rev().map(Line::from) {
            grid.raw.swap(line, line - positions);
        }
        let range = region.start.0..(region.start + positions).0;
        for line in range.rev().map(Line::from) {
            grid.raw[line].reset(&grid.cursor.template);
        }
    }
}

/// Every retained row, top of history first, with its `occ`.
fn snapshot(grid: &Grid<usize>) -> Vec<(usize, usize)> {
    let top = -(grid.history_size() as i32);
    (top..grid.screen_lines() as i32)
        .map(|line| {
            let row = &grid[Line(line)];
            (row[Column(0)], row.occ)
        })
        .collect()
}

/// Scrolling a region moves its rows as one slice rotation when they sit
/// contiguously in the ring buffer, and row by row when they wrap. Either way
/// every line, history row, `occ`, and offset must match the row-by-row swap
/// the grid used before, with and without scrollback.
#[test]
fn region_scrolls_match_the_row_by_row_swap() {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = move |bound: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % bound as u64) as usize
    };

    let mut value = 1;
    for _ in 0..4000 {
        let lines = 3 + next(10);
        let history = [0, 3, 7][next(3)];
        let mut grid = Grid::<usize>::new(lines, 1, history);

        // Scroll the full screen a random number of times, writing fresh rows,
        // so the ring buffer starts at a random offset and regions often wrap.
        for _ in 0..next(3 * (lines + history) + 1) {
            for line in 0..lines {
                grid[Line(line as i32)][Column(0)] = value;
                value += 1;
            }
            grid.scroll_up::<usize>(&(Line(0)..Line(lines as i32)), 1 + next(2));
        }
        for line in 0..lines {
            grid[Line(line as i32)][Column(0)] = value;
            value += 1;
        }
        if history > 0 && next(4) == 0 {
            grid.scroll_display(Scroll::Delta(1 + next(history) as i32));
        }

        let start = next(lines);
        let end = start + 1 + next(lines - start);
        let region = Line(start as i32)..Line(end as i32);
        let positions = 1 + next(end - start);
        let up = next(2) == 0;

        let mut expected = grid.clone();
        if up {
            grid.scroll_up::<usize>(&region, positions);
            reference_scroll_up(&mut expected, &region, positions);
        } else {
            grid.scroll_down::<usize>(&region, positions);
            reference_scroll_down(&mut expected, &region, positions);
        }

        let case = format!(
            "{} {region:?} by {positions}, {lines} lines, history {history}",
            if up { "up" } else { "down" }
        );
        assert_eq!(snapshot(&grid), snapshot(&expected), "{case}");
        assert_eq!(grid.display_offset, expected.display_offset, "{case}");
        assert_eq!(grid.history_origin, expected.history_origin, "{case}");
    }
}
