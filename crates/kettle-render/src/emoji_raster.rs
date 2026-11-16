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
use read_fonts::TableProvider;

use super::glyphpipe::GlyphSlot;

/// The largest colour-bitmap strike used as a source, in pixels per em. A
/// glyph whose every strike is larger keeps its shaped-size image instead.
pub(crate) const MAX_SOURCE_PPEM: u16 = 256;

/// The most pixels a source image may have, whatever its strike or outline
/// size claimed: resampling holds four `f32`s per source pixel.
pub(crate) const MAX_SOURCE_PIXELS: u64 = 1 << 20;

/// Source pixels per target pixel for an outline (or COLR) emoji, which is
/// drawn at that multiple of its target size and averaged down.
const OUTLINE_SUPERSAMPLE: f32 = 4.0;

/// The largest outline source, in pixels per em.
pub(crate) const MAX_OUTLINE_PPEM: f32 = 512.0;

/// The encoding at the atlas boundary, matching `RasterGlyph::from_swash`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlphaEncoding {
    Straight,
    Premultiplied,
}

impl AlphaEncoding {
    pub(crate) fn of(image: &glyphon::cosmic_text::SwashImage) -> Self {
        if matches!(image.source, swash::scale::Source::ColorOutline(_)) {
            Self::Premultiplied
        } else {
            Self::Straight
        }
    }
}

/// A fitted image in its source's RGBA alpha encoding, or mask coverage.
#[derive(Debug)]
pub(crate) struct FittedRaster {
    pub(crate) color: bool,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) data: Vec<u8>,
}

/// The pixels-per-em to rasterize `key` at as the source of a `target_height`
/// image whose shaped-size image is `natural_height` pixels tall, or `None`
/// when no bounded source exists.
///
/// A colour bitmap face is read from its largest strike that holds the glyph
/// and is at most [`MAX_SOURCE_PPEM`], which swash returns unscaled; a glyph
/// whose strikes are all larger has no bounded source. Anything else is drawn
/// as an outline (see [`outline_source_ppem`]).
pub(crate) fn source_ppem(
    fonts: &mut FontSystem,
    key: CacheKey,
    natural_height: u32,
    target_height: u32,
) -> Option<f32> {
    let font_size = f32::from_bits(key.font_size_bits);
    let index = fonts.db().face(key.font_id)?.index;
    if let Some(font) = fonts.get_font(key.font_id, key.font_weight) {
        let strikes: Vec<_> = font
            .as_swash()
            .color_strikes()
            .enumerate()
            .filter(|(_, strike)| strike.contains(key.glyph_id))
            .map(|(index, strike)| (index, strike.ppem()))
            .collect();
        if !strikes.is_empty() {
            let sizes: Vec<_> = strikes.iter().map(|&(_, ppem)| ppem).collect();
            let ppem = pick_strike(&sizes)?;
            // BestFit takes the first strike at least as large as requested.
            // Refuse unsorted strikes that would select a different source.
            let &(strike, actual) = strikes.iter().find(|&&(_, size)| size >= ppem)?;
            if actual != ppem {
                return None;
            }
            let font = read_fonts::FontRef::from_index(font.data(), index).ok()?;
            let dimensions = bitmap_dimensions(&font, strike, key.glyph_id)?;
            return bounded_bitmap_ppem(ppem, dimensions).map(f32::from);
        }
    }
    Some(outline_source_ppem(
        font_size,
        natural_height,
        target_height,
    ))
}

fn bounded_bitmap_ppem(ppem: u16, [width, height]: [u32; 2]) -> Option<u16> {
    (width > 0 && height > 0 && u64::from(width) * u64::from(height) <= MAX_SOURCE_PIXELS)
        .then_some(ppem)
}

