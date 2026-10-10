//! Content-based container identification shared by video callers, and the
//! seam a video decoder outside the renderer plugs into.

use std::fs::File;
use std::time::Instant;

use crate::{FailureCode, PathIdentity, StillSample, VideoInfo};

/// The environment variable the parent names the external decoder's ffmpeg
/// in, when it starts a worker. Only Kettle's own platform code sets it, so
/// no request can choose a binary.
pub const DECODER_ENV: &str = "KETTLE_MEDIA_DECODER";

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
