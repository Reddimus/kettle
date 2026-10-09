//! Mermaid source-to-pixels using one operation and the final SVG font database.

use std::sync::Arc;
use std::time::Duration;

use kettle_media::{Canvas, FailureCode, Job, MAX_MERMAID_BYTES, Rendered};
use merman::svg::{
    HostTheme, HostThemePreset, Presentation, RenderResourcePolicy, RootBackgroundPostprocessor,
    SvgEnvironment, SvgPipeline, TextMeasurementSource, ThemeRole,
};
use merman::{
    Engine, OperationControl, RenderError, RenderOutput, RenderRequest, Renderer, SvgRequest,
};

use crate::{source, svg};

fn hex(color: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

fn mixed(a: [u8; 4], b: [u8; 4], weight: u16) -> String {
    let mut color = [0; 4];
    for channel in 0..3 {
        color[channel] =
            ((u16::from(a[channel]) * (100 - weight) + u16::from(b[channel]) * weight) / 100) as u8;
    }
    hex(color)
}

fn theme(job: &Job, family: &str) -> Result<HostTheme, FailureCode> {
    let dark = job.canvas != Canvas::White && job.theme.is_dark;
    let preset = if dark {
        HostThemePreset::EditorDark
    } else {
        HostThemePreset::EditorLight
    };
    let mut theme = HostTheme::from_preset(preset)
        .try_with_font_family(family)
        .map_err(|_| FailureCode::RenderParse)?;
    if job.canvas == Canvas::White {
        return theme
            .try_with_role(ThemeRole::Canvas, "#ffffff")
            .map_err(|_| FailureCode::RenderParse);
    }
    let bg = job.theme.background;
    let fg = job.theme.foreground;
    let accent = job.theme.accent;
    let canvas = if job.canvas == Canvas::Checker {
        "transparent".to_owned()
    } else {
        hex(bg)
    };
    for (role, color) in [
        (ThemeRole::Canvas, canvas),
        (ThemeRole::Surface, mixed(bg, accent, 12)),
        (ThemeRole::SurfaceAlt, mixed(bg, fg, 8)),
        (ThemeRole::SurfaceMuted, mixed(bg, fg, 4)),
        (ThemeRole::Text, hex(fg)),
        (ThemeRole::SubtleText, mixed(fg, bg, 30)),
        (ThemeRole::Border, hex(accent)),
        (ThemeRole::Line, hex(accent)),
        (ThemeRole::EdgeLabelBackground, hex(bg)),
        (ThemeRole::ClusterBackground, mixed(bg, accent, 4)),
        (ThemeRole::ClusterBorder, hex(accent)),
        (
            ThemeRole::NoteBackground,
            mixed(bg, job.theme.palette[3], 15),
        ),
        (ThemeRole::NoteBorder, hex(job.theme.palette[3])),
        (ThemeRole::NoteText, hex(fg)),
        (ThemeRole::ActorBackground, mixed(bg, accent, 12)),
        (ThemeRole::ActorBorder, hex(accent)),
        (ThemeRole::ActorText, hex(fg)),
        (ThemeRole::ActivationBackground, mixed(bg, accent, 20)),
        (ThemeRole::ActivationBorder, hex(accent)),
        (ThemeRole::Error, hex(job.theme.palette[1])),
        (ThemeRole::Warning, hex(job.theme.palette[3])),
        (ThemeRole::Success, hex(job.theme.palette[2])),
    ] {
        theme = theme
            .try_with_role(role, color)
            .map_err(|_| FailureCode::RenderParse)?;
    }
    theme
        .try_with_series_palette(job.theme.palette[1..7].iter().copied().map(hex))
        .map_err(|_| FailureCode::RenderParse)
}

/// merman's resvg-safe pipeline, with the diagram's background as its canvas:
/// the pane's own, white, or none, so a checkerboard shows through. merman
/// otherwise paints every diagram on white, whatever its theme.
fn pipeline(job: &Job) -> SvgPipeline {
    let mut pipeline = SvgPipeline::resvg_safe();
    pipeline.push_postprocessor(RootBackgroundPostprocessor::new(match job.canvas {
        Canvas::Theme => hex(job.theme.background),
        Canvas::White => "#ffffff".to_owned(),
        Canvas::Checker => "transparent".to_owned(),
    }));
    pipeline
}

fn failure(error: RenderError) -> FailureCode {
    match error {
        RenderError::Cancelled(_) => FailureCode::RenderTimeout,
        RenderError::ResourceLimitExceeded(_) => FailureCode::RenderResource,
        RenderError::NoDiagram | RenderError::UnsupportedTarget(_) => FailureCode::UnsupportedMedia,
        RenderError::Parse(error)
            if error.terminal_diagnostic_details().code == "merman.parse.no_diagram_detected" =>
        {
            FailureCode::UnsupportedMedia
        }
        _ => FailureCode::RenderParse,
    }
}

/// The time one diagram may take, from before its source and fallback fonts
/// load, not from layout: inside the worker's own deadline for the kind.
const DEADLINE: Duration = Duration::from_secs(2);

/// A fresh control whose deadline starts now.
pub(crate) fn control() -> OperationControl {
    OperationControl::new().with_deadline(DEADLINE)
}

pub(crate) fn render(job: &Job) -> Result<Rendered, FailureCode> {
    let control = control();
    let snapshot = source::load(&job.source, MAX_MERMAID_BYTES)?;
    render_loaded(job, &snapshot, control)
}

pub(crate) fn render_loaded(
    job: &Job,
    snapshot: &source::Snapshot<'_>,
    control: OperationControl,
) -> Result<Rendered, FailureCode> {
    if snapshot.bytes.len() > MAX_MERMAID_BYTES {
        return Err(FailureCode::TooLarge);
    }
    let text = std::str::from_utf8(&snapshot.bytes).map_err(|_| FailureCode::RenderParse)?;
    control
        .checkpoint()
        .map_err(|_| FailureCode::RenderTimeout)?;
    let fonts = Arc::new(svg::fonts::for_mermaid_job(&job.fallback_fonts)?);
    let host = svg::text_host::NativeTextHost::new(fonts.clone(), control.clone());
    host.finish()?;
    let presentation = Presentation::new()
        .with_theme(theme(job, &fonts.family)?)
        .resolve();
    let engine = presentation.materialize_engine(Engine::new());
    let request = SvgRequest {
        environment: SvgEnvironment::deterministic()
            .with_resource_policy(RenderResourcePolicy::constrained())
            .with_text_measurement_policy(host.policy()?),
        pipeline: Some(pipeline(job)),
        presentation: presentation.render_policy(),
        ..SvgRequest::default()
    };
    let result = crate::guarded(FailureCode::RenderParse, || {
        Renderer::new()
            .with_engine(engine)
            .render(RenderRequest::svg(text, control, request))
            .map_err(failure)
    });
    // Native error has precedence over the cancellation used to unwind the
    // host fallback route. Merman's own resource errors keep their type.
    if let Some(failure) = host.native_failure()? {
        return Err(failure);
    }
    let output = result?;
    host.finish()?;
    let RenderOutput::Svg(Some(output)) = output else {
        return Err(FailureCode::UnsupportedMedia);
    };
    if output
        .evidence()
        .measurement()
        .entries()
        .iter()
        .any(|entry| {
            entry.provenance().source != TextMeasurementSource::Host
                || entry.provenance().fallback_reason.is_some()
        })
    {
        return Err(FailureCode::RenderParse);
    }
    let (generated, _) = output.into_parts();
    host.finish()?;
    let rendered = svg::render_generated_svg(job, generated, snapshot, &fonts)?;
    host.finish()?;
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_media::{JobKind, Source, Target, Theme, content_digest};

    fn job(text: &str) -> Job {
        Job {
            kind: JobKind::Mermaid,
            source: Source::Bytes(text.as_bytes().to_vec()),
            target: Target {
                width: 320,
                height: 200,
                scale: 1.0,
                crop: None,
            },
            theme: Theme {
                background: [20, 24, 30, 255],
                foreground: [240, 242, 244, 255],
                palette: [[160, 100, 180, 255]; 16],
                accent: [125, 207, 255, 255],
                is_dark: true,
            },
            canvas: Canvas::Theme,
            fallback_fonts: Vec::new(),
        }
    }

    #[test]
    fn white_canvas_uses_a_light_palette_while_theme_keeps_pane_colors() {
        let mut job = job("flowchart LR; A-->B");
        let current = theme(&job, "Fira Sans").unwrap();
        assert_eq!(current.role(ThemeRole::Canvas), Some("#14181e"));
        assert_eq!(current.role(ThemeRole::Text), Some("#f0f2f4"));
        assert_eq!(current.role(ThemeRole::Border), Some("#7dcfff"));
        job.canvas = Canvas::White;
        let white = theme(&job, "Fira Sans").unwrap();
        assert_eq!(white.role(ThemeRole::Canvas), Some("#ffffff"));
        assert_ne!(white.role(ThemeRole::Text), current.role(ThemeRole::Text));
        assert_eq!(white.font_family(), Some("Fira Sans"));
        job.canvas = Canvas::Checker;
        assert_eq!(
            theme(&job, "Fira Sans").unwrap().role(ThemeRole::Canvas),
            Some("transparent")
        );
    }

    #[test]
    fn actual_mermaid_pixels_retain_original_source_and_digest() {
        let source = "flowchart LR\n A[AV ffi] --> B[Wide WWWW]";
        let job = job(source);
        let rendered = render(&job).unwrap();
        rendered.validate().unwrap();
        assert_eq!(
            rendered.digest,
            content_digest(source.as_bytes(), None).unwrap()
        );
        assert_eq!(rendered.source_text[0], "flowchart LR");
        assert!(
            rendered
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] != 0)
        );
        assert!(rendered.uncovered_scripts.is_empty());
    }

    /// A diagram's background is its canvas: the pane's own, white, or none
    /// at all for a checkerboard to show through, never merman's white.
    #[test]
    fn a_diagrams_background_is_its_canvas() {
        let corner = |canvas| {
            let mut job = job("flowchart LR\n A --> B");
            job.canvas = canvas;
            render(&job).unwrap().rgba[..4].to_vec()
        };
        assert_eq!(corner(Canvas::Theme), [20, 24, 30, 255]);
        assert_eq!(corner(Canvas::White), [255, 255, 255, 255]);
        assert_eq!(corner(Canvas::Checker), [0, 0, 0, 0]);
    }

    #[test]
    fn white_canvas_rerenders_pixels_without_changing_original_source_identity() {
        let mut job = job("flowchart LR\n A[AV] --> B[WWW]");
        let original = render(&job).unwrap();
        job.canvas = Canvas::White;
        let white = render(&job).unwrap();
        assert_eq!(original.digest, white.digest);
        assert_eq!(original.source_text, white.source_text);
        assert_ne!(original.rgba, white.rgba);
    }

    #[test]
    fn ordinary_content_and_invalid_known_diagram_syntax_remain_distinct() {
        assert_eq!(
            render(&job("ordinary prose")),
            Err(FailureCode::UnsupportedMedia)
        );
        assert_eq!(
            render(&job("flowchart TD\n A[unterminated")),
            Err(FailureCode::RenderParse)
        );
    }

    #[test]
    fn original_deadline_and_input_failures_keep_fixed_failure_codes() {
        let job = job("flowchart LR; A-->B");
        let snapshot = source::load(&job.source, MAX_MERMAID_BYTES).unwrap();
        assert_eq!(
            render_loaded(
                &job,
                &snapshot,
                OperationControl::new().with_deadline(Duration::ZERO)
            ),
            Err(FailureCode::RenderTimeout)
        );
        let mut invalid = job.clone();
        invalid.source = Source::Bytes(vec![255]);
        assert_eq!(render(&invalid), Err(FailureCode::RenderParse));
        invalid.source = Source::Bytes(vec![b'x'; MAX_MERMAID_BYTES + 1]);
        assert_eq!(render(&invalid), Err(FailureCode::TooLarge));
        invalid.source = Source::Bytes(Vec::new());
        assert_eq!(render(&invalid), Err(FailureCode::UnsupportedMedia));
    }
}
