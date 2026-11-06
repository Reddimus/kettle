//! Each pane row's bidi display layout, computed once per frame from the
//! snapshot and shared with the UI's hit tests.
//!
//! The layout itself is `kettle_core::bidi`: the renderer only decides which
//! rows need one (normal screen, `bidi` on, a strong right-to-left
//! character in the row) and keeps them while the row's cells are unchanged.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use alacritty_terminal::term::cell::Flags;
use kettle_core::bidi::{BidiCell, CellKind, RowBidi, is_strong_rtl, row_bidi};

use crate::snapshot::{PaneSnapshot, SnapCell};

/// One pane's row layouts, indexed by viewport row.
#[derive(Default, Clone)]
pub(crate) struct PaneBidi {
    rows: Vec<Option<Arc<RowBidi>>>,
    /// The hash of the cells each layout was computed from (0: no layout).
    inputs: Vec<u64>,
    scratch: Vec<BidiCell>,
}

impl PaneBidi {
    /// Recompute the layouts for `snap`. Rows whose cells did not change keep
    /// their layout; rows without right-to-left text, the alternate screen
    /// and a disabled `bidi` have none.
    pub(crate) fn update(&mut self, snap: &PaneSnapshot, enabled: bool) {
        let rows = snap.screen_lines;
        self.rows.resize(rows, None);
        self.inputs.resize(rows, 0);
        if !enabled || snap.alt_screen || !snap.has_rtl() {
            self.rows.fill(None);
            self.inputs.fill(0);
            return;
        }
        let display_offset = snap.display_offset as i32;
        let mut seen = vec![false; rows];
        for row_cells in snap.cells.chunk_by(|a, b| a.line == b.line) {
            let Some(row) = usize::try_from(row_cells[0].line + display_offset)
                .ok()
                .filter(|&row| row < rows)
            else {
                continue;
            };
            seen[row] = true;
            if !row_cells.iter().any(|cell| is_strong_rtl(cell.c)) {
                self.rows[row] = None;
                self.inputs[row] = 0;
                continue;
            }
            self.scratch.clear();
            let mut hasher = std::hash::DefaultHasher::new();
            for cell in row_cells {
                let cell = bidi_cell(cell);
                cell.hash(&mut hasher);
                self.scratch.push(cell);
            }
            // Never 0, which marks a row without a layout.
            let key = hasher.finish() | 1;
            if self.inputs[row] != key {
                self.rows[row] = row_bidi(&self.scratch).map(Arc::new);
                self.inputs[row] = key;
            }
        }
        for (row, seen) in seen.into_iter().enumerate() {
            if !seen {
                self.rows[row] = None;
                self.inputs[row] = 0;
            }
        }
    }

    /// The layout of viewport row `row`, or `None` for the identity.
    pub(crate) fn row(&self, row: i32) -> Option<&RowBidi> {
        usize::try_from(row)
            .ok()
            .and_then(|row| self.rows.get(row))
            .and_then(Option::as_deref)
    }

    /// Whether any row has a layout.
    pub(crate) fn any(&self) -> bool {
        self.rows.iter().any(Option::is_some)
    }

    /// The layouts, shared without copying them.
    pub(crate) fn rows(&self) -> Vec<Option<Arc<RowBidi>>> {
        self.rows.clone()
    }

    pub(crate) fn clear(&mut self) {
        self.rows.clear();
        self.inputs.clear();
    }
}

/// What the layout needs from a snapshot cell: its raw character (an inline
/// card's placeholder, a hidden cell's text), its width and its joiners.
fn bidi_cell(cell: &SnapCell) -> BidiCell {
    let (c, kind) = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
        (' ', CellKind::WideSpacer)
    } else if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
        (' ', CellKind::Narrow)
    } else if cell.flags.contains(Flags::WIDE_CHAR) {
        (cell.c, CellKind::WideLead)
    } else {
        (cell.c, CellKind::Narrow)
    };
    BidiCell::new(c, kind, cell.zerowidth())
}

/// The row layouts a frame drew for one pane, kept so a pointer over that
/// pane maps to the logical cell under it.
#[derive(Clone)]
pub struct PaintedBidi {
    /// The pane's terminal rectangle (x, y, width, height), as drawn.
    pub terminal: (f32, f32, f32, f32),
    rows: Vec<Option<Arc<RowBidi>>>,
}

impl PaintedBidi {
    pub(crate) fn new(terminal: (f32, f32, f32, f32), rows: Vec<Option<Arc<RowBidi>>>) -> Self {
        Self { terminal, rows }
    }

    /// The layout of a viewport row, or `None` for the identity.
    pub fn row(&self, row: usize) -> Option<&RowBidi> {
        self.rows.get(row).and_then(Option::as_deref)
    }
}
