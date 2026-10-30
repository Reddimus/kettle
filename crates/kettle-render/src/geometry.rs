use super::{ImePreedit, PaneSnapshot};

/// Pixel rectangle `(x, y, w, h)`.
pub type Rect4 = (f32, f32, f32, f32);

pub(crate) fn row_thumb_floor(row_h: f32) -> f32 {
    (row_h * 0.5).round().max(4.0)
}

pub(crate) fn ime_preedit_rect(
    preedit: &ImePreedit,
    origin: (f32, f32),
    cell: (f32, f32),
    shaped_size: (f32, f32),
) -> Rect4 {
    let cells = unicode_width::UnicodeWidthStr::width(preedit.text.as_str()).max(1);
    (
        origin.0 + preedit.col as f32 * cell.0,
        origin.1 + preedit.row as f32 * cell.1,
        (cells as f32 * cell.0).max(shaped_size.0).ceil(),
        cell.1.max(shaped_size.1).ceil(),
    )
}

pub(crate) fn rects_overlap(a: Rect4, b: Rect4) -> bool {
    a.0 < b.0 + b.2 && a.0 + a.2 > b.0 && a.1 < b.1 + b.3 && a.1 + a.3 > b.1
}

/// Clamp a window size into the range `Surface::configure` will accept.
///
/// Zero panics, and anything above the device's announced
/// `max_texture_dimension_2d` fails validation and leaves a stale surface that
/// paints nothing. Raising the REQUESTED limit at device creation (see
/// [`live_device_limits`]) lifts this ceiling to whatever the hardware really
/// supports, so an 8K or multi-display window keeps its true size instead of
/// being clipped at wgpu's default 8192 -- but the clamp itself has to stay.
/// Dropping it would turn a gracefully clipped window into a broken surface on
/// any adapter whose genuine limit is smaller than the window.
pub(crate) fn live_surface_dimensions(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    let max = max_dimension.max(1);
    (width.clamp(1, max), height.clamp(1, max))
}

/// Divide a bounded frame resource across independent panes in deterministic
/// round-robin order. Saturated or empty panes donate their unused share, so
/// the result consumes `min(sum(counts), limit)` slots without allowing the
/// first busy pane to starve every pane after it.
pub(crate) fn fair_placement_quotas(counts: &[usize], limit: usize) -> Vec<usize> {
    let mut quotas = vec![0; counts.len()];
    let mut remaining = limit;
    while remaining > 0 {
        let before = remaining;
        for (quota, count) in quotas.iter_mut().zip(counts.iter().copied()) {
            if *quota < count {
                *quota += 1;
                remaining -= 1;
                if remaining == 0 {
                    break;
                }
            }
        }
        if remaining == before {
            break;
        }
    }
    quotas
}

pub(crate) fn placement_is_visible(
    snap: &PaneSnapshot,
    placement: &kettle_core::Placement,
) -> bool {
    let top = u128::from(snapshot_viewport_top(snap));
    let start = u128::from(placement.abs_line);
    let end = start + placement.cell_rows as u128;
    let viewport_end = top + snap.screen_lines as u128;
    snap.screen_lines != 0 && placement.cell_rows != 0 && start < viewport_end && top < end
}

pub(crate) fn snapshot_viewport_top(snap: &PaneSnapshot) -> u64 {
    snap.history_origin
        .saturating_add(snap.history_size as u64)
        .saturating_sub(snap.display_offset.min(snap.history_size) as u64)
}

pub(crate) fn placement_viewport_row(
    snap: &PaneSnapshot,
    placement: &kettle_core::Placement,
) -> Option<i64> {
    let delta = i128::from(placement.abs_line) - i128::from(snapshot_viewport_top(snap));
    i64::try_from(delta).ok()
}

pub(crate) fn inline_placement_rect(
    base_x: f32,
    base_y: f32,
    row: i64,
    cell_width: f32,
    cell_height: f32,
    placement: &kettle_core::Placement,
) -> (f32, f32, f32, f32) {
    (
        base_x + (placement.col as f32 + placement.x_offset_cells) * cell_width,
        base_y + (row as f32 + placement.y_offset_cells) * cell_height,
        placement.display_cols * cell_width,
        placement.display_rows * cell_height,
    )
}

