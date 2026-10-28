//! Default labels retain their ink and gain contrast over variable fills.

use std::borrow::Cow;
use std::ops::Range;

use kettle_media::FailureCode;
use merman::svg::{SvgPostprocessContext, SvgPostprocessor};
use merman::{OperationControl, SemanticArtifact};
use merman_core::theme_color::ThemeColor;
use merman_render::{Error, Result};

pub(super) struct DefaultLabels {
    style: String,
    control: OperationControl,
    treemap: bool,
}

impl DefaultLabels {
    pub(super) fn for_diagram(
        semantic: &SemanticArtifact,
        foreground: &str,
        control: OperationControl,
    ) -> std::result::Result<Option<Self>, FailureCode> {
        let treemap = semantic.diagram_type() == "treemap";
        if !treemap && semantic.diagram_type() != "journey" {
            return Ok(None);
        }
        let config = &semantic.metadata().config;
        if [
            "theme",
            "themeCSS",
            "journey.sectionFills",
            "journey.actorColours",
            "journey.titleColor",
        ]
        .iter()
        .any(|key| {
            key.split('.')
                .try_fold(config.as_value(), |value, part| value.get(part))
                .is_some()
        }) {
            return Ok(None);
        }
        if treemap {
            let model = crate::guarded(FailureCode::RenderParse, || {
                semantic.compatibility_json().map_err(super::failure)
            })?;
            let mut pending = vec![model.as_value()];
            let mut visited = 0usize;
            while let Some(value) = pending.pop() {
                if visited.is_multiple_of(256) {
                    control
                        .checkpoint()
                        .map_err(|_| FailureCode::RenderTimeout)?;
                }
                visited += 1;
                match value {
                    serde_json::Value::Object(fields) => {
                        for (key, value) in fields {
                            if matches!(key.as_str(), "styles" | "textStyles" | "cssCompiledStyles")
                                && value.as_array().is_some_and(|styles| !styles.is_empty())
                            {
                                return Ok(None);
                            }
                            pending.push(value);
                        }
                    }
                    serde_json::Value::Array(values) => pending.extend(values),
                    _ => {}
                }
            }
        }
        if config
            .as_value()
            .get("themeVariables")
            .is_some_and(|variables| {
                variables.as_object().is_none_or(|fields| {
                    fields
                        .keys()
                        .any(|key| !matches!(key.as_str(), "fontFamily" | "fontSize"))
                })
            })
        {
            return Ok(None);
        }
        let rgb = ThemeColor::parse(foreground)
            .map_err(|_| FailureCode::RenderParse)?
            .rgba_channels();
        let linear = |channel: f64| {
            let channel = channel / 255.0;
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };
        let light =
            0.2126 * linear(rgb.red) + 0.7152 * linear(rgb.green) + 0.0722 * linear(rgb.blue);
        let halo = if (light + 0.05) / 0.05 >= 1.05 / (light + 0.05) {
            "#000000"
        } else {
            "#ffffff"
        };
        Ok(Some(Self {
            style: format!(
                "fill:{foreground};stroke:{halo};stroke-width:2;stroke-linejoin:round;paint-order:stroke fill"
            ),
            control,
            treemap,
        }))
    }
}

