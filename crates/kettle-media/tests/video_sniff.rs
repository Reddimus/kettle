use kettle_media::video::{MAX_VIDEO_PREFIX_BYTES, VideoContainer, sniff_video_container};

const MP4: &[u8] = include_bytes!("../../kettle-ui/testdata/video-preview.mp4");

#[test]
fn a_real_mp4_is_identified_by_its_header() {
    assert_eq!(
        sniff_video_container(MP4, MP4.len() as u64),
        Some(VideoContainer::IsoBmff)
    );
}

fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
    let mut bytes = ((16 + compatible.len() * 4) as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(b"ftyp");
    bytes.extend_from_slice(major);
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    for brand in compatible {
        bytes.extend_from_slice(*brand);
    }
    bytes
}

fn sniff(bytes: &[u8]) -> Option<VideoContainer> {
    sniff_video_container(bytes, bytes.len() as u64)
}

#[test]
fn iso_brands_distinguish_movies_from_still_images() {
    assert_eq!(sniff(&ftyp(b"mp42", &[])), Some(VideoContainer::IsoBmff));
    assert_eq!(
        sniff(&ftyp(b"????", &[b"isom"])),
        Some(VideoContainer::IsoBmff)
    );
    assert_eq!(sniff(&ftyp(b"qt  ", &[])), Some(VideoContainer::QuickTime));
    // M4A is a container brand that can carry video, not proof of audio-only.
    assert_eq!(
        sniff(&ftyp(b"M4A ", &[b"isom"])),
        Some(VideoContainer::IsoBmff)
    );
    for brand in [b"avif", b"heic", b"mif1", b"jxl "] {
        assert_eq!(sniff(&ftyp(brand, &[b"isom"])), None);
        assert_eq!(sniff(&ftyp(b"isom", &[brand])), None);
    }
    assert_eq!(sniff(&ftyp(b"????", &[])), None);
    let mut minor = ftyp(b"isom", &[]);
    minor[12..16].copy_from_slice(b"avif");
    assert_eq!(sniff(&minor), Some(VideoContainer::IsoBmff));
}

#[test]
fn complete_file_type_boxes_can_follow_padding_or_use_large_sizes() {
    let mut padded = b"\0\0\0\x0cfree1234\0\0\0\x08wide".to_vec();
    padded.extend_from_slice(&ftyp(b"mp42", &[]));
    assert_eq!(sniff(&padded), Some(VideoContainer::IsoBmff));
    let mut large = b"\0\0\0\x01ftyp".to_vec();
    large.extend_from_slice(&24_u64.to_be_bytes());
    large.extend_from_slice(b"isom\0\0\0\0");
    assert_eq!(sniff(&large), Some(VideoContainer::IsoBmff));
    let mut to_end = ftyp(b"mp42", &[]);
    to_end[..4].fill(0);
    assert_eq!(sniff(&to_end), Some(VideoContainer::IsoBmff));
    assert_eq!(
        sniff_video_container(&MP4[..32], MP4.len() as u64),
        Some(VideoContainer::IsoBmff)
    );
}

#[test]
fn partial_or_inconsistent_atom_headers_are_not_identified() {
    let normal = ftyp(b"mp42", &[b"isom"]);
    for end in 0..normal.len() {
        assert_eq!(sniff(&normal[..end]), None, "truncated file at {end}");
        assert_eq!(
            sniff_video_container(&normal[..end], normal.len() as u64),
            None,
            "truncated prefix at {end}"
        );
    }
    assert_eq!(sniff_video_container(&normal, 8), None);
    for size in [2, 7, 17, 100] {
        let mut malformed = normal.clone();
        malformed[..4].copy_from_slice(&u32::to_be_bytes(size));
        assert_eq!(sniff(&malformed), None, "box size {size}");
    }
    let mut extended = b"\0\0\0\x01ftyp".to_vec();
    extended.extend_from_slice(&u64::MAX.to_be_bytes());
    assert_eq!(sniff(&extended), None);
}

fn ebml(children: &[u8]) -> Vec<u8> {
    assert!(children.len() < 127);
    let mut bytes = b"\x1a\x45\xdf\xa3".to_vec();
    bytes.push(0x80 | children.len() as u8);
    bytes.extend_from_slice(children);
    bytes
}

