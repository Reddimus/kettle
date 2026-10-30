use super::{
    CompletionPalette, QuadInstance, Rect4, Translator, display_width, rect, row_thumb_floor,
};

/// A shell-owned completion list projected over the focused pane. Kettle only
/// presents these rows; accepting or executing a candidate remains the shell's
/// job.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionOverlay {
    /// The UI's language, for the card's header count.
    pub tr: Translator,
    pub pane_rect: (f32, f32, f32, f32),
    /// Exact terminal grid bounds inside `pane_rect`, excluding padding and a
    /// pane title bar. The completion card stays inside this grid whether it
    /// opens above or below the active command.
    pub grid_rect: (f32, f32, f32, f32),
    /// Inclusive screen-row span from OSC 133 prompt start through the shell
    /// cursor. Geometry must stay outside it, including multiline prompts and
    /// wrapped input.
    pub command_rows: (usize, usize),
    /// Stable start column of the editable command, captured before candidate
    /// insertion moved the cursor. Legacy publishers fall back to the grid
    /// edge.
    pub anchor_col: Option<usize>,
    pub kind: String,
    pub source: String,
    pub selected: Option<usize>,
    /// Total rows in the shell result; `candidates` is one bounded page.
    pub total: usize,
    /// Protocol v4 emphasis hint: the token the shell was completing. Used only
    /// to tint the first literal occurrence inside a label. Kettle never
    /// filters, ranks, quotes, or inserts anything from it.
    pub token: Option<String>,
    pub candidates: Vec<CompletionOverlayRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionOverlayRow {
    pub label: String,
    pub description: String,
    /// Zero-based position in the complete shell result.
    pub position: usize,
}

/// Visible candidate rows in one completion card.
pub(crate) const MAX_COMPLETION_ROWS: usize = 10;

/// Overall card width in grid columns. The published page picks a content-fit
/// width inside this band so the card neither shrink-wraps to an unreadable
/// sliver nor spans a wide pane.
pub(crate) const COMPLETION_MIN_COLUMNS: usize = 20;

pub(crate) const COMPLETION_MAX_COLUMNS: usize = 96;

const COMPLETION_MIN_LABEL_COLUMNS: usize = 8;

const COMPLETION_MAX_LABEL_COLUMNS: usize = 40;

const COMPLETION_MAX_DESCRIPTION_COLUMNS: usize = 48;

/// One column of inner padding on each side of the card.
pub(crate) const COMPLETION_PAD_COLUMNS: usize = 1;

/// Two columns between the label and description lanes; the hairline divider
/// sits on the boundary between them.
const COMPLETION_DIVIDER_COLUMNS: usize = 2;

pub(crate) const COMPLETION_BORDER: f32 = 1.0;

/// Right-edge scroll track/thumb, drawn only when the shell result is longer
/// than the visible rows.
pub(crate) const COMPLETION_SCROLL_W: f32 = 2.0;

