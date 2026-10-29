use super::{Rect4, display_width, fit_single_line_label, take_cols_back};

/// Renderer-side projection of `WindowState::settings_nav` + the resolved
/// field values. The UI computes labels/values (reading `Config`); the renderer
/// just paints a centered panel — a row of category tabs, then label/value
/// rows for the active category, with the focused row highlighted.
// `PartialEq` lets the renderer memoize `settings_display_lines`'s output
// against the last `SettingsOverlay` it was computed from, instead of
// re-running a `format!()` per display line on every painted frame the
// Settings overlay stays open.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsOverlay {
    /// Category tab names, in order.
    pub categories: Vec<String>,
    /// Longest label and display value across all settings categories.
    pub label_cols: usize,
    pub value_cols: usize,
    /// Index of the active category (its tab is highlighted, its rows shown).
    pub active_category: usize,
    /// Whether the active category previews the terminal background.
    pub background_page: bool,
    /// The active category's fields as (label, current-value) pairs.
    pub rows: Vec<SettingsRow>,
    /// Index into `rows` of the focused field (gets the accent highlight).
    pub focused_row: usize,
    /// The panel title, already in the UI's language ("Settings — Tabs").
    pub title: String,
    /// The key-hint footer, already in the UI's language. It names the vim
    /// keys when `vim-menu-nav` is on.
    pub hints: String,
    /// An optional contextual note shown below the keybind footer, e.g. the
    /// Graphics category's active GPU and pending restart. `None`
    /// on categories that don't need it.
    pub footer_note: Option<String>,
}

impl SettingsOverlay {
    pub(crate) fn backdrop_alpha(&self) -> f32 {
        if self.background_page { 0.30 } else { 0.55 }
    }
}

/// One settings row — a human label and its current value string.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsRow {
    pub label: String,
    pub value: String,
    /// `true` when this row doesn't apply to the current state (e.g. the image
    /// path while `background-type != image`). Drawn dimmed and skipped by
    /// keyboard/mouse nav.
    pub disabled: bool,
}

/// Number of header display-lines before the field rows in the
/// settings panel (title, category tabs, blank). The focused-row highlight
/// quad and the per-line text areas both index off this.
pub(crate) const SETTINGS_FIELD_START: usize = 3;

/// Build the settings panel's display lines from its renderer-side
/// projection — title, a category-tab strip (active category underlined), a
/// blank, one `"▸ label        value"` line per field (focused row marked),
/// a blank, then the keybind footer. Shared by the buffer-text pass and the
/// quad/area pass so they stay in lockstep (same row count + ordering).
pub(crate) fn settings_display_lines(set: &SettingsOverlay) -> Vec<String> {
    let mut lines = Vec::with_capacity(set.rows.len() + SETTINGS_FIELD_START + 2);
    lines.push(format!("⚙  {}", set.title));
    lines.push(set.categories.join("  "));
    lines.push(String::new());
    for (i, row) in set.rows.iter().enumerate() {
        let mark = if i == set.focused_row { "▸ " } else { "  " };
        lines.push(settings_row_line(set, row, mark, usize::MAX));
    }
    lines.push(String::new());
    lines.push(set.hints.clone());
    // Notes follow navigation in the buffer list. The vertical layout pins
    // them below the scrolling content, keeping pending changes visible.
    if let Some(note) = &set.footer_note {
        lines.extend(note.lines().map(str::to_string));
    }
    lines
}

/// Stable panel width from all-category column budgets and category names.
/// Contextual notes and capture prompts fit to it instead of moving the panel.
pub(crate) fn settings_panel_cols(set: &SettingsOverlay, lines: &[String]) -> f32 {
    let columns = 2 + set.label_cols + SETTINGS_COLUMN_GAP + set.value_cols;
    let tabs = lines.get(1).map_or(0, |line| display_width(line));
    columns.max(tabs).max(display_width(&set.hints)).max(44) as f32
}

pub(crate) fn settings_fit_line(line: &str, panel_w: f32, cell_w: f32) -> String {
    let text_w = (panel_w - 32.0).max(cell_w);
    let text_cols = (text_w / cell_w).floor().max(1.0) as usize;
    fit_single_line_label(line, text_cols)
}

const SETTINGS_COLUMN_GAP: usize = 2;

const SETTINGS_TAB_GAP: usize = 2;

