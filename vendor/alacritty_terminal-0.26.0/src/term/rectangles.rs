use super::*;

struct Area {
    rows: Range<usize>,
    columns: Range<Column>,
}

impl<T: EventListener> Term<T> {
    fn rect_limits(&self) -> Area {
        if self.mode.contains(TermMode::ORIGIN) {
            Area {
                rows: self.scroll_region.start.0 as usize..self.scroll_region.end.0 as usize,
                columns: self.horizontal_region(),
            }
        } else {
            Area {
                rows: 0..self.screen_lines(),
                columns: Column(0)..Column(self.columns()),
            }
        }
    }

    fn resolve_rect(&self, area: ansi::Rectangle) -> Option<Area> {
        let bounds = self.rect_limits();
        let coordinate = |value: u16, default: usize, start: usize, end: usize| {
            let relative = if value == 0 { default } else { value as usize };
            (start + relative - 1).min(end - 1)
        };
        let top = coordinate(area.top, 1, bounds.rows.start, bounds.rows.end);
        let left = coordinate(area.left, 1, bounds.columns.start.0, bounds.columns.end.0);
        let bottom = coordinate(
            area.bottom,
            bounds.rows.len(),
            bounds.rows.start,
            bounds.rows.end,
        );
        let right = coordinate(
            area.right,
            bounds.columns.end.0 - bounds.columns.start.0,
            bounds.columns.start.0,
            bounds.columns.end.0,
        );
        (top <= bottom && left <= right).then_some(Area {
            rows: top..bottom + 1,
            columns: Column(left)..Column(right + 1),
        })
    }

    fn finish_rect(&mut self, rows: Range<usize>) {
        self.selection = self.selection.take().filter(|selection| {
            !selection.intersects_range(Line(rows.start as i32)..=Line(rows.end as i32 - 1))
        });
        self.mark_fully_damaged();
    }

