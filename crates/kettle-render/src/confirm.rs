//! Geometry and retained text for the final confirmation layer.
use super::{
    Attrs, ConfirmDialogDetail, ConfirmDialogOverlay, Family, FontSystem, GColor, Metrics, Rect4,
    Shaping, TextArea, TextBounds, TextBuffer, Weight, Wrap, display_width, take_cols_front,
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// What a retained block of dialog text is. It decides the block's colour and
/// how it sits in its rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextRole {
    Heading,
    Body,
    Preview,
    Findings,
    Link,
    Button(usize),
}

#[derive(Debug, Clone)]
struct TextItem {
    role: TextRole,
    text: String,
    /// Byte ranges of `text` drawn in the warning colour.
    marks: Vec<Range<usize>>,
    full: Rect4,
    clip: Rect4,
    /// Lines of a label centred in its rectangle.
    lines: usize,
}

/// One control token of the preview: its display line, the bytes of that
/// line that spell it, and the cells it takes with one cell of padding on
/// each side.
#[derive(Debug, Clone, PartialEq)]
struct TokenSpan {
    line: usize,
    bytes: Range<usize>,
    column: usize,
    columns: usize,
}

/// Resolved geometry of a dialog's [`ConfirmDialogDetail`]. Rectangles are
/// the visible parts, clipped to the scrolled body or to the dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmDetailLayout {
    /// The inset box around the preview.
    pub preview_rect: Rect4,
    /// The preview as drawn: wrapped, each control token padded by a space.
    pub preview_lines: Vec<String>,
    /// One chip per control token, placed by cell. The painter refines these
    /// from the shaped glyphs.
    pub tokens: Vec<Rect4>,
    pub summary_rect: Rect4,
    /// One rectangle per findings row, the trailing "more" row included.
    pub finding_rects: Vec<Rect4>,
    /// The findings as drawn, one string per row; a wrapped row holds `\n`.
    pub finding_lines: Vec<String>,
    pub policy_rect: Rect4,
    /// The policy text as drawn, with its separator when the link follows on
    /// the same line.
    pub policy: String,
    pub link_rect: Rect4,
    /// The link as drawn.
    pub link: String,
    /// The link sits on its own line below the policy text.
    pub link_wrapped: bool,
    link_full: Rect4,
    preview_text: Rect4,
    token_spans: Vec<TokenSpan>,
}

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
    /// The buttons sit in one column, each as wide as the dialog's content,
    /// instead of in one row.
    pub stacked: bool,
    pub detail: Option<ConfirmDetailLayout>,
    button_full: Vec<Rect4>,
    heading_full: Rect4,
    clip: Rect4,
    items: Vec<TextItem>,
    cell: f32,
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

/// How the buttons share the dialog.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arrangement {
    /// One row of equal widths inside the base width.
    EqualRow,
    /// One row, each button as wide as its own label, in a widened dialog.
    NaturalRow,
    /// One full-width column.
    Stack,
}

/// The separator between parts that share a line: the counts of the
/// summary, and the policy text and its link. It never ends a line: where a
/// line breaks between two parts, the separator is dropped.
const SEPARATOR: &str = " \u{b7} ";

/// The measured, not yet placed, parts of a [`ConfirmDialogDetail`].
struct DetailMeasure {
    preview_lines: Vec<String>,
    token_spans: Vec<TokenSpan>,
    summary: Vec<String>,
    /// Per findings row: its lines and the bytes of its token.
    findings: Vec<(Vec<String>, Range<usize>)>,
    policy: Vec<String>,
    link: Vec<String>,
    link_wrapped: bool,
}

impl DetailMeasure {
    fn new(detail: &ConfirmDialogDetail, columns: usize, preview_columns: usize) -> Self {
        let (preview_lines, token_spans) =
            wrap_preview(&detail.preview, &detail.preview_tokens, preview_columns);
        let summary = join_parts(&detail.summary, columns);
        let token_columns = detail
            .findings
            .iter()
            .map(|finding| display_width(&finding.token))
            .max()
            .unwrap_or(0);
        let count_columns = detail
            .findings
            .iter()
            .map(|finding| display_width(&finding.count))
            .max()
            .unwrap_or(0);
        // Two cells of indent, the token column, two cells, the meaning, two
        // cells, the count flush right.
        let lead = 2 + token_columns + 2;
        let meaning_columns = columns
            .saturating_sub(lead + 2 + count_columns)
            .max(8.min(columns));
        let mut findings: Vec<_> = detail
            .findings
            .iter()
            .map(|finding| {
                let meaning = wrap(&finding.meaning, meaning_columns);
                let mut lines = Vec::with_capacity(meaning.len());
                for (index, part) in meaning.iter().enumerate() {
                    let mut line = String::new();
                    if index == 0 {
                        line.push_str("  ");
                        line.push_str(&finding.token);
                        pad(&mut line, lead);
                        line.push_str(part);
                        pad(&mut line, lead + meaning_columns + 2);
                        pad(
                            &mut line,
                            lead + meaning_columns + 2 + count_columns
                                - display_width(&finding.count),
                        );
                        line.push_str(&finding.count);
                    } else {
                        pad(&mut line, lead);
                        line.push_str(part);
                    }
                    lines.push(line);
                }
                (lines, 2..2 + finding.token.len())
            })
            .collect();
        if !detail.more.is_empty() {
            let lines = wrap(&detail.more, columns.saturating_sub(2).max(1))
                .into_iter()
                .map(|line| format!("  {line}"))
                .collect();
            findings.push((lines, 0..0));
        }
        let one_line =
            display_width(&detail.policy) + display_width(SEPARATOR) + display_width(&detail.link)
                <= columns;
        let (policy, link) = if detail.link.is_empty() {
            (wrap(&detail.policy, columns), Vec::new())
        } else if one_line {
            (
                vec![format!("{}{SEPARATOR}", detail.policy)],
                vec![detail.link.clone()],
            )
        } else {
            // A separator with nothing after it on its line would dangle.
            (wrap(&detail.policy, columns), wrap(&detail.link, columns))
        };
        Self {
            preview_lines,
            token_spans,
            summary,
            findings,
            policy,
            link_wrapped: !one_line && !link.is_empty(),
            link,
        }
    }

    fn preview_height(&self, body_line: f32, unit: f32) -> f32 {
        self.preview_lines.len() as f32 * body_line + 12.0 * unit
    }

    fn findings_gap(&self, unit: f32) -> f32 {
        if self.findings.is_empty() {
            0.0
        } else {
            4.0 * unit
        }
    }

    /// Height of the part that scrolls with the body, below `lead` of gap.
    fn body_height(&self, body_line: f32, unit: f32, lead: f32) -> f32 {
        let rows: usize = self.findings.iter().map(|(lines, _)| lines.len()).sum();
        lead + self.preview_height(body_line, unit)
            + 8.0 * unit
            + self.summary.len() as f32 * body_line
            + self.findings_gap(unit)
            + rows as f32 * body_line
    }

    /// Lines of the pinned policy block above the buttons.
    fn policy_lines(&self) -> usize {
        self.policy.len()
            + if self.link_wrapped {
                self.link.len()
            } else {
                0
            }
    }
}

/// Lay `parts` out in lines of `columns` cells: parts share a line, joined by
/// [`SEPARATOR`], while they fit, and a part that does not fit starts the
/// next line with no separator left behind.
fn join_parts(parts: &[String], columns: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for part in parts.iter().filter(|part| !part.is_empty()) {
        for (index, piece) in wrap(part, columns).into_iter().enumerate() {
            match lines.last_mut() {
                Some(line)
                    if index == 0
                        && display_width(line)
                            + display_width(SEPARATOR)
                            + display_width(&piece)
                            <= columns =>
                {
                    line.push_str(SEPARATOR);
                    line.push_str(&piece);
                }
                _ => lines.push(piece),
            }
        }
    }
    lines
}

