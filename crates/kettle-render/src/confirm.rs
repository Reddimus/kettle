//! Geometry and retained text for the final confirmation layer.
use super::{
    Attrs, ConfirmDialogOverlay, Family, FontSystem, GColor, Metrics, Rect4, Shaping, TextArea,
    TextBounds, TextBuffer, Weight, Wrap, display_width, take_cols_front,
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug)]
pub struct ConfirmDialogLayout {
    pub rect: Rect4,
    pub heading_rect: Rect4,
    pub body_rect: Rect4,
    pub buttons: Vec<Rect4>,
    pub max_scroll: usize,
    pub scroll_offset: usize,
    pub whole_scroll: bool,
    pub unit: f32,
    pub body_font: f32,
    pub body_line: f32,
    pub heading_font: f32,
    pub heading_line: f32,
    heading: String,
    body: String,
    button_full: Vec<Rect4>,
    heading_full: Rect4,
    body_full: Rect4,
    button_offsets: Vec<usize>,
    visible_steps: usize,
    button_labels: Vec<String>,
    button_lines: Vec<usize>,
}

pub(crate) fn intersect(a: Rect4, b: Rect4) -> Rect4 {
    let x = a.0.max(b.0).min(b.0 + b.2);
    let y = a.1.max(b.1).min(b.1 + b.3);
    (
        x,
        y,
        ((a.0 + a.2).min(b.0 + b.2) - x).max(0.0),
        ((a.1 + a.3).min(b.1 + b.3) - y).max(0.0),
    )
}

