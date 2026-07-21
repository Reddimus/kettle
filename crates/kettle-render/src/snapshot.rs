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

/// Max combining (zero-width) marks stored inline per [`SnapCell`]. A cell with
/// more than this many marks (pathological) has the excess dropped at capture —
/// the rendered base grapheme stays correct, only an extra accent beyond the
/// fourth is lost. Inline storage keeps `SnapCell: Copy` and the snapshot's
/// zero-allocation pooling (no per-cell `Vec`).
const MAX_ZEROWIDTH: usize = 4;

/// One viewport cell, captured verbatim from the grid's `display_iter`
/// (including wide-char spacers — the renderer's wide-cursor logic depends
/// on seeing them).
#[derive(Clone, Copy)]
pub struct SnapCell {
    /// Grid-absolute line — negative when scrolled into history, exactly as
    /// `display_iter` yields it. The renderer converts to a viewport row
    /// with `line + display_offset` where needed.
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
    /// Combining (zero-width) marks layered on `c` — a decomposed accent
    /// (`e`+U+0301), an emoji ZWJ sequence, a variation selector. Captured so
    /// the renderer draws the full grapheme rather than a stripped base char
    /// (audit v2.32.0). Stored inline (see [`MAX_ZEROWIDTH`]); read via
    /// [`SnapCell::zerowidth`], mirroring the grid `Cell::zerowidth`.
    zerowidth: [char; MAX_ZEROWIDTH],
    zerowidth_len: u8,
}

impl SnapCell {
    /// Reconstruct the grid-absolute `Point` this cell was captured at
    /// (for `SelectionRange::contains`).
    #[inline]
    pub fn point(&self) -> Point {
        Point::new(Line(self.line), Column(self.col))
    }

    /// The combining (zero-width) marks layered on this cell's base char, in
    /// order — empty when the cell carries none. Mirrors the grid
    /// `Cell::zerowidth()` (which returns `Option<&[char]>`); here an empty
    /// slice is returned instead of `None` since the inline array is always
    /// present.
    #[inline]
    pub fn zerowidth(&self) -> &[char] {
        &self.zerowidth[..self.zerowidth_len as usize]
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
            // Copy combining marks inline (the common case is none → zero work).
            let mut zerowidth = ['\0'; MAX_ZEROWIDTH];
            let mut zerowidth_len = 0u8;
            if let Some(marks) = cell.zerowidth() {
                for &mark in marks.iter().take(MAX_ZEROWIDTH) {
                    zerowidth[zerowidth_len as usize] = mark;
                    zerowidth_len += 1;
                }
            }
            self.cells.push(SnapCell {
                line: indexed.point.line.0,
                col: indexed.point.column.0,
                c: cell.c,
                fg: cell.fg,
                bg: cell.bg,
                flags: cell.flags,
                underline_color: cell.underline_color(),
                zerowidth,
                zerowidth_len,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::Term;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::Config as TermConfig;
    use alacritty_terminal::vte::ansi::Processor;
    use kettle_core::Waker;

    /// Minimal `Dimensions` so the test can build a `Term` without pulling in
    /// kettle-core's (private) `TermSize` test helper.
    struct Size {
        cols: usize,
        rows: usize,
    }
    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.rows
        }
        fn screen_lines(&self) -> usize {
            self.rows
        }
        fn columns(&self) -> usize {
            self.cols
        }
    }

    fn test_term(cols: usize, rows: usize) -> (Term<EventProxy>, Processor) {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let proxy = EventProxy::new(tx, waker);
        let term = Term::new(TermConfig::default(), &Size { cols, rows }, proxy);
        (term, Processor::new())
    }

    #[test]
    fn snapcell_zerowidth_roundtrips_combining_marks() {
        // Feed a base 'e' immediately followed by COMBINING ACUTE ACCENT
        // (U+0301): the alacritty engine attaches the accent as a zero-width
        // mark on the base cell. capture() must carry it into SnapCell.
        let (mut term, mut proc) = test_term(8, 2);
        proc.advance(&mut term, "e\u{0301}x".as_bytes());

        let mut snap = PaneSnapshot::default();
        snap.capture(&term);

        // Column 0 holds the base 'e' with the accent as its only mark.
        let base = snap
            .cells
            .iter()
            .find(|c| c.col == 0 && c.line == 0)
            .expect("base cell present");
        assert_eq!(base.c, 'e');
        assert_eq!(base.zerowidth(), &['\u{0301}']);

        // Column 1 holds plain 'x' with no marks (empty slice, not None).
        let next = snap
            .cells
            .iter()
            .find(|c| c.col == 1 && c.line == 0)
            .expect("next cell present");
        assert_eq!(next.c, 'x');
        assert!(next.zerowidth().is_empty());
    }

    #[test]
    fn snapcell_zerowidth_empty_for_plain_cells() {
        let (mut term, mut proc) = test_term(8, 2);
        proc.advance(&mut term, b"ab");
        let mut snap = PaneSnapshot::default();
        snap.capture(&term);
        assert!(snap.cells.iter().all(|c| c.zerowidth().is_empty()));
    }
}