fn pad(line: &mut String, columns: usize) {
    for _ in display_width(line)..columns {
        line.push(' ');
    }
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
        let available = (sw - margin * 2.0).max(0.0);
        let padding_for = |width: f32| (24.0 * unit).min(width * 0.08).min(sh * 0.04);
        let gap = 12.0 * unit;
        let count = dialog.buttons.len();
        let gaps = gap * count.saturating_sub(1) as f32;
        let label_widths: Vec<f32> = dialog
            .buttons
            .iter()
            .map(|b| display_width(&b.label) as f32 * body_cell + 32.0 * unit)
            .collect();
        // Equal widths in one row inside the base width come first, so every
        // dialog that fits that way keeps its geometry. Failing that, each
        // button takes its own width and the dialog widens for the row, as far
        // as the window allows. Failing that too, the buttons stack edge to
        // edge, which leaves no empty block beside them.
        let base_width = (480.0 * unit).min(available);
        let base_inner = (base_width - padding_for(base_width) * 2.0).max(0.0);
        let equal_w = label_widths
            .iter()
            .copied()
            .fold(96.0 * unit, f32::max)
            .min(base_inner);
        let natural: Vec<f32> = label_widths.iter().map(|w| w.max(96.0 * unit)).collect();
        let natural_row = natural.iter().sum::<f32>() + gaps;
        let wide_limit = (800.0 * unit).min(available);
        let wide_width = natural_row + padding_for(wide_limit) * 2.0;
        let (width, arrangement) = if equal_w * count as f32 + gaps <= base_inner {
            (base_width, Arrangement::EqualRow)
        } else if wide_width <= wide_limit {
            (wide_width.max(base_width), Arrangement::NaturalRow)
        } else {
            (base_width, Arrangement::Stack)
        };
        let stacked = arrangement == Arrangement::Stack;
        let padding = padding_for(width);
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
        let text_h = body_lines.len() as f32 * body_line;
        let button_widths: Vec<f32> = match arrangement {
            Arrangement::EqualRow => vec![equal_w; count],
            Arrangement::NaturalRow => natural,
            Arrangement::Stack => vec![inner; count],
        };
        let button_labels: Vec<_> = dialog
            .buttons
            .iter()
            .zip(&button_widths)
            .map(|(b, width)| {
                let label_columns =
                    (((width - 24.0 * unit).max(body_cell) / body_cell).floor() as usize).max(1);
                wrap(&b.label, label_columns)
            })
            .collect();
        let button_lines: Vec<_> = button_labels.iter().map(Vec::len).collect();
        let button_h = (40.0 * unit)
            .max(button_lines.iter().copied().max().unwrap_or(1) as f32 * body_line + 16.0 * unit);
        let button_labels: Vec<String> = button_labels
            .into_iter()
            .map(|lines| lines.join("\n"))
            .collect();
        let buttons_h = if count == 0 {
            0.0
        } else if stacked {
            count as f32 * button_h + gaps
        } else {
            button_h
        };
        let preview_columns =
            ((((inner - 16.0 * unit).max(0.0)) / body_cell).floor() as usize).max(1);
        let measure = dialog
            .detail
            .as_ref()
            .map(|detail| DetailMeasure::new(detail, columns, preview_columns));
        let detail_lead = if dialog.body.is_empty() {
            0.0
        } else {
            10.0 * unit
        };
        let body_h = text_h
            + measure
                .as_ref()
                .map_or(0.0, |m| m.body_height(body_line, unit, detail_lead));
        // The policy line and its link are pinned with the buttons, so the
        // link stays reachable however far the body scrolls.
        let policy_h = measure
            .as_ref()
            .map_or(0.0, |m| m.policy_lines() as f32 * body_line + 12.0 * unit);
        let actions_h = policy_h + buttons_h;
        let heading_gap = if dialog.body.is_empty() && measure.is_none() {
            0.0
        } else {
            8.0 * unit
        };
        let action_gap = if measure.is_some() {
            16.0 * unit
        } else {
            24.0 * unit
        };
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
        let mut shift = scroll_offset as f32 * body_line;
        // Focus stops in content order from the top of the actions block: the
        // link, then the buttons.
        let link_lines = measure.as_ref().map_or(0, |m| m.link.len());
        let link_top = measure.as_ref().map_or(0.0, |m| {
            if m.link_wrapped {
                m.policy.len() as f32 * body_line
            } else {
                0.0
            }
        });
        let link_h = link_lines as f32 * body_line;
        let mut stops: Vec<(f32, f32)> = (0..count)
            .map(|index| {
                (
                    policy_h
                        + if stacked {
                            index as f32 * (button_h + gap)
                        } else {
                            0.0
                        },
                    button_h,
                )
            })
            .collect();
        if link_lines > 0 {
            stops.push((link_top, link_h));
        }
        if whole_scroll {
            for &(offset, stop_h) in &stops {
                if stop_h > clip.3 {
                    continue;
                }
                let top = heading_h + heading_gap + body_h + action_gap + offset;
                let minimum = (top + stop_h - clip.3).max(0.0);
                let first = (minimum / body_line).ceil() as usize;
                let last = (top / body_line).floor() as usize;
                // When no line offset exposes the full stop, let the two
                // bracketing offsets reach its visibility boundaries instead.
                if first > last && (scroll_offset == first || scroll_offset == last) {
                    shift = shift.clamp(minimum, top);
                    break;
                }
            }
        }
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
        let row_w = button_widths.iter().sum::<f32>() + gaps;
        let mut button_full = Vec::with_capacity(count);
        let mut bx = clip.0 + if stacked { 0.0 } else { inner - row_w };
        for (index, &button_w) in button_widths.iter().enumerate() {
            let by = action_top
                + policy_h
                + if stacked {
                    index as f32 * (button_h + gap)
                } else {
                    0.0
                };
            button_full.push((bx, by, button_w, button_h));
            if !stacked {
                bx += button_w + gap;
            }
        }
        let buttons: Vec<Rect4> = button_full.iter().map(|&r| intersect(r, clip)).collect();
        let heading_rect = intersect(heading_full, clip);
        let mut items = vec![
            TextItem {
                role: TextRole::Heading,
                text: heading_lines.join("\n"),
                marks: Vec::new(),
                full: heading_full,
                clip: heading_rect,
                lines: heading_lines.len(),
            },
            TextItem {
                role: TextRole::Body,
                text: body_lines.join("\n"),
                marks: Vec::new(),
                full: (body_full.0, body_full.1, inner, text_h),
                clip: body_rect,
                lines: body_lines.len(),
            },
        ];
        let detail = measure.map(|m| {
            let mut top = body_full.1 + text_h + detail_lead;
            let preview_full = (clip.0, top, inner, m.preview_height(body_line, unit));
            let preview_text = (
                clip.0 + 8.0 * unit,
                top + 6.0 * unit,
                (inner - 16.0 * unit).max(0.0),
                m.preview_lines.len() as f32 * body_line,
            );
            let preview_rect = intersect(preview_full, body_rect);
            let mut marks = Vec::with_capacity(m.token_spans.len());
            let mut line_starts = Vec::with_capacity(m.preview_lines.len());
            let mut offset = 0;
            for line in &m.preview_lines {
                line_starts.push(offset);
                offset += line.len() + 1;
            }
            let tokens = m
                .token_spans
                .iter()
                .map(|span| {
                    marks.push(
                        line_starts[span.line] + span.bytes.start
                            ..line_starts[span.line] + span.bytes.end,
                    );
                    intersect(
                        chip_rect(
                            preview_text,
                            span.line,
                            (span.column as f32 + 0.5) * body_cell,
                            (span.columns as f32 - 1.0) * body_cell,
                            body_line,
                            unit,
                        ),
                        preview_rect,
                    )
                })
                .collect();
            items.push(TextItem {
                role: TextRole::Preview,
                text: m.preview_lines.join("\n"),
                marks,
                full: preview_text,
                clip: intersect(preview_text, preview_rect),
                lines: m.preview_lines.len(),
            });
            top += preview_full.3 + 8.0 * unit;
            let summary_full = (clip.0, top, inner, m.summary.len() as f32 * body_line);
            items.push(TextItem {
                role: TextRole::Body,
                text: m.summary.join("\n"),
                marks: Vec::new(),
                full: summary_full,
                clip: intersect(summary_full, body_rect),
                lines: m.summary.len(),
            });
            top += summary_full.3 + m.findings_gap(unit);
            let findings_top = top;
            let mut finding_rects = Vec::with_capacity(m.findings.len());
            let mut finding_lines = Vec::with_capacity(m.findings.len());
            let mut text = String::new();
            let mut marks = Vec::new();
            let mut rows = 0;
            for (lines, token) in &m.findings {
                let row = (clip.0, top, inner, lines.len() as f32 * body_line);
                finding_rects.push(intersect(row, body_rect));
                if !text.is_empty() {
                    text.push('\n');
                }
                if !token.is_empty() {
                    marks.push(text.len() + token.start..text.len() + token.end);
                }
                let joined = lines.join("\n");
                text.push_str(&joined);
                finding_lines.push(joined);
                rows += lines.len();
                top += row.3;
            }
            let findings_full = (clip.0, findings_top, inner, rows as f32 * body_line);
            items.push(TextItem {
                role: TextRole::Findings,
                text,
                marks,
                full: findings_full,
                clip: intersect(findings_full, body_rect),
                lines: rows,
            });
            let policy_full = (clip.0, action_top, inner, m.policy.len() as f32 * body_line);
            let policy_text = m.policy.join("\n");
            items.push(TextItem {
                role: TextRole::Body,
                text: policy_text.clone(),
                marks: Vec::new(),
                full: policy_full,
                clip: intersect(policy_full, clip),
                lines: m.policy.len(),
            });
            let link_columns = m
                .link
                .iter()
                .map(|line| display_width(line))
                .max()
                .unwrap_or(0);
            let link_full = (
                clip.0
                    + if m.link_wrapped {
                        0.0
                    } else {
                        m.policy.first().map_or(0, |line| display_width(line)) as f32 * body_cell
                    },
                action_top + link_top,
                (link_columns as f32 * body_cell).min(inner),
                link_h,
            );
            let link_text = m.link.join("\n");
            items.push(TextItem {
                role: TextRole::Link,
                text: link_text.clone(),
                marks: Vec::new(),
                full: link_full,
                clip: intersect(link_full, clip),
                lines: m.link.len(),
            });
            ConfirmDetailLayout {
                preview_rect,
                preview_lines: m.preview_lines,
                tokens,
                summary_rect: intersect(summary_full, body_rect),
                finding_rects,
                finding_lines,
                policy_rect: intersect(policy_full, clip),
                policy: policy_text,
                link_rect: intersect(link_full, clip),
                link: link_text,
                link_wrapped: m.link_wrapped,
                link_full,
                preview_text,
                token_spans: m.token_spans,
            }
        });
        for (index, label) in button_labels.iter().enumerate() {
            items.push(TextItem {
                role: TextRole::Button(index),
                text: label.clone(),
                marks: Vec::new(),
                full: button_full[index],
                clip: buttons[index],
                lines: button_lines[index],
            });
        }
        Self {
            rect: (x, y, width, height),
            heading_rect,
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
            stacked,
            detail,
            button_full,
            heading_full,
            clip,
            items,
            cell: body_cell,
        }
    }

    /// The text of the first block with `role`, as drawn.
    #[cfg(test)]
    fn text(&self, role: TextRole) -> &str {
        &self
            .items
            .iter()
            .find(|item| item.role == role)
            .expect("a block with that role")
            .text
    }

    /// The link's index among the focus stops: one past the last button.
    /// `None` for a dialog without a link.
    pub fn link_stop(&self) -> Option<usize> {
        self.detail
            .as_ref()
            .filter(|detail| !detail.link.is_empty())
            .map(|_| self.buttons.len())
    }

    /// The visible rectangle of a focus stop: a button, or the link at
    /// [`Self::link_stop`].
    pub fn stop_rect(&self, index: usize) -> Option<Rect4> {
        if Some(index) == self.link_stop() {
            self.detail.as_ref().map(|detail| detail.link_rect)
        } else {
            self.buttons.get(index).copied()
        }
    }

    fn stop_full(&self, index: usize) -> Option<Rect4> {
        if Some(index) == self.link_stop() {
            self.detail.as_ref().map(|detail| detail.link_full)
        } else {
            self.button_full.get(index).copied()
        }
    }

    /// The scroll offset that shows focus stop `index` whole: a button, or
    /// the link at [`Self::link_stop`].
    pub fn scroll_to_button(&self, index: usize) -> usize {
        if !self.whole_scroll {
            return self.scroll_offset;
        }
        let Some(button) = self.stop_full(index) else {
            return self.scroll_offset;
        };
        let shift = self.clip.1 - self.heading_full.1;
        let top = button.1 + shift - self.clip.1;
        let bottom = top + button.3;
        // Aligned edges can differ by subpixel floating-point roundoff.
        if button.1 < self.clip.1 - 0.001 {
            (top / self.body_line).floor().max(0.0) as usize
        } else if button.1 + button.3 > self.clip.1 + self.clip.3 + 0.001 {
            ((bottom - self.clip.3) / self.body_line).ceil().max(0.0) as usize
        } else {
            self.scroll_offset
        }
        .min(self.max_scroll)
    }
}