/// Leading accent rail on the selected row only.
pub(crate) const COMPLETION_RAIL_W: f32 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CompletionPanelGeometry {
    /// The whole card, header and padding included.
    pub(crate) rect: Rect4,
    /// Header band inside the card. Part of the list container, never a row.
    pub(crate) header: Rect4,
    /// Top of the first painted candidate row.
    pub(crate) list_top: f32,
    pub(crate) row_h: f32,
    pub(crate) first: usize,
    pub(crate) rows: usize,
    /// Card width minus its side padding, in columns. The header lanes and the
    /// candidate lanes are both cut out of this.
    pub(crate) inner_columns: usize,
    pub(crate) label_x: f32,
    pub(crate) label_w: f32,
    pub(crate) label_columns: usize,
    /// Divider hairline abscissa; `None` when the page has no descriptions.
    pub(crate) divider_x: Option<f32>,
    pub(crate) description_x: f32,
    pub(crate) description_w: f32,
    pub(crate) description_columns: usize,
    pub(crate) placement: CompletionPanelPlacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionPanelPlacement {
    Above,
    Below,
}

impl CompletionPanelGeometry {
    /// Band occupied by the painted candidate rows.
    pub(crate) fn list_rect(&self) -> Rect4 {
        (
            self.rect.0,
            self.list_top,
            self.rect.2,
            self.rows as f32 * self.row_h,
        )
    }
}

/// Left header caption, e.g. `Completions · fish`.
pub(crate) fn completion_header_label(overlay: &CompletionOverlay) -> String {
    format!("{} · {}", overlay.kind, overlay.source)
}

/// Right header caption. A selected candidate reports its absolute position in
/// the shell result; otherwise the result size is spelled out.
pub(crate) fn completion_header_count(overlay: &CompletionOverlay) -> String {
    if let Some(selected) = overlay
        .selected
        .and_then(|index| overlay.candidates.get(index))
    {
        return format!("{}/{}", selected.position.saturating_add(1), overlay.total);
    }
    overlay.tr.completion_matches(overlay.total as u64)
}

pub(crate) fn completion_panel_geometry(
    overlay: &CompletionOverlay,
    cell: (f32, f32),
) -> Option<CompletionPanelGeometry> {
    let count = overlay.candidates.len();
    if count == 0 {
        return None;
    }
    let (cw, ch) = cell;
    if !cw.is_finite() || !ch.is_finite() || cw <= 0.0 || ch <= 0.0 {
        return None;
    }
    let (px, py, pw, ph) = overlay.pane_rect;
    let (gx, gy, gw, gh) = overlay.grid_rect;
    let left = px.max(gx);
    let right = (px + pw).min(gx + gw);
    let top = py.max(gy);
    let bottom = (py + ph).min(gy + gh);
    let available_w = (right - left).max(0.0);
    let grid_columns = (available_w / cw).floor().max(0.0) as usize;
    if grid_columns < COMPLETION_MIN_COLUMNS {
        return None;
    }

    // Vertical rhythm. A half-cell gap separates the card from the prompt and
    // from the top of the grid; rows are a little taller than a terminal line
    // so the labels are not boxed in. Every step is rounded to whole pixels so
    // the card stays crisp at fractional DPI.
    let half_cell = (ch * 0.5).round().max(1.0);
    let gap = half_cell.max(4.0);
    let row_h = (ch + 4.0).max((ch * 1.35).round());
    let header_h = row_h;
    // Restrained padding above and below the candidate rows. Deriving it from
    // the row's own leading keeps every glyph baseline optically centered: the
    // header and the first row are separated by exactly the same slack that
    // centers text inside a row.
    let list_pad = ((row_h - ch) * 0.5).round().max(2.0);
    let chrome_h = 2.0 * COMPLETION_BORDER + header_h + 2.0 * list_pad;

    // Prefer the detached lane above the command. If that lane cannot show the
    // requested page and the lane below can show more, flip the whole card
    // below the final wrapped row. Both lanes stay inside the terminal grid,
    // so the card never crosses pane chrome or the window's tab bar.
    let grid_rows = ((bottom - top) / ch).floor().max(0.0) as usize;
    if grid_rows == 0 {
        return None;
    }
    let command_start = overlay
        .command_rows
        .0
        .min(overlay.command_rows.1)
        .min(grid_rows - 1);
    let command_end = overlay
        .command_rows
        .0
        .max(overlay.command_rows.1)
        .min(grid_rows - 1);
    let above_bottom = top + command_start as f32 * ch - gap;
    let below_top = top + (command_end + 1) as f32 * ch + gap;
    let above_h = above_bottom - (top + half_cell);
    let below_h = (bottom - half_cell) - below_top;
    let capacity = |lane_h: f32| ((lane_h - chrome_h) / row_h).floor().max(0.0) as usize;
    let above_capacity = capacity(above_h).min(MAX_COMPLETION_ROWS);
    let below_capacity = capacity(below_h).min(MAX_COMPLETION_ROWS);
    let wanted_rows = count.min(MAX_COMPLETION_ROWS);
    let placement = if above_capacity >= wanted_rows {
        CompletionPanelPlacement::Above
    } else if below_capacity > above_capacity {
        CompletionPanelPlacement::Below
    } else if above_capacity > 0 {
        CompletionPanelPlacement::Above
    } else if below_capacity > 0 {
        CompletionPanelPlacement::Below
    } else {
        return None;
    };
    let rows = wanted_rows.min(match placement {
        CompletionPanelPlacement::Above => above_capacity,
        CompletionPanelPlacement::Below => below_capacity,
    });
    if rows == 0 {
        return None;
    }

    // Keep one row of lookahead under the selection so the next candidate is
    // already on screen when the user presses Tab again.
    let selected = overlay.selected.unwrap_or(0).min(count - 1);
    let first = selected
        .saturating_add(2)
        .saturating_sub(rows)
        .min(count - rows)
        .min(selected);

    // Content fit is computed over the whole published page, not just the
    // visible rows, so cycling inside one page never resizes the card.
    let label_columns = overlay
        .candidates
        .iter()
        .map(|candidate| display_width(&candidate.label))
        .max()
        .unwrap_or(0)
        .clamp(COMPLETION_MIN_LABEL_COLUMNS, COMPLETION_MAX_LABEL_COLUMNS);
    let description_columns = overlay
        .candidates
        .iter()
        .map(|candidate| display_width(&candidate.description))
        .max()
        .unwrap_or(0)
        .min(COMPLETION_MAX_DESCRIPTION_COLUMNS);
    let divider_columns = if description_columns > 0 {
        COMPLETION_DIVIDER_COLUMNS
    } else {
        0
    };
    let padding_columns = 2 * COMPLETION_PAD_COLUMNS;
    let content_columns = padding_columns + label_columns + divider_columns + description_columns;
    // The header is part of the card, so it participates in the content fit.
    let header_columns = padding_columns
        + display_width(&completion_header_label(overlay))
        + 2
        + display_width(&completion_header_count(overlay));
    let columns = content_columns
        .max(header_columns)
        .clamp(COMPLETION_MIN_COLUMNS, COMPLETION_MAX_COLUMNS)
        .min(grid_columns);

    // Redistribute after the clamp so paint, text preparation, and hit testing
    // agree on the lane split even when the grid is narrower than the content.
    let inner_columns = columns - padding_columns;
    let mut divider_columns = divider_columns.min(inner_columns);
    let mut label_columns = label_columns.min(inner_columns - divider_columns);
    let mut description_columns = inner_columns - divider_columns - label_columns;
    if description_columns == 0 {
        // A clamped card with no description lane must not keep a detached
        // hairline and two dead columns at its right edge. Give those columns
        // back to the label and omit the divider entirely.
        divider_columns = 0;
        label_columns = label_columns
            .saturating_add(COMPLETION_DIVIDER_COLUMNS)
            .min(inner_columns);
        description_columns = inner_columns - label_columns;
    }

    let width = columns as f32 * cw;
    let height = chrome_h + rows as f32 * row_h;
    // IDE completion follows the editable command column, not the decorative
    // prompt. Clamp toward the left only when a right-edge command would push
    // the content-fitted card outside the grid.
    let preferred_x = left + overlay.anchor_col.unwrap_or(0).min(grid_columns) as f32 * cw;
    let x = preferred_x.min((right - width).max(left));
    let y = match placement {
        CompletionPanelPlacement::Above => above_bottom - height,
        CompletionPanelPlacement::Below => below_top,
    };
    let label_x = x + COMPLETION_PAD_COLUMNS as f32 * cw;
    let label_w = label_columns as f32 * cw;
    // One free column on each side of the hairline keeps the two lanes from
    // reading as a table rule.
    let divider_x = (divider_columns > 0).then(|| (label_x + label_w + cw).round());
    let description_x = x + (COMPLETION_PAD_COLUMNS + label_columns + divider_columns) as f32 * cw;
    Some(CompletionPanelGeometry {
        rect: (x, y, width, height),
        header: (x, y + COMPLETION_BORDER, width, header_h),
        list_top: y + COMPLETION_BORDER + header_h + list_pad,
        row_h,
        first,
        rows,
        inner_columns,
        label_x,
        label_w,
        label_columns,
        divider_x,
        description_x,
        description_w: description_columns as f32 * cw,
        description_columns,
        placement,
    })
}

/// Header lane split in columns: the right-aligned count is served first and
/// the caption takes what is left, minus one separating column.
pub(crate) fn completion_header_columns(
    geometry: &CompletionPanelGeometry,
    count: &str,
) -> (usize, usize) {
    let count_columns = display_width(count).min(geometry.inner_columns);
    (
        geometry.inner_columns.saturating_sub(count_columns + 1),
        count_columns,
    )
}

pub(crate) fn push_completion_selection_quads(
    quads: &mut Vec<QuadInstance>,
    selected: bool,
    geometry: &CompletionPanelGeometry,
    row_y: f32,
    scroll_inset: f32,
    palette: &CompletionPalette,
) {
    if !selected {
        return;
    }
    let (x, _, width, _) = geometry.rect;
    // Rows never touch the card border, so the active surface spans the full
    // row height with no notch to inset around.
    quads.push(rect(
        x + COMPLETION_BORDER,
        row_y,
        (width - COMPLETION_BORDER * 2.0 - scroll_inset).max(0.0),
        geometry.row_h,
        palette.selection_bg,
        1.0,
    ));
    quads.push(rect(
        x + COMPLETION_BORDER,
        row_y,
        COMPLETION_RAIL_W,
        geometry.row_h,
        palette.rail,
        1.0,
    ));
}

/// Top and height of the scroll thumb, or `None` when the whole shell result
/// already fits the visible rows.
pub(crate) fn completion_scroll_thumb(
    overlay: &CompletionOverlay,
    geometry: &CompletionPanelGeometry,
) -> Option<(f32, f32)> {
    let total = overlay.total;
    if total <= geometry.rows {
        return None;
    }
    let (_, track_y, _, track_h) = geometry.list_rect();
    if track_h <= 0.0 {
        return None;
    }
    let first_position = overlay
        .candidates
        .get(geometry.first)
        .map(|candidate| candidate.position)
        .unwrap_or(geometry.first)
        .min(total.saturating_sub(geometry.rows));
    let visible = geometry.rows as f32 / total as f32;
    // Keep a short thumb legible on a very long result.
    let thumb_h = (track_h * visible)
        .max(row_thumb_floor(geometry.row_h))
        .min(track_h);
    let travel = (track_h - thumb_h).max(0.0);
    let progress = if total > geometry.rows {
        first_position as f32 / (total - geometry.rows) as f32
    } else {
        0.0
    };
    let thumb_y = track_y + (travel * progress.clamp(0.0, 1.0)).round();
    Some((thumb_y, thumb_h.round().max(1.0)))
}

/// Byte range of the emphasis token inside an already-fitted label.
///
/// Kettle never filters, ranks, quotes, or inserts from the token: this is the
/// only thing it is allowed to do with one. Pure ASCII compares
/// case-insensitively because shells complete case-insensitively in practice;
/// anything else needs an exact substring, since case folding a script Kettle
/// does not analyze can change byte lengths and split a grapheme.
pub(crate) fn completion_match_span(label: &str, token: &str) -> Option<(usize, usize)> {
    if token.is_empty() || token.len() > label.len() {
        return None;
    }
    if label.is_ascii() && token.is_ascii() {
        let haystack = label.as_bytes();
        let needle = token.as_bytes();
        return (0..=haystack.len() - needle.len())
            .find(|start| haystack[*start..*start + needle.len()].eq_ignore_ascii_case(needle))
            .map(|start| (start, start + needle.len()));
    }
    label.find(token).map(|start| (start, start + token.len()))
}

/// Bounds of the visible completion card. Shared with accessibility so the
/// semantic list occupies the same compact region users see on screen.
pub fn completion_overlay_rect(overlay: &CompletionOverlay, cell: (f32, f32)) -> Option<Rect4> {
    completion_panel_geometry(overlay, cell).map(|geometry| geometry.rect)
}

/// Candidate index and pixel bounds for each visible completion row. Paint
/// and accessibility share this geometry so assistive hit targets never point
/// at clipped or off-screen candidates, and so the header — which belongs to
/// the list container, not to any candidate — is never inside a row.
pub fn completion_overlay_row_rects(
    overlay: &CompletionOverlay,
    cell: (f32, f32),
) -> Vec<(usize, Rect4)> {
    let Some(geometry) = completion_panel_geometry(overlay, cell) else {
        return Vec::new();
    };
    (0..geometry.rows)
        .map(|row| {
            (
                geometry.first + row,
                (
                    geometry.rect.0,
                    geometry.list_top + row as f32 * geometry.row_h,
                    geometry.rect.2,
                    geometry.row_h,
                ),
            )
        })
        .collect()
}
