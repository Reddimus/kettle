//! Content-based container identification shared by video callers, and the
//! seam a video decoder outside the renderer plugs into.

use std::fs::File;
use std::time::Instant;

use crate::{FailureCode, PathIdentity, StillSample, StillsLayout, VideoInfo, VideoStills};

/// The environment variable the parent names the external decoder's ffmpeg
/// in, when it starts a worker. Only Kettle's own platform code sets it, so
/// no request can choose a binary.
pub const DECODER_ENV: &str = "KETTLE_MEDIA_DECODER";

/// The video decoders a worker on this platform has, in the order it tries
/// them: Apple's own (AVFoundation) on macOS, then the user's ffmpeg, which
/// the parent finds in fixed places when it starts a worker. None on Windows,
/// where no worker runs. Kettle ships and loads no GStreamer.
pub const DECODERS: &[&str] = if cfg!(target_os = "macos") {
    &["avfoundation", "ffmpeg"]
} else if cfg!(target_os = "linux") {
    &["ffmpeg"]
} else {
    &[]
};

/// A video decoder the worker supplies to the renderer, which itself starts
/// no process and links no codec.
pub trait VideoDecoder {
    /// Decode the stills `plan` asks for once it knows what the video is.
    /// `plan` turns the video's description into the instants to show and
    /// the size of each frame; frames come back in that order, at that size,
    /// each with the time of the frame actually shown.
    fn stills(
        &self,
        input: VideoInput<'_>,
        plan: &mut dyn FnMut(&VideoInfo) -> Result<StillsPlan, FailureCode>,
        deadline: Instant,
    ) -> Result<DecodedStills, FailureCode>;
}

/// The video to decode: the open file the renderer holds, what it was when
/// opened, and the container its first bytes say it is.
#[derive(Clone, Copy, Debug)]
pub struct VideoInput<'a> {
    pub file: &'a File,
    pub identity: PathIdentity,
    pub container: VideoContainer,
}

/// What a stills job wants from the decoder: instants in ms from the
/// video's start, in layout order, and the size every frame is drawn at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StillsPlan {
    pub times_ms: Vec<u64>,
    pub width: u32,
    pub height: u32,
}

/// A decoder's answer: the video, one frame per instant asked for, and how
/// far a frame's time may be from the instant it stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedStills {
    pub info: VideoInfo,
    pub frames: Vec<DecodedFrame>,
    pub tolerance_ms: u32,
}

/// One frame as straight RGBA at the plan's size, with its times.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedFrame {
    pub sample: StillSample,
    pub rgba: Vec<u8>,
}

/// Maximum prefix inspected without reading the rest of a video file.
pub const MAX_VIDEO_PREFIX_BYTES: usize = 64 * 1024;

/// A container or elementary-stream family. Track and codec support require decoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoContainer {
    /// ISO base media file format, including MP4.
    IsoBmff,
    /// QuickTime movie.
    QuickTime,
    /// Matroska container.
    Matroska,
    /// WebM container.
    WebM,
    /// RIFF AVI container.
    Avi,
    /// Flash Video container.
    FlashVideo,
    /// MPEG program stream.
    MpegProgramStream,
    /// MPEG elementary video stream.
    MpegVideo,
    /// MPEG transport stream, including M2TS.
    MpegTransportStream,
    /// Ogg container.
    Ogg,
    /// Advanced Systems Format container, including WMV.
    Asf,
}

impl VideoContainer {
    /// Every container, in declaration order.
    pub const ALL: [Self; 11] = [
        Self::IsoBmff,
        Self::QuickTime,
        Self::Matroska,
        Self::WebM,
        Self::Avi,
        Self::FlashVideo,
        Self::MpegProgramStream,
        Self::MpegVideo,
        Self::MpegTransportStream,
        Self::Ogg,
        Self::Asf,
    ];