/// A chip around the token whose letters start `left` into `line` of the
/// preview text and are `width` wide: half a cell of padding on each side is
/// already part of both.
fn chip_rect(text: Rect4, line: usize, left: f32, width: f32, body_line: f32, unit: f32) -> Rect4 {
    (
        text.0 + left,
        text.1 + line as f32 * body_line + 1.5 * unit,
        width,
        body_line - 3.0 * unit,
    )
}

/// Where a label shows its access key: the byte offset of the first word
/// that starts with the letter, or failing that of the letter's first
/// occurrence. Letters compare without case.
pub fn access_key_index(label: &str, key: char) -> Option<usize> {
    let same = |c: char| c.to_lowercase().eq(key.to_lowercase());
    let mut word_start = true;
    let mut first = None;
    for (index, c) in label.char_indices() {
        if same(c) {
            if word_start {
                return Some(index);
            }
            first.get_or_insert(index);
        }
        word_start = !c.is_alphanumeric();
    }
    first
}

/// One unit of the preview: a grapheme, a control token, or a line break.
enum PreviewAtom<'a> {
    Text(&'a str),
    Token(&'a str),
    Break,
}

impl PreviewAtom<'_> {
    /// Cells taken; a token has one cell of padding on each side.
    fn width(&self) -> usize {
        match self {
            Self::Text(text) => display_width(text),
            Self::Token(text) => display_width(text) + 2,
            Self::Break => 0,
        }
    }

    fn is_space(&self) -> bool {
        matches!(self, Self::Text(text) if text.trim().is_empty())
    }
}

fn push_preview_text<'a>(text: &'a str, atoms: &mut Vec<PreviewAtom<'a>>) {
    for grapheme in text.graphemes(true) {
        atoms.push(if grapheme == "\n" {
            PreviewAtom::Break
        } else {
            PreviewAtom::Text(grapheme)
        });
    }
}

/// Wrap the literal preview to `columns` cells. Unlike [`wrap`] it keeps
/// every space, since the text is shown as it will be sent. A line breaks
/// before a word that fits on a line of its own, so a control stays with the
/// text it is part of (`ESC` with its `[201~`); a longer word breaks
/// anywhere. Each range of `tokens` is kept whole and padded with one space
/// on each side, which its chip covers.
fn wrap_preview(
    preview: &str,
    tokens: &[Range<usize>],
    columns: usize,
) -> (Vec<String>, Vec<TokenSpan>) {
    let mut atoms = Vec::new();
    let mut cursor = 0;
    for token in tokens {
        // A range that is out of order, out of bounds or inside a character
        // is not a token; its text is drawn plainly with the rest.
        if token.start < cursor
            || token.end <= token.start
            || token.end > preview.len()
            || !preview.is_char_boundary(token.start)
            || !preview.is_char_boundary(token.end)
        {
            continue;
        }
        push_preview_text(&preview[cursor..token.start], &mut atoms);
        atoms.push(PreviewAtom::Token(&preview[token.clone()]));
        cursor = token.end;
    }
    push_preview_text(&preview[cursor..], &mut atoms);

    let mut lines = vec![String::new()];
    let mut spans = Vec::new();
    let mut column = 0;
    let mut index = 0;
    while index < atoms.len() {
        if matches!(atoms[index], PreviewAtom::Break) {
            lines.push(String::new());
            column = 0;
            index += 1;
            continue;
        }
        // A word runs to the next space or line break. A space is a word of
        // its own, so it is never dropped.
        let end = if atoms[index].is_space() {
            index + 1
        } else {
            atoms[index..]
                .iter()
                .position(|atom| atom.is_space() || matches!(atom, PreviewAtom::Break))
                .map_or(atoms.len(), |length| index + length)
        };
        let word: usize = atoms[index..end].iter().map(PreviewAtom::width).sum();
        if column > 0 && column + word > columns && word <= columns {
            lines.push(String::new());
            column = 0;
        }
        for atom in &atoms[index..end] {
            let cells = atom.width();
            if column > 0 && column + cells > columns {
                lines.push(String::new());
                column = 0;
            }
            let row = lines.len() - 1;
            let line = &mut lines[row];
            match atom {
                PreviewAtom::Token(text) => {
                    line.push(' ');
                    let start = line.len();
                    line.push_str(text);
                    line.push(' ');
                    spans.push(TokenSpan {
                        line: row,
                        bytes: start..start + text.len(),
                        column,
                        columns: cells,
                    });
                }
                PreviewAtom::Text(text) => line.push_str(text),
                PreviewAtom::Break => {}
            }
            column += cells;
        }
        index = end;
    }
    (lines, spans)
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
                    // A no-break space keeps a count with its noun.
                    .find(|&(_, c)| c.is_whitespace() && !matches!(c, '\u{a0}' | '\u{202f}'))
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

/// Colours of the dialog's text blocks, by role.
pub(crate) struct ConfirmTextColors {
    pub text: GColor,
    pub link: GColor,
    pub buttons: Vec<GColor>,
}

/// A thin line the painter draws under text: a link's underline, or the
/// mark under an access-key letter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Underline {
    pub role: TextRole,
    pub rect: Rect4,
}

