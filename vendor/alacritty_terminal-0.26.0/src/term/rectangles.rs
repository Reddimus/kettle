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

    pub(super) fn mark_rect(&mut self, area: ansi::Rectangle, attributes: &[u16], reverse: bool) {
        let Some(area) = self.resolve_rect(area) else {
            return;
        };
        let exact = self.attribute_extent == 2;
        let last_row = area.rows.end - 1;
        let columns = self.columns();
        for row in area.rows.clone() {
            let left = if exact || row == area.rows.start {
                area.columns.start.0
            } else {
                0
            };
            let right = if exact || row == last_row {
                area.columns.end.0
            } else {
                columns
            };
            for column in left..right {
                let cell = &mut self.grid[Line(row as i32)][Column(column)];
                if !exact && !cell.flags.contains(Flags::DRAWN) {
                    continue;
                }
                if exact {
                    if !cell.flags.contains(Flags::DRAWN) {
                        cell.c = ' ';
                    }
                    cell.flags.insert(Flags::DRAWN);
                }
                for attribute in attributes {
                    let flag = match attribute {
                        0 => Flags::BOLD | Flags::ALL_UNDERLINES | Flags::BLINK | Flags::INVERSE,
                        1 | 22 => Flags::BOLD,
                        4 | 24 => Flags::ALL_UNDERLINES,
                        5 | 25 => Flags::BLINK,
                        7 | 27 => Flags::INVERSE,
                        8 | 28 => Flags::HIDDEN,
                        _ => continue,
                    };
                    if reverse {
                        if *attribute >= 22 {
                            continue;
                        }
                        if *attribute == 4 {
                            let underline = !cell.flags.intersects(Flags::ALL_UNDERLINES);
                            cell.flags.remove(Flags::ALL_UNDERLINES);
                            cell.flags.set(Flags::UNDERLINE, underline);
                        } else if *attribute == 0 {
                            cell.flags
                                .toggle(Flags::BOLD | Flags::BLINK | Flags::INVERSE);
                            let underline = !cell.flags.intersects(Flags::ALL_UNDERLINES);
                            cell.flags.remove(Flags::ALL_UNDERLINES);
                            cell.flags.set(Flags::UNDERLINE, underline);
                        } else {
                            cell.flags.toggle(flag);
                        }
                    } else if *attribute == 0 || *attribute >= 22 {
                        cell.flags.remove(flag);
                    } else if *attribute == 4 {
                        cell.flags.remove(Flags::ALL_UNDERLINES);
                        cell.flags.insert(Flags::UNDERLINE);
                    } else {
                        cell.flags.insert(flag);
                    }
                }
            }
        }
        self.mark_fully_damaged();
    }

    fn invalidate_rect_selection(&mut self, area: &Area) {
        self.selection = self.selection.take().filter(|selection| {
            self.selection_outside_region(
                selection,
                Line(area.rows.start as i32),
                Line(area.rows.end as i32),
                &area.columns,
            )
        });
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
        blank.flags.remove(
            Flags::WIDE_CHAR
                | Flags::WIDE_CHAR_SPACER
                | Flags::LEADING_WIDE_CHAR_SPACER
                | Flags::PARTIAL_WRAPLINE,
        );
        if keep_attributes {
            blank.set_underline_color(cell.underline_color());
        }
        blank
    }

    pub(super) fn fill_rect(&mut self, area: ansi::Rectangle, character: char) {
        let Some(area) = self.resolve_rect(area) else {
            return;
        };
        self.invalidate_rect_selection(&area);
        let mut cell = self.grid.cursor.template.clone();
        cell.c = self.grid.cursor.charsets[self.active_charset].map(character);
        cell.flags.insert(Flags::DRAWN);
        cell.flags.remove(
            Flags::WIDE_CHAR
                | Flags::WIDE_CHAR_SPACER
                | Flags::LEADING_WIDE_CHAR_SPACER
                | Flags::WRAPLINE
                | Flags::PARTIAL_WRAPLINE,
        );
        let partial_column = self.horizontal_region().end - 1;
        for row in area.rows.clone() {
            let line = Line(row as i32);
            let partial_wrap = self.grid[line][partial_column]
                .flags
                .contains(Flags::PARTIAL_WRAPLINE);
            self.clear_linked_leading_wide_spacer(line, area.columns.start, false);
            let wrap = self.grid[line][Column(self.columns() - 1)]
                .flags
                .contains(Flags::WRAPLINE);
            self.erase_cells(line, area.columns.start, area.columns.end);
            self.grid[line][area.columns.clone()].fill(cell.clone());
            let last = Column(self.columns() - 1);
            self.grid[line][last].flags.set(Flags::WRAPLINE, wrap);
            self.grid[line][partial_column]
                .flags
                .set(Flags::PARTIAL_WRAPLINE, partial_wrap);
        }
        self.mark_fully_damaged();
    }

    pub(super) fn erase_rect(&mut self, area: ansi::Rectangle, selective: bool) {
        let Some(area) = self.resolve_rect(area) else {
            return;
        };
        self.invalidate_rect_selection(&area);
        let bg = self.grid.cursor.template.bg;
        let respect = selective && self.protection_mode == ProtectionMode::Dec;
        let partial_column = self.horizontal_region().end - 1;
        for row in area.rows.clone() {
            let line = Line(row as i32);
            let partial_wrap = self.grid[line][partial_column]
                .flags
                .contains(Flags::PARTIAL_WRAPLINE);
            self.clear_linked_leading_wide_spacer(line, area.columns.start, respect);
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
                blank.flags.insert(Flags::DRAWN);
                blank.flags.set(Flags::WRAPLINE, wrap);
                cells[Column(col)] = blank;
            }
            cells[partial_column]
                .flags
                .set(Flags::PARTIAL_WRAPLINE, partial_wrap);
        }
        self.mark_fully_damaged();
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
        self.invalidate_rect_selection(&Area {
            rows: destination.rows.start..destination.rows.start + height,
            columns: destination.columns.start..destination.columns.start + width,
        });
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
                    cell.flags.insert(Flags::DRAWN);
                    cell.flags.remove(
                        Flags::WRAPLINE | Flags::LEADING_WIDE_CHAR_SPACER | Flags::PARTIAL_WRAPLINE,
                    );
                }
                cells
            })
            .collect();
        let partial_column = self.horizontal_region().end - 1;
        for (offset, cells) in snapshot.iter().enumerate() {
            let line = Line((destination.rows.start + offset) as i32);
            let partial_wrap = self.grid[line][partial_column]
                .flags
                .contains(Flags::PARTIAL_WRAPLINE);
            self.clear_linked_leading_wide_spacer(line, destination.columns.start, false);
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
            self.grid[line][partial_column]
                .flags
                .set(Flags::PARTIAL_WRAPLINE, partial_wrap);
        }
        self.mark_fully_damaged();
    }
}