impl ConfirmDialogLayout {
    pub fn new(
        surface: (f32, f32),
        metrics: (f32, f32),
        scale_factor: f32,
        dialog: &ConfirmDialogOverlay,
    ) -> Self {
        let (sw, sh) = surface;
        let (cw, lh) = metrics;
        let valid = [sw, sh, cw, lh, scale_factor]
            .into_iter()
            .all(|n| n.is_finite() && n > 0.0);
        let (sw, sh, cw, lh, scale_factor) = if valid {
            (sw, sh, cw, lh, scale_factor)
        } else {
            (0.0, 0.0, 8.0, 16.0, 1.0)
        };
        let body_font = (lh / 1.25).max(14.0 * scale_factor);
        let unit = body_font / 14.0;
        let body_line = body_font * 1.4;
        let body_cell = cw * (body_font / (lh / 1.25));
        let heading_font = body_font * 16.0 / 14.0;
        let heading_line = heading_font * 1.3;
        let margin = (24.0 * unit).min(sw * 0.05).min(sh * 0.05);
        let width = (480.0 * unit).min((sw - margin * 2.0).max(0.0));
        let padding = (24.0 * unit).min(width * 0.08).min(sh * 0.04);
        let inner = (width - padding * 2.0).max(0.0);
        let columns = ((inner / body_cell).floor() as usize)
            .saturating_sub(1)
            .max(1);
        let heading_columns = ((inner / (body_cell * 16.0 / 14.0)).floor() as usize)
            .saturating_sub(1)
            .max(1);
        let heading_lines = wrap(&dialog.heading, heading_columns);
        let body_lines = wrap(&dialog.body, columns);
        let heading_h = heading_lines.len() as f32 * heading_line;
        let body_h = body_lines.len() as f32 * body_line;
        let button_w = dialog
            .buttons
            .iter()
            .map(|b| display_width(&b.label) as f32 * body_cell + 32.0 * unit)
            .fold(96.0 * unit, f32::max)
            .min(inner);
        let label_columns =
            (((button_w - 24.0 * unit).max(body_cell) / body_cell).floor() as usize).max(1);
        let button_labels: Vec<_> = dialog
            .buttons
            .iter()
            .map(|b| wrap(&b.label, label_columns))
            .collect();
        let button_lines: Vec<_> = button_labels.iter().map(Vec::len).collect();
        let button_h = (40.0 * unit)
            .max(button_lines.iter().copied().max().unwrap_or(1) as f32 * body_line + 16.0 * unit);
        let button_labels = button_labels
            .into_iter()
            .map(|lines| lines.join("\n"))
            .collect();
        let gap = 12.0 * unit;
        let count = dialog.buttons.len();
        let stacked = button_w * count as f32 + gap * count.saturating_sub(1) as f32 > inner;
        let actions_h = if count == 0 {
            0.0
        } else if stacked {
            count as f32 * button_h + count.saturating_sub(1) as f32 * gap
        } else {
            button_h
        };
        let heading_gap = if dialog.body.is_empty() {
            0.0
        } else {
            8.0 * unit
        };
        let action_gap = 24.0 * unit;
        let desired = padding * 2.0 + heading_h + heading_gap + body_h + action_gap + actions_h;
        let maximum = (sh - margin * 2.0).max(0.0);
        let minimum = padding * 2.0 + heading_h + heading_gap + body_line + action_gap + actions_h;
        let whole_scroll = maximum < minimum;
        let height = desired.min(maximum);
        let x = (sw - width) * 0.5;
        let y = (sh - height) * 0.5;
        let clip = (
            x + padding,
            y + padding,
            inner,
            (height - padding * 2.0).max(0.0),
        );
        // Preserve the measured body height when it fits. Subtracting all
        // other sections can turn roundoff into a whole scroll step at ceil.
        let body_view_h = if whole_scroll || desired <= maximum {
            body_h
        } else {
            (height - padding * 2.0 - heading_h - heading_gap - action_gap - actions_h).max(0.0)
        };
        let overflow = if whole_scroll {
            (desired - height).max(0.0)
        } else {
            (body_h - body_view_h).max(0.0)
        };
        let max_scroll = (overflow / body_line).ceil() as usize;
        let scroll_offset = dialog.scroll.min(max_scroll);
        let shift = scroll_offset as f32 * body_line;
        let heading_full = (
            clip.0,
            clip.1 - if whole_scroll { shift } else { 0.0 },
            inner,
            heading_h,
        );
        let body_full = (
            clip.0,
            heading_full.1 + heading_h + heading_gap - if whole_scroll { 0.0 } else { shift },
            inner,
            body_h,
        );
        let body_rect = if whole_scroll {
            intersect(body_full, clip)
        } else {
            (clip.0, clip.1 + heading_h + heading_gap, inner, body_view_h)
        };
        let action_top = if whole_scroll {
            body_full.1 + body_h + action_gap
        } else {
            y + height - padding - actions_h
        };
        let actions_w = if stacked {
            button_w
        } else {
            button_w * count as f32 + gap * count.saturating_sub(1) as f32
        };
        let mut button_full = Vec::with_capacity(count);
        let mut button_offsets = Vec::with_capacity(count);
        for index in 0..count {
            let bx = clip.0 + inner - actions_w
                + if stacked {
                    0.0
                } else {
                    index as f32 * (button_w + gap)
                };
            let by = action_top
                + if stacked {
                    index as f32 * (button_h + gap)
                } else {
                    0.0
                };
            button_full.push((bx, by, button_w, button_h));
            button_offsets.push(((by + shift - clip.1) / body_line).floor().max(0.0) as usize);
        }
        let buttons = button_full.iter().map(|&r| intersect(r, clip)).collect();
        Self {
            rect: (x, y, width, height),
            heading_rect: intersect(heading_full, clip),
            body_rect,
            buttons,
            max_scroll,
            scroll_offset,
            whole_scroll,
            unit,
            body_font,
            body_line,
            heading_font,
            heading_line,
            heading: heading_lines.join("\n"),
            body: body_lines.join("\n"),
            button_full,
            heading_full,
            body_full,
            button_offsets,
            visible_steps: (clip.3 / body_line).floor().max(1.0) as usize,
            button_labels,
            button_lines,
        }
    }

    pub fn scroll_to_button(&self, index: usize) -> usize {
        if !self.whole_scroll {
            return self.scroll_offset;
        }
        let Some(&top) = self.button_offsets.get(index) else {
            return self.scroll_offset;
        };
        let rows = self
            .button_full
            .get(index)
            .map_or(1, |r| (r.3 / self.body_line).ceil() as usize);
        if top < self.scroll_offset {
            top.min(self.max_scroll)
        } else if top + rows > self.scroll_offset + self.visible_steps {
            (top + rows)
                .saturating_sub(self.visible_steps)
                .min(self.max_scroll)
        } else {
            self.scroll_offset
        }
    }
}

