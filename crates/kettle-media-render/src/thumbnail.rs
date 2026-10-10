//! Cached thumbnail jobs: a video's thumbnail as the freedesktop.org
//! Thumbnail Managing Standard caches it, a PNG whose text names the video it
//! was made from (`Thumb::URI`) and that video's modification time when it
//! was made (`Thumb::MTime`). It renders as a raster only when both are the
//! video's the job names. A thumbnail of another file, of the video before
//! it last changed, or one that names neither is `Changed`; anything but a
//! PNG is `UnsupportedMedia`.
//!
//! The text is read from the chunks before the image data, where the
//! standard's writers put it, and the image's size is checked against the
//! largest thumbnail accepted, all before a pixel is decoded; the pixels
//! then go through the raster path, under its own caps.

use kettle_media::{FailureCode, Job, Rendered, ThumbnailOf};

use crate::{raster, source};

/// Every PNG starts with these bytes.
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
/// The largest thumbnail accepted, on a side and in pixels. The standard's
/// largest size is 1024 on a side; other writers make larger ones.
const MAX_EDGE: u32 = 4_096;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
/// What reading the chunks before the pixels may allocate.
const MAX_HEADER_BYTES: usize = 1024 * 1024;

pub(crate) fn render(job: &Job, of: ThumbnailOf) -> Result<Rendered, FailureCode> {
    let snapshot = source::load(&job.source, job.kind.input_cap())?;
    crate::guarded(FailureCode::RenderParse, || check(&snapshot.bytes, of))?;
    raster::render_loaded(job, &snapshot)
}

/// Whether `bytes` are a PNG thumbnail of the video `of` names, at a size
/// accepted.
fn check(bytes: &[u8], of: ThumbnailOf) -> Result<(), FailureCode> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Err(FailureCode::UnsupportedMedia);
    }
    let mut decoder = png::Decoder::new_with_limits(
        std::io::Cursor::new(bytes),
        png::Limits {
            bytes: MAX_HEADER_BYTES,
        },
    );
    // The size first, from the header alone: reading the text sizes the
    // image's buffers, which fails on its own for a huge one.
    let header = decoder.read_header_info().map_err(failure)?;
    if header.width > MAX_EDGE
        || header.height > MAX_EDGE
        || u64::from(header.width) * u64::from(header.height) > MAX_PIXELS
    {
        return Err(FailureCode::RenderResource);
    }
    let reader = decoder.read_info().map_err(failure)?;
    let info = reader.info();
    let text = |key: &str| {
        info.uncompressed_latin1_text
            .iter()
            .find(|chunk| chunk.keyword == key)
            .map(|chunk| chunk.text.as_str())
    };
    let named = text("Thumb::URI").is_some_and(|uri| ThumbnailOf::uri_digest(uri) == of.uri_sha256);
    let current = text("Thumb::MTime")
        .is_some_and(|recorded| mtime_matches(recorded, of.mtime_seconds, of.mtime_nanos));
    if named && current {
        Ok(())
    } else {
        Err(FailureCode::Changed)
    }
}

/// The failure a PNG decoder error is: a limit is a resource, anything else
/// a parse failure.
fn failure(error: png::DecodingError) -> FailureCode {
    match error {
        png::DecodingError::LimitsExceeded => FailureCode::RenderResource,
        _ => FailureCode::RenderParse,
    }
}

