//! PDF pages through the renderer, with documents written here, so their
//! provenance is this file, and upstream hayro's regression file for the
//! vendored parser fix (see `fixtures/pdf/README.md`).
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::time::{Duration, Instant};

use kettle_media::{
    Canvas, Crop, FailureCode, Job, JobKind, MediaKind, PageOf, Source, Target, Theme,
};
use kettle_media_render::{Means, render_with, render_with_kind};

#[path = "support/pdf.rs"]
mod support;
use support::pdf;

/// Fill the whole page in one color, given as PDF's 0..1 RGB.
fn fill(rgb: &str) -> String {
    format!("{rgb} rg 0 0 10000 10000 re f")
}

fn job(kind: JobKind, bytes: Vec<u8>, width: u32, height: u32, crop: Option<Crop>) -> Job {
    Job {
        kind,
        source: Source::Bytes(bytes),
        theme: Theme {
            background: [0; 4],
            foreground: [255; 4],
            palette: [[0; 4]; 16],
            accent: [0; 4],
            is_dark: true,
        },
        canvas: Canvas::Theme,
        target: Target {
            width,
            height,
            scale: 1.0,
            crop,
        },
        fallback_fonts: vec![],
    }
}

fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * width + x) * 4) as usize;
    rgba[at..at + 4].try_into().unwrap()
}

/// A page is drawn on white, fitted to the box keeping its shape, and says
/// which page it is of how many; an Auto job finds it by its bytes.
#[test]
fn a_pdf_page_renders_on_white_fitted_to_its_box() {
    // The left half red, the right half left as paper.
    let document = pdf(200, 100, &["1 0 0 rg 0 0 100 100 re f"]);
    for kind in [JobKind::Auto, JobKind::PdfPage { index: 0 }] {
        let (made, rendered) =
            render_with_kind(&job(kind, document.clone(), 400, 400, None), |_| {}).unwrap();
        assert_eq!(made, MediaKind::Pdf, "{kind:?}");
        assert_eq!((rendered.width, rendered.height), (400, 200));
        assert_eq!(rendered.page, Some(PageOf { index: 0, count: 1 }));
        assert_eq!(pixel(&rendered.rgba, 400, 50, 100), [255, 0, 0, 255]);
        assert_eq!(pixel(&rendered.rgba, 400, 350, 100), [255, 255, 255, 255]);
        assert!(rendered.exact_source.is_none());
    }
}

/// Each page renders by its index; a page past the last is out of range.
#[test]
fn each_page_renders_by_its_index() {
    let document = pdf(100, 100, &[&fill("1 0 0"), &fill("0 1 0"), &fill("0 0 1")]);
    for (index, color) in [
        (0, [255, 0, 0, 255]),
        (1, [0, 255, 0, 255]),
        (2, [0, 0, 255, 255]),
    ] {
        let (_, rendered) = render_with_kind(
            &job(JobKind::PdfPage { index }, document.clone(), 64, 64, None),
            |_| {},
        )
        .unwrap();
        assert_eq!(rendered.page, Some(PageOf { index, count: 3 }));
        assert_eq!(pixel(&rendered.rgba, rendered.width, 10, 10), color);
    }
    assert_eq!(
        render_with_kind(
            &job(JobKind::PdfPage { index: 3 }, document, 64, 64, None),
            |_| {}
        )
        .unwrap_err(),
        FailureCode::IndexOutOfRange
    );
}

/// A PDF is found by `%PDF-` near its start, whatever comes before it, and
/// an image that merely mentions it is still the image.
#[test]
fn a_pdf_is_found_by_its_header() {
    let mut late = b"junk a mailer prepended\r\n".to_vec();
    late.extend(pdf(100, 100, &[&fill("0 0 1")]));
    let (kind, _) = render_with_kind(&job(JobKind::Auto, late, 64, 64, None), |_| {}).unwrap();
    assert_eq!(kind, MediaKind::Pdf);
    // A PNG whose text chunk says %PDF-: its signature decides.
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder
            .add_text_chunk("Comment".into(), "%PDF-1.7".into())
            .unwrap();
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[1, 2, 3, 255])
            .unwrap();
    }
    let (kind, _) = render_with_kind(&job(JobKind::Auto, png, 8, 8, None), |_| {}).unwrap();
    assert_eq!(kind, MediaKind::Raster);
}

/// An unconfined worker parses no PDF, however it was asked for.
#[test]
fn an_unconfined_worker_draws_no_pdf() {
    let document = pdf(100, 100, &[&fill("1 0 0")]);
    let unconfined = Means {
        decoder: None,
        confined: false,
    };
    for kind in [JobKind::Auto, JobKind::PdfPage { index: 0 }] {
        let mut heard = Vec::new();
        let result = render_with(
            &job(kind, document.clone(), 64, 64, None),
            |kind| heard.push(kind),
            unconfined,
        );
        assert_eq!(
            result.unwrap_err(),
            FailureCode::SandboxUnavailable,
            "{kind:?}"
        );
        assert_eq!(heard, [MediaKind::Pdf], "{kind:?}");
    }
}

/// A crop renders just the part in view, as an SVG's does, for a zoomed lane.
#[test]
fn a_crop_renders_the_part_in_view() {
    let document = pdf(200, 100, &["1 0 0 rg 0 0 100 100 re f"]);
    let crop = Crop {
        x: 300,
        y: 100,
        width: 100,
        height: 100,
    };
    let (_, rendered) = render_with_kind(
        &job(
            JobKind::PdfPage { index: 0 },
            document,
            400,
            400,
            Some(crop),
        ),
        |_| {},
    )
    .unwrap();
    assert_eq!((rendered.width, rendered.height), (100, 100));
    assert_eq!(rendered.layout.result_in_target, crop);
    // The crop's left edge is the page's middle: red to its left, paper here.
    assert_eq!(pixel(&rendered.rgba, 100, 50, 50), [255, 255, 255, 255]);
}

/// A chain of object streams, each inside the next, is refused at once: the
/// released parser recursed once per level until the stack overflowed, an
/// abort no guard can catch (LaurenzV/hayro#1509).
#[test]
fn nested_object_streams_are_refused_without_overflowing() {
    let document = include_bytes!("fixtures/pdf/nested_object_streams.pdf").to_vec();
    let started = Instant::now();
    let result = render_with_kind(
        &job(JobKind::PdfPage { index: 0 }, document, 64, 64, None),
        |_| {},
    );
    assert!(result.is_err(), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// A page asked of what is no PDF by its bytes is unsupported, not broken.
#[test]
fn a_page_of_something_else_is_unsupported() {
    for bytes in [
        b"<svg/>".to_vec(),
        b"flowchart TD\nA-->B".to_vec(),
        vec![0xff; 64],
    ] {
        assert_eq!(
            render_with_kind(
                &job(JobKind::PdfPage { index: 0 }, bytes, 64, 64, None),
                |_| {}
            )
            .unwrap_err(),
            FailureCode::UnsupportedMedia
        );
    }
}

/// What is not a PDF, or is a broken one, fails as one.
#[test]
fn a_broken_pdf_is_a_parse_failure() {
    for bytes in [b"%PDF-1.7\nnothing else".to_vec(), pdf(100, 100, &[])] {
        assert_eq!(
            render_with_kind(
                &job(JobKind::PdfPage { index: 0 }, bytes, 64, 64, None),
                |_| {}
            )
            .unwrap_err(),
            FailureCode::RenderParse
        );
    }
}