    /// The extension a copy in this container is named with, so the program
    /// it is handed to reads it as what its bytes are. It comes from the
    /// sniffed bytes, never from the name the video arrived under.
    pub fn extension(self) -> &'static str {
        match self {
            Self::IsoBmff => "mp4",
            Self::QuickTime => "mov",
            Self::Matroska => "mkv",
            Self::WebM => "webm",
            Self::Avi => "avi",
            Self::FlashVideo => "flv",
            Self::MpegProgramStream => "mpg",
            Self::MpegVideo => "m2v",
            Self::MpegTransportStream => "ts",
            Self::Ogg => "ogv",
            Self::Asf => "asf",
        }
    }
}

/// Identify a container from the beginning of a file and its full length.
///
/// This opens no files, allocates nothing and inspects at most
/// [`MAX_VIDEO_PREFIX_BYTES`]. The inspected framing fields must be complete;
/// stream payloads and codecs are left to the decoder.
pub fn sniff_video_container(prefix: &[u8], file_len: u64) -> Option<VideoContainer> {
    if prefix.len() as u64 > file_len {
        return None;
    }
    let prefix = &prefix[..prefix.len().min(MAX_VIDEO_PREFIX_BYTES)];
    iso_bmff(prefix, file_len)
        .or_else(|| ebml(prefix))
        .or_else(|| riff_avi(prefix, file_len))
        .or_else(|| flash_video(prefix))
        .or_else(|| ogg(prefix, file_len))
        .or_else(|| asf(prefix, file_len))
        .or_else(|| mpeg(prefix))
        .or_else(|| transport_stream(prefix))
}

fn flash_video(prefix: &[u8]) -> Option<VideoContainer> {
    let header = prefix.get(..9)?;
    if &header[..4] != b"FLV\x01" || header[4] & !0x05 != 0 {
        return None;
    }
    let offset = usize::try_from(u32::from_be_bytes(header[5..9].try_into().ok()?)).ok()?;
    if offset < 9 || prefix.get(offset..offset.checked_add(4)?)? != [0; 4] {
        return None;
    }
    Some(VideoContainer::FlashVideo)
}

fn ogg(prefix: &[u8], file_len: u64) -> Option<VideoContainer> {
    let header = prefix.get(..27)?;
    if &header[..5] != b"OggS\0" || header[5] & !0x06 != 0 || header[5] & 0x02 == 0 {
        return None;
    }
    let end = 27 + usize::from(header[26]);
    let payload_len = prefix
        .get(27..end)?
        .iter()
        .map(|size| u64::from(*size))
        .sum::<u64>();
    (end as u64 + payload_len <= file_len).then_some(VideoContainer::Ogg)
}

fn asf(prefix: &[u8], file_len: u64) -> Option<VideoContainer> {
    const HEADER: &[u8; 16] = b"\x30\x26\xb2\x75\x8e\x66\xcf\x11\xa6\xd9\0\xaa\0\x62\xce\x6c";
    let header = prefix.get(..30)?;
    if &header[..16] != HEADER || header[28..] != [1, 2] {
        return None;
    }
    let size = u64::from_le_bytes(header[16..24].try_into().ok()?);
    (size >= 30 && size <= file_len).then_some(VideoContainer::Asf)
}

fn mpeg(prefix: &[u8]) -> Option<VideoContainer> {
    match prefix.get(..4)? {
        b"\0\0\x01\xba" => {
            let first = *prefix.get(4)?;
            if first & 0xf0 == 0x20 {
                let header = prefix.get(..12)?;
                (first & 1 != 0
                    && header[6] & 1 != 0
                    && header[8] & 1 != 0
                    && header[9] & 0x80 != 0
                    && header[11] & 1 != 0)
                    .then_some(VideoContainer::MpegProgramStream)
            } else if first & 0xc0 == 0x40 {
                let header = prefix.get(..14)?;
                let stuffing = prefix.get(14..14 + usize::from(header[13] & 7))?;
                (first & 4 != 0
                    && header[6] & 4 != 0
                    && header[8] & 4 != 0
                    && header[9] & 1 != 0
                    && header[12] & 3 == 3
                    && header[13] & 0xf8 == 0xf8
                    && stuffing.iter().all(|byte| *byte == 0xff))
                .then_some(VideoContainer::MpegProgramStream)
            } else {
                None
            }
        }
        b"\0\0\x01\xb3" => {
            let header = prefix.get(..12)?;
            let width = u16::from(header[4]) << 4 | u16::from(header[5] >> 4);
            let height = u16::from(header[5] & 0x0f) << 8 | u16::from(header[6]);
            (width != 0
                && height != 0
                && (1..=14).contains(&(header[7] >> 4))
                && (1..=8).contains(&(header[7] & 0x0f))
                && header[10] & 0x20 != 0)
                .then_some(VideoContainer::MpegVideo)
        }
        _ => None,
    }
}

