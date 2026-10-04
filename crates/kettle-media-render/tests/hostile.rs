//! Hostile containers and costly resizes, rendered under an allocator that
//! records the largest single allocation: a refusal must come before a
//! decoder allocates for the size the input claims, and a resize must not
//! build an intermediate larger than the image it starts from.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use image::{ImageFormat, Rgba, RgbaImage};
use kettle_media::{
    Canvas, FailureCode, FallbackFont, Job, JobKind, NativePath, Rendered, Source, Target, Theme,
};
use kettle_media_render::render;

/// The system allocator, recording the largest single request made on each
/// thread, so tests running beside a measurement cannot count toward it.
struct Largest;

thread_local! {
    // Constant-initialized with no destructor: reading it never allocates.
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

fn record(size: usize) {
    let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(size)));
}

// SAFETY: every call forwards to `System` unchanged; the only addition is a
// thread-local counter, which allocates nothing.
unsafe impl GlobalAlloc for Largest {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: the caller's contract for `alloc`, passed through.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: the caller's contract for `alloc_zeroed`, passed through.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(new_size);
        // SAFETY: the caller's contract for `realloc`, passed through.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract for `dealloc`, passed through.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Largest = Largest;

/// Render `bytes` into a `width` by `height` box: the result, and the largest
/// single allocation this thread made while rendering.
fn measured(bytes: Vec<u8>, width: u32, height: u32) -> (Result<Rendered, FailureCode>, usize) {
    measured_as(JobKind::Raster, bytes, width, height)
}

/// `measured` for a job of `kind`.
fn measured_as(
    kind: JobKind,
    bytes: Vec<u8>,
    width: u32,
    height: u32,
) -> (Result<Rendered, FailureCode>, usize) {
    measured_with_fonts(kind, bytes, width, height, Vec::new())
}

fn measured_with_fonts(
    kind: JobKind,
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    fallback_fonts: Vec<FallbackFont>,
) -> (Result<Rendered, FailureCode>, usize) {
    let job = Job {
        kind,
        source: Source::Bytes(bytes),
        theme: Theme {
            background: [0; 4],
            foreground: [255; 4],
            palette: [[0; 4]; 16],
            accent: [0; 4],
            is_dark: true,
        },
        canvas: Canvas::Checker,
        target: Target {
            width,
            height,
            scale: 1.0,
            crop: None,
        },
        fallback_fonts,
    };
    LARGEST.set(0);
    let result = render(&job);
    (result, LARGEST.get())
}

#[test]
fn a_collection_count_cannot_allocate_all_faces() {
    use std::os::unix::ffi::OsStrExt as _;
    // Warm only the trusted bundled database, outside the measurement.
    kettle_media_render::prepare();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("huge-count.ttc");
    std::fs::write(&path, b"ttcf\0\x01\0\0\xff\xff\xff\xff").unwrap();
    let entry = FallbackFont {
        path: NativePath::new(path.as_os_str().as_bytes().to_vec()).unwrap(),
        face_index: 0,
    };
    let (result, largest) = measured_with_fonts(
        JobKind::Svg,
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>".to_vec(),
        1,
        1,
        vec![entry],
    );
    assert_eq!(result.unwrap_err(), FailureCode::RenderParse);
    assert!(
        largest < 64 * 1024,
        "collection count caused a {largest}-byte allocation"
    );
}

/// A 2x2 lossy VP8 key frame of one color, from `cwebp -q 80` (libwebp) on a
/// 2x2 PNG. Its size fields are at bytes 6..10; `vp8` rewrites them.
const VP8_2X2: [u8; 46] = [
    0x90, 0x01, 0x00, 0x9d, 0x01, 0x2a, 0x02, 0x00, 0x02, 0x00, 0x01, 0x40, 0x26, 0x25, 0xa0, 0x02,
    0x74, 0xba, 0x00, 0x03, 0x98, 0x00, 0xfe, 0xf1, 0x4d, 0xaf, 0xe2, 0xda, 0x47, 0x42, 0x99, 0x0f,
    0xfe, 0xf1, 0x9f, 0xff, 0x71, 0x9f, 0xff, 0x71, 0x9f, 0xfc, 0x88, 0x00, 0x00, 0x00,
];

fn chunk(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = fourcc.to_vec();
    bytes.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend(payload);
    if payload.len() % 2 == 1 {
        bytes.push(0);
    }
    bytes
}

fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
    let body = chunks.concat();
    let mut bytes = b"RIFF".to_vec();
    bytes.extend(u32::try_from(body.len() + 4).unwrap().to_le_bytes());
    bytes.extend(b"WEBP");
    bytes.extend(body);
    bytes
}