pub(crate) fn settings_row_line(
    set: &SettingsOverlay,
    row: &SettingsRow,
    mark: &str,
    text_cols: usize,
) -> String {
    let fixed_cols = display_width(mark) + SETTINGS_COLUMN_GAP;
    if text_cols <= fixed_cols + 1 {
        return fit_single_line_label(&format!("{mark}{}", row.value), text_cols);
    }
    let available = text_cols - fixed_cols;
    // Keep at least half the remaining cells for a long value. Labels shrink
    // first when the longest value fits, then both columns ellipsize.
    let reserved_value = set.value_cols.max(1).min(available / 2);
    let label_cols = set.label_cols.min(available - reserved_value);
    let value_cols = available - label_cols;
    let label = fit_single_line_label(&row.label, label_cols);
    let value = settings_fit_value(&row.value, value_cols);
    format!(
        "{mark}{label}{}{value}",
        " ".repeat(label_cols - display_width(&label) + SETTINGS_COLUMN_GAP),
    )
}

fn settings_fit_value(value: &str, cols: usize) -> String {
    if value.ends_with('▏') && display_width(value) > cols {
        if cols <= 1 {
            return take_cols_back(value, cols);
        }
        // An inline editor must retain the current input end and its caret.
        format!("…{}", take_cols_back(value, cols - 1))
    } else {
        fit_single_line_label(value, cols)
    }
}

pub(crate) fn settings_fit_display_line(
    set: &SettingsOverlay,
    line_index: usize,
    line: &str,
    panel_w: f32,
    cell_w: f32,
) -> String {
    if let Some(row) = line_index
        .checked_sub(SETTINGS_FIELD_START)
        .and_then(|i| set.rows.get(i))
    {
        let text_cols = ((panel_w - 32.0).max(cell_w) / cell_w).floor().max(1.0) as usize;
        let mark = if line_index == SETTINGS_FIELD_START + set.focused_row {
            "▸ "
        } else {
            "  "
        };
        settings_row_line(set, row, mark, text_cols)
    } else {
        settings_fit_line(line, panel_w, cell_w)
    }
}

pub(crate) fn settings_tab_span(set: &SettingsOverlay, index: usize) -> (usize, usize) {
    let start = set
        .categories
        .iter()
        .take(index)
        .map(|c| display_width(c) + SETTINGS_TAB_GAP)
        .sum();
    let width = set.categories.get(index).map_or(0, |c| display_width(c));
    (start, width)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SettingsPanelLayout {
    pub(crate) panel_w: f32,
    pub(crate) panel_h: f32,
    pub(crate) px: f32,
    pub(crate) py: f32,
    pub(crate) row_h: f32,
    pub(crate) first_line: usize,
    pub(crate) visible_lines: usize,
    note_first: usize,
    note_lines: usize,
}

/// Live settings geometry shared with `ui_geometry` diagnostics. The renderer
/// remains the source of truth for scrolling, so automation addresses the row
/// that is actually painted instead of reconstructing the panel math.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettingsPanelGeometry {
    pub rect: Rect4,
    pub focused_rect: Rect4,
    pub first_line: usize,
    /// Visible scrolling content lines, excluding the pinned notes.
    pub visible_lines: usize,
}

/// The text-area pass and hit testing share this mapping from painted slots
/// to buffer indices. Notes occupy the last slots, outside field scrolling.
pub(crate) fn settings_painted_lines(
    layout: &SettingsPanelLayout,
) -> impl Iterator<Item = (usize, usize)> {
    (layout.first_line..layout.first_line + layout.visible_lines)
        .chain(layout.note_first..layout.note_first + layout.note_lines)
        .enumerate()
}

pub(crate) fn settings_panel_layout(
    set: &SettingsOverlay,
    lines: &[String],
    cell_w: f32,
    cell_h: f32,
    surface_w: f32,
    surface_h: f32,
) -> SettingsPanelLayout {
    let row_h = cell_h + 6.0;
    let panel_w =
        (settings_panel_cols(set, lines) * cell_w + 48.0).min((surface_w - 40.0).max(120.0));
    let panel_h = (lines.len() as f32 * row_h + 24.0).min((surface_h - 40.0).max(80.0));
    let px = ((surface_w - panel_w) * 0.5).max(0.0);
    let py = ((surface_h - panel_h) * 0.5).max(0.0);
    let capacity =
        (((panel_h - 24.0).max(row_h) / row_h).floor() as usize).clamp(1, lines.len().max(1));
    let content_end = (SETTINGS_FIELD_START + set.rows.len() + 2).min(lines.len());
    // Keep one slot for the focused field. At extreme heights, retain the
    // last notes first because the pending notice is appended last.
    let note_lines = (lines.len() - content_end).min(capacity.saturating_sub(1));
    let note_first = lines.len() - note_lines;
    let visible_lines = (capacity - note_lines).min(content_end);
    let focused_line = (SETTINGS_FIELD_START + set.focused_row).min(content_end.saturating_sub(1));
    let max_first = content_end.saturating_sub(visible_lines);
    let first_line = focused_line
        .saturating_add(1)
        .saturating_sub(visible_lines)
        .min(max_first);
    SettingsPanelLayout {
        panel_w,
        panel_h,
        px,
        py,
        row_h,
        first_line,
        visible_lines,
        note_first,
        note_lines,
    }
}

