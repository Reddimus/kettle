//! Operation-aware text facts from the worker's final SVG font database.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use kettle_media::{FailureCode, MAX_MERMAID_BYTES};
use merman::svg::{
    DeterministicTextMeasurer, HostMeasurementResult, HostTextMeasurement,
    HostTextMeasurementError, HostTextMeasurementRequest, HostTextMeasurer, MeasurementProfileId,
    TextMeasurementOperation, TextMeasurementPhase, TextMeasurementPolicy,
    TextMeasurementProfileIdentity, TextMetrics, TextStyle, WrapMode,
    validate_host_text_measurement,
};
use merman::{OperationControl, OperationPhase};

use super::fonts::JobFonts;
use super::text_cache::NativeMeasurements;
use super::text_metrics::{
    FontSlant, Metrics, TextShape, TextStyle as NativeStyle, TextWhitespace,
};
use super::text_rows::MAX_ROWS;

#[derive(Clone)]
pub(crate) struct NativeTextHost {
    fonts: Arc<JobFonts>,
    native: Arc<NativeMeasurements>,
    control: OperationControl,
    first_failure: Arc<Mutex<Option<FailureCode>>>,
}

impl NativeTextHost {
    pub(crate) fn new(fonts: Arc<JobFonts>, control: OperationControl) -> Self {
        Self {
            native: Arc::new(NativeMeasurements::new(fonts.clone())),
            fonts,
            control,
            first_failure: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn policy(&self) -> Result<TextMeasurementPolicy, FailureCode> {
        let name =
            MeasurementProfileId::new("kettle-native-svg").map_err(|_| FailureCode::RenderParse)?;
        let identity = TextMeasurementProfileIdentity::new(name, "usvg-0.48.1-fira-4.301-v1")
            .map_err(|_| FailureCode::RenderParse)?;
        Ok(TextMeasurementPolicy::host_display(
            identity,
            Arc::new(self.clone()),
            TextMeasurementPhase::ALL,
        ))
    }

    /// Check this after layout and before accepting generated SVG. Host errors
    /// cancel the shared operation so deterministic fallback cannot be accepted.
    pub(crate) fn finish(&self) -> Result<(), FailureCode> {
        self.checkpoint()
    }

    pub(crate) fn native_failure(&self) -> Result<Option<FailureCode>, FailureCode> {
        self.first_failure
            .lock()
            .map(|value| *value)
            .map_err(|_| FailureCode::RenderResource)
    }

    fn checkpoint(&self) -> Result<(), FailureCode> {
        if let Some(failure) = *self
            .first_failure
            .lock()
            .map_err(|_| FailureCode::RenderResource)?
        {
            return Err(failure);
        }
        self.control
            .checkpoint_at(OperationPhase::Layout)
            .map_err(|_| FailureCode::RenderTimeout)
    }

    fn remember_failure(&self, failure: FailureCode) -> FailureCode {
        let first = match self.first_failure.lock() {
            Ok(mut first) => *first.get_or_insert(failure),
            Err(_) => FailureCode::RenderResource,
        };
        // Clone retains the SAME operation state, not a fresh child or deadline.
        // A controlled Merman route checks it before entering its fallback.
        self.control.cancel();
        first
    }

    fn style<'a>(&'a self, style: &'a TextStyle) -> Result<NativeStyle<'a>, FailureCode> {
        let weight = match style.font_weight.as_deref().map(str::trim) {
            None | Some("") | Some("normal") => "normal",
            Some("bolder") => "700",
            Some("lighter") => "200",
            Some(value) => value,
        };
        let slant = match style.font_style.as_deref().map(str::trim) {
            None | Some("") | Some("normal") => FontSlant::Normal,
            Some("italic") => FontSlant::Italic,
            Some("oblique") => FontSlant::Oblique,
            Some(_) => return Err(FailureCode::RenderParse),
        };
        Ok(NativeStyle {
            family: style.font_family.as_deref().unwrap_or(&self.fonts.family),
            size: style.font_size,
            weight,
            slant,
        })
    }

    fn facts(
        &self,
        text: &str,
        style: &TextStyle,
        shape: TextShape,
        whitespace: TextWhitespace,
    ) -> Result<Arc<Option<Metrics>>, FailureCode> {
        self.native.measure_shape_with_whitespace(
            text,
            &self.style(style)?,
            shape,
            whitespace,
            || self.checkpoint(),
        )
    }

    fn advance(
        &self,
        text: &str,
        style: &TextStyle,
        whitespace: TextWhitespace,
    ) -> Result<f64, FailureCode> {
        let facts = self.facts(text, style, TextShape::Tspan, whitespace)?;
        Ok(facts.as_ref().as_ref().map_or(0.0, |value| value.advance))
    }

    fn wrapped(
        &self,
        request: &HostTextMeasurementRequest<'_>,
    ) -> Result<(TextMetrics, Option<f64>), FailureCode> {
        let lines = DeterministicTextMeasurer::normalized_text_lines(request.text);
        if lines.len() > MAX_ROWS {
            return Err(FailureCode::RenderResource);
        }
        if request.wrap_mode == WrapMode::HtmlLike {
            let mut raw_width: f64 = 0.0;
            for line in &lines {
                raw_width =
                    raw_width.max(self.advance(line, request.style, TextWhitespace::Preserve)?);
            }
            let limit = request
                .max_width
                .filter(|width| width.is_finite() && *width > 0.0);
            let break_spaces = limit.is_some_and(|width| raw_width > width);
            let mut width: f64 = 0.0;
            let mut count = 0;
            for line in lines {
                self.checkpoint()?;
                let rows = super::text_wrap::html_line(
                    &line,
                    limit,
                    break_spaces,
                    |candidate| self.advance(candidate, request.style, TextWhitespace::Preserve),
                    || self.checkpoint(),
                )?;
                for row in rows {
                    if count == MAX_ROWS {
                        return Err(FailureCode::RenderResource);
                    }
                    width =
                        width.max(self.advance(&row, request.style, TextWhitespace::Preserve)?);
                    count += 1;
                }
            }
            if break_spaces && let Some(limit) = limit {
                width = width.max(limit);
            }
            self.checkpoint()?;
            // HtmlLike explicitly uses CSS line-height: 1.5. Font bbox height
            // is an SVG primitive and does not replace this CSS line box.
            return Ok((
                TextMetrics {
                    width,
                    height: count as f64 * request.style.font_size * 1.5,
                    line_count: count.max(1),
                },
                Some(raw_width),
            ));
        }
        let mut width: f64 = 0.0;
        let mut top = f64::INFINITY;
        let mut bottom = f64::NEG_INFINITY;
        let mut count = 0;
        let shape = match request.wrap_mode {
            WrapMode::SvgLike => TextShape::Formatted,
            WrapMode::SvgLikeSingleRun => TextShape::Raw,
            WrapMode::HtmlLike => unreachable!("HTML returned above"),
        };
        for line in lines {
            self.checkpoint()?;
            let rows = super::text_wrap::lines(
                &line,
                request.max_width,
                |candidate| {
                    let facts =
                        self.facts(candidate, request.style, shape, TextWhitespace::SvgDefault)?;
                    Ok(facts.as_ref().as_ref().map_or(0.0, |value| value.advance))
                },
                || self.checkpoint(),
            )?;
            for row in rows {
                if count == MAX_ROWS {
                    return Err(FailureCode::RenderResource);
                }
                let facts = self.facts(&row, request.style, shape, TextWhitespace::SvgDefault)?;
                if let Some(facts) = facts.as_ref() {
                    width = width.max(facts.advance);
                    let offset = count as f64 * request.style.font_size * 1.1;
                    top = top.min(f64::from(facts.bounds.y()) + offset);
                    bottom = bottom.max(f64::from(facts.bounds.bottom()) + offset);
                }
                count += 1;
            }
        }
        self.checkpoint()?;
        Ok((
            TextMetrics {
                width,
                height: if top.is_finite() { bottom - top } else { 0.0 },
                line_count: count.max(1),
            },
            None,
        ))
    }

    fn measure_request(
        &self,
        request: &HostTextMeasurementRequest<'_>,
    ) -> Result<HostTextMeasurement, FailureCode> {
        self.checkpoint()?;
        if request.text.len() > MAX_MERMAID_BYTES {
            return Err(FailureCode::RenderResource);
        }
        if request.operation != TextMeasurementOperation::CanvasMeasureTextWidth {
            super::text_metrics::validate(request.text, &self.style(request.style)?)?;
        }
        use TextMeasurementOperation as Op;
        use TextShape as Shape;
        use TextWhitespace as Space;
        match request.operation {
            Op::Measure | Op::Wrapped => {
                let (metrics, _) = self.wrapped(request)?;
                Ok(HostTextMeasurement::Metrics(metrics))
            }
            Op::WrappedWithRawWidth => {
                let (metrics, raw_width) = self.wrapped(request)?;
                Ok(HostTextMeasurement::WrappedWithRawWidth { metrics, raw_width })
            }
            Op::ComputedLength => Ok(HostTextMeasurement::Length(self.advance(
                request.text,
                request.style,
                Space::SvgDefault,
            )?)),
            Op::CanvasMeasureTextWidth => {
                // Canvas text preparation replaces each ASCII whitespace scalar
                // with SPACE without collapsing runs or replacing NBSP.
                let text = if request.text.bytes().any(|b| matches!(b, 9 | 10 | 12 | 13)) {
                    Cow::Owned(
                        request
                            .text
                            .chars()
                            .map(|c| {
                                if matches!(c, '\t' | '\n' | '\u{c}' | '\r') {
                                    ' '
                                } else {
                                    c
                                }
                            })
                            .collect::<String>(),
                    )
                } else {
                    Cow::Borrowed(request.text)
                };
                Ok(HostTextMeasurement::Length(self.advance(
                    &text,
                    request.style,
                    Space::Preserve,
                )?))
            }
            operation => {
                let shape = match operation {
                    Op::BBoxX | Op::BBoxXWithAsciiOverhang | Op::CreateTextBBoxYOffset => {
                        Shape::Formatted
                    }
                    Op::CreateTextMiddleBBoxYOffset => Shape::FormattedMiddle,
                    Op::SimpleBBoxWidth
                    | Op::TspanBBoxWidth
                    | Op::TspanBBoxHeight
                    | Op::WrapProbeBBoxWidth
                    | Op::SimpleBBoxHeight
                    | Op::MermaidCalculateTextDimensions => Shape::Tspan,
                    Op::TitleBBoxX
                    | Op::RawBBoxWidth
                    | Op::BoundingClientRectWidth
                    | Op::RawBBoxHeight => Shape::Raw,
                    _ => return Err(FailureCode::RenderParse),
                };
                let facts = self.facts(request.text, request.style, shape, Space::SvgDefault)?;
                let width = facts
                    .as_ref()
                    .as_ref()
                    .map_or(0.0, |v| f64::from(v.bounds.width()));
                let height = facts
                    .as_ref()
                    .as_ref()
                    .map_or(0.0, |v| f64::from(v.bounds.height()));
                let y = facts
                    .as_ref()
                    .as_ref()
                    .map_or(0.0, |v| f64::from(v.bounds.y()));
                match operation {
                    Op::BBoxX | Op::BBoxXWithAsciiOverhang | Op::TitleBBoxX => {
                        let (left, right) = facts.as_ref().as_ref().map_or((0.0, 0.0), |v| {
                            let centered_x = f64::from(v.bounds.x()) - v.advance / 2.0;
                            ((-centered_x).max(0.0), (centered_x + width).max(0.0))
                        });
                        Ok(HostTextMeasurement::HorizontalExtents { left, right })
                    }
                    Op::TspanBBoxHeight | Op::SimpleBBoxHeight | Op::RawBBoxHeight => {
                        Ok(HostTextMeasurement::Length(height))
                    }
                    Op::CreateTextBBoxYOffset | Op::CreateTextMiddleBBoxYOffset => {
                        Ok(HostTextMeasurement::Length(y))
                    }
                    Op::MermaidCalculateTextDimensions => {
                        Ok(HostTextMeasurement::Metrics(TextMetrics {
                            width,
                            height,
                            line_count: 1,
                        }))
                    }
                    _ => Ok(HostTextMeasurement::Length(width)),
                }
            }
        }
    }
}

impl HostTextMeasurer for NativeTextHost {
    fn measure(&self, request: HostTextMeasurementRequest<'_>) -> HostMeasurementResult {
        let result = self.measure_request(&request).and_then(|value| {
            validate_host_text_measurement(&request, &value)
                .map_err(|_| FailureCode::RenderParse)?;
            self.checkpoint()?;
            Ok(value)
        });
        match result {
            Ok(value) => Ok(Some(value)),
            Err(failure) => {
                let first = self.remember_failure(failure);
                Err(HostTextMeasurementError::new(
                    first.reason().unwrap_or(first.code()),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> NativeTextHost {
        NativeTextHost::new(
            Arc::new(super::super::fonts::for_mermaid_job(&[]).unwrap()),
            OperationControl::new(),
        )
    }

    fn request<'a>(
        operation: TextMeasurementOperation,
        text: &'a str,
        style: &'a TextStyle,
    ) -> HostTextMeasurementRequest<'a> {
        HostTextMeasurementRequest {
            operation,
            phase: TextMeasurementPhase::Layout,
            text,
            style,
            max_width: None,
            wrap_mode: WrapMode::SvgLike,
        }
    }

    #[test]
    fn every_operation_returns_its_valid_kind_for_empty_and_native_text() {
        let host = fixture();
        let style = TextStyle::default();
        for text in ["", "AV ffi", "A & <B>", "\u{301}"] {
            for operation in TextMeasurementOperation::ALL {
                let request = request(operation, text, &style);
                let value = host.measure(request).unwrap().unwrap();
                validate_host_text_measurement(&request, &value).unwrap();
            }
        }
        host.finish().unwrap();
    }

    #[test]
    fn computed_length_and_raw_bbox_are_independent_native_facts() {
        let host = fixture();
        let style = TextStyle {
            font_size: 24.0,
            ..TextStyle::default()
        };
        let text = "AV ffi";
        let actual = host
            .facts(text, &style, TextShape::Raw, TextWhitespace::SvgDefault)
            .unwrap();
        let actual = actual.as_ref().as_ref().unwrap();
        let HostTextMeasurement::Length(advance) = host
            .measure(request(
                TextMeasurementOperation::ComputedLength,
                text,
                &style,
            ))
            .unwrap()
            .unwrap()
        else {
            panic!("length result");
        };
        let HostTextMeasurement::Length(width) = host
            .measure(request(
                TextMeasurementOperation::RawBBoxWidth,
                text,
                &style,
            ))
            .unwrap()
            .unwrap()
        else {
            panic!("length result");
        };
        assert_eq!(advance, actual.advance);
        assert_eq!(width, f64::from(actual.bounds.width()));
        assert!(!actual.faces.is_empty());
    }

    #[test]
    fn native_styles_preserve_exact_slants_and_usvg_relative_weight() {
        let host = NativeTextHost::new(
            Arc::new(super::super::fonts::for_mermaid_job(&[]).unwrap()),
            OperationControl::new(),
        );
        let mut style = TextStyle::default();
        for (name, slant) in [
            ("normal", FontSlant::Normal),
            ("italic", FontSlant::Italic),
            ("oblique", FontSlant::Oblique),
        ] {
            style.font_style = Some(name.into());
            assert_eq!(host.style(&style).unwrap().slant, slant);
        }
        for invalid in ["italicjunk", "obliquejunk", "oblique 20deg"] {
            style.font_style = Some(invalid.into());
            assert!(matches!(host.style(&style), Err(FailureCode::RenderParse)));
        }
        style.font_style = Some("normal".into());
        style.font_weight = Some("lighter".into());
        assert_eq!(host.style(&style).unwrap().weight, "200");
        style.font_weight = Some("bolder".into());
        assert_eq!(host.style(&style).unwrap().weight, "700");
    }

    #[test]
    fn expired_original_deadline_is_observed_even_for_empty_text() {
        let control = OperationControl::new().with_deadline(std::time::Duration::ZERO);
        let host = NativeTextHost::new(
            Arc::new(super::super::fonts::for_mermaid_job(&[]).unwrap()),
            control.clone(),
        );
        assert!(
            host.measure(request(
                TextMeasurementOperation::Measure,
                "",
                &TextStyle::default()
            ))
            .is_err()
        );
        assert_eq!(host.finish(), Err(FailureCode::RenderTimeout));
        assert!(control.is_cancelled());
    }

    #[test]
    fn original_native_failure_wins_over_its_unwind_cancellation() {
        let host = fixture();
        let invalid = TextStyle {
            font_size: f64::NAN,
            ..TextStyle::default()
        };
        let first = host
            .measure(request(
                TextMeasurementOperation::ComputedLength,
                "x",
                &invalid,
            ))
            .unwrap_err();
        assert_eq!(first.message(), FailureCode::RenderParse.reason().unwrap());
        let subsequent = host
            .measure(request(
                TextMeasurementOperation::Measure,
                "ok",
                &TextStyle::default(),
            ))
            .unwrap_err();
        assert_eq!(first, subsequent);
        assert_eq!(host.finish(), Err(FailureCode::RenderParse));
        assert!(host.control.is_cancelled());
    }

    #[test]
    fn cancellation_is_observed_before_a_repeated_cached_label() {
        let host = fixture();
        let style = TextStyle::default();
        let r = request(TextMeasurementOperation::ComputedLength, "cached", &style);
        host.measure(r).unwrap();
        host.control.cancel();
        assert!(host.measure(r).is_err());
        assert_eq!(host.finish(), Err(FailureCode::RenderTimeout));
    }

    #[test]
    fn native_svg_wrapping_keeps_clusters_and_uses_font_sensitive_widths() {
        let host = fixture();
        let style = TextStyle {
            font_size: 24.0,
            ..TextStyle::default()
        };
        let mut r = request(TextMeasurementOperation::Wrapped, "WWWW WWWW", &style);
        r.max_width = Some(60.0);
        let HostTextMeasurement::Metrics(wide) = host.measure(r).unwrap().unwrap() else {
            panic!("metrics");
        };
        r.text = "iiii iiii";
        let HostTextMeasurement::Metrics(narrow) = host.measure(r).unwrap().unwrap() else {
            panic!("metrics");
        };
        assert!(wide.line_count > narrow.line_count);
        r.text = "a\u{301}a\u{301}";
        r.max_width = Some(0.1);
        let HostTextMeasurement::Metrics(clusters) = host.measure(r).unwrap().unwrap() else {
            panic!("metrics");
        };
        assert_eq!(clusters.line_count, 2);
    }

    #[test]
    fn formatted_middle_offsets_keep_native_child_baseline_semantics() {
        let host = fixture();
        let style = TextStyle::default();
        let ordinary = host
            .measure(request(
                TextMeasurementOperation::CreateTextBBoxYOffset,
                "Ag",
                &style,
            ))
            .unwrap()
            .unwrap();
        let middle = host
            .measure(request(
                TextMeasurementOperation::CreateTextMiddleBBoxYOffset,
                "Ag",
                &style,
            ))
            .unwrap()
            .unwrap();
        let (HostTextMeasurement::Length(a), HostTextMeasurement::Length(b)) = (ordinary, middle)
        else {
            panic!("offsets");
        };
        // The child spans keep their default baseline in the pinned SVG engine.
        assert_eq!(a, b);
    }

    #[test]
    fn html_raw_width_and_line_boxes_keep_unbreakable_tokens_and_nbsp() {
        let host = fixture();
        let style = TextStyle::default();
        let mut r = request(
            TextMeasurementOperation::WrappedWithRawWidth,
            "token\u{a0}token",
            &style,
        );
        r.wrap_mode = WrapMode::HtmlLike;
        r.max_width = Some(1.0);
        let HostTextMeasurement::WrappedWithRawWidth { metrics, raw_width } =
            host.measure(r).unwrap().unwrap()
        else {
            panic!("wrapped/raw");
        };
        assert_eq!(metrics.line_count, 1);
        assert_eq!(metrics.height, style.font_size * 1.5);
        assert_eq!(raw_width, Some(metrics.width));
    }

    #[test]
    fn canvas_preparation_preserves_space_runs_and_non_ascii_whitespace() {
        let host = fixture();
        let style = TextStyle::default();
        let actual = host
            .measure(request(
                TextMeasurementOperation::CanvasMeasureTextWidth,
                "\tA\nB\r\u{c}C  \u{a0}",
                &style,
            ))
            .unwrap()
            .unwrap();
        let expected = host
            .measure(request(
                TextMeasurementOperation::CanvasMeasureTextWidth,
                " A B  C  \u{a0}",
                &style,
            ))
            .unwrap()
            .unwrap();
        let (HostTextMeasurement::Length(actual), HostTextMeasurement::Length(expected)) =
            (actual, expected)
        else {
            panic!("lengths");
        };
        assert_eq!(actual, expected);
        let HostTextMeasurement::Length(collapsed) = host
            .measure(request(
                TextMeasurementOperation::ComputedLength,
                " A B  C  \u{a0}",
                &style,
            ))
            .unwrap()
            .unwrap()
        else {
            panic!("length");
        };
        assert!(actual > collapsed);
    }

    #[test]
    fn actual_flowchart_render_reports_only_native_host_measurements() {
        use merman::svg::{SvgEnvironment, SvgPipeline, TextMeasurementSource};
        use merman::{RenderOutput, RenderRequest, Renderer, SvgRequest};
        let host = fixture();
        let request = SvgRequest {
            environment: SvgEnvironment::deterministic()
                .with_text_measurement_policy(host.policy().unwrap()),
            pipeline: Some(SvgPipeline::resvg_safe()),
            ..SvgRequest::default()
        };
        let output = Renderer::new()
            .render(RenderRequest::svg(
                "flowchart LR\n A[AV ffi] --> B[Wide WWWW]",
                host.control.clone(),
                request,
            ))
            .unwrap();
        host.finish().unwrap();
        let RenderOutput::Svg(Some(svg)) = output else {
            panic!("nonempty SVG output");
        };
        let evidence = svg.evidence().measurement().entries();
        assert!(!evidence.is_empty());
        for entry in evidence {
            assert_eq!(entry.provenance().source, TextMeasurementSource::Host);
            assert_eq!(entry.provenance().fallback_reason, None);
        }
        let tree = usvg::Tree::from_str(svg.svg(), &super::super::options(&host.fonts)).unwrap();
        // usvg's has_text_nodes() omits ordinary child groups in this version.
        let mut groups = vec![tree.root()];
        let mut has_text = false;
        while let Some(group) = groups.pop() {
            for node in group.children() {
                match node {
                    usvg::Node::Text(text) => has_text |= !text.layouted().is_empty(),
                    usvg::Node::Group(group) => groups.push(group),
                    _ => {}
                }
            }
        }
        assert!(has_text, "the generated labels must retain shaped text");
        assert!(host.fonts.coverage(&tree).unwrap().scripts.is_empty());
    }
}