#[test]
fn ebml_document_type_is_parsed_as_an_element() {
    assert_eq!(
        sniff(&ebml(b"\x42\x86\x81\x01\x42\x82\x88matroska\xec\x80")),
        Some(VideoContainer::Matroska)
    );
    assert_eq!(
        sniff(&ebml(b"\x42\x82\x86webm\0\0")),
        Some(VideoContainer::WebM)
    );
    // A DocType-shaped value inside an opaque element is not a DocType.
    assert_eq!(sniff(&ebml(b"\xec\x87\x42\x82\x84webm")), None);
    assert_eq!(sniff(&ebml(b"\x42\x82\x84WEBM")), None);
    assert_eq!(sniff(&ebml(b"\x42\x82\x84webm\x42\x82\x84webm")), None);
    assert_eq!(sniff(&ebml(b"\x42\x82\x86webm\0x")), None);
}

#[test]
fn ebml_requires_the_complete_declared_header() {
    let bytes = ebml(b"\x42\x82\x84webm\xec\x80");
    for end in 0..bytes.len() {
        assert_eq!(sniff(&bytes[..end]), None, "file truncated at {end}");
        assert_eq!(
            sniff_video_container(&bytes[..end], bytes.len() as u64),
            None,
            "prefix truncated at {end}"
        );
    }
    assert_eq!(sniff(b"\x1a\x45\xdf\xa3\xff\x42\x82\x84webm"), None);
    assert_eq!(sniff(&ebml(b"\x42\x82\xffwebm")), None);
    assert_eq!(sniff(&ebml(b"\x42\x82\x89webm")), None);
}

#[test]
fn riff_identifies_avi_without_confusing_audio_or_images() {
    assert_eq!(sniff(b"RIFF\x04\0\0\0AVI "), Some(VideoContainer::Avi));
    assert_eq!(
        sniff_video_container(b"RIFF\x10\0\0\0AVI ", 24),
        Some(VideoContainer::Avi)
    );
    for form in [b"WAVE", b"WEBP", b"AVIX"] {
        let mut bytes = b"RIFF\x04\0\0\0".to_vec();
        bytes.extend_from_slice(form);
        assert_eq!(sniff(&bytes), None);
    }
    assert_eq!(sniff(b"RIFF\x03\0\0\0AVI "), None);
    assert_eq!(sniff(b"RIFF\x10\0\0\0AVI "), None);
    for end in 0..12 {
        assert_eq!(sniff(&b"RIFF\x04\0\0\0AVI "[..end]), None);
    }
}

#[test]
fn a_quicktime_movie_without_a_file_type_box_is_still_identified() {
    for kind in [b"moov", b"mdat"] {
        let mut bytes = 8_u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        assert_eq!(sniff(&bytes), Some(VideoContainer::QuickTime));
        for end in 0..bytes.len() {
            assert_eq!(sniff(&bytes[..end]), None);
        }
    }
}

#[test]
fn flash_video_uses_its_header_offset_and_initial_back_pointer() {
    let bytes = b"FLV\x01\x05\0\0\0\x09\0\0\0\0";
    assert_eq!(sniff(bytes), Some(VideoContainer::FlashVideo));
    for end in 0..bytes.len() {
        assert_eq!(sniff(&bytes[..end]), None);
    }
    for (index, value) in [(3, 2), (4, 0x80), (8, 8), (8, 100), (12, 1)] {
        let mut bad = *bytes;
        bad[index] = value;
        assert_eq!(sniff(&bad), None);
    }
    let mut extended = bytes[..9].to_vec();
    extended[8] = 10;
    extended.extend_from_slice(b"x\0\0\0\0");
    assert_eq!(sniff(&extended), Some(VideoContainer::FlashVideo));
}

#[test]
fn ogg_identifies_a_complete_first_page_header() {
    let mut bytes = vec![0; 27];
    bytes[..4].copy_from_slice(b"OggS");
    bytes[5] = 2;
    bytes[26] = 1;
    bytes.extend_from_slice(b"\x04data");
    assert_eq!(sniff(&bytes), Some(VideoContainer::Ogg));
    for end in 0..bytes.len() {
        assert_eq!(sniff(&bytes[..end]), None);
    }
    assert_eq!(
        sniff_video_container(&bytes[..28], bytes.len() as u64),
        Some(VideoContainer::Ogg)
    );
    bytes[4] = 1;
    assert_eq!(sniff(&bytes), None);
    bytes[4] = 0;
    bytes[5] = 1;
    assert_eq!(sniff(&bytes), None);
}

