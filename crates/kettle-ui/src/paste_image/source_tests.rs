//! A PNG another program put on the clipboard: read by its header, decoded
//! under limits, and kept only re-encoded.
use super::*;

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut crc = !0u32;
    for byte in kind.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&(!crc).to_be_bytes());
    out
}

/// A PNG that declares `width` by `height` at `depth` bits a channel of colour
/// type `colour` (0 grey, 2 RGB, 6 RGBA), with the start of an image stream
/// and no pixels: all a decoder needs to decide what to allocate.
fn declares(width: u32, height: u32, depth: u8, colour: u8) -> Vec<u8> {
    let mut header = [width.to_be_bytes(), height.to_be_bytes()].concat();
    header.extend_from_slice(&[depth, colour, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(chunk(b"IHDR", &header));
    png.extend(chunk(b"IDAT", &[0x78, 0x01]));
    png
}

/// A whole 2x2 RGB PNG carrying `extra` after its header, as a source's
/// metadata would be.
fn rgb_png(extra: &[u8]) -> (Vec<u8>, Vec<u8>) {
    use image::ImageEncoder as _;
    let rgb = [10u8, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&rgb, 2, 2, image::ExtendedColorType::Rgb8)
        .unwrap();
    // The signature and the IHDR chunk are 33 bytes.
    png.splice(33..33, extra.iter().copied());
    let rgba = rgb
        .chunks(3)
        .flat_map(|px| [px[0], px[1], px[2], 255])
        .collect();
    (png, rgba)
}

fn scratch(name: &str) -> PathBuf {
    let dir = scratch_root().join(format!(
        "kettle-paste-source-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_png_header_gives_its_size_without_a_decoder() {
    assert_eq!(
        png_dimensions(&declares(20_000, 3, 8, 6)),
        Some((20_000, 3))
    );
    assert_eq!(png_dimensions(&rgb_png(&[]).0), Some((2, 2)));
    let whole = declares(640, 480, 8, 2);
    assert_eq!(png_dimensions(&whole[..24]), Some((640, 480)));
    assert_eq!(png_dimensions(&whole[..23]), None, "a cut-off header");
    assert_eq!(png_dimensions(b""), None);
    assert_eq!(
        png_dimensions(b"GIF89a\x02\x00\x02\x00 not a PNG at all"),
        None
    );
    let mut other_first_chunk = whole.clone();
    other_first_chunk[12..16].copy_from_slice(b"tEXt");
    assert_eq!(png_dimensions(&other_first_chunk), None);
}

#[test]
fn an_oversize_header_is_refused_before_anything_is_decoded_or_stored() {
    let dir = scratch("oversize");
    let mut images = PastedImages::with_dir(dir.clone());
    for (name, png) in [
        ("20,000 px wide", declares(20_000, 1, 8, 6)),
        ("16,385 px tall", declares(1, 16_385, 8, 6)),
        ("1 GiB as RGBA", declares(16_384, 16_384, 8, 6)),
        ("over 256 MiB as RGBA", declares(8_200, 8_200, 8, 6)),
        ("no pixels", declares(0, 7, 8, 6)),
        ("not a PNG", b"\xff\xd8\xff\xe0 a JPEG says".to_vec()),
    ] {
        let error = images.reserve_png_from(&png, true).err().expect(name);
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{name}");
    }
    assert!(images.reserved.is_empty());
    assert!(!dir.exists(), "a refused image never touches the disk");
}

#[test]
fn the_decoder_has_its_limits_before_it_allocates_a_pixel() {
    let refused = |png: &[u8], width, height| decode_png(png, width, height).unwrap_err().kind();
    // The decoder itself holds the 16,384 px side limit.
    assert_eq!(
        refused(&declares(20_000, 1, 8, 6), 20_000, 1),
        io::ErrorKind::FileTooLarge
    );
    assert_eq!(
        refused(&declares(1, 16_385, 8, 6), 1, 16_385),
        io::ErrorKind::FileTooLarge
    );
    // A decompression bomb: 8000x8000 is 256 MB as RGBA8, which the store
    // takes, but at 16 bits a channel the decoder would allocate 512 MB.
    // The stream behind the header is a few bytes; had the decoder got as
    // far as reading it, the error would be the truncated stream's.
    assert_eq!(
        refused(&declares(8_000, 8_000, 16, 6), 8_000, 8_000),
        io::ErrorKind::FileTooLarge
    );
    // The same truncated stream within the limits is only a bad PNG.
    assert_eq!(
        refused(&declares(8, 8, 8, 6), 8, 8),
        io::ErrorKind::InvalidData
    );
    // The size reserved for is the size decoded, or nothing is.
    let (png, rgba) = rgb_png(&[]);
    assert_eq!(decode_png(&png, 2, 2).unwrap(), rgba);
    assert_eq!(refused(&png, 2, 3), io::ErrorKind::InvalidData);
    assert_eq!(refused(&png, 1, 4), io::ErrorKind::InvalidData);
}

#[test]
fn a_source_png_is_kept_only_re_encoded_in_a_private_file_and_directory() {
    let dir = scratch("re-encode");
    let mut images = PastedImages::with_dir(dir.clone());
    let (source, rgba) = rgb_png(&chunk(b"tEXt", b"Comment\0written by the source"));
    assert!(source.windows(21).any(|w| w == b"written by the source"));

    let png = images.reserve_png_from(&source, true).unwrap();
    let encoded = png.encode_source(&PngSource::Png(source.clone()), &|| false);
    let path = images.keep_png(encoded).unwrap();
    assert_eq!(path, dir.join("0001.png"));

    let kept = std::fs::read(&path).unwrap();
    assert_ne!(kept, source, "the source's bytes are never the file");
    assert!(
        !kept.windows(21).any(|w| w == b"written by the source"),
        "no ancillary chunk of the source survives"
    );
    let decoded = image::load_from_memory(&kept).unwrap();
    assert_eq!(decoded.color(), image::ColorType::Rgba8);
    assert_eq!(decoded.into_rgba8().into_raw(), rgba);
    assert!(images.take_preview_for_path(&path).is_some());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir), 0o700);
    }
    images.cleanup();
    assert!(!dir.exists());
}

#[test]
fn a_source_png_that_fails_to_decode_leaves_no_file_and_gives_its_room_back() {
    let dir = scratch("bad-source");
    let mut images = PastedImages::with_dir(dir.clone());
    let caller = std::thread::current().id();
    let (woke, wakes) = crossbeam_channel::unbounded();
    let encoder = PngEncoder::new(Box::new(move || {
        let _ = woke.send(std::thread::current().id());
    }))
    .unwrap();
    for (id, source, kind) in [
        (
            1,
            declares(8_000, 8_000, 16, 6),
            io::ErrorKind::FileTooLarge,
        ),
        (2, declares(64, 64, 8, 6), io::ErrorKind::InvalidData),
    ] {
        // The header is within the store's bounds, so room is set aside.
        let png = images.reserve_png_from(&source, true).unwrap();
        assert_eq!(images.reserved.len(), 1);
        assert!(encoder.submit(id, png, PngSource::Png(source)).is_ok());
        let worker = wakes.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_ne!(worker, caller, "decoded on the encoder thread");
        let (done, encoded) = encoder.try_recv().unwrap();
        assert_eq!(done, id);
        assert_eq!(images.keep_png(encoded).unwrap_err().kind(), kind);
        assert!(images.reserved.is_empty(), "the room is given back");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "no file, whole or partial"
        );
    }
    // The store works on: the next image takes the first name.
    let (source, rgba) = rgb_png(&[]);
    let png = images.reserve_png_from(&source, false).unwrap();
    assert!(encoder.submit(3, png, PngSource::Png(source)).is_ok());
    wakes.recv_timeout(Duration::from_secs(30)).unwrap();
    let path = images.keep_png(encoder.try_recv().unwrap().1).unwrap();
    assert_eq!(path.file_name(), Some(OsStr::new("0001.png")));
    assert_eq!(image::open(&path).unwrap().to_rgba8().into_raw(), rgba);
    drop(encoder);
    images.cleanup();
    assert!(!dir.exists());
}

