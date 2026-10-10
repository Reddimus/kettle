//! Apple's own decoder for MP4 and QuickTime files: AVFoundation, in the
//! worker, with nothing to install. Video decoding itself runs in Apple's
//! decoder service; the file is demuxed here.
//!
//! The asset is opened through `/dev/fd/N`, a fresh descriptor of the held
//! file, so the path is never resolved again. A descriptor has no file name,
//! so the content's sniffed MIME type is given, and references to media
//! outside the file are forbidden. The VP9 decoder Apple ships but does not
//! register by default is registered first. Every property is loaded
//! asynchronously within the deadline, and the frames come from one batch
//! request to an image generator that applies the track's display transform
//! and returns each frame's actual time; at the deadline the batch is
//! cancelled. Each frame is drawn at exactly the plan's size in sRGB, then
//! un-premultiplied to straight RGBA.
//!
//! Containers AVFoundation does not read (WebM, Matroska, AVI and the rest)
//! are `UnsupportedContainer`, and a codec it cannot decode is
//! `CodecUnavailable`, which [`Decoders`](crate::Decoders) answers with the external decoder
//! when there is one.

use std::os::fd::AsRawFd as _;
use std::sync::{Arc, Condvar, Mutex, Once};
use std::time::Instant;

use block2::RcBlock;
use kettle_media::video::{
    DecodedFrame, DecodedStills, StillsPlan, VideoContainer, VideoDecoder, VideoInput,
};
use kettle_media::{
    FailureCode, MAX_VIDEO_FPS_MILLI, MAX_VIDEO_SIDE, StillSample, VideoCodec, VideoInfo,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetImageGenerator, AVAssetImageGeneratorResult, AVAssetReferenceRestrictions, AVAssetTrack,
    AVAsynchronousKeyValueLoading, AVMediaTypeAudio, AVMediaTypeVideo, AVURLAsset,
    AVURLAssetOverrideMIMETypeKey, AVURLAssetPreferPreciseDurationAndTimingKey,
    AVURLAssetReferenceRestrictionsKey, NSValueAVFoundationExtensions as _,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGInterpolationQuality, kCGColorSpaceSRGB,
};
use objc2_core_media::{CMFormatDescription, CMTime};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString, NSURL, NSValue};

use crate::reopen::reopen;

/// `kCMTimeFlags_Valid`, and the flags that make a time no number.
const TIME_VALID: u32 = 1;
const TIME_NOT_A_NUMBER: u32 = 0x4 | 0x8 | 0x10;
/// AVFoundation's errors for a missing or failing decoder.
const DECODER_NOT_FOUND: isize = -11833;
const DECODE_FAILED: isize = -11821;
const DECODER_TEMPORARILY_UNAVAILABLE: isize = -11839;

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    /// Registers a decoder Apple ships but does not load by default
    /// (macOS 11 and later).
    fn VTRegisterSupplementalVideoDecoderIfAvailable(codec_type: u32);
}

/// Apple's decoder, for the containers it reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AvFoundation;

/// A value one callback sets and one thread waits for until a deadline.
struct Signal<T> {
    value: Mutex<Option<T>>,
    ready: Condvar,
}

impl<T> Signal<T> {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            value: Mutex::new(None),
            ready: Condvar::new(),
        })
    }

    fn set(&self, value: T) {
        let mut slot = self
            .value
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *slot = Some(value);
        self.ready.notify_all();
    }

    fn wait(&self, deadline: Instant) -> Option<T> {
        let mut slot = self
            .value
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        loop {
            if let Some(value) = slot.take() {
                return Some(value);
            }
            let remaining = deadline.checked_duration_since(Instant::now())?;
            slot = self
                .ready
                .wait_timeout(slot, remaining)
                .unwrap_or_else(|poison| poison.into_inner())
                .0;
        }
    }
}

/// The MIME type AVFoundation is told a sniffed container is.
fn mime(container: VideoContainer) -> Option<&'static str> {
    match container {
        VideoContainer::IsoBmff => Some("video/mp4"),
        VideoContainer::QuickTime => Some("video/quicktime"),
        _ => None,
    }
}