fn transport_stream(prefix: &[u8]) -> Option<VideoContainer> {
    for (stride, offset) in [(188, 0), (192, 4)] {
        if let Some(packets) = prefix.get(..stride * 3)
            && packets
                .chunks_exact(stride)
                .all(|packet| packet[offset] == 0x47 && packet[offset + 3] & 0x30 != 0)
        {
            return Some(VideoContainer::MpegTransportStream);
        }
    }
    None
}

// RFC 8794: parse the complete EBML header, not a substring in its payload.
fn ebml(prefix: &[u8]) -> Option<VideoContainer> {
    if !prefix.starts_with(b"\x1a\x45\xdf\xa3") {
        return None;
    }
    let (size, width) = ebml_vint(prefix.get(4..)?, false)?;
    let start = 4 + width;
    let end = start.checked_add(usize::try_from(size).ok()?)?;
    let header = prefix.get(start..end)?;
    let mut offset = 0;
    let mut document_type = None;
    while offset < header.len() {
        let (id, width) = ebml_vint(header.get(offset..)?, true)?;
        offset += width;
        let (size, width) = ebml_vint(header.get(offset..)?, false)?;
        offset += width;
        let end = offset.checked_add(usize::try_from(size).ok()?)?;
        let value = header.get(offset..end)?;
        if id == 0x4282 {
            if document_type.is_some() {
                return None;
            }
            let name = value
                .iter()
                .rposition(|byte| *byte != 0)
                .map_or(&[][..], |end| &value[..=end]);
            document_type = Some(match name {
                b"matroska" => VideoContainer::Matroska,
                b"webm" => VideoContainer::WebM,
                _ => return None,
            });
        }
        offset = end;
    }
    document_type
}

fn ebml_vint(bytes: &[u8], is_id: bool) -> Option<(u64, usize)> {
    let first = *bytes.first()?;
    let width = first.leading_zeros() as usize + 1;
    if width > if is_id { 4 } else { 8 } {
        return None;
    }
    let marker = 1_u8 << (8 - width);
    let mut value = u64::from(first & (marker - 1));
    for byte in bytes.get(1..width)? {
        value = (value << 8) | u64::from(*byte);
    }
    // All-one data denotes an unknown size or a reserved ID; neither is a
    // finite header field. IDs keep their marker, whereas sizes remove it.
    if value == (1_u64 << (7 * width)) - 1 || (is_id && value == 0) {
        return None;
    }
    if is_id {
        value |= 1_u64 << (7 * width);
    }
    Some((value, width))
}

fn riff_avi(prefix: &[u8], file_len: u64) -> Option<VideoContainer> {
    let header = prefix.get(..12)?;
    if &header[..4] != b"RIFF" || &header[8..] != b"AVI " {
        return None;
    }
    // RIFF's little-endian size includes the form type, but not these first
    // eight bytes. The rest of the file need not fit in our prefix.
    let size = u64::from(u32::from_le_bytes(header[4..8].try_into().ok()?));
    (size >= 4 && size + 8 <= file_len).then_some(VideoContainer::Avi)
}