/// Intersect the owning pane's drawable body with its terminal grid viewport.
///
/// Inline images are cell-anchored terminal content: they must not paint the
/// pane border/titlebar, padding, sibling panes, or window chrome even when a
/// placement starts above the viewport or declares an oversized destination.
pub(crate) fn inline_image_clip(
    pane_body: (f32, f32, f32, f32),
    grid_origin: (f32, f32),
    grid_size: (usize, usize),
    cell_size: (f32, f32),
) -> Option<[f32; 4]> {
    let body = [pane_body.0, pane_body.1, pane_body.2, pane_body.3];
    let grid = [
        grid_origin.0,
        grid_origin.1,
        grid_size.0 as f32 * cell_size.0,
        grid_size.1 as f32 * cell_size.1,
    ];
    if !body.into_iter().chain(grid).all(f32::is_finite)
        || body[2] <= 0.0
        || body[3] <= 0.0
        || grid[2] <= 0.0
        || grid[3] <= 0.0
    {
        return None;
    }

    let x0 = body[0].max(grid[0]);
    let y0 = body[1].max(grid[1]);
    let x1 = (body[0] + body[2]).min(grid[0] + grid[2]);
    let y1 = (body[1] + body[3]).min(grid[1] + grid[3]);
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}

pub(crate) fn background_image_rect(
    mode: &str,
    align_horiz: &str,
    align_vert: &str,
    surface: [f32; 2],
    image: [f32; 2],
) -> [f32; 4] {
    let [sw, sh] = surface;
    let [img_w, img_h] = image;
    let (w, h) = match mode {
        "center" => (img_w, img_h),
        "scale" => {
            let scale = (sw / img_w).min(sh / img_h);
            (img_w * scale, img_h * scale)
        }
        _ => return [0.0, 0.0, sw, sh],
    };
    let x = match align_horiz {
        "left" => 0.0,
        "right" => sw - w,
        _ => (sw - w) * 0.5,
    };
    let y = match align_vert {
        "top" => 0.0,
        "bottom" => sh - h,
        _ => (sh - h) * 0.5,
    };
    [x, y, w, h]
}

pub(crate) fn rect_covers_surface(rect: [f32; 4], surface: [f32; 2]) -> bool {
    let [x, y, width, height] = rect;
    let [surface_width, surface_height] = surface;
    rect.into_iter().chain(surface).all(f32::is_finite)
        && surface_width > 0.0
        && surface_height > 0.0
        && width > 0.0
        && height > 0.0
        && x <= 0.0
        && y <= 0.0
        && x + width >= surface_width
        && y + height >= surface_height
}

/// The grid rows of a selection that are actually on screen.
///
/// Selection endpoints are grid-absolute and a selection can cover the entire
/// scrollback: `Ctrl+A` in a pane holding a million lines of build output is one
/// gesture. Walking `start..=end` and skipping the offscreen rows inside the
/// loop cost a million iterations on EVERY repaint — every blink, every
/// keystroke — to draw at most `screen_lines` quads. Clamping first makes the
/// work proportional to what is drawn.
///
/// Returns an empty range when the selection is entirely off screen, which is
/// the ordinary case while scrolled away from it.
pub(crate) fn visible_selection_rows(
    start_line: i32,
    end_line: i32,
    display_offset: i32,
    screen_rows: i32,
) -> std::ops::RangeInclusive<i32> {
    // `vrow = r + display_offset` must land in `0..screen_rows`.
    let first = start_line.max(-display_offset);
    let last = end_line.min(screen_rows - display_offset - 1);
    first..=last
}

