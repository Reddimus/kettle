//! HEIF and AVIF stills through Apple's ImageIO, from bytes in memory, never
//! a path. The still's declared size is read first and refused past what
//! Kettle draws; it is then drawn as a thumbnail no larger than asked, its
//! orientation applied, in sRGB. It needs nothing beyond the worker's base
//! sandbox, as measured on macOS 27: confined, ImageIO scales in software
//! rather than with the hardware scaler, within 2 of it on average and with
//! no shift in color, in about 25 MB and a tenth of a second for a
//! 48-megapixel photo; a full-size decode would take over 300 MB.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::OnceLock;

use kettle_media::FailureCode;
use kettle_media::video::{DecodedStill, StillImage, still_size_drawn};
use objc2_core_foundation::{
    CFArray, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, kCFBooleanTrue,
    kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks,
};
use objc2_core_graphics::CGImage;

/// ImageIO's image source, a Core Foundation object.
#[repr(C)]
struct CGImageSource {
    _private: [u8; 0],
}

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    fn CGImageSourceCreateWithData(
        data: &CFData,
        options: Option<&CFDictionary>,
    ) -> Option<NonNull<CGImageSource>>;
    fn CGImageSourceGetCount(source: NonNull<CGImageSource>) -> usize;
    fn CGImageSourceCopyPropertiesAtIndex(
        source: NonNull<CGImageSource>,
        index: usize,
        options: Option<&CFDictionary>,
    ) -> Option<NonNull<CFDictionary>>;
    fn CGImageSourceCreateThumbnailAtIndex(
        source: NonNull<CGImageSource>,
        index: usize,
        options: &CFDictionary,
    ) -> Option<NonNull<CGImage>>;
    fn CGImageSourceCopyTypeIdentifiers() -> Option<NonNull<CFArray>>;
    static kCGImageSourceTypeIdentifierHint: &'static CFString;
    static kCGImageSourceCreateThumbnailFromImageAlways: &'static CFString;
    static kCGImageSourceCreateThumbnailWithTransform: &'static CFString;
    static kCGImageSourceThumbnailMaxPixelSize: &'static CFString;
    static kCGImagePropertyPixelWidth: &'static CFString;
    static kCGImagePropertyPixelHeight: &'static CFString;
    static kCGImagePropertyOrientation: &'static CFString;
}

/// The uniform type ImageIO knows `still` by.
fn type_identifier(still: StillImage) -> &'static str {
    match still {
        StillImage::Heif => "public.heic",
        StillImage::Avif => "public.avif",
    }
}

/// Whether this system's ImageIO reads `still`: every macOS Kettle runs on
/// reads HEIF, and AVIF from macOS 13.
pub(crate) fn reads(still: StillImage) -> bool {
    static TYPES: OnceLock<Vec<String>> = OnceLock::new();
    TYPES
        .get_or_init(|| {
            // SAFETY: the call takes nothing and returns an array it made,
            // which is released with its owner here.
            let Some(types) = (unsafe { CGImageSourceCopyTypeIdentifiers() }) else {
                return Vec::new();
            };
            // SAFETY: a Copy function's result is owned by the caller.
            let types = unsafe { CFRetained::from_raw(types) };
            (0..types.count())
                .filter_map(|index| {
                    // SAFETY: `index` is within the array, whose values are
                    // CFStrings the array keeps alive while it is read.
                    let value = unsafe { types.value_at_index(index) }.cast::<CFString>();
                    // SAFETY: as above.
                    unsafe { value.as_ref() }.map(ToString::to_string)
                })
                .collect()
        })
        .iter()
        .any(|known| known == type_identifier(still))
}

