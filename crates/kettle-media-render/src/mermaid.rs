//! Mermaid source-to-pixels using one operation and the final SVG font database.

use std::sync::Arc;
use std::time::Duration;

use kettle_media::{Canvas, FailureCode, Job, MAX_MERMAID_BYTES, Rendered};
use merman::svg::{
    HostTheme, HostThemePreset, Presentation, RenderResourcePolicy, RootBackgroundPostprocessor,
    ScopedCssPostprocessor, SvgEnvironment, SvgPipeline, TextMeasurementSource, ThemeRole,
};
use merman::{
    Engine, MermaidConfig, OperationControl, RenderError, RenderOutput, RenderRequest, Renderer,
    SvgRequest,
};
use merman_core::theme_color::ThemeColor;

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
    let dark = job.canvas == Canvas::Theme && job.theme.is_dark;
    let preset = if dark {
        HostThemePreset::EditorDark
    } else {
        HostThemePreset::EditorLight
    };
    let mut theme = HostTheme::from_preset(preset)
        .try_with_font_family(family)
        .map_err(|_| FailureCode::RenderParse)?;
    if job.canvas != Canvas::Theme {
        let canvas = if job.canvas == Canvas::White {
            "#ffffff"
        } else {
            "transparent"
        };
        return theme
            .try_with_role(ThemeRole::Canvas, canvas)
            .map_err(|_| FailureCode::RenderParse);
    }
    let bg = job.theme.background;
    let fg = job.theme.foreground;
    let accent = job.theme.accent;
    for (role, color) in [
        (ThemeRole::Canvas, hex(bg)),
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

// These families use raw series colors or light-only fills instead of surface roles.
fn diagram_defaults(job: &Job, theme: &HostTheme) -> Result<MermaidConfig, FailureCode> {
    let background = if job.canvas == Canvas::Theme {
        job.theme.background
    } else {
        [255; 4]
    };
    let palette = theme
        .series_palette()
        .get(..5)
        .ok_or(FailureCode::RenderParse)?;
    let fills = palette
        .iter()
        .map(|color| {
            let rgb = ThemeColor::parse(color)
                .map_err(|_| FailureCode::RenderParse)?
                .rgba_channels();
            Ok(mixed(
                background,
                [
                    rgb.red.round() as u8,
                    rgb.green.round() as u8,
                    rgb.blue.round() as u8,
                    255,
                ],
                18,
            ))
        })
        .collect::<Result<Vec<_>, FailureCode>>()?;
    let [command, read_model, event, processor, confusion] = fills.as_slice() else {
        return Err(FailureCode::RenderParse);
    };
    Ok(MermaidConfig::from_value(serde_json::json!({
        "themeVariables": {
            "emProcessorFill": processor,
            "emReadModelFill": read_model,
            "emCommandFill": command,
            "emEventFill": event,
            "cynefin": {
                "complexBg": read_model,
                "complicatedBg": processor,
                "chaoticBg": command,
                "clearBg": event,
                "confusionBg": confusion
            }
        }
    })))
}

/// merman's resvg-safe pipeline, with the diagram's background as its canvas:
/// the pane's own, white, or none, so a checkerboard shows through. merman
/// otherwise paints every diagram on white, whatever its theme.
fn pipeline(job: &Job) -> SvgPipeline {
    let mut pipeline = SvgPipeline::resvg_safe();
    // Event-model headings set CSS color; SVG text also needs a fill.
    pipeline.push_postprocessor(ScopedCssPostprocessor::new(
        ".em-swimlane text { fill: currentColor; }",
    ));
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
    let host_theme = theme(job, &fonts.family)?;
    let defaults = diagram_defaults(job, &host_theme)?;
    let presentation = Presentation::new().with_theme(host_theme).resolve();
    let engine = presentation.materialize_engine(Engine::new().with_site_config(defaults));
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
    fn fixed_canvases_use_a_light_palette_while_theme_keeps_pane_colors() {
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
        let checker = theme(&job, "Fira Sans").unwrap();
        assert_eq!(checker.role(ThemeRole::Canvas), Some("transparent"));
        assert_eq!(checker.role(ThemeRole::Text), white.role(ThemeRole::Text));
        assert_eq!(checker.series_palette(), white.series_palette());
    }

    fn pixel(image: &Rendered, x: f64, y: f64) -> [u8; 3] {
        let x = (x * f64::from(image.width)) as usize;
        let y = (y * f64::from(image.height)) as usize;
        let at = (y * image.width as usize + x) * 4;
        image.rgba[at..at + 3].try_into().unwrap()
    }

    fn luminance(rgb: [u8; 3]) -> f64 {
        let linear = rgb.map(|channel| {
            let channel = f64::from(channel) / 255.0;
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        });
        0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]
    }

    fn foreground(job: &Job) -> [u8; 3] {
        let resolved = theme(job, "Fira Sans").unwrap();
        let text = resolved.role(ThemeRole::Text).unwrap();
        std::array::from_fn(|i| u8::from_str_radix(&text[1 + 2 * i..3 + 2 * i], 16).unwrap())
    }

    fn event_heading_pixels(image: &Rendered, foreground: [u8; 3]) -> usize {
        // Only the Events heading occupies this part of the swimlane.
        let mut text_pixels = 0;
        for y in image.height / 4..image.height / 3 {
            for x in image.width / 9..image.width / 4 {
                let at = ((y * image.width + x) * 4) as usize;
                if image.rgba[at..at + 3]
                    .iter()
                    .zip(foreground)
                    .all(|(&actual, expected)| actual.abs_diff(expected) < 20)
                {
                    text_pixels += 1;
                }
            }
        }
        text_pixels
    }

    #[test]
    fn event_headings_use_the_theme_text_color() {
        for canvas in [Canvas::Theme, Canvas::White, Canvas::Checker] {
            let mut job = job(include_str!("../tests/fixtures/mermaid/eventmodeling.mmd"));
            job.target.width = 960;
            job.target.height = 600;
            job.canvas = canvas;
            let image = render(&job).unwrap();
            assert!(
                event_heading_pixels(&image, foreground(&job)) > 10,
                "{canvas:?}: heading has no theme-colored text",
            );
        }
    }

    #[test]
    fn diagram_card_fills_contrast_with_default_text() {
        for (source, sample) in [
            (
                include_str!("../tests/fixtures/mermaid/eventmodeling.mmd"),
                (0.62, 0.35),
            ),
            (
                include_str!("../tests/fixtures/mermaid/cynefin.mmd"),
                (0.225, 0.335),
            ),
        ] {
            for canvas in [Canvas::Theme, Canvas::White, Canvas::Checker] {
                let mut job = job(source);
                job.target.width = 960;
                job.target.height = 600;
                job.canvas = canvas;
                let image = render(&job).unwrap();
                // Sample card padding, clear of glyphs and the border.
                let background = pixel(&image, sample.0, sample.1);
                let text = luminance(foreground(&job));
                let fill = luminance(background);
                let contrast = (text.max(fill) + 0.05) / (text.min(fill) + 0.05);
                assert!(
                    contrast >= 4.5,
                    "{canvas:?}: card {background:?}, contrast {contrast}"
                );
            }
        }
    }

    #[test]
    fn checker_canvas_keeps_domain_labels_readable() {
        let mut job = job(include_str!("../tests/fixtures/mermaid/cynefin.mmd"));
        job.target.width = 960;
        job.target.height = 600;
        job.canvas = Canvas::Checker;
        let image = render(&job).unwrap();
        let foreground = foreground(&job);
        let mut label_pixels = 0;
        for y in image.height / 5..image.height / 3 {
            for x in image.width / 5..image.width * 2 / 5 {
                let at = ((y * image.width + x) * 4) as usize;
                if image.rgba[at + 3] > 240
                    && image.rgba[at..at + 3]
                        .iter()
                        .zip(foreground)
                        .all(|(&actual, expected)| actual.abs_diff(expected) < 20)
                {
                    label_pixels += 1;
                }
            }
        }
        assert!(
            label_pixels > 10,
            "the domain labels have no text-colored pixels"
        );

        // Clear quadrant padding: composite its translucent fill over both
        // fixed checker tiles, in linear light as the image pipeline does.
        let at = ((image.height as usize * 18 / 100) * image.width as usize
            + image.width as usize / 4)
            * 4;
        let alpha = f64::from(image.rgba[at + 3]) / 255.0;
        assert!(alpha > 0.0 && alpha < 1.0);
        let fill = luminance(image.rgba[at..at + 3].try_into().unwrap());
        let text = luminance(foreground);
        for tile in [[0xcc; 3], [0xee; 3]] {
            let background = fill * alpha + luminance(tile) * (1.0 - alpha);
            let contrast = (text.max(background) + 0.05) / (text.min(background) + 0.05);
            assert!(
                contrast >= 4.5,
                "checker {tile:?}: label contrast {contrast}"
            );
        }
    }

    #[test]
    fn fixed_canvases_do_not_inherit_the_pane_palette() {
        for source in [
            include_str!("../tests/fixtures/mermaid/eventmodeling.mmd"),
            include_str!("../tests/fixtures/mermaid/cynefin.mmd"),
        ] {
            for canvas in [Canvas::White, Canvas::Checker] {
                let mut job = job(source);
                job.canvas = canvas;
                let before = render(&job).unwrap();
                job.theme.background = [3, 7, 11, 255];
                job.theme.foreground = [19, 29, 43, 255];
                job.theme.palette = [[255, 0, 0, 255]; 16];
                job.theme.accent = [0, 255, 0, 255];
                job.theme.is_dark = false;
                let after = render(&job).unwrap();
                assert!(
                    before.rgba == after.rgba,
                    "{canvas:?} canvas inherited pane colors"
                );
            }
        }
    }

    #[test]
    fn explicit_diagram_colors_override_host_defaults() {
        for (source, colors, sample) in [
            (
                include_str!("../tests/fixtures/mermaid/eventmodeling.mmd"),
                "emEventFill: '#d02020'",
                (0.62, 0.35),
            ),
            (
                include_str!("../tests/fixtures/mermaid/cynefin.mmd"),
                "cynefin: {clearBg: '#d02020'}",
                (0.692, 0.775),
            ),
        ] {
            let text = format!(
                "---\nconfig:\n  themeVariables:\n    textColor: '#22cc88'\n    {colors}\n---\n{source}"
            );
            for canvas in [Canvas::Theme, Canvas::White, Canvas::Checker] {
                let mut job = job(&text);
                job.target.width = 960;
                job.target.height = 600;
                job.canvas = canvas;
                let image = render(&job).unwrap();
                let color = pixel(&image, sample.0, sample.1);
                assert!(
                    color[0] > 180 && color[1] < 60 && color[2] < 60,
                    "{color:?}"
                );
                if source.starts_with("eventmodeling") {
                    assert!(event_heading_pixels(&image, [34, 204, 136]) > 10);
                }
            }
        }
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
        assert_eq!(rendered.exact_source.as_deref(), Some(source));
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

    /// A `%%` comment marker with nothing after it, alone on a line or
    /// ending the source, survives Mermaid's comment stripping and reaches
    /// each diagram's own lexer. merman's sequence lexer once matched the
    /// line feed before it as a comment it never consumed, and spun past
    /// every deadline (#584). Every bundled diagram, both ways, has to come
    /// back. Rendered with no deadline, so a case cannot pass by running out
    /// of time before it reaches the lexer.
    #[test]
    fn bare_comment_markers_never_hang_a_diagram() {
        let fixtures =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mermaid");
        let mut paths: Vec<_> = std::fs::read_dir(&fixtures)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "mmd"))
            .collect();
        paths.sort();
        assert!(paths.len() >= 30, "{} diagram fixtures", paths.len());
        for path in paths {
            let text = std::fs::read_to_string(&path).unwrap();
            let (header, body) = text.split_once('\n').unwrap();
            let body = body.trim_end();
            for source in [
                format!("{header}\n%%\n{body}\n"),
                format!("{header}\n{body}\n%%"),
            ] {
                let mut job = job(&source);
                job.target.width = 64;
                job.target.height = 64;
                let (sender, receiver) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let snapshot = source::load(&job.source, MAX_MERMAID_BYTES).unwrap();
                    let _ = sender.send(render_loaded(&job, &snapshot, OperationControl::new()));
                });
                let result = receiver.recv_timeout(Duration::from_secs(60));
                let Ok(result) = result else {
                    panic!("{} hung on {source:?}", path.display());
                };
                // The diagram that hung is a valid one, so it must render.
                if path.ends_with("sequence.mmd") {
                    assert!(result.is_ok(), "{source:?}: {:?}", result.err());
                }
            }
        }
    }
}
