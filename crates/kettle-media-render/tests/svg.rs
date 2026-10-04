//! SVG jobs through `render`: what is drawn, what is stripped, and what is
//! refused before it is allocated.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::path::Path;

use image::{ImageFormat, Rgba, RgbaImage};
use kettle_media::{
    Canvas, Crop, FailureCode, GuiActionWitness, Job, JobKind, MAX_SVG_BYTES, NativePath, Rendered,
    Source, Target, Theme,
};
use kettle_media_render::render;

fn theme() -> Theme {
    Theme {
        background: [0; 4],
        foreground: [255; 4],
        palette: [[0; 4]; 16],
        accent: [0; 4],
        is_dark: true,
    }
}

fn job(source: Source, width: u32, height: u32, crop: Option<Crop>) -> Job {
    Job {
        kind: JobKind::Svg,
        source,
        theme: theme(),
        canvas: Canvas::White,
        target: Target {
            width,
            height,
            scale: 1.0,
            crop,
        },
        fallback_fonts: vec![],
    }
}

fn svg(body: &str, size: u32) -> Vec<u8> {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{size}" height="{size}" viewBox="0 0 {size} {size}">{body}</svg>"#
    )
    .into_bytes()
}

fn render_svg(body: &str, size: u32) -> Result<Rendered, FailureCode> {
    render(&job(Source::Bytes(svg(body, size)), size, size, None))
}

fn pixel(rendered: &Rendered, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * rendered.width + x) * 4) as usize;
    rendered.rgba[at..at + 4].try_into().unwrap()
}

fn has_red(rendered: &Rendered) -> bool {
    rendered
        .rgba
        .chunks(4)
        .any(|pixel| pixel[3] > 0 && pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60)
}

/// A red PNG written into `directory`, as the injected images name.
fn red_png(directory: &Path) -> std::path::PathBuf {
    let path = directory.join("secret.png");
    RgbaImage::from_pixel(8, 8, Rgba([255, 0, 0, 255]))
        .save_with_format(&path, ImageFormat::Png)
        .unwrap();
    path
}

#[test]
fn shapes_render_with_straight_alpha() {
    let rendered = render_svg(
        r#"<rect width="10" height="10" fill="rgb(200,100,50)"/><rect x="10" width="10" height="10" fill="rgb(200,100,50)" fill-opacity="0.5"/>"#,
        20,
    )
    .unwrap();
    assert_eq!((rendered.width, rendered.height), (20, 20));
    assert_eq!(pixel(&rendered, 5, 5), [200, 100, 50, 255]);
    let half = pixel(&rendered, 15, 5);
    assert_eq!(half[3], 128);
    for (channel, expected) in half[..3].iter().zip([200u8, 100, 50]) {
        assert!(channel.abs_diff(expected) <= 1, "{half:?}");
    }
    // Uncovered pixels are transparent and all zero.
    assert_eq!(pixel(&rendered, 5, 15), [0; 4]);
    // The source comes back as display lines.
    assert_eq!(rendered.source_text.len(), 1);
    assert!(rendered.warnings.is_empty());
}