fn u24(value: u32) -> [u8; 3] {
    let [a, b, c, _] = value.to_le_bytes();
    [a, b, c]
}

/// The extended header: flags (0x10 alpha, 0x02 animation) and the canvas.
fn vp8x(flags: u8, width: u32, height: u32) -> Vec<u8> {
    let payload = [
        [flags, 0, 0, 0].as_slice(),
        &u24(width - 1),
        &u24(height - 1),
    ]
    .concat();
    chunk(b"VP8X", &payload)
}

/// The VP8 fixture, declaring `width` by `height`.
fn vp8(width: u16, height: u16) -> Vec<u8> {
    let mut payload = VP8_2X2;
    payload[6..8].copy_from_slice(&width.to_le_bytes());
    payload[8..10].copy_from_slice(&height.to_le_bytes());
    chunk(b"VP8 ", &payload)
}

/// An uncompressed, unfiltered, fully opaque alpha plane.
fn alph(width: usize, height: usize) -> Vec<u8> {
    chunk(b"ALPH", &[vec![0], vec![255; width * height]].concat())
}

fn anim() -> Vec<u8> {
    chunk(b"ANIM", &[0, 0, 0, 0, 0, 0])
}

/// One animation frame at the origin, `width` by `height`, holding `inner`.
fn anmf(width: u32, height: u32, inner: &[Vec<u8>]) -> Vec<u8> {
    let header = [u24(0), u24(0), u24(width - 1), u24(height - 1), u24(100)].concat();
    chunk(b"ANMF", &[header, vec![0], inner.concat()].concat())
}

/// Far less than any frame the inputs below claim.
const SMALL: usize = 1024 * 1024;

#[test]
fn well_formed_extended_webp_still_renders() {
    for bytes in [
        // A still lossy image, with and without an alpha plane.
        riff(&[vp8x(0, 2, 2), vp8(2, 2)]),
        riff(&[vp8x(0x10, 2, 2), alph(2, 2), vp8(2, 2)]),
        // An animation's first frame, with and without one.
        riff(&[vp8x(0x02, 2, 2), anim(), anmf(2, 2, &[vp8(2, 2)])]),
        riff(&[
            vp8x(0x12, 2, 2),
            anim(),
            anmf(2, 2, &[alph(2, 2), vp8(2, 2)]),
        ]),
    ] {
        let (rendered, _) = measured(bytes, 2, 2);
        let rendered = rendered.unwrap();
        assert_eq!((rendered.width, rendered.height), (2, 2));
        assert!(rendered.rgba.chunks(4).all(|pixel| pixel[3] == 255));
    }
}

#[test]
fn webp_lossy_frame_larger_than_its_canvas_is_refused_before_allocation() {
    // A 1x1 canvas holding a bitstream that declares 4096x4096: the decoder
    // would allocate the frame's planes before comparing.
    let (result, largest) = measured(riff(&[vp8x(0, 1, 1), vp8(4096, 4096)]), 1, 1);
    assert_eq!(result.unwrap_err(), FailureCode::RenderParse);
    assert!(largest < SMALL, "allocated {largest} bytes");
}