/// Draw the still `bytes` hold, `still` by its brands, its orientation
/// applied and no longer than `max_edge` on its longer side.
pub(crate) fn decode(
    bytes: &[u8],
    still: StillImage,
    max_edge: u32,
) -> Result<DecodedStill, FailureCode> {
    if !reads(still) {
        return Err(FailureCode::CodecUnavailable);
    }
    let data = CFData::from_bytes(bytes);
    let hint = CFString::from_static_str(type_identifier(still));
    // SAFETY: the key is ImageIO's constant, the value a CFString held here.
    let source_options = dictionary(&[(unsafe { kCGImageSourceTypeIdentifierHint }, &hint)])?;
    // SAFETY: `data` and the options outlive the call, which retains what it
    // keeps.
    let source = unsafe { CGImageSourceCreateWithData(&data, Some(&source_options)) }
        .ok_or(FailureCode::RenderParse)?;
    // SAFETY: a Create function's result is owned by the caller; holding it
    // as a CFType releases it when this returns.
    let _owned = unsafe { CFRetained::<CFType>::from_raw(source.cast()) };
    // SAFETY: `source` is a live image source.
    if unsafe { CGImageSourceGetCount(source) } == 0 {
        return Err(FailureCode::RenderParse);
    }
    // SAFETY: as above; index 0 exists.
    let properties = unsafe { CGImageSourceCopyPropertiesAtIndex(source, 0, None) }
        .ok_or(FailureCode::RenderParse)?;
    // SAFETY: a Copy function's result is owned by the caller.
    let properties = unsafe { CFRetained::from_raw(properties) };
    // SAFETY: the keys are ImageIO's constants.
    let (width, height) = unsafe {
        (
            number(&properties, kCGImagePropertyPixelWidth),
            number(&properties, kCGImagePropertyPixelHeight),
        )
    };
    let (Some(width), Some(height)) = (width, height) else {
        return Err(FailureCode::RenderParse);
    };
    let (width, height) = (
        u32::try_from(width).map_err(|_| FailureCode::RenderParse)?,
        u32::try_from(height).map_err(|_| FailureCode::RenderParse)?,
    );
    if !still_size_drawn(width, height) {
        return Err(FailureCode::FileTooLarge);
    }
    // EXIF orientations 5 to 8 turn the picture a quarter.
    // SAFETY: the key is ImageIO's constant.
    let orientation = unsafe { number(&properties, kCGImagePropertyOrientation) }.unwrap_or(1);
    let (source_width, source_height) = if (5..=8).contains(&orientation) {
        (height, width)
    } else {
        (width, height)
    };
    let max = CFNumber::new_i32(i32::try_from(max_edge).map_err(|_| FailureCode::BadParams)?);
    // SAFETY: kCFBooleanTrue is Core Foundation's constant.
    let yes = unsafe { kCFBooleanTrue }.ok_or(FailureCode::RenderResource)?;
    // SAFETY: the keys are ImageIO's constants; the values live here.
    let options = unsafe {
        dictionary(&[
            (kCGImageSourceCreateThumbnailFromImageAlways, yes.as_ref()),
            (kCGImageSourceCreateThumbnailWithTransform, yes.as_ref()),
            (kCGImageSourceThumbnailMaxPixelSize, max.as_ref()),
        ])
    }?;
    // SAFETY: `source` is live and the options outlive the call.
    let image = unsafe { CGImageSourceCreateThumbnailAtIndex(source, 0, &options) }
        .ok_or(FailureCode::RenderParse)?;
    // SAFETY: a Create function's result is owned by the caller.
    let image = unsafe { CFRetained::from_raw(image) };
    let (drawn_width, drawn_height) = (CGImage::width(Some(&image)), CGImage::height(Some(&image)));
    let (Ok(drawn_width), Ok(drawn_height)) =
        (u32::try_from(drawn_width), u32::try_from(drawn_height))
    else {
        return Err(FailureCode::RenderResource);
    };
    if drawn_width == 0 || drawn_height == 0 || drawn_width.max(drawn_height) > max_edge {
        return Err(FailureCode::RenderResource);
    }
    let rgba = crate::avfoundation::pixels(&image, drawn_width, drawn_height)?;
    Ok(DecodedStill {
        width: drawn_width,
        height: drawn_height,
        rgba,
        source_width,
        source_height,
    })
}

