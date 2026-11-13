use super::{Rect4, display_width, fit_single_line_label};

/// Resolved settings data. Every consumer uses `settings_layout` for placement.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsOverlay {
    pub categories: Vec<String>,
    pub label_cols: usize,
    pub value_cols: usize,
    pub active_category: usize,
    pub background_page: bool,
    pub rows: Vec<SettingsRow>,
    pub focused_row: usize,
    pub title: String,
    /// Key hints as `key action` groups joined by ` · `. The footer wraps
    /// between groups, never inside one.
    pub hints: String,
    pub footer_note: Option<String>,
    pub search_query: String,
    pub search_focused: bool,
    pub search_placeholder: String,
    pub no_matches: String,
}

impl SettingsOverlay {
    pub(crate) fn backdrop_alpha(&self) -> f32 {
        if self.background_page { 0.30 } else { 0.55 }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SettingsControl {
    Toggle(bool),
    Choice,
    Text,
    #[default]
    Value,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsRow {
    pub label: String,
    pub value: String,
    pub description: String,
    pub category: usize,
    pub control: SettingsControl,
    pub disabled: bool,
}

impl SettingsRow {
    fn chip_text(&self) -> String {
        if self.control == SettingsControl::Choice && !self.value.is_empty() {
            format!("‹ {} ›", self.value)
        } else {
            self.value.clone()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsRowGeometry {
    pub index: usize,
    pub rect: Rect4,
    pub label_rect: Rect4,
    pub description_rect: Rect4,
    pub chip_rect: Rect4,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsLayout {
    pub rect: Rect4,
    pub sidebar_rect: Option<Rect4>,
    pub content_rect: Rect4,
    pub search_rect: Rect4,
    pub title_rect: Rect4,
    pub category_rects: Vec<Option<Rect4>>,
    pub category_previous: Option<(usize, Rect4)>,
    pub category_next: Option<(usize, Rect4)>,
    pub rows: Vec<SettingsRowGeometry>,
    pub groups: Vec<(usize, Rect4)>,
    pub footer: Vec<(String, Rect4)>,
    pub empty_rect: Rect4,
    pub first_row: usize,
}

impl SettingsLayout {
    /// Wheel notches are positive toward earlier categories, as for menu rows.
    pub fn pointer_hit(&self, cursor: (f32, f32), wheel: Option<i32>) -> SettingsHit {
        let hit = self.hit_test(cursor);
        if self.sidebar_rect.is_none()
            && matches!(hit, SettingsHit::Category(_))
            && let Some(direction) = wheel
        {
            return self
                .category_page(-direction.signum())
                .map_or(SettingsHit::Inert, SettingsHit::Category);
        }
        hit
    }

    fn hit_test(&self, cursor: (f32, f32)) -> SettingsHit {
        if !contains(self.rect, cursor) {
            return SettingsHit::Outside;
        }
        if contains(self.search_rect, cursor) {
            return SettingsHit::Search;
        }
        for (index, rect) in [self.category_previous, self.category_next]
            .into_iter()
            .flatten()
        {
            if contains(rect, cursor) {
                return SettingsHit::Category(index);
            }
        }
        for (i, rect) in self.category_rects.iter().enumerate() {
            if rect.is_some_and(|rect| contains(rect, cursor)) {
                return SettingsHit::Category(i);
            }
        }
        for row in &self.rows {
            if contains(row.rect, cursor) {
                return SettingsHit::Field(row.index);
            }
        }
        SettingsHit::Inert
    }

    fn category_page(&self, direction: i32) -> Option<usize> {
        if direction < 0 {
            self.category_previous.map(|(index, _)| index)
        } else if direction > 0 {
            self.category_next.map(|(index, _)| index)
        } else {
            None
        }
    }
}

/// The diameter of a settings switch's knob in a chip `height` tall.
pub(crate) fn settings_toggle_knob(height: f32) -> f32 {
    (height - 6.0).max(4.0)
}

fn inset((x, y, w, h): Rect4, padding: f32) -> Rect4 {
    let p = padding.min(w * 0.5).min(h * 0.5);
    (x + p, y + p, (w - p * 2.0).max(0.0), (h - p * 2.0).max(0.0))
}

fn contains((x, y, w, h): Rect4, point: (f32, f32)) -> bool {
    point.0 >= x && point.0 < x + w && point.1 >= y && point.1 < y + h
}

fn wrapped_lines(text: &str, width: f32, cw: f32) -> Vec<String> {
    let cols = (width / cw.max(1.0)).floor().max(1.0) as usize;
    let mut result = Vec::new();
    for source in text.lines() {
        let mut line = String::new();
        for word in source.split_whitespace() {
            if !line.is_empty() && display_width(&line) + 1 + display_width(word) > cols {
                result.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        result.push(fit_single_line_label(&line, cols));
    }
    result
}

/// Wrap key hints between their ` · ` groups, so a key stays beside what it
/// does and no line starts or ends with a separator. A group wider than the
/// line is cut with an ellipsis.
fn wrapped_hint_lines(text: &str, width: f32, cw: f32) -> Vec<String> {
    let cols = (width / cw.max(1.0)).floor().max(1.0) as usize;
    let mut result = Vec::new();
    for source in text.lines() {
        let mut line = String::new();
        for group in source.split(" · ").map(str::trim).filter(|g| !g.is_empty()) {
            if !line.is_empty() && display_width(&line) + 3 + display_width(group) > cols {
                result.push(fit_single_line_label(&std::mem::take(&mut line), cols));
            }
            if !line.is_empty() {
                line.push_str(" · ");
            }
            line.push_str(group);
        }
        result.push(fit_single_line_label(&line, cols));
    }
    result
}

/// Pixel layout shared by painting, pointer input, text editing, AX and ctl.
/// Offscreen rows have no rectangle, and pinned controls never scroll with rows.
pub fn settings_layout(
    set: &SettingsOverlay,
    cell: (f32, f32),
    surface: (f32, f32),
    text_cw: f32,
) -> SettingsLayout {
    let (sw, sh) = surface;
    let w = (sw * 0.72).clamp(560.0, 980.0).min((sw - 32.0).max(0.0));
    let h = (sh * 0.78).clamp(360.0, 760.0).min((sh - 32.0).max(0.0));
    let rect = ((sw - w) * 0.5, (sh - h) * 0.5, w, h);
    // Short panels trade the repeated title and spacing for a complete field.
    let compact = h < 240.0;
    let pad = if compact { 4.0_f32 } else { 12.0_f32 }
        .min(w / 8.0)
        .min(h / 8.0);
    let line = cell
        .1
        .max(1.0)
        .min((h / if compact { 4.0 } else { 8.0 }).max(1.0));
    let control_h = line + 12.0;
    let gap = if compact { 2.0 } else { 6.0 };
    let row_h = line * 2.0 + 16.0;
    let wide = w >= 640.0;
    let side_w = (w * 0.26).max(150.0);
    let sidebar_rect = wide.then_some((rect.0, rect.1, side_w, h));
    let content_rect = if wide {
        (rect.0 + side_w, rect.1, w - side_w, h)
    } else {
        rect
    };
    let inner = inset(content_rect, pad);
    let shared_header = !wide && inner.3 < 2.0 * control_h + row_h + 2.0 * gap + 2.0;
    let search_rect = if wide {
        (rect.0 + pad, rect.1 + pad, side_w - pad * 2.0, control_h)
    } else {
        (
            inner.0,
            inner.1,
            if shared_header {
                inner.2 * 0.38
            } else {
                inner.2
            },
            control_h,
        )
    };
    let strip = if shared_header {
        (
            inner.0 + search_rect.2 + 4.0,
            inner.1,
            inner.2 - search_rect.2 - 4.0,
            control_h,
        )
    } else {
        (inner.0, search_rect.1 + control_h + gap, inner.2, control_h)
    };
    let mut category_rects = vec![None; set.categories.len()];
    let mut category_previous = None;
    let mut category_next = None;
    if wide {
        let start = rect.1 + pad + control_h + 12.0;
        let capacity = ((h - pad - (start - rect.1)) / control_h).floor().max(0.0) as usize;
        let first = set
            .active_category
            .saturating_add(1)
            .saturating_sub(capacity);
        for (i, slot) in category_rects
            .iter_mut()
            .enumerate()
            .skip(first)
            .take(capacity)
        {
            *slot = Some((
                rect.0 + pad,
                start + (i - first) as f32 * control_h,
                side_w - 2.0 * pad,
                control_h,
            ));
        }
    } else {
        // Scroll the one-line strip just enough to keep the active tab visible.
        let widths: Vec<_> = set
            .categories
            .iter()
            .map(|name| display_width(name) as f32 * text_cw + 16.0)
            .collect();
        let overflow = widths.iter().sum::<f32>() > strip.2;
        let arrow_w = if overflow {
            (text_cw + 12.0).max(24.0).min(strip.2 / 3.0)
        } else {
            0.0
        };
        let strip_w = (strip.2 - 2.0 * arrow_w).max(0.0);
        let active = set.active_category.min(widths.len().saturating_sub(1));
        let mut first = active;
        let mut used = widths.get(active).copied().unwrap_or(0.0).min(strip_w);
        while first > 0 && used + widths[first - 1] <= strip_w {
            first -= 1;
            used += widths[first];
        }
        let mut x = strip.0 + arrow_w;
        let category_y = strip.1;
        let mut last = first;
        for i in first..widths.len() {
            let width = widths[i].min(strip_w);
            if x + width > strip.0 + arrow_w + strip_w + 0.01 {
                break;
            }
            category_rects[i] = Some((x, category_y, width, control_h));
            last = i;
            x += width;
        }
        if first > 0 {
            category_previous = Some((first - 1, (strip.0, category_y, arrow_w, control_h)));
        }
        if last + 1 < widths.len() {
            category_next = Some((
                last + 1,
                (strip.0 + strip.2 - arrow_w, category_y, arrow_w, control_h),
            ));
        }
    }
    let title_y = if wide {
        inner.1
    } else if shared_header {
        inner.1 + control_h + gap
    } else {
        search_rect.1 + 2.0 * control_h + gap * 2.0 + if compact { 0.0 } else { 6.0 }
    };
    let title_rect = (
        inner.0,
        title_y,
        inner.2,
        if compact { 0.0 } else { line * 1.25 + 6.0 },
    );
    let rows_top = title_y + title_rect.3 + if compact { 0.0 } else { 8.0 };
    let footer_gap = if compact { 2.0 } else { 12.0 };
    let mut footer_lines = wrapped_hint_lines(&set.hints, inner.2, text_cw);
    if let Some(note) = &set.footer_note {
        footer_lines.extend(wrapped_lines(note, inner.2, text_cw));
    }
    // Even in a very short window, retain a complete focused row. Pending
    // notices at the end take precedence when all the footer cannot fit.
    let footer_capacity = ((inner.1 + inner.3 - rows_top - row_h - line - 12.0) / line)
        .floor()
        .max(0.0) as usize;
    if footer_lines.len() > footer_capacity {
        footer_lines.drain(..footer_lines.len() - footer_capacity);
    }
    let footer_top = inner.1 + inner.3 - footer_lines.len() as f32 * line;
    let footer = footer_lines
        .into_iter()
        .enumerate()
        .map(|(i, text)| (text, (inner.0, footer_top + i as f32 * line, inner.2, line)))
        .collect();
    let available = (footer_top - footer_gap - rows_top).max(0.0);
    let grouped = !set.search_query.is_empty() && available >= row_h + line + 6.0;
    let focused = set.focused_row.min(set.rows.len().saturating_sub(1));
    let group_h = if grouped { line + 6.0 } else { 0.0 };
    let mut first = focused;
    let mut used = row_h + group_h;
    while first > 0 {
        let extra = row_h
            + if grouped && set.rows[first - 1].category != set.rows[first].category {
                group_h
            } else {
                0.0
            };
        if used + extra > available {
            break;
        }
        used += extra;
        first -= 1;
    }
    let mut y = rows_top;
    let mut rows = Vec::new();
    let mut groups = Vec::new();
    let mut previous = None;
    for (index, row) in set.rows.iter().enumerate().skip(first) {
        let group = grouped && previous != Some(row.category);
        if y + row_h + if group { group_h } else { 0.0 } > rows_top + available + 0.01 {
            break;
        }
        if group {
            groups.push((row.category, (inner.0, y, inner.2, line)));
            y += group_h;
        }
        // A switch also holds its knob beside the label.
        let knob = if matches!(row.control, SettingsControl::Toggle(_)) {
            settings_toggle_knob(line + 8.0) + 2.0
        } else {
            0.0
        };
        let chip_w = if row.control == SettingsControl::Text {
            inner.2 * 0.48
        } else {
            (display_width(&row.chip_text()) as f32 * text_cw + 20.0 + knob)
                .max(64.0)
                .min(inner.2 * 0.48)
        };
        let chip_rect = (inner.0 + inner.2 - chip_w, y + 2.0, chip_w, line + 8.0);
        rows.push(SettingsRowGeometry {
            index,
            rect: (inner.0, y, inner.2, row_h),
            label_rect: (
                inner.0 + 6.0,
                y + 6.0,
                (inner.2 - chip_w - 18.0).max(0.0),
                line,
            ),
            description_rect: (
                inner.0 + 6.0,
                y + line + 12.0,
                (inner.2 - 12.0).max(0.0),
                line,
            ),
            chip_rect,
        });
        y += row_h;
        previous = Some(row.category);
    }
    SettingsLayout {
        rect,
        sidebar_rect,
        content_rect,
        search_rect,
        title_rect,
        category_rects,
        category_previous,
        category_next,
        rows,
        groups,
        footer,
        empty_rect: (inner.0, rows_top, inner.2, line),
        first_row: first,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettingsPanelGeometry {
    pub rect: Rect4,
    pub focused_rect: Rect4,
    pub first_line: usize,
    pub visible_lines: usize,
}

pub fn settings_panel_geometry(
    set: &SettingsOverlay,
    cw: f32,
    ch: f32,
    sw: f32,
    sh: f32,
) -> SettingsPanelGeometry {
    let layout = settings_layout(set, (cw, ch), (sw, sh), cw);
    SettingsPanelGeometry {
        rect: layout.rect,
        focused_rect: layout
            .rows
            .iter()
            .find(|row| row.index == set.focused_row)
            .map_or(layout.empty_rect, |row| row.rect),
        first_line: layout.first_row,
        visible_lines: layout.rows.len(),
    }
}

/// Editable interior of the same chip painted for the focused row.
pub fn settings_value_rect(
    set: &SettingsOverlay,
    cw: f32,
    ch: f32,
    sw: f32,
    sh: f32,
    text_cw: f32,
) -> Rect4 {
    let layout = settings_layout(set, (cw, ch), (sw, sh), text_cw);
    layout
        .rows
        .iter()
        .find(|row| row.index == set.focused_row)
        .map_or(layout.empty_rect, |row| inset(row.chip_rect, 4.0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    Outside,
    Inert,
    Search,
    Category(usize),
    Field(usize),
}

pub fn settings_hit_test(
    set: &SettingsOverlay,
    cw: f32,
    ch: f32,
    sw: f32,
    sh: f32,
    x: f32,
    y: f32,
) -> SettingsHit {
    settings_hit_test_with_text_width(set, (cw, ch), (sw, sh), (x, y), cw)
}

pub fn settings_hit_test_with_text_width(
    set: &SettingsOverlay,
    cell: (f32, f32),
    surface: (f32, f32),
    cursor: (f32, f32),
    text_cw: f32,
) -> SettingsHit {
    let layout = settings_layout(set, cell, surface, text_cw);
    layout.pointer_hit(cursor, None)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsTextKind {
    Title,
    Category(usize),
    Heading,
    Label(usize),
    Description(usize),
    Value(usize),
    Muted,
}

pub(crate) struct SettingsTextItem {
    pub text: String,
    pub rect: Rect4,
    pub kind: SettingsTextKind,
}

pub(crate) fn settings_text_items(
    set: &SettingsOverlay,
    layout: &SettingsLayout,
    cw: f32,
) -> Vec<SettingsTextItem> {
    let mut items = Vec::new();
    let mut push = |text: &str, rect: Rect4, kind| {
        let scale = if kind == SettingsTextKind::Title {
            1.2
        } else {
            1.0
        };
        items.push(SettingsTextItem {
            text: fit_single_line_label(
                text,
                (rect.2 / (cw * scale).max(1.0)).floor().max(0.0) as usize,
            ),
            rect,
            kind,
        });
    };
    if layout.title_rect.3 > 0.0 {
        push(&set.title, layout.title_rect, SettingsTextKind::Title);
    }
    for (text, arrow) in [("<", layout.category_previous), (">", layout.category_next)] {
        if let Some((_, rect)) = arrow {
            push(text, inset(rect, 4.0), SettingsTextKind::Heading);
        }
    }
    for (i, rect) in layout.category_rects.iter().enumerate() {
        if let Some(rect) = rect {
            push(
                &set.categories[i],
                inset(*rect, 6.0),
                SettingsTextKind::Category(i),
            );
        }
    }
    if !set.search_focused {
        push(
            if set.search_query.is_empty() {
                &set.search_placeholder
            } else {
                &set.search_query
            },
            inset(layout.search_rect, 6.0),
            SettingsTextKind::Muted,
        );
    }
    for (category, rect) in &layout.groups {
        if let Some(name) = set.categories.get(*category) {
            push(name, *rect, SettingsTextKind::Heading);
        }
    }
    for row in &layout.rows {
        let data = &set.rows[row.index];
        push(
            &data.label,
            row.label_rect,
            SettingsTextKind::Label(row.index),
        );
        let mut value_rect = inset(row.chip_rect, 4.0);
        // Reserve the knob after chip-width clamping, on either end of the switch.
        if matches!(data.control, SettingsControl::Toggle(_)) {
            let knob = (settings_toggle_knob(row.chip_rect.3) + 2.0).min(value_rect.2);
            if data.control == SettingsControl::Toggle(false) || data.disabled {
                value_rect.0 += knob;
            }
            value_rect.2 -= knob;
        }
        let cols = (value_rect.2 / cw.max(1.0)).floor().max(0.0) as usize;
        let value =
            if data.control == SettingsControl::Choice && !data.value.is_empty() && cols >= 4 {
                format!("‹ {} ›", fit_single_line_label(&data.value, cols - 4))
            } else {
                data.chip_text()
            };
        push(&value, value_rect, SettingsTextKind::Value(row.index));
        push(
            &data.description,
            row.description_rect,
            SettingsTextKind::Description(row.index),
        );
    }
    if set.rows.is_empty() {
        push(&set.no_matches, layout.empty_rect, SettingsTextKind::Muted);
    }
    for (text, rect) in &layout.footer {
        push(text, *rect, SettingsTextKind::Muted);
    }
    items
}
