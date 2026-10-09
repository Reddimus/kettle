//! Native text metrics from the same shaping stack as final SVG pixels.

use kettle_media::{FailureCode, MAX_MERMAID_BYTES};
#[cfg(test)]
use usvg::fontdb;

use super::fonts::JobFonts;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextStyle<'a> {
    pub family: &'a str,
    pub size: f64,
    pub weight: &'a str,
    pub slant: FontSlant,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum FontSlant {
    Normal,
    Italic,
    Oblique,
}

impl FontSlant {
    pub(crate) fn as_svg(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Italic => "italic",
            Self::Oblique => "oblique",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum TextShape {
    Raw,
    Tspan,
    Formatted,
    FormattedMiddle,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum TextWhitespace {
    Preserve,
    SvgDefault,
}

fn text_element(id: &str, anchor: &str, text: &str, shape: TextShape, rows: &[String]) -> String {
    match shape {
        TextShape::Raw => format!("<text id='{id}' text-anchor='{anchor}'>{text}</text>"),
        TextShape::Tspan => {
            format!("<text id='{id}' text-anchor='{anchor}'><tspan>{text}</tspan></text>")
        }
        TextShape::Formatted | TextShape::FormattedMiddle => {
            let baseline = if shape == TextShape::FormattedMiddle {
                " dominant-baseline='middle'"
            } else {
                ""
            };
            let mut element =
                format!("<text id='{id}' text-anchor='{anchor}' y='-10.1'{baseline}>");
            for (index, row) in rows.iter().enumerate() {
                let y = if index == 0 {
                    "-0.1em".to_owned()
                } else {
                    format!("{:.1}em", 1.0 + (index - 1) as f64 * 1.1)
                };
                element.push_str(&format!("<tspan x='0' y='{y}' dy='1.1em'>{row}</tspan>"));
            }
            element.push_str("</text>");
            element
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Metrics {
    pub advance: f64,
    pub bounds: usvg::Rect,
    #[cfg(test)]
    pub ink: Option<usvg::Rect>,
    #[cfg(test)]
    pub faces: Vec<fontdb::ID>,
}

pub(super) fn escape_xml(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            _ => out.push(c),
        }
    }
}

pub(crate) fn validate(text: &str, style: &TextStyle<'_>) -> Result<u16, FailureCode> {
    let weight = match style.weight {
        "normal" | "400" => Some(400),
        "bold" | "700" => Some(700),
        "100" => Some(100),
        "200" => Some(200),
        "300" => Some(300),
        "500" => Some(500),
        "600" => Some(600),
        "800" => Some(800),
        "900" => Some(900),
        _ => None,
    };
    if text.len() > MAX_MERMAID_BYTES || style.family.len() > 1024 {
        return Err(FailureCode::RenderResource);
    }
    if !style.size.is_finite()
        || !(1.0..=512.0).contains(&style.size)
        || weight.is_none()
        || text.chars().any(|c| {
            (c < ' ' && !matches!(c, '\n' | '\r' | '\t')) || matches!(c, '\u{fffe}' | '\u{ffff}')
        })
    {
        return Err(FailureCode::RenderParse);
    }
    weight.ok_or(FailureCode::RenderParse)
}

#[cfg(test)]
pub(crate) fn measure(
    text: &str,
    style: &TextStyle<'_>,
    fonts: &JobFonts,
) -> Result<Option<Metrics>, FailureCode> {
    measure_shape(text, style, TextShape::Raw, fonts)
}

#[cfg(test)]
pub(crate) fn measure_shape(
    text: &str,
    style: &TextStyle<'_>,
    shape: TextShape,
    fonts: &JobFonts,
) -> Result<Option<Metrics>, FailureCode> {
    measure_shape_with_whitespace(text, style, shape, TextWhitespace::Preserve, fonts)
}

pub(crate) fn measure_shape_with_whitespace(
    text: &str,
    style: &TextStyle<'_>,
    shape: TextShape,
    whitespace: TextWhitespace,
    fonts: &JobFonts,
) -> Result<Option<Metrics>, FailureCode> {
    validate(text, style)?;
    if text.is_empty() {
        return Ok(None);
    }
    let mut family = String::new();
    escape_xml(style.family, &mut family);
    let mut escaped = String::with_capacity(text.len());
    escape_xml(text, &mut escaped);
    // usvg drops text with no outlines. A visible suffix retains native space
    // shaping; subtract its advance below without estimating font metrics.
    let space_probe =
        matches!(shape, TextShape::Raw | TextShape::Tspan) && text.chars().all(char::is_whitespace);
    if space_probe {
        escaped.push('M');
    }
    let font_style = style.slant.as_svg();
    let rows = if matches!(shape, TextShape::Formatted | TextShape::FormattedMiddle) {
        super::text_rows::formatted_rows(text)?
    } else {
        Vec::new()
    };
    let start_element = text_element("start", "start", &escaped, shape, &rows);
    let end_element = text_element("end", "end", &escaped, shape, &rows);
    let mut probes = String::new();
    for (index, row) in rows.iter().enumerate() {
        for anchor in ["start", "end"] {
            probes.push_str(&format!(
                "<text id='advance-{anchor}-{index}' text-anchor='{anchor}'>{row}</text>"
            ));
        }
    }
    let xml_space = match whitespace {
        TextWhitespace::Preserve => "preserve",
        TextWhitespace::SvgDefault => "default",
    };
    let document = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='1' height='1'><g font-family='{family}' font-size='{}' font-weight='{}' font-style='{font_style}' xml:space='{xml_space}'>{start_element}{end_element}{probes}</g></svg>",
        style.size, style.weight,
    );
    if document.len() > 5 * 1024 * 1024 {
        return Err(FailureCode::RenderResource);
    }
    let tree = crate::guarded(FailureCode::RenderParse, || {
        usvg::Tree::from_str(&document, &super::options(fonts))
            .map_err(|_| FailureCode::RenderParse)
    })?;
    let Some(usvg::Node::Text(start)) = tree.node_by_id("start") else {
        return Ok(None);
    };
    let Some(usvg::Node::Text(end)) = tree.node_by_id("end") else {
        return Err(FailureCode::RenderParse);
    };
    let mut bounds = start.bounding_box();
    // usvg positions each end-anchored cluster by minus the sum of advances.
    // Subtracting otherwise identical bounds cancels its nonzero bbox padding,
    // including the one-pixel box used for zero-advance combining marks.
    let mut advance = if rows.is_empty() {
        f64::from(bounds.x()) - f64::from(end.bounding_box().x())
    } else {
        let mut widest: f64 = 0.0;
        for index in 0..rows.len() {
            let Some(usvg::Node::Text(a)) = tree.node_by_id(&format!("advance-start-{index}"))
            else {
                continue;
            };
            let Some(usvg::Node::Text(b)) = tree.node_by_id(&format!("advance-end-{index}")) else {
                return Err(FailureCode::RenderParse);
            };
            let width = f64::from(a.bounding_box().x()) - f64::from(b.bounding_box().x());
            if !width.is_finite() || width < 0.0 {
                return Err(FailureCode::RenderParse);
            }
            widest = widest.max(width);
        }
        widest
    };
    if space_probe {
        let suffix = measure_shape_with_whitespace("M", style, shape, whitespace, fonts)?
            .ok_or(FailureCode::RenderParse)?;
        advance -= suffix.advance;
        if advance == 0.0 {
            return Ok(None);
        }
        bounds = usvg::Rect::from_xywh(bounds.x(), bounds.y(), advance as f32, bounds.height())
            .ok_or(FailureCode::RenderParse)?;
    }
    if !advance.is_finite() || advance < 0.0 {
        return Err(FailureCode::RenderParse);
    }
    #[cfg(test)]
    let faces = {
        let mut faces = Vec::new();
        for span in start.layouted() {
            for glyph in &span.positioned_glyphs {
                if !faces.contains(&glyph.font) {
                    faces.push(glyph.font);
                }
            }
        }
        faces
    };
    #[cfg(test)]
    let ink = (!space_probe && !start.flattened().children().is_empty())
        .then(|| start.flattened().bounding_box());
    Ok(Some(Metrics {
        advance,
        bounds,
        #[cfg(test)]
        ink,
        #[cfg(test)]
        faces,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(fonts: &JobFonts) -> TextStyle<'_> {
        TextStyle {
            family: &fonts.family,
            size: 24.0,
            weight: "normal",
            slant: FontSlant::Normal,
        }
    }

    #[test]
    fn diagram_code_metrics_and_final_text_select_the_same_bundled_monospace_face() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let code_style = TextStyle {
            family: "monospace",
            ..style(&fonts)
        };
        let narrow = measure("iiii", &code_style, &fonts).unwrap().unwrap();
        let wide = measure("WWWW", &code_style, &fonts).unwrap().unwrap();
        assert_eq!(narrow.advance, wide.advance);
        assert_eq!(narrow.faces, wide.faces);
        let prose_narrow = measure("iiii", &style(&fonts), &fonts).unwrap().unwrap();
        let prose_wide = measure("WWWW", &style(&fonts), &fonts).unwrap().unwrap();
        assert!(prose_narrow.advance < prose_wide.advance);
        let tree = usvg::Tree::from_str(
            "<svg xmlns='http://www.w3.org/2000/svg' width='100' height='100'><text id='code' y='30' font-family='monospace' font-size='24'>iiii</text></svg>",
            &super::super::options(&fonts),
        ).unwrap();
        let Some(usvg::Node::Text(code)) = tree.node_by_id("code") else {
            panic!("final code label remains text");
        };
        assert!(!narrow.faces.is_empty());
        assert!(
            code.layouted()
                .iter()
                .any(|span| !span.positioned_glyphs.is_empty())
        );
        for span in code.layouted() {
            for glyph in &span.positioned_glyphs {
                assert!(narrow.faces.contains(&glyph.font));
            }
        }
        assert!(
            !fonts
                .coverage(&tree)
                .unwrap()
                .warnings
                .contains(&kettle_media::Warning::FontFallback)
        );
    }

    #[test]
    fn svg_default_collapses_ascii_edges_and_preserve_keeps_literal_spaces() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let selected = style(&fonts);
        for shape in [TextShape::Raw, TextShape::Tspan] {
            let collapsed = measure_shape_with_whitespace(
                "  A \t B  ",
                &selected,
                shape,
                TextWhitespace::SvgDefault,
                &fonts,
            )
            .unwrap()
            .unwrap();
            let plain = measure_shape_with_whitespace(
                "A B",
                &selected,
                shape,
                TextWhitespace::SvgDefault,
                &fonts,
            )
            .unwrap()
            .unwrap();
            let preserved = measure_shape("  A \t B  ", &selected, shape, &fonts)
                .unwrap()
                .unwrap();
            assert_eq!(collapsed.advance, plain.advance);
            assert_eq!(collapsed.bounds, plain.bounds);
            assert!(preserved.advance > collapsed.advance);
            assert!(
                measure_shape_with_whitespace(
                    " \t \n ",
                    &selected,
                    shape,
                    TextWhitespace::SvgDefault,
                    &fonts,
                )
                .unwrap()
                .is_none()
            );
        }
    }

    #[test]
    fn svg_default_keeps_nbsp_and_collapses_spaces_inside_literal_formatted_words() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let selected = style(&fonts);
        let text = "\u{a0}A\u{a0}";
        let default = measure_shape_with_whitespace(
            text,
            &selected,
            TextShape::Raw,
            TextWhitespace::SvgDefault,
            &fonts,
        )
        .unwrap()
        .unwrap();
        let preserve = measure(text, &selected, &fonts).unwrap().unwrap();
        assert_eq!(default.advance, preserve.advance);
        assert!(default.advance > measure("A", &selected, &fonts).unwrap().unwrap().advance);
        let default = measure_shape_with_whitespace(
            "<tag  attr>",
            &selected,
            TextShape::Formatted,
            TextWhitespace::SvgDefault,
            &fonts,
        )
        .unwrap()
        .unwrap();
        let preserve = measure_shape("<tag  attr>", &selected, TextShape::Formatted, &fonts)
            .unwrap()
            .unwrap();
        assert!(default.advance < preserve.advance);
    }

    #[test]
    fn xml_valid_unicode_controls_are_not_rejected_as_ascii_controls() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let selected = style(&fonts);
        let text = "A\u{85}B\u{7f}C";
        assert_eq!(validate(text, &selected), Ok(400));
        assert!(measure(text, &selected, &fonts).unwrap().is_some());
        for forbidden in [
            "\0", "\u{8}", "\u{b}", "\u{c}", "\u{1f}", "\u{fffe}", "\u{ffff}",
        ] {
            assert_eq!(
                validate(forbidden, &selected),
                Err(FailureCode::RenderParse)
            );
        }
    }

    #[test]
    fn formatted_multiline_advance_is_the_widest_row_and_bounds_cover_all_rows() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let selected = style(&fonts);
        let first = measure_shape("Long AV", &selected, TextShape::Formatted, &fonts)
            .unwrap()
            .unwrap();
        let both = measure_shape("Long AV<br>x\n\n", &selected, TextShape::Formatted, &fonts)
            .unwrap()
            .unwrap();
        assert!((both.advance - first.advance).abs() < 0.001);
        assert!(both.bounds.height() > first.bounds.height() + selected.size as f32);
        assert!((both.bounds.y() - first.bounds.y()).abs() < 0.001);
    }

