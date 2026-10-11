//! Animated images as stills: GIF, APNG and animated WebP, decoded by the
//! image crate's animation decoders, never a video decoder. Frame delays
//! are known only by decoding, so an animation is decoded twice within the
//! job's deadline: once for its timeline, then again keeping just the frames
//! the stills show, each resized to its tile as it is kept, so no more than
//! one full frame and the kept tiles are held at once.

use std::io::Cursor;
use std::time::Instant;

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, Frames, ImageDecoder as _, ImageError, ImageFormat, Limits};
use kettle_media::{
    FailureCode, MAX_DECODED_BYTES, MAX_DECODED_EDGE, StillSample, VideoCodec, VideoInfo,
    VideoStills, rgba_len,
};

use crate::stills::{Still, sample_times, tile_size};

/// Most frames an animation may have.
const MAX_FRAMES: usize = 10_000;
/// Most frame pixels decoding may produce, both passes together: about two
/// hundred full 4K frames.
const MAX_DECODED_PIXELS: u64 = 2_000_000_000;
/// How long a GIF frame of less than 20 ms is shown, as browsers show it.
const GIF_SHORT_DELAY_MS: u64 = 100;
/// How long a frame with no delay is shown in an APNG or WebP.
const ZERO_DELAY_MS: u64 = 100;

/// The animation's frames `stills` asks for, at its tile size, with what it
/// is and the farthest any frame's start is from its instant. A PNG that
/// does not animate is no animation.
pub(crate) fn frames(
    bytes: &[u8],
    stills: &VideoStills,
    deadline: Instant,
) -> Result<(VideoInfo, Vec<Still>, u32), FailureCode> {
    let format = image::guess_format(bytes).map_err(|_| FailureCode::UnsupportedMedia)?;
    crate::container::check(format, bytes)?;
    let (width, height, codec) = describe(format, bytes)?;
    rgba_len(width, height, MAX_DECODED_EDGE, MAX_DECODED_BYTES)
        .map_err(|_| FailureCode::RenderResource)?;
    let frame_pixels = u64::from(width) * u64::from(height);
    let mut decoded = 0u64;
    let mut spend = |deadline: Instant| -> Result<(), FailureCode> {
        decoded = decoded.saturating_add(frame_pixels);
        if decoded > MAX_DECODED_PIXELS {
            return Err(FailureCode::RenderResource);
        }
        if Instant::now() >= deadline {
            return Err(FailureCode::RenderTimeout);
        }
        Ok(())
    };
    // The timeline: each frame's delay.
    let mut delays = Vec::new();
    let mut timeline = open(format, bytes)?;
    while let Some(frame) = next(&mut timeline)? {
        spend(deadline)?;
        if delays.len() == MAX_FRAMES {
            return Err(FailureCode::RenderResource);
        }
        let (numer, denom) = frame.delay().numer_denom_ms();
        let ms = if denom == 0 {
            0
        } else {
            u64::from(numer) / u64::from(denom)
        };
        delays.push(match (format, ms) {
            (ImageFormat::Gif, ms) if ms < 20 => GIF_SHORT_DELAY_MS,
            (_, 0) => ZERO_DELAY_MS,
            (_, ms) => ms,
        });
    }
    if delays.is_empty() {
        return Err(FailureCode::RenderParse);
    }
    // A still image shows for no time at all.
    let duration_ms = if delays.len() == 1 {
        0
    } else {
        delays.iter().sum()
    };
    let starts: Vec<u64> = delays
        .iter()
        .scan(0u64, |at, delay| {
            let start = *at;
            *at += delay;
            Some(start)
        })
        .collect();
    let times = sample_times(duration_ms, stills);
    // The frame showing at each time: the last whose start is not after it.
    let shown: Vec<usize> = times
        .iter()
        .map(|&time| {
            starts
                .partition_point(|&start| start <= time)
                .saturating_sub(1)
        })
        .collect();
    let (tile_w, tile_h) = tile_size(width, height, stills).ok_or(FailureCode::RenderResource)?;
    let mut kept: Vec<Option<Vec<u8>>> = vec![None; delays.len()];
    let last = *shown.iter().max().unwrap_or(&0);
    let mut again = open(format, bytes)?;
    for (index, slot) in kept.iter_mut().enumerate().take(last + 1) {
        let frame = next(&mut again)?.ok_or(FailureCode::RenderParse)?;
        spend(deadline)?;
        if shown.contains(&index) {
            // Premultiplied, so a transparent pixel's hidden color stays
            // out of its neighbours.
            let tile = crate::raster::resample(frame.buffer(), tile_w, tile_h)?;
            *slot = Some(tile.into_raw());
        }
    }
    let stills = times
        .iter()
        .zip(&shown)
        .map(|(&requested_ms, &index)| {
            let rgba = kept[index].clone().ok_or(FailureCode::RenderParse)?;
            Ok(Still {
                sample: StillSample {
                    requested_ms,
                    actual_ms: starts[index].min(duration_ms),
                },
                width: tile_w,
                height: tile_h,
                rgba,
            })
        })
        .collect::<Result<Vec<_>, FailureCode>>()?;
    let even = delays.iter().all(|&delay| delay == delays[0]);
    let info = VideoInfo {
        duration_ms,
        width,
        height,
        rotation: 0,
        codec,
        fps_milli: (even && delays.len() > 1)
            .then(|| u32::try_from(1_000_000 / delays[0]).ok())
            .flatten()
            .filter(|&fps| fps > 0),
        has_audio: false,
        container: None,
    };
    let farthest = stills
        .iter()
        .map(|still| still.sample.requested_ms.abs_diff(still.sample.actual_ms))
        .max()
        .unwrap_or(0);
    Ok((info, stills, u32::try_from(farthest).unwrap_or(u32::MAX)))
}