#[test]
fn asf_identifies_the_complete_fixed_header() {
    let mut bytes = b"\x30\x26\xb2\x75\x8e\x66\xcf\x11\xa6\xd9\0\xaa\0\x62\xce\x6c".to_vec();
    bytes.extend_from_slice(&30_u64.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&[1, 2]);
    assert_eq!(sniff(&bytes), Some(VideoContainer::Asf));
    for end in 0..bytes.len() {
        assert_eq!(sniff(&bytes[..end]), None);
    }
    bytes[16] = 29;
    assert_eq!(sniff(&bytes), None);
    bytes[16] = 31;
    assert_eq!(sniff(&bytes), None);
    bytes[16] = 30;
    bytes[29] = 1;
    assert_eq!(sniff(&bytes), None);
}

#[test]
fn mpeg_program_and_elementary_stream_headers_are_identified() {
    let first = b"\0\0\x01\xba\x21\0\x01\0\x01\x80\0\x01";
    let second = b"\0\0\x01\xba\x44\0\x04\0\x04\x01\0\0\x03\xf8";
    let elementary = b"\0\0\x01\xb3\x01\0\x10\x11\0\x01\x20\0";
    for (bytes, kind) in [
        (first.as_slice(), VideoContainer::MpegProgramStream),
        (second.as_slice(), VideoContainer::MpegProgramStream),
        (elementary.as_slice(), VideoContainer::MpegVideo),
    ] {
        assert_eq!(sniff(bytes), Some(kind));
        for end in 0..bytes.len() {
            assert_eq!(sniff(&bytes[..end]), None);
        }
    }
    assert_eq!(sniff(b"\0\0\x01\xba\0\0\0\0\0\0\0\0\0\0"), None);
    assert_eq!(sniff(b"\0\0\x01\xb3\0\0\0\0"), None);
}

fn transport_packets(stride: usize, offset: usize) -> Vec<u8> {
    let mut bytes = vec![0; stride * 3];
    for n in 0..3 {
        let start = n * stride + offset;
        bytes[start..start + 4].copy_from_slice(&[0x47, 0, 0, 0x10 | n as u8]);
    }
    bytes
}

#[test]
fn transport_streams_have_repeated_packet_framing() {
    for (stride, offset) in [(188, 0), (192, 4)] {
        let mut bytes = transport_packets(stride, offset);
        assert_eq!(sniff(&bytes), Some(VideoContainer::MpegTransportStream));
        assert_eq!(sniff(&bytes[..stride * 3 - 1]), None);
        bytes[2 * stride + offset] = 0;
        assert_eq!(sniff(&bytes), None);
    }
    assert_eq!(sniff(b"G..."), None);
}

#[test]
fn the_inspection_limit_includes_a_header_ending_on_the_boundary() {
    let padding = MAX_VIDEO_PREFIX_BYTES - 16;
    let mut bytes = vec![0; padding];
    bytes[..4].copy_from_slice(&(padding as u32).to_be_bytes());
    bytes[4..8].copy_from_slice(b"free");
    bytes.extend_from_slice(&ftyp(b"mp42", &[]));
    assert_eq!(sniff(&bytes), Some(VideoContainer::IsoBmff));
    bytes.insert(padding, 0);
    bytes[..4].copy_from_slice(&((padding + 1) as u32).to_be_bytes());
    assert_eq!(sniff(&bytes), None);
}

#[test]
fn ebml_size_fields_can_use_wide_encodings() {
    let bytes = b"\x1a\x45\xdf\xa3\x01\0\0\0\0\0\0\x0e\x42\x82\x01\0\0\0\0\0\0\x04webm";
    assert_eq!(sniff(bytes), Some(VideoContainer::WebM));
}

/// Each container names its copies with an extension of its own: short
/// lowercase letters and digits, so a name made from one is a plain file
/// name that no two containers share.
#[test]
fn each_container_has_its_own_copy_extension() {
    let extensions: Vec<_> = VideoContainer::ALL
        .iter()
        .map(|container| container.extension())
        .collect();
    for extension in &extensions {
        assert!(
            (2..=4).contains(&extension.len())
                && extension
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
            "{extension}"
        );
    }
    let mut unique = extensions.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), extensions.len(), "{extensions:?}");
    assert_eq!(VideoContainer::IsoBmff.extension(), "mp4");
    assert_eq!(VideoContainer::QuickTime.extension(), "mov");
    assert_eq!(VideoContainer::WebM.extension(), "webm");
}