fn iso_bmff(prefix: &[u8], file_len: u64) -> Option<VideoContainer> {
    let mut offset = 0;
    loop {
        let (kind, payload_start, end) = atom(prefix, offset, file_len)?;
        match kind {
            b"free" | b"skip" | b"wide" => offset = usize::try_from(end).ok()?,
            b"ftyp" => {
                let payload = prefix.get(payload_start..usize::try_from(end).ok()?)?;
                if payload.len() < 8 || payload.len() % 4 != 0 {
                    return None;
                }
                let brands = || {
                    std::iter::once(&payload[..4]).chain(
                        payload[8..]
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|brand| brand.as_slice()),
                    )
                };
                if brands().any(image_brand) || !brands().any(movie_brand) {
                    return None;
                }
                return Some(if &payload[..4] == b"qt  " {
                    VideoContainer::QuickTime
                } else {
                    VideoContainer::IsoBmff
                });
            }
            b"moov" | b"mdat" => return Some(VideoContainer::QuickTime),
            _ => return None,
        }
    }
}

/// Atom headers can describe payloads beyond the prefix, but never the file.
fn atom(prefix: &[u8], offset: usize, file_len: u64) -> Option<(&[u8], usize, u64)> {
    let bytes = prefix.get(offset..)?;
    let short = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?);
    let kind = bytes.get(4..8)?;
    let (size, header) = match short {
        0 => (file_len.checked_sub(offset as u64)?, 8),
        1 => (u64::from_be_bytes(bytes.get(8..16)?.try_into().ok()?), 16),
        _ => (u64::from(short), 8),
    };
    if size < header as u64 {
        return None;
    }
    let end = (offset as u64).checked_add(size)?;
    if end > file_len {
        return None;
    }
    Some((kind, offset.checked_add(header)?, end))
}

// Registered file-type brands: https://mp4ra.org/registered-types/brands.
// A compatible base brand alone does not turn a still-image file into video.
fn image_brand(brand: &[u8]) -> bool {
    matches!(
        brand,
        b"avif"
            | b"avis"
            | b"mif1"
            | b"msf1"
            | b"heic"
            | b"heix"
            | b"heim"
            | b"heis"
            | b"hevc"
            | b"hevx"
            | b"hevm"
            | b"hevs"
            | b"avci"
            | b"avcs"
            | b"jpeg"
            | b"j2ki"
            | b"j2is"
            | b"jxl "
            | b"jp2 "
            | b"jpx "
            | b"jpm "
            | b"jpxb"
    )
}

fn movie_brand(brand: &[u8]) -> bool {
    matches!(
        brand,
        b"isom"
            | b"iso2"
            | b"iso3"
            | b"iso4"
            | b"iso5"
            | b"iso6"
            | b"iso7"
            | b"iso8"
            | b"iso9"
            | b"isoa"
            | b"isob"
            | b"isoc"
            | b"mp41"
            | b"mp42"
            | b"avc1"
            | b"av01"
            | b"qt  "
            | b"M4A "
            | b"M4B "
            | b"M4P "
            | b"M4V "
            | b"MSNV"
            | b"3g2a"
            | b"3ge6"
            | b"3ge7"
            | b"3ge9"
            | b"3gg6"
            | b"3gg9"
            | b"3gp4"
            | b"3gp5"
            | b"3gp6"
            | b"3gp7"
            | b"3gp8"
            | b"3gp9"
            | b"3gr6"
            | b"3gr9"
            | b"3gs6"
            | b"3gs9"
            | b"dash"
            | b"msdh"
            | b"msix"
            | b"cmfc"
            | b"cmfs"
            | b"mjp2"
            | b"F4V "
            | b"f4v "
    )
}

/// Space between a sheet's frames and around them, in pixels.
pub const SHEET_GAP: u32 = 4;

/// The instants `stills` asks for in a video `duration_ms` long, in the order
/// they are laid out: the one at `at_s`, or `count` evenly through the window
/// from `start_s` to `end_s` (the end without one), each the middle of its
/// share. Past the end is the end.
pub fn sample_times(duration_ms: u64, stills: &VideoStills) -> Vec<u64> {
    let ms = |seconds: f64| {
        let ms = (seconds * 1000.0).round();
        if ms >= duration_ms as f64 {
            duration_ms
        } else {
            ms.max(0.0) as u64
        }
    };
    if let Some(at) = stills.at_s {
        return vec![ms(at)];
    }
    let start = ms(stills.start_s);
    let end = stills.end_s.map_or(duration_ms, ms).max(start);
    let count = u64::from(stills.count.max(1));
    let span = end - start;
    (0..count)
        .map(|index| start + (span * (2 * index + 1)) / (2 * count))
        .collect()
}

