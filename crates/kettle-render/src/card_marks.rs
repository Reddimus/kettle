//! Pooled complete marks for inline-card recognition after snapshot capture.

use std::ops::Range;

const MAX_CELLS: usize = 4096;
const MAX_MARKS: usize = MAX_CELLS * 8;
const PLACEHOLDER: char = '\u{10eeee}';

#[derive(Debug)]
struct CardMarkCell {
    snapshot_index: usize,
    marks: Range<usize>,
}

#[derive(Default)]
pub struct CardMarks {
    cells: Vec<CardMarkCell>,
    marks: Vec<char>,
}

impl CardMarks {
    pub(crate) fn clear(&mut self) {
        self.cells.clear();
        self.marks.clear();
    }

    pub(crate) fn collect(&mut self, snapshot_index: usize, base: char, marks: &[char]) {
        if base != PLACEHOLDER || marks.len() <= 4 {
            return;
        }
        if self.cells.len() == MAX_CELLS || marks.len() > MAX_MARKS - self.marks.len() {
            return;
        }
        let start = self.marks.len();
        self.marks.extend_from_slice(marks);
        self.cells.push(CardMarkCell {
            snapshot_index,
            marks: start..self.marks.len(),
        });
    }

    /// Complete collected sequences remain usable when later cells do not fit.
    /// Recognition still validates each complete visible footprint separately.
    pub fn cells(&self) -> impl ExactSizeIterator<Item = (usize, &[char])> + '_ {
        self.cells
            .iter()
            .map(|cell| (cell.snapshot_index, &self.marks[cell.marks.clone()]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EIGHT: [char; 8] = [
        '\u{0305}', '\u{030d}', '\u{030e}', '\u{0310}', '\u{0312}', '\u{033d}', '\u{033e}',
        '\u{033f}',
    ];

    #[test]
    fn complete_sequence_is_owned_and_indexed() {
        let mut side = CardMarks::default();
        let mut source = EIGHT;
        side.collect(7, PLACEHOLDER, &source);
        source.fill('x');
        let cells: Vec<_> = side.cells().collect();
        assert_eq!(cells, vec![(7, EIGHT.as_slice())]);
    }

    #[test]
    fn ordinary_combining_and_kitty_cells_are_not_candidates() {
        let mut side = CardMarks::default();
        side.collect(0, 'e', &EIGHT);
        side.collect(1, PLACEHOLDER, &EIGHT[..4]);
        assert!(side.cells().len() == 0);
        assert!(side.marks.is_empty());
    }

    #[test]
    fn overwrite_clears_candidates_and_reuses_storage() {
        let mut side = CardMarks::default();
        side.collect(0, PLACEHOLDER, &EIGHT);
        let capacities = (side.cells.capacity(), side.marks.capacity());
        side.clear();
        assert!(side.cells().len() == 0);
        side.collect(0, 'x', &[]);
        assert!(side.cells().len() == 0);
        assert_eq!(capacities, (side.cells.capacity(), side.marks.capacity()));
    }

    #[test]
    fn exact_budget_keeps_complete_prefix_after_later_overflow() {
        let mut side = CardMarks::default();
        for index in 0..MAX_CELLS {
            side.collect(index, PLACEHOLDER, &EIGHT);
        }
        assert_eq!(side.cells().len(), MAX_CELLS);
        assert_eq!(side.marks.len(), MAX_MARKS);
        side.collect(MAX_CELLS, PLACEHOLDER, &EIGHT);
        assert_eq!(side.cells().len(), MAX_CELLS);
        assert_eq!(side.marks.len(), MAX_MARKS);
        side.clear();
        side.collect(3, PLACEHOLDER, &EIGHT);
        assert_eq!(side.cells().next().unwrap().0, 3);
    }

    #[test]
    fn malformed_sequence_is_preserved_or_refused_without_truncation() {
        let mut side = CardMarks::default();
        side.collect(0, PLACEHOLDER, &EIGHT[..7]);
        assert_eq!(side.cells().next().unwrap(), (0, &EIGHT[..7]));
        let excessive = vec!['\u{0305}'; MAX_MARKS + 1];
        side.collect(1, PLACEHOLDER, &excessive);
        assert_eq!(side.cells().len(), 1);
        assert_eq!(side.marks.len(), 7);
        side.collect(2, PLACEHOLDER, &EIGHT);
        let cells: Vec<_> = side.cells().collect();
        assert_eq!(cells[1], (2, EIGHT.as_slice()));
        assert_eq!(side.marks.len(), 15);
    }
}
