//! Raster jobs: PNG, JPEG, WebP, BMP and the first frame of a GIF.
//!
//! The format comes from the content, never a name. The container's own
//! header is checked first where a decoder would not check it soundly
//! (`container`), then the decoder reports the image's dimensions and decoded
//! size before any pixel is decoded, and both are checked against the decoded
//! caps (codec limits are set too, but they are best effort). The image is
//! fitted inside the target box keeping its aspect ratio, resampled with
//! premultiplied alpha so transparent pixels do not bleed color, and returned
//! as straight RGBA whose fully transparent pixels are all zero. Resampling
//! holds a few rows at a time beside the result, never a full-size
//! intermediate. A crop, in target-box coordinates around the centered image,
//! returns just that region, transparent outside the image.
//!
//! The canvas is the GUI's to draw behind the result, and the scale is for
//! vector content: a raster target is already in device pixels.

use std::collections::VecDeque;
use std::io::Cursor;

use image::{
    DynamicImage, ImageDecoder as _, ImageError, ImageFormat, ImageReader, Limits, RgbaImage,
};
use kettle_media::{
    Crop, FailureCode, Job, MAX_DECODED_BYTES, MAX_DECODED_EDGE, MAX_RENDERED_BYTES,
    MAX_RENDERED_EDGE, Rendered, Target, content_digest, rgba_len,
};

use crate::{container, source};

pub(crate) fn render(job: &Job) -> Result<Rendered, FailureCode> {
    let snapshot = source::load(&job.source, job.kind.input_cap())?;
    render_loaded(job, &snapshot)
}

/// Render a snapshot already loaded within the raster input cap.
pub(crate) fn render_loaded(
    job: &Job,
    snapshot: &source::Snapshot<'_>,
) -> Result<Rendered, FailureCode> {
    let image = crate::guarded(FailureCode::RenderParse, || decode(&snapshot.bytes))?;
    let (width, height, rgba) = fit(image, job.target)?;
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
    container::check(format, bytes)?;
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

/// Fit `image` inside `target`, never past its own size, then apply its crop:
/// the width, height and straight RGBA to return. Enlarging would add no
/// detail, only pixels to hold, and would report a size the image does not
/// have; whoever paints the result scales it as it needs.
pub(crate) fn fit(image: RgbaImage, target: Target) -> Result<(u32, u32, Vec<u8>), FailureCode> {
    let (width, height) = image.dimensions();
    let scale = f64::min(
        f64::from(target.width) / f64::from(width),
        f64::from(target.height) / f64::from(height),
    )
    .min(1.0);
    let fitted_width = scaled(width, scale, target.width);
    let fitted_height = scaled(height, scale, target.height);
    let mut fitted = if (fitted_width, fitted_height) == (width, height) {
        image
    } else {
        resample(&image, fitted_width, fitted_height)?
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

/// One output pixel's inputs along one axis: the first input index and each
/// input's weight, summing to 1.
struct Taps {
    start: usize,
    weights: Vec<f32>,
}

/// The triangle filter's taps from `from` inputs to `to` outputs, widened by
/// the ratio when shrinking so every input counts.
fn taps(from: u32, to: u32) -> Vec<Taps> {
    let ratio = f64::from(from) / f64::from(to);
    let support = ratio.max(1.0);
    (0..to)
        .map(|out| {
            let center = (f64::from(out) + 0.5) * ratio;
            // Both ends lie in 0..=from, which fits a usize.
            let first = (center - support).floor().max(0.0) as usize;
            let last = ((center + support).ceil().min(f64::from(from)) as usize).max(first + 1);
            let mut weights: Vec<f32> = (first..last)
                .map(|input| {
                    let distance = (input as f64 + 0.5 - center) / support;
                    (1.0 - distance.abs()).max(0.0) as f32
                })
                .collect();
            let sum: f32 = weights.iter().sum();
            if sum > 0.0 {
                weights.iter_mut().for_each(|weight| *weight /= sum);
            } else {
                weights.iter_mut().for_each(|weight| *weight = 0.0);
                weights[0] = 1.0;
            }
            Taps {
                start: first,
                weights,
            }
        })
        .collect()
}

/// Resample to `width` by `height` with premultiplied alpha, so a transparent
/// pixel's hidden color does not bleed into its neighbours. Each input row is
/// filtered across once and kept only while an output row still needs it:
/// beside the result, memory holds those rows, never a full-size
/// intermediate.
fn resample(image: &RgbaImage, width: u32, height: u32) -> Result<RgbaImage, FailureCode> {
    let len = rgba_len(width, height, MAX_RENDERED_EDGE, MAX_RENDERED_BYTES)
        .map_err(|_| FailureCode::RenderResource)?;
    let columns = taps(image.width(), width);
    let rows = taps(image.height(), height);
    let stride = usize::try_from(image.width()).map_err(|_| FailureCode::RenderResource)? * 4;
    let source = image.as_raw();
    let mut out = vec![0; len];
    // Premultiplied, filtered rows, oldest first, with the first one's index.
    let mut cached: VecDeque<Vec<[f32; 4]>> = VecDeque::new();
    let mut first_cached = 0;
    let mut spare: Vec<Vec<[f32; 4]>> = Vec::new();
    let mut line = vec![[0.0f32; 4]; columns.len()];
    for (row, out_row) in rows.iter().zip(out.chunks_exact_mut(columns.len() * 4)) {
        while first_cached < row.start && !cached.is_empty() {
            spare.extend(cached.pop_front());
            first_cached += 1;
        }
        if cached.is_empty() {
            first_cached = row.start;
        }
        while first_cached + cached.len() < row.start + row.weights.len() {
            let input = first_cached + cached.len();
            let mut filtered = spare.pop().unwrap_or_default();
            filter_row(&source[input * stride..][..stride], &columns, &mut filtered);
            cached.push_back(filtered);
        }
        line.iter_mut().for_each(|pixel| *pixel = [0.0; 4]);
        for (filtered, &weight) in cached
            .iter()
            .skip(row.start - first_cached)
            .zip(&row.weights)
        {
            for (sum, pixel) in line.iter_mut().zip(filtered) {
                for (sum, value) in sum.iter_mut().zip(pixel) {
                    *sum += value * weight;
                }
            }
        }
        for (pixel, out) in line.iter().zip(out_row.as_chunks_mut::<4>().0) {
            let alpha = pixel[3].round().clamp(0.0, 255.0);
            if alpha == 0.0 {
                continue;
            }
            for (channel, value) in out[..3].iter_mut().zip(pixel) {
                // Rounded and clamped into 0..=255 first.
                *channel = (value / pixel[3]).round().clamp(0.0, 255.0) as u8;
            }
            out[3] = alpha as u8;
        }
    }
    RgbaImage::from_raw(width, height, out).ok_or(FailureCode::RenderResource)
}

/// One input row of straight RGBA, filtered across to the output width as
/// premultiplied color and alpha.
fn filter_row(input: &[u8], columns: &[Taps], filtered: &mut Vec<[f32; 4]>) {
    filtered.clear();
    filtered.extend(columns.iter().map(|column| {
        let mut sum = [0.0f32; 4];
        let pixels = input[column.start * 4..].as_chunks::<4>().0;
        for (pixel, &weight) in pixels.iter().zip(&column.weights) {
            let alpha = f32::from(pixel[3]) * weight;
            sum[0] += f32::from(pixel[0]) * alpha;
            sum[1] += f32::from(pixel[1]) * alpha;
            sum[2] += f32::from(pixel[2]) * alpha;
            sum[3] += alpha;
        }
        sum
    }));
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