/// The marked ranges a buffer was last shaped with, and their colour.
#[derive(Default, PartialEq)]
struct MarkStyle {
    marks: Vec<Range<usize>>,
    color: Option<[u8; 3]>,
}

pub(crate) struct ConfirmText {
    buffers: Vec<TextBuffer>,
    texts: Vec<String>,
    styles: Vec<MarkStyle>,
    family: String,
}
impl ConfirmText {
    pub fn new() -> Self {
        Self {
            buffers: Vec::new(),
            texts: Vec::new(),
            styles: Vec::new(),
            family: String::new(),
        }
    }
    pub fn prepare(
        &mut self,
        fonts: &mut FontSystem,
        family: &str,
        layout: &ConfirmDialogLayout,
        warning: GColor,
    ) {
        let family_changed = self.family != family;
        self.family = family.to_string();
        while self.buffers.len() < layout.items.len() {
            self.buffers.push(TextBuffer::new(
                fonts,
                Metrics::new(layout.body_font, layout.body_line),
            ));
            self.texts.push(String::new());
            self.styles.push(MarkStyle::default());
        }
        // Blocks a previous dialog had and this one lacks draw nothing and
        // keep no text.
        for index in layout.items.len()..self.buffers.len() {
            self.texts[index].clear();
        }
        for (index, item) in layout.items.iter().enumerate() {
            let heading = item.role == TextRole::Heading;
            let buffer = &mut self.buffers[index];
            buffer.set_metrics(if heading {
                Metrics::new(layout.heading_font, layout.heading_line)
            } else {
                Metrics::new(layout.body_font, layout.body_line)
            });
            buffer.set_wrap(Wrap::None);
            buffer.set_size(Some(item.full.2.max(1.0)), None);
            let style = MarkStyle {
                marks: item.marks.clone(),
                color: (!item.marks.is_empty()).then_some([warning.r(), warning.g(), warning.b()]),
            };
            if family_changed || self.texts[index] != item.text || self.styles[index] != style {
                let attrs = Attrs::new()
                    .family(Family::Name(family))
                    .weight(if heading {
                        Weight::BOLD
                    } else {
                        Weight::NORMAL
                    });
                if item.marks.is_empty() {
                    buffer.set_text(&item.text, &attrs, Shaping::Advanced, None);
                } else {
                    let marked = attrs.clone().color(warning);
                    let mut spans = Vec::with_capacity(item.marks.len() * 2 + 1);
                    let mut cursor = 0;
                    for mark in &item.marks {
                        spans.push((&item.text[cursor..mark.start], attrs.clone()));
                        spans.push((&item.text[mark.clone()], marked.clone()));
                        cursor = mark.end;
                    }
                    spans.push((&item.text[cursor..], attrs.clone()));
                    buffer.set_rich_text(spans, &attrs, Shaping::Advanced, None);
                }
                self.texts[index].clone_from(&item.text);
                self.styles[index] = style;
            }
            buffer.shape_until_scroll(fonts, false);
        }
    }
    pub fn key(&self) -> &[String] {
        &self.texts
    }

    /// Where a block's text starts: a button label is centred in its
    /// rectangle, every other block starts at its top left.
    fn origin(&self, layout: &ConfirmDialogLayout, index: usize) -> (f32, f32) {
        let item = &layout.items[index];
        if !matches!(item.role, TextRole::Button(_)) {
            return (item.full.0, item.full.1);
        }
        let width = self.buffers[index]
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0, f32::max);
        (
            item.full.0 + (item.full.2 - width).max(0.0) * 0.5,
            item.full.1 + (item.full.3 - layout.body_line * item.lines as f32) * 0.5,
        )
    }

    pub fn areas<'a>(
        &'a self,
        layout: &ConfirmDialogLayout,
        colors: &ConfirmTextColors,
    ) -> Vec<TextArea<'a>> {
        layout
            .items
            .iter()
            .zip(&self.buffers)
            .enumerate()
            .map(|(index, (item, buffer))| {
                let (left, top) = self.origin(layout, index);
                TextArea {
                    buffer,
                    left,
                    top,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: item.clip.0.ceil() as i32,
                        top: item.clip.1.ceil() as i32,
                        right: (item.clip.0 + item.clip.2).floor() as i32,
                        bottom: (item.clip.1 + item.clip.3).floor() as i32,
                    },
                    default_color: match item.role {
                        TextRole::Link => colors.link,
                        TextRole::Button(button) => colors.buttons[button],
                        _ => colors.text,
                    },
                    custom_glyphs: &[],
                }
            })
            .collect()
    }

    /// The chip of each preview token, around the letters as shaped. A font
    /// that does not keep the cell grid (a fallback face for wide or rare
    /// characters earlier on the line) moves the letters, and the chip moves
    /// with them.
    pub fn chips(&self, layout: &ConfirmDialogLayout) -> Vec<Rect4> {
        let Some(detail) = &layout.detail else {
            return Vec::new();
        };
        let Some(index) = layout
            .items
            .iter()
            .position(|item| item.role == TextRole::Preview)
        else {
            return Vec::new();
        };
        let buffer = &self.buffers[index];
        detail
            .token_spans
            .iter()
            .zip(&detail.tokens)
            .map(|(span, &estimate)| {
                let shaped =
                    buffer
                        .layout_runs()
                        .find(|run| run.line_i == span.line)
                        .and_then(|run| {
                            run.glyphs
                                .iter()
                                .filter(|glyph| {
                                    glyph.start >= span.bytes.start && glyph.end <= span.bytes.end
                                })
                                .fold(None, |extent: Option<(f32, f32)>, glyph| {
                                    let (left, right) = (glyph.x, glyph.x + glyph.w);
                                    Some(extent.map_or((left, right), |(l, r)| {
                                        (l.min(left), r.max(right))
                                    }))
                                })
                        });
                match shaped {
                    Some((left, right)) => intersect(
                        chip_rect(
                            detail.preview_text,
                            span.line,
                            left - layout.cell * 0.5,
                            right - left + layout.cell,
                            layout.body_line,
                            layout.unit,
                        ),
                        detail.preview_rect,
                    ),
                    None => estimate,
                }
            })
            .collect()
    }

    /// The link's underline, and a mark under each access-key letter. A
    /// link is underlined so it does not rely on its colour; its access key
    /// gets a heavier stroke over that line.
    pub fn underlines(
        &self,
        layout: &ConfirmDialogLayout,
        dialog: &ConfirmDialogOverlay,
    ) -> Vec<Underline> {
        let mut lines = Vec::new();
        for (index, item) in layout.items.iter().enumerate() {
            let key = match item.role {
                TextRole::Button(button) => dialog.buttons.get(button).and_then(|b| b.access_key),
                TextRole::Link => dialog.detail.as_ref().and_then(|d| d.link_access_key),
                _ => continue,
            };
            let link = item.role == TextRole::Link;
            if !link && key.is_none() {
                continue;
            }
            let (left, top) = self.origin(layout, index);
            // The key's place as (line, byte within the line).
            let key_at = key
                .and_then(|key| access_key_index(&item.text, key))
                .map(|offset| {
                    let before = &item.text[..offset];
                    (
                        before.matches('\n').count(),
                        offset - before.rfind('\n').map_or(0, |at| at + 1),
                    )
                });
            for run in self.buffers[index].layout_runs() {
                let y = top + run.line_y + 2.0 * layout.unit;
                if link {
                    lines.push(Underline {
                        role: item.role,
                        rect: intersect((left, y, run.line_w, layout.unit), item.clip),
                    });
                }
                if let Some((line, byte)) = key_at
                    && line == run.line_i
                    && let Some(glyph) = run.glyphs.iter().find(|glyph| glyph.start == byte)
                {
                    let thickness = if link { 2.0 } else { 1.0 } * layout.unit;
                    lines.push(Underline {
                        role: item.role,
                        rect: intersect((left + glyph.x, y, glyph.w, thickness), item.clip),
                    });
                }
            }
        }
        lines
    }
}

