//! Raster jobs through `render`, with fixtures encoded here by the `image`
//! crate's own encoders, so their provenance is this file.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::io::Cursor;
use std::os::unix::fs::MetadataExt as _;
use std::time::{Duration, Instant};

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use kettle_media::{
    Canvas, Crop, FailureCode, GuiActionWitness, Job, JobKind, NativePath, PathIdentity, Source,
    Target, Theme, content_digest,
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

fn target(width: u32, height: u32, crop: Option<Crop>) -> Target {
    Target {
        width,
        height,
        scale: 1.0,
        crop,
    }
}

fn job(source: Source, target: Target) -> Job {
    Job {
        kind: JobKind::Raster,
        source,
        theme: theme(),
        canvas: Canvas::Checker,
        target,
        fallback_fonts: vec![],
    }
}

fn solid(width: u32, height: u32, color: [u8; 4]) -> RgbaImage {
    RgbaImage::from_pixel(width, height, Rgba(color))
}

fn encode(image: &RgbaImage, format: ImageFormat) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    let image = DynamicImage::ImageRgba8(image.clone());
    // JPEG has no alpha channel.
    let image = if format == ImageFormat::Jpeg {
        DynamicImage::ImageRgb8(image.to_rgb8())
    } else {
        image
    };
    image.write_to(&mut bytes, format).unwrap();
    bytes.into_inner()
}

fn user_pull(path: &std::path::Path) -> Source {
    use std::os::unix::ffi::OsStrExt as _;
    Source::user_pull(
        NativePath::new(path.as_os_str().as_bytes().to_vec()).unwrap(),
        GuiActionWitness::from_explicit_gui_action(),
    )
}

#[test]
fn every_supported_format_renders() {
    let image = solid(4, 2, [10, 200, 30, 255]);
    for format in [
        ImageFormat::Png,
        ImageFormat::Jpeg,
        ImageFormat::WebP,
        ImageFormat::Bmp,
        ImageFormat::Gif,
    ] {
        let rendered = render(&job(
            Source::Bytes(encode(&image, format)),
            target(4, 2, None),
        ))
        .unwrap_or_else(|code| panic!("{format:?}: {code:?}"));
        assert_eq!((rendered.width, rendered.height), (4, 2), "{format:?}");
        assert_eq!(rendered.rgba.len(), 4 * 2 * 4);
    }
}

#[test]
fn spoofed_extension_uses_content() {
    let directory = tempfile::tempdir().unwrap();
    // A PNG named as a JPEG renders; text named as a PNG does not.
    let png_as_jpeg = directory.path().join("image.jpg");
    std::fs::write(
        &png_as_jpeg,
        encode(&solid(2, 2, [1, 2, 3, 255]), ImageFormat::Png),
    )
    .unwrap();
    assert!(render(&job(user_pull(&png_as_jpeg), target(2, 2, None))).is_ok());
    let text_as_png = directory.path().join("image.png");
    std::fs::write(&text_as_png, b"not an image at all").unwrap();
    assert_eq!(
        render(&job(user_pull(&text_as_png), target(2, 2, None))).unwrap_err(),
        FailureCode::UnsupportedMedia
    );
    // A format the job does not take (TIFF) is refused by content too.
    assert_eq!(
        render(&job(
            Source::Bytes(b"II*\0\x08\0\0\0".to_vec()),
            target(2, 2, None)
        ))
        .unwrap_err(),
        FailureCode::UnsupportedMedia
    );
}

