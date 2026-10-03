//! Raster jobs: PNG, JPEG, WebP, BMP and the first frame of a GIF.
//!
//! The format comes from the content, never a name. The decoder reports the
//! image's dimensions and decoded size before any pixel is decoded, and both
//! are checked against the decoded caps first (codec limits are set too, but
//! they are best effort). The image is fitted inside the target box keeping
//! its aspect ratio, resampled with premultiplied alpha so transparent pixels
//! do not bleed color, and returned as straight RGBA whose fully transparent
//! pixels are all zero. A crop, in target-box coordinates around the centered
//! image, returns just that region, transparent outside the image.
//!
//! The canvas is the GUI's to draw behind the result, and the scale is for
//! vector content: a raster target is already in device pixels.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{
    DynamicImage, ImageDecoder as _, ImageError, ImageFormat, ImageReader, Limits, RgbaImage,
};
use kettle_media::{
    Crop, FailureCode, Job, MAX_DECODED_BYTES, MAX_DECODED_EDGE, MAX_RENDERED_BYTES,
    MAX_RENDERED_EDGE, Rendered, Target, content_digest, rgba_len,
};

use crate::source;

pub(crate) fn render(job: &Job) -> Result<Rendered, FailureCode> {
    let snapshot = source::load(&job.source, job.kind.input_cap())?;
    let image = decode(&snapshot.bytes)?;
    let (width, height, rgba) = fit(&image, job.target)?;
    let digest =
        content_digest(&snapshot.bytes, snapshot.identity).map_err(|_| FailureCode::BadParams)?;
    let rendered = Rendered {
        width,
        height,
        rgba,
        digest,
        source_text: Vec::new(),
        fence_sources: Vec::new(),
        fence_count: 0,
        fence_index: None,
        uncovered_scripts: Vec::new(),
        warnings: Vec::new(),
    };
    rendered
        .validate()
        .map_err(|_| FailureCode::RenderResource)?;
    Ok(rendered)
}

/// The formats a raster job may hold.
fn supported(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::WebP
            | ImageFormat::Bmp
            | ImageFormat::Gif
    )
}

/// Decode `bytes` to RGBA, checking the dimensions and the decoded size the
/// decoder reports before it decodes a pixel. A GIF yields its first frame.
pub(crate) fn decode(bytes: &[u8]) -> Result<RgbaImage, FailureCode> {
    let format = image::guess_format(bytes).map_err(|_| FailureCode::UnsupportedMedia)?;
    if !supported(format) {
        return Err(FailureCode::UnsupportedMedia);
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODED_EDGE);
    limits.max_image_height = Some(MAX_DECODED_EDGE);
    limits.max_alloc = u64::try_from(MAX_DECODED_BYTES).ok();
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(decode_failure)?;
    let (width, height) = decoder.dimensions();
    // The RGBA the result will need, and the decoder's own native size (a
    // 16-bit image needs more), both within the cap before decoding.
    rgba_len(width, height, MAX_DECODED_EDGE, MAX_DECODED_BYTES)
        .map_err(|_| FailureCode::RenderResource)?;
    if decoder.total_bytes() > u64::try_from(MAX_DECODED_BYTES).unwrap_or(u64::MAX) {
        return Err(FailureCode::RenderResource);
    }
    let image = DynamicImage::from_decoder(decoder).map_err(decode_failure)?;
    Ok(image.into_rgba8())
}

fn decode_failure(error: ImageError) -> FailureCode {
    match error {
        ImageError::Limits(_) => FailureCode::RenderResource,
        ImageError::Unsupported(_) => FailureCode::UnsupportedMedia,
        _ => FailureCode::RenderParse,
    }
}

/// Fit `image` inside `target`, then apply its crop: the width, height and
/// straight RGBA to return.
pub(crate) fn fit(image: &RgbaImage, target: Target) -> Result<(u32, u32, Vec<u8>), FailureCode> {
    let (width, height) = image.dimensions();
    let scale = f64::min(
        f64::from(target.width) / f64::from(width),
        f64::from(target.height) / f64::from(height),
    );
    let fitted_width = scaled(width, scale, target.width);
    let fitted_height = scaled(height, scale, target.height);
    let mut fitted = if (fitted_width, fitted_height) == (width, height) {
        image.clone()
    } else {
        resample(image, fitted_width, fitted_height)
    };
    clear_transparent(&mut fitted);
    match target.crop {
        None => Ok((fitted_width, fitted_height, fitted.into_raw())),
        Some(crop) => crop_in_box(&fitted, target, crop),
    }
}

/// `edge * scale`, rounded, kept within 1..=`limit`.
fn scaled(edge: u32, scale: f64, limit: u32) -> u32 {
    let value = (f64::from(edge) * scale).round();
    if value < 1.0 {
        1
    } else if value >= f64::from(limit) {
        limit
    } else {
        // In range: 1 <= value < limit <= u32::MAX.
        value as u32
    }
}

/// Resample with premultiplied alpha, so a transparent pixel's hidden color
/// does not bleed into its neighbours.
fn resample(image: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    let mut premultiplied = image.clone();
    for pixel in premultiplied.pixels_mut() {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
    let mut resized = image::imageops::resize(&premultiplied, width, height, FilterType::Triangle);
    for pixel in resized.pixels_mut() {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 {
            continue;
        }
        for channel in &mut pixel.0[..3] {
            *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
    resized
}

/// A fully transparent pixel is all zero: no hidden color reaches the GUI.
fn clear_transparent(image: &mut RgbaImage) {
    for pixel in image.pixels_mut() {
        if pixel[3] == 0 {
            pixel.0 = [0; 4];
        }
    }
}

/// The `crop` region of the target box, with the fitted image centered in the
/// box and transparent everywhere outside it.
fn crop_in_box(
    fitted: &RgbaImage,
    target: Target,
    crop: Crop,
) -> Result<(u32, u32, Vec<u8>), FailureCode> {
    let len = rgba_len(
        crop.width,
        crop.height,
        MAX_RENDERED_EDGE,
        MAX_RENDERED_BYTES,
    )
    .map_err(|_| FailureCode::BadParams)?;
    let mut out = vec![0; len];
    let left = (target.width - fitted.width()) / 2;
    let top = (target.height - fitted.height()) / 2;
    for row in 0..crop.height {
        let box_y = crop.y + row;
        let Some(image_y) = box_y.checked_sub(top).filter(|&y| y < fitted.height()) else {
            continue;
        };
        for column in 0..crop.width {
            let box_x = crop.x + column;
            let Some(image_x) = box_x.checked_sub(left).filter(|&x| x < fitted.width()) else {
                continue;
            };
            let at = (usize::try_from(row).map_err(|_| FailureCode::BadParams)?
                * usize::try_from(crop.width).map_err(|_| FailureCode::BadParams)?
                + usize::try_from(column).map_err(|_| FailureCode::BadParams)?)
                * 4;
            out[at..at + 4].copy_from_slice(&fitted.get_pixel(image_x, image_y).0);
        }
    }
    Ok((crop.width, crop.height, out))
}