pub fn settings_panel_geometry(
    set: &SettingsOverlay,
    cell_w: f32,
    cell_h: f32,
    surface_w: f32,
    surface_h: f32,
) -> SettingsPanelGeometry {
    let lines = settings_display_lines(set);
    let layout = settings_panel_layout(set, &lines, cell_w, cell_h, surface_w, surface_h);
    let focused_line = (SETTINGS_FIELD_START + set.focused_row).min(lines.len().saturating_sub(1));
    let focused_y =
        layout.py + 12.0 + focused_line.saturating_sub(layout.first_line) as f32 * layout.row_h;
    SettingsPanelGeometry {
        rect: (layout.px, layout.py, layout.panel_w, layout.panel_h),
        focused_rect: (
            layout.px + 6.0,
            focused_y,
            layout.panel_w - 12.0,
            layout.row_h,
        ),
        first_line: layout.first_line,
        visible_lines: layout.visible_lines,
    }
}

/// What a click on the settings overlay landed on. The hit-test recomputes the
/// geometry from the SAME inputs the draw uses (`settings_display_lines` +
/// `settings_panel_layout`), so it can't drift from what's painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    /// Outside the panel entirely — dismiss the overlay.
    Outside,
    /// Inside the panel but not on an actionable row (title / blank / footer).
    Inert,
    /// The category-tab strip — switch to this category index.
    Category(usize),
    /// A field row — index into the active category's fields.
    Field(usize),
}

/// Map a cursor position to a [`SettingsHit`]. Pure (no GPU) so the row/tab
/// mapping is unit-tested. `cell_w`/`cell_h` are the renderer's cell metrics and
/// `surface_w`/`surface_h` the surface size — exactly the values the draw uses.
pub fn settings_hit_test(
    set: &SettingsOverlay,
    cell_w: f32,
    cell_h: f32,
    surface_w: f32,
    surface_h: f32,
    cursor_x: f32,
    cursor_y: f32,
) -> SettingsHit {
    let (cw, ch, sw, sh) = (cell_w, cell_h, surface_w, surface_h);
    let lines = settings_display_lines(set);
    let layout = settings_panel_layout(set, &lines, cw, ch, sw, sh);
    let (row_h, panel_w, panel_h, px, py) = (
        layout.row_h,
        layout.panel_w,
        layout.panel_h,
        layout.px,
        layout.py,
    );
    if cursor_x < px || cursor_x >= px + panel_w || cursor_y < py || cursor_y >= py + panel_h {
        return SettingsHit::Outside;
    }
    // Rows are laid out from `py + 12.0`, each `row_h` tall (mirrors the draw).
    let rel = cursor_y - (py + 12.0);
    if rel < 0.0 {
        return SettingsHit::Inert;
    }
    let slot = (rel / row_h) as usize;
    let Some((_, line)) = settings_painted_lines(&layout).nth(slot) else {
        return SettingsHit::Inert;
    };
    // Line 1 is the category-tab strip.
    if line == 1 {
        let text_left = px + 16.0;
        let strip_cols = ((panel_w - 32.0).max(cw) / cw).floor().max(1.0) as usize;
        let visible_cols = if display_width(&lines[1]) > strip_cols {
            strip_cols.saturating_sub(1) // the ellipsis is inert
        } else {
            strip_cols
        };
        for i in 0..set.categories.len() {
            let (col, width) = settings_tab_span(set, i);
            let start = text_left + col as f32 * cw;
            let end = text_left + (col + width).min(visible_cols) as f32 * cw;
            if cursor_x >= start && cursor_x < end {
                return SettingsHit::Category(i);
            }
        }
        return SettingsHit::Inert;
    }
    // Field rows: SETTINGS_FIELD_START .. SETTINGS_FIELD_START + rows.len().
    if line >= SETTINGS_FIELD_START && line < SETTINGS_FIELD_START + set.rows.len() {
        return SettingsHit::Field(line - SETTINGS_FIELD_START);
    }
    SettingsHit::Inert
}