fn wrap(text: &str, columns: usize) -> Vec<String> {
    let mut result = Vec::new();
    for paragraph in text.split('\n') {
        let mut rest = paragraph.trim();
        if rest.is_empty() {
            result.push(String::new());
            continue;
        }
        while !rest.is_empty() {
            let prefix = take_cols_front(rest, columns);
            let length = if prefix.is_empty() {
                rest.graphemes(true).next().map_or(rest.len(), str::len)
            } else if prefix.len() == rest.len() {
                rest.len()
            } else {
                prefix
                    .char_indices()
                    .rev()
                    .find(|&(_, c)| c.is_whitespace())
                    .map(|(i, _)| i)
                    .filter(|&i| i > 0)
                    .unwrap_or(prefix.len())
            };
            result.push(rest[..length].trim_end().to_string());
            rest = rest[length..].trim_start();
        }
    }
    result
}

pub(crate) struct ConfirmText {
    buffers: Vec<TextBuffer>,
    texts: Vec<String>,
    family: String,
}
impl ConfirmText {
    pub fn new() -> Self {
        Self {
            buffers: Vec::new(),
            texts: Vec::new(),
            family: String::new(),
        }
    }
    pub fn prepare(
        &mut self,
        fonts: &mut FontSystem,
        family: &str,
        layout: &ConfirmDialogLayout,
        _dialog: &ConfirmDialogOverlay,
    ) {
        let family_changed = self.family != family;
        self.family = family.to_string();
        let mut text = vec![layout.heading.clone(), layout.body.clone()];
        text.extend(layout.button_labels.iter().cloned());
        while self.buffers.len() < text.len() {
            self.buffers.push(TextBuffer::new(
                fonts,
                Metrics::new(layout.body_font, layout.body_line),
            ));
            self.texts.push(String::new());
        }
        for (index, value) in text.into_iter().enumerate() {
            let heading = index == 0;
            let buffer = &mut self.buffers[index];
            buffer.set_metrics(if heading {
                Metrics::new(layout.heading_font, layout.heading_line)
            } else {
                Metrics::new(layout.body_font, layout.body_line)
            });
            buffer.set_wrap(Wrap::None);
            let rect = if heading {
                layout.heading_full
            } else if index == 1 {
                layout.body_full
            } else {
                layout.button_full[index - 2]
            };
            buffer.set_size(Some(rect.2.max(1.0)), None);
            if family_changed || self.texts[index] != value {
                let attrs = Attrs::new()
                    .family(Family::Name(family))
                    .weight(if heading {
                        Weight::BOLD
                    } else {
                        Weight::NORMAL
                    });
                buffer.set_text(&value, &attrs, Shaping::Advanced, None);
                self.texts[index] = value;
            }
            buffer.shape_until_scroll(fonts, false);
        }
    }
    pub fn key(&self) -> &[String] {
        &self.texts
    }
    pub fn areas<'a>(
        &'a self,
        layout: &ConfirmDialogLayout,
        colors: &[GColor],
    ) -> Vec<TextArea<'a>> {
        self.buffers
            .iter()
            .take(layout.buttons.len() + 2)
            .enumerate()
            .map(|(index, buffer)| {
                let rect = if index == 0 {
                    layout.heading_full
                } else if index == 1 {
                    layout.body_full
                } else {
                    layout.button_full[index - 2]
                };
                let clip = if index == 0 {
                    layout.heading_rect
                } else if index == 1 {
                    layout.body_rect
                } else {
                    layout.buttons[index - 2]
                };
                let width = buffer
                    .layout_runs()
                    .map(|run| run.line_w)
                    .fold(0.0, f32::max);
                TextArea {
                    buffer,
                    left: if index >= 2 {
                        rect.0 + (rect.2 - width).max(0.0) * 0.5
                    } else {
                        rect.0
                    },
                    top: if index >= 2 {
                        rect.1
                            + (rect.3 - layout.body_line * layout.button_lines[index - 2] as f32)
                                * 0.5
                    } else {
                        rect.1
                    },
                    scale: 1.0,
                    bounds: TextBounds {
                        left: clip.0.ceil() as i32,
                        top: clip.1.ceil() as i32,
                        right: (clip.0 + clip.2).floor() as i32,
                        bottom: (clip.1 + clip.3).floor() as i32,
                    },
                    default_color: colors[index],
                    custom_glyphs: &[],
                }
            })
            .collect()
    }
}

