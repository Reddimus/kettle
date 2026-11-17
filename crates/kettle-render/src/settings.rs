use super::{Rect4, display_width, fit_single_line_label};
use kettle_config::Rgb;

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
    /// Keys and their translated actions stay together when hints wrap.
    pub hints: Vec<(String, String)>,
    pub footer_note: Option<String>,
    pub search_query: String,
    pub search_focused: bool,
    pub search_placeholder: String,
    pub no_matches: String,
    /// The Reset chip's label in the UI's language. Layout, hit testing,
    /// painting and the accessibility tree all measure this one string.
    pub reset_label: String,
    /// The accessible name of the selected row's Reset button, with the
    /// default it restores ("Reset Font size to 14 px"); empty where the
    /// selected row has no Reset.
    pub reset_name: String,
}

impl SettingsOverlay {
    pub fn heading(&self, index: usize) -> &str {
        let row = &self.rows[index];
        if self.search_query.is_empty() {
            &row.group
        } else {
            self.categories.get(row.category).map_or("", String::as_str)
        }
    }

    pub(crate) fn backdrop_alpha(&self) -> f32 {
        if self.background_page { 0.30 } else { 0.55 }
    }
}

/// How strongly the focused row and the active category are filled with the
/// accent. Their focus bar, not the fill, shows focus, so the fill stays
/// light enough for the row's text to keep most of its contrast.
pub(crate) const SETTINGS_SELECTED_ALPHA: f64 = 0.08;

/// How far a description may fall below its label: at least this fraction
/// of the label's contrast, between `TEXT_CONTRAST` and
/// `SECONDARY_TEXT_CONTRAST`. A theme whose labels are only 7:1 still keeps
/// its descriptions visibly quieter than its labels.
pub(crate) const SETTINGS_DESCRIPTION_SHARE: f64 = 0.8;

/// The same on the focused row, whose fill already sets it apart.
pub(crate) const SETTINGS_FOCUSED_DESCRIPTION_SHARE: f64 = 0.9;

/// The Settings panel's surfaces, strokes and inks. The panel is opaque, so
/// they are resolved against the theme's own background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SettingsColors {
    /// The focused row's fill and the active category's.
    pub(crate) selected: Rgb,
    /// The panel's own outline and the sidebar divider: `ICON_CONTRAST`
    /// against the panel, so the panel's edge shows over the dimmed window.
    pub(crate) border: Rgb,
    /// The outlines of fields, choices, switches, Reset, the search field and
    /// the category arrows, and an off switch's knob. They show where a
    /// control is, so they keep `TEXT_CONTRAST` against the panel where the
    /// theme allows and never less than `ICON_CONTRAST`.
    pub(crate) control: Rgb,
    /// `control` on the selected row, where the outline also has to stand
    /// out from the row's fill around it.
    pub(crate) selected_control: Rgb,
    pub(crate) well: Rgb,
    pub(crate) well_control: Rgb,
    pub(crate) selected_well_control: Rgb,
    pub(crate) unavailable_control: Rgb,
    /// Descriptions, footer hints and notes, the search placeholder and an
    /// unavailable switch's label, on the panel.
    pub(crate) description: Rgb,
    /// A description on the focused row's fill.
    pub(crate) description_selected: Rgb,
    /// The bar beside the focused row and the active category:
    /// `ICON_CONTRAST` against both the panel and the fill.
    pub(crate) focus: Rgb,
}

/// The key `Colors` keeps the resolved `SettingsColors` under.
pub(crate) type SettingsColorKey = ([u32; 4], bool);

pub(crate) fn settings_color_key(
    theme: &kettle_config::Theme,
    accent: Rgb,
    linear: bool,
) -> SettingsColorKey {
    (
        [theme.background, theme.foreground, theme.palette[8], accent]
            .map(crate::MinimumContrastCache::color_key),
        linear,
    )
}