/// Colours of a dialog's detail block.
pub(crate) struct DetailColors {
    /// Fill of the inset box around the preview.
    pub preview_fill: kettle_config::Rgb,
    /// Faint fill of a control token's chip.
    pub chip_fill: kettle_config::Rgb,
    /// Letters and outline of a control token, readable on the chip, the
    /// box and the dialog.
    pub warning: kettle_config::Rgb,
    pub link: kettle_config::Rgb,
}

/// The accent a dialog's recommended button and its link use. It is the
/// window's accent unless that could be taken for the warning colour: a
/// window whose accent is the theme's red would otherwise paint the
/// recommended choice and the destructive one alike. Then it is the first
/// palette colour whose hue is clearly another.
pub(crate) fn choice_accent(
    theme: &kettle_config::Theme,
    accent: kettle_config::Rgb,
) -> kettle_config::Rgb {
    use super::color;
    let warning = theme.palette[1];
    let (warning_hue, warning_saturation, _) = color::hsl(warning);
    // Hue runs over six sextants. Within one sextant of the warning's hue a
    // saturated colour reads as the same colour.
    let distinct = |candidate: kettle_config::Rgb| {
        let (hue, saturation, _) = color::hsl(candidate);
        let apart = (hue - warning_hue).rem_euclid(6.0);
        candidate != warning
            && (saturation < 0.15 || warning_saturation < 0.15 || apart.min(6.0 - apart) >= 1.0)
    };
    [accent]
        .into_iter()
        .chain([4, 2, 6, 5, 3].map(|index| theme.palette[index]))
        .find(|candidate| distinct(*candidate))
        // A theme of one hue has no other colour to offer; a tone between
        // its background and foreground at least differs in lightness.
        .unwrap_or_else(|| color::over(theme.background, theme.foreground, 0.35, false))
}

pub(crate) fn detail_colors(
    theme: &kettle_config::Theme,
    accent: kettle_config::Rgb,
) -> DetailColors {
    use super::color;
    let preview_fill = color::over(theme.background, theme.foreground, 0.06, false);
    let chip_fill = color::over(preview_fill, theme.palette[1], 0.14, false);
    // The warning colour is drawn on the chip, on the box and on the dialog.
    // Contrast depends on luminance alone, so a colour that passes on the
    // lightest and the darkest of the three passes on the one between.
    let mut surfaces = [theme.background, preview_fill, chip_fill];
    surfaces
        .sort_by(|a, b| color::relative_luminance(*a).total_cmp(&color::relative_luminance(*b)));
    let warning =
        color::with_min_contrast_on_both(theme.palette[1], surfaces[0], surfaces[2], 4.5, false);
    DetailColors {
        preview_fill,
        chip_fill,
        warning,
        link: color::with_min_contrast(accent, theme.background, 4.5),
    }
}

pub(crate) fn button_colors(
    theme: &kettle_config::Theme,
    accent: kettle_config::Rgb,
    dialog: &ConfirmDialogOverlay,
    index: usize,
) -> (kettle_config::Rgb, kettle_config::Rgb) {
    use super::color;
    // One accent button at most says which choice is recommended; a choice
    // that is neither that nor destructive takes the first button's fill.
    let base = if index == 0 || dialog.buttons[index].neutral {
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

    /// A count and its noun, joined by a no-break space, never split.
    #[test]
    fn a_no_break_space_keeps_a_count_with_its_noun() {
        let lines = super::wrap("send 2\u{a0}lines now", 8);
        assert_eq!(lines, ["send", "2\u{a0}lines", "now"]);
    }

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
                    ..Default::default()
                },
                super::super::ConfirmDialogButton {
                    label: "Cerrar ventana 界".into(),
                    destructive: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }
    #[test]
    fn focused_button_is_fully_visible_when_it_fits() {
        let mut d = dialog();
        d.buttons[0].label = "Cancel".into();
        d.buttons[1].label = "Close window".into();
        let initial = ConfirmDialogLayout::new((120.0, 100.0), (8.0, 16.0), 1.0, &d);
        d.scroll = initial.scroll_to_button(0);
        let focused = ConfirmDialogLayout::new((120.0, 100.0), (8.0, 16.0), 1.0, &d);
        assert!(
            (focused.buttons[0].3 - focused.button_full[0].3).abs() < 0.01,
            "focused button is clipped: {:?} versus {:?}",
            focused.buttons[0],
            focused.button_full[0]
        );
    }

    #[test]
    fn focused_button_is_fully_visible_between_line_offsets() {
        for height in 68..=120 {
            let surface = (120.0, height as f32);
            let mut d = dialog();
            d.buttons[0].label = "Cancel".into();
            d.buttons[1].label = "Close window".into();
            for start in [0, usize::MAX] {
                d.scroll = start;
                for index in [0, 1, 0] {
                    let initial = ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &d);
                    assert!(initial.whole_scroll);
                    assert!(initial.button_full[index].3 <= initial.clip.3);
                    d.scroll = initial.scroll_to_button(index);
                    let focused = ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &d);
                    assert!(
                        (focused.buttons[index].3 - focused.button_full[index].3).abs() < 0.01,
                        "button {index} clipped at {surface:?}: {:?} versus {:?}",
                        focused.buttons[index],
                        focused.button_full[index]
                    );
                    assert_eq!(focused.scroll_to_button(index), d.scroll);
                }
            }
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
            assert!(!layout.text(TextRole::Body).contains('…'));
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
            narrow
                .text(TextRole::Button(1))
                .split_whitespace()
                .collect::<Vec<_>>(),
            d.buttons[1].label.split_whitespace().collect::<Vec<_>>()
        );
        assert_eq!(narrow.buttons[0].2, narrow.buttons[1].2);
    }
}

#[cfg(test)]
mod paste_layout_tests {
    use super::super::{ConfirmDialogButton, ConfirmDialogFinding};
    use super::*;

    const EN: [&str; 6] = [
        "Cancel",
        "Paste without controls",
        "Paste as one line",
        "Paste",
        "Paste protection: Automatic",
        "Paste settings…",
    ];
    const ES: [&str; 6] = [
        "Cancelar",
        "Pegar sin caracteres de control",
        "Pegar en una sola línea",
        "Pegar",
        "Protección al pegar: Automático",
        "Configuración de pegado…",
    ];

    /// The Confirm paste dialog as kettle-ui projects it for
    /// `printf 'x'<ESC>[201~; echo injected<LF>`.
    fn paste(text: [&str; 6]) -> ConfirmDialogOverlay {
        let button = |label: &str, key: char| ConfirmDialogButton {
            label: label.into(),
            access_key: Some(key),
            ..Default::default()
        };
        let preview = "printf 'x'ESC[201~; echo injected";
        ConfirmDialogOverlay {
            heading: "Confirm paste".into(),
            body: "This text contains ESC[201~, the code that ends a bracketed paste. Kettle \
                   removes it, but the text after it may have been written to run as commands."
                .into(),
            buttons: vec![
                button(text[0], 'C'),
                button(text[1], 'W'),
                ConfirmDialogButton {
                    neutral: true,
                    ..button(text[2], 'O')
                },
                ConfirmDialogButton {
                    destructive: true,
                    ..button(text[3], 'P')
                },
            ],
            detail: Some(ConfirmDialogDetail {
                preview: preview.into(),
                preview_tokens: vec![Range { start: 10, end: 13 }],
                summary: vec![
                    "1\u{a0}line, ends with a line break".into(),
                    "1\u{a0}escape sequence".into(),
                ],
                findings: vec![ConfirmDialogFinding {
                    token: "ESC[201~".into(),
                    meaning: "ends bracketed paste".into(),
                    count: "\u{d7}1".into(),
                }],
                more: String::new(),
                policy: text[4].into(),
                link: text[5].into(),
                link_access_key: Some('S'),
                description: String::new(),
            }),
            ..Default::default()
        }
    }

    fn inside(inner: Rect4, outer: Rect4) -> bool {
        inner.0 >= outer.0 - 0.01
            && inner.1 >= outer.1 - 0.01
            && inner.0 + inner.2 <= outer.0 + outer.2 + 0.01
            && inner.1 + inner.3 <= outer.1 + outer.3 + 0.01
    }