    #[test]
    fn formatted_non_markdown_words_reset_inherited_bold_and_italic_styles() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let normal = style(&fonts);
        let mut bold = normal.clone();
        bold.weight = "bold";
        bold.slant = FontSlant::Italic;
        let a = measure_shape("AV ffi", &normal, TextShape::Formatted, &fonts)
            .unwrap()
            .unwrap();
        let b = measure_shape("AV ffi", &bold, TextShape::Formatted, &fonts)
            .unwrap()
            .unwrap();
        assert_eq!(a.faces, b.faces);
        assert_eq!(a.bounds, b.bounds);
        assert!((a.advance - b.advance).abs() < 0.001);
        assert_ne!(
            measure("AV ffi", &bold, &fonts).unwrap().unwrap().faces,
            b.faces
        );
    }

    #[test]
    fn formatted_label_absolute_tspan_y_replaces_the_parent_offset() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let selected = style(&fonts);
        let raw = measure_shape("Av", &selected, TextShape::Raw, &fonts)
            .unwrap()
            .unwrap();
        let formatted = measure_shape("Av", &selected, TextShape::Formatted, &fonts)
            .unwrap()
            .unwrap();
        assert!(
            (f64::from(formatted.bounds.y() - raw.bounds.y()) - selected.size).abs() < 0.001,
            "raw={raw:?}; formatted={formatted:?}"
        );
        assert!((formatted.advance - raw.advance).abs() < 0.001);
    }

    #[test]
    fn formatted_child_spans_keep_their_own_default_baseline() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let selected = style(&fonts);
        let ordinary = measure_shape("Av", &selected, TextShape::Formatted, &fonts)
            .unwrap()
            .unwrap();
        let middle = measure_shape("Av", &selected, TextShape::FormattedMiddle, &fonts)
            .unwrap()
            .unwrap();
        let delta = f64::from(middle.bounds.y() - ordinary.bounds.y());
        assert_eq!(delta, 0.0, "ordinary={ordinary:?}; middle={middle:?}");
        assert_eq!(middle.faces, ordinary.faces);
    }

    #[test]
    fn tspan_shape_keeps_native_spaces_and_italic_overhang() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let mut selected = style(&fonts);
        selected.slant = FontSlant::Italic;
        let raw = measure_shape("fj  ", &selected, TextShape::Raw, &fonts)
            .unwrap()
            .unwrap();
        let tspan = measure_shape("fj  ", &selected, TextShape::Tspan, &fonts)
            .unwrap()
            .unwrap();
        assert!((tspan.advance - raw.advance).abs() < 0.001);
        assert_eq!(tspan.bounds, raw.bounds);
        assert_eq!(tspan.ink, raw.ink);
    }

    #[test]
    fn empty_text_has_no_fake_advance_or_box() {
        let fonts = super::super::fonts::for_job(&[]).unwrap();
        assert!(measure("", &style(&fonts), &fonts).unwrap().is_none());
    }

    #[test]
    fn spaces_keep_their_advance_without_creating_ink() {
        let fonts = super::super::fonts::for_job(&[]).unwrap();
        let style = style(&fonts);
        let one = measure("a", &style, &fonts).unwrap().unwrap();
        let spaces = measure("  ", &style, &fonts).unwrap().unwrap();
        let padded = measure("a  ", &style, &fonts).unwrap().unwrap();
        assert!(spaces.ink.is_none());
        assert!(spaces.advance > 0.0);
        assert!((padded.advance - one.advance - spaces.advance).abs() < 0.001);
        assert_eq!(padded.faces, one.faces);
    }

    #[test]
    fn whitespace_probes_preserve_nbsp_and_collapse_only_svg_default_edges() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let selected = style(&fonts);
        for text in [" ", "\t\n", "\u{a0}", " \u{a0} "] {
            let preserved = measure_shape_with_whitespace(
                text,
                &selected,
                TextShape::Tspan,
                TextWhitespace::Preserve,
                &fonts,
            )
            .unwrap()
            .unwrap();
            assert!(preserved.advance > 0.0, "{text:?}");
            assert!(preserved.ink.is_none());
            let with_ink = measure(&format!("{text}M"), &selected, &fonts)
                .unwrap()
                .unwrap();
            let sentinel = measure("M", &selected, &fonts).unwrap().unwrap();
            assert!((with_ink.advance - sentinel.advance - preserved.advance).abs() < 0.001);
        }
        assert!(
            measure_shape_with_whitespace(
                " \t\n",
                &selected,
                TextShape::Tspan,
                TextWhitespace::SvgDefault,
                &fonts,
            )
            .unwrap()
            .is_none()
        );
        let nbsp = measure_shape_with_whitespace(
            "\u{a0}",
            &selected,
            TextShape::Tspan,
            TextWhitespace::SvgDefault,
            &fonts,
        )
        .unwrap()
        .unwrap();
        assert!(nbsp.advance > 0.0);
        assert!(nbsp.ink.is_none());
    }

    #[test]
    fn combining_marks_do_not_inherit_the_nonzero_box_width() {
        let fonts = super::super::fonts::for_job(&[]).unwrap();
        let mark = measure("\u{301}", &style(&fonts), &fonts).unwrap().unwrap();
        assert_eq!(mark.advance, 0.0);
        assert!(mark.bounds.width() >= 1.0);
        let plain = measure("a", &style(&fonts), &fonts).unwrap().unwrap();
        let composed = measure("a\u{301}", &style(&fonts), &fonts)
            .unwrap()
            .unwrap();
        assert!((plain.advance - composed.advance).abs() < 0.001);
    }

    #[test]
    fn text_is_measured_as_literal_characters() {
        let fonts = super::super::fonts::for_job(&[]).unwrap();
        let text = "<&\"'>";
        let measured = measure(text, &style(&fonts), &fonts).unwrap().unwrap();
        let unit = measure("a", &style(&fonts), &fonts).unwrap().unwrap();
        assert!((measured.advance - text.chars().count() as f64 * unit.advance).abs() < 0.001);
        assert!(measured.ink.is_some());
    }

    #[test]
    fn metrics_use_the_actual_per_job_face_and_size() {
        let fonts = super::super::fonts::for_job(&[]).unwrap();
        let small = measure("ffi AV", &style(&fonts), &fonts).unwrap().unwrap();
        let mut large_style = style(&fonts);
        large_style.size *= 2.0;
        let large = measure("ffi AV", &large_style, &fonts).unwrap().unwrap();
        assert!((large.advance - small.advance * 2.0).abs() < 0.001);
        assert!(
            (f64::from(large.bounds.height()) - f64::from(small.bounds.height()) * 2.0).abs()
                < 0.001
        );
        assert!(!small.faces.is_empty());
        assert!(
            small
                .faces
                .iter()
                .all(|id| fonts.database.face(*id).is_some())
        );
    }

    #[test]
    fn oblique_metrics_use_the_final_svg_font_selector() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let mut selected = style(&fonts);
        selected.slant = FontSlant::Oblique;
        let metrics = measure("AV ffi", &selected, &fonts).unwrap().unwrap();
        let face = fonts
            .database
            .query(&fontdb::Query {
                families: &[fontdb::Family::Name(&fonts.family)],
                style: fontdb::Style::Oblique,
                ..fontdb::Query::default()
            })
            .unwrap();
        assert_eq!(metrics.faces, [face]);
        assert!(metrics.advance > 0.0);
    }

    #[test]
    fn proportional_styles_use_their_actual_matching_outlines() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let mut selected = std::collections::HashSet::new();
        for (weight, italic) in [
            ("normal", false),
            ("bold", false),
            ("normal", true),
            ("bold", true),
        ] {
            let styled = TextStyle {
                family: &fonts.family,
                size: 24.0,
                weight,
                slant: if italic {
                    FontSlant::Italic
                } else {
                    FontSlant::Normal
                },
            };
            let metrics = measure("AV ffi", &styled, &fonts).unwrap().unwrap();
            let expected = fonts
                .database
                .query(&fontdb::Query {
                    families: &[fontdb::Family::Name(&fonts.family)],
                    weight: if weight == "bold" {
                        fontdb::Weight::BOLD
                    } else {
                        fontdb::Weight::NORMAL
                    },
                    style: if italic {
                        fontdb::Style::Italic
                    } else {
                        fontdb::Style::Normal
                    },
                    ..fontdb::Query::default()
                })
                .unwrap();
            assert_eq!(metrics.faces, [expected]);
            assert!(selected.insert(expected));
            assert!(metrics.ink.is_some());
        }
        assert_eq!(selected.len(), 4);
    }

    #[test]
    fn proportional_kerning_is_retained_by_the_advance_probe() {
        let fonts = super::super::fonts::for_mermaid_job(&[]).unwrap();
        let style = style(&fonts);
        let pair = measure("AV", &style, &fonts).unwrap().unwrap();
        let a = measure("A", &style, &fonts).unwrap().unwrap();
        let v = measure("V", &style, &fonts).unwrap().unwrap();
        assert!(pair.advance < a.advance + v.advance - 0.1);
        let ffi = measure("ffi", &style, &fonts).unwrap().unwrap();
        let padded = measure("ffi ", &style, &fonts).unwrap().unwrap();
        let space = measure(" ", &style, &fonts).unwrap().unwrap();
        assert!((padded.advance - ffi.advance - space.advance).abs() < 0.001);
    }

    #[test]
    fn nonfinite_styles_and_excessive_source_fail_before_shaping() {
        let fonts = super::super::fonts::for_job(&[]).unwrap();
        for size in [f64::NAN, f64::INFINITY, -1.0, 0.0, 513.0] {
            let mut invalid = style(&fonts);
            invalid.size = size;
            assert_eq!(
                measure("a", &invalid, &fonts).unwrap_err(),
                FailureCode::RenderParse
            );
        }
        let too_long = "x".repeat(MAX_MERMAID_BYTES + 1);
        assert_eq!(
            measure(&too_long, &style(&fonts), &fonts).unwrap_err(),
            FailureCode::RenderResource
        );
    }
}
