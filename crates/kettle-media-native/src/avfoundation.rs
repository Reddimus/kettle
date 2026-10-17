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
//! A playback session reads the video track with an asset reader from its
//! start to the end, each frame decoded to BGRA at the session's size as
//! coded, which Apple's decoder service scales: nothing is drawn or
//! composited in the worker, so it needs no GPU. Each frame is turned
//! upright as it is copied out, and one sooner than a frame length of the
//! rate asked after the last one kept is skipped.
//!
//! Containers AVFoundation does not read (WebM, Matroska, AVI and the rest)
//! are `UnsupportedContainer`, and a codec it cannot decode is
//! `CodecUnavailable`, which [`Decoders`](crate::Decoders) answers with the external decoder
//! when there is one.

use std::os::fd::AsRawFd as _;
use std::sync::{Arc, Condvar, Mutex, Once};
use std::time::Instant;

use block2::RcBlock;
use kettle_media::audio::{AudioBlock, AudioSource, CHANNELS, MAX_PENDING_FRAMES, SAMPLE_RATE};
use kettle_media::video::{
    DecodedFrame, DecodedStills, FrameSlots, FrameSource, PlayGate, PlayRequest, Playback,
    StillsPlan, VideoContainer, VideoDecoder, VideoInput, VideoPlayer,
};
use kettle_media::{
    AudioRequest, AudioStatus, FailureCode, MAX_VIDEO_FPS_MILLI, MAX_VIDEO_SIDE, Opened,
    StillSample, VideoCodec, VideoInfo, play_size,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetImageGenerator, AVAssetImageGeneratorResult, AVAssetReader, AVAssetReaderStatus,
    AVAssetReaderTrackOutput, AVAssetReferenceRestrictions, AVAssetTrack,
    AVAsynchronousKeyValueLoading, AVMediaTypeAudio, AVMediaTypeVideo, AVURLAsset,
    AVURLAssetOverrideMIMETypeKey, AVURLAssetPreferPreciseDurationAndTimingKey,
    AVURLAssetReferenceRestrictionsKey, NSValueAVFoundationExtensions as _,
};
use objc2_core_foundation::CFString;
use objc2_core_foundation::{CGAffineTransform, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGInterpolationQuality, kCGColorSpaceSRGB,
};
use objc2_core_media::{CMFormatDescription, CMSampleBuffer, CMTime, CMTimeRange};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferHeightKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_32BGRA, kCVReturnSuccess,
};
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
    in_units(time, 1000)
}

/// `time` in whole µs, rounded; `None` when it is not a number.
fn us(time: CMTime) -> Option<i64> {
    in_units(time, 1_000_000)
}

/// `time` in whole `per_second`ths of a second, rounded.
fn in_units(time: CMTime, per_second: i128) -> Option<i64> {
    if time.flags.0 & TIME_VALID == 0
        || time.flags.0 & TIME_NOT_A_NUMBER != 0
        || time.timescale <= 0
    {
        return None;
    }
    let scale = i128::from(time.timescale);
    let units = (i128::from(time.value) * per_second + scale / 2).div_euclid(scale);
    i64::try_from(units).ok()
}

/// A time of whole ms.
fn time(ms: u64) -> CMTime {
    // SAFETY: CMTimeMake only builds a value from its two numbers.
    unsafe { CMTime::new(i64::try_from(ms).unwrap_or(i64::MAX), 1000) }
}

