//! Emoji images rasterized at exactly the size their cell draws them.
//!
//! A grid cell fits an emoji cluster to its columns (`fonts::fit_emoji_pixels`),
//! which is rarely the font size the row was shaped at. Stretching the
//! shaped-size image on the GPU duplicates rows and columns, and that image is
//! poor to begin with: swash serves a colour bitmap face from its *nearest*
//! strike, resampled (20 ppem for a 13 px Apple Color Emoji glyph), which
//! flattens the top of the faces. So each fitted image is resampled here,
//! once per glyph and size, from a large source by area averaging, and drawn
//! 1:1.

use glyphon::cosmic_text::{CacheKey, FontSystem, SwashCache, SwashContent};

use super::glyphpipe::GlyphSlot;

/// The largest colour-bitmap strike used as a source, in pixels per em. A
/// face whose every strike is larger uses its smallest.
const MAX_SOURCE_PPEM: u16 = 256;

/// Source pixels per target pixel for an outline (or COLR) emoji, which is
/// drawn at that multiple of its target size and averaged down.
const OUTLINE_SUPERSAMPLE: f32 = 4.0;

/// The largest outline source, in pixels per em.
const MAX_OUTLINE_PPEM: f32 = 512.0;

/// A fitted emoji image: straight-alpha RGBA when `color`, else coverage.
#[derive(Debug)]
pub(crate) struct FittedRaster {
    pub(crate) color: bool,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) data: Vec<u8>,
}

/// The pixels-per-em to rasterize `key` at as the source of a `target_height`
/// image whose shaped-size image is `natural_height` pixels tall.
///
/// A colour bitmap face is read from its largest strike that holds the glyph
/// (at most [`MAX_SOURCE_PPEM`]), which swash returns unscaled. Anything else
/// is drawn at [`OUTLINE_SUPERSAMPLE`] times the target.
pub(crate) fn source_ppem(
    fonts: &mut FontSystem,
    key: CacheKey,
    natural_height: u32,
    target_height: u32,
) -> f32 {
    let font_size = f32::from_bits(key.font_size_bits);
    if let Some(font) = fonts.get_font(key.font_id, key.font_weight) {
        let face = font.as_swash();
        let mut sizes: Vec<u16> = face
            .color_strikes()
            .filter(|strike| strike.contains(key.glyph_id))
            .map(|strike| strike.ppem())
            .filter(|&ppem| ppem > 0)
            .collect();
        sizes.sort_unstable();
        if let Some(&largest) = sizes.iter().rev().find(|&&ppem| ppem <= MAX_SOURCE_PPEM) {
            return f32::from(largest);
        }
        if let Some(&smallest) = sizes.first() {
            return f32::from(smallest);
        }
    }
    let scale = target_height.max(1) as f32 / natural_height.max(1) as f32;
    (font_size * scale * OUTLINE_SUPERSAMPLE).clamp(font_size, MAX_OUTLINE_PPEM)
}

/// `key`'s image resampled to exactly `size` pixels, its shaped-size image
/// having been `natural`. `None` when the glyph has no image.
pub(crate) fn fitted_emoji_raster(
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    key: CacheKey,
    natural: GlyphSlot,
    size: [u32; 2],
) -> Option<FittedRaster> {
    let [width, height] = size;
    if width == 0 || height == 0 {
        return None;
    }
    let ppem = source_ppem(fonts, key, natural.h as u32, height);
    let (source, _, _) = CacheKey::new(
        key.font_id,
        key.glyph_id,
        ppem,
        (0.0, 0.0),
        key.font_weight,
        key.flags,
    );
    let image = swash.get_image_uncached(fonts, source)?;
    let channels = match image.content {
        SwashContent::Color => 4,
        SwashContent::Mask => 1,
        SwashContent::SubpixelMask => return None,
    };
    let (sw, sh) = (image.placement.width, image.placement.height);
    if sw == 0 || sh == 0 || image.data.len() != (sw * sh) as usize * channels {
        return None;
    }
    Some(FittedRaster {
        color: channels == 4,
        width,
        height,
        data: area_resample(&image.data, [sw, sh], channels, size),
    })
}

