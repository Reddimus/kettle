//! SVG jobs: sanitized, admitted, then rendered with resvg.
//!
//! The source must be UTF-8 within the SVG input cap. It is parsed once with
//! no DTD and admitted structurally (`structure`), rewritten so nothing in it
//! refers outside the document (`sanitize`), and only then parsed by usvg,
//! with resolvers that load nothing, no resources directory, and the bundled
//! face and explicit per-job outline fonts (`fonts`). The image is fitted inside the target box keeping
//! its aspect ratio, then within `MAX_SVG_RENDERED_EDGE` and
//! `MAX_SVG_RENDERED_PIXELS`; a crop, in target-box coordinates around the
//! centered image, must itself fit those. Every layer, filter result, mask,
//! clip and pattern tile resvg would allocate is counted (`layers`) before
//! the canvas is. resvg's premultiplied result comes back as straight RGBA
//! with fully transparent pixels all zero, beside the source as display
//! lines.

mod css;
pub(crate) mod fonts;
mod generated_css;
mod layers;
mod sanitize;
mod structure;
pub(crate) mod text_cache;
pub(crate) mod text_host;
pub(crate) mod text_metrics;
mod text_rows;
pub(crate) mod text_wrap;

use kettle_media::{
    Crop, FailureCode, Job, JobKind, MAX_SOURCE_LINE_BYTES, MAX_SOURCE_LINES,
    MAX_SVG_RENDERED_EDGE, MAX_SVG_RENDERED_PIXELS, RenderLayout, Rendered, Target, Warning,
    content_digest,
};
use resvg::tiny_skia::{Pixmap, Transform};

use crate::source;

/// Build the bundled font database.
pub(crate) fn prepare() {
    fonts::bundled();
    fonts::mermaid_bundled();
}

pub(crate) fn render(job: &Job) -> Result<Rendered, FailureCode> {
    let snapshot = source::load(&job.source, job.kind.input_cap())?;
    let text = std::str::from_utf8(&snapshot.bytes).map_err(|_| FailureCode::RenderParse)?;
    render_parsed(job, &snapshot, &sanitize::parse(text)?)
}

/// Render an Auto job's snapshot if it is SVG: UTF-8 markup whose root
/// element is `svg`, within the SVG input cap. Anything else is
/// `UnsupportedMedia`, whatever its size and however hard it would be to
/// parse: the root is found before the cap applies or the parser runs.
/// `on_svg` runs once the snapshot is known to be SVG, before it is parsed.
pub(crate) fn render_auto(
    job: &Job,
    snapshot: &source::Snapshot<'_>,
    on_svg: impl FnOnce(),
) -> Result<Rendered, FailureCode> {
    let text = std::str::from_utf8(&snapshot.bytes).map_err(|_| FailureCode::UnsupportedMedia)?;
    if root_element_name(text) != Some("svg") {
        return Err(FailureCode::UnsupportedMedia);
    }
    snapshot.within(JobKind::Svg.input_cap())?;
    on_svg();
    let document = sanitize::parse(text)?;
    // The parser's root is the element found above; anything else is a
    // document it read differently, which is not one to render.
    if document.root_element().tag_name().name() != "svg" {
        return Err(FailureCode::RenderParse);
    }
    render_parsed(job, snapshot, &document)
}

/// Whether `text` is markup: an element follows its byte-order mark and
/// prolog.
pub(crate) fn is_markup(text: &str) -> bool {
    root_element_name(text).is_some()
}

/// Whether `text` is SVG markup: its first element, past the byte-order mark
/// and prolog, is `svg`.
pub(crate) fn is_svg(text: &str) -> bool {
    root_element_name(text) == Some("svg")
}

