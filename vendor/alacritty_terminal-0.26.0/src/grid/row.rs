//! Defines the Row type which makes up lines in the grid.

use std::cmp::{max, min};
use std::iter::FusedIterator;
use std::ops::{Index, IndexMut, Range, RangeFrom, RangeFull, RangeTo, RangeToInclusive};
use std::{mem, ptr, slice};

#[cfg(feature = "serde")]
use serde::ser::{SerializeStruct, Serializer};
#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize};

use crate::grid::GridCell;
use crate::index::Column;
use crate::term::cell::ResetDiscriminant;

/// Most allocations of evicted history rows a grid keeps for reuse.
pub(crate) const MAX_SPARES: usize = 32;

#[cfg(test)]
thread_local! {
    /// Lets a test run grids that keep every row at full width, to compare
    /// them with grids that compact their history.
    pub(crate) static COMPACT_ROWS: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// A row in the grid.
///
/// A row in scrollback history may be compact: it stores its cells up to the
/// first of a run of `T::default()` cells that reaches the end of the row, and
/// that cell stands for the whole run. Reads see every column. Mutable access
/// stores the full width again first, and rows on screen always have it,
/// unless a caller assigns a history row there through `IndexMut<Line>`;
/// a slice of such a row holds only its stored cells until it is written.
#[derive(Default, Clone, Debug)]
pub struct Row<T> {
    inner: Vec<T>,

    /// Maximum number of occupied entries.
    ///
    /// This is the upper bound on the number of elements in the row, which have been modified
    /// since the last reset. All cells after this point are guaranteed to be equal.
    pub(crate) occ: usize,

    /// Number of columns while the row is compact, and zero otherwise.
    ///
    /// A compact row stores fewer cells than this, and the last one it stores
    /// is a `T::default()` that every later column repeats.
    compact_len: usize,
}

impl<T: PartialEq> PartialEq for Row<T> {
    fn eq(&self, other: &Self) -> bool {
        if !self.is_compact() && !other.is_compact() {
            return self.inner == other.inner;
        }
        self.len() == other.len() && self.into_iter().eq(other)
    }
}

/// Rows serialize their cells at full width. `occ` is only a hint for
/// resets, and compaction can lower it.
#[cfg(feature = "serde")]
impl<T: Serialize> Serialize for Row<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Cells<'a, T>(&'a Row<T>);

        impl<T: Serialize> Serialize for Cells<'_, T> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_seq(self.0)
            }
        }

        let mut row = serializer.serialize_struct("Row", 2)?;
        row.serialize_field("inner", &Cells(self))?;
        row.serialize_field("occ", &self.occ)?;
        row.end()
    }
}

/// A row read back counts every cell as occupied: one written elsewhere, or
/// by an older version, may not keep the `occ` invariant that compaction
/// relies on to skip the cells past it.
#[cfg(feature = "serde")]
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Row<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields<T> {
            inner: Vec<T>,
            #[serde(rename = "occ")]
            _occ: usize,
        }

        let Fields { inner, .. } = Fields::deserialize(deserializer)?;
        Ok(Row {
            occ: inner.len(),
            inner,
            compact_len: 0,
        })
    }
}

impl<T: Default> Row<T> {
    /// Create a new terminal row.
    ///
    /// Ideally the `template` should be `Copy` in all performance sensitive scenarios.
    pub fn new(columns: usize) -> Row<T> {
        debug_assert!(columns >= 1);

        let mut inner: Vec<T> = Vec::with_capacity(columns);

        // This is a slightly optimized version of `std::vec::Vec::resize`.
        unsafe {
            let mut ptr = inner.as_mut_ptr();

            for _ in 1..columns {
                ptr::write(ptr, T::default());
                ptr = ptr.offset(1);
            }
            ptr::write(ptr, T::default());

            inner.set_len(columns);
        }

        Row {
            inner,
            occ: 0,
            compact_len: 0,
        }
    }

    /// Create a compact row of `columns` default cells, which stores one cell.
    pub(crate) fn new_compact(columns: usize) -> Row<T> {
        #[cfg(test)]
        if !COMPACT_ROWS.with(std::cell::Cell::get) {
            return Row::new(columns);
        }
        if columns < 2 {
            return Row::new(columns);
        }
        Row {
            inner: vec![T::default()],
            occ: 0,
            compact_len: columns,
        }
    }

