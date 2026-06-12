//! v2.20.0 P2 (perf): decouple PTY parsing from GPU rendering.
//!
//! `redraw` used to hand the renderer a `&Term<EventProxy>` borrowed from a
//! held `MutexGuard` — so every pane's Term lock stayed held across the WHOLE
//! GPU frame (cosmic-text shaping, `surface.get_current_texture()` which can
//! block up to a vsync, submit, present). Under output flood, frames fire at
//! the 16ms coalescer budget, which kept the lock held nearly continuously
//! and starved the PTY reader thread (`processor.advance` blocks on the same
//! lock). Measured cost on the v2.19.0 baseline: 0.42–0.8 MB/s throughput vs
//! 3–9 MB/s for WT / Alacritty / WezTerm on the identical harness.
//!
//! The fix: capture the pane's renderable state into a [`PaneSnapshot`]
//! while the lock is held — a µs-scale flat copy — then drop the guard and
//! render from the snapshot. Cells are captured RAW (unresolved `AnsiColor`,
//! raw `Flags`) in exact `display_iter` order, so the renderer's per-cell
//! loop (SGR resolution, INVERSE swap, DIM blend, minimum-contrast lift,
//! run merging) runs byte-identical logic — it just no longer holds the lock
//! while doing it.
//!
//! Snapshots are pooled per window (`WindowState::pane_snapshots`): the
//! `cells` Vec keeps its high-water capacity across frames, so steady-state
//! capture does zero allocation.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::RenderableCursor;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors as TermColors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape};
use kettle_core::EventProxy;

/// One viewport cell, captured verbatim from the grid's `display_iter`
/// (including wide-char spacers — the renderer's wide-cursor logic depends
/// on seeing them).
#[derive(Clone, Copy)]
pub struct SnapCell {
    /// Grid-absolute line — negative when scrolled into history, exactly as
    /// `display_iter` yields it. The renderer converts to a viewport row
    /// with `line + display_offset` (cycle 912) where needed.
    pub line: i32,
    pub col: usize,
    pub c: char,
    /// RAW (unresolved) colors — `color::resolve` runs render-side so the
    /// per-cell pipeline stays identical to the borrowed-Term era.
    pub fg: AnsiColor,
    pub bg: AnsiColor,
    pub flags: Flags,
    /// SGR 58 per-cell underline color (neovim spell squiggles).
    pub underline_color: Option<AnsiColor>,
}

impl SnapCell {
    /// Reconstruct the grid-absolute `Point` this cell was captured at
    /// (for `SelectionRange::contains`).
    #[inline]
    pub fn point(&self) -> Point {
        Point::new(Line(self.line), Column(self.col))
    }
}

/// Everything the renderer reads from a pane's `Term`, captured under the
/// Term lock so the GPU frame can run lock-free. Mirrors alacritty's
/// `RenderableContent` plus the grid dimensions the scrollbar / image
/// anchoring need.
pub struct PaneSnapshot {
    /// Viewport cells in `display_iter` order (row-major, all columns).
    pub cells: Vec<SnapCell>,
    pub cursor: RenderableCursor,
    /// Grid-absolute selection range (`contains` does the point math).
    pub selection: Option<SelectionRange>,
    /// Full 269-slot color table (OSC 4/10/11/12 overrides + dims) — `Copy`
    /// in alacritty, ~1KB flat.
    pub colors: TermColors,
    pub display_offset: usize,
    pub columns: usize,
    pub screen_lines: usize,
    pub history_size: usize,
}

impl Default for PaneSnapshot {
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            cursor: RenderableCursor {
                shape: CursorShape::Hidden,
                point: Point::new(Line(0), Column(0)),
            },
            selection: None,
            colors: TermColors::default(),
            display_offset: 0,
            columns: 0,
            screen_lines: 0,
            history_size: 0,
        }
    }
}

impl PaneSnapshot {
    /// Capture `term`'s renderable state into this (pooled) snapshot.
    ///
    /// Called with the Term mutex held; everything here is a flat copy —
    /// no shaping, no resolution, no allocation once `cells` has reached
    /// its high-water capacity.
    pub fn capture(&mut self, term: &Term<EventProxy>) {
        let grid = term.grid();
        self.columns = grid.columns();
        self.screen_lines = grid.screen_lines();
        self.history_size = grid.history_size();

        let content = term.renderable_content();
        self.display_offset = content.display_offset;
        self.cursor = content.cursor;
        self.selection = content.selection;
        self.colors = *content.colors;

        self.cells.clear();
        self.cells
            .reserve(self.columns.saturating_mul(self.screen_lines));
        for indexed in content.display_iter {
            let cell = indexed.cell;
            self.cells.push(SnapCell {
                line: indexed.point.line.0,
                col: indexed.point.column.0,
                c: cell.c,
                fg: cell.fg,
                bg: cell.bg,
                flags: cell.flags,
                underline_color: cell.underline_color(),
            });
        }
    }
}