/// The local name of the first element in `text`, past a byte-order mark and
/// the prolog (the XML declaration, processing instructions, comments and a
/// document type declaration), found by one bounded scan without parsing.
/// `None` when `text` is not markup or its prolog never ends.
fn root_element_name(text: &str) -> Option<&str> {
    const XML_SPACE: [char; 4] = [' ', '\t', '\r', '\n'];
    let mut rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    loop {
        rest = rest.trim_start_matches(XML_SPACE);
        if let Some(after) = rest.strip_prefix("<?") {
            rest = &after[after.find("?>")? + 2..];
        } else if let Some(after) = rest.strip_prefix("<!--") {
            rest = &after[after.find("-->")? + 3..];
        } else if let Some(after) = rest.strip_prefix("<!") {
            rest = after_declaration(after)?;
        } else {
            let after = rest.strip_prefix('<')?;
            let end = after
                .find(|c: char| XML_SPACE.contains(&c) || c == '/' || c == '>')
                .unwrap_or(after.len());
            let name = &after[..end];
            return Some(name.rsplit_once(':').map_or(name, |(_, local)| local));
        }
    }
}

/// What follows a `<!...>` declaration: its end is the first `>` outside
/// quoted literals and an internal subset in brackets, where comments and
/// processing instructions are skipped whole, whatever they contain.
fn after_declaration(text: &str) -> Option<&str> {
    let mut depth = 0_usize;
    let mut rest = text;
    loop {
        let at = rest.find(['"', '\'', '[', ']', '>', '<'])?;
        let c = rest[at..].chars().next()?;
        let after = &rest[at + c.len_utf8()..];
        rest = match c {
            '"' | '\'' => &after[after.find(c)? + 1..],
            '<' => {
                if let Some(comment) = after.strip_prefix("!--") {
                    &comment[comment.find("-->")? + 3..]
                } else if let Some(instruction) = after.strip_prefix('?') {
                    &instruction[instruction.find("?>")? + 2..]
                } else {
                    after
                }
            }
            '[' => {
                depth += 1;
                after
            }
            ']' => {
                depth = depth.saturating_sub(1);
                after
            }
            '>' if depth == 0 => return Some(after),
            _ => after,
        };
    }
}

/// Render `document`, parsed from the UTF-8 of `snapshot`.
fn render_parsed(
    job: &Job,
    snapshot: &source::Snapshot<'_>,
    document: &roxmltree::Document<'_>,
) -> Result<Rendered, FailureCode> {
    // Sanitized (CSS resolved into attributes), then admitted on exactly
    // the text usvg will parse.
    let sanitized = sanitize::write(document)?;
    structure::admit(&sanitize::parse(&sanitized)?)?;
    let fonts = fonts::for_job(&job.fallback_fonts)?;
    render_admitted_svg(job, sanitized, snapshot, &fonts)
}

/// Generated diagrams retain their original source snapshot and layout fonts.
pub(crate) fn render_generated_svg(
    job: &Job,
    generated: String,
    snapshot: &source::Snapshot<'_>,
    fonts: &fonts::JobFonts,
) -> Result<Rendered, FailureCode> {
    if generated.len() > kettle_media::MAX_SVG_BYTES {
        return Err(FailureCode::RenderResource);
    }
    let document = sanitize::parse(&generated)?;
    let sanitized = sanitize::write_generated(&document)?;
    structure::admit(&sanitize::parse(&sanitized)?)?;
    drop(document);
    drop(generated);
    render_admitted_svg(job, sanitized, snapshot, fonts)
}