    /// Guard for every dialog that already fit: equal widths in one row
    /// inside 480 units keep their exact rectangles.
    #[test]
    fn a_two_button_dialog_keeps_its_rectangles() {
        let dialog = ConfirmDialogOverlay {
            heading: "Close this window?".into(),
            body:
                "Both panes will close. Commands may be interrupted and unsaved work may be lost."
                    .into(),
            buttons: vec![
                ConfirmDialogButton {
                    label: "Cancelar".into(),
                    ..Default::default()
                },
                ConfirmDialogButton {
                    label: "Cerrar ventana 界".into(),
                    destructive: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let layout = ConfirmDialogLayout::new((800.0, 600.0), (8.0, 16.0), 1.0, &dialog);
        assert_eq!(layout.rect.0, 160.0);
        assert_eq!(layout.rect.2, 480.0);
        assert_eq!(
            layout.buttons,
            [
                (242.5, layout.buttons[0].1, 180.75, 40.0),
                (435.25, layout.buttons[0].1, 180.75, 40.0)
            ]
        );
        assert_eq!(
            layout.buttons[0].1 + 40.0,
            layout.rect.1 + layout.rect.3 - 24.0
        );
    }

    #[test]
    fn four_paste_buttons_share_one_row_when_the_window_allows() {
        // A cell is 0.625 of the body font at the (8, 16) test metrics and
        // 0.6 at (8.4, 17.5), the advance of most monospace faces. English
        // fits a row from a 730-unit window at the first; Spanish needs the
        // second and an 845-unit window, and stacks at the wider cell.
        for (text, metrics, scale, surface, width) in [
            (EN, (8.0, 16.0), 1.0, (1280.0, 800.0), 681.25),
            (EN, (16.0, 32.0), 2.0, (2560.0, 1600.0), 681.25),
            (EN, (8.0, 16.0), 1.0, (730.0, 800.0), 681.25),
            (ES, (8.4, 17.5), 1.0, (1280.0, 800.0), 796.8),
            (ES, (8.4, 17.5), 1.0, (845.0, 800.0), 796.8),
        ] {
            let cell = metrics.0 * (14.0 * scale / (metrics.1 / 1.25));
            let dialog = paste(text);
            let layout = ConfirmDialogLayout::new(surface, metrics, scale, &dialog);
            assert!(!layout.stacked, "{text:?} at {surface:?}");
            assert!(layout.rect.2 > 480.0 * scale && layout.rect.2 <= 800.0 * scale);
            assert!(
                (layout.rect.2 - width * scale).abs() < 0.01,
                "{:?}",
                layout.rect
            );
            let y = layout.buttons[0].1;
            let mut right = layout.rect.0;
            for (index, button) in layout.buttons.iter().enumerate() {
                assert_eq!(button.1, y, "button {index} left the row");
                assert!(button.0 >= right - 0.01, "button {index} overlaps");
                right = button.0 + button.2;
                // Natural width: the label and 32 units, at least 96.
                let label = display_width(&dialog.buttons[index].label) as f32 * cell;
                assert!((button.2 - (label + 32.0 * scale).max(96.0 * scale)).abs() < 0.01);
                assert!(!layout.text(TextRole::Button(index)).contains('\n'));
            }
            // The row ends at the content's right edge: no block is left
            // empty beside a column of buttons.
            assert!((right - (layout.rect.0 + layout.rect.2 - 24.0 * scale)).abs() < 0.01);
            assert!(inside(layout.rect, (0.0, 0.0, surface.0, surface.1)));
        }
    }

    #[test]
    fn paste_buttons_stack_at_the_full_inner_width_when_the_row_does_not_fit() {
        for (text, surface, metrics, scale) in [
            (EN, (520.0, 600.0), (8.0, 16.0), 1.0),
            (EN, (700.0, 600.0), (8.0, 16.0), 1.0),
            (ES, (840.0, 600.0), (8.4, 17.5), 1.0),
            // At the wider cell the Spanish row passes 800 units and stacks
            // however wide the window is.
            (ES, (1600.0, 900.0), (8.0, 16.0), 1.0),
            (ES, (1040.0, 1200.0), (16.0, 32.0), 2.0),
            (EN, (300.0, 700.0), (8.0, 16.0), 1.0),
        ] {
            let dialog = paste(text);
            let layout = ConfirmDialogLayout::new(surface, metrics, scale, &dialog);
            assert!(layout.stacked, "{text:?} at {surface:?}");
            assert!(layout.rect.2 <= 480.0 * scale + 0.01);
            let mut bottom = f32::MIN;
            for (index, (button, full)) in
                layout.buttons.iter().zip(&layout.button_full).enumerate()
            {
                assert_eq!(full.0, layout.clip.0, "button {index}");
                assert_eq!(full.2, layout.clip.2, "button {index} is not full width");
                assert!(full.1 > bottom, "button {index} is out of order");
                bottom = full.1 + full.3;
                assert!(inside(*button, layout.rect));
            }
        }
    }

    #[test]
    fn the_policy_separator_never_dangles_at_the_end_of_a_line() {
        let wide = ConfirmDialogLayout::new((1280.0, 800.0), (8.0, 16.0), 1.0, &paste(EN));
        let detail = wide.detail.as_ref().unwrap();
        assert!(!detail.link_wrapped);
        assert_eq!(detail.policy, "Paste protection: Automatic \u{b7} ");
        assert_eq!(detail.link, "Paste settings…");
        assert_eq!(detail.link_rect.1, detail.policy_rect.1);
        // The link starts where the policy text and its separator end.
        assert!(
            (detail.link_rect.0 - (detail.policy_rect.0 + 30.0 * 8.75)).abs() < 0.01,
            "{:?}",
            detail.link_rect
        );
        assert!((detail.link_rect.2 - 15.0 * 8.75).abs() < 0.01);
        for (text, surface) in [(ES, (520.0, 700.0)), (EN, (360.0, 700.0))] {
            let narrow = ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &paste(text));
            let detail = narrow.detail.as_ref().unwrap();
            assert!(detail.link_wrapped, "{text:?}");
            assert!(!detail.policy.contains('\u{b7}'), "{:?}", detail.policy);
            assert_eq!(detail.link_rect.0, detail.policy_rect.0);
            assert!(detail.link_rect.1 >= detail.policy_rect.1 + detail.policy_rect.3 - 0.01);
            // The link stays above the first button, inside the dialog.
            assert!(detail.link_rect.1 + detail.link_rect.3 <= narrow.buttons[0].1 + 0.01);
            assert!(inside(detail.link_rect, narrow.rect));
        }
    }

    #[test]
    fn the_summary_separator_never_dangles_either() {
        let mut dialog = paste(ES);
        let parts = [
            "2\u{a0}líneas, termina con un salto de línea",
            "1\u{a0}secuencia de escape",
            "3\u{a0}caracteres de control más",
        ];
        let mut summary = |surface, count: usize| {
            dialog.detail.as_mut().unwrap().summary =
                parts[..count].iter().map(|part| part.to_string()).collect();
            let layout = ConfirmDialogLayout::new(surface, (8.4, 17.5), 1.0, &dialog);
            layout
                .items
                .iter()
                .filter(|item| item.role == TextRole::Body)
                .nth(1)
                .expect("the summary block")
                .text
                .clone()
        };
        // Wide: the parts that fit share a line, joined by the separator.
        assert_eq!(
            summary((1280.0, 800.0), 2),
            "2\u{a0}líneas, termina con un salto de línea \u{b7} 1\u{a0}secuencia de escape"
        );
        // A part that does not fit starts the next line, and no line ends or
        // starts with the separator, at any width.
        for surface in [
            (1280.0, 800.0),
            (520.0, 800.0),
            (400.0, 800.0),
            (300.0, 800.0),
        ] {
            let text = summary(surface, 3);
            assert!(text.contains('\n'), "{surface:?}: {text:?}");
            for line in text.split('\n') {
                let line = line.trim();
                assert!(
                    !line.ends_with('\u{b7}') && !line.starts_with('\u{b7}'),
                    "{line:?}"
                );
            }
            assert_eq!(text.matches("secuencia").count(), 1);
            assert_eq!(text.matches("caracteres").count(), 1);
        }
        assert_eq!(
            join_parts(&["a".into(), String::new(), "bb".into(), "cc".into()], 6),
            ["a \u{b7} bb", "cc"]
        );
        assert!(join_parts(&[], 10).is_empty());
    }

    #[test]
    fn the_recommended_accent_is_never_the_warning_colour() {
        use super::super::color::hsl;
        let apart = |a: kettle_config::Rgb, b: kettle_config::Rgb| {
            let ((hue, saturation, _), (other, other_saturation, _)) = (hsl(a), hsl(b));
            let turn = (hue - other).rem_euclid(6.0);
            saturation < 0.15 || other_saturation < 0.15 || turn.min(6.0 - turn) >= 1.0
        };
        let mut hue_themes = 0;
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            let warning = theme.palette[1];
            // Whether the theme has any colour of another hue to offer.
            let has_other_hue = [4, 2, 6, 5, 3].iter().any(|&index| {
                theme.palette[index] != warning && apart(theme.palette[index], warning)
            });
            hue_themes += usize::from(has_other_hue);
            // A window's accent can be any palette colour, the red included.
            for window_accent in (1..=6).map(|index| theme.palette[index]) {
                let accent = choice_accent(&theme, window_accent);
                assert_ne!(accent, warning, "{name}");
                if has_other_hue {
                    assert!(
                        apart(accent, warning),
                        "{name}: {accent:?} reads as the warning {warning:?}"
                    );
                }
                // An accent that is already another colour is kept.
                if window_accent != warning && apart(window_accent, warning) {
                    assert_eq!(accent, window_accent, "{name}");
                }
                // The recommended and the destructive button differ.
                let dialog = paste(EN);
                let recommended = button_colors(&theme, accent, &dialog, 1).0;
                let destructive = button_colors(&theme, accent, &dialog, 3).0;
                assert_ne!(recommended, destructive, "{name}");
            }
            // The case the live capture showed: the window's accent is the
            // theme's red, and the two buttons were painted alike.
            let dialog = paste(EN);
            assert_eq!(
                button_colors(&theme, warning, &dialog, 1).0,
                button_colors(&theme, warning, &dialog, 3).0,
                "{name}: the unadjusted accent is the hazard"
            );
        }
        assert!(hue_themes > 100, "most themes have more than one hue");
    }

    #[test]
    fn a_short_window_caps_the_dialog_and_keeps_every_stop_reachable() {
        for (text, surface) in [
            (EN, (520.0, 420.0)),
            (ES, (520.0, 380.0)),
            (EN, (520.0, 200.0)),
            (ES, (300.0, 160.0)),
            (EN, (1280.0, 260.0)),
        ] {
            let mut dialog = paste(text);
            let layout = ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &dialog);
            let margin = 24.0f32.min(surface.0 * 0.05).min(surface.1 * 0.05);
            assert!(
                layout.rect.3 <= surface.1 - margin * 2.0 + 0.01,
                "{surface:?}: {:?}",
                layout.rect
            );
            assert!(inside(layout.rect, (0.0, 0.0, surface.0, surface.1)));
            assert!(layout.max_scroll > 0, "{surface:?} should scroll");
            if !layout.whole_scroll {
                // The body scrolls; the policy line and the buttons stay put.
                dialog.scroll = layout.max_scroll;
                let end = ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &dialog);
                assert_eq!(end.buttons, layout.buttons);
                assert_eq!(
                    end.detail.as_ref().unwrap().link_rect,
                    layout.detail.as_ref().unwrap().link_rect
                );
                dialog.scroll = 0;
            }
            let link = layout.link_stop().unwrap();
            assert_eq!(link, 4);
            for stop in [link, 0, 1, 2, 3, link, 3] {
                let before = ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &dialog);
                dialog.scroll = before.scroll_to_button(stop);
                let after = ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &dialog);
                let (shown, full) = (
                    after.stop_rect(stop).unwrap(),
                    after.stop_full(stop).unwrap(),
                );
                if full.3 <= after.clip.3 {
                    assert!(
                        (shown.3 - full.3).abs() < 0.01,
                        "stop {stop} is clipped at {surface:?}: {shown:?} of {full:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_preview_sits_in_a_box_with_a_chip_around_each_control() {
        let mut dialog = paste(EN);
        let detail = dialog.detail.as_mut().unwrap();
        detail.preview = "aESCbBEL^C\nsecond line".into();
        detail.preview_tokens = vec![1..4, 5..8, 8..10];
        for surface in [(1280.0, 800.0), (520.0, 700.0)] {
            let layout = ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &dialog);
            let detail = layout.detail.as_ref().unwrap();
            // Each token keeps one cell of padding inside its chip.
            assert_eq!(detail.preview_lines, ["a ESC b BEL  ^C ", "second line"]);
            assert_eq!(detail.tokens.len(), 3);
            let cell = 8.75;
            let text_left = detail.preview_rect.0 + 8.0;
            for (chip, (column, letters)) in detail.tokens.iter().zip([(1, 3), (7, 3), (12, 2)]) {
                assert!(inside(*chip, detail.preview_rect), "{chip:?}");
                assert!((chip.0 - (text_left + (column as f32 + 0.5) * cell)).abs() < 0.01);
                assert!((chip.2 - (letters as f32 + 1.0) * cell).abs() < 0.01);
                assert!(chip.3 < layout.body_line);
            }
            // Chips never touch: there is a cell between two of them.
            assert!(detail.tokens[2].0 - (detail.tokens[1].0 + detail.tokens[1].2) >= cell - 0.01);
            assert!(inside(detail.preview_rect, layout.body_rect));
            assert!(detail.preview_rect.1 >= layout.heading_rect.1 + layout.heading_rect.3);
            assert!(detail.summary_rect.1 >= detail.preview_rect.1 + detail.preview_rect.3);
            assert!(detail.finding_rects[0].1 >= detail.summary_rect.1 + detail.summary_rect.3);
            assert!(detail.policy_rect.1 >= layout.body_rect.1 + layout.body_rect.3);
            // The marked bytes of the preview block are the tokens' letters.
            let preview = layout
                .items
                .iter()
                .find(|item| item.role == TextRole::Preview)
                .unwrap();
            let marked: Vec<_> = preview
                .marks
                .iter()
                .map(|range| &preview.text[range.clone()])
                .collect();
            assert_eq!(marked, ["ESC", "BEL", "^C"]);
        }
    }