/// The codec a track's four-character code names.
fn codec(fourcc: u32) -> VideoCodec {
    match &fourcc.to_be_bytes() {
        b"avc1" | b"avc3" => VideoCodec::H264,
        b"hvc1" | b"hev1" => VideoCodec::Hevc,
        b"vp08" => VideoCodec::Vp8,
        b"vp09" => VideoCodec::Vp9,
        b"av01" => VideoCodec::Av1,
        b"mp4v" => VideoCodec::Mpeg4,
        b"mp2v" | b"hdv1" | b"hdv2" | b"hdv3" | b"xdv2" => VideoCodec::Mpeg2,
        b"apcn" | b"apch" | b"apcs" | b"apco" | b"ap4h" | b"ap4x" | b"aprn" | b"aprh" => {
            VideoCodec::ProRes
        }
        b"jpeg" | b"mjpa" | b"mjpb" => VideoCodec::Mjpeg,
        b"rle " => VideoCodec::QuickTimeAnimation,
        _ => VideoCodec::Unknown,
    }
}

/// A time as whole ms, rounded to nearest; `None` when it is no number.
fn ms(time: CMTime) -> Option<i64> {
    if time.flags.0 & TIME_VALID == 0
        || time.flags.0 & TIME_NOT_A_NUMBER != 0
        || time.timescale <= 0
    {
        return None;
    }
    let scale = i128::from(time.timescale);
    let ms = (i128::from(time.value) * 1000 + scale / 2).div_euclid(scale);
    i64::try_from(ms).ok()
}

/// A time of whole ms.
fn time(ms: u64) -> CMTime {
    // SAFETY: CMTimeMake only builds a value from its two numbers.
    unsafe { CMTime::new(i64::try_from(ms).unwrap_or(i64::MAX), 1000) }
}

/// Degrees clockwise a display transform turns the picture, to the nearest
/// quarter turn. In AVFoundation's flipped coordinates a positive angle of
/// `(a, b)` turns clockwise as shown.
fn rotation(a: f64, b: f64) -> u16 {
    let degrees = b.atan2(a).to_degrees().round() as i64;
    ((degrees.rem_euclid(360) + 45) / 90 % 4 * 90) as u16
}

/// The picture as shown: the bounding box of the track's size under its
/// display transform, each term's size taken on its own so a turn that is
/// not a quarter still covers what it shows.
fn shown_size(size: CGSize, transform: objc2_core_foundation::CGAffineTransform) -> (f64, f64) {
    let width = (transform.a * size.width).abs() + (transform.c * size.height).abs();
    let height = (transform.b * size.width).abs() + (transform.d * size.height).abs();
    (width.round(), height.round())
}

/// Load `keys` on `object`, waiting no later than `deadline`.
fn load<T: AVAsynchronousKeyValueLoading + objc2::Message>(
    object: &T,
    keys: &[&str],
    deadline: Instant,
) -> Result<(), FailureCode> {
    let signal = Signal::new();
    let done = Arc::clone(&signal);
    let block = RcBlock::new(move || done.set(()));
    let keys: Vec<Retained<NSString>> = keys.iter().map(|key| NSString::from_str(key)).collect();
    let keys = NSArray::from_retained_slice(&keys);
    // SAFETY: `keys` are NSStrings naming the object's properties, and the
    // block only sets a shared signal, so it may run on any thread.
    unsafe { object.loadValuesAsynchronouslyForKeys_completionHandler(&keys, Some(&block)) };
    signal.wait(deadline).ok_or(FailureCode::RenderTimeout)
}

/// One frame's outcome, as the image generator reports it.
type Outcome = Result<(i64, Vec<u8>), FailureCode>;

/// What the frames' callbacks share with the waiting thread.
struct Batch {
    times: Vec<u64>,
    outcomes: Mutex<Vec<Option<Outcome>>>,
    finished: Condvar,
}

impl Batch {
    fn finish(&self, requested: CMTime, outcome: Outcome) {
        let Some(at) = ms(requested)
            .and_then(|requested| u64::try_from(requested).ok())
            .and_then(|requested| self.times.iter().position(|&time| time == requested))
        else {
            return;
        };
        let mut outcomes = self
            .outcomes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if outcomes[at].is_none() {
            outcomes[at] = Some(outcome);
        }
        self.finished.notify_all();
    }

    /// Every outcome, once all are in, or `None` at the deadline.
    fn wait(&self, deadline: Instant) -> Option<Vec<Outcome>> {
        let mut outcomes = self
            .outcomes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        loop {
            if outcomes.iter().all(Option::is_some) {
                return Some(outcomes.iter_mut().filter_map(Option::take).collect());
            }
            let remaining = deadline.checked_duration_since(Instant::now())?;
            outcomes = self
                .finished
                .wait_timeout(outcomes, remaining)
                .unwrap_or_else(|poison| poison.into_inner())
                .0;
        }
    }
}