/// The next frame, decoded under the panic guard: a decoder's bug on input
/// nothing foresaw is a parse failure.
fn next(frames: &mut Frames<'_>) -> Result<Option<image::Frame>, FailureCode> {
    crate::guarded(FailureCode::RenderParse, || {
        frames.next().transpose().map_err(decode_failure)
    })
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODED_EDGE);
    limits.max_image_height = Some(MAX_DECODED_EDGE);
    limits.max_alloc = u64::try_from(MAX_DECODED_BYTES).ok();
    limits
}

/// The animation's canvas and codec, before any frame is decoded. A PNG
/// that does not animate is no animation.
fn describe(format: ImageFormat, bytes: &[u8]) -> Result<(u32, u32, VideoCodec), FailureCode> {
    let (dimensions, codec) = match format {
        ImageFormat::Gif => {
            let mut decoder = GifDecoder::new(Cursor::new(bytes)).map_err(decode_failure)?;
            decoder.set_limits(limits()).map_err(decode_failure)?;
            (decoder.dimensions(), VideoCodec::Gif)
        }
        ImageFormat::Png => {
            let decoder =
                PngDecoder::with_limits(Cursor::new(bytes), limits()).map_err(decode_failure)?;
            if !decoder.is_apng().map_err(decode_failure)? {
                return Err(FailureCode::UnsupportedMedia);
            }
            (decoder.dimensions(), VideoCodec::Apng)
        }
        ImageFormat::WebP => {
            let mut decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(decode_failure)?;
            decoder.set_limits(limits()).map_err(decode_failure)?;
            (decoder.dimensions(), VideoCodec::WebP)
        }
        _ => return Err(FailureCode::UnsupportedMedia),
    };
    Ok((dimensions.0, dimensions.1, codec))
}

/// The animation's frames, composited onto its canvas, one at a time.
fn open(format: ImageFormat, bytes: &[u8]) -> Result<Frames<'_>, FailureCode> {
    crate::guarded(FailureCode::RenderParse, || {
        Ok(match format {
            ImageFormat::Gif => {
                let mut decoder = GifDecoder::new(Cursor::new(bytes)).map_err(decode_failure)?;
                decoder.set_limits(limits()).map_err(decode_failure)?;
                decoder.into_frames()
            }
            ImageFormat::Png => PngDecoder::with_limits(Cursor::new(bytes), limits())
                .map_err(decode_failure)?
                .apng()
                .map_err(decode_failure)?
                .into_frames(),
            ImageFormat::WebP => {
                let mut decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(decode_failure)?;
                decoder.set_limits(limits()).map_err(decode_failure)?;
                decoder.into_frames()
            }
            _ => return Err(FailureCode::UnsupportedMedia),
        })
    })
}