#[test]
fn source_pngs_waiting_to_be_decoded_count_against_the_store() {
    let dir = scratch("waiting");
    let small = StoreKind {
        max_bytes: 3 * 1024 * 1024,
        ..PASTED
    };
    let mut images = PastedImages::of_kind_in(small, dir.clone());
    // A 2x2 image carrying a megabyte the decoder will throw away.
    let (source, _) = rgb_png(&chunk(b"tEXt", &[b'x'; 1024 * 1024]));
    let first = images.reserve_png_from(&source, false).unwrap();
    assert!(images.reserved[0].1 >= source.len() as u64);
    let second = images.reserve_png_from(&source, false).unwrap();
    let third = images.reserve_png_from(&source, false).unwrap();
    assert_eq!(
        images
            .reserve_png_from(&source, false)
            .err()
            .map(|error| error.kind()),
        Some(io::ErrorKind::QuotaExceeded),
        "three megabyte sources fill a 3 MiB store"
    );
    // Kept, each is its few re-encoded bytes, and the room comes back.
    for png in [first, second, third] {
        let encoded = png.encode_source(&PngSource::Png(source.clone()), &|| false);
        images.keep_png(encoded).unwrap();
    }
    assert!(images.bytes < 4096);
    assert!(images.reserve_png_from(&source, false).is_ok());
    images.cleanup();
    assert!(!dir.exists());
}