pub(crate) fn settings_colors(
    theme: &kettle_config::Theme,
    accent: Rgb,
    linear: bool,
    colors: &mut super::chrome::Colors,
) -> SettingsColors {
    use super::chrome::{ICON_CONTRAST, TEXT_CONTRAST};
    use super::color::with_min_contrast_on_both;
    let key = settings_color_key(theme, accent, linear);
    if let Some((saved, palette)) = colors.settings
        && saved == key
    {
        return palette;
    }
    let bg = theme.background;
    let selected = colors.surface(bg, accent, SETTINGS_SELECTED_ALPHA, linear);
    let quiet = colors.surface(bg, theme.palette[8], 0.65, linear);
    let border = colors.icon(quiet, bg);
    // A mid-tone theme can leave no ink at the text floor against both the
    // panel and the fill; the outline then keeps the icon floor.
    let floored = |ink: Rgb, minimum: f64| {
        let ink = with_min_contrast_on_both(ink, bg, selected, minimum, false);
        let weakest =
            super::color::contrast_ratio(ink, bg).min(super::color::contrast_ratio(ink, selected));
        (weakest >= minimum).then_some(ink)
    };
    let control = colors.text(quiet, bg);
    let selected_control = floored(control, TEXT_CONTRAST)
        .or_else(|| floored(control, ICON_CONTRAST))
        .unwrap_or_else(|| with_min_contrast_on_both(control, bg, selected, ICON_CONTRAST, false));
    let control = if super::color::contrast_ratio(control, bg) >= TEXT_CONTRAST {
        control
    } else {
        colors.icon(quiet, bg)
    };
    let well = colors.surface(bg, theme.foreground, 0.06, linear);
    let well_control = settings_outline(control, well, bg);
    // A joined Reset exposes the panel as well as the well on its inside.
    let surfaces = [bg, well, selected];
    let darkest = *surfaces
        .iter()
        .min_by(|a, b| {
            super::color::relative_luminance(**a).total_cmp(&super::color::relative_luminance(**b))
        })
        .unwrap();
    let lightest = *surfaces
        .iter()
        .max_by(|a, b| {
            super::color::relative_luminance(**a).total_cmp(&super::color::relative_luminance(**b))
        })
        .unwrap();
    let selected_well_control = settings_outline(control, darkest, lightest);
    let unavailable_control = with_min_contrast_on_both(border, bg, selected, ICON_CONTRAST, false);
    let description =
        settings_quieter_ink(colors, theme.foreground, bg, SETTINGS_DESCRIPTION_SHARE);
    let description_selected = settings_quieter_ink(
        colors,
        theme.foreground,
        selected,
        SETTINGS_FOCUSED_DESCRIPTION_SHARE,
    );
    let focus = with_min_contrast_on_both(accent, bg, selected, ICON_CONTRAST, true);
    let palette = SettingsColors {
        selected,
        border,
        control,
        selected_control,
        well,
        well_control,
        selected_well_control,
        unavailable_control,
        description,
        description_selected,
        focus,
    };
    colors.settings = Some((key, palette));
    palette
}

fn settings_outline(ink: Rgb, inside: Rgb, outside: Rgb) -> Rgb {
    use super::chrome::{ICON_CONTRAST, TEXT_CONTRAST};
    use super::color::{contrast_ratio, with_min_contrast_on_both};
    let ink = with_min_contrast_on_both(ink, inside, outside, TEXT_CONTRAST, false);
    if contrast_ratio(ink, inside).min(contrast_ratio(ink, outside)) >= TEXT_CONTRAST {
        ink
    } else {
        with_min_contrast_on_both(ink, inside, outside, ICON_CONTRAST, false)
    }
}