/// Resample a `channels`-channel (1 or 4) image of `from` pixels to `to`
/// pixels by area averaging: each target pixel is the mean of the source area
/// it covers, weighted by overlap. Four-channel images are straight-alpha
/// RGBA and are averaged premultiplied, so a transparent pixel's colour never
/// tints its neighbours.
pub(crate) fn area_resample(src: &[u8], from: [u32; 2], channels: usize, to: [u32; 2]) -> Vec<u8> {
    let ([sw, sh], [dw, dh]) = (from.map(|v| v as usize), to.map(|v| v as usize));
    if dw == 0 || dh == 0 {
        return Vec::new();
    }
    if sw == 0 || sh == 0 || src.len() != sw * sh * channels {
        return vec![0; dw * dh * channels];
    }
    let premultiplied: Vec<f32> = if channels == 4 {
        src.as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let a = f32::from(p[3]) / 255.0;
                [
                    f32::from(p[0]) * a,
                    f32::from(p[1]) * a,
                    f32::from(p[2]) * a,
                    f32::from(p[3]),
                ]
            })
            .collect()
    } else {
        src.iter().map(|&v| f32::from(v)).collect()
    };
    let columns = area_taps(sw, dw);
    let rows = area_taps(sh, dh);
    // Horizontal pass: sh rows of dw pixels.
    let mut across = vec![0f32; sh * dw * channels];
    for y in 0..sh {
        for (x, taps) in columns.iter().enumerate() {
            let out = &mut across[(y * dw + x) * channels..][..channels];
            for &(sx, weight) in taps {
                let input = &premultiplied[(y * sw + sx) * channels..][..channels];
                for c in 0..channels {
                    out[c] += input[c] * weight;
                }
            }
        }
    }
    let mut out = vec![0u8; dw * dh * channels];
    let mut pixel = vec![0f32; channels];
    for (y, taps) in rows.iter().enumerate() {
        for x in 0..dw {
            pixel.fill(0.0);
            for &(sy, weight) in taps {
                let input = &across[(sy * dw + x) * channels..][..channels];
                for c in 0..channels {
                    pixel[c] += input[c] * weight;
                }
            }
            let target = &mut out[(y * dw + x) * channels..][..channels];
            if channels == 4 {
                let alpha = pixel[3];
                if alpha > 0.0 {
                    let unmultiply = 255.0 / alpha;
                    for c in 0..3 {
                        target[c] = to_byte(pixel[c] * unmultiply);
                    }
                }
                target[3] = to_byte(alpha);
            } else {
                target[0] = to_byte(pixel[0]);
            }
        }
    }
    out
}

fn to_byte(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// For each of `to` target pixels along one axis, the source pixels it covers
/// out of `from` and their weights, which sum to one.
fn area_taps(from: usize, to: usize) -> Vec<Vec<(usize, f32)>> {
    let ratio = from as f64 / to as f64;
    (0..to)
        .map(|i| {
            let (start, end) = (i as f64 * ratio, (i + 1) as f64 * ratio);
            let first = start.floor() as usize;
            let last = (end.ceil() as usize).min(from);
            (first..last)
                .filter_map(|j| {
                    let overlap = end.min(j as f64 + 1.0) - start.max(j as f64);
                    (overlap > 0.0).then(|| (j, (overlap / (end - start)) as f32))
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_ratios_average_whole_blocks() {
        // 4x2 coverage down to 2x1: each target pixel is a 2x2 block mean.
        let src = [0, 40, 100, 200, 80, 120, 50, 250];
        assert_eq!(area_resample(&src, [4, 2], 1, [2, 1]), vec![60, 150]);
    }

    #[test]
    fn fractional_ratios_weight_by_overlap() {
        // Three pixels into two: the middle one is split evenly.
        let src = [0, 90, 255];
        // (0 * 1 + 90 * 0.5) / 1.5 = 30; (90 * 0.5 + 255 * 1) / 1.5 = 200.
        assert_eq!(area_resample(&src, [3, 1], 1, [2, 1]), vec![30, 200]);
    }

    #[test]
    fn upsampling_keeps_a_flat_image_flat_and_sized() {
        let src = vec![7u8; 2 * 3 * 4];
        let out = area_resample(&src, [2, 3], 4, [5, 7]);
        assert_eq!(out.len(), 5 * 7 * 4);
        assert!(out.iter().all(|&v| v == 7));
    }

    #[test]
    fn transparent_colour_never_tints_its_neighbours() {
        // Opaque red beside a fully transparent pixel holding green.
        let src = [255, 0, 0, 255, 0, 255, 0, 0];
        let out = area_resample(&src, [2, 1], 4, [1, 1]);
        assert_eq!(out, vec![255, 0, 0, 128]);
    }

    #[test]
    fn mismatched_input_draws_nothing_rather_than_reading_past_it() {
        assert_eq!(area_resample(&[1, 2, 3], [2, 2], 1, [1, 1]), vec![0]);
        assert!(area_resample(&[1, 2, 3, 4], [2, 2], 1, [0, 1]).is_empty());
    }
}