#[test]
fn webp_animation_frame_must_match_its_bitstream() {
    // Larger than the frame, with no alpha plane: allocated before compared.
    let (result, largest) = measured(
        riff(&[vp8x(0x02, 1, 1), anim(), anmf(1, 1, &[vp8(4096, 4096)])]),
        1,
        1,
    );
    assert_eq!(result.unwrap_err(), FailureCode::RenderParse);
    assert!(largest < SMALL, "allocated {largest} bytes");
    // With an alpha plane the decoder never compares: a real 2x2 frame in a
    // 1x1 animation frame would write past the frame's buffer.
    let (result, _) = measured(
        riff(&[
            vp8x(0x12, 1, 1),
            anim(),
            anmf(1, 1, &[alph(1, 1), vp8(2, 2)]),
        ]),
        1,
        1,
    );
    assert_eq!(result.unwrap_err(), FailureCode::RenderParse);
    // And whatever follows the alpha plane is decoded as VP8, so it must be.
    let (result, _) = measured(
        riff(&[
            vp8x(0x12, 1, 1),
            anim(),
            anmf(1, 1, &[alph(1, 1), chunk(b"JUNK", &VP8_2X2)]),
        ]),
        1,
        1,
    );
    assert_eq!(result.unwrap_err(), FailureCode::RenderParse);
}

/// A BMP file and info header declaring `width` by `height` at 32 bits, with
/// no pixel data.
fn bmp_header(width: i32, height: i32) -> Vec<u8> {
    let mut bytes = b"BM".to_vec();
    bytes.extend(54u32.to_le_bytes()); // file size
    bytes.extend([0; 4]);
    bytes.extend(54u32.to_le_bytes()); // pixel data offset
    bytes.extend(40u32.to_le_bytes()); // info header size
    bytes.extend(width.to_le_bytes());
    bytes.extend(height.to_le_bytes());
    bytes.extend(1u16.to_le_bytes()); // planes
    bytes.extend(32u16.to_le_bytes()); // bits per pixel
    bytes.extend([0; 24]);
    bytes
}

#[test]
fn oversized_headers_are_resource_failures_for_every_decoder() {
    for bytes in [
        // Past the BMP decoder's own limit, and top-down past ours.
        bmp_header(70_000, 1),
        bmp_header(1, -70_000),
        bmp_header(9_000, 1),
        // A canvas whose pixel count overflows the WebP decoder's check, and
        // one merely past the edge cap.
        riff(&[vp8x(0, 70_000, 70_000), vp8(1, 1)]),
        riff(&[vp8x(0, 9_000, 1), vp8(1, 1)]),
    ] {
        let (result, largest) = measured(bytes, 8, 8);
        assert_eq!(result.unwrap_err(), FailureCode::RenderResource);
        assert!(largest < SMALL, "allocated {largest} bytes");
    }
}

#[test]
fn resize_holds_no_intermediate_larger_than_the_image() {
    // A 2048x2048 image (16 MiB decoded) halved: the largest allocation is
    // the decoded image itself, not a full-size working copy beside it.
    let image = RgbaImage::from_pixel(2048, 2048, Rgba([30, 60, 90, 200]));
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, ImageFormat::Png).unwrap();
    let decoded = 2048 * 2048 * 4;
    let (rendered, largest) = measured(png.into_inner(), 1024, 1024);
    let rendered = rendered.unwrap();
    assert_eq!((rendered.width, rendered.height), (1024, 1024));
    assert!(
        rendered
            .rgba
            .chunks(4)
            .all(|pixel| pixel == [30, 60, 90, 200])
    );
    assert!(largest <= decoded, "allocated {largest} bytes");
}

#[test]
fn svg_layer_rejection_precedes_pixmap_allocation() {
    // A filter region five canvases wide on a 1024 canvas: resvg would
    // allocate a 5120x5120 layer (100 MiB) and a result per primitive. The
    // refusal comes first, before the canvas itself.
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024"><filter id="f" x="-2" y="-2" width="5" height="5"><feFlood flood-color="red"/><feFlood flood-color="blue"/></filter><rect width="1024" height="1024" filter="url(#f)"/></svg>"#;
    let canvas = 1024 * 1024 * 4;
    let (result, largest) = measured_as(JobKind::Svg, svg.as_bytes().to_vec(), 1024, 1024);
    assert_eq!(result.unwrap_err(), FailureCode::RenderResource);
    assert!(largest < canvas, "allocated {largest} bytes");
}