/// How a display transform shows the coded picture: degrees clockwise, to
/// the nearest quarter turn, and whether it is mirrored first. A transform
/// that reflects (its determinant below zero) is the coded picture flipped
/// left to right, then turned.
fn orientation(transform: CGAffineTransform) -> (u16, bool) {
    let (a, b, c, d) = (transform.a, transform.b, transform.c, transform.d);
    if a * d - b * c < 0.0 {
        // The transform is the flip, then the turn: the turn is the
        // transform with the flip undone, its first row negated.
        (rotation(-a, -b), true)
    } else {
        (rotation(a, b), false)
    }
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
fn shown_size(size: CGSize, transform: CGAffineTransform) -> (f64, f64) {
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
pub(crate) fn pixels(image: &CGImage, width: u32, height: u32) -> Result<Vec<u8>, FailureCode> {
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

/// A video opened for AVFoundation: the descriptor it reads the file
/// through, the asset and its first video track, and what the video is.
struct Asset {
    file: std::fs::File,
    asset: Retained<AVURLAsset>,
    video: Retained<AVAssetTrack>,
    info: VideoInfo,
    /// Where the video track ends, within the asset's length: its other
    /// tracks can run on past it.
    video_end_ms: u64,
    /// Whether the picture shows mirrored, before its turn.
    mirrored: bool,
    /// The first audio track, when the asset has sound.
    audio: Option<Retained<AVAssetTrack>>,
}

/// Open `input` and describe it, within `deadline`.
fn open(input: &VideoInput<'_>, deadline: Instant) -> Result<Asset, FailureCode> {
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
    let audio = tracks.iter().find(|track| of_type(track, audio_type));
    let has_audio = audio.is_some();
    load(
        &*video,
        &[
            "naturalSize",
            "preferredTransform",
            "nominalFrameRate",
            "formatDescriptions",
            "timeRange",
        ],
        deadline,
    )?;
    // SAFETY: these keys are loaded, so none of these reads blocks.
    let (size, transform, rate, descriptions, range) = unsafe {
        (
            video.naturalSize(),
            video.preferredTransform(),
            video.nominalFrameRate(),
            video.formatDescriptions(),
            video.timeRange(),
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
    let (rotation, mirrored) = orientation(transform);
    let info = VideoInfo {
        duration_ms,
        width: width as u32,
        height: height as u32,
        rotation,
        codec: codec(fourcc),
        fps_milli: (fps >= 1.0 && fps <= f64::from(MAX_VIDEO_FPS_MILLI)).then_some(fps as u32),
        has_audio,
        container: Some(input.container),
    };
    info.validate().map_err(|_| FailureCode::RenderParse)?;
    let video_end_ms = ms(range.start)
        .zip(ms(range.duration))
        .and_then(|(start, length)| u64::try_from(start.checked_add(length)?).ok())
        .map_or(duration_ms, |end| end.min(duration_ms));
    Ok(Asset {
        file,
        asset,
        video,
        info,
        video_end_ms,
        mirrored,
        audio,
    })
}

impl VideoDecoder for AvFoundation {
    /// A HEIF or AVIF still, through ImageIO, Apple's image decoder beside
    /// its video one.
    fn still_image(
        &self,
        input: kettle_media::video::StillInput<'_>,
        still: kettle_media::video::StillImage,
        max_edge: u32,
        deadline: Instant,
    ) -> Result<kettle_media::video::DecodedStill, FailureCode> {
        if Instant::now() >= deadline {
            return Err(FailureCode::RenderTimeout);
        }
        crate::imageio::decode(input.bytes, still, max_edge)
    }

    fn stills(
        &self,
        input: VideoInput<'_>,
        plan: &mut dyn FnMut(&VideoInfo) -> Result<StillsPlan, FailureCode>,
        deadline: Instant,
    ) -> Result<DecodedStills, FailureCode> {
        let Asset {
            file, asset, info, ..
        } = open(&input, deadline)?;
        let duration_ms = info.duration_ms;
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

/// The output a session reads: the video track decoded to BGRA at `width`
/// by `height` as coded, the picture's own orientation, scaled by Apple's
/// decoder service. Nothing here draws or composites, so no GPU is needed
/// in the worker; the worker turns each frame upright as it copies it.
fn track_output(asset: &Asset, width: u32, height: u32) -> Retained<AVAssetReaderTrackOutput> {
    // The keys are CFStrings, bridged toll-free to NSStrings.
    let bridge = |key: &'static CFString| -> &'static NSString {
        // SAFETY: CFString and NSString are toll-free bridged.
        unsafe { &*std::ptr::from_ref(key).cast::<NSString>() }
    };
    // SAFETY: the keys are CoreVideo's own constants.
    let keys = unsafe {
        [
            bridge(kCVPixelBufferPixelFormatTypeKey),
            bridge(kCVPixelBufferWidthKey),
            bridge(kCVPixelBufferHeightKey),
        ]
    };
    let (format, width, height) = (
        NSNumber::numberWithUnsignedInt(kCVPixelFormatType_32BGRA),
        NSNumber::numberWithUnsignedInt(width),
        NSNumber::numberWithUnsignedInt(height),
    );
    let values: [&AnyObject; 3] = [&format, &width, &height];
    let settings = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
    // SAFETY: the output is made for the loaded track, with uncompressed
    // video settings, before any reader reads through it.
    unsafe {
        let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
            &asset.video,
            Some(&settings),
        );
        output.setAlwaysCopiesSampleData(false);
        output
    }
}

/// An opened asset, held by a session's readers so it and its descriptor
/// outlive them. Nothing calls through it once they read: what the frames'
/// and the sound's threads share is its lifetime, and its objects'
/// reference counts are atomic.
struct Keep(Asset);

impl std::ops::Deref for Keep {
    type Target = Asset;

    fn deref(&self) -> &Asset {
        &self.0
    }
}

// SAFETY: see above; the asset is only dropped, once, by the last holder.
unsafe impl Send for Keep {}
// SAFETY: as above: no thread calls through a shared `Keep`.
unsafe impl Sync for Keep {}

/// An output for `track` that decodes its sound to interleaved stereo
/// 32-bit float samples at 48 kHz, as every session's sound is.
fn audio_output(track: &AVAssetTrack) -> Retained<AVAssetReaderTrackOutput> {
    // AVFAudio's settings keys, whose values are their own names: this
    // crate does not link AVFAudio for six strings.
    let keys = [
        "AVFormatIDKey",
        "AVSampleRateKey",
        "AVNumberOfChannelsKey",
        "AVLinearPCMBitDepthKey",
        "AVLinearPCMIsFloatKey",
        "AVLinearPCMIsBigEndianKey",
        "AVLinearPCMIsNonInterleaved",
    ]
    .map(NSString::from_str);
    let keys: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
    let (format, rate, channels, depth, float, big, planar) = (
        // kAudioFormatLinearPCM: 'lpcm'.
        NSNumber::numberWithUnsignedInt(u32::from_be_bytes(*b"lpcm")),
        NSNumber::numberWithDouble(f64::from(SAMPLE_RATE)),
        NSNumber::numberWithUnsignedInt(CHANNELS as u32),
        NSNumber::numberWithUnsignedInt(32),
        NSNumber::numberWithBool(true),
        NSNumber::numberWithBool(false),
        NSNumber::numberWithBool(false),
    );
    let values: [&AnyObject; 7] = [&format, &rate, &channels, &depth, &float, &big, &planar];
    let settings = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
    // SAFETY: the output is made for the asset's own audio track, with
    // uncompressed settings, before any reader reads through it.
    unsafe {
        AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
            track,
            Some(&settings),
        )
    }
}

/// A session's sound from a reader of its own over the same asset, so the
/// video's reader and this one are taken from independently.
struct AudioFrames {
    reader: Retained<AVAssetReader>,
    output: Retained<AVAssetReaderTrackOutput>,
    /// Held while the reader reads through its descriptor.
    _asset: Arc<Keep>,
    /// The first frame the session plays: earlier ones are dropped.
    from_frame: u64,
    /// Decoded frames not yet taken, and where the first plays.
    pending: Vec<f32>,
    pending_at: u64,
    /// Where the last block taken ended: none starts before it.
    next_frame: u64,
}

// SAFETY: as for `Frames`: one thread takes the sound, and the reader and
// its output are not tied to the thread that made them.
unsafe impl Send for AudioFrames {}

impl AudioFrames {
    /// Read `asset`'s sound from `start_ms` to its end.
    fn start(asset: &Arc<Keep>, track: &AVAssetTrack, start_ms: u64) -> Option<Self> {
        let output = audio_output(track);
        // SAFETY: the reader is made for the loaded asset, and its time
        // range and output are set before it starts.
        let reader = unsafe { AVAssetReader::assetReaderWithAsset_error(&asset.asset) }.ok()?;
        // SAFETY: as above.
        unsafe {
            reader.setTimeRange(CMTimeRange {
                start: time(start_ms),
                duration: time(asset.info.duration_ms - start_ms.min(asset.info.duration_ms)),
            });
            if !reader.canAddOutput(&output) {
                return None;
            }
            reader.addOutput(&output);
            if !reader.startReading() {
                return None;
            }
        }
        let from_frame = kettle_media::audio::frame_of_ms(start_ms);
        Some(Self {
            reader,
            output,
            _asset: Arc::clone(asset),
            from_frame,
            pending: Vec::new(),
            pending_at: from_frame,
            next_frame: from_frame,
        })
    }

    /// Decode the next sample buffer into `pending`; false at the end.
    fn refill(&mut self) -> Result<bool, FailureCode> {
        let sample = objc2::rc::autoreleasepool(|_| {
            // SAFETY: the output belongs to the started reader, which only
            // this thread reads.
            unsafe { self.output.copyNextSampleBuffer() }
        });
        let Some(sample) = sample else {
            // SAFETY: plain properties of the reader.
            return match unsafe { AVAssetReader::status(&self.reader) } {
                AVAssetReaderStatus::Completed => Ok(false),
                AVAssetReaderStatus::Failed => {
                    Err(failure(unsafe { self.reader.error() }.as_deref()))
                }
                _ => Err(FailureCode::RenderParse),
            };
        };
        let (at, samples) = pcm(&sample)?;
        // Frames before the start, or before the last block ended, are
        // dropped: a block never starts before the one before it ended.
        self.pending = samples;
        self.pending_at =
            kettle_media::audio::trim(&mut self.pending, at, self.from_frame.max(self.next_frame));
        Ok(true)
    }
}

/// A sample buffer's frames and where the first plays: exactly its sample
/// count of interleaved stereo floats, refused past `MAX_PENDING_FRAMES`
/// before any is copied, and finite.
fn pcm(sample: &CMSampleBuffer) -> Result<(u64, Vec<f32>), FailureCode> {
    // SAFETY: an audio output's sample carries its count, time and data.
    let (count, time, data) = unsafe {
        (
            sample.num_samples(),
            sample.presentation_time_stamp(),
            sample.data_buffer(),
        )
    };
    let frames = usize::try_from(count)
        .ok()
        .filter(|frames| *frames <= MAX_PENDING_FRAMES)
        .ok_or(FailureCode::RenderResource)?;
    let at = (time.flags.0 & TIME_VALID != 0 && time.flags.0 & TIME_NOT_A_NUMBER == 0)
        .then(|| kettle_media::audio::frames_at(time.value, time.timescale))
        .flatten()
        .ok_or(FailureCode::RenderParse)?;
    let data = data.ok_or(FailureCode::RenderParse)?;
    let bytes = frames * CHANNELS * size_of::<f32>();
    // SAFETY: a plain property of the block buffer.
    if unsafe { data.data_length() } != bytes {
        return Err(FailureCode::RenderParse);
    }
    let mut samples = vec![0f32; frames * CHANNELS];
    if bytes > 0 {
        let destination = std::ptr::NonNull::new(samples.as_mut_ptr().cast::<std::ffi::c_void>())
            .ok_or(FailureCode::RenderResource)?;
        // SAFETY: `samples` holds exactly `bytes` bytes, the block's length.
        if unsafe { data.copy_data_bytes(0, bytes, destination) } != 0 {
            return Err(FailureCode::RenderParse);
        }
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(FailureCode::RenderParse);
    }
    Ok((at, samples))
}

impl AudioSource for AudioFrames {
    fn next_block(&mut self, samples: &mut [f32]) -> Result<Option<AudioBlock>, FailureCode> {
        while self.pending.is_empty() {
            if !self.refill()? {
                return Ok(None);
            }
        }
        let frames = (samples.len() / CHANNELS).min(self.pending.len() / CHANNELS);
        samples[..frames * CHANNELS].copy_from_slice(&self.pending[..frames * CHANNELS]);
        self.pending.drain(..frames * CHANNELS);
        let block = AudioBlock {
            frames,
            at_frame: self.pending_at,
        };
        self.pending_at += frames as u64;
        self.next_frame = self.pending_at;
        Ok(Some(block))
    }
}

impl VideoPlayer for AvFoundation {
    /// One reader from the start asked for to the end. A start past the
    /// video's last frame shows that frame, though the asset's sound runs
    /// on. AVFoundation times each frame, and gives every one: those kept
    /// stay within the rate asked ([`FrameSlots`]). Its sound, when asked
    /// for, comes from a reader of its own over the same asset, from the
    /// same start.
    fn play(
        &self,
        input: VideoInput<'_>,
        request: PlayRequest,
        deadline: Instant,
    ) -> Result<Playback, FailureCode> {
        let asset = Arc::new(Keep(open(&input, deadline)?));
        // SAFETY: open loaded the track's preferredTransform key.
        let transform = unsafe { asset.video.preferredTransform() };
        // The copy path supports only axis-aligned scaling and quarter turns.
        if !((transform.b == 0.0 && transform.c == 0.0)
            || (transform.a == 0.0 && transform.d == 0.0))
        {
            return Err(FailureCode::BackendUnavailable);
        }
        let (info, mirrored) = (asset.info, asset.mirrored);
        let (width, height) = play_size(
            info.width,
            info.height,
            request.max_width,
            request.max_height,
        )
        .ok_or(FailureCode::RenderResource)?;
        let fps_milli = info
            .fps_milli
            .map_or(request.max_fps_milli, |fps| fps.min(request.max_fps_milli));
        // The video's own frame length, not the rate asked: a slower rate
        // would start a frame early, and keep that frame for the last.
        let frame_ms = 1_000_000u64.div_ceil(u64::from(info.fps_milli.unwrap_or(fps_milli)));
        let start_ms = request
            .start_ms
            .min(asset.video_end_ms.saturating_sub(frame_ms));
        let audio = match (request.audio, &asset.audio) {
            (AudioRequest::Off, _) | (_, None) => None,
            (_, Some(track)) => AudioFrames::start(&asset, track, start_ms),
        };
        let opened = Opened {
            info,
            width,
            height,
            fps_milli,
            start_ms,
            audio: match (&audio, request.audio) {
                (_, AudioRequest::Off) => AudioStatus::Off,
                (Some(_), _) => AudioStatus::Ready,
                // A track no reader could take: its form is not one this
                // machine plays.
                (None, _) if info.has_audio => AudioStatus::Unsupported,
                (None, _) => AudioStatus::NoTrack,
            },
        };
        opened.validate().map_err(|_| FailureCode::RenderParse)?;
        let coded = if info.rotation % 180 == 90 {
            (height, width)
        } else {
            (width, height)
        };
        let output = track_output(&asset, coded.0, coded.1);
        // SAFETY: the reader is made for the loaded asset, and its time
        // range and output are set before it starts.
        let reader = unsafe { AVAssetReader::assetReaderWithAsset_error(&asset.asset) }
            .map_err(|_| FailureCode::BackendUnavailable)?;
        // SAFETY: as above.
        unsafe {
            reader.setTimeRange(CMTimeRange {
                start: time(start_ms),
                duration: time(info.duration_ms - start_ms),
            });
            if !reader.canAddOutput(&output) {
                return Err(FailureCode::BackendUnavailable);
            }
            reader.addOutput(&output);
            if !reader.startReading() {
                return Err(failure(reader.error().as_deref()));
            }
        }
        Ok(Playback {
            video_end_ms: Some(asset.video_end_ms),
            opened,
            frames: Box::new(Frames {
                reader,
                output,
                _asset: asset,
                coded,
                rotation: info.rotation,
                mirrored,
                duration_ms: info.duration_ms,
                slots: FrameSlots::new(fps_milli),
            }),
            audio: audio.map(|audio| Box::new(audio) as Box<dyn AudioSource>),
        })
    }
}

/// A session's frames from one reader.
struct Frames {
    reader: Retained<AVAssetReader>,
    output: Retained<AVAssetReaderTrackOutput>,
    /// Held while the reader reads through its descriptor.
    _asset: Arc<Keep>,
    /// Each frame as decoded, and the quarter turns clockwise that show it.
    coded: (u32, u32),
    rotation: u16,
    mirrored: bool,
    duration_ms: u64,
    /// Which frames are kept, for the rate asked.
    slots: FrameSlots,
}

// SAFETY: a session's frames are taken by one thread at a time (the one the
// reader moves to once open), and AVFoundation's reader and its output are
// not tied to the thread that made them; nothing else holds them.
unsafe impl Send for Frames {}

/// One sample's outcome: a frame kept, one skipped, or the end.
enum Taken {
    Frame(u64),
    Skipped,
    End,
}

impl FrameSource for Frames {
    fn next_frame(
        &mut self,
        rgba: &mut [u8],
        _gate: &PlayGate,
    ) -> Result<Option<u64>, FailureCode> {
        if rgba.len() != self.coded.0 as usize * self.coded.1 as usize * 4 {
            return Err(FailureCode::BadParams);
        }
        loop {
            let taken = objc2::rc::autoreleasepool(|_| self.take(rgba))?;
            match taken {
                Taken::Frame(pts) => return Ok(Some(pts)),
                Taken::Skipped => {}
                Taken::End => return Ok(None),
            }
        }
    }
}

impl Frames {
    fn take(&mut self, rgba: &mut [u8]) -> Result<Taken, FailureCode> {
        // SAFETY: the output belongs to the started reader, which only this
        // thread reads.
        let Some(sample) = (unsafe { self.output.copyNextSampleBuffer() }) else {
            // SAFETY: plain properties of the reader.
            // Named in full: the crate's source check reads a method call
            // spelled `.status(` as a process being started.
            return match unsafe { AVAssetReader::status(&self.reader) } {
                AVAssetReaderStatus::Completed => Ok(Taken::End),
                AVAssetReaderStatus::Failed => {
                    Err(failure(unsafe { self.reader.error() }.as_deref()))
                }
                _ => Err(FailureCode::RenderParse),
            };
        };
        // SAFETY: a video output's sample carries its image and time.
        let (image, time) = unsafe { (sample.image_buffer(), sample.presentation_time_stamp()) };
        let pts_us = us(time)
            .and_then(|pts| u64::try_from(pts).ok())
            .unwrap_or(0);
        if !self.slots.keep(pts_us) {
            return Ok(Taken::Skipped);
        }
        let pts = ms(time)
            .and_then(|pts| u64::try_from(pts).ok())
            .unwrap_or(0)
            .min(self.duration_ms);
        let image = image.ok_or(FailureCode::RenderParse)?;
        copy_upright(&image, self.coded, self.rotation, self.mirrored, rgba)?;
        Ok(Taken::Frame(pts))
    }
}

/// Copy `image`, which must be exactly `coded` BGRA, into `rgba` as opaque
/// straight RGBA, turned `rotation` degrees clockwise to show it.
fn copy_upright(
    image: &CVPixelBuffer,
    coded: (u32, u32),
    rotation: u16,
    mirrored: bool,
    rgba: &mut [u8],
) -> Result<(), FailureCode> {
    if CVPixelBufferGetWidth(image) != coded.0 as usize
        || CVPixelBufferGetHeight(image) != coded.1 as usize
        || CVPixelBufferGetPixelFormatType(image) != kCVPixelFormatType_32BGRA
    {
        return Err(FailureCode::RenderParse);
    }
    // SAFETY: locking a pixel buffer for reading has no precondition; it is
    // unlocked below on every path.
    if unsafe { CVPixelBufferLockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly) }
        != kCVReturnSuccess
    {
        return Err(FailureCode::RenderResource);
    }
    let (width, height) = (coded.0 as usize, coded.1 as usize);
    let stride = CVPixelBufferGetBytesPerRow(image);
    let base = CVPixelBufferGetBaseAddress(image).cast::<u8>().cast_const();
    let copied = if base.is_null() || stride < width * 4 {
        Err(FailureCode::RenderParse)
    } else {
        // SAFETY: the locked buffer holds `height` rows `stride` bytes apart,
        // each at least `width * 4` bytes long.
        let source = unsafe { std::slice::from_raw_parts(base, stride * (height - 1) + width * 4) };
        turn(source, stride, coded, rotation, mirrored, rgba);
        Ok(())
    };
    // SAFETY: unlocking the lock taken above.
    unsafe { CVPixelBufferUnlockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly) };
    copied
}

/// Write `source`, `coded` BGRA pixels in rows `stride` bytes apart, into
/// `rgba` as opaque RGBA turned `rotation` degrees clockwise (0, 90, 180 or
/// 270): what shows at (x, y) is the coded pixel that turn takes there.
fn turn(
    source: &[u8],
    stride: usize,
    coded: (u32, u32),
    rotation: u16,
    mirrored: bool,
    rgba: &mut [u8],
) {
    let (width, height) = (coded.0 as usize, coded.1 as usize);
    let shown_width = if rotation % 180 == 90 { height } else { width };
    for (at, out) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (at % shown_width, at / shown_width);
        let (from_x, from_y) = match rotation {
            90 => (y, height - 1 - x),
            180 => (width - 1 - x, height - 1 - y),
            270 => (width - 1 - y, x),
            _ => (x, y),
        };
        // The turn reads the mirrored picture: undo the mirror too.
        let from_x = if mirrored { width - 1 - from_x } else { from_x };
        let from = from_y * stride + from_x * 4;
        *out = [source[from + 2], source[from + 1], source[from], 255];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3x2 coded picture, its pixels numbered 0 to 5 in reading order,
    /// shows turned each quarter clockwise as a person would draw it; rows
    /// past the picture's width in a longer stride are never read.
    #[test]
    fn frames_turn_upright_as_they_are_copied() {
        // BGRA: blue carries the number, padding rows carry 99.
        let stride = 3 * 4 + 4;
        let mut source = vec![99u8; stride * 2];
        for number in 0..6usize {
            let at = (number / 3) * stride + (number % 3) * 4;
            source[at..at + 4].copy_from_slice(&[number as u8, 0, 0, 200]);
        }
        let shown_as = |rotation, mirrored| {
            let mut rgba = vec![0; 6 * 4];
            turn(&source, stride, (3, 2), rotation, mirrored, &mut rgba);
            assert!(rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255));
            rgba.as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[2])
                .collect::<Vec<_>>()
        };
        let shown = |rotation| shown_as(rotation, false);
        // 0 1 2      3 0      5 4 3      2 5
        // 3 4 5      4 1      2 1 0      1 4
        //            5 2                 0 3
        assert_eq!(shown(0), [0, 1, 2, 3, 4, 5]);
        assert_eq!(shown(90), [3, 0, 4, 1, 5, 2]);
        assert_eq!(shown(180), [5, 4, 3, 2, 1, 0]);
        assert_eq!(shown(270), [2, 5, 1, 4, 0, 3]);
        // Mirrored first, then turned:
        // 2 1 0      5 2      3 4 5      0 3
        // 5 4 3      4 1      0 1 2      1 4
        //            3 0                 2 5
        assert_eq!(shown_as(0, true), [2, 1, 0, 5, 4, 3]);
        assert_eq!(shown_as(90, true), [5, 2, 4, 1, 3, 0]);
        assert_eq!(shown_as(180, true), [3, 4, 5, 0, 1, 2]);
        assert_eq!(shown_as(270, true), [0, 3, 1, 4, 2, 5]);
    }

    /// A transform that reflects shows the picture mirrored, then turned:
    /// left to right alone is mirrored upright, top to bottom is mirrored
    /// and turned half way, and a reflection across the diagonal is
    /// mirrored and turned a quarter.
    #[test]
    fn display_transforms_that_reflect_mirror_the_picture() {
        let transform = |a: f64, b: f64, c: f64, d: f64| CGAffineTransform {
            a,
            b,
            c,
            d,
            tx: 0.0,
            ty: 0.0,
        };
        assert_eq!(orientation(transform(1.0, 0.0, 0.0, 1.0)), (0, false));
        assert_eq!(orientation(transform(0.0, 1.0, -1.0, 0.0)), (90, false));
        assert_eq!(orientation(transform(-1.0, 0.0, 0.0, 1.0)), (0, true));
        assert_eq!(orientation(transform(1.0, 0.0, 0.0, -1.0)), (180, true));
        assert_eq!(orientation(transform(0.0, 1.0, 1.0, 0.0)), (270, true));
    }

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