/// Draw `image` at exactly `width` by `height` in sRGB, as straight RGBA.
fn pixels(image: &CGImage, width: u32, height: u32) -> Result<Vec<u8>, FailureCode> {
    let (w, h) = (width as usize, height as usize);
    let mut rgba = vec![0u8; w * h * 4];
    // SAFETY: kCGColorSpaceSRGB is a constant CFString.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or(FailureCode::RenderResource)?;
    // SAFETY: `rgba` holds `h` rows of `w * 4` bytes and outlives the
    // context, which draws into it and is dropped before it is read.
    let context = unsafe {
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            w,
            h,
            8,
            w * 4,
            Some(&space),
            CGImageAlphaInfo::PremultipliedLast.0,
        )
    }
    .ok_or(FailureCode::RenderResource)?;
    CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::High);
    let whole = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: f64::from(width),
            height: f64::from(height),
        },
    };
    CGContext::draw_image(Some(&context), whole, Some(image));
    drop(context);
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        if alpha != 0 && alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    Ok(rgba)
}

/// The failure an AVFoundation error stands for: a decoder it lacks, which
/// another decoder may have, or the stream itself.
fn failure(error: Option<&NSError>) -> FailureCode {
    match error.map(NSError::code) {
        Some(DECODER_NOT_FOUND | DECODE_FAILED | DECODER_TEMPORARILY_UNAVAILABLE) => {
            FailureCode::CodecUnavailable
        }
        _ => FailureCode::RenderParse,
    }
}

