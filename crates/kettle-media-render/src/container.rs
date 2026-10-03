//! What a raster container declares, read from the bytes before any decoder
//! sees them.
//!
//! Two decoders refuse an oversized header with a decoding error of their own
//! rather than a size limit (BMP past 65535 pixels, a WebP canvas whose pixel
//! count overflows), and the WebP decoder allocates a lossy frame at the size
//! its bitstream declares before comparing that with the canvas or animation
//! frame it belongs to, or, for a frame with an alpha chunk, never compares
//! them. So an oversized BMP or WebP is `RenderResource` here, and a WebP
//! whose lossy frame does not match the size it fills is `RenderParse`, before
//! it is decoded. Anything these checks cannot read is left to the decoder,
//! which refuses it.

use image::ImageFormat;
use kettle_media::{FailureCode, MAX_DECODED_BYTES, MAX_DECODED_EDGE};

/// Refuse `bytes` if its header declares an image past the decoded caps, or a
/// WebP lossy frame whose own size differs from the size it must fill.
pub(crate) fn check(format: ImageFormat, bytes: &[u8]) -> Result<(), FailureCode> {
    match format {
        ImageFormat::Bmp => bmp_size(bytes).map_or(Ok(()), within_caps),
        ImageFormat::WebP => webp(bytes),
        _ => Ok(()),
    }
}

fn within_caps((width, height): (u64, u64)) -> Result<(), FailureCode> {
    let edge = u64::from(MAX_DECODED_EDGE);
    let cap = u64::try_from(MAX_DECODED_BYTES).unwrap_or(u64::MAX);
    if width > edge || height > edge || width.saturating_mul(height).saturating_mul(4) > cap {
        return Err(FailureCode::RenderResource);
    }
    Ok(())
}

/// The size a BMP header declares, for the header sizes the decoder reads: a
/// core header's unsigned 16-bit fields, or an info header's signed 32-bit
/// ones, where a negative height is a top-down image. A negative width is
/// malformed, and the decoder's to refuse.
fn bmp_size(bytes: &[u8]) -> Option<(u64, u64)> {
    if bytes.get(..2)? != b"BM" {
        return None;
    }
    match u32_le(bytes, 14)? {
        12 => Some((u16_le(bytes, 18)?.into(), u16_le(bytes, 20)?.into())),
        40 | 52 | 56 | 108 | 124 => {
            let width = u64::try_from(i32_le(bytes, 18)?).ok()?;
            Some((width, i32_le(bytes, 22)?.unsigned_abs().into()))
        }
        _ => None,
    }
}

/// A simple WebP holds one bitstream whose header the decoder reads for the
/// image's size, so it needs nothing here. An extended one (`VP8X`) declares
/// its canvas, then holds a still bitstream or animation frames; the chunks
/// are walked exactly as the decoder walks them.
fn webp(bytes: &[u8]) -> Result<(), FailureCode> {
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
        return Ok(());
    }
    let (Some(riff_size), Some((b"VP8X", vp8x_size))) = (u32_le(bytes, 4), chunk(bytes, 12)) else {
        return Ok(());
    };
    // Flags, three reserved bytes, then the canvas size less one, 24 bits each.
    let (Some(&flags), Some(width), Some(height)) =
        (bytes.get(20), u24_le(bytes, 24), u24_le(bytes, 27))
    else {
        return Ok(());
    };
    let canvas = (u64::from(width) + 1, u64::from(height) + 1);
    within_caps(canvas)?;
    let animated = flags & 0b10 != 0;
    let mut position = 20 + rounded(vp8x_size);
    let end = position + u64::from(riff_size.saturating_sub(12));
    while position < end {
        let Some((fourcc, size)) = chunk(bytes, position) else {
            break;
        };
        let payload = position + 8;
        position = payload + rounded(size);
        match fourcc {
            // A still image's bitstream fills the canvas. The decoder reads
            // the first; every one is checked.
            b"VP8 " if !animated => matches(vp8_size(bytes, payload), canvas)?,
            // The decoder renders the first frame; every one is checked.
            b"ANMF" => frame(bytes, payload)?,
            _ => {}
        }
    }
    Ok(())
}

/// An animation frame: offsets and size less one (24 bits each), duration
/// and flags, then its bitstream. A lossy frame with alpha is an `ALPH` chunk
/// and then the chunk after it, which the decoder decodes as VP8 whatever it
/// is named, so it must be VP8 here.
fn frame(bytes: &[u8], payload: u64) -> Result<(), FailureCode> {
    let (Some(width), Some(height)) = (u24_le(bytes, payload + 6), u24_le(bytes, payload + 9))
    else {
        return Ok(());
    };
    let size = (u64::from(width) + 1, u64::from(height) + 1);
    match chunk(bytes, payload + 16) {
        Some((b"VP8 ", _)) => matches(vp8_size(bytes, payload + 24), size),
        Some((b"ALPH", alpha_size)) => {
            let next = payload + 24 + rounded(alpha_size);
            match chunk(bytes, next) {
                Some((b"VP8 ", _)) => matches(vp8_size(bytes, next + 8), size),
                _ => Err(FailureCode::RenderParse),
            }
        }
        _ => Ok(()),
    }
}

/// A lossy bitstream that declares another size than it must fill. One whose
/// header cannot be read declares none, and the decoder allocates nothing for
/// it.
fn matches(declared: Option<(u64, u64)>, size: (u64, u64)) -> Result<(), FailureCode> {
    match declared {
        Some(declared) if declared != size => Err(FailureCode::RenderParse),
        _ => Ok(()),
    }
}

/// A VP8 key frame's size: a three-byte frame tag (bit 0 clear), the start
/// code, then width and height in the low 14 bits of two 16-bit fields.
fn vp8_size(bytes: &[u8], at: u64) -> Option<(u64, u64)> {
    let header = slice(bytes, at, 10)?;
    if header[0] & 1 != 0 || header[3..6] != [0x9d, 0x01, 0x2a] {
        return None;
    }
    let width = u16::from_le_bytes([header[6], header[7]]) & 0x3fff;
    let height = u16::from_le_bytes([header[8], header[9]]) & 0x3fff;
    Some((width.into(), height.into()))
}

/// A RIFF chunk header at `at`: its four-character code and payload size.
fn chunk(bytes: &[u8], at: u64) -> Option<(&[u8; 4], u32)> {
    let fourcc = slice(bytes, at, 4)?.try_into().ok()?;
    Some((fourcc, u32_le(bytes, at.checked_add(4)?)?))
}

/// A chunk's payload is padded to an even length.
fn rounded(size: u32) -> u64 {
    u64::from(size.saturating_add(size & 1))
}

/// `len` bytes at `at`, if the data holds them.
fn slice(bytes: &[u8], at: u64, len: usize) -> Option<&[u8]> {
    let at = usize::try_from(at).ok()?;
    bytes.get(at..at.checked_add(len)?)
}

fn u16_le(bytes: &[u8], at: u64) -> Option<u16> {
    Some(u16::from_le_bytes(slice(bytes, at, 2)?.try_into().ok()?))
}

fn u24_le(bytes: &[u8], at: u64) -> Option<u32> {
    let b = slice(bytes, at, 3)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], 0]))
}

fn u32_le(bytes: &[u8], at: u64) -> Option<u32> {
    Some(u32::from_le_bytes(slice(bytes, at, 4)?.try_into().ok()?))
}

fn i32_le(bytes: &[u8], at: u64) -> Option<i32> {
    Some(i32::from_le_bytes(slice(bytes, at, 4)?.try_into().ok()?))
}