/// A PNG that is little more than a header claiming `width` by `height` RGBA
/// pixels.
fn png_header(width: u32, height: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(13u32.to_be_bytes());
    png.extend(&ihdr);
    png.extend(crc32(&ihdr).to_be_bytes());
    // A first data chunk (an empty zlib stream), so the decoder reaches the
    // point where it reports the dimensions; nothing more is needed.
    let mut idat = b"IDAT".to_vec();
    idat.extend([0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
    png.extend(8u32.to_be_bytes());
    png.extend(&idat);
    png.extend(crc32(&idat).to_be_bytes());
    png.extend(0u32.to_be_bytes());
    png.extend(b"IEND");
    png.extend(crc32(b"IEND").to_be_bytes());
    png
}

#[test]
fn decoded_edge_and_byte_caps_precede_pixel_allocation() {
    // Over the edge cap, and within it but over the decoded byte cap (8192²
    // RGBA is 256 MiB): refused from the header, at once, with no pixel
    // decoded or allocated.
    for (width, height) in [(9000, 10), (10, 9000), (8192, 8192), (u32::MAX, 1)] {
        let started = Instant::now();
        assert_eq!(
            render(&job(
                Source::Bytes(png_header(width, height)),
                target(64, 64, None)
            ))
            .unwrap_err(),
            FailureCode::RenderResource,
            "{width}x{height}"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

#[test]
fn gif_returns_first_frame_only() {
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame};
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut bytes);
        for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
            encoder
                .encode_frame(Frame::from_parts(
                    solid(3, 3, color),
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
        }
    }
    let rendered = render(&job(Source::Bytes(bytes), target(3, 3, None))).unwrap();
    for pixel in rendered.rgba.chunks(4) {
        assert_eq!(pixel, [255, 0, 0, 255]);
    }
}

#[test]
fn rgba_is_straight_with_zero_alpha_zero_rgb() {
    let mut image = RgbaImage::new(2, 1);
    image.put_pixel(0, 0, Rgba([200, 100, 50, 128]));
    image.put_pixel(1, 0, Rgba([255, 255, 255, 0]));
    let rendered = render(&job(
        Source::Bytes(encode(&image, ImageFormat::Png)),
        target(2, 1, None),
    ))
    .unwrap();
    assert_eq!(rendered.rgba, [200, 100, 50, 128, 0, 0, 0, 0]);
    // Resampled too: a half-transparent edge keeps its own color, with no
    // white bleeding in from the transparent pixels beside it.
    let mut image = RgbaImage::from_pixel(4, 4, Rgba([255, 255, 255, 0]));
    for y in 0..4 {
        image.put_pixel(0, y, Rgba([0, 0, 255, 255]));
        image.put_pixel(1, y, Rgba([0, 0, 255, 255]));
    }
    let rendered = render(&job(
        Source::Bytes(encode(&image, ImageFormat::Png)),
        target(2, 2, None),
    ))
    .unwrap();
    for pixel in rendered.rgba.chunks(4) {
        if pixel[3] == 0 {
            assert_eq!(pixel, [0, 0, 0, 0]);
        } else {
            assert_eq!(&pixel[..3], [0, 0, 255], "{pixel:?}");
        }
    }
}

#[test]
fn canvas_and_crop_are_bounded() {
    let red = [255, 0, 0, 255];
    let png = encode(&solid(8, 4, red), ImageFormat::Png);
    // Fitted inside a 4x4 box keeping the aspect ratio: 4x2.
    let rendered = render(&job(Source::Bytes(png.clone()), target(4, 4, None))).unwrap();
    assert_eq!((rendered.width, rendered.height), (4, 2));
    // Never enlarged: in a 16x16 box it keeps its own 8x4.
    let rendered = render(&job(Source::Bytes(png.clone()), target(16, 16, None))).unwrap();
    assert_eq!((rendered.width, rendered.height), (8, 4));
    assert!(rendered.rgba.chunks(4).all(|pixel| pixel == red));
    // A crop is in box coordinates around the centered image: in a 16x16
    // box, columns 4..12 and rows 6..10.
    let rendered = render(&job(
        Source::Bytes(png.clone()),
        target(
            16,
            16,
            Some(Crop {
                x: 2,
                y: 4,
                width: 4,
                height: 4,
            }),
        ),
    ))
    .unwrap();
    assert_eq!((rendered.width, rendered.height), (4, 4));
    let rows: Vec<&[u8]> = rendered.rgba.chunks(4 * 4).collect();
    for (row, pixels) in rows.iter().enumerate() {
        for (column, pixel) in pixels.chunks(4).enumerate() {
            let inside = row >= 2 && column >= 2;
            let expected = if inside { red } else { [0; 4] };
            assert_eq!(pixel, expected, "row {row}, column {column}");
        }
    }
    // A target past the rendered cap, or a crop outside its box, is refused.
    for bad in [
        target(5000, 10, None),
        target(
            8,
            8,
            Some(Crop {
                x: 6,
                y: 0,
                width: 4,
                height: 4,
            }),
        ),
    ] {
        assert_eq!(
            render(&job(Source::Bytes(png.clone()), bad)).unwrap_err(),
            FailureCode::BadParams
        );
    }
}

#[test]
fn source_digest_uses_held_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("image.png");
    let png = encode(&solid(2, 2, [9, 9, 9, 255]), ImageFormat::Png);
    std::fs::write(&path, &png).unwrap();
    let metadata = std::fs::metadata(&path).unwrap();
    let identity = PathIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        size: metadata.len(),
        mtime_seconds: metadata.mtime(),
        mtime_nanos: u32::try_from(metadata.mtime_nsec()).unwrap(),
    };
    let rendered = render(&job(user_pull(&path), target(2, 2, None))).unwrap();
    assert_eq!(
        rendered.digest,
        content_digest(&png, Some(identity)).unwrap()
    );
    // The same bytes inline carry no file identity.
    let inline = render(&job(Source::Bytes(png.clone()), target(2, 2, None))).unwrap();
    assert_eq!(inline.digest, content_digest(&png, None).unwrap());
}

#[test]
fn jobs_other_than_raster_and_svg_are_unsupported_for_now() {
    let png = encode(&solid(2, 2, [1, 1, 1, 255]), ImageFormat::Png);
    for kind in [JobKind::Mermaid, JobKind::VideoProbe] {
        let mut job = job(Source::Bytes(png.clone()), target(2, 2, None));
        job.kind = kind;
        assert_eq!(render(&job).unwrap_err(), FailureCode::UnsupportedMedia);
    }
}