pub(crate) fn button_colors(
    theme: &kettle_config::Theme,
    accent: kettle_config::Rgb,
    dialog: &ConfirmDialogOverlay,
    index: usize,
) -> (kettle_config::Rgb, kettle_config::Rgb) {
    use super::color;
    let base = if index == 0 {
        color::over(theme.background, theme.foreground, 0.08, false)
    } else if dialog.buttons[index].destructive {
        theme.palette[1]
    } else {
        accent
    };
    let mix = if dialog.pressed_idx == Some(index) {
        0.18
    } else if dialog.hover_idx == Some(index) {
        0.09
    } else {
        0.0
    };
    let target = if color::relative_luminance(base) > 0.4 {
        kettle_config::Rgb::new(0, 0, 0)
    } else {
        kettle_config::Rgb::new(255, 255, 255)
    };
    let fill = color::over(base, target, mix, false);
    (fill, color::with_min_contrast(theme.foreground, fill, 4.5))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dialog() -> ConfirmDialogOverlay {
        ConfirmDialogOverlay {
            heading: "Close this window?".into(),
            body:
                "Both panes will close. Commands may be interrupted and unsaved work may be lost."
                    .into(),
            buttons: vec![
                super::super::ConfirmDialogButton {
                    label: "Cancelar".into(),
                    destructive: false,
                },
                super::super::ConfirmDialogButton {
                    label: "Cerrar ventana 界".into(),
                    destructive: true,
                },
            ],
            focus_idx: 0,
            hover_idx: None,
            pressed_idx: None,
            scroll: 0,
        }
    }
    #[test]
    fn confirmation_geometry_is_bounded_without_shrinking_readable_fonts() {
        let mut d = dialog();
        d.body = "Long translated consequences 界界 with many words. ".repeat(30);
        for (surface, metrics, scale) in [
            ((800.0, 600.0), (8.25, 16.5), 1.0),
            ((1600.0, 1200.0), (16.5, 33.0), 2.0),
            ((192.0, 600.0), (8.25, 16.5), 1.0),
            ((800.0, 160.0), (8.25, 16.5), 1.0),
            ((80.0, 60.0), (8.25, 16.5), 1.0),
            ((1.0, 1.0), (8.25, 16.5), 1.0),
        ] {
            let layout = ConfirmDialogLayout::new(surface, metrics, scale, &d);
            let (x, y, w, h) = layout.rect;
            assert!(x >= 0.0 && y >= 0.0 && x + w <= surface.0 + 0.01 && y + h <= surface.1 + 0.01);
            assert!(layout.body_font >= 14.0 * scale);
            assert_eq!(layout.buttons.len(), 2);
            for &(bx, by, bw, bh) in &layout.buttons {
                assert!(
                    bx >= x
                        && by >= y
                        && bw >= 0.0
                        && bh >= 0.0
                        && bx + bw <= x + w + 0.01
                        && by + bh <= y + h + 0.01
                );
            }
            assert!(!layout.body.contains('…'));
            assert!(layout.max_scroll > 0);
            d.scroll = usize::MAX;
            let scrolled = ConfirmDialogLayout::new(surface, metrics, scale, &d);
            assert_eq!(scrolled.scroll_offset, scrolled.max_scroll);
            d.scroll = 0;
        }
    }
    #[test]
    fn fully_visible_confirmations_have_no_scroll_cue_at_fractional_metrics() {
        for (heading, body) in [
            (
                "Close this window?",
                "Both panes will close. Commands may be interrupted and unsaved work may be lost.",
            ),
            (
                "Reassign Ctrl+Shift+T?",
                "Assign this shortcut to Split pane right. New tab will lose this shortcut.",
            ),
        ] {
            let mut d = dialog();
            d.heading = heading.into();
            d.body = body.into();
            for surface in [
                (800.0, 600.0),
                (1080.0, 900.0),
                (1920.0, 1080.0),
                (640.0, 600.0),
                (420.0, 700.0),
                (320.0, 700.0),
            ] {
                for (metrics, scale) in [
                    ((8.4, 16.4), 1.0),
                    ((8.25, 16.5), 1.0),
                    ((9.1, 18.2), 1.0),
                    ((10.5, 20.5), 1.25),
                ] {
                    let layout = ConfirmDialogLayout::new(surface, metrics, scale, &d);
                    assert!(!layout.whole_scroll);
                    assert_eq!(
                        layout.max_scroll, 0,
                        "spurious scroll cue for {heading} at {surface:?}, {metrics:?}, scale {scale}"
                    );
                }
            }
        }
    }

    #[test]
    fn confirmation_pins_actions_until_whole_content_scrolling_is_necessary() {
        let mut d = dialog();
        d.body = "Readable consequences. ".repeat(80);
        let first = ConfirmDialogLayout::new((800.0, 400.0), (8.0, 16.0), 1.0, &d);
        assert!(!first.whole_scroll);
        assert!(first.max_scroll > 0);
        d.scroll = first.max_scroll;
        let last = ConfirmDialogLayout::new((800.0, 400.0), (8.0, 16.0), 1.0, &d);
        assert_eq!(first.buttons, last.buttons);
        assert_eq!(first.heading_rect, last.heading_rect);
        d.scroll = 0;
        let short = ConfirmDialogLayout::new((800.0, 100.0), (8.0, 16.0), 1.0, &d);
        assert!(short.whole_scroll);
        d.scroll = short.scroll_to_button(1);
        let focused = ConfirmDialogLayout::new((800.0, 100.0), (8.0, 16.0), 1.0, &d);
        assert!(focused.buttons[1].3 > 0.0);
    }
    #[test]
    fn confirmation_scales_once_and_keeps_long_translated_button_labels() {
        let mut d = dialog();
        d.buttons[1].label = "Confirmar cierre de todas las terminales".into();
        let a = ConfirmDialogLayout::new((800.0, 600.0), (8.0, 16.0), 1.0, &d);
        let b = ConfirmDialogLayout::new((1600.0, 1200.0), (16.0, 32.0), 2.0, &d);
        for (v, w) in [a.rect.2, a.rect.3, a.buttons[0].2, a.buttons[0].3]
            .into_iter()
            .zip([b.rect.2, b.rect.3, b.buttons[0].2, b.buttons[0].3])
        {
            assert!((v * 2.0 - w).abs() < 0.01);
        }
        let narrow = ConfirmDialogLayout::new((192.0, 600.0), (8.0, 16.0), 1.0, &d);
        assert_eq!(
            narrow.button_labels[1]
                .split_whitespace()
                .collect::<Vec<_>>(),
            d.buttons[1].label.split_whitespace().collect::<Vec<_>>()
        );
        assert_eq!(narrow.buttons[0].2, narrow.buttons[1].2);
    }
}