/// The layout's grid, columns by rows.
pub fn grid(stills: &VideoStills) -> (u32, u32) {
    match stills.layout {
        StillsLayout::Poster => (1, 1),
        StillsLayout::Sheet { cols, .. } => {
            let cols = u32::from(cols.max(1));
            (cols, u32::from(stills.count).div_ceil(cols))
        }
    }
}

/// The size each frame of a picture `width` by `height` (as shown) is drawn
/// at: as large as the grid and its gaps allow within `max_edge` on a side,
/// never larger than the picture itself, its shape kept. `None` for an empty
/// picture.
pub fn tile_size(width: u32, height: u32, stills: &VideoStills) -> Option<(u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }
    let (cols, rows) = grid(stills);
    let gap = gap(stills);
    let room = |count: u32| {
        stills
            .max_edge
            .checked_sub(gap * (count + 1))
            .map(|left| left / count)
            .filter(|&room| room > 0)
    };
    let (room_w, room_h) = (room(cols)?, room(rows)?);
    let scale = (f64::from(room_w) / f64::from(width))
        .min(f64::from(room_h) / f64::from(height))
        .min(1.0);
    let tile = |side: u32| ((f64::from(side) * scale).floor() as u32).max(1);
    Some((tile(width), tile(height)))
}

/// The space between a layout's frames: none for a poster.
pub fn gap(stills: &VideoStills) -> u32 {
    match stills.layout {
        StillsLayout::Poster => 0,
        StillsLayout::Sheet { .. } => SHEET_GAP,
    }
}

/// Where frame `index` of the sheet `stills` lays out, with frames `tile`
/// large, has its top-left corner, in the order frames are laid out.
pub fn tile_origin(stills: &VideoStills, tile: (u32, u32), index: u32) -> (u32, u32) {
    let (cols, _) = grid(stills);
    let gap = gap(stills);
    (
        gap + (index % cols) * (tile.0 + gap),
        gap + (index / cols) * (tile.1 + gap),
    )
}

/// The whole sheet `stills` lays out with frames `tile` large: every frame,
/// the gaps between them and the margin around them.
pub fn sheet_size(stills: &VideoStills, tile: (u32, u32)) -> (u32, u32) {
    let (cols, rows) = grid(stills);
    let gap = gap(stills);
    (
        cols * tile.0 + gap * (cols + 1),
        rows * tile.1 + gap * (rows + 1),
    )
}