/// The inclusive `(first_col, last_col)` the mouse selection highlights on grid
/// row `r`. A **block** (Alt+drag) selection is a column rectangle: every row
/// spans the same `min(start_col, end_col)..=max(start_col, end_col)`, matching
/// the rectangular text that's copied. A **linear** selection wraps full lines:
/// the start row begins at the anchor and runs to `cols-1`, interior rows span
/// the whole width, and the end row runs from column 0 to the cursor.
///
/// Splitting this out of the inline `build_pane` loop is what makes the
/// block/linear distinction unit-testable: the highlight quad must match the
/// copied text, and a block selection drawn with the linear (full-line) spans
/// highlighted cells the copy never includes.
pub(crate) fn selection_row_span(
    r: i32,
    start: (i32, usize),
    end: (i32, usize),
    cols: usize,
    is_block: bool,
) -> (usize, usize) {
    if is_block {
        return (start.1.min(end.1), start.1.max(end.1));
    }
    let last_col = cols.saturating_sub(1);
    if start.0 == end.0 {
        (start.1, end.1)
    } else if r == start.0 {
        (start.1, last_col)
    } else if r == end.0 {
        (0, end.1)
    } else {
        (0, last_col)
    }
}

/// Cap a cell count so `requested * cell_px + chrome_px <= 8192` —
/// the wgpu per-side texture limit. Returns at least 1 so a degenerate
/// clamp (huge font + huge padding) doesn't produce a zero-cell PNG.
/// Pure so the arithmetic is unit-tested without standing up wgpu.
pub fn cap_axis_cells(requested: u32, cell_px: f32, chrome_px: f32) -> u32 {
    const MAX_TEXTURE_PX: f32 = 8192.0;
    let cell = cell_px.max(1.0); // never divide by zero
    let safe_body = (MAX_TEXTURE_PX - chrome_px).max(cell);
    let cap = (safe_body / cell).floor() as u32;
    requested.min(cap).max(1)
}

pub(crate) fn centered_text_origin(rect: Rect4, text_size: (f32, f32)) -> (f32, f32) {
    let (x, y, w, h) = rect;
    let (text_w, text_h) = text_size;
    (
        x + ((w - text_w) * 0.5).max(0.0),
        y + ((h - text_h) * 0.5).max(0.0),
    )
}

/// AppKit's decorated-window bottom radius in logical points. The renderer
/// multiplies by the live scale factor, so the visible curve stays concentric
/// with the native mask on both Retina and non-Retina displays.
pub(crate) const MACOS_WINDOW_CORNER_RADIUS_POINTS: f32 = 16.0;

const PANE_EDGE_EPSILON: f32 = 1.0;

pub(crate) const OUTLINE_BOTTOM_RIGHT: u32 = 1 << 2;

pub(crate) const OUTLINE_BOTTOM_LEFT: u32 = 1 << 3;

pub(crate) fn pane_bottom_window_corner_mask(
    rect: (f32, f32, f32, f32),
    surface: (f32, f32),
    rounded_window_corners: bool,
) -> u32 {
    if !rounded_window_corners {
        return 0;
    }
    let (x, y, width, height) = rect;
    let (surface_width, surface_height) = surface;
    if (y + height - surface_height).abs() > PANE_EDGE_EPSILON {
        return 0;
    }
    let mut mask = 0;
    if x.abs() <= PANE_EDGE_EPSILON {
        mask |= OUTLINE_BOTTOM_LEFT;
    }
    if (x + width - surface_width).abs() <= PANE_EDGE_EPSILON {
        mask |= OUTLINE_BOTTOM_RIGHT;
    }
    mask
}

/// Surface-pixel origin of a pane's terminal grid: inside the padding of its
/// terminal rectangle, which the layout has already carved clear of the
/// pane's titlebar (above or below) and preview lane.
pub fn pane_grid_origin(terminal: (f32, f32, f32, f32), padding: (f32, f32)) -> (f32, f32) {
    (terminal.0 + padding.0, terminal.1 + padding.1)
}

