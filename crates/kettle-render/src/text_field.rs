//! Modal text uses search's shaping, grapheme clusters and selection geometry.
use super::{Attrs, Family, FontSystem, Metrics, Rect4, Shaping, TextBuffer};
use crate::search::{
    SearchBarSegment, SearchControl, cluster_spans, cluster_stops, segment_clusters,
    shape_search_segment, starts_right_to_left, stop_x,
};

#[derive(Clone, Debug, PartialEq)]
pub struct TextFieldOverlay {
    /// A contained picker header keeps the complete hint only when the query
    /// and a caret-sized gap fit beside it.
    pub picker_header: bool,
    /// What a picker's empty query field says it is for, in the secondary
    /// text colour. Paint only: it is never the field's value.
    pub placeholder: String,
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

/// Prefixes fit at most 30% of the field. Picker headers show the whole hint
/// beside a fitting query or drop it; other fields fit hints by segments.
/// An empty picker query's placeholder comes before the hint: the hint gives
/// way when only that lets the placeholder show, and is back once a query
/// replaces it. Measuring and painting share these strings and their clipped
/// slots.
pub fn text_field_labels(field: &TextFieldOverlay, cw: f32) -> (String, String) {
    let budget = if cw > 0.0 {
        (field.rect.2.max(0.0) * 0.3 / cw).floor() as usize
    } else {
        0
    };
    let prefix = super::fit_single_line_label(&field.prefix, budget);
    let hint = if field.picker_header {
        let needed = super::display_width(&prefix)
            + super::display_width(&field.text).max(1)
            + 2
            + super::display_width(&field.hint)
            + 2;
        let hint = format!("{}  ", field.hint);
        // The editor slot's width with and without the hint, as
        // `text_field_rects` lays it out.
        let prefix_w = super::display_width(&prefix) as f32 * cw;
        let hint_w = super::display_width(&hint) as f32 * cw;
        let crowds_placeholder = wants_placeholder(field, cw)
            && !placeholder_fits(&field.placeholder, field.rect.2 - prefix_w - hint_w, cw)
            && placeholder_fits(&field.placeholder, field.rect.2 - prefix_w, cw);
        if needed as f32 * cw <= field.rect.2 && !field.hint.is_empty() && !crowds_placeholder {
            hint
        } else {
            String::new()
        }
    } else {
        super::fit_key_hint(&field.hint, budget)
    };
    (prefix, hint)
}

/// Whether `field` would show a placeholder in a slot wide enough: a picker
/// header's empty query (an IME preedit counts as text) with one to show.
fn wants_placeholder(field: &TextFieldOverlay, cw: f32) -> bool {
    field.picker_header && field.text.is_empty() && !field.placeholder.is_empty() && cw > 0.0
}

/// Whether `placeholder` fits whole in an editor slot `width` wide, less its
/// 2 px inset and a two-column gap before the hint.
fn placeholder_fits(placeholder: &str, width: f32, cw: f32) -> bool {
    let columns = ((width.max(0.0) - 2.0) / cw).floor() - 2.0;
    columns >= super::display_width(placeholder) as f32
}

/// The placeholder a picker header paints in its empty editor slot: the
/// whole string when it fits the slot less a two-column gap before the hint,
/// otherwise nothing, since a cut-off "Type a c…" reads worse than an empty
/// field. The hint gives way to it first (see [`text_field_labels`]). Hidden
/// too while the field holds text (an IME preedit included) and outside
/// picker headers. Paint measures and draws this same string.
pub fn text_field_placeholder(field: &TextFieldOverlay, cw: f32) -> String {
    if !wants_placeholder(field, cw) {
        return String::new();
    }
    let editor = text_field_rects(field, cw)[1];
    if placeholder_fits(&field.placeholder, editor.2, cw) {
        field.placeholder.clone()
    } else {
        String::new()
    }
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

/// Match fixed-height fields to their band for painting and pointer geometry.
pub fn text_field_cell_width(field: &TextFieldOverlay, cw: f32, line_height: f32) -> f32 {
    cw * crate::tab_text_scale(field.rect.3, line_height)
}

pub(crate) fn prepare_text_field(
    fonts: &mut FontSystem,
    buffer: &mut TextBuffer,
    field: &TextFieldOverlay,
    cw: f32,
    metrics: Metrics,
    family: &str,
) -> TextFieldLayout {
    let cw = text_field_cell_width(field, cw, metrics.line_height);
    let scale = crate::tab_text_scale(field.rect.3, metrics.line_height);
    let metrics = Metrics::new(metrics.font_size * scale, metrics.line_height * scale);
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
    let scale = crate::tab_text_scale(height, metrics.line_height);
    let font_size = metrics.font_size * scale;
    buffer.set_metrics(Metrics::new(font_size, height.max(font_size)));
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
                        picker_header: false,
                        placeholder: String::new(),
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
                    picker_header: false,
                    placeholder: String::new(),
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
                picker_header: false,
                placeholder: String::new(),
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
            if crate::display_width(&field.hint) as f32 * cw <= width * 0.3 {
                assert_eq!(hint, field.hint);
            } else if crate::display_width("(Esc cancel)") as f32 * cw <= width * 0.3 {
                assert!(hint.ends_with("Esc cancel)"), "{width}: {hint:?}");
            } else {
                assert!(hint.ends_with('…'), "{width}: {hint:?}");
            }
        }
    }