/// Render an admitted SVG while retaining the original source and its identity.
/// Generated diagrams share their font database with layout measurements.
pub(crate) fn render_admitted_svg(
    job: &Job,
    sanitized: String,
    snapshot: &source::Snapshot<'_>,
    fonts: &fonts::JobFonts,
) -> Result<Rendered, FailureCode> {
    let text = std::str::from_utf8(&snapshot.bytes).map_err(|_| FailureCode::RenderParse)?;
    let tree = crate::guarded(FailureCode::RenderParse, || {
        usvg::Tree::from_str(&sanitized, &options(fonts)).map_err(|_| FailureCode::RenderParse)
    })?;
    drop(sanitized);
    let placement = place(tree.size(), job.target)?;
    let mut coverage = fonts.coverage(&tree)?;
    let pixmap = crate::guarded(FailureCode::RenderParse, || {
        layers::admit(
            &tree,
            placement.transform,
            placement.width,
            placement.height,
        )?;
        let mut pixmap =
            Pixmap::new(placement.width, placement.height).ok_or(FailureCode::RenderResource)?;
        resvg::render(&tree, placement.transform, &mut pixmap.as_mut());
        Ok(pixmap)
    })?;
    drop(tree);
    let rgba = unpremultiply(pixmap.take());
    let digest =
        content_digest(&snapshot.bytes, snapshot.identity).map_err(|_| FailureCode::BadParams)?;
    let (source_text, clipped) = source_lines(text);
    if clipped {
        coverage.warnings.push(Warning::SourceDisplayClipped);
    }
    let rendered = Rendered {
        width: placement.width,
        height: placement.height,
        rgba,
        digest,
        layout: placement.layout,
        exact_source: Some(text.to_owned()),
        source_text,
        fence_sources: Vec::new(),
        fence_count: 0,
        fence_index: None,
        uncovered_scripts: coverage.scripts,
        warnings: coverage.warnings,
    };
    rendered
        .validate()
        .map_err(|_| FailureCode::RenderResource)?;
    Ok(rendered)
}

/// usvg's options: no resources directory, resolvers that load nothing for
/// data and string references alike (a `None` directory alone would still
/// let the default resolver read absolute paths), and this job's font bytes.
fn options(fonts: &fonts::JobFonts) -> usvg::Options<'static> {
    usvg::Options {
        resources_dir: None,
        font_family: fonts.family.clone(),
        fontdb: fonts.database.clone(),
        font_resolver: fonts::resolver(),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        style_sheet: None,
        ..usvg::Options::default()
    }
}

/// Where the image lands: the canvas to allocate and the transform onto it,
/// and where both sit in the target box.
#[derive(Debug, PartialEq)]
struct Placement {
    width: u32,
    height: u32,
    transform: Transform,
    layout: RenderLayout,
}

/// `edge * scale`, rounded, kept within 1..=`limit`.
fn scaled(edge: f32, scale: f32, limit: u32) -> u32 {
    let value = (f64::from(edge) * f64::from(scale)).round();
    // NaN lands here too.
    if value.is_nan() || value < 1.0 {
        1
    } else if value >= f64::from(limit) {
        limit
    } else {
        // In range: 1 <= value < limit <= u32::MAX.
        value as u32
    }
}

fn within_ceiling(width: u32, height: u32) -> bool {
    width <= MAX_SVG_RENDERED_EDGE
        && height <= MAX_SVG_RENDERED_EDGE
        && u64::from(width) * u64::from(height) <= MAX_SVG_RENDERED_PIXELS
}