/// A dictionary of `entries`, keys and values retained by it.
fn dictionary(entries: &[(&CFString, &CFType)]) -> Result<CFRetained<CFDictionary>, FailureCode> {
    let mut keys: Vec<*const c_void> = entries
        .iter()
        .map(|(key, _)| std::ptr::from_ref(*key).cast())
        .collect();
    let mut values: Vec<*const c_void> = entries
        .iter()
        .map(|(_, value)| std::ptr::from_ref(*value).cast())
        .collect();
    // SAFETY: both lists hold `entries.len()` live CF objects, which the
    // type callbacks retain into the new dictionary.
    unsafe {
        CFDictionary::new(
            None,
            keys.as_mut_ptr(),
            values.as_mut_ptr(),
            entries.len() as isize,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
    }
    .ok_or(FailureCode::RenderResource)
}

/// The integer `properties` holds under `key`, if it holds a number there.
///
/// # Safety
///
/// `key` must be a live CFString.
unsafe fn number(properties: &CFDictionary, key: &CFString) -> Option<i64> {
    // SAFETY: the key is live, and a value found is owned by the dictionary,
    // which outlives this read.
    let value = unsafe { properties.value(std::ptr::from_ref(key).cast()) };
    if value.is_null() {
        return None;
    }
    let value = value.cast::<CFType>();
    // SAFETY: as above.
    let value = unsafe { &*value };
    value.downcast_ref::<CFNumber>()?.as_i64()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture from `tests/fixtures/still` (its README says how it was
    /// made): 640x480, every pixel (230, 40, 20).
    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/still")
                .join(name),
        )
        .unwrap()
    }

    /// Whether every pixel of `rgba` is the fixtures' colour, give or take
    /// what lossy coding moves it.
    fn the_fixture_colour(rgba: &[u8]) -> bool {
        rgba.as_chunks::<4>().0.iter().all(|pixel| {
            pixel[0].abs_diff(230) <= 4
                && pixel[1].abs_diff(40) <= 4
                && pixel[2].abs_diff(20) <= 4
                && pixel[3] == 255
        })
    }

    /// A HEIF and an AVIF still are drawn no larger than asked, their own
    /// size kept, in their own colour; never enlarged past it; and bytes
    /// that only say they are a still are a broken one.
    #[test]
    fn stills_are_drawn_at_the_size_asked_in_their_own_colour() {
        for (name, still) in [
            ("solid.heic", StillImage::Heif),
            ("solid.avif", StillImage::Avif),
        ] {
            assert!(reads(still), "{name}");
            let drawn = decode(&fixture(name), still, 64).unwrap();
            assert_eq!((drawn.width, drawn.height), (64, 48), "{name}");
            assert_eq!((drawn.source_width, drawn.source_height), (640, 480));
            assert!(
                the_fixture_colour(&drawn.rgba),
                "{name}: {:?}",
                &drawn.rgba[..4]
            );
            let whole = decode(&fixture(name), still, 4096).unwrap();
            assert_eq!((whole.width, whole.height), (640, 480), "never enlarged");
        }
        let grid = decode(&fixture("grid.heic"), StillImage::Heif, 64).unwrap();
        assert_eq!((grid.width, grid.height), (64, 48), "a grid of tiles");
        assert_eq!((grid.source_width, grid.source_height), (1024, 768));
        assert!(
            the_fixture_colour(&grid.rgba),
            "grid: {:?}",
            &grid.rgba[..4]
        );
        let mut broken = vec![0, 0, 0, 24];
        broken.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
        broken.extend_from_slice(&[0x5a; 64]);
        assert_eq!(
            decode(&broken, StillImage::Heif, 64).unwrap_err(),
            FailureCode::RenderParse
        );
    }
}
