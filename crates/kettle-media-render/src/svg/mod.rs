//! SVG jobs: sanitized, admitted, then rendered with resvg.
//!
//! The source must be UTF-8 within the SVG input cap. It is parsed once with
//! no DTD and admitted structurally (`structure`), rewritten so nothing in it
//! refers outside the document (`sanitize`), and only then parsed by usvg,
//! with resolvers that load nothing, no resources directory, and the bundled
//! font alone (`fonts`). The image is fitted inside the target box keeping
//! its aspect ratio, then within `MAX_SVG_RENDERED_EDGE` and
//! `MAX_SVG_RENDERED_PIXELS`; a crop, in target-box coordinates around the
//! centered image, must itself fit those. Every layer, filter result, mask,
//! clip and pattern tile resvg would allocate is counted (`layers`) before
//! the canvas is. resvg's premultiplied result comes back as straight RGBA
//! with fully transparent pixels all zero, beside the source as display
//! lines.

mod css;
mod fonts;
mod layers;
mod sanitize;
mod structure;

use kettle_media::{
    Crop, FailureCode, Job, MAX_SOURCE_LINE_BYTES, MAX_SOURCE_LINES, MAX_SVG_RENDERED_EDGE,
    MAX_SVG_RENDERED_PIXELS, Rendered, Target, Warning, content_digest,
};
use resvg::tiny_skia::{Pixmap, Transform};

use crate::source;

/// Build the bundled font database.
pub(crate) fn prepare() {
    fonts::bundled();
}

pub(crate) fn render(job: &Job) -> Result<Rendered, FailureCode> {
    let snapshot = source::load(&job.source, job.kind.input_cap())?;
    let text = std::str::from_utf8(&snapshot.bytes).map_err(|_| FailureCode::RenderParse)?;
    // Sanitized (CSS resolved into attributes), then admitted on exactly
    // the text usvg will parse.
    let sanitized = sanitize::write(&sanitize::parse(text)?)?;
    structure::admit(&sanitize::parse(&sanitized)?)?;
    let tree =
        usvg::Tree::from_str(&sanitized, &options()).map_err(|_| FailureCode::RenderParse)?;
    drop(sanitized);
    let placement = place(tree.size(), job.target)?;
    layers::admit(
        &tree,
        placement.transform,
        placement.width,
        placement.height,
    )?;
    let mut pixmap =
        Pixmap::new(placement.width, placement.height).ok_or(FailureCode::RenderResource)?;
    resvg::render(&tree, placement.transform, &mut pixmap.as_mut());
    drop(tree);
    let rgba = unpremultiply(pixmap.take());
    let digest =
        content_digest(&snapshot.bytes, snapshot.identity).map_err(|_| FailureCode::BadParams)?;
    let (source_text, clipped) = source_lines(text);
    let rendered = Rendered {
        width: placement.width,
        height: placement.height,
        rgba,
        digest,
        source_text,
        fence_sources: Vec::new(),
        fence_count: 0,
        fence_index: None,
        uncovered_scripts: Vec::new(),
        warnings: if clipped {
            vec![Warning::SourceDisplayClipped]
        } else {
            Vec::new()
        },
    };
    rendered
        .validate()
        .map_err(|_| FailureCode::RenderResource)?;
    Ok(rendered)
}

/// usvg's options: no resources directory, resolvers that load nothing for
/// data and string references alike (a `None` directory alone would still
/// let the default resolver read absolute paths), and the bundled font.
fn options() -> usvg::Options<'static> {
    let (fontdb, family) = fonts::bundled();
    usvg::Options {
        resources_dir: None,
        font_family: family.clone(),
        fontdb: fontdb.clone(),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        style_sheet: None,
        ..usvg::Options::default()
    }
}

/// Where the image lands: the canvas to allocate and the transform onto it.
#[derive(Debug, PartialEq)]
struct Placement {
    width: u32,
    height: u32,
    transform: Transform,
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
    for line in text.split('\n') {
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
    if text.ends_with('\n') && lines.last().is_some_and(String::is_empty) {
        lines.pop();
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
        // A 4096 box is fitted, then brought within 1024 a side.
        let placed = place(size(100.0, 100.0), target(4096, 4096, None)).unwrap();
        assert_eq!((placed.width, placed.height), (1024, 1024));
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
        let tree = usvg::Tree::from_str(&text, &options()).unwrap();
        assert!(!has_image(tree.root()));
    }

    #[test]
    fn unpremultiplied_pixels_round_and_clear() {
        assert_eq!(
            unpremultiply(vec![64, 32, 0, 128, 9, 9, 9, 0, 255, 255, 255, 255]),
            [128, 64, 0, 128, 0, 0, 0, 0, 255, 255, 255, 255]
        );
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
}