    #[test]
    fn narrow_key_hints_drop_segments_before_the_way_out() {
        let hint = "(Enter run · Tab/↑↓ select · Esc cancel)";
        assert_eq!(crate::fit_key_hint(hint, 40), hint);
        assert_eq!(crate::fit_key_hint(hint, 39), "(Enter run · Esc cancel)");
        assert_eq!(crate::fit_key_hint(hint, 24), "(Enter run · Esc cancel)");
        assert_eq!(crate::fit_key_hint(hint, 23), "(Esc cancel)");
        assert_eq!(crate::fit_key_hint(hint, 12), "(Esc cancel)");
        assert_eq!(crate::fit_key_hint(hint, 11), "(Esc cance…");
        assert_eq!(crate::fit_key_hint(hint, 0), "");
        // Middle segments go before leading ones.
        let ssh = "(Enter connect · Tab complete · ↑↓ select · Esc cancel)";
        assert_eq!(
            crate::fit_key_hint(ssh, 44),
            "(Enter connect · Tab complete · Esc cancel)"
        );
        assert_eq!(crate::fit_key_hint(ssh, 28), "(Enter connect · Esc cancel)");
        // Spanish keeps its way out too, and other text is a plain label.
        let tr = kettle_i18n::Translator::new(kettle_i18n::Language::Es);
        let es = format!(
            "(Enter {} · Tab/↑↓ {} · Esc {})",
            tr.text(kettle_i18n::Text::PickerHintRun),
            tr.text(kettle_i18n::Text::PickerHintSelect),
            tr.text(kettle_i18n::Text::PickerHintCancel)
        );
        assert_eq!(crate::fit_key_hint(&es, 20), "(Esc cancelar)");
        assert_eq!(crate::fit_key_hint("no hint shape", 6), "no hi…");
    }