/// Whether a recorded `Thumb::MTime` is the video's modification time,
/// `seconds` and `nanos`. The standard writes whole seconds; tumbler adds a
/// fraction (`1696300000.123456`). Either is the video's time when it agrees
/// with the seconds and nanoseconds to the recorded precision, truncated or
/// rounded, a rounding that carries into the next second included. Anything
/// else (another time, signs, exponents, spaces, more than nine fraction
/// digits, a fraction before 1970, where the sign makes it ambiguous) is
/// stale.
fn mtime_matches(recorded: &str, seconds: i64, nanos: u32) -> bool {
    let nanos = i64::from(nanos);
    let Some((whole, fraction)) = recorded.split_once('.') else {
        return recorded == seconds.to_string();
    };
    if seconds < 0
        || fraction.is_empty()
        || fraction.len() > 9
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return false;
    }
    let (Ok(recorded_seconds), Ok(value)) = (whole.parse::<i64>(), fraction.parse::<i64>()) else {
        return false;
    };
    // Exactly the digits an integer prints: no sign, no leading zero.
    if whole != recorded_seconds.to_string() {
        return false;
    }
    // The fraction's unit, in nanoseconds: 100_000_000 for one digit.
    let digits = fraction.len() as u32;
    let unit = 10i64.pow(9 - digits);
    let truncated = (seconds, nanos / unit);
    // A rounding that carries past the largest second matches nothing.
    let rounded = match (nanos + unit / 2) / unit {
        carry if carry == 10i64.pow(digits) => seconds.checked_add(1).map(|next| (next, 0)),
        rounded => Some((seconds, rounded)),
    };
    (recorded_seconds, value) == truncated || Some((recorded_seconds, value)) == rounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_media::{Canvas, JobKind, Source, Target, Theme};

    const URI: &str = "file:///home/user/Videos/clip%20one.mp4";
    const SECONDS: i64 = 1_696_300_000;
    const NANOS: u32 = 123_456_789;

    fn of() -> ThumbnailOf {
        ThumbnailOf {
            uri_sha256: ThumbnailOf::uri_digest(URI),
            mtime_seconds: SECONDS,
            mtime_nanos: NANOS,
        }
    }

    /// A `width` x 1 PNG with `text` before its pixels, and `late` after
    /// them.
    fn thumbnail(width: u32, text: &[(&str, &str)], late: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            for &(key, value) in text {
                encoder.add_text_chunk(key.into(), value.into()).unwrap();
            }
            let mut writer = encoder.write_header().unwrap();
            let pixels: Vec<u8> = (0..width).flat_map(|_| [10, 200, 30, 255]).collect();
            writer.write_image_data(&pixels).unwrap();
            for &(key, value) in late {
                writer
                    .write_text_chunk(&png::text_metadata::TEXtChunk::new(key, value))
                    .unwrap();
            }
        }
        bytes
    }

    fn current(seconds: &str) -> Vec<u8> {
        thumbnail(2, &[("Thumb::URI", URI), ("Thumb::MTime", seconds)], &[])
    }

    fn job(bytes: Vec<u8>, of: ThumbnailOf) -> Job {
        Job {
            kind: JobKind::CachedThumbnail(of),
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
                width: 256,
                height: 160,
                scale: 1.0,
                crop: None,
            },
            fallback_fonts: vec![],
        }
    }

    fn rendered(bytes: Vec<u8>) -> Result<Rendered, FailureCode> {
        let job = job(bytes, of());
        render(&job, of())
    }

    /// A thumbnail naming the video and its time renders as the raster it
    /// is, at its own size.
    #[test]
    fn a_thumbnail_of_the_video_renders() {
        for recorded in ["1696300000", "1696300000.123456"] {
            let rendered = rendered(current(recorded)).unwrap();
            assert_eq!((rendered.width, rendered.height), (2, 1), "{recorded}");
            assert_eq!(&rendered.rgba[..4], &[10, 200, 30, 255]);
        }
    }

    /// Another video's thumbnail, or this one's from before it changed, is
    /// `Changed`; so is one that does not say, or says it only after its
    /// pixels.
    #[test]
    fn a_thumbnail_of_anything_else_is_changed() {
        for bytes in [
            current("1696299999"),
            current("1696300000.2"),
            thumbnail(
                2,
                &[
                    ("Thumb::URI", "file:///home/user/Videos/clip%20two.mp4"),
                    ("Thumb::MTime", "1696300000"),
                ],
                &[],
            ),
            thumbnail(2, &[("Thumb::MTime", "1696300000")], &[]),
            thumbnail(2, &[("Thumb::URI", URI)], &[]),
            thumbnail(2, &[], &[]),
            thumbnail(
                2,
                &[],
                &[("Thumb::URI", URI), ("Thumb::MTime", "1696300000")],
            ),
        ] {
            assert_eq!(rendered(bytes).unwrap_err(), FailureCode::Changed);
        }
    }

    /// Only a PNG is a cached thumbnail, and only one of an accepted size.
    #[test]
    fn only_a_png_of_an_accepted_size_is_a_thumbnail() {
        let mut jpeg = Vec::new();
        image::RgbImage::from_pixel(2, 1, image::Rgb([10, 200, 30]))
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        assert_eq!(rendered(jpeg).unwrap_err(), FailureCode::UnsupportedMedia);
        let wide = thumbnail(
            MAX_EDGE + 1,
            &[("Thumb::URI", URI), ("Thumb::MTime", "1696300000")],
            &[],
        );
        assert_eq!(rendered(wide).unwrap_err(), FailureCode::RenderResource);
        let widest = thumbnail(
            MAX_EDGE,
            &[("Thumb::URI", URI), ("Thumb::MTime", "1696300000")],
            &[],
        );
        assert!(rendered(widest).is_ok());
        let mut truncated = current("1696300000");
        truncated.truncate(PNG_SIGNATURE.len() + 10);
        assert_eq!(rendered(truncated).unwrap_err(), FailureCode::RenderParse);
        // Text past what reading the chunks may hold is a resource failure.
        let wordy = "x".repeat(MAX_HEADER_BYTES + 1);
        let long = thumbnail(
            2,
            &[
                ("Thumb::URI", URI),
                ("Thumb::MTime", "1696300000"),
                ("Comment", &wordy),
            ],
            &[],
        );
        assert_eq!(rendered(long).unwrap_err(), FailureCode::RenderResource);
        // A header for an image too large to hold is a resource failure,
        // however the decoder would have sized it.
        for (width, height) in [(1 << 30, 1 << 30), (MAX_EDGE, MAX_EDGE + 1)] {
            assert_eq!(
                rendered(header_only(width, height)).unwrap_err(),
                FailureCode::RenderResource,
                "{width}x{height}"
            );
        }
    }

    /// A PNG of the signature and an RGBA16 header for `width` x `height`,
    /// and nothing else.
    fn header_only(width: u32, height: u32) -> Vec<u8> {
        let mut ihdr = b"IHDR".to_vec();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[16, 6, 0, 0, 0]);
        let mut bytes = PNG_SIGNATURE.to_vec();
        bytes.extend_from_slice(&13_u32.to_be_bytes());
        bytes.extend_from_slice(&ihdr);
        bytes.extend_from_slice(&crc32(&ihdr).to_be_bytes());
        bytes
    }

    /// The CRC-32 a PNG chunk carries (ISO 3309), bit by bit.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
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

    /// A recorded `Thumb::MTime` is the video's time in whole seconds, or
    /// with a fraction (tumbler) that agrees with the nanoseconds to its own
    /// precision, truncated or rounded; nothing else matches.
    #[test]
    fn a_fractional_thumbnail_mtime_matches_the_same_video() {
        for recorded in [
            "1696300000",
            "1696300000.1",
            "1696300000.12",
            "1696300000.123456",
            "1696300000.123457",
            "1696300000.123456789",
        ] {
            assert!(mtime_matches(recorded, SECONDS, NANOS), "{recorded}");
        }
        for recorded in [
            "1696299999",
            "1696300001",
            "1696300000.2",
            "1696300000.123458",
            "1696300000.1234567890",
            "1696300000.",
            ".123",
            "+1696300000",
            "1696300000.12e3",
            " 1696300000",
            "1696300000.12 ",
            "01696300000",
            "1696300000.-1",
            "-1696300000.1",
        ] {
            assert!(!mtime_matches(recorded, SECONDS, NANOS), "{recorded}");
        }
        // Rounding may carry into the next second.
        assert!(mtime_matches("1696300001.000000", SECONDS, 999_999_600));
        assert!(mtime_matches("1696300000.999999", SECONDS, 999_999_600));
        assert!(!mtime_matches("1696300001.000000", SECONDS, 999_999_000));
        // Before 1970 the sign makes a fraction ambiguous: whole seconds only.
        assert!(mtime_matches("-1", -1, 250_000_000));
        assert!(!mtime_matches("-1.250000", -1, 250_000_000));
        // A rounding past the largest second matches nothing, and wraps to
        // nothing; truncation still matches.
        assert!(!mtime_matches(
            "-9223372036854775808.0",
            i64::MAX,
            999_999_999
        ));
        assert!(!mtime_matches(
            "9223372036854775807.0",
            i64::MAX,
            999_999_999
        ));
        assert!(mtime_matches(
            "9223372036854775807.9",
            i64::MAX,
            999_999_999
        ));
        assert!(mtime_matches("-9223372036854775808", i64::MIN, 0));
    }
}