impl VideoDecoder for AvFoundation {
    fn stills(
        &self,
        input: VideoInput<'_>,
        plan: &mut dyn FnMut(&VideoInfo) -> Result<StillsPlan, FailureCode>,
        deadline: Instant,
    ) -> Result<DecodedStills, FailureCode> {
        let mime = mime(input.container).ok_or(FailureCode::UnsupportedContainer)?;
        static VP9: Once = Once::new();
        // SAFETY: the call takes a codec code and registers Apple's own
        // decoder for it, if the system has one.
        VP9.call_once(|| unsafe {
            VTRegisterSupplementalVideoDecoderIfAvailable(u32::from_be_bytes(*b"vp09"));
        });
        // AVFoundation reads its own description of the file, opened by
        // descriptor: no path is resolved, and nothing else shares its offset.
        let file = reopen(input.file, input.identity)?;
        let url = NSURL::URLWithString(&NSString::from_str(&format!(
            "file:///dev/fd/{}",
            file.as_raw_fd()
        )))
        .ok_or(FailureCode::BackendUnavailable)?;
        let mime = NSString::from_str(mime);
        let forbid = NSNumber::numberWithUnsignedInteger(AVAssetReferenceRestrictions::ForbidAll.0);
        let precise = NSNumber::numberWithBool(true);
        // SAFETY: the keys are AVFoundation's own option constants.
        let keys = unsafe {
            [
                AVURLAssetOverrideMIMETypeKey,
                AVURLAssetReferenceRestrictionsKey,
                AVURLAssetPreferPreciseDurationAndTimingKey,
            ]
        };
        let values: [&AnyObject; 3] = [&mime, &forbid, &precise];
        let options = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
        // SAFETY: each option holds the type AVFoundation documents for it:
        // a MIME string, a restriction mask and a boolean.
        let asset = unsafe { AVURLAsset::URLAssetWithURL_options(&url, Some(&options)) };
        load(&*asset, &["duration", "tracks"], deadline)?;
        // SAFETY: both keys are loaded, so neither read blocks.
        let (duration, tracks) = unsafe { (asset.duration(), asset.tracks()) };
        // SAFETY: the media type constants are AVFoundation's own.
        let (video_type, audio_type) = unsafe { (AVMediaTypeVideo, AVMediaTypeAudio) };
        let of_type = |track: &AVAssetTrack, kind: Option<&NSString>| {
            // SAFETY: a loaded track's media type is a plain property.
            kind.is_some_and(|kind| unsafe { track.mediaType() }.isEqualToString(kind))
        };
        let video = tracks
            .iter()
            .find(|track| of_type(track, video_type))
            .ok_or_else(|| {
                if tracks.count() == 0 {
                    // No tracks at all: AVFoundation could not read it.
                    FailureCode::UnsupportedContainer
                } else {
                    FailureCode::UnsupportedMedia
                }
            })?;
        let has_audio = tracks.iter().any(|track| of_type(&track, audio_type));
        load(
            &*video,
            &[
                "naturalSize",
                "preferredTransform",
                "nominalFrameRate",
                "formatDescriptions",
            ],
            deadline,
        )?;
        // SAFETY: these keys are loaded, so none of these reads blocks.
        let (size, transform, rate, descriptions) = unsafe {
            (
                video.naturalSize(),
                video.preferredTransform(),
                video.nominalFrameRate(),
                video.formatDescriptions(),
            )
        };
        let fourcc = descriptions.iter().next().map_or(0, |description| {
            let description: *const AnyObject = &*description;
            // SAFETY: a track's format descriptions are CMFormatDescription
            // objects, bridged into the array as objects.
            unsafe { (*description.cast::<CMFormatDescription>()).media_sub_type() }
        });
        let duration_ms = ms(duration)
            .and_then(|duration| u64::try_from(duration).ok())
            .ok_or(FailureCode::RenderParse)?;
        let (width, height) = shown_size(size, transform);
        let side = f64::from(MAX_VIDEO_SIDE);
        if !(1.0..=side).contains(&width) || !(1.0..=side).contains(&height) {
            return Err(if width > side || height > side {
                FailureCode::RenderResource
            } else {
                FailureCode::RenderParse
            });
        }
        let fps = (f64::from(rate) * 1000.0).round();
        let info = VideoInfo {
            duration_ms,
            width: width as u32,
            height: height as u32,
            rotation: rotation(transform.a, transform.b),
            codec: codec(fourcc),
            fps_milli: (fps >= 1.0 && fps <= f64::from(MAX_VIDEO_FPS_MILLI)).then_some(fps as u32),
            has_audio,
            container: Some(input.container),
        };
        info.validate().map_err(|_| FailureCode::RenderParse)?;
        let wanted = plan(&info)?;
        if wanted.times_ms.is_empty() || wanted.width == 0 || wanted.height == 0 {
            return Err(FailureCode::BadParams);
        }

        let mut times = wanted.times_ms.clone();
        times.sort_unstable();
        times.dedup();
        // Each frame may be taken from within a quarter of the gap to its
        // neighbours, which decodes less than an exact time and keeps it
        // inside its own share of the window (half the gap would let two
        // instants take the keyframe between them); a single instant is
        // exact. The time reported is the frame's own.
        let tolerance = times
            .windows(2)
            .map(|pair| (pair[1] - pair[0]) / 4)
            .min()
            .unwrap_or(0)
            .min(1000);
        // SAFETY: the generator is made for a loaded asset, and its settings
        // are plain values set before any image is asked for.
        let generator = unsafe {
            let generator = AVAssetImageGenerator::assetImageGeneratorWithAsset(&asset);
            generator.setAppliesPreferredTrackTransform(true);
            // A bound only: frames are drawn at the exact size after. Both
            // sides take the longer edge, so a rotated frame is not shrunk.
            let edge = f64::from(wanted.width.max(wanted.height));
            generator.setMaximumSize(CGSize {
                width: edge,
                height: edge,
            });
            generator.setRequestedTimeToleranceBefore(time(tolerance));
            generator.setRequestedTimeToleranceAfter(time(tolerance));
            generator
        };
        let batch = Arc::new(Batch {
            outcomes: Mutex::new(vec![None; times.len()]),
            times: times.clone(),
            finished: Condvar::new(),
        });
        let (width, height) = (wanted.width, wanted.height);
        let shared = Arc::clone(&batch);
        let handler = RcBlock::new(
            move |requested: CMTime,
                  image: *mut CGImage,
                  actual: CMTime,
                  result: AVAssetImageGeneratorResult,
                  error: *mut NSError| {
                // SAFETY: the generator passes a live image (or null) and a
                // live error (or null) for the duration of the call.
                let (image, error) = unsafe { (image.as_ref(), error.as_ref()) };
                let outcome = match (result, image, ms(actual)) {
                    (AVAssetImageGeneratorResult::Succeeded, Some(image), Some(actual)) => {
                        pixels(image, width, height).map(|rgba| (actual, rgba))
                    }
                    (AVAssetImageGeneratorResult::Cancelled, ..) => Err(FailureCode::RenderTimeout),
                    _ => Err(failure(error)),
                };
                shared.finish(requested, outcome);
            },
        );
        let requested: Vec<Retained<NSValue>> = times
            .iter()
            // SAFETY: an NSValue wrapping a CMTime by value.
            .map(|&ms| unsafe { NSValue::valueWithCMTime(time(ms)) })
            .collect();
        let requested = NSArray::from_retained_slice(&requested);
        // SAFETY: the handler only touches shared, synchronized state and
        // copies each image before returning, so it may run on any thread,
        // after this call returns, or after a cancellation.
        unsafe {
            generator.generateCGImagesAsynchronouslyForTimes_completionHandler(
                &requested,
                RcBlock::as_ptr(&handler),
            );
        }
        let Some(outcomes) = batch.wait(deadline) else {
            // SAFETY: cancelling outstanding requests is always allowed; the
            // handler answers each with Cancelled.
            unsafe { generator.cancelAllCGImageGeneration() };
            return Err(FailureCode::RenderTimeout);
        };
        let mut decoded: Vec<(i64, Vec<u8>)> = Vec::with_capacity(outcomes.len());
        for outcome in outcomes {
            decoded.push(outcome?);
        }
        drop(file);

        let mut tolerance_ms = 0u64;
        let frames = wanted
            .times_ms
            .iter()
            .map(|&requested_ms| {
                let at = times
                    .binary_search(&requested_ms)
                    .map_err(|_| FailureCode::RenderParse)?;
                let (actual, rgba) = &decoded[at];
                let actual_ms = u64::try_from(*actual).unwrap_or(0).min(duration_ms);
                tolerance_ms = tolerance_ms.max(requested_ms.abs_diff(actual_ms));
                Ok(DecodedFrame {
                    sample: StillSample {
                        requested_ms,
                        actual_ms,
                    },
                    rgba: rgba.clone(),
                })
            })
            .collect::<Result<Vec<_>, FailureCode>>()?;
        Ok(DecodedStills {
            info,
            frames,
            tolerance_ms: u32::try_from(tolerance_ms).unwrap_or(u32::MAX),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_transforms_turn_clockwise_by_quarters() {
        assert_eq!(rotation(1.0, 0.0), 0);
        assert_eq!(rotation(0.0, 1.0), 90);
        assert_eq!(rotation(-1.0, 0.0), 180);
        assert_eq!(rotation(0.0, -1.0), 270);
        assert_eq!(rotation(0.9, 0.1), 0, "nearly upright");
    }

    #[test]
    fn the_shown_size_bounds_the_transformed_picture() {
        let transform = |a: f64, b: f64, c: f64, d: f64| objc2_core_foundation::CGAffineTransform {
            a,
            b,
            c,
            d,
            tx: 0.0,
            ty: 0.0,
        };
        let size = CGSize {
            width: 1920.0,
            height: 1080.0,
        };
        assert_eq!(
            shown_size(size, transform(1.0, 0.0, 0.0, 1.0)),
            (1920.0, 1080.0)
        );
        assert_eq!(
            shown_size(size, transform(0.0, 1.0, -1.0, 0.0)),
            (1080.0, 1920.0)
        );
        let half = std::f64::consts::FRAC_1_SQRT_2;
        assert_eq!(
            shown_size(size, transform(half, half, -half, half)),
            (2121.0, 2121.0),
            "an eighth of a turn"
        );
        let square = CGSize {
            width: 100.0,
            height: 100.0,
        };
        assert_eq!(
            shown_size(square, transform(half, half, -half, half)),
            (141.0, 141.0)
        );
    }

    #[test]
    fn four_character_codes_name_codecs() {
        let code = |name: &[u8; 4]| codec(u32::from_be_bytes(*name));
        assert_eq!(code(b"avc1"), VideoCodec::H264);
        assert_eq!(code(b"hev1"), VideoCodec::Hevc);
        assert_eq!(code(b"apcn"), VideoCodec::ProRes);
        assert_eq!(code(b"rle "), VideoCodec::QuickTimeAnimation);
        assert_eq!(code(b"zzzz"), VideoCodec::Unknown);
    }

    #[test]
    fn times_are_numbers_only_when_valid() {
        // SAFETY: CMTimeMake only builds a value.
        let valid = unsafe { CMTime::new(1501, 1000) };
        assert_eq!(ms(valid), Some(1501));
        assert_eq!(ms(time(250)), Some(250));
        let mut indefinite = valid;
        indefinite.flags.0 |= 0x10;
        assert_eq!(ms(indefinite), None);
        let mut invalid = valid;
        invalid.flags.0 = 0;
        assert_eq!(ms(invalid), None);
    }
}