    #[test]
    fn translated_prefixes_and_hints_leave_a_clipped_editor_slot() {
        for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
            let tr = kettle_i18n::Translator::new(language);
            for width in [0.0, 1.0, 40.0, 300.0, 800.0] {
                let field = TextFieldOverlay {
                    picker_header: false,
                    placeholder: String::new(),
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

#[cfg(test)]
mod picker_header_tests {
    use super::*;

    /// The command palette's key hint, as the UI builds it, in `tr`.
    fn palette_hint(tr: &kettle_i18n::Translator) -> String {
        use kettle_i18n::Text;
        format!(
            "(Enter {} · Tab/↑↓ {} · Esc {})",
            tr.text(Text::PickerHintRun),
            tr.text(Text::PickerHintSelect),
            tr.text(Text::PickerHintCancel)
        )
    }

    /// A palette header `width` px wide holding `text`, with `placeholder`,
    /// `hint` and the palette's `⌘` prefix.
    fn palette_field(placeholder: &str, hint: &str, text: &str, width: f32) -> TextFieldOverlay {
        TextFieldOverlay {
            picker_header: true,
            placeholder: placeholder.into(),
            text: text.into(),
            cursor_byte: text.len(),
            selection: None,
            horizontal_scroll: 0,
            rect: (10.0, 10.0, width, 30.0),
            prefix: "  \u{2318} ".into(),
            hint: hint.into(),
            accent_palette: 5,
        }
    }

    /// The palette's placeholders (the command palette's and the keyboard
    /// shortcut list's), in either language, show whole in the editor slot
    /// with a two-column gap before the hint, or not at all; a typed query,
    /// an IME preedit or a non-picker field shows none.
    #[test]
    fn picker_placeholder_fits_its_slot_and_never_reaches_the_hint() {
        use kettle_i18n::{Language, Text, Translator};
        for language in [Language::En, Language::Es] {
            let tr = Translator::new(language);
            let hint = palette_hint(&tr);
            for placeholder in [
                Text::PickerPlaceholderCommand,
                Text::PickerPlaceholderShortcuts,
            ]
            .map(|text| tr.text(text))
            {
                for cw in [7.0_f32, 8.0, 9.6, 16.0] {
                    for width in [120.0_f32, 200.0, 300.0, 360.0, 448.0, 560.0, 720.0, 1440.0] {
                        let mut field = palette_field(placeholder, &hint, "", width);
                        let shown = text_field_placeholder(&field, cw);
                        let [_, editor, hint_slot] = text_field_rects(&field, cw);
                        let right = editor.0 + 2.0 + crate::display_width(&shown) as f32 * cw;
                        assert!(
                            right + 2.0 * cw <= hint_slot.0 + 0.001,
                            "{language:?} {cw}/{width}: {shown:?} reaches the hint"
                        );
                        // Whole or not at all, never cut off.
                        assert!(shown.is_empty() || shown == placeholder, "{shown:?}");
                        let room = ((editor.2 - 2.0) / cw).floor() - 2.0;
                        let fits = room >= crate::display_width(placeholder) as f32;
                        assert_eq!(!shown.is_empty(), fits, "{language:?} {cw}/{width}");
                        if width >= 1440.0 && cw <= 9.6 {
                            assert_eq!(shown, placeholder, "{language:?} {cw}");
                        }
                        field.text = "x".into();
                        assert_eq!(text_field_placeholder(&field, cw), "");
                        field.text = String::new();
                        field.picker_header = false;
                        assert_eq!(text_field_placeholder(&field, cw), "");
                    }
                }
            }
        }
    }

    /// An empty picker query's placeholder comes before the key hint. For
    /// both palette placeholders in either language, the placeholder shows
    /// whenever the header has room for it without the hint. The hint gives
    /// way only to a placeholder that would not fit beside it, and is back
    /// once a query is typed. At the widest picker card (720 px) and the
    /// default 14 pt cell (8.4 px), each placeholder shows whole: the
    /// Spanish keyboard-shortcut list's once lost to the hint at every width.
    #[test]
    fn an_empty_picker_header_gives_its_hint_up_before_its_placeholder() {
        use kettle_i18n::{Language, Text, Translator};
        for language in [Language::En, Language::Es] {
            let tr = Translator::new(language);
            let hint = palette_hint(&tr);
            for placeholder in [
                Text::PickerPlaceholderCommand,
                Text::PickerPlaceholderShortcuts,
            ]
            .map(|text| tr.text(text))
            {
                let width_of = |text: &str| crate::display_width(text) as f32;
                for cw in [7.0_f32, 8.0, 8.4, 9.6, 16.0] {
                    for width in [
                        120.0_f32, 200.0, 300.0, 360.0, 448.0, 560.0, 640.0, 720.0, 1440.0,
                    ] {
                        let empty = palette_field(placeholder, &hint, "", width);
                        let typed = palette_field(placeholder, &hint, "x", width);
                        let alone = palette_field(placeholder, "", "", width);
                        let shown = text_field_placeholder(&empty, cw);
                        assert_eq!(
                            shown,
                            text_field_placeholder(&alone, cw),
                            "{language:?} {placeholder:?} {cw}/{width}: the hint crowds it out"
                        );
                        let empty_hint = text_field_labels(&empty, cw).1;
                        let typed_hint = text_field_labels(&typed, cw).1;
                        if empty_hint != typed_hint {
                            // The hint gave way: the placeholder shows, and
                            // would not have fitted beside it.
                            assert!(empty_hint.is_empty(), "{language:?} {cw}/{width}");
                            assert_eq!(shown, placeholder, "{language:?} {cw}/{width}");
                            let beside = text_field_rects(&typed, cw)[1].2;
                            let room = ((beside - 2.0) / cw).floor() - 2.0;
                            assert!(room < width_of(placeholder), "{language:?} {cw}/{width}");
                        }
                    }
                }
                let widest = palette_field(placeholder, &hint, "", 720.0);
                assert_eq!(
                    text_field_placeholder(&widest, 8.4),
                    placeholder,
                    "{language:?}"
                );
            }
        }
    }
    #[test]
    fn picker_hint_is_complete_or_absent_without_overlapping_query() {
        for hint in [
            "Enter run · Tab/↑↓ select · Esc cancel",
            "Enter ejecutar · Tab/↑↓ seleccionar · Esc cancelar",
        ] {
            let mut field = TextFieldOverlay {
                picker_header: true,
                placeholder: String::new(),
                text: "año".into(),
                cursor_byte: 4,
                selection: None,
                horizontal_scroll: 0,
                rect: (16.0, 70.0, 720.0, 30.0),
                prefix: "  ⌘ ".into(),
                hint: hint.into(),
                accent_palette: 5,
            };
            assert_eq!(text_field_labels(&field, 8.0).1.trim_end(), hint);
            let slots = text_field_rects(&field, 8.0);
            assert!(slots[1].2 >= 5.0 * 8.0);
            assert_eq!(slots[1].0 + slots[1].2, slots[2].0);
            field.rect.2 = 328.0;
            assert!(text_field_labels(&field, 8.0).1.is_empty());
            field.rect.2 = 720.0;
            field.text = "query".repeat(30);
            assert!(text_field_labels(&field, 8.0).1.is_empty());
        }
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    #[test]
    fn review_large_title_editor_scales_input_and_hint_shaping() {
        let mut fonts = FontSystem::new();
        for face in kettle_config::font::all() {
            crate::load_bundled_font(&mut fonts, face);
        }
        for scale in [1.0, 2.0] {
            let metrics = Metrics::new(72.0 * scale, 90.0 * scale);
            let mut buffer = TextBuffer::new(&mut fonts, metrics);
            buffer.set_wrap(glyphon::cosmic_text::Wrap::None);
            let field = TextFieldOverlay {
                picker_header: false,
                placeholder: String::new(),
                text: "Rename".into(),
                cursor_byte: 3,
                selection: Some((0, 3)),
                horizontal_scroll: 0,
                rect: (78.0 * scale, 0.0, 640.0 * scale, 28.0 * scale),
                prefix: "Title: ".into(),
                hint: "(Enter apply Esc cancel)".into(),
                accent_palette: 3,
            };
            let layout = prepare_text_field(
                &mut fonts,
                &mut buffer,
                &field,
                43.2 * scale,
                metrics,
                "JetBrains Mono",
            );
            let cw = text_field_cell_width(&field, 43.2 * scale, metrics.line_height);
            assert!((cw - 13.44 * scale).abs() < 0.001);
            let slots = text_field_rects(&field, cw);
            assert_eq!(layout.editor, slots[1]);
            assert!((slots[0].2 - 7.0 * cw).abs() < 0.001);
            assert!(buffer.metrics().font_size <= field.rect.3);
            assert_eq!(buffer.metrics().line_height, field.rect.3);
            assert!(!buffer.layout_runs().next().unwrap().glyphs.is_empty());
            let cursor_x = layout.stops.iter().find(|(b, _)| *b == 3).unwrap().1;
            assert_eq!(cursor_x, layout.caret.0);
            let mut hint = TextBuffer::new(&mut fonts, metrics);
            shape_field_label(
                &mut fonts,
                &mut hint,
                "Esc cancel",
                field.rect.3,
                metrics,
                "JetBrains Mono",
            );
            assert!(hint.metrics().font_size <= field.rect.3);
            assert_eq!(hint.metrics().line_height, field.rect.3);
            assert_eq!(hint.layout_runs().count(), 1);
        }
    }
}
