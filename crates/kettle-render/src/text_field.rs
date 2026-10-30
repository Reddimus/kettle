//! Modal text uses search's shaping, grapheme clusters and selection geometry.
use super::{Attrs, Family, FontSystem, Metrics, Rect4, Shaping, TextBuffer};
use crate::search::{
    SearchBarSegment, SearchControl, cluster_spans, cluster_stops, segment_clusters,
    shape_search_segment, starts_right_to_left, stop_x,
};

#[derive(Clone, Debug, PartialEq)]
pub struct TextFieldOverlay {
    pub text: String,
    pub cursor_byte: usize,
    pub selection: Option<(usize, usize)>,
    pub horizontal_scroll: usize,
    pub rect: Rect4,
    pub prefix: String,
    pub hint: String,
    pub accent_palette: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextFieldLayout {
    pub editor: Rect4,
    pub text_left: f32,
    pub stops: Vec<(usize, f32)>,
    pub caret: Rect4,
    pub selection: Vec<Rect4>,
}

/// The prefix and hint as painted: each fitted, with an ellipsis, to at most
/// 30% of the field. Measuring and painting both use these strings, so a long
/// or translated label is shortened instead of overflowing its slot.
pub fn text_field_labels(field: &TextFieldOverlay, cw: f32) -> (String, String) {
    let budget = if cw > 0.0 {
        (field.rect.2.max(0.0) * 0.3 / cw).floor() as usize
    } else {
        0
    };
    (
        super::fit_single_line_label(&field.prefix, budget),
        super::fit_single_line_label(&field.hint, budget),
    )
}

pub fn text_field_rects(field: &TextFieldOverlay, cw: f32) -> [Rect4; 3] {
    let (x, y, w, h) = field.rect;
    let (prefix, hint) = text_field_labels(field, cw);
    let prefix = super::display_width(&prefix) as f32 * cw;
    let hint = super::display_width(&hint) as f32 * cw;
    [
        (x, y, prefix, h),
        (x + prefix, y, (w - prefix - hint).max(0.0), h),
        (x + w - hint, y, hint, h),
    ]
}

pub(crate) fn prepare_text_field(
    fonts: &mut FontSystem,
    buffer: &mut TextBuffer,
    field: &TextFieldOverlay,
    cw: f32,
    metrics: Metrics,
    family: &str,
) -> TextFieldLayout {
    let editor = text_field_rects(field, cw)[1];
    let segment = SearchBarSegment {
        control: Some(SearchControl::Editor),
        rect: editor,
        left: editor.0,
        text: field.text.clone(),
        rtl: starts_right_to_left(&field.text),
    };
    shape_search_segment(
        fonts,
        buffer,
        &segment,
        Metrics::new(metrics.font_size, editor.3.max(metrics.font_size)),
        Family::Name(family),
        true,
    );
    let clusters = segment_clusters(buffer, &segment);
    let stops = cluster_stops(&clusters);
    layout_text_field(field, editor, &stops, &clusters)
}

fn layout_text_field(
    field: &TextFieldOverlay,
    editor: Rect4,
    stops: &[(usize, f32)],
    clusters: &[super::SearchEditorCluster],
) -> TextFieldLayout {
    let cursor_x = stop_x(stops, field.cursor_byte);
    let width = (editor.2 - 4.0).max(0.0);
    use unicode_segmentation::UnicodeSegmentation as _;
    use unicode_width::UnicodeWidthStr as _;
    let mut column = 0;
    let scroll_byte = field
        .text
        .grapheme_indices(true)
        .find_map(|(byte, g)| {
            if column >= field.horizontal_scroll {
                Some(byte)
            } else {
                column += g.width();
                None
            }
        })
        .unwrap_or(field.text.len());
    // A logical boundary can sit at the right edge of an RTL run. Never
    // displace fitting text, or scroll past the shaped text's overflow.
    let max_scroll = (clusters.iter().map(|c| c.right).fold(0.0_f32, f32::max) - width).max(0.0);
    let desired = stop_x(stops, scroll_byte).clamp(0.0, max_scroll);
    let scroll = desired.clamp((cursor_x - width).max(0.0), cursor_x.max(0.0));
    let text_left = editor.0 + 2.0 - scroll;
    let clamp = |x: f32| x.clamp(editor.0, editor.0 + editor.2);
    let selection = field.selection.map_or_else(Vec::new, |(a, b)| {
        cluster_spans(clusters, a.min(b), a.max(b))
            .into_iter()
            .filter_map(|(a, b)| {
                let (a, b) = (clamp(text_left + a), clamp(text_left + b));
                (b > a).then_some((a, editor.1 + 2.0, b - a, (editor.3 - 4.0).max(0.0)))
            })
            .collect()
    });
    TextFieldLayout {
        editor,
        text_left,
        stops: stops
            .iter()
            .map(|&(byte, x)| (byte, text_left + x))
            .collect(),
        caret: (
            clamp(text_left + cursor_x),
            editor.1 + 2.0,
            1.0_f32.min(editor.2),
            (editor.3 - 4.0).max(0.0),
        ),
        selection,
    }
}

impl super::Renderer {
    /// Shape the exact painter input for pointer and IME use, including preedit.
    pub fn measure_text_field(&mut self, field: &TextFieldOverlay) -> TextFieldLayout {
        let mut buffer = TextBuffer::new(&mut self.font_system, self.metrics);
        let cw = self.overlay_text_cell_width();
        prepare_text_field(
            &mut self.font_system,
            &mut buffer,
            field,
            cw,
            self.metrics,
            &self.font_family,
        )
    }
}

pub(crate) fn shape_field_label(
    fonts: &mut FontSystem,
    buffer: &mut TextBuffer,
    text: &str,
    height: f32,
    metrics: Metrics,
    family: &str,
) {
    buffer.set_metrics(Metrics::new(
        metrics.font_size,
        height.max(metrics.font_size),
    ));
    buffer.set_size(None, Some(height));
    buffer.set_text(
        text,
        &Attrs::new().family(Family::Name(family)),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(fonts, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_segmentation::UnicodeSegmentation as _;

    #[test]
    fn fitting_rtl_text_stays_visible_at_every_caret_and_scroll_boundary() {
        let mut fonts = FontSystem::new();
        for face in kettle_config::font::all() {
            crate::load_bundled_font(&mut fonts, face);
        }
        let metrics = Metrics::new(13.0, 26.0);
        let mut buffer = TextBuffer::new(&mut fonts, metrics);
        buffer.set_wrap(glyphon::cosmic_text::Wrap::None);
        for text in [
            "\u{627}\u{644}\u{633}\u{644}\u{627}\u{645}",
            "\u{5e9}\u{5dc}\u{5d5}\u{5dd} abc",
        ] {
            for cursor in text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .chain([text.len()])
            {
                for horizontal_scroll in [0, 1, 20] {
                    let field = TextFieldOverlay {
                        text: text.into(),
                        cursor_byte: cursor,
                        selection: Some((0, text.len())),
                        horizontal_scroll,
                        rect: (20.0, 30.0, 400.0, 26.0),
                        prefix: String::new(),
                        hint: String::new(),
                        accent_palette: 5,
                    };
                    let layout = prepare_text_field(
                        &mut fonts,
                        &mut buffer,
                        &field,
                        8.0,
                        metrics,
                        "JetBrains Mono",
                    );
                    assert!(
                        layout.stops.iter().all(|&(_, x)| x >= layout.editor.0 + 2.0
                            && x <= layout.editor.0 + layout.editor.2 - 2.0),
                        "{text:?}/{cursor}/{horizontal_scroll}: {:?}",
                        layout.stops
                    );
                    let clusters = segment_clusters(
                        &buffer,
                        &SearchBarSegment {
                            control: Some(SearchControl::Editor),
                            rect: layout.editor,
                            left: layout.editor.0,
                            text: text.into(),
                            rtl: starts_right_to_left(text),
                        },
                    );
                    assert!(
                        clusters
                            .iter()
                            .all(|c| layout.text_left + c.left >= layout.editor.0
                                && layout.text_left + c.right <= layout.editor.0 + layout.editor.2)
                    );
                    let selection_width: f32 = layout.selection.iter().map(|r| r.2).sum();
                    let shaped_width: f32 = cluster_spans(&clusters, 0, text.len())
                        .iter()
                        .map(|(a, b)| b - a)
                        .sum();
                    assert!(shaped_width > 0.0);
                    assert!((selection_width - shaped_width).abs() < 0.01);
                }
            }
        }
    }

    #[test]
    fn modal_paint_hit_testing_and_caret_use_the_same_shaped_stops() {
        let mut fonts = FontSystem::new();
        for face in kettle_config::font::all() {
            crate::load_bundled_font(&mut fonts, face);
        }
        let metrics = Metrics::new(13.0, 26.0);
        let mut buffer = TextBuffer::new(&mut fonts, metrics);
        buffer.set_wrap(glyphon::cosmic_text::Wrap::None);
        for text in [
            "abcdefghijklmnopqrstuvwxyz",
            "a\u{301}\u{1f469}\u{200d}\u{1f4bb}\u{65e5}\u{672c}z",
            "\u{65e5}\u{672c}\u{8a9e}".repeat(20).as_str(),
            "\u{627}\u{644}\u{633}\u{644}\u{627}\u{645}",
            "\u{627}\u{644}\u{633}\u{644}\u{627}\u{645}"
                .repeat(20)
                .as_str(),
        ] {
            for cursor in text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .chain([text.len()])
            {
                let field = TextFieldOverlay {
                    text: text.into(),
                    cursor_byte: cursor,
                    selection: Some((0, cursor)),
                    horizontal_scroll: 0,
                    rect: (20.0, 30.0, 100.0, 26.0),
                    prefix: String::new(),
                    hint: String::new(),
                    accent_palette: 5,
                };
                let layout = prepare_text_field(
                    &mut fonts,
                    &mut buffer,
                    &field,
                    8.0,
                    metrics,
                    "JetBrains Mono",
                );
                let painted_x = layout
                    .stops
                    .iter()
                    .find(|(byte, _)| *byte == cursor)
                    .unwrap()
                    .1;
                assert!((painted_x - layout.caret.0).abs() < 0.01);
                assert!(layout.caret.0 >= layout.editor.0);
                assert!(layout.caret.0 + layout.caret.2 <= layout.editor.0 + layout.editor.2);
                let nearest = layout
                    .stops
                    .iter()
                    .min_by(|a, b| (a.1 - painted_x).abs().total_cmp(&(b.1 - painted_x).abs()))
                    .unwrap();
                // Missing-glyph fonts can give distinct graphemes the same stop.
                assert!((nearest.1 - painted_x).abs() < 0.01);
                for &(x, y, w, h) in &layout.selection {
                    assert!(x >= layout.editor.0 && x + w <= layout.editor.0 + layout.editor.2);
                    assert!(y >= layout.editor.1 && y + h <= layout.editor.1 + layout.editor.3);
                }
            }
        }
    }

    #[test]
    fn long_hints_are_shortened_to_the_slot_they_are_painted_in() {
        let cw = 8.0;
        for width in [120.0, 400.0, 875.0, 2000.0] {
            let field = TextFieldOverlay {
                text: "spilt".into(),
                cursor_byte: 3,
                selection: None,
                horizontal_scroll: 0,
                rect: (0.0, 0.0, width, 26.0),
                prefix: "⌘ ".into(),
                hint: "(Enter run · Tab/↑↓ select · Esc cancel)".into(),
                accent_palette: 5,
            };
            let (_, hint) = text_field_labels(&field, cw);
            let [_, _, slot] = text_field_rects(&field, cw);
            let painted = crate::display_width(&hint) as f32 * cw;
            assert!(painted <= slot.2 + 0.001, "{width}: {hint:?}");
            assert!(slot.2 <= width * 0.3 + 0.001);
            if crate::display_width(&field.hint) as f32 * cw > width * 0.3 {
                assert!(hint.ends_with('…'), "{width}: {hint:?}");
            } else {
                assert_eq!(hint, field.hint);
            }
        }
    }

    #[test]
    fn translated_prefixes_and_hints_leave_a_clipped_editor_slot() {
        for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
            let tr = kettle_i18n::Translator::new(language);
            for width in [0.0, 1.0, 40.0, 300.0, 800.0] {
                let field = TextFieldOverlay {
                    text: String::new(),
                    cursor_byte: 0,
                    selection: None,
                    horizontal_scroll: 0,
                    rect: (0.0, 10.0, width, 26.0),
                    prefix: crate::theme_picker_prefix(&tr),
                    hint: tr.text(kettle_i18n::Text::PickerHintRestore).into(),
                    accent_palette: 5,
                };
                let [prefix, editor, hint] = text_field_rects(&field, 8.0);
                assert_eq!(editor.0, prefix.0 + prefix.2);
                assert!((editor.0 + editor.2 - hint.0).abs() < 0.001);
                assert!((hint.0 + hint.2 - width).abs() < 0.001);
                assert!(editor.2 >= width * 0.39);
            }
        }
    }
}