fn attribute(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '"' => escaped.push_str("&quot;"),
            '<' => escaped.push_str("&lt;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

impl SvgPostprocessor for DefaultLabels {
    fn name(&self) -> &'static str {
        "default-label-contrast"
    }

    fn process<'a>(
        &self,
        svg: Cow<'a, str>,
        _: &SvgPostprocessContext<'_>,
    ) -> Result<Cow<'a, str>> {
        self.control.checkpoint()?;
        let document = roxmltree::Document::parse(&svg).map_err(|error| Error::SvgPostprocess {
            pass: self.name().into(),
            message: error.to_string(),
        })?;
        let mut edits = Vec::<(Range<usize>, String)>::new();
        for (index, node) in document.descendants().enumerate() {
            if index.is_multiple_of(256) {
                self.control.checkpoint()?;
            }
            if !node.is_element() || node.tag_name().name() != "text" {
                continue;
            }
            if !node.attribute("class").is_some_and(|classes| {
                classes.split_whitespace().any(|class| {
                    if self.treemap {
                        matches!(
                            class,
                            "treemapLabel"
                                | "treemapValue"
                                | "treemapSectionLabel"
                                | "treemapSectionValue"
                        )
                    } else {
                        matches!(class, "task" | "journey-section")
                    }
                })
            }) {
                continue;
            }
            let style = node.attributes().find(|value| value.name() == "style");
            let (range, replacement) = if let Some(style) = style {
                (
                    style.range(),
                    format!("style=\"{};{}\"", attribute(style.value()), self.style),
                )
            } else {
                let start = node.range().start + 1;
                let end = svg[start..]
                    .find([' ', '\t', '\r', '\n', '/', '>'])
                    .ok_or_else(|| Error::SvgPostprocess {
                        pass: self.name().into(),
                        message: "missing label tag end".into(),
                    })?
                    + start;
                (end..end, format!(" style=\"{}\"", self.style))
            };
            edits.push((range, replacement));
        }
        self.control.checkpoint()?;
        if edits.is_empty() {
            return Ok(svg);
        }
        let mut result = String::with_capacity(svg.len());
        let mut cursor = 0;
        for (range, replacement) in edits {
            self.control.checkpoint()?;
            result.push_str(&svg[cursor..range.start]);
            result.push_str(&replacement);
            cursor = range.end;
        }
        result.push_str(&svg[cursor..]);
        self.control.checkpoint()?;
        Ok(Cow::Owned(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use merman::svg::SvgPipeline;
    use merman::{RenderOutput, RenderTarget, Renderer, SvgRequest};

    fn svg(source: &str, improve: bool) -> String {
        let control = OperationControl::new();
        let semantic = Renderer::new()
            .prepare_semantic(source, control.clone())
            .unwrap()
            .unwrap();
        let mut pipeline = SvgPipeline::resvg_safe();
        if improve
            && let Some(labels) = DefaultLabels::for_diagram(&semantic, "#f0f2f4", control).unwrap()
        {
            pipeline.push_postprocessor(labels);
        }
        let RenderOutput::Svg(Some(output)) = semantic
            .render(RenderTarget::Svg(SvgRequest {
                pipeline: Some(pipeline),
                ..SvgRequest::default()
            }))
            .unwrap()
        else {
            panic!("missing SVG");
        };
        output.into_parts().0
    }

    #[test]
    fn authored_paint_and_classes_keep_the_original_output() {
        let journey = "journey\n section Work\n  Build: 5: Me\n";
        let treemap = "treemap-beta\n\"Main\":::important\n  \"A\": 20\nclassDef important fill:#f96,color:#123456;\n";
        for body in [journey, "treemap-beta\n\"Main\"\n  \"A\": 20\n"] {
            for config in [
                "theme: dark",
                "themeVariables: {textColor: '#123456'}",
                "themeCSS: 'text {fill: #123456;}'",
                "journey: {sectionFills: ['#f96']}",
                "journey: {actorColours: ['#123456']}",
                "journey: {titleColor: '#123456'}",
            ] {
                let source = format!("---\nconfig:\n  {config}\n---\n{body}");
                assert_eq!(svg(&source, false), svg(&source, true), "{config}");
            }
        }
        assert_eq!(svg(treemap, false), svg(treemap, true));
    }

    #[test]
    fn default_labels_change_only_paint_and_keep_typography_and_geometry() {
        for body in [
            "journey\n title Release\n section Work\n  Build: 5: Me\n",
            "treemap-beta\n\"styles\"\n  \"textStyles\": 20\n",
        ] {
            let source = format!("---\nconfig:\n  fontFamily: monospace\n---\n{body}");
            let before = svg(&source, false);
            let after = svg(&source, true);
            assert_ne!(before, after);
            let before = roxmltree::Document::parse(&before).unwrap();
            let after = roxmltree::Document::parse(&after).unwrap();
            let mut changed = 0;
            for (left, right) in before.descendants().zip(after.descendants()) {
                assert_eq!(left.node_type(), right.node_type());
                assert_eq!(left.tag_name(), right.tag_name());
                assert_eq!(left.text(), right.text());
                let attrs = |node: roxmltree::Node<'_, '_>| {
                    node.attributes()
                        .filter(|attr| attr.name() != "style")
                        .map(|attr| (attr.name().to_owned(), attr.value().to_owned()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(attrs(left), attrs(right));
                if left.attribute("style") != right.attribute("style") {
                    changed += 1;
                    assert_eq!(left.tag_name().name(), "text");
                    assert!(
                        right
                            .attribute("style")
                            .unwrap()
                            .contains("paint-order:stroke fill")
                    );
                    let prior = left.attribute("style").unwrap_or_default();
                    assert!(right.attribute("style").unwrap().starts_with(prior));
                }
            }
            assert!(changed > 0);
            assert_eq!(before.descendants().count(), after.descendants().count());
        }
    }
}