/// The largest `max_edge`, up to [`crate::MAX_VIDEO_JOB_EDGE`], at which
/// [`tile_size`] gives each frame of `stills`'s layout, for a picture
/// `width` by `height` (as shown), at most `longest` pixels on each side:
/// frames as large as that allows, never larger than the picture. Tiles
/// grow in steps (whole pixels of room, floored), so this searches rather
/// than solves. `None` when no edge fits a frame.
pub fn edge_for_tiles(stills: &VideoStills, width: u32, height: u32, longest: u32) -> Option<u32> {
    let fits = |max_edge| {
        tile_size(
            width,
            height,
            &VideoStills {
                max_edge,
                ..*stills
            },
        )
        .is_some_and(|(tile_w, tile_h)| tile_w <= longest && tile_h <= longest)
    };
    // An edge too small for the margins gives no tile at all. From the
    // smallest that gives one, tiles never shrink as the edge grows: find
    // where they stop fitting.
    let (cols, rows) = grid(stills);
    let gap = gap(stills);
    let smallest = (gap * (cols + 1) + cols).max(gap * (rows + 1) + rows);
    if smallest > crate::MAX_VIDEO_JOB_EDGE || !fits(smallest) {
        return None;
    }
    let (mut fitting, mut over) = (smallest, crate::MAX_VIDEO_JOB_EDGE + 1);
    while over - fitting > 1 {
        let edge = fitting + (over - fitting) / 2;
        if fits(edge) {
            fitting = edge;
        } else {
            over = edge;
        }
    }
    Some(fitting)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(count: u8, cols: u8) -> VideoStills {
        VideoStills {
            count,
            max_edge: 1568,
            start_s: 0.0,
            end_s: None,
            at_s: None,
            layout: StillsLayout::Sheet {
                cols,
                labels: false,
            },
        }
    }

    /// Frames sit after the margin, one tile and one gap apart, row by row,
    /// and the sheet holds them all with its margin round them.
    #[test]
    fn frames_sit_where_the_sheet_puts_them() {
        let stills = sheet(8, 4);
        assert_eq!(tile_origin(&stills, (384, 216), 0), (4, 4));
        assert_eq!(tile_origin(&stills, (384, 216), 3), (4 + 3 * 388, 4));
        assert_eq!(tile_origin(&stills, (384, 216), 4), (4, 4 + 220));
        assert_eq!(tile_origin(&stills, (384, 216), 7), (4 + 3 * 388, 4 + 220));
        assert_eq!(sheet_size(&stills, (384, 216)), (1556, 444));
        let poster = VideoStills {
            count: 1,
            layout: StillsLayout::Poster,
            ..stills
        };
        assert_eq!(tile_origin(&poster, (640, 360), 0), (0, 0));
        assert_eq!(sheet_size(&poster, (640, 360)), (640, 360));
    }

    /// The edge found for a longest side gives frames that fit it, as large
    /// as it allows, never larger than the picture, for every shape.
    #[test]
    fn an_edge_for_tiles_fits_them_to_the_longest_side() {
        let layout = sheet(8, 4);
        let tiles = |width, height| {
            let max_edge = edge_for_tiles(&layout, width, height, 384).unwrap();
            assert!(max_edge <= crate::MAX_VIDEO_JOB_EDGE);
            tile_size(width, height, &VideoStills { max_edge, ..layout }).unwrap()
        };
        assert_eq!(tiles(1920, 1080), (384, 216));
        assert_eq!(tiles(1080, 1920), (216, 384));
        assert_eq!(tiles(1000, 1000), (384, 384));
        assert_eq!(tiles(320, 180), (320, 180), "never larger than the picture");
        // The largest edge that fits: one more would not, or would change
        // nothing because the picture is already whole.
        for width in (1..4000).step_by(97) {
            for height in (1..4000).step_by(89) {
                let (tile_w, tile_h) = tiles(width, height);
                assert!(tile_w <= 384 && tile_h <= 384, "{width}x{height}");
                let max_edge = edge_for_tiles(&layout, width, height, 384).unwrap();
                let next = tile_size(
                    width,
                    height,
                    &VideoStills {
                        max_edge: max_edge + 1,
                        ..layout
                    },
                )
                .unwrap();
                assert!(
                    next.0 > 384 || next.1 > 384 || next == (tile_w, tile_h),
                    "{width}x{height}: {tile_w}x{tile_h} at {max_edge}, {next:?} past it"
                );
            }
        }
        // Where flooring makes a closed form overshoot.
        assert!(tiles(268, 535).1 <= 384);
        assert_eq!(edge_for_tiles(&layout, 0, 10, 384), None);
        assert_eq!(edge_for_tiles(&layout, 10, 10, 0), None);
        // Tiles fit only between an edge with room for the margins and one
        // past the longest side: the search starts where tiles begin.
        let tiny = edge_for_tiles(&layout, 1000, 1000, 1).unwrap();
        assert_eq!(
            tile_size(
                1000,
                1000,
                &VideoStills {
                    max_edge: tiny,
                    ..layout
                }
            ),
            Some((1, 1))
        );
        assert!(
            tile_size(
                1000,
                1000,
                &VideoStills {
                    max_edge: tiny + 4,
                    ..layout
                }
            )
            .is_some_and(|(width, _)| width > 1)
        );
    }
}
