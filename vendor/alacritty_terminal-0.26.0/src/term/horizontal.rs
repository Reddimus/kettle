use super::*;

impl<T> Term<T> {
    pub(super) fn clear_partial_wraps(&mut self) {
        for grid in [&mut self.grid, &mut self.inactive_grid] {
            for line in grid.topmost_line().0..grid.screen_lines() as i32 {
                for cell in &mut grid[Line(line)][..] {
                    cell.flags.remove(Flags::PARTIAL_WRAPLINE);
                }
            }
        }
    }

    pub(super) fn relocate_partial_wraps(&mut self, column: Column) {
        let old_column = self.horizontal_margins.end - 1;
        if old_column == column {
            return;
        }
        for grid in [&mut self.grid, &mut self.inactive_grid] {
            if old_column >= grid.columns() {
                continue;
            }
            for line in 0..grid.screen_lines() as i32 {
                let row = &mut grid[Line(line)];
                let wrapped = row[old_column].flags.contains(Flags::PARTIAL_WRAPLINE);
                row[old_column].flags.remove(Flags::PARTIAL_WRAPLINE);
                row[column].flags.set(Flags::PARTIAL_WRAPLINE, wrapped);
            }
        }
    }
}

impl<T: EventListener> Term<T> {
    pub(super) fn scroll_columns(&mut self, count: usize, insert: bool, column: Column) {
        let original = self.grid.cursor.point;
        if self.conformance_level < 4
            || count == 0
            || !self.scroll_region.contains(&original.line)
            || !self.cursor_in_horizontal_region()
        {
            return;
        }
        let columns = column..self.horizontal_region().end;
        let count = count.min(columns.end.0 - columns.start.0);
        self.selection = self.selection.take().filter(|selection| {
            self.selection_outside_region(
                selection,
                self.scroll_region.start,
                self.scroll_region.end,
                &columns,
            )
        });
        // Retire links before any earlier row can shift their spacer.
        for line in self.scroll_region.start.0..self.scroll_region.end.0 {
            self.clear_linked_leading_wide_spacer(Line(line), column, false);
            if columns.end == self.columns() {
                self.clear_leading_wide_spacer(Line(line));
            }
        }
        for line in self.scroll_region.start.0..self.scroll_region.end.0 {
            self.grid.cursor.point = Point::new(Line(line), column);
            if insert {
                self.insert_blank(count);
            } else {
                self.delete_chars(count);
            }
        }
        self.grid.cursor.point = original;
        self.mark_fully_damaged();
    }

    pub(super) fn column_index(&mut self, backward: bool) {
        if self.conformance_level < 4 {
            return;
        }
        let columns = self.horizontal_region();
        let point = self.grid.cursor.point;
        if (backward && point.column == columns.start)
            || (!backward && point.column == columns.end - 1)
        {
            self.scroll_columns(1, backward, columns.start);
        } else if backward {
            self.damage_cursor();
            self.grid.cursor.point.column = Column(point.column.0.saturating_sub(1));
            self.grid.cursor.input_needs_wrap = false;
            self.damage_cursor();
        } else {
            self.move_forward(1);
        }
    }
    pub(super) fn reverse_cursor(&mut self, mut count: usize) -> bool {
        if !self.mode.contains(TermMode::LINE_WRAP)
            || !(self.reverse_wrap || self.extended_reverse_wrap)
        {
            return false;
        }
        self.damage_cursor();
        if count > 0 && self.grid.cursor.input_needs_wrap {
            count -= 1;
        }
        self.grid.cursor.input_needs_wrap = false;
        let mut point = self.grid.cursor.point;
        let columns = self.horizontal_region();
        let left = if point.column < columns.start {
            Column(0)
        } else {
            columns.start
        };
        let right = columns.end - 1;
        let rows = if self.scroll_region.contains(&point.line) {
            self.scroll_region.clone()
        } else {
            Line(0)..Line(self.screen_lines() as i32)
        };
        while count > 0 {
            let available = point.column.0 - left.0;
            if count <= available {
                point.column -= count;
                break;
            }
            count -= available + 1;
            point.column = left;
            let previous = if point.line > rows.start {
                point.line - 1i32
            } else if self.extended_reverse_wrap {
                rows.end - 1i32
            } else {
                break;
            };
            let wrapped = self.grid[previous][self.last_column()]
                .flags
                .contains(Flags::WRAPLINE)
                || self.grid[previous][right]
                    .flags
                    .contains(Flags::PARTIAL_WRAPLINE);
            if !self.extended_reverse_wrap && !wrapped {
                break;
            }
            point = Point::new(previous, right);
            if self.extended_reverse_wrap {
                let width = (right.0 - left.0 + 1) as u128;
                let height = (rows.end - rows.start).0 as u128;
                let total = width * height;
                let position =
                    (point.line - rows.start).0 as u128 * width + (point.column.0 - left.0) as u128;
                let position = (position + total - count as u128 % total) % total;
                point.line = rows.start + (position / width) as usize;
                point.column = left + (position % width) as usize;
                break;
            }
        }
        self.grid.cursor.point = point;
        self.damage_cursor();
        true
    }
}