    /// Store every cell of the row, if it is compact.
    #[inline]
    pub(crate) fn make_full(&mut self) {
        if self.is_compact() {
            self.expand();
        }
    }

    #[cold]
    #[inline(never)]
    fn expand(&mut self) {
        let columns = self.compact_len;
        self.inner.reserve_exact(columns - self.inner.len());
        self.inner.resize_with(columns, T::default);
        self.compact_len = 0;
    }

    /// Number of leading cells a compact copy of this full row would store
    /// before its blank, or `None` when compacting would not save a cell.
    fn compact_end(&self) -> Option<usize>
    where
        T: PartialEq,
    {
        #[cfg(test)]
        if !COMPACT_ROWS.with(std::cell::Cell::get) {
            return None;
        }
        let len = self.inner.len();
        if self.is_compact() || len < 2 {
            return None;
        }

        // Cells past `occ` all equal the last one, so when that one is default
        // there is no need to look at them.
        let blank = T::default();
        let mut end = if self.occ < len && self.inner[len - 1] == blank {
            self.occ
        } else {
            len
        };
        while end > 0 && self.inner[end - 1] == blank {
            end -= 1;
        }
        debug_assert!(
            self.inner[end..].iter().all(|cell| *cell == blank),
            "a cell past `occ` differs from the last cell"
        );
        (end + 1 < len).then_some(end)
    }

    /// Move the first `end` cells into `stored`, followed by the blank that
    /// stands for the rest, and return the full-width buffer, which is left
    /// holding only default cells.
    fn store_compact(&mut self, end: usize, mut stored: Vec<T>) -> Vec<T>
    where
        T: GridCell,
    {
        assert!(stored.is_empty(), "a spare still holds cells");
        stored.reserve_exact(end + 1);
        let blank = T::default();
        T::take_cells(&mut self.inner[..end], &mut stored);
        assert_eq!(
            stored.len(),
            end,
            "`GridCell::take_cells` moved the wrong number of cells"
        );
        stored.push(blank);
        self.occ = min(self.occ, end);
        self.compact_len = self.inner.len();
        mem::replace(&mut self.inner, stored)
    }

    /// Stop storing the default cells that end the row, keeping one to stand
    /// for them. Nothing changes when this would not save a cell.
    pub(crate) fn compact(&mut self)
    where
        T: GridCell + PartialEq,
    {
        if let Some(end) = self.compact_end() {
            self.store_compact(end, Vec::with_capacity(end + 1));
        }
    }

    /// Compact `history`, a row that just moved into history, and give its
    /// full-width buffer to `cleared`, a compact row about to be reset.
    ///
    /// `history` stores its cells in the newest of `spares` close to the size
    /// it needs, and the allocation `cleared` stored its cells in joins the
    /// spares, so scrolling through a full history seldom allocates or frees,
    /// whatever the lengths of its lines. A spare keeps its old cells until it
    /// is reused or dropped. `cleared` then holds only default cells.
    pub(crate) fn recycle(history: &mut Row<T>, cleared: &mut Row<T>, spares: &mut Vec<Vec<T>>)
    where
        T: GridCell + PartialEq,
    {
        let Some(end) = history.compact_end() else {
            return;
        };
        let fits = |spare: &Vec<T>| spare.capacity() > end && spare.capacity() <= end + end / 2 + 8;
        let stored = match spares.iter().rposition(fits) {
            Some(index) => {
                let mut spare = spares.remove(index);
                T::clear_cells(&mut spare);
                spare
            }
            None => Vec::with_capacity(end + 1),
        };
        let buffer = history.store_compact(end, stored);
        if cleared.compact_len == buffer.len() {
            let spare = mem::replace(&mut cleared.inner, buffer);
            cleared.occ = 0;
            cleared.compact_len = 0;
            // Drop the oldest, so the spares follow the lengths output has now.
            if spares.len() == MAX_SPARES {
                spares.remove(0);
            }
            spares.push(spare);
        }
    }

    /// Increase the number of columns in the row.
    #[inline]
    pub fn grow(&mut self, columns: usize)
    where
        T: PartialEq,
    {
        if self.len() >= columns {
            return;
        }
        if self.is_compact() {
            // The new cells are default, like the ones the row does not store.
            self.compact_len = columns;
            return;
        }

        // Cells past `occ` must all be equal, and the new cells are default.
        let len = self.inner.len();
        if self.occ < len && self.inner[len - 1] != T::default() {
            self.occ = len;
        }
        self.inner.resize_with(columns, T::default);
    }