/// Fit an image of `size` into `target`: within the box keeping its aspect
/// ratio, and without a crop within the SVG ceiling too. A crop selects
/// target-box pixels around the centered image and must fit the ceiling.
fn place(size: usvg::Size, target: Target) -> Result<Placement, FailureCode> {
    let (width, height) = (size.width(), size.height());
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return Err(FailureCode::RenderParse);
    }
    let mut scale = f32::min(target.width as f32 / width, target.height as f32 / height);
    let mut fitted = (
        scaled(width, scale, target.width),
        scaled(height, scale, target.height),
    );
    // The fitted image's place in the box, before any ceiling shrinks the
    // pixels that cover it.
    let image_in_target = Crop {
        x: (target.width - fitted.0) / 2,
        y: (target.height - fitted.1) / 2,
        width: fitted.0,
        height: fitted.1,
    };
    let layout = |result_in_target| RenderLayout {
        source_width: f64::from(width),
        source_height: f64::from(height),
        image_in_target,
        result_in_target,
    };
    if target.crop.is_none() && !within_ceiling(fitted.0, fitted.1) {
        let edge = MAX_SVG_RENDERED_EDGE as f32;
        let area = (MAX_SVG_RENDERED_PIXELS as f64 / (f64::from(fitted.0) * f64::from(fitted.1)))
            .sqrt() as f32;
        let shrink = (edge / fitted.0 as f32)
            .min(edge / fitted.1 as f32)
            .min(area);
        scale *= shrink;
        fitted = (
            scaled(width, scale, MAX_SVG_RENDERED_EDGE),
            scaled(height, scale, MAX_SVG_RENDERED_EDGE),
        );
        // Rounding may leave the area a pixel row over; take it back.
        while !within_ceiling(fitted.0, fitted.1) {
            fitted = (
                fitted.0.saturating_sub(1).max(1),
                fitted.1.saturating_sub(1).max(1),
            );
        }
    }
    let to_fitted = Transform::from_scale(fitted.0 as f32 / width, fitted.1 as f32 / height);
    match target.crop {
        None => Ok(Placement {
            width: fitted.0,
            height: fitted.1,
            transform: to_fitted,
            layout: layout(image_in_target),
        }),
        Some(Crop {
            x,
            y,
            width: crop_width,
            height: crop_height,
        }) => {
            if !within_ceiling(crop_width, crop_height) {
                return Err(FailureCode::BadParams);
            }
            // The fitted image is centered in the box; the crop is in box
            // coordinates (validated to lie inside the box).
            let left = (target.width - fitted.0) / 2;
            let top = (target.height - fitted.1) / 2;
            let shift = Transform::from_translate(left as f32 - x as f32, top as f32 - y as f32);
            Ok(Placement {
                width: crop_width,
                height: crop_height,
                transform: shift.pre_concat(to_fitted),
                layout: layout(Crop {
                    x,
                    y,
                    width: crop_width,
                    height: crop_height,
                }),
            })
        }
    }
}