// Swash's BitmapStrike::get is private. Read the same headers with the font
// parser we already use, without creating or decoding an image.
fn bitmap_dimensions(
    font: &read_fonts::FontRef<'_>,
    strike: usize,
    glyph: u16,
) -> Option<[u32; 2]> {
    use read_fonts::types::{GlyphId, Tag};

    let mut glyph = GlyphId::new(u32::from(glyph));
    if let Ok(sbix) = font.sbix() {
        let strike = sbix.strikes().get(strike).ok()?;
        // Swash follows at most one sbix duplicate reference.
        for _ in 0..2 {
            let data = strike.glyph_data(glyph).ok()??;
            match data.graphic_type() {
                tag if tag == Tag::new(b"png ") => return png_dimensions(data.data()),
                tag if tag == Tag::new(b"dupe") => {
                    glyph = GlyphId::new(u32::from(u16::from_be_bytes(
                        data.data().get(..2)?.try_into().ok()?,
                    )));
                }
                _ => return None,
            }
        }
        return None;
    }
    let cblc = font.cblc().ok()?;
    let cbdt = font.cbdt().ok()?;
    cbdt_dimensions(&cblc, &cbdt, strike, glyph)
}

fn cbdt_dimensions(
    cblc: &read_fonts::tables::cblc::Cblc<'_>,
    cbdt: &read_fonts::tables::cbdt::Cbdt<'_>,
    strike: usize,
    glyph: read_fonts::types::GlyphId,
) -> Option<[u32; 2]> {
    use read_fonts::tables::bitmap::{BitmapContent, BitmapDataFormat, BitmapMetrics};

    let size = cblc.bitmap_sizes().get(strike)?;
    let list = size.index_subtable_list(cblc.offset_data()).ok()?;
    let record = list
        .index_subtable_records()
        .iter()
        .find(|record| (record.first_glyph_index()..=record.last_glyph_index()).contains(&glyph))?;
    let subtable = record.index_subtable(list.offset_data()).ok()?;
    // Swash and read-fonts disagree on sparse format-4 offsets. Refuse it
    // rather than preflight a different record from the one Swash decodes.
    // Swash does not support format 5 either.
    if !matches!(subtable.index_format(), 1..=3) {
        return None;
    }
    let location = size.location(cblc.offset_data(), glyph).ok()?;
    let data = cbdt.data(&location).ok()?;
    match data.content {
        BitmapContent::Data(BitmapDataFormat::Png, bytes) => png_dimensions(bytes),
        BitmapContent::Data(_, _) => match data.metrics {
            BitmapMetrics::Small(metrics) => {
                Some([u32::from(metrics.width), u32::from(metrics.height)])
            }
            BitmapMetrics::Big(metrics) => {
                Some([u32::from(metrics.width), u32::from(metrics.height)])
            }
        },
        BitmapContent::Composite(_) => None,
    }
}

fn png_dimensions(bytes: &[u8]) -> Option<[u32; 2]> {
    if bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    Some([
        u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?),
        u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?),
    ])
}

/// The largest of a glyph's colour-bitmap strike sizes that is at most
/// [`MAX_SOURCE_PPEM`], or `None` when every one is larger.
pub(crate) fn pick_strike(sizes: &[u16]) -> Option<u16> {
    sizes
        .iter()
        .copied()
        .filter(|&ppem| ppem > 0 && ppem <= MAX_SOURCE_PPEM)
        .max()
}

/// The size to draw an outline (or COLR) glyph at as the source of a
/// `target_height` image: [`OUTLINE_SUPERSAMPLE`] times the target, at least
/// the shaped `font_size` and never more than [`MAX_OUTLINE_PPEM`], even when
/// the font size alone exceeds it.
pub(crate) fn outline_source_ppem(font_size: f32, natural_height: u32, target_height: u32) -> f32 {
    let scale = target_height.max(1) as f32 / natural_height.max(1) as f32;
    // The floor is bounded first, so the clamp's minimum is a number no
    // larger than its maximum (otherwise it panics), whatever the font size.
    let floor = if font_size.is_finite() {
        font_size.clamp(1.0, MAX_OUTLINE_PPEM)
    } else {
        MAX_OUTLINE_PPEM
    };
    let wanted = font_size * scale * OUTLINE_SUPERSAMPLE;
    if wanted.is_finite() {
        wanted.clamp(floor, MAX_OUTLINE_PPEM)
    } else {
        MAX_OUTLINE_PPEM
    }
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
    let ppem = source_ppem(fonts, key, natural.h as u32, height)?;
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
    if sw == 0
        || sh == 0
        || u64::from(sw) * u64::from(sh) > MAX_SOURCE_PIXELS
        || image.data.len() != (sw * sh) as usize * channels
    {
        return None;
    }
    Some(FittedRaster {
        color: channels == 4,
        width,
        height,
        data: area_resample(
            &image.data,
            [sw, sh],
            channels,
            size,
            AlphaEncoding::of(&image),
        ),
    })
}