/// `fg`'s readable ink on `surface`, moved toward the surface until its
/// contrast falls to `share` of the label's, kept between `TEXT_CONTRAST` and
/// `SECONDARY_TEXT_CONTRAST`. Never moved further than `muted` moves (55%).
pub(crate) fn settings_quieter_ink(
    colors: &mut super::chrome::Colors,
    fg: Rgb,
    surface: Rgb,
    share: f64,
) -> Rgb {
    use super::chrome::{SECONDARY_TEXT_CONTRAST, TEXT_CONTRAST};
    use super::color::{contrast_ratio, over};
    let label = colors.text(fg, surface);
    let target =
        (share * contrast_ratio(label, surface)).clamp(TEXT_CONTRAST, SECONDARY_TEXT_CONTRAST);
    let reaches = |t: f64| contrast_ratio(over(label, surface, t, false), surface) >= target;
    let mut dim = 0.0_f64;
    if reaches(0.0) {
        let (mut lo, mut hi) = (0.0_f64, 0.55_f64);
        if reaches(hi) {
            lo = hi;
        }
        while hi - lo > 1.0 / 16384.0 {
            let mid = (lo + hi) / 2.0;
            if reaches(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        dim = lo;
    }
    colors.text(over(label, surface, dim, false), surface)
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
    pub group: String,
    pub control: SettingsControl,
    pub disabled: bool,
    /// Whether Reset covers this row: `None` where it does not (the theme,
    /// shortcuts, free text), otherwise whether the value differs from its
    /// default, which is when Reset can act.
    pub reset: Option<bool>,
    /// The switch's effect is unavailable in this session, or still being
    /// checked: it is drawn without the accent fill, its knob hollow.
    pub unavailable: bool,
    /// Columns of the widest value this row can show in the UI's language,
    /// so its control keeps one width while the value changes. `0` sizes it
    /// to the value shown.
    pub value_cols: usize,
}

impl SettingsRow {
    fn chip_text(&self) -> String {
        if self.control == SettingsControl::Choice && !self.value.is_empty() {
            format!("‹ {} ›", self.value)
        } else {
            self.value.clone()
        }
    }

    /// Columns the chip's text is sized for: the value shown, or the widest
    /// it can show, with a choice's arrows.
    fn chip_cols(&self) -> usize {
        let stable = match self.control {
            SettingsControl::Choice if self.value_cols > 0 => self.value_cols + 4,
            _ => self.value_cols,
        };
        display_width(&self.chip_text()).max(stable)
    }
}

/// A row's Reset chip as laid out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettingsResetChip {
    pub rect: Rect4,
    /// The row differs from its default, so the chip resets it. A disabled
    /// chip shows on the focused row at its default only.
    pub enabled: bool,
    /// Too narrow beside the label for the word: the chip shows the reset
    /// icon alone. Its accessible name stays the full one.
    pub icon: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsRowGeometry {
    pub index: usize,
    pub rect: Rect4,
    pub label_rect: Rect4,
    pub description_rect: Rect4,
    pub chip_rect: Rect4,
    pub reset: Option<SettingsResetChip>,
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
    pub scale: f32,
    pub line: f32,
    pub text_cw: f32,
    pub hints: Vec<SettingsHintLine>,
    pub sidebar_hints: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsHintLine {
    pub runs: Vec<(String, bool)>,
    pub rect: Rect4,
}

impl SettingsHintLine {
    pub fn text(&self) -> String {
        self.runs.iter().map(|(text, _)| text.as_str()).collect()
    }
}

impl SettingsLayout {
    fn scale_rects(&mut self, scale: f32) {
        let rect = |r: &mut Rect4| {
            r.0 *= scale;
            r.1 *= scale;
            r.2 *= scale;
            r.3 *= scale;
        };
        rect(&mut self.rect);
        if let Some(r) = &mut self.sidebar_rect {
            rect(r);
        }
        rect(&mut self.content_rect);
        rect(&mut self.search_rect);
        rect(&mut self.title_rect);
        rect(&mut self.empty_rect);
        for r in self.category_rects.iter_mut().flatten() {
            rect(r);
        }
        for (_, r) in [&mut self.category_previous, &mut self.category_next]
            .into_iter()
            .flatten()
        {
            rect(r);
        }
        for row in &mut self.rows {
            rect(&mut row.rect);
            rect(&mut row.label_rect);
            rect(&mut row.description_rect);
            rect(&mut row.chip_rect);
            if let Some(reset) = &mut row.reset {
                rect(&mut reset.rect);
            }
        }
        for (_, r) in &mut self.groups {
            rect(r);
        }
        for (_, r) in &mut self.footer {
            rect(r);
        }
        for line in &mut self.hints {
            rect(&mut line.rect);
        }
    }

    /// Wheel notches are positive toward earlier categories, as for menu rows.
    /// A wheel over a Reset chip steps its row, as it does anywhere else on it.
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
        match hit {
            SettingsHit::Reset(index) if wheel.is_some() => SettingsHit::Field(index),
            hit => hit,
        }
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
            if row.reset.is_some_and(|reset| contains(reset.rect, cursor)) {
                return SettingsHit::Reset(row.index);
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

/// The diameter of a settings switch's knob in a chip `height` tall: 3 px in
/// from the track, and no more than four fifths of it, so a tall chip at a
/// large font keeps room for its label.
pub(crate) fn settings_toggle_knob(height: f32, scale: f32) -> f32 {
    (height - 6.0 * scale).min(height * 0.8).max(4.0 * scale)
}

/// Height in logical pixels of each Settings control for a logical line.
/// `settings_layout` applies the backing scale after placement.
pub fn settings_control_height(line: f32) -> f32 {
    (line + 12.0).max(line * 1.5)
}

/// The narrowest a label beside an icon-only Reset chip may become before
/// the chip gives way, in columns.
const MIN_LABEL_COLS: f32 = 12.0;

/// Where a row's Reset chip goes, and how much room it leaves its label.
///
/// The value chip never yields, and Reset never pushes into it. In order,
/// as the row narrows: a disabled chip (the focused row at its default)
/// shows only beside the whole label; an enabled chip shows its word beside
/// the whole label, else the icon beside the whole label, else the icon
/// while the label keeps `MIN_LABEL_COLS`; past that it is not drawn, and
/// Delete and the accessibility tree still reset the row.
fn place_reset(
    row: &SettingsRow,
    shown: bool,
    reset_label: &str,
    (label_x, chip_x): (f32, f32),
    chip_h: f32,
    text_cw: f32,
) -> Option<(f32, bool, bool)> {
    let enabled = row.reset?;
    if row.disabled || !shown {
        return None;
    }
    let gap = if matches!(row.control, SettingsControl::Toggle(_)) {
        4.0
    } else {
        0.0
    };
    let room = |chip: f32| chip_x - 12.0 - label_x - chip - gap;
    let need = display_width(&row.label) as f32 * text_cw;
    let word = (display_width(reset_label) as f32 * text_cw + 20.0).max(chip_h);
    if need <= room(word) + 0.01 {
        return Some((word, enabled, false));
    }
    if !enabled {
        return None;
    }
    let icon = chip_h;
    (need <= room(icon) + 0.01 || room(icon) >= need.min(MIN_LABEL_COLS * text_cw))
        .then_some((icon, true, true))
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
fn wrapped_hint_lines(
    hints: &[(String, String)],
    width: f32,
    cw: f32,
    separate: bool,
) -> Vec<Vec<(String, bool)>> {
    let cols = (width / cw.max(1.0)).floor().max(1.0) as usize;
    let mut lines = Vec::new();
    let mut runs = Vec::new();
    let mut used = 0;
    for (key, action) in hints {
        let group = format!("{key} {action}");
        let width = display_width(&group);
        if !runs.is_empty() && (separate || used + 3 + width > cols) {
            lines.push(std::mem::take(&mut runs));
            used = 0;
        }
        if !runs.is_empty() {
            runs.push((" · ".into(), false));
            used += 3;
        }
        // Truncate a whole pair, never wrap between the key and its action.
        let fitted = fit_single_line_label(&group, cols);
        let key_end = key.len().min(fitted.len());
        if fitted.starts_with(key) {
            runs.push((key.clone(), true));
            runs.push((fitted[key_end..].to_string(), false));
        } else {
            runs.push((fitted, true));
        }
        used += width.min(cols);
    }
    if !runs.is_empty() {
        lines.push(runs);
    }
    lines
}

/// Pixel layout shared by painting, pointer input, text editing, AX and ctl.
/// Offscreen rows have no rectangle, and pinned controls never scroll with rows.
pub fn settings_layout(
    set: &SettingsOverlay,
    cell: (f32, f32),
    surface: (f32, f32),
    text_cw: f32,
    scale: f32,
) -> SettingsLayout {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    // All Settings dimensions, padding and breakpoints are logical pixels.
    // Only this boundary converts to/from the device-pixel consumers.
    let cell = (cell.0 / scale, cell.1 / scale);
    let text_cw = text_cw / scale;
    let (sw, sh) = (surface.0 / scale, surface.1 / scale);
    let w = (sw * 0.72).clamp(560.0, 980.0).min((sw - 32.0).max(0.0));
    let h = (sh * 0.84).clamp(360.0, 820.0).min((sh - 32.0).max(0.0));
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
    let control_h = settings_control_height(line);
    let gap = if compact { 2.0 } else { 6.0 };
    // The chip, then the description below it. Short panels drop the
    // spacing between them first.
    let row_gap = if compact { 4.0 } else { 6.0 };
    let row_h = control_h + line + row_gap;
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
    let sidebar_start =
        rect.1 + pad + control_h + 12.0 + set.categories.len() as f32 * control_h + 12.0;
    let sidebar_hints = wide
        && category_rects.iter().all(Option::is_some)
        && set
            .categories
            .iter()
            .all(|name| display_width(name) as f32 * text_cw <= side_w - pad * 2.0 - 12.0)
        && sidebar_start + set.hints.len() as f32 * line + pad <= rect.1 + h
        && set.hints.iter().all(|(key, action)| {
            display_width(&format!("{key} {action}")) as f32 * text_cw <= side_w - pad * 2.0
        });
    let hint_runs = wrapped_hint_lines(
        &set.hints,
        if sidebar_hints {
            side_w - 2.0 * pad
        } else {
            inner.2
        },
        text_cw,
        sidebar_hints,
    );
    let mut footer_lines: Vec<String> = if sidebar_hints {
        Vec::new()
    } else {
        hint_runs
            .iter()
            .map(|runs| runs.iter().map(|(text, _)| text.as_str()).collect())
            .collect()
    };
    if let Some(note) = &set.footer_note {
        footer_lines.extend(wrapped_lines(note, inner.2, text_cw));
    }
    // Even in a very short window, retain a complete focused row. Pending
    // notices at the end take precedence when all the footer cannot fit.
    let footer_capacity = ((inner.1 + inner.3 - rows_top - row_h - line - 12.0) / line)
        .floor()
        .max(0.0) as usize;
    let removed = footer_lines.len().saturating_sub(footer_capacity);
    footer_lines.drain(..removed);
    let footer_gap = if footer_lines.is_empty() {
        0.0
    } else if compact {
        2.0
    } else {
        12.0
    };
    let footer_top = inner.1 + inner.3 - footer_lines.len() as f32 * line;
    let footer: Vec<_> = footer_lines
        .into_iter()
        .enumerate()
        .map(|(i, text)| (text, (inner.0, footer_top + i as f32 * line, inner.2, line)))
        .collect();
    let available = (footer_top - footer_gap - rows_top).max(0.0);
    let heading_h = line + 12.0;
    let grouped = set
        .rows
        .iter()
        .enumerate()
        .any(|(i, _)| !set.heading(i).is_empty())
        && available + 0.01 >= row_h + heading_h;
    let focused = set.focused_row.min(set.rows.len().saturating_sub(1));
    let group_h = if grouped { heading_h } else { 0.0 };
    let mut first = focused;
    let mut used = row_h + group_h;
    while first > 0 {
        let extra = row_h
            + if grouped && set.heading(first - 1) != set.heading(first) {
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
        let group =
            grouped && !set.heading(index).is_empty() && previous != Some(set.heading(index));
        if y + row_h + if group { group_h } else { 0.0 } > rows_top + available + 0.01 {
            break;
        }
        if group {
            groups.push((index, (inner.0, y, inner.2, group_h)));
            y += group_h;
        }
        // A switch also holds its knob beside the label.
        let knob = if matches!(row.control, SettingsControl::Toggle(_)) {
            settings_toggle_knob(control_h, 1.0) + 2.0
        } else {
            0.0
        };
        // Every chip keeps one width while its value changes and is at
        // least eight columns wide. It may take what the label leaves, so a
        // long value shows whole beside a short label; where both are long
        // it keeps about half the row.
        let chip_w = if row.control == SettingsControl::Text {
            inner.2 * 0.48
        } else {
            let label_need = display_width(&row.label) as f32 * text_cw;
            (row.chip_cols() as f32 * text_cw + 20.0 + knob)
                .max(8.0 * text_cw + 20.0)
                .min((inner.2 - 18.0 - label_need).max(inner.2 * 0.48))
        };
        let chip_rect = (inner.0 + inner.2 - chip_w, y + 1.0, chip_w, control_h);
        let label_x = inner.0 + 6.0;
        let reset = place_reset(
            row,
            row.reset == Some(true) || (index == focused && !set.search_focused),
            &set.reset_label,
            (label_x, chip_rect.0),
            control_h,
            text_cw,
        )
        .map(|(width, enabled, icon)| SettingsResetChip {
            rect: (
                chip_rect.0
                    - if matches!(row.control, SettingsControl::Toggle(_)) {
                        4.0
                    } else {
                        0.0
                    }
                    - width,
                chip_rect.1,
                width,
                control_h,
            ),
            enabled,
            icon,
        });
        let label_end = reset.map_or(chip_rect.0, |reset| reset.rect.0);
        rows.push(SettingsRowGeometry {
            index,
            rect: (inner.0, y, inner.2, row_h),
            label_rect: (
                label_x,
                chip_rect.1 + (control_h - line) * 0.5,
                (label_end - 12.0 - label_x).max(0.0),
                line,
            ),
            description_rect: (
                label_x,
                y + control_h + row_gap * 0.5,
                (inner.2 - 12.0).max(0.0),
                line,
            ),
            chip_rect,
            reset,
        });
        y += row_h;
        previous = Some(set.heading(index));
    }
    let hints = if sidebar_hints {
        hint_runs
            .into_iter()
            .enumerate()
            .map(|(i, runs)| SettingsHintLine {
                runs,
                rect: (
                    rect.0 + pad,
                    sidebar_start + i as f32 * line,
                    side_w - 2.0 * pad,
                    line,
                ),
            })
            .collect()
    } else {
        hint_runs
            .into_iter()
            .enumerate()
            .skip(removed)
            .filter_map(|(i, runs)| {
                footer
                    .get(i - removed)
                    .map(|(_, rect)| SettingsHintLine { runs, rect: *rect })
            })
            .collect()
    };
    let mut layout = SettingsLayout {
        scale,
        line: line * scale,
        text_cw: text_cw * scale,
        hints,
        sidebar_hints,
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
    };
    layout.scale_rects(scale);
    layout
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
    scale: f32,
) -> SettingsPanelGeometry {
    let layout = settings_layout(set, (cw, ch), (sw, sh), cw, scale);
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
    scale: f32,
) -> Rect4 {
    let layout = settings_layout(set, (cw, ch), (sw, sh), text_cw, scale);
    layout
        .rows
        .iter()
        .find(|row| row.index == set.focused_row)
        .map_or(layout.empty_rect, |row| {
            chip_text_rect(row.chip_rect, row.label_rect.3, 4.0 * layout.scale)
        })
}

/// One line of text inside `chip`, `padding` in from its sides and centred
/// on its height.
fn chip_text_rect((x, y, w, h): Rect4, line: f32, padding: f32) -> Rect4 {
    let p = padding.min(w * 0.5);
    let height = line.min(h).max(0.0);
    (
        x + p,
        y + (h - height) * 0.5,
        (w - 2.0 * p).max(0.0),
        height,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    Outside,
    Inert,
    Search,
    Category(usize),
    Field(usize),
    /// A row's Reset chip, enabled or not. Only a primary click resets; the
    /// pointer layer treats anything else there as a hit on the row.
    Reset(usize),
}

#[allow(clippy::too_many_arguments)]
pub fn settings_hit_test(
    set: &SettingsOverlay,
    cw: f32,
    ch: f32,
    sw: f32,
    sh: f32,
    x: f32,
    y: f32,
    scale: f32,
) -> SettingsHit {
    settings_hit_test_with_text_width(set, (cw, ch), (sw, sh), (x, y), cw, scale)
}

pub fn settings_hit_test_with_text_width(
    set: &SettingsOverlay,
    cell: (f32, f32),
    surface: (f32, f32),
    cursor: (f32, f32),
    text_cw: f32,
    scale: f32,
) -> SettingsHit {
    let layout = settings_layout(set, cell, surface, text_cw, scale);
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
    /// The word on a row's Reset chip.
    Reset(usize),
    Muted,
}

pub(crate) struct SettingsTextItem {
    pub text: String,
    pub rect: Rect4,
    pub kind: SettingsTextKind,
    pub runs: Vec<(String, bool)>,
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
            runs: Vec::new(),
        });
    };
    if layout.title_rect.3 > 0.0 {
        push(&set.title, layout.title_rect, SettingsTextKind::Title);
    }
    for (text, arrow) in [("<", layout.category_previous), (">", layout.category_next)] {
        if let Some((_, rect)) = arrow {
            push(
                text,
                inset(rect, 4.0 * layout.scale),
                SettingsTextKind::Heading,
            );
        }
    }
    for (i, rect) in layout.category_rects.iter().enumerate() {
        if let Some(rect) = rect {
            push(
                &set.categories[i],
                inset(*rect, 6.0 * layout.scale),
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
            inset(layout.search_rect, 6.0 * layout.scale),
            SettingsTextKind::Muted,
        );
    }
    for (first_row, rect) in &layout.groups {
        push(
            set.heading(*first_row),
            (rect.0, rect.1 + 12.0 * layout.scale, rect.2, layout.line),
            SettingsTextKind::Heading,
        );
    }
    // Centred text: fitted to `room`, then given a rect just its own width.
    let centred = |items: &mut Vec<SettingsTextItem>, text: &str, room: Rect4, kind| {
        let cols = (room.2 / cw.max(1.0)).floor().max(0.0) as usize;
        let text = fit_single_line_label(text, cols);
        let width = (display_width(&text) as f32 * cw).min(room.2);
        items.push(SettingsTextItem {
            rect: (room.0 + (room.2 - width) * 0.5, room.1, width, room.3),
            text,
            kind,
            runs: Vec::new(),
        });
    };
    let mut centred_items = Vec::new();
    for row in &layout.rows {
        let data = &set.rows[row.index];
        let line = row.label_rect.3;
        push(
            &data.label,
            row.label_rect,
            SettingsTextKind::Label(row.index),
        );
        if let Some(reset) = row.reset.filter(|reset| !reset.icon) {
            centred(
                &mut centred_items,
                &set.reset_label,
                chip_text_rect(reset.rect, line, 10.0 * layout.scale),
                SettingsTextKind::Reset(row.index),
            );
        }
        let mut room = chip_text_rect(row.chip_rect, line, 4.0 * layout.scale);
        // Reserve the knob after chip-width clamping, on either end of the switch.
        if matches!(data.control, SettingsControl::Toggle(_)) {
            let knob = (settings_toggle_knob(row.chip_rect.3, layout.scale) + 2.0 * layout.scale)
                .min(room.2);
            if !settings_knob_on_right(data) {
                room.0 += knob;
            }
            room.2 -= knob;
        }
        let cols = (room.2 / cw.max(1.0)).floor().max(0.0) as usize;
        let value =
            if data.control == SettingsControl::Choice && !data.value.is_empty() && cols >= 4 {
                // The arrows sit at the chip's ends, the value centred
                // between them, so they stay put as the value changes.
                let value = fit_single_line_label(&data.value, cols - 4);
                let spare = (cols - 4).saturating_sub(display_width(&value));
                format!(
                    "‹ {}{value}{} ›",
                    " ".repeat(spare / 2),
                    " ".repeat(spare - spare / 2)
                )
            } else {
                data.chip_text()
            };
        if data.control == SettingsControl::Text {
            // Free text starts where its inline editor does, so opening
            // the editor does not move it.
            push(&value, room, SettingsTextKind::Value(row.index));
        } else {
            centred(
                &mut centred_items,
                &value,
                room,
                SettingsTextKind::Value(row.index),
            );
        }
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
        if !layout.hints.iter().any(|hint| hint.rect == *rect) {
            push(text, *rect, SettingsTextKind::Muted);
        }
    }
    for hint in &layout.hints {
        items.push(SettingsTextItem {
            text: hint.text(),
            rect: hint.rect,
            kind: SettingsTextKind::Muted,
            runs: hint.runs.clone(),
        });
    }
    items.extend(centred_items);
    items
}

/// Width of the bar that marks the focused row and the active category:
/// a sixth of a line, so it keeps its weight at 2x.
pub(crate) fn settings_focus_bar_width(line: f32) -> f32 {
    (line / 6.0).round().max(2.0)
}

/// The focus bar along the left edge of `rect`.
pub(crate) fn settings_focus_bar((x, y, _, h): Rect4, width: f32, scale: f32) -> Rect4 {
    (x, y + 4.0 * scale, width, (h - 8.0 * scale).max(0.0))
}

/// The ink of one Settings text item. Labels, values and titles are
/// `TEXT_CONTRAST` text; descriptions, hints, notes and the placeholder use
/// the palette's quieter inks. A disabled row stays at the `muted` floor
/// (4.5:1), quieter than enabled descriptions: WCAG exempts an inactive
/// control's text, and the floor keeps it readable anyway.
pub(crate) fn settings_text_color(
    set: &SettingsOverlay,
    kind: SettingsTextKind,
    palette: &SettingsColors,
    (theme, accent, on_ink): (&kettle_config::Theme, Rgb, Rgb),
    colors: &mut super::chrome::Colors,
) -> Rgb {
    let (fg, bg) = (theme.foreground, theme.background);
    let index = match kind {
        SettingsTextKind::Category(index) if index == set.active_category => {
            return colors.text(fg, palette.selected);
        }
        SettingsTextKind::Title | SettingsTextKind::Heading | SettingsTextKind::Category(_) => {
            return colors.text(fg, bg);
        }
        SettingsTextKind::Muted => return palette.description,
        SettingsTextKind::Label(index)
        | SettingsTextKind::Value(index)
        | SettingsTextKind::Description(index)
        | SettingsTextKind::Reset(index) => index,
    };
    let Some(row) = set.rows.get(index) else {
        return colors.text(fg, bg);
    };
    let focused = index == set.focused_row && !set.search_focused;
    // Labels and descriptions sit on the focused row's fill; chips are
    // filled with the panel.
    let in_chip = matches!(
        kind,
        SettingsTextKind::Value(_) | SettingsTextKind::Reset(_)
    );
    let surface = if matches!(kind, SettingsTextKind::Value(_))
        && matches!(row.control, SettingsControl::Value | SettingsControl::Text)
    {
        palette.well
    } else if focused && !in_chip {
        palette.selected
    } else {
        bg
    };
    if row.disabled {
        return colors.muted(fg, surface);
    }
    match kind {
        SettingsTextKind::Value(_) if row.unavailable => colors.text(palette.description, surface),
        SettingsTextKind::Value(_) if settings_knob_on_right(row) => colors.text(on_ink, accent),
        SettingsTextKind::Description(_) if focused => palette.description_selected,
        SettingsTextKind::Description(_) => palette.description,
        SettingsTextKind::Reset(_) if row.reset != Some(true) => colors.muted(fg, bg),
        _ => colors.text(fg, surface),
    }
}

/// Whether a switch's knob sits at its right end: switched on, even where
/// this session cannot show the effect. A disabled switch reads as off.
pub(crate) fn settings_knob_on_right(row: &SettingsRow) -> bool {
    row.control == SettingsControl::Toggle(true) && !row.disabled
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SettingsSurface {
    pub(crate) rect: Rect4,
    pub(crate) fill: Rgb,
    pub(crate) stroke: Rgb,
    pub(crate) width: f32,
    pub(crate) radius: f32,
}

pub(crate) fn settings_row_surfaces(
    row: &SettingsRowGeometry,
    data: &SettingsRow,
    palette: &SettingsColors,
    (bg, accent, on_ink): (Rgb, Rgb, Rgb),
    selected: bool,
    scale: f32,
) -> Vec<SettingsSurface> {
    let switch = matches!(data.control, SettingsControl::Toggle(_));
    let on = settings_knob_on_right(data) && !data.unavailable;
    let well = matches!(data.control, SettingsControl::Value | SettingsControl::Text);
    let fill = if on {
        accent
    } else if well {
        palette.well
    } else {
        bg
    };
    let control = if switch && data.unavailable {
        palette.unavailable_control
    } else if well && selected {
        palette.selected_well_control
    } else if well {
        palette.well_control
    } else if selected {
        palette.selected_control
    } else {
        palette.control
    };
    let mut shapes = Vec::new();
    let mut shape = |rect, fill, stroke, width, radius| {
        shapes.push(SettingsSurface {
            rect,
            fill,
            stroke,
            width,
            radius,
        })
    };
    let joined = row.reset.filter(|_| {
        matches!(
            data.control,
            SettingsControl::Value | SettingsControl::Choice
        )
    });
    if let Some(reset) = joined {
        let (x, y, w, h) = reset.rect;
        shape(
            (x, y, row.chip_rect.0 + row.chip_rect.2 - x, h),
            fill,
            control,
            scale,
            4.0 * scale,
        );
        shape(inset(reset.rect, scale), bg, bg, 0.0, 3.0 * scale);
        shape(
            (
                x + 4.0 * scale,
                y + scale,
                (w - 4.0 * scale).max(0.0),
                h - 2.0 * scale,
            ),
            bg,
            bg,
            0.0,
            0.0,
        );
        shape(
            (row.chip_rect.0 - 0.5 * scale, y, scale, h),
            control,
            control,
            0.0,
            0.0,
        );
    } else {
        shape(
            row.chip_rect,
            fill,
            control,
            scale,
            if switch {
                row.chip_rect.3 / 2.0
            } else {
                4.0 * scale
            },
        );
    }
    if switch {
        let (x, y, w, h) = row.chip_rect;
        let d = settings_toggle_knob(h, scale);
        let kx = if settings_knob_on_right(data) {
            x + w - d - 3.0 * scale
        } else {
            x + 3.0 * scale
        };
        let knob = (kx, y + (h - d) / 2.0, d, d);
        if data.unavailable {
            shape(knob, bg, control, 1.5 * scale, d / 2.0);
        } else {
            let fill = if on { on_ink } else { control };
            shape(knob, fill, fill, 0.0, d / 2.0);
        }
    }
    if let Some(reset) = row.reset.filter(|_| joined.is_none()) {
        shape(
            reset.rect,
            bg,
            if reset.enabled {
                control
            } else {
                palette.border
            },
            scale,
            4.0 * scale,
        );
    }
    shapes
}

pub(crate) struct SettingsTextStyle<'a> {
    pub(crate) metrics: super::Metrics,
    pub(crate) family: &'a str,
    pub(crate) color: Rgb,
    pub(crate) description: Rgb,
}

pub(crate) fn shape_settings_text(
    font_system: &mut super::FontSystem,
    buffer: &mut super::TextBuffer,
    key: &mut u64,
    item: &SettingsTextItem,
    style: &SettingsTextStyle<'_>,
) {
    use super::{Attrs, Family, Metrics, Shaping, Weight, Wrap};
    use std::hash::{Hash, Hasher};
    let title = item.kind == SettingsTextKind::Title;
    let bold = title
        || item.kind == SettingsTextKind::Heading
        || (matches!(item.kind, SettingsTextKind::Label(_))
            && style.family == kettle_config::font::FAMILY);
    let factor = if title { 1.2 } else { 1.0 };
    let metrics = Metrics::new(
        style.metrics.font_size * factor,
        style.metrics.line_height * factor,
    );
    let mut hash = std::hash::DefaultHasher::new();
    item.text.hash(&mut hash);
    item.runs.hash(&mut hash);
    bold.hash(&mut hash);
    for ink in [style.color, style.description] {
        (ink.r, ink.g, ink.b).hash(&mut hash);
    }
    metrics.font_size.to_bits().hash(&mut hash);
    metrics.line_height.to_bits().hash(&mut hash);
    item.rect.2.to_bits().hash(&mut hash);
    item.rect.3.to_bits().hash(&mut hash);
    style.family.hash(&mut hash);
    let current = hash.finish();
    if *key != current {
        buffer.set_metrics(metrics);
        buffer.set_size(Some(item.rect.2), Some(item.rect.3));
        buffer.set_wrap(Wrap::None);
        let base = Attrs::new()
            .family(Family::Name(style.family))
            .weight(if bold { Weight::BOLD } else { Weight::NORMAL });
        if item.runs.is_empty() {
            buffer.set_text(&item.text, &base, Shaping::Advanced, None);
        } else {
            buffer.set_rich_text(
                item.runs.iter().map(|(text, key)| {
                    let ink = if *key { style.color } else { style.description };
                    (
                        text.as_str(),
                        base.clone().color(super::GColor::rgb(ink.r, ink.g, ink.b)),
                    )
                }),
                &base,
                Shaping::Advanced,
                None,
            );
        }
        *key = current;
    }
    buffer.shape_until_scroll(font_system, false);
}