    #[test]
    fn a_preview_token_is_never_split_and_a_bad_range_is_plain_text() {
        // One word longer than the line breaks anywhere, but never in a token.
        let (lines, spans) = wrap_preview("abcdefESC[201~xy", &[Range { start: 6, end: 9 }], 10);
        assert_eq!(lines, ["abcdef", " ESC [201~", "xy"]);
        assert_eq!(
            spans,
            [TokenSpan {
                line: 1,
                bytes: 1..4,
                column: 0,
                columns: 5
            }]
        );
        // A word that fits on a line of its own moves there whole, so the
        // end marker is not cut off from its ESC.
        let (lines, spans) = wrap_preview(
            "printf 'x'ESC[201~; echo injected",
            &[Range { start: 10, end: 13 }],
            20,
        );
        assert_eq!(lines, ["printf ", "'x' ESC [201~; echo ", "injected"]);
        assert_eq!(spans[0].line, 1);
        assert_eq!(&lines[1][spans[0].bytes.clone()], "ESC");
        assert_eq!(spans[0].column, 3);
        // Out of bounds, inside a character, empty, or out of order.
        let (lines, spans) = wrap_preview("é^Cz", &[1..2, 3..3, 2..4, 0..1, 9..12], 80);
        assert_eq!(lines, ["é ^C z"]);
        assert_eq!(spans.len(), 1);
        assert_eq!(&lines[0][spans[0].bytes.clone()], "^C");
        // Spaces are text here: none is trimmed or merged.
        let (lines, _) = wrap_preview("  a  b\n\n c", &[], 4);
        assert_eq!(lines, ["  a ", " b", "", " c"]);
        let (lines, _) = wrap_preview("ab cd  ef", &[], 5);
        assert_eq!(lines, ["ab cd", "  ef"]);
        assert_eq!(lines.concat(), "ab cd  ef");
        // Wide characters wrap by cells.
        assert_eq!(wrap_preview("界界界", &[], 4).0, ["界界", "界"]);
    }