/// Resample a `channels`-channel (1 or 4) image of `from` pixels to `to`
/// pixels by area averaging: each target pixel is the mean of the source area
/// it covers, weighted by overlap. RGBA is averaged as premultiplied linear
/// light and returned in the input alpha encoding. Swash COLR has premultiplied
/// encoded RGB, whereas PNG bitmaps have straight RGB. Keeping that encoding
/// matches the normal upload, which passes Swash bytes through unchanged.
/// One-channel coverage is already linear.
pub(crate) fn area_resample(
    src: &[u8],
    from: [u32; 2],
    channels: usize,
    to: [u32; 2],
    alpha_encoding: AlphaEncoding,
) -> Vec<u8> {
    let ([sw, sh], [dw, dh]) = (from.map(|v| v as usize), to.map(|v| v as usize));
    if dw == 0 || dh == 0 {
        return Vec::new();
    }
    if sw == 0 || sh == 0 || src.len() != sw * sh * channels {
        return vec![0; dw * dh * channels];
    }
    let premultiplied: Vec<f32> = if channels == 4 {
        let decode: [f32; 256] = std::array::from_fn(|v| srgb_to_linear(v as f32 / 255.0));
        src.as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let a = f32::from(p[3]) / 255.0;
                let linear = |channel: u8| match alpha_encoding {
                    AlphaEncoding::Straight => decode[usize::from(channel)] * a,
                    AlphaEncoding::Premultiplied if a > 0.0 => {
                        srgb_to_linear((f32::from(channel) / f32::from(p[3])).min(1.0)) * a
                    }
                    AlphaEncoding::Premultiplied => 0.0,
                };
                [linear(p[0]), linear(p[1]), linear(p[2]), f32::from(p[3])]
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
                        let encoded = linear_to_srgb(pixel[c] * unmultiply);
                        target[c] = to_byte(
                            encoded
                                * match alpha_encoding {
                                    AlphaEncoding::Straight => 255.0,
                                    AlphaEncoding::Premultiplied => alpha,
                                },
                        );
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

/// An sRGB-encoded channel in `0..=1` as linear light.
pub(crate) fn srgb_to_linear(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light in `0..=1` as an sRGB-encoded channel.
pub(crate) fn linear_to_srgb(linear: f32) -> f32 {
    let linear = linear.clamp(0.0, 1.0);
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
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
    fn cbdt_preflight_reads_the_same_record_as_swash_or_refuses_it() {
        use read_fonts::FontRead;
        use read_fonts::types::GlyphId;
        // One strike, glyph 1, index format 1, image format 17. No PNG body:
        // reading dimensions must use only the record and image headers.
        let mut cblc = vec![0u8; 80];
        cblc[..8].copy_from_slice(&[0, 3, 0, 0, 0, 0, 0, 1]);
        cblc[8..12].copy_from_slice(&56u32.to_be_bytes());
        cblc[12..16].copy_from_slice(&24u32.to_be_bytes());
        cblc[16..20].copy_from_slice(&1u32.to_be_bytes());
        cblc[48..56].copy_from_slice(&[0, 1, 0, 1, 160, 160, 32, 1]);
        cblc[56..64].copy_from_slice(&[0, 1, 0, 1, 0, 0, 0, 8]);
        cblc[64..72].copy_from_slice(&[0, 1, 0, 17, 0, 0, 0, 4]);
        cblc[76..80].copy_from_slice(&33u32.to_be_bytes());
        let mut cbdt = vec![0, 3, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0, 24];
        cbdt.extend_from_slice(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\x40\0\0\0\x40\0");
        let data = read_fonts::tables::cbdt::Cbdt::read(cbdt.as_slice().into()).unwrap();
        let header = read_fonts::tables::cblc::Cblc::read(cblc.as_slice().into()).unwrap();
        let dimensions = cbdt_dimensions(&header, &data, 0, GlyphId::new(1)).unwrap();
        assert_eq!(
            dimensions,
            [16384, 16384],
            "PNG dimensions override small metrics"
        );
        assert_eq!(bounded_bitmap_ppem(160, dimensions), None);
        // A sparse format-4 record must not be checked with read-fonts and
        // then decoded at a different address by Swash.
        cblc.resize(84, 0);
        cblc[12..16].copy_from_slice(&28u32.to_be_bytes());
        cblc[64..66].copy_from_slice(&4u16.to_be_bytes());
        cblc[72..76].copy_from_slice(&1u32.to_be_bytes());
        cblc[76..80].copy_from_slice(&[0, 1, 0, 0]);
        cblc[80..84].copy_from_slice(&[0, 2, 0, 33]);
        let header = read_fonts::tables::cblc::Cblc::read(cblc.as_slice().into()).unwrap();
        assert_eq!(cbdt_dimensions(&header, &data, 0, GlyphId::new(1)), None);
    }

    #[test]
    fn oversized_bitmap_headers_are_refused_before_decoding() {
        assert_eq!(bounded_bitmap_ppem(160, [160, 160]), Some(160));
        assert_eq!(bounded_bitmap_ppem(160, [1024, 1024]), Some(160));
        for dimensions in [
            [16384, 16384],
            [1025, 1024],
            [u32::MAX, u32::MAX],
            [0, 1],
            [1, 0],
        ] {
            assert_eq!(bounded_bitmap_ppem(160, dimensions), None, "{dimensions:?}");
        }
        let mut header = *b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\x40\0\0\0\x40\0";
        assert_eq!(png_dimensions(&header), Some([16384, 16384]));
        assert_eq!(
            bounded_bitmap_ppem(160, png_dimensions(&header).unwrap()),
            None
        );
        assert_eq!(png_dimensions(&header[..23]), None);
        header[0] = 0;
        assert_eq!(png_dimensions(&header), None);
    }

    #[test]
    fn premultiplied_colr_keeps_its_colour_and_upload_encoding() {
        use glyphon::cosmic_text::SwashImage;
        let mut image = SwashImage::new();
        image.content = SwashContent::Color;
        image.source = swash::scale::Source::ColorOutline(0);
        image.placement.width = 1;
        image.placement.height = 1;
        image.data = vec![127, 0, 0, 127];
        let uploaded = super::super::glyphpipe::RasterGlyph::from_swash(&image).unwrap();
        assert_eq!(uploaded.data, &[127, 0, 0, 127]);
        assert_eq!(AlphaEncoding::of(&image), AlphaEncoding::Premultiplied);
        assert_eq!(
            area_resample(&image.data, [1, 1], 4, [1, 1], AlphaEncoding::of(&image)),
            uploaded.data
        );
        let premultiplied = [127, 0, 0, 127, 253, 0, 0, 253];
        assert_eq!(
            area_resample(&premultiplied, [2, 1], 4, [1, 1], AlphaEncoding::of(&image)),
            vec![190, 0, 0, 190]
        );
        let bw = [0, 0, 0, 128, 128, 128, 128, 128];
        assert_eq!(
            area_resample(&bw, [2, 1], 4, [1, 1], AlphaEncoding::of(&image)),
            vec![94, 94, 94, 128]
        );
        image.source = swash::scale::Source::ColorBitmap(swash::scale::StrikeWith::BestFit);
        assert_eq!(AlphaEncoding::of(&image), AlphaEncoding::Straight);
        let straight = [255, 0, 0, 127, 255, 0, 0, 253];
        assert_eq!(
            area_resample(&straight, [2, 1], 4, [1, 1], AlphaEncoding::of(&image)),
            vec![255, 0, 0, 190]
        );
        assert_eq!(
            area_resample(
                &[255, 255, 255, 0],
                [1, 1],
                4,
                [2, 1],
                AlphaEncoding::Premultiplied
            ),
            vec![0; 8]
        );
    }

    #[test]
    fn integer_ratios_average_whole_blocks() {
        // 4x2 coverage down to 2x1: each target pixel is a 2x2 block mean.
        let src = [0, 40, 100, 200, 80, 120, 50, 250];
        assert_eq!(
            area_resample(&src, [4, 2], 1, [2, 1], AlphaEncoding::Straight),
            vec![60, 150]
        );
    }

    #[test]
    fn fractional_ratios_weight_by_overlap() {
        // Three pixels into two: the middle one is split evenly.
        let src = [0, 90, 255];
        // (0 * 1 + 90 * 0.5) / 1.5 = 30; (90 * 0.5 + 255 * 1) / 1.5 = 200.
        assert_eq!(
            area_resample(&src, [3, 1], 1, [2, 1], AlphaEncoding::Straight),
            vec![30, 200]
        );
    }

    #[test]
    fn upsampling_keeps_a_flat_image_flat_and_sized() {
        let src = vec![7u8; 2 * 3 * 4];
        let out = area_resample(&src, [2, 3], 4, [5, 7], AlphaEncoding::Straight);
        assert_eq!(out.len(), 5 * 7 * 4);
        assert!(out.iter().all(|&v| v == 7));
    }

    #[test]
    fn transparent_colour_never_tints_its_neighbours() {
        // Opaque red beside a fully transparent pixel holding green.
        let src = [255, 0, 0, 255, 0, 255, 0, 0];
        let out = area_resample(&src, [2, 1], 4, [1, 1], AlphaEncoding::Straight);
        assert_eq!(out, vec![255, 0, 0, 128]);
    }

    #[test]
    fn colour_is_averaged_in_linear_light() {
        // Opaque black beside opaque white: half the light is sRGB 188, not
        // the 128 an average of the encoded values gives.
        let src = [0, 0, 0, 255, 255, 255, 255, 255];
        assert_eq!(
            area_resample(&src, [2, 1], 4, [1, 1], AlphaEncoding::Straight),
            vec![188, 188, 188, 255]
        );
        // Coverage is linear already.
        assert_eq!(
            area_resample(&[0, 255], [2, 1], 1, [1, 1], AlphaEncoding::Straight),
            vec![128]
        );
    }

    #[test]
    fn a_strike_above_the_cap_is_never_a_source() {
        assert_eq!(pick_strike(&[20, 40, 160]), Some(160));
        assert_eq!(pick_strike(&[300, 160, 20]), Some(160));
        assert_eq!(pick_strike(&[MAX_SOURCE_PPEM]), Some(MAX_SOURCE_PPEM));
        // Only strikes above the cap: no source, so the cluster keeps its
        // shaped-size image rather than decoding a huge one.
        assert_eq!(pick_strike(&[512]), None);
        assert_eq!(pick_strike(&[4096, 1024]), None);
        assert_eq!(pick_strike(&[0]), None);
    }

    #[test]
    fn an_outline_source_is_bounded_for_any_font_size() {
        let sizes = [
            0.0f32,
            1.0,
            13.0,
            100.0,
            511.0,
            512.0,
            600.0,
            4096.0,
            f32::MAX,
            f32::INFINITY,
            f32::NAN,
        ];
        for font_size in sizes {
            for (natural, target) in [(0, 0), (13, 14), (13, 13), (600, 600), (1, 4096)] {
                let ppem = outline_source_ppem(font_size, natural, target);
                assert!(
                    (1.0..=MAX_OUTLINE_PPEM).contains(&ppem),
                    "{font_size} px, {natural} -> {target}: {ppem}"
                );
            }
        }
        // Four times the target where that fits.
        assert!((outline_source_ppem(13.0, 13, 14) - 56.0).abs() < 1e-3);
    }

    #[test]
    fn mismatched_input_draws_nothing_rather_than_reading_past_it() {
        assert_eq!(
            area_resample(&[1, 2, 3], [2, 2], 1, [1, 1], AlphaEncoding::Straight),
            vec![0]
        );
        assert!(
            area_resample(&[1, 2, 3, 4], [2, 2], 1, [0, 1], AlphaEncoding::Straight).is_empty()
        );
    }
}