/// tiny-skia's premultiplied RGBA as straight RGBA, rounded, with a fully
/// transparent pixel all zero.
fn unpremultiply(mut rgba: Vec<u8>) -> Vec<u8> {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 {
            *pixel = [0; 4];
            continue;
        }
        for channel in &mut pixel[..3] {
            // A premultiplied channel never exceeds its alpha; min() keeps a
            // malformed one in range anyway.
            *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
    rgba
}

/// The source as display lines: at most `MAX_SOURCE_LINES` lines of at most
/// `MAX_SOURCE_LINE_BYTES` bytes, cut at a character boundary, without line
/// breaks. Whether anything was left out.
fn source_lines(text: &str) -> (Vec<String>, bool) {
    let mut clipped = false;
    let mut lines = Vec::new();
    for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
        if lines.len() == MAX_SOURCE_LINES {
            clipped = true;
            break;
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut end = line.len().min(MAX_SOURCE_LINE_BYTES);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        clipped |= end < line.len();
        lines.push(line[..end].replace('\r', ""));
    }
    (lines, clipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: f32, height: f32) -> usvg::Size {
        usvg::Size::from_wh(width, height).unwrap()
    }

    fn target(width: u32, height: u32, crop: Option<Crop>) -> Target {
        Target {
            width,
            height,
            scale: 1.0,
            crop,
        }
    }

    #[test]
    fn placement_fits_the_box_then_the_svg_ceiling() {
        let placed = place(size(200.0, 100.0), target(400, 400, None)).unwrap();
        assert_eq!((placed.width, placed.height), (400, 200));
        let fitted = Crop {
            x: 0,
            y: 100,
            width: 400,
            height: 200,
        };
        assert_eq!(
            placed.layout,
            RenderLayout {
                source_width: 200.0,
                source_height: 100.0,
                image_in_target: fitted,
                result_in_target: fitted,
            }
        );
        // A 4096 box is fitted, then brought within 1024 a side: the pixels
        // still cover the whole fitted rectangle, more sparsely.
        let placed = place(size(100.0, 100.0), target(4096, 4096, None)).unwrap();
        assert_eq!((placed.width, placed.height), (1024, 1024));
        let whole = Crop {
            x: 0,
            y: 0,
            width: 4096,
            height: 4096,
        };
        assert_eq!(placed.layout.image_in_target, whole);
        assert_eq!(placed.layout.result_in_target, whole);
        placed.layout.validate(placed.width, placed.height).unwrap();
        let placed = place(size(4000.0, 1000.0), target(4096, 4096, None)).unwrap();
        assert_eq!((placed.width, placed.height), (1024, 256));
    }

    #[test]
    fn a_crop_must_fit_the_ceiling_and_shifts_the_image() {
        let crop = Crop {
            x: 100,
            y: 50,
            width: 512,
            height: 512,
        };
        let placed = place(size(100.0, 100.0), target(2048, 2048, Some(crop))).unwrap();
        assert_eq!((placed.width, placed.height), (512, 512));
        assert_eq!(placed.layout.result_in_target, crop);
        assert_eq!(placed.layout.image_in_target.width, 2048);
        assert_eq!(
            placed.transform,
            Transform::from_translate(-100.0, -50.0).pre_scale(20.48, 20.48)
        );
        let too_big = Crop {
            x: 0,
            y: 0,
            width: 2048,
            height: 2048,
        };
        assert_eq!(
            place(size(100.0, 100.0), target(2048, 2048, Some(too_big))).unwrap_err(),
            FailureCode::BadParams
        );
    }

    /// Images anywhere in a tree, nested groups included.
    fn has_image(group: &usvg::Group) -> bool {
        group.children().iter().any(|node| match node {
            usvg::Node::Image(_) => true,
            usvg::Node::Group(group) => has_image(group),
            _ => false,
        })
    }

    #[test]
    fn resolvers_load_nothing_even_where_sanitizing_would_miss() {
        // Unsanitized input straight to usvg with these options: a readable
        // absolute path, a raster data URL and an SVG data URL all stay out.
        let directory = tempfile::tempdir().unwrap();
        let png = directory.path().join("secret.png");
        std::fs::write(
            &png,
            [
                0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D',
                b'R', 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89,
            ],
        )
        .unwrap();
        let text = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><image href="{}" width="8" height="8"/><image href="data:image/png;base64,iVBORw0KGgo=" width="8" height="8"/><image href="data:image/svg+xml;utf8,&lt;svg xmlns='http://www.w3.org/2000/svg'&gt;&lt;rect width='8' height='8'/&gt;&lt;/svg&gt;" width="8" height="8"/></svg>"#,
            png.display()
        );
        let fonts = fonts::for_job(&[]).unwrap();
        let tree = usvg::Tree::from_str(&text, &options(&fonts)).unwrap();
        assert!(!has_image(tree.root()));
    }

    #[test]
    fn unpremultiplied_pixels_round_and_clear() {
        assert_eq!(
            unpremultiply(vec![64, 32, 0, 128, 9, 9, 9, 0, 255, 255, 255, 255]),
            [128, 64, 0, 128, 0, 0, 0, 0, 255, 255, 255, 255]
        );
    }

    fn generated_job(source: &[u8]) -> Job {
        Job {
            kind: kettle_media::JobKind::Mermaid,
            source: kettle_media::Source::Bytes(source.to_vec()),
            theme: kettle_media::Theme {
                background: [0; 4],
                foreground: [255; 4],
                palette: [[0; 4]; 16],
                accent: [0; 4],
                is_dark: true,
            },
            canvas: kettle_media::Canvas::Theme,
            target: target(16, 16, None),
            fallback_fonts: Vec::new(),
        }
    }

    #[test]
    fn generated_svg_keeps_original_source_and_file_identity() {
        let original = b"flowchart LR\n  A --> B\n";
        let job = generated_job(original);
        let fonts = fonts::for_job(&[]).unwrap();
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="red"/></svg>"#;
        let identity = kettle_media::PathIdentity {
            dev: 7,
            ino: 11,
            size: original.len() as u64,
            mtime_seconds: 13,
            mtime_nanos: 17,
        };
        let snapshot = source::Snapshot {
            bytes: std::borrow::Cow::Borrowed(original),
            identity: Some(identity),
        };
        let rendered = render_admitted_svg(&job, svg.to_owned(), &snapshot, &fonts).unwrap();
        assert_eq!(rendered.source_text, ["flowchart LR", "  A --> B"]);
        assert_eq!(
            rendered.exact_source.as_deref(),
            Some("flowchart LR\n  A --> B\n"),
            "the original source, not the generated SVG"
        );
        assert_eq!(
            rendered.digest,
            content_digest(original, Some(identity)).unwrap()
        );
        assert_ne!(
            rendered.digest,
            content_digest(svg.as_bytes(), None).unwrap()
        );
        assert_eq!(&rendered.rgba[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn generated_svg_pixels_can_change_without_replacing_source_metadata() {
        let original = b"flowchart LR\n  A --> B";
        let job = generated_job(original);
        let fonts = fonts::for_job(&[]).unwrap();
        let snapshot = source::Snapshot {
            bytes: std::borrow::Cow::Borrowed(original),
            identity: None,
        };
        let make_svg = |color| {
            format!(
                "<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'><rect width='16' height='16' fill='{color}'/></svg>"
            )
        };
        let red = render_admitted_svg(&job, make_svg("red"), &snapshot, &fonts).unwrap();
        let blue = render_admitted_svg(&job, make_svg("blue"), &snapshot, &fonts).unwrap();
        assert_eq!(red.digest, blue.digest);
        assert_eq!(red.source_text, blue.source_text);
        assert_ne!(red.rgba, blue.rgba);
        assert_eq!(&blue.rgba[..4], &[0, 0, 255, 255]);
    }

    fn generated_class_marker(fill: &str) -> Rendered {
        let original = b"classDiagram\n  A -- B";
        let mut job = generated_job(original);
        job.target = target(64, 32, None);
        let fonts = fonts::for_mermaid_job(&[]).unwrap();
        let snapshot = source::Snapshot {
            bytes: std::borrow::Cow::Borrowed(original),
            identity: None,
        };
        // Match Merman's generated marker-parent CSS, including priority and
        // descendant scoping. This is generated output, not a caller SVG job.
        let svg = format!(
            "<svg xmlns='http://www.w3.org/2000/svg' id='diagram' width='64' height='32'><style>#diagram .aggregation{{fill:{fill}!important;stroke:#00ff00!important;stroke-width:1;}}</style><defs><marker id='diagram-classDiagram-aggregationEnd' class='marker aggregation classDiagram' refX='18' refY='7' markerWidth='20' markerHeight='14' orient='0' markerUnits='userSpaceOnUse'><path d='M18,7 L9,13 L1,7 L9,1 Z'/></marker></defs><path d='M4,16 H50' stroke='#0000ff' stroke-width='1' marker-end='url(#diagram-classDiagram-aggregationEnd)'/></svg>"
        );
        render_admitted_svg(&job, svg, &snapshot, &fonts).unwrap()
    }

    #[test]
    fn generated_class_markers_keep_hollow_fill_from_parent_css() {
        let rendered = generated_class_marker("transparent");
        let inside = (19 * rendered.width as usize + 41) * 4;
        assert_eq!(&rendered.rgba[inside..inside + 4], &[0; 4]);
        // Interior probe is away from the relation line; this also requires
        // an actual marker outline, rather than a completely missing marker.
        assert!(
            rendered
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[1] > 100 && pixel[0] < 10 && pixel[2] < 10 && pixel[3] > 100)
        );
        assert_eq!(rendered.source_text, ["classDiagram", "  A -- B"]);
    }

    #[test]
    fn generated_class_markers_keep_filled_shapes_distinct_from_hollow_shapes() {
        let filled = generated_class_marker("#00ff00");
        let hollow = generated_class_marker("transparent");
        let inside = (19 * filled.width as usize + 41) * 4;
        assert_eq!(&filled.rgba[inside..inside + 4], &[0, 255, 0, 255]);
        assert_ne!(filled.rgba, hollow.rgba);
        assert_eq!(filled.digest, hollow.digest);
    }

    #[test]
    fn the_root_is_found_past_the_prolog_without_parsing() {
        for (text, root) in [
            ("<svg/>", Some("svg")),
            ("\u{feff} \r\n\t<svg width='1'>", Some("svg")),
            ("<svg>", Some("svg")),
            ("<svgx/>", Some("svgx")),
            ("<s:svg xmlns:s='http://www.w3.org/2000/svg'/>", Some("svg")),
            ("<?xml version='1.0'?><!-- <svg> --><html/>", Some("html")),
            ("<?xml version='1.0'?>\n<!-- a > b -->\n<svg/>", Some("svg")),
            (
                "<!DOCTYPE svg PUBLIC '-//W3C//DTD SVG 1.1//EN' 'x>y'><svg/>",
                Some("svg"),
            ),
            ("<!DOCTYPE html [<!ENTITY a '>'>]><html/>", Some("html")),
            // Inside the subset, comments and instructions are opaque.
            ("<!DOCTYPE html [<!-- ]><svg/> -->]><html/>", Some("html")),
            ("<!DOCTYPE html [<?pi ]><svg/> ?>]><html/>", Some("html")),
            ("<!DOCTYPE svg [<!-- ' -->]><svg/>", Some("svg")),
            ("<!DOCTYPE svg [<!-- [ -->]><svg/>", Some("svg")),
            ("<!DOCTYPE svg [<!ENTITY a \"]>\">]><svg/>", Some("svg")),
            ("< svg/>", Some("")),
            ("svg", None),
            ("", None),
            ("<!-- never closed <svg/>", None),
            ("<?xml never closed <svg/>", None),
            ("<!DOCTYPE never closed [<svg/>", None),
            ("\u{a0}<svg/>", None),
        ] {
            assert_eq!(root_element_name(text), root, "{text:?}");
        }
    }

    #[test]
    fn exact_source_keeps_line_endings_and_what_display_clips() {
        let long = "x".repeat(MAX_SOURCE_LINE_BYTES + 10);
        let original = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4\" height=\"4\">\r\n<!-- {long} -->\r\n</svg>"
        );
        let job = generated_job(original.as_bytes());
        let fonts = fonts::for_job(&[]).unwrap();
        let snapshot = source::Snapshot {
            bytes: std::borrow::Cow::Borrowed(original.as_bytes()),
            identity: None,
        };
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#;
        let rendered = render_admitted_svg(&job, svg.to_owned(), &snapshot, &fonts).unwrap();
        assert_eq!(rendered.exact_source.as_deref(), Some(original.as_str()));
        assert!(rendered.warnings.contains(&Warning::SourceDisplayClipped));
        assert!(rendered.source_text[1].len() < long.len());
    }

    #[test]
    fn source_lines_are_bounded_and_clean() {
        assert_eq!(
            source_lines("<svg>\r\n<g/>\r</svg>\n"),
            (vec!["<svg>".to_owned(), "<g/></svg>".to_owned()], false)
        );
        let long = "é".repeat(MAX_SOURCE_LINE_BYTES);
        let (lines, clipped) = source_lines(&long);
        assert!(clipped && lines[0].len() <= MAX_SOURCE_LINE_BYTES);
        let many = "x\n".repeat(MAX_SOURCE_LINES + 1);
        let (lines, clipped) = source_lines(&many);
        assert!(clipped && lines.len() == MAX_SOURCE_LINES);
    }

    #[test]
    fn source_lines_keep_real_blank_lines_at_the_cap() {
        for count in [MAX_SOURCE_LINES + 1, MAX_SOURCE_LINES, MAX_SOURCE_LINES - 1] {
            let (lines, clipped) = source_lines(&"\n".repeat(count));
            assert_eq!(lines.len(), count.min(MAX_SOURCE_LINES), "{count}");
            assert!(lines.iter().all(String::is_empty));
            assert_eq!(clipped, count > MAX_SOURCE_LINES, "{count}");
        }
        let long = format!("{}\n\n", "x".repeat(MAX_SOURCE_LINE_BYTES + 1));
        let (lines, clipped) = source_lines(&long);
        assert!(clipped);
        assert_eq!(lines, ["x".repeat(MAX_SOURCE_LINE_BYTES), String::new()]);
    }
}