pub(crate) fn focus_color(
    theme: &kettle_config::Theme,
    accent: kettle_config::Rgb,
) -> kettle_config::Rgb {
    super::color::with_min_contrast(accent, theme.background, 3.0)
}

#[cfg(test)]
mod contrast_tests {
    use super::*;
    #[test]
    fn confirmation_buttons_and_focus_keep_contrast_in_every_theme_and_state() {
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            for destructive in [false, true] {
                let mut d = ConfirmDialogOverlay {
                    heading: String::new(),
                    body: String::new(),
                    buttons: vec![
                        super::super::ConfirmDialogButton {
                            label: "Cancel".into(),
                            destructive: false,
                        },
                        super::super::ConfirmDialogButton {
                            label: "Continue".into(),
                            destructive,
                        },
                    ],
                    focus_idx: 0,
                    hover_idx: None,
                    pressed_idx: None,
                    scroll: 0,
                };
                for index in 0..2 {
                    let normal = button_colors(&theme, theme.palette[4], &d, index);
                    d.hover_idx = Some(index);
                    let hovered = button_colors(&theme, theme.palette[4], &d, index);
                    d.pressed_idx = Some(index);
                    let pressed = button_colors(&theme, theme.palette[4], &d, index);
                    for (fill, label) in [normal, hovered, pressed] {
                        assert!(
                            super::super::color::contrast_ratio(fill, label) + 1e-6 >= 4.5,
                            "{name}"
                        );
                    }
                    assert_ne!(normal.0, hovered.0, "hover feedback in {name}");
                    assert_ne!(hovered.0, pressed.0, "pressed feedback in {name}");
                    d.hover_idx = None;
                    d.pressed_idx = None;
                }
                assert!(
                    super::super::color::contrast_ratio(
                        focus_color(&theme, theme.palette[4]),
                        theme.background
                    ) + 1e-6
                        >= 3.0,
                    "{name}"
                );
            }
        }
    }
}