/// The interior of a pane's terminal to paint with its own default
/// background: the terminal rectangle `(x, y, w, h)` inset by the border
/// width `bw`, so the backdrop never overpaints the focus border, the
/// titlebar or the preview lane beside it. `None` when the interior would be
/// empty (a degenerate pane, or a border at least half its size).
pub(crate) fn pane_backdrop_rect(
    terminal: (f32, f32, f32, f32),
    bw: f32,
) -> Option<(f32, f32, f32, f32)> {
    let (rx, ry, rw, rh) = terminal;
    let bwid = (rw - 2.0 * bw).max(0.0);
    let bhgt = (rh - 2.0 * bw).max(0.0);
    (bwid > 0.0 && bhgt > 0.0).then_some((rx + bw, ry + bw, bwid, bhgt))
}

/// Grid column of the cluster a laid-out glyph belongs to (cell-locked
/// rendering). `char_starts[k]` is the byte offset of cell k's base scalar,
/// including wide-char spacers but excluding attached zero-width marks.
/// A mark's cluster start therefore maps to the same column as its base.
pub(crate) fn glyph_grid_col(char_starts: &[u32], cluster_start: usize) -> usize {
    char_starts
        .partition_point(|&bs| (bs as usize) <= cluster_start)
        .saturating_sub(1)
}

/// Cell-locked logical pen X (physical px), snapped to an integer pixel so the
/// glyph is crisp and shares one subpixel-bin (x_bin = 0) cache slot regardless
/// of cell: the grid cell's left edge plus any intra-cluster `x_offset` (kept so
/// a combining mark still stacks on its base). It replaces cosmic-text's
/// advance-accumulated `glyph.x`. The two agree for a primary-face monospace
/// glyph (advance == cell_w), so only advance-mismatched glyphs (fallback / CJK /
/// ligature) move.
pub(crate) fn cell_locked_pen_x(cell_left: f32, x_offset_px: f32) -> f32 {
    (cell_left + x_offset_px).round()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_requires_positive_intersection_on_both_axes() {
        let rect = (10.0, 20.0, 30.0, 40.0);
        assert!(rects_overlap(rect, rect));
        assert!(rects_overlap(rect, (15.0, 25.0, 5.0, 5.0)));
        assert!(rects_overlap(rect, (39.0, 59.0, 5.0, 5.0)));
        assert!(!rects_overlap(rect, (41.0, 25.0, 5.0, 5.0)));
        assert!(!rects_overlap(rect, (15.0, 61.0, 5.0, 5.0)));
    }

    #[test]
    fn touching_edges_and_corners_do_not_overlap() {
        let rect = (10.0, 20.0, 30.0, 40.0);
        for other in [
            (40.0, 20.0, 10.0, 40.0),
            (0.0, 20.0, 10.0, 40.0),
            (10.0, 60.0, 30.0, 10.0),
            (10.0, 10.0, 30.0, 10.0),
            (40.0, 60.0, 10.0, 10.0),
        ] {
            assert!(!rects_overlap(rect, other));
            assert!(!rects_overlap(other, rect));
        }
    }

    #[test]
    fn thumb_floor_rounds_half_a_row_with_a_four_pixel_minimum() {
        for height in [-10.0, 0.0, 6.0, 8.0] {
            assert_eq!(row_thumb_floor(height), 4.0);
        }
        assert_eq!(row_thumb_floor(9.0), 5.0);
        assert_eq!(row_thumb_floor(15.0), 8.0);
        assert_eq!(row_thumb_floor(20.0), 10.0);
    }

    #[test]
    fn viewport_top_clamps_scroll_offset_and_saturates_history_origin() {
        let mut snap = PaneSnapshot {
            history_origin: 100,
            history_size: 20,
            display_offset: 5,
            ..PaneSnapshot::default()
        };
        assert_eq!(snapshot_viewport_top(&snap), 115);
        snap.display_offset = 30;
        assert_eq!(snapshot_viewport_top(&snap), 100);
        snap.history_origin = u64::MAX - 5;
        snap.display_offset = 0;
        assert_eq!(snapshot_viewport_top(&snap), u64::MAX);
    }
}