    /// Reduce the number of columns in the row.
    ///
    /// This will return all non-empty cells that were removed.
    pub fn shrink(&mut self, columns: usize) -> Option<Vec<T>>
    where
        T: GridCell,
    {
        if self.len() <= columns {
            return None;
        }
        if self.is_compact() {
            if !T::default().is_empty() {
                self.expand();
            } else if self.inner.len() < columns {
                // Only cells the row does not store are cut, and they are empty.
                self.compact_len = columns;
                return None;
            } else {
                // Every unstored cell is cut, and none of them is returned.
                self.compact_len = 0;
                if self.inner.len() <= columns {
                    return None;
                }
            }
        }

        // Split off cells for a new row.
        let mut new_row = self.inner.split_off(columns);
        let index = new_row
            .iter()
            .rposition(|c| !c.is_empty())
            .map_or(0, |i| i + 1);
        new_row.truncate(index);

        self.occ = min(self.occ, columns);

        if new_row.is_empty() {
            None
        } else {
            Some(new_row)
        }
    }

    /// Reset all cells in the row to the `template` cell.
    #[inline]
    pub fn reset<D>(&mut self, template: &T)
    where
        T: ResetDiscriminant<D> + GridCell,
        D: PartialEq,
    {
        self.make_full();
        debug_assert!(!self.inner.is_empty());

        // Mark all cells as dirty if template cell changed.
        let len = self.inner.len();
        if self.inner[len - 1].discriminant() != template.discriminant() {
            self.occ = len;
        }

        // Reset every dirty cell in the row.
        for item in &mut self.inner[0..self.occ] {
            item.reset(template);
        }

        self.occ = 0;
    }
}

#[allow(clippy::len_without_is_empty)]
impl<T> Row<T> {
    /// Create a row of `vec`'s cells.
    ///
    /// The row counts every cell as occupied, whatever `occ` says: compaction
    /// skips the cells past `occ`, so a wrong one would lose cells.
    #[inline]
    pub fn from_vec(vec: Vec<T>, occ: usize) -> Row<T> {
        Row {
            occ: max(occ, vec.len()),
            inner: vec,
            compact_len: 0,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        max(self.inner.len(), self.compact_len)
    }

    /// Whether the row stores fewer cells than it has.
    #[inline]
    pub(crate) fn is_compact(&self) -> bool {
        self.compact_len != 0
    }

    /// Number of cells allocated for the row, which is less than [`Row::len`]
    /// for a compact row.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    #[inline]
    pub fn last(&self) -> Option<&T> {
        self.inner.last()
    }

    #[inline]
    pub fn last_mut(&mut self) -> Option<&mut T>
    where
        T: Default,
    {
        self.make_full();
        self.occ = self.inner.len();
        self.inner.last_mut()
    }

    #[inline]
    pub fn append(&mut self, vec: &mut Vec<T>)
    where
        T: GridCell + Default,
    {
        self.make_full();
        // Cells past `occ` must all be equal, so every appended cell counts.
        self.occ = self.inner.len() + vec.len();
        self.inner.append(vec);
    }

    #[inline]
    pub fn append_front(&mut self, mut vec: Vec<T>)
    where
        T: Default,
    {
        self.make_full();
        self.occ += vec.len();

        vec.append(&mut self.inner);
        self.inner = vec;
    }

    /// Check if all cells in the row are empty.
    #[inline]
    pub fn is_clear(&self) -> bool
    where
        T: GridCell,
    {
        self.inner.iter().all(GridCell::is_empty)
    }

    #[inline]
    pub fn front_split_off(&mut self, at: usize) -> Vec<T>
    where
        T: Default,
    {
        self.make_full();
        self.occ = self.occ.saturating_sub(at);

        let mut split = self.inner.split_off(at);
        std::mem::swap(&mut split, &mut self.inner);
        split
    }

    /// Cell `column` of a compact row which the row does not store.
    #[cold]
    #[inline(never)]
    fn unstored(&self, column: usize) -> &T {
        assert!(
            column < self.compact_len,
            "column {column} is out of bounds for a row of {} columns",
            self.len()
        );
        &self.inner[self.inner.len() - 1]
    }
}

/// Iterator over every cell of a row, including those a compact row does not
/// store.
#[derive(Clone, Debug)]
pub struct RowIter<'a, T> {
    stored: slice::Iter<'a, T>,
    blank: Option<&'a T>,
    blanks: usize,
}