#[test]
fn text_renders_with_the_bundled_face_whatever_family_is_named() {
    for family in ["Arial", "serif", "NoSuchFont, sans-serif", "monospace"] {
        let rendered = render_svg(
            &format!(r#"<text x="2" y="40" font-size="40" font-family="{family}" fill="black">Kettle</text>"#),
            160,
        )
        .unwrap();
        let inked = rendered.rgba.chunks(4).filter(|pixel| pixel[3] > 0).count();
        assert!(inked > 100, "{family}: {inked} inked pixels");
    }
}

#[test]
fn injection_vectors_load_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let red = red_png(directory.path());
    let red = red.display();
    let other = directory.path().join("other.svg");
    std::fs::write(
        &other,
        svg(r#"<rect id="x" width="64" height="64" fill="red"/>"#, 64),
    )
    .unwrap();
    let other = other.display();
    let png_base64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";
    let vectors = [
        // A Mermaid style value that broke out of its attribute.
        format!(r##"<rect fill="#fff" width="64" height="64"/><image href="{red}" width="64" height="64"/><rect fill="#fff"/>"##),
        format!(r#"<image href="file://{red}" width="64" height="64"/>"#),
        r#"<image xlink:href="secret.png" width="64" height="64"/>"#.to_owned(),
        r#"<image href="http://127.0.0.1:9/secret.png" width="64" height="64"/>"#.to_owned(),
        format!(r#"<image href="data:image/png;base64,{png_base64}" width="64" height="64"/>"#),
        r#"<script>document.write('x')</script><rect onload="alert(1)" width="1" height="1"/><a href="javascript:alert(1)"><rect width="1" height="1"/></a>"#.to_owned(),
        format!(r#"<use href="{other}#x"/><use xlink:href="file://{other}#x"/>"#),
        format!(
            r#"<filter id="f"><feImage href="{red}"/></filter><rect width="64" height="64" filter="url(#f)"/><rect width="64" height="64" style="fill:url({other}#x)"/>"#
        ),
    ];
    for vector in &vectors {
        let rendered = render_svg(vector, 64).unwrap_or_else(|code| panic!("{code:?}: {vector}"));
        assert!(!has_red(&rendered), "{vector}");
    }
}

#[test]
fn foreign_content_is_dropped_and_its_fallback_drawn() {
    let rendered = render_svg(
        r#"<switch><foreignObject width="64" height="64"><div xmlns="http://www.w3.org/1999/xhtml" style="background:red">x</div></foreignObject><rect width="64" height="64" fill="blue"/></switch>"#,
        64,
    )
    .unwrap();
    assert_eq!(pixel(&rendered, 30, 30), [0, 0, 255, 255]);
}

#[test]
fn dtd_and_entities_are_refused() {
    for document in [
        r#"<!DOCTYPE svg [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;">]><svg xmlns="http://www.w3.org/2000/svg"><text>&b;</text></svg>"#,
        r#"<!DOCTYPE svg SYSTEM "file:///etc/passwd"><svg xmlns="http://www.w3.org/2000/svg"/>"#,
    ] {
        assert_eq!(
            render(&job(
                Source::Bytes(document.as_bytes().to_vec()),
                8,
                8,
                None
            ))
            .unwrap_err(),
            FailureCode::RenderParse
        );
    }
}

#[test]
fn shared_definition_is_charged_per_use() {
    // One blur, each use a full-canvas layer and result: at 512 a few uses
    // pass and many do not.
    let filter = r#"<filter id="f"><feGaussianBlur stdDeviation="2"/></filter>"#;
    let uses = |count: usize| -> String {
        (0..count)
            .map(|_| r#"<rect width="512" height="512" fill="green" filter="url(#f)"/>"#)
            .collect()
    };
    assert!(render_svg(&format!("{filter}{}", uses(4)), 512).is_ok());
    assert_eq!(
        render_svg(&format!("{filter}{}", uses(16)), 512).unwrap_err(),
        FailureCode::RenderResource
    );
}

#[test]
fn filter_primitives_each_consume_area() {
    let primitives = |count: usize| -> String {
        let chain: String = (0..count)
            .map(|_| r#"<feColorMatrix type="saturate" values="0.5"/>"#)
            .collect();
        format!(
            r#"<filter id="f">{chain}</filter><rect width="512" height="512" fill="green" filter="url(#f)"/>"#
        )
    };
    assert!(render_svg(&primitives(8), 512).is_ok());
    assert_eq!(
        render_svg(&primitives(16), 512).unwrap_err(),
        FailureCode::RenderResource
    );
}

#[test]
fn large_pattern_tile_is_rejected() {
    // An 8192-unit tile at a 1:1 scale: 67 million pixels for one fill.
    let pattern = r#"<pattern id="p" width="8192" height="8192" patternUnits="userSpaceOnUse"><rect width="8" height="8" fill="red"/></pattern><rect width="64" height="64" fill="url(#p)"/>"#;
    assert_eq!(
        render_svg(pattern, 64).unwrap_err(),
        FailureCode::RenderResource
    );
    // A small tile is fine.
    let small = r#"<pattern id="p" width="8" height="8" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="red"/></pattern><rect width="64" height="64" fill="url(#p)"/>"#;
    assert!(has_red(&render_svg(small, 64).unwrap()));
}

#[test]
fn nested_masks_exceed_layer_budget() {
    // A chain of masks, each a layer-sized canvas or more, under one group.
    let mut masks = String::new();
    for level in 0..40 {
        let next = if level < 39 {
            format!(r#" mask="url(#m{})""#, level + 1)
        } else {
            String::new()
        };
        masks.push_str(&format!(
            r#"<mask id="m{level}"{next}><rect width="512" height="512" fill="white"/></mask>"#
        ));
    }
    let body = format!(r#"{masks}<rect width="512" height="512" fill="green" mask="url(#m0)"/>"#);
    assert_eq!(
        render_svg(&body, 512).unwrap_err(),
        FailureCode::RenderResource
    );
}

#[test]
fn nonfinite_transform_is_refused_or_never_drawn() {
    // Each scale is finite; composed, they are not: refused before drawing.
    assert_eq!(
        render_svg(
            r#"<g transform="scale(1e30)"><g transform="scale(1e30)"><rect width="8" height="8" fill="red"/></g></g>"#,
            64
        )
        .unwrap_err(),
        FailureCode::RenderParse
    );
    // usvg drops a transform it cannot represent; whatever reaches the
    // layer walk with a non-finite transform is refused.
    for body in [
        r#"<g transform="scale(1e38) scale(1e38)"><rect width="8" height="8" fill="red"/></g>"#,
        r#"<g transform="matrix(1e39 0 0 1 0 0)" opacity="0.5"><rect width="8" height="8" fill="red"/></g>"#,
    ] {
        match render_svg(body, 64) {
            Ok(rendered) => assert!(!has_red(&rendered), "{body}"),
            Err(code) => assert_eq!(code, FailureCode::RenderParse, "{body}"),
        }
    }
}

#[test]
fn reference_bombs_are_refused_before_usvg_expands_them() {
    let mut body = String::from(r#"<rect id="l0" width="1" height="1"/>"#);
    for level in 1..=8 {
        body.push_str(&format!(r#"<g id="l{level}">"#));
        for _ in 0..10 {
            body.push_str(&format!(r##"<use href="#l{}"/>"##, level - 1));
        }
        body.push_str("</g>");
    }
    let started = std::time::Instant::now();
    assert_eq!(
        render_svg(&body, 64).unwrap_err(),
        FailureCode::RenderResource
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn the_svg_ceiling_bounds_the_result() {
    let rendered = render(&job(
        Source::Bytes(svg(r#"<rect width="10" height="10" fill="red"/>"#, 10)),
        4096,
        2048,
        None,
    ))
    .unwrap();
    assert_eq!((rendered.width, rendered.height), (1024, 1024));
    // A crop reaches the full fitted image, within the ceiling.
    let rendered = render(&job(
        Source::Bytes(svg(
            r#"<rect width="5" height="10" fill="red"/><rect x="5" width="5" height="10" fill="blue"/>"#,
            10,
        )),
        2048,
        2048,
        Some(Crop {
            x: 1000,
            y: 0,
            width: 96,
            height: 8,
        }),
    ))
    .unwrap();
    assert_eq!((rendered.width, rendered.height), (96, 8));
    assert_eq!(pixel(&rendered, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&rendered, 95, 0), [0, 0, 255, 255]);
}

#[test]
fn input_cap_boundary() {
    let base = svg(r#"<rect width="8" height="8" fill="red"/>"#, 8);
    let padded = |total: usize| -> Vec<u8> {
        // A comment pads the document to exactly `total` bytes.
        let pad = total - base.len() - "<!---->".len();
        let mut bytes = base.clone();
        bytes.extend(format!("<!--{}-->", " ".repeat(pad)).into_bytes());
        bytes
    };
    let at_cap = padded(MAX_SVG_BYTES);
    assert_eq!(at_cap.len(), MAX_SVG_BYTES);
    assert!(render(&job(Source::Bytes(at_cap), 8, 8, None)).is_ok());
    assert_eq!(
        render(&job(Source::Bytes(padded(MAX_SVG_BYTES + 1)), 8, 8, None)).unwrap_err(),
        FailureCode::TooLarge
    );
}

#[test]
fn a_path_source_renders_through_its_held_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("drawing.svg");
    std::fs::write(&path, svg(r#"<rect width="8" height="8" fill="red"/>"#, 8)).unwrap();
    use std::os::unix::ffi::OsStrExt as _;
    let source = Source::user_pull(
        NativePath::new(path.as_os_str().as_bytes().to_vec()).unwrap(),
        GuiActionWitness::from_explicit_gui_action(),
    );
    let rendered = render(&job(source, 8, 8, None)).unwrap();
    assert!(has_red(&rendered));
    assert!(rendered.digest.path_identity.is_some());
}

#[test]
fn not_utf8_or_not_svg_is_a_parse_failure() {
    for bytes in [
        vec![0xff, 0xfe, b'<'],
        b"<html/>".to_vec(),
        b"<svg/>".to_vec(),
        b"not xml".to_vec(),
    ] {
        assert_eq!(
            render(&job(Source::Bytes(bytes), 8, 8, None)).unwrap_err(),
            FailureCode::RenderParse
        );
    }
}

#[test]
fn group_layers_are_charged_per_isolated_group() {
    // Each translucent group is its own canvas-sized layer.
    let groups = |count: usize| -> String {
        (0..count)
            .map(|_| r#"<g opacity="0.5"><rect width="512" height="512" fill="green"/></g>"#)
            .collect()
    };
    assert!(render_svg(&groups(10), 512).is_ok());
    assert_eq!(
        render_svg(&groups(20), 512).unwrap_err(),
        FailureCode::RenderResource
    );
}

#[test]
fn clip_chains_are_charged_per_clip() {
    let mut clips = String::new();
    for level in 0..40 {
        let next = if level < 39 {
            format!(r#" clip-path="url(#c{})""#, level + 1)
        } else {
            String::new()
        };
        clips.push_str(&format!(
            r#"<clipPath id="c{level}"{next}><rect width="512" height="512"/></clipPath>"#
        ));
    }
    let body =
        format!(r#"{clips}<rect width="512" height="512" fill="green" clip-path="url(#c0)"/>"#);
    assert_eq!(
        render_svg(&body, 512).unwrap_err(),
        FailureCode::RenderResource
    );
    let one = r#"<clipPath id="c"><rect width="256" height="512"/></clipPath><rect width="512" height="512" fill="green" clip-path="url(#c)"/>"#;
    let rendered = render_svg(one, 512).unwrap();
    assert_eq!(pixel(&rendered, 100, 100), [0, 128, 0, 255]);
    assert_eq!(pixel(&rendered, 400, 100), [0; 4]);
}

#[test]
fn style_applied_patterns_are_refused() {
    // A style sheet gives every rectangle a pattern whose content is drawn
    // for each: two thousand fills of a thousand shapes. The structural pass
    // cannot tell which elements a rule matches, so a sheet naming a pattern
    // that holds painted shapes may close a cycle, and is refused as one
    // (the layer walk's drawing count stands behind it).
    let content: String = (0..1000)
        .map(|_| r#"<rect width="1" height="1" fill="red"/>"#)
        .collect();
    let rects: String = (0..2000)
        .map(|_| r#"<rect width="2" height="2"/>"#)
        .collect();
    let body = format!(
        r#"<style>.p {{ fill: url(#p) }}</style><pattern id="p" width="2" height="2" patternUnits="userSpaceOnUse">{content}</pattern><g class="p">{rects}</g>"#
    );
    assert_eq!(render_svg(&body, 64).unwrap_err(), FailureCode::RenderParse);
}

#[test]
fn filter_results_are_charged_at_the_layer_size() {
    // One filter makes the layer 1000x1000; a second, a pixel wide, takes
    // the source graphic ten times, and each result is a copy of the layer.
    let matrices: String = (0..10)
        .map(|_| r#"<feColorMatrix in="SourceGraphic" type="saturate" values="0.5"/>"#)
        .collect();
    let body = format!(
        r#"<filter id="a" x="0" y="0" width="1000" height="1000" filterUnits="userSpaceOnUse"><feOffset dx="1"/></filter><filter id="b" x="0" y="0" width="1" height="1" filterUnits="userSpaceOnUse">{matrices}</filter><rect width="1000" height="1000" fill="green" filter="url(#a) url(#b)"/>"#
    );
    assert_eq!(
        render_svg(&body, 1000).unwrap_err(),
        FailureCode::RenderResource
    );
}

#[test]
fn filter_rectangles_past_integer_range_are_refused_not_panicked_on() {
    // In bounds as written, scaled past what tiny-skia converts without
    // overflowing (its conversion unwraps).
    let body = r#"<g transform="scale(1000)"><filter id="f" primitiveUnits="userSpaceOnUse"><feFlood x="10000000" width="10" height="10" flood-color="red"/></filter><rect width="1" height="1" filter="url(#f)"/></g>"#;
    assert_eq!(render_svg(body, 64).unwrap_err(), FailureCode::RenderParse);
}

#[test]
fn marker_sizes_stay_within_what_usvg_multiplies() {
    // Past the number bounds: refused before usvg multiplies them.
    let huge = r##"<marker id="m" markerWidth="1e30" markerHeight="1e30" viewBox="0 0 1 1"><rect width="1" height="1"/></marker><path d="M0 0L10 10" stroke="black" stroke-width="1e10" marker-end="url(#m)"/>"##;
    assert_eq!(render_svg(huge, 64).unwrap_err(), FailureCode::RenderParse);
    // The extremes the bounds allow multiply to finite, non-zero sizes.
    for (size, stroke) in [("10000000", "10000000"), ("1e-20", "1e-20")] {
        let body = format!(
            r##"<marker id="m" markerWidth="{size}" markerHeight="{size}" viewBox="0 0 1 1"><rect width="1" height="1"/></marker><path d="M0 0L10 10" stroke="black" stroke-width="{stroke}" marker-end="url(#m)"/>"##
        );
        render_svg(&body, 64).unwrap_or_else(|code| panic!("{code:?}: {body}"));
    }
}

#[test]
fn inputs_that_would_crash_usvg_or_resvg_are_refused() {
    for body in [
        // A pattern drawn with itself through inherited paint: usvg recurses
        // until the stack overflows.
        r##"<g fill="url(#p2)"><pattern id="p1" width="4" height="4"><rect width="2" height="2"/></pattern></g><g fill="url(#p3)"><pattern id="p2" width="4" height="4"><rect width="2" height="2"/></pattern></g><g fill="url(#p1)"><pattern id="p3" width="4" height="4"><rect width="2" height="2"/></pattern><rect width="8" height="8"/></g>"##,
        // Relative font sizes multiplying a marker's size and a stroke width
        // past what usvg unwraps.
        r##"<g font-size="10000000"><g font-size="10000000em"><marker id="m" markerWidth="10000000em" markerHeight="10000000em" viewBox="0 0 1 1"><rect width="1" height="1"/></marker><path d="M0 0L10 10" stroke="black" stroke-width="10000000em" marker-end="url(#m)"/></g></g>"##,
        // A kernel whose dimensions multiply past 32 bits.
        r##"<filter id="f"><feConvolveMatrix order="65536 65536"/></filter><rect width="8" height="8" filter="url(#f)"/>"##,
    ] {
        assert_eq!(
            render_svg(body, 64).unwrap_err(),
            FailureCode::RenderParse,
            "{body}"
        );
    }
}