    fn clear_rect_leading_spacer(&mut self, line: Line, start: Column, respect: bool) -> bool {
        if start > 1
            || line == self.topmost_line()
            || !self.grid[line][Column(0)].flags.contains(Flags::WIDE_CHAR)
            || (respect
                && (self.grid[line][Column(0)].flags | self.grid[line][Column(1)].flags)
                    .contains(Flags::PROTECTED))
        {
            return false;
        }
        let last = self.last_column();
        let spacer = &mut self.grid[line - 1i32][last];
        if !spacer.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
            return false;
        }
        spacer
            .flags
            .remove(Flags::LEADING_WIDE_CHAR_SPACER | Flags::PROTECTED);
        self.selection = self
            .selection
            .take()
            .filter(|selection| !selection.intersects_range(line - 1i32..=line - 1i32));
        true
    }

    fn rect_blank(cell: &Cell, keep_attributes: bool) -> Cell {
        let mut blank = if keep_attributes {
            Cell {
                fg: cell.fg,
                bg: cell.bg,
                flags: cell.flags,
                ..Cell::default()
            }
        } else {
            Cell::default()
        };
        blank
            .flags
            .remove(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
        if keep_attributes {
            blank.set_underline_color(cell.underline_color());
        }
        blank
    }

    pub(super) fn fill_rect(&mut self, area: ansi::Rectangle, character: char) {
        let Some(area) = self.resolve_rect(area) else {
            return;
        };
        let mut cell = self.grid.cursor.template.clone();
        cell.c = self.grid.cursor.charsets[self.active_charset].map(character);
        cell.flags.remove(
            Flags::WIDE_CHAR
                | Flags::WIDE_CHAR_SPACER
                | Flags::LEADING_WIDE_CHAR_SPACER
                | Flags::WRAPLINE,
        );
        let mut first = area.rows.start;
        for row in area.rows.clone() {
            let line = Line(row as i32);
            if self.clear_rect_leading_spacer(line, area.columns.start, false) {
                first = first.min(row.saturating_sub(1));
            }
            let wrap = self.grid[line][Column(self.columns() - 1)]
                .flags
                .contains(Flags::WRAPLINE);
            self.erase_cells(line, area.columns.start, area.columns.end);
            self.grid[line][area.columns.clone()].fill(cell.clone());
            let last = Column(self.columns() - 1);
            self.grid[line][last].flags.set(Flags::WRAPLINE, wrap);
        }
        self.finish_rect(first..area.rows.end);
    }

    pub(super) fn erase_rect(&mut self, area: ansi::Rectangle, selective: bool) {
        let Some(area) = self.resolve_rect(area) else {
            return;
        };
        let bg = self.grid.cursor.template.bg;
        let respect = selective && self.protection_mode == ProtectionMode::Dec;
        let mut first = area.rows.start;
        for row in area.rows.clone() {
            let line = Line(row as i32);
            if self.clear_rect_leading_spacer(line, area.columns.start, respect) {
                first = first.min(row.saturating_sub(1));
            }
            let cells = &mut self.grid[line];
            let mut start = area.columns.start.0;
            let mut end = area.columns.end.0;
            if start > 0 && cells[Column(start)].flags.contains(Flags::WIDE_CHAR_SPACER) {
                start -= 1;
            }
            if end < cells.len() && cells[Column(end - 1)].flags.contains(Flags::WIDE_CHAR) {
                end += 1;
            }
            for col in start..end {
                let flags = cells[Column(col)].flags;
                let protected = flags.contains(Flags::PROTECTED)
                    || (flags.contains(Flags::WIDE_CHAR)
                        && col + 1 < cells.len()
                        && cells[Column(col + 1)].flags.contains(Flags::PROTECTED))
                    || (flags.contains(Flags::WIDE_CHAR_SPACER)
                        && col > 0
                        && cells[Column(col - 1)].flags.contains(Flags::PROTECTED));
                if respect && protected {
                    continue;
                }
                let wrap = flags.contains(Flags::WRAPLINE);
                let mut blank = Self::rect_blank(&cells[Column(col)], selective);
                if !selective {
                    blank.bg = bg;
                }
                blank.flags.set(Flags::WRAPLINE, wrap);
                cells[Column(col)] = blank;
            }
        }
        self.finish_rect(first..area.rows.end);
    }

    pub(super) fn copy_rect(&mut self, source: ansi::Rectangle, top: u16, left: u16) {
        let Some(source) = self.resolve_rect(source) else {
            return;
        };
        let Some(destination) = self.resolve_rect(ansi::Rectangle {
            top,
            left,
            ..ansi::Rectangle::default()
        }) else {
            return;
        };
        let height = source.rows.len().min(destination.rows.len());
        let width = (source.columns.end.0 - source.columns.start.0)
            .min(destination.columns.end.0 - destination.columns.start.0);
        // Snapshot before writing, so overlapping source and destination are independent.
        let snapshot: Vec<_> = source
            .rows
            .take(height)
            .map(|row| {
                let mut cells = self.grid[Line(row as i32)]
                    [source.columns.start..source.columns.start + width]
                    .to_vec();
                if cells[0].flags.contains(Flags::WIDE_CHAR_SPACER) {
                    cells[0] = Self::rect_blank(&cells[0], true);
                }
                if cells[width - 1].flags.contains(Flags::WIDE_CHAR) {
                    cells[width - 1] = Self::rect_blank(&cells[width - 1], true);
                }
                for cell in &mut cells {
                    cell.flags
                        .remove(Flags::WRAPLINE | Flags::LEADING_WIDE_CHAR_SPACER);
                }
                cells
            })
            .collect();
        let mut first = destination.rows.start;
        for (offset, cells) in snapshot.iter().enumerate() {
            let line = Line((destination.rows.start + offset) as i32);
            if self.clear_rect_leading_spacer(line, destination.columns.start, false) {
                first = first.min((destination.rows.start + offset).saturating_sub(1));
            }
            let last = Column(self.columns() - 1);
            let wrap = self.grid[line][last].flags.contains(Flags::WRAPLINE);
            self.erase_cells(
                line,
                destination.columns.start,
                destination.columns.start + width,
            );
            self.grid[line][destination.columns.start..destination.columns.start + width]
                .clone_from_slice(cells);
            self.grid[line][last].flags.set(Flags::WRAPLINE, wrap);
        }
        self.finish_rect(first..destination.rows.start + height);
    }
}