impl<'a, T> Iterator for RowIter<'a, T> {
    type Item = &'a T;

    #[inline]
    fn next(&mut self) -> Option<&'a T> {
        if let Some(cell) = self.stored.next() {
            return Some(cell);
        }
        self.blanks = self.blanks.checked_sub(1)?;
        self.blank
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.stored.len() + self.blanks;
        (len, Some(len))
    }
}

impl<'a, T> DoubleEndedIterator for RowIter<'a, T> {
    #[inline]
    fn next_back(&mut self) -> Option<&'a T> {
        match self.blanks.checked_sub(1) {
            Some(blanks) => {
                self.blanks = blanks;
                self.blank
            }
            None => self.stored.next_back(),
        }
    }
}

impl<T> ExactSizeIterator for RowIter<'_, T> {}

impl<T> FusedIterator for RowIter<'_, T> {}

impl<'a, T> IntoIterator for &'a Row<T> {
    type IntoIter = RowIter<'a, T>;
    type Item = &'a T;

    #[inline]
    fn into_iter(self) -> RowIter<'a, T> {
        match self.inner.split_last() {
            Some((blank, stored)) if self.is_compact() => RowIter {
                stored: stored.iter(),
                blank: Some(blank),
                blanks: self.compact_len - stored.len(),
            },
            _ => RowIter {
                stored: self.inner.iter(),
                blank: None,
                blanks: 0,
            },
        }
    }
}

impl<'a, T: Default> IntoIterator for &'a mut Row<T> {
    type IntoIter = slice::IterMut<'a, T>;
    type Item = &'a mut T;

    #[inline]
    fn into_iter(self) -> slice::IterMut<'a, T> {
        self.make_full();
        self.occ = self.len();
        self.inner.iter_mut()
    }
}

impl<T> Index<Column> for Row<T> {
    type Output = T;

    #[inline]
    fn index(&self, index: Column) -> &T {
        match self.inner.get(index.0) {
            Some(cell) => cell,
            None => self.unstored(index.0),
        }
    }
}

impl<T: Default> IndexMut<Column> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Column) -> &mut T {
        self.make_full();
        self.occ = max(self.occ, *index + 1);
        &mut self.inner[index.0]
    }
}

// A slice of a compact row can hold only the cells it stores. Rows on screen
// are never compact; read history rows by column or through `&Row`.

impl<T> Index<Range<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: Range<Column>) -> &[T] {
        &self.inner[(index.start.0)..(index.end.0)]
    }
}

impl<T: Default> IndexMut<Range<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Range<Column>) -> &mut [T] {
        self.make_full();
        self.occ = max(self.occ, *index.end);
        &mut self.inner[(index.start.0)..(index.end.0)]
    }
}

impl<T> Index<RangeTo<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeTo<Column>) -> &[T] {
        &self.inner[..(index.end.0)]
    }
}

impl<T: Default> IndexMut<RangeTo<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeTo<Column>) -> &mut [T] {
        self.make_full();
        self.occ = max(self.occ, *index.end);
        &mut self.inner[..(index.end.0)]
    }
}

impl<T> Index<RangeFrom<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeFrom<Column>) -> &[T] {
        debug_assert_eq!(self.compact_len, 0, "slicing the end of a compact row");
        &self.inner[(index.start.0)..]
    }
}

impl<T: Default> IndexMut<RangeFrom<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeFrom<Column>) -> &mut [T] {
        self.make_full();
        self.occ = self.len();
        &mut self.inner[(index.start.0)..]
    }
}

impl<T> Index<RangeFull> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, _: RangeFull) -> &[T] {
        debug_assert_eq!(self.compact_len, 0, "slicing all of a compact row");
        &self.inner[..]
    }
}

impl<T: Default> IndexMut<RangeFull> for Row<T> {
    #[inline]
    fn index_mut(&mut self, _: RangeFull) -> &mut [T] {
        self.make_full();
        self.occ = self.len();
        &mut self.inner[..]
    }
}

impl<T> Index<RangeToInclusive<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeToInclusive<Column>) -> &[T] {
        &self.inner[..=(index.end.0)]
    }
}

impl<T: Default> IndexMut<RangeToInclusive<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeToInclusive<Column>) -> &mut [T] {
        self.make_full();
        self.occ = max(self.occ, *index.end + 1);
        &mut self.inner[..=(index.end.0)]
    }
}