    #[test]
    fn findings_rows_line_up_token_meaning_and_count() {
        let mut dialog = paste(EN);
        let detail = dialog.detail.as_mut().unwrap();
        detail.findings = vec![
            ConfirmDialogFinding {
                token: "ESC[201~".into(),
                meaning: "ends bracketed paste".into(),
                count: "\u{d7}1".into(),
            },
            ConfirmDialogFinding {
                token: "^C".into(),
                meaning: "interrupt (Ctrl+C)".into(),
                count: "\u{d7}12".into(),
            },
            ConfirmDialogFinding {
                token: "ESC".into(),
                meaning: "read as the Esc key or the start of a key sequence".into(),
                count: "\u{d7}3".into(),
            },
        ];
        detail.more = "and 2\u{a0}more kinds".into();
        let wide = ConfirmDialogLayout::new((1280.0, 800.0), (8.0, 16.0), 1.0, &dialog);
        let rows = &wide.detail.as_ref().unwrap().finding_lines;
        assert_eq!(rows.len(), 4);
        assert!(rows[0].starts_with("  ESC[201~  ends bracketed paste "));
        assert!(rows[1].starts_with("  ^C        interrupt (Ctrl+C) "));
        assert!(rows[2].starts_with("  ESC       read as the Esc key"));
        assert_eq!(rows[3], "  and 2\u{a0}more kinds");
        // Counts end in one column, at the right edge of the content.
        let columns = (wide.clip.2 / 8.75).floor() as usize - 1;
        for (row, count) in rows.iter().zip(["\u{d7}1", "\u{d7}12", "\u{d7}3"]) {
            assert!(row.ends_with(count), "{row:?}");
            assert_eq!(display_width(row), columns, "{row:?}");
        }
        let findings = wide
            .items
            .iter()
            .find(|item| item.role == TextRole::Findings)
            .unwrap();
        let marked: Vec<_> = findings
            .marks
            .iter()
            .map(|range| &findings.text[range.clone()])
            .collect();
        assert_eq!(marked, ["ESC[201~", "^C", "ESC"]);
        // A narrow dialog wraps the meaning under itself; the row grows.
        let narrow = ConfirmDialogLayout::new((400.0, 900.0), (8.0, 16.0), 1.0, &dialog);
        let detail = narrow.detail.as_ref().unwrap();
        let long = &detail.finding_lines[2];
        assert!(long.contains('\n'), "{long:?}");
        for line in long.split('\n').skip(1) {
            assert!(
                line.starts_with("            ") && !line.ends_with(' '),
                "{line:?}"
            );
        }
        assert!(detail.finding_rects[2].3 > detail.finding_rects[1].3);
        let mut top = f32::MIN;
        for rect in &detail.finding_rects {
            assert!(rect.1 >= top - 0.01);
            top = rect.1 + rect.3;
        }
    }

    #[test]
    fn an_access_key_marks_the_first_word_that_starts_with_it() {
        assert_eq!(access_key_index("Paste settings…", 'S'), Some(6));
        assert_eq!(access_key_index("Paste without controls", 'w'), Some(6));
        assert_eq!(access_key_index("Paste as one line", 'O'), Some(9));
        assert_eq!(
            access_key_index("Pegar sin caracteres de control", 'S'),
            Some(6)
        );
        assert_eq!(access_key_index("Pegar en una sola línea", 'U'), Some(9));
        // No word starts with it: its first occurrence.
        assert_eq!(access_key_index("Configuración de pegado…", 'G'), Some(5));
        assert_eq!(access_key_index("Cancel", 'c'), Some(0));
        assert_eq!(access_key_index("Paste", 'x'), None);
        assert_eq!(access_key_index("Ñandú ñ", 'ñ'), Some(0));
    }

    #[test]
    fn one_accent_button_marks_the_recommended_choice() {
        let dialog = paste(EN);
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            let accent = theme.palette[4];
            let fills: Vec<_> = (0..4)
                .map(|index| button_colors(&theme, accent, &dialog, index).0)
                .collect();
            // Cancel and the one-line paste share the neutral fill; the
            // sanitised paste alone takes the accent; the raw paste is red.
            assert_eq!(fills[0], fills[2], "{name}");
            assert_eq!(fills[1], accent, "{name}");
            assert_eq!(fills[3], theme.palette[1], "{name}");
            if accent != fills[0] && accent != theme.palette[1] {
                assert_eq!(fills.iter().filter(|fill| **fill == accent).count(), 1);
            }
        }
    }

    #[test]
    fn control_tokens_and_the_link_keep_contrast_in_every_theme() {
        use super::super::color::contrast_ratio;
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            let colors = detail_colors(&theme, theme.palette[4]);
            for surface in [colors.chip_fill, colors.preview_fill, theme.background] {
                assert!(
                    contrast_ratio(colors.warning, surface) + 1e-6 >= 4.5,
                    "{name}: token on {surface:?}"
                );
            }
            assert!(
                contrast_ratio(colors.link, theme.background) + 1e-6 >= 4.5,
                "{name}"
            );
            // The chip is told from the box by its outline, not by its fill.
            assert!(contrast_ratio(colors.warning, colors.preview_fill) + 1e-6 >= 3.0);
        }
    }

    #[test]
    fn shaped_text_places_chips_underlines_and_the_link() {
        let mut fonts = FontSystem::new();
        let mut text = ConfirmText::new();
        let dialog = paste(EN);
        let layout = ConfirmDialogLayout::new((1280.0, 800.0), (8.0, 16.0), 1.0, &dialog);
        text.prepare(&mut fonts, "monospace", &layout, GColor::rgb(200, 0, 0));
        let detail = layout.detail.as_ref().unwrap();
        // One block per role, in paint order; the buttons come last.
        assert_eq!(text.key().len(), 11);
        assert_eq!(text.key()[2], "printf 'x' ESC [201~; echo injected");
        assert_eq!(text.key()[5], "Paste protection: Automatic \u{b7} ");
        assert_eq!(text.key()[6], "Paste settings…");
        assert_eq!(text.key()[7], "Cancel");
        let chips = text.chips(&layout);
        assert_eq!(chips.len(), 1);
        assert!(inside(chips[0], detail.preview_rect), "{:?}", chips[0]);
        assert!(chips[0].2 > 0.0 && chips[0].3 > 0.0);
        let lines = text.underlines(&layout, &dialog);
        // Every button marks its access key inside itself.
        for (index, button) in layout.buttons.iter().enumerate() {
            let marks: Vec<_> = lines
                .iter()
                .filter(|line| line.role == TextRole::Button(index))
                .collect();
            assert_eq!(marks.len(), 1, "button {index}");
            assert!(inside(marks[0].rect, *button), "{:?}", marks[0].rect);
            assert!(marks[0].rect.2 > 0.0 && marks[0].rect.3 > 0.0);
        }
        // The link is underlined from end to end, and its access key has a
        // heavier stroke.
        let link: Vec<_> = lines
            .iter()
            .filter(|line| line.role == TextRole::Link)
            .collect();
        assert_eq!(link.len(), 2);
        assert!(link[0].rect.2 > link[1].rect.2 && link[1].rect.3 > link[0].rect.3);
        assert!(
            link[1].rect.0 > link[0].rect.0,
            "the key is the s of settings"
        );
        let colors = ConfirmTextColors {
            text: GColor::rgb(1, 1, 1),
            link: GColor::rgb(2, 2, 2),
            buttons: (0..4).map(|n| GColor::rgb(10 + n, 0, 0)).collect(),
        };
        let areas = text.areas(&layout, &colors);
        assert_eq!(areas.len(), 11);
        assert_eq!(areas[6].default_color, colors.link);
        assert_eq!(areas[6].left, detail.link_rect.0);
        assert_eq!(areas[10].default_color, colors.buttons[3]);
        // Without access keys (macOS) only the link's own underline is left.
        let mut plain = dialog.clone();
        for button in &mut plain.buttons {
            button.access_key = None;
        }
        plain.detail.as_mut().unwrap().link_access_key = None;
        let lines = text.underlines(&layout, &plain);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].role, TextRole::Link);
        // A dialog without a detail block draws none of the paste blocks.
        let mut close = plain;
        close.detail = None;
        close.buttons.truncate(2);
        let layout = ConfirmDialogLayout::new((1280.0, 800.0), (8.0, 16.0), 1.0, &close);
        text.prepare(&mut fonts, "monospace", &layout, GColor::rgb(200, 0, 0));
        assert_eq!(text.areas(&layout, &colors).len(), 4);
        assert!(text.chips(&layout).is_empty());
        assert!(text.underlines(&layout, &close).is_empty());
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
                            ..Default::default()
                        },
                        super::super::ConfirmDialogButton {
                            label: "Continue".into(),
                            destructive,
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
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