fn decode_failure(error: ImageError) -> FailureCode {
    match error {
        ImageError::Limits(_) => FailureCode::RenderResource,
        ImageError::Unsupported(_) => FailureCode::UnsupportedMedia,
        _ => FailureCode::RenderParse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame, Rgba, RgbaImage};
    use kettle_media::StillsLayout;
    use std::time::Duration;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];

    fn gif(frames: &[([u8; 4], u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut out);
            encoder
                .set_repeat(image::codecs::gif::Repeat::Infinite)
                .unwrap();
            for &(color, ms) in frames {
                let buffer = RgbaImage::from_pixel(8, 6, Rgba(color));
                encoder
                    .encode_frame(Frame::from_parts(
                        buffer,
                        0,
                        0,
                        Delay::from_numer_denom_ms(ms, 1),
                    ))
                    .unwrap();
            }
        }
        out
    }

    fn apng(frames: &[([u8; 4], u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, 8, 6);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_animated(frames.len() as u32, 0).unwrap();
            let mut writer = encoder.write_header().unwrap();
            for &(color, ms) in frames {
                writer.set_frame_delay(ms, 1000).unwrap();
                writer.write_image_data(&color.repeat(48)).unwrap();
            }
        }
        out
    }

    fn sheet(count: u8) -> VideoStills {
        VideoStills {
            count,
            max_edge: 256,
            start_s: 0.0,
            end_s: None,
            at_s: None,
            layout: StillsLayout::Sheet {
                cols: count,
                labels: false,
            },
        }
    }

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    /// Each still is the frame showing at its time, with that frame's start
    /// as its time: the timeline is the sum of the delays, and the tolerance
    /// the longest of them.
    #[test]
    fn each_still_is_the_frame_showing_at_its_time() {
        let bytes = gif(&[(RED, 100), (GREEN, 200), (BLUE, 300), (WHITE, 400)]);
        let (info, stills, tolerance) = frames(&bytes, &sheet(4), later()).unwrap();
        assert_eq!(
            (
                info.duration_ms,
                info.width,
                info.height,
                info.codec,
                info.container
            ),
            (1000, 8, 6, VideoCodec::Gif, None)
        );
        assert_eq!(info.fps_milli, None, "uneven delays have no rate");
        assert_eq!(tolerance, 275, "875 ms shows the frame starting at 600");
        // Midpoints 125, 375, 625 and 875 fall in the second, third, fourth
        // and fourth frames.
        assert_eq!(
            stills
                .iter()
                .map(|still| (still.sample.requested_ms, still.sample.actual_ms))
                .collect::<Vec<_>>(),
            [(125, 100), (375, 300), (625, 600), (875, 600)]
        );
        let colors: Vec<[u8; 4]> = stills
            .iter()
            .map(|still| still.rgba[..4].try_into().unwrap())
            .collect();
        assert_eq!(colors, [GREEN, BLUE, WHITE, WHITE]);
        assert!(
            stills
                .iter()
                .all(|still| (still.width, still.height) == (8, 6))
        );
        let at = VideoStills {
            at_s: Some(0.05),
            count: 1,
            layout: StillsLayout::Poster,
            ..sheet(1)
        };
        let (_, poster, _) = frames(&bytes, &at, later()).unwrap();
        assert_eq!(
            (poster[0].sample.actual_ms, &poster[0].rgba[..4]),
            (0, &RED[..])
        );
    }

    /// Delays are as players show them: a GIF frame under 20 ms for 100 ms,
    /// a frame of no delay in APNG for 100 ms; even delays give a frame
    /// rate; a single frame is a still image of no duration.
    #[test]
    fn delays_follow_how_players_show_them() {
        let short = gif(&[(RED, 10), (GREEN, 10)]);
        let (info, _, tolerance) = frames(&short, &sheet(2), later()).unwrap();
        assert_eq!(
            (info.duration_ms, tolerance, info.fps_milli),
            (200, 50, Some(10_000))
        );
        let one = gif(&[(BLUE, 500)]);
        let (info, stills, _) = frames(&one, &sheet(3), later()).unwrap();
        assert_eq!(info.duration_ms, 0);
        assert!(stills.iter().all(|still| still.sample.actual_ms == 0));
        assert_eq!(info.fps_milli, None);
        let animated = apng(&[(RED, 0), (GREEN, 250)]);
        let (info, stills, _) = frames(&animated, &sheet(2), later()).unwrap();
        assert_eq!((info.codec, info.duration_ms), (VideoCodec::Apng, 350));
        assert_eq!(&stills[1].rgba[..4], &GREEN[..]);
    }

    /// A frame is scaled with premultiplied alpha: what a transparent
    /// pixel's hidden color is changes nothing shown.
    #[test]
    fn hidden_colors_stay_hidden_when_frames_shrink() {
        let frame = |hidden: [u8; 3]| {
            let mut out = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut out, 4, 2);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_animated(2, 0).unwrap();
                let mut writer = encoder.write_header().unwrap();
                let pixel = |x: usize| {
                    if x.is_multiple_of(2) {
                        [0, 0, 255, 255]
                    } else {
                        [hidden[0], hidden[1], hidden[2], 0]
                    }
                };
                let image: Vec<u8> = (0..8).flat_map(|at| pixel(at % 4)).collect();
                for _ in 0..2 {
                    writer.set_frame_delay(100, 1000).unwrap();
                    writer.write_image_data(&image).unwrap();
                }
            }
            out
        };
        let poster = VideoStills {
            count: 1,
            max_edge: 2,
            start_s: 0.0,
            end_s: None,
            at_s: Some(0.0),
            layout: StillsLayout::Poster,
        };
        let (_, white, _) = frames(&frame([255, 255, 255]), &poster, later()).unwrap();
        let (_, red, _) = frames(&frame([255, 0, 0]), &poster, later()).unwrap();
        assert_eq!(white[0].rgba, red[0].rgba);
        assert!(
            white[0]
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[..3] == [0, 0, 255])
        );
    }

    /// An animated WebP plays like the others; a PNG that does not animate
    /// is no animation; the deadline holds.
    #[test]
    fn webp_plays_and_still_pngs_and_late_jobs_are_refused() {
        let webp = include_bytes!("../tests/fixtures/stills/anim.webp");
        let (info, stills, _) = frames(webp, &sheet(3), later()).unwrap();
        assert_eq!(
            (info.codec, info.duration_ms, info.width, info.height),
            (VideoCodec::WebP, 1000, 16, 12)
        );
        let colors: Vec<[u8; 4]> = stills
            .iter()
            .map(|still| still.rgba[..4].try_into().unwrap())
            .collect();
        // The decoder blends each frame onto the last, rounding a channel by
        // a step.
        for (color, want) in colors.iter().zip([RED, GREEN, BLUE]) {
            assert!(
                color
                    .iter()
                    .zip(want)
                    .all(|(got, want)| got.abs_diff(want) <= 2),
                "{color:?} for {want:?}"
            );
        }
        let mut still = Vec::new();
        image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 4, Rgba(RED)))
            .write_to(&mut Cursor::new(&mut still), ImageFormat::Png)
            .unwrap();
        assert_eq!(
            frames(&still, &sheet(1), later()).unwrap_err(),
            FailureCode::UnsupportedMedia
        );
        let bytes = gif(&[(RED, 100), (GREEN, 100)]);
        assert_eq!(
            frames(&bytes, &sheet(2), Instant::now()).unwrap_err(),
            FailureCode::RenderTimeout
        );
        assert_eq!(
            frames(b"GIF89a broken", &sheet(1), later()).unwrap_err(),
            FailureCode::RenderParse
        );
    }
}
