//! Entry points for fuzzing what this crate reads from a PTY: the
//! extractor's byte stream and the image, completion and placeholder
//! decoders behind it. Each takes any bytes and must return without
//! panicking. The fuzz targets (feature `fuzzing`) and the in-process
//! sweeps below call the same functions.
//!
//! [`vt`] feeds a script of PTY reads and extractor calls to two
//! extractors, one read whole and one in small pieces, and requires the
//! same result from both: the bytes the terminal engine sees, the bytes a
//! raw consumer sees, and every other chunk at the same place in those
//! streams. Bytes with no ESC pass through untouched, every decoded image
//! is the size it says and within its limits, and once the extractors and
//! their chunks are gone their graphics account is back to zero.
//!
//! [`images`] runs one decoder directly: Sixel, iTerm2, Kitty commands,
//! the completion protocol, or the placeholder marks.

use std::fmt::Write as _;

use crate::completion::{self, CompletionUpdate};
use crate::extract::{Chunk, Extractor};
use crate::graphics_limits::{GraphicsBudget, GraphicsLimits};
use crate::image::{ImageData, MAX_IMAGE_DIM};
use crate::inline_marker::{InlineMarker, strip_placeholders};
use crate::kitty::{KittyOut, KittyState};
use crate::placeholder::{self, PLACEHOLDER};
use crate::{iterm, sixel};

/// In a [`vt`] script, this byte and the one after it are a call on the
/// extractor rather than PTY bytes; `0xff 0x00` is a literal `0xff`.
const CALL: u8 = 0xff;

/// Limits small enough for a fuzz input to reach every one of them.
fn small_limits() -> GraphicsLimits {
    const KIB: usize = 1024;
    GraphicsLimits {
        sequence_bytes: 4 * KIB,
        transmission_bytes: 64 * KIB,
        in_flight_slots: 2,
        in_flight_bytes: 128 * KIB,
        image_bytes: 64 * KIB,
        retained_bytes: 256 * KIB,
        process_cpu_bytes: 512 * KIB,
        process_gpu_bytes: 512 * KIB,
        animation_frames: 4,
        animation_bytes: 128 * KIB,
        placements: 8,
        sixel_column_writes: 64 * KIB,
    }
}

/// One step of a [`vt`] script.
enum Step {
    Read(Vec<u8>),
    Call(u8),
}

/// Split `data` into PTY reads and extractor calls at each [`CALL`] byte.
fn script(data: &[u8]) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut read = Vec::new();
    let mut bytes = data.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte != CALL {
            read.push(byte);
            continue;
        }
        match bytes.next() {
            Some(0) => read.push(CALL),
            Some(call) => {
                if !read.is_empty() {
                    steps.push(Step::Read(std::mem::take(&mut read)));
                }
                steps.push(Step::Call(call));
            }
            None => {}
        }
    }
    if !read.is_empty() {
        steps.push(Step::Read(read));
    }
    steps
}

fn call(extractor: &mut Extractor, call: u8) {
    match call % 11 {
        0 => extractor.enter_alternate_screen(false),
        1 => extractor.enter_alternate_screen(true),
        2 => extractor.leave_alternate_screen(false),
        3 => extractor.leave_alternate_screen(true),
        4 => extractor.clear_active_graphics(),
        5 => extractor.reset_all_graphics(),
        6 => extractor.set_graphics_deferred(true),
        7 => extractor.set_graphics_deferred(false),
        8 => extractor.set_raw_tap(true),
        9 => extractor.set_raw_tap(false),
        _ => extractor.clear_reflowed_regular_placements(),
    }
}

/// What a run produced: the engine's bytes, a raw consumer's bytes, and
/// every other chunk with how far into each stream it came.
#[derive(Debug, Default, PartialEq, Eq)]
struct Outcome {
    engine: Vec<u8>,
    raw: Vec<u8>,
    chunks: Vec<(usize, usize, String)>,
}

/// Check an image's size against its limits and describe its pixels.
fn image(image: &ImageData, limits: &GraphicsLimits) -> String {
    let (width, height) = (image.width, image.height);
    assert!(
        width <= MAX_IMAGE_DIM && height <= MAX_IMAGE_DIM,
        "a {width}x{height} image"
    );
    let expected = u64::from(width) * u64::from(height) * 4;
    assert_eq!(
        image.rgba.len() as u64,
        expected,
        "a {width}x{height} image's pixels"
    );
    assert!(
        image.rgba.len() <= limits.image_bytes,
        "a {width}x{height} image over its {} byte limit",
        limits.image_bytes
    );
    // FNV-1a, so two runs' pixels compare without keeping them.
    let hash = image
        .rgba
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("{width}x{height}:{hash:016x}")
}

fn record(outcome: &mut Outcome, chunk: Chunk, limits: &GraphicsLimits) {
    let mut images = Vec::new();
    match &chunk {
        Chunk::Pass(bytes) => {
            outcome.engine.extend_from_slice(bytes);
            outcome.raw.extend_from_slice(bytes);
            return;
        }
        Chunk::Terminal(bytes) => {
            outcome.engine.extend_from_slice(bytes);
            return;
        }
        Chunk::Raw(bytes) => {
            outcome.raw.extend_from_slice(bytes);
            return;
        }
        Chunk::Image(placed) => images.push(&placed.img),
        Chunk::VirtualImage { img, .. } | Chunk::RelativePlacement { img, .. } => images.push(img),
        Chunk::Animation { imgs, .. } => images.extend(imgs),
        Chunk::Completion(update) => completion_update(update),
        Chunk::ImageQuota(request) => {
            // What the request decides, not its allocations' addresses.
            let images: Vec<_> = request
                .images
                .iter()
                .map(|image| (image.key, image.created))
                .collect();
            outcome.chunks.push((
                outcome.engine.len(),
                outcome.raw.len(),
                format!("ImageQuota plan {:?} images {images:?}", request.plan(&[])),
            ));
            return;
        }
        _ => {}
    }
    let mut description = format!("{chunk:?}");
    for picture in images {
        let _ = write!(description, " {}", image(picture, limits));
    }
    outcome
        .chunks
        .push((outcome.engine.len(), outcome.raw.len(), description));
}

/// Read the script, in pieces of `piece` bytes when given. Also returns
/// whether a private completion message outgrew its cap.
fn run(steps: &[Step], budget: &GraphicsBudget, piece: Option<usize>) -> (Outcome, bool) {
    let limits = budget.limits();
    let mut extractor = Extractor::with_budget(budget.clone());
    let mut outcome = Outcome::default();
    for step in steps {
        match step {
            Step::Read(bytes) => {
                let pieces: Vec<&[u8]> = match piece {
                    Some(size) => bytes.chunks(size).collect(),
                    None => vec![bytes],
                };
                for piece in pieces {
                    extractor.feed_with(piece, |_, chunk| record(&mut outcome, chunk, &limits));
                }
            }
            Step::Call(which) => call(&mut extractor, *which),
        }
    }
    (outcome, extractor.private_overflows() > 0)
}

/// Run `data` as a script of PTY reads and extractor calls: its first byte
/// picks the limits, its second the size of the pieces the second
/// extractor reads in, and the rest is the script.
pub fn vt(data: &[u8]) {
    let [flags, piece, data @ ..] = data else {
        return;
    };
    let limits = if flags & 1 == 0 {
        GraphicsLimits::default()
    } else {
        small_limits()
    };
    let steps = script(data);
    let budget = GraphicsBudget::independent(limits).expect("valid limits");
    let (whole, whole_overflowed) = run(&steps, &budget, None);
    let split_budget = GraphicsBudget::independent(limits).expect("valid limits");
    let (split, split_overflowed) = run(&steps, &split_budget, Some(usize::from(*piece) + 1));
    // A private completion message that outgrows its cap is private through
    // the read that crossed it, and ordinary output resumes a bounded window
    // later (the raw output filter matches this), so there, and only there,
    // read sizes may move where output resumes.
    if !(whole_overflowed || split_overflowed) {
        assert_eq!(whole, split, "reading in pieces changed the result");
    }
    let read: Vec<u8> = steps
        .iter()
        .filter_map(|step| match step {
            Step::Read(bytes) => Some(bytes.as_slice()),
            Step::Call(_) => None,
        })
        .flatten()
        .copied()
        .collect();
    if !read.contains(&0x1b) {
        assert_eq!(
            whole.engine, read,
            "bytes with no ESC changed on the way through"
        );
        assert!(
            whole.chunks.is_empty(),
            "bytes with no ESC made {:?}",
            whole.chunks
        );
    }
    for account in [&budget, &split_budget] {
        assert_eq!(
            account.usage(),
            (0, 0, 0, 0),
            "graphics bytes outlived their owners"
        );
    }
}

/// A completion update holds what the protocol's limits allow.
fn completion_update(update: &CompletionUpdate) {
    let (CompletionUpdate::Show(list) | CompletionUpdate::Update(list)) = update else {
        return;
    };
    let candidates = &list.candidates;
    assert!(!candidates.is_empty() && candidates.len() <= completion::MAX_CANDIDATES);
    assert!(list.total >= candidates.len() && list.total <= completion::MAX_TOTAL_CANDIDATES);
    assert!(
        list.selected
            .is_none_or(|selected| selected < candidates.len())
    );
    assert!(
        candidates
            .windows(2)
            .all(|pair| pair[0].position < pair[1].position)
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.position < list.total)
    );
    let fields = [
        (list.source.as_str(), 64),
        (
            list.token.as_deref().unwrap_or_default(),
            completion::MAX_TOKEN_BYTES,
        ),
        (
            list.input_prefix.as_deref().unwrap_or_default(),
            completion::MAX_INPUT_PREFIX_BYTES,
        ),
    ]
    .into_iter()
    .chain(candidates.iter().flat_map(|candidate| {
        [
            (candidate.label.as_str(), completion::MAX_LABEL_BYTES),
            (
                candidate.description.as_str(),
                completion::MAX_DESCRIPTION_BYTES,
            ),
        ]
    }));
    for (field, limit) in fields {
        assert!(
            field.len() <= limit,
            "a {} byte field past {limit}",
            field.len()
        );
        assert!(
            !field.chars().any(completion::unsafe_display_scalar),
            "an unsafe character in {field:?}"
        );
    }
    assert!(
        candidates
            .iter()
            .all(|candidate| !candidate.label.is_empty())
    );
}

/// Decode `data` past its first byte with the decoder that byte picks.
pub fn images(data: &[u8]) {
    images_with(data, |decoded, limits| {
        image(decoded, limits);
    });
}

fn images_with(data: &[u8], mut visit: impl FnMut(&ImageData, &GraphicsLimits)) {
    let Some((&pick, bytes)) = data.split_first() else {
        return;
    };
    let limits = if pick & 0x80 == 0 {
        GraphicsLimits::default()
    } else {
        small_limits()
    };
    let budget = GraphicsBudget::independent(limits).expect("valid limits");
    let text = std::str::from_utf8(bytes).ok();
    match (pick & 0x7f) % 5 {
        0 => {
            if let Some(decoded) = sixel::decode_with_budget(bytes, &budget) {
                visit(&decoded, &limits);
            }
        }
        1 => {
            if let Some(decoded) = text.and_then(|body| iterm::decode_with_budget(body, &budget)) {
                visit(&decoded, &limits);
            }
        }
        2 => {
            // Kitty command bodies, one per NUL-separated part.
            let mut state = KittyState::new(budget.clone());
            for body in bytes.split(|&byte| byte == 0) {
                let Ok(body) = std::str::from_utf8(body) else {
                    continue;
                };
                if let KittyOut::Place(placed) = state.feed(body) {
                    visit(&placed.img, &limits);
                }
            }
        }
        3 => {
            if let Some(update) = completion::parse(bytes) {
                completion_update(&update);
            }
        }
        _ => {
            if let Some(text) = text {
                marks(text);
            }
        }
    }
    assert_eq!(
        budget.usage(),
        (0, 0, 0, 0),
        "graphics bytes outlived their owners"
    );
}

/// The placeholder marks: an inline-card marker decodes, encodes and
/// decodes to itself, and stripping placeholders leaves none behind.
fn marks(text: &str) {
    let chars: Vec<char> = text.chars().collect();
    for window in chars.windows(8).take(64) {
        if let Some(marker) = InlineMarker::decode(window) {
            let encoded = marker.encode().expect("a decoded marker encodes");
            let marks: Vec<char> = encoded.chars().skip(1).collect();
            assert_eq!(InlineMarker::decode(&marks), Some(marker));
            assert_eq!(strip_placeholders(&encoded), "");
        }
    }
    let stripped = strip_placeholders(text);
    assert!(!stripped.contains(PLACEHOLDER));
    assert_eq!(strip_placeholders(&stripped), stripped);
    let _ = placeholder::CellDiacritics::parse(&chars);
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ::image::{ImageFormat, Rgba, RgbaImage};
    use base64::Engine as _;
    use kettle_test_support::mutate::{cases, save_seeds};

    use super::*;

    fn png() -> Vec<u8> {
        let picture = RgbaImage::from_fn(3, 2, |x, y| Rgba([x as u8 * 80, y as u8 * 90, 40, 255]));
        let mut bytes = Cursor::new(Vec::new());
        picture.write_to(&mut bytes, ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    /// PTY streams that reach each protocol the extractor handles.
    fn streams() -> Vec<Vec<u8>> {
        let png = base64::engine::general_purpose::STANDARD.encode(png());
        let mut streams: Vec<Vec<u8>> = vec![
            "plain text, UTF-8 \u{2713}, tabs\tand\r\nlines".into(),
            b"\x1b[1;31mred\x1b[0m \x1b]0;title\x07 \x1b]2;other\x1b\\".to_vec(),
            b"\x1b]7;file://localhost/tmp/dir\x07\x1b]133;A\x07$ \x1b]133;B\x07ls\x1b]133;C\x07".to_vec(),
            b"\x1b]9;4;1;50\x07\x1b]9;hello\x07\x1b]777;notify;Title;Body\x1b\\".to_vec(),
            b"\x1bPq#0;2;100;0;0#0~~~~-#0!4~\x1b\\".to_vec(),
            b"\x1b_Ga=T,f=32,s=1,v=1,i=7;AAAA/w==\x1b\\\x1b_Ga=p,i=7,p=2\x1b\\\x1b_Ga=d,d=i,i=7\x1b\\".to_vec(),
            b"\x1b_Ga=q,i=1,s=1,v=1,f=24;AAAA\x1b\\\x1b_Ga=t,i=3,m=1;AAAA\x1b\\\x1b_Gm=0;/w==\x1b\\".to_vec(),
            b"\x1b_Ga=T,U=1,f=32,s=1,v=1,i=9,c=2,r=1;AAAA/w==\x1b\\".to_vec(),
            b"\x1b_Ga=f,i=7,f=32,s=1,v=1;AAAA/w==\x1b\\\x1b_Ga=a,i=7,s=3\x1b\\".to_vec(),
            format!("\x1b]1337;File=inline=1;size=10:{png}\x07").into_bytes(),
            b"\x1b]777;kettle-completion;4;show;1;2;3;completion;0;zsh;gi;git%20;0;2;git;version%20control;gitk;viewer\x07".to_vec(),
            b"\x1b[?2026h\x1b_Ga=T,f=32,s=1,v=1;AAAA/w==\x1b\\text\x1b[?2026l".to_vec(),
            b"\x1b[?1049h\x1b[2Jalt\x1b[?1049l\x1bP+q544e\x1b\\\x1b[>q\x1b[?6n".to_vec(),
            b"\x9c\x1b]0;\xe2\x9c\xb3 spinner\x9c\x1b]52;c;aGk=\x07\x18\x1a".to_vec(),
        ];
        // An OSC past the small limit's sequence bytes, and one cancelled.
        let mut long = b"\x1b]0;".to_vec();
        long.extend(std::iter::repeat_n(b'x', 5000));
        long.extend_from_slice(b"\x07after");
        streams.push(long);
        streams.push(b"\x1b]0;open\x1b\x18still text".to_vec());
        streams
    }

    /// Every stream behind each pair of flag and piece-size bytes, and
    /// interleaved with extractor calls.
    fn vt_seeds() -> Vec<Vec<u8>> {
        let mut seeds = Vec::new();
        for stream in streams() {
            for (flags, piece) in [(0, 0), (1, 2), (0, 6)] {
                let mut seed = vec![flags, piece];
                seed.extend_from_slice(&stream);
                seeds.push(seed);
            }
            let mut calls = vec![1, 3, CALL, 1, CALL, 8];
            calls.extend_from_slice(&stream);
            calls.extend_from_slice(&[CALL, 3, CALL, 0, CALL, 6]);
            calls.extend_from_slice(&stream);
            calls.extend_from_slice(&[CALL, 5]);
            seeds.push(calls);
        }
        seeds
    }

    #[test]
    fn mutated_pty_streams_read_the_same_whole_or_in_pieces() {
        let seeds = vt_seeds();
        let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
        save_seeds("vt", &seeds);
        for case in cases(&seeds, 600, 0x5eed_0101) {
            vt(&case);
        }
    }

    #[test]
    fn small_limit_seeds_decode_with_the_same_protocol() {
        for (pick, payload) in [
            (0, &b"\"1;1;1;6#0;2;100;0;0~"[..]),
            (2, &b"a=T,f=32,s=1,v=1,i=1;AQID/w=="[..]),
        ] {
            for flags in [0, 0x80] {
                let mut seed = vec![pick | flags];
                seed.extend_from_slice(payload);
                let mut decoded = 0;
                super::images_with(&seed, |img, limits| {
                    super::image(img, limits);
                    decoded += 1;
                });
                assert_eq!(
                    decoded, 1,
                    "protocol {pick} must decode with flags {flags:#x}"
                );
            }
        }
    }

    #[test]
    fn mutated_decoder_inputs_stay_within_their_limits() {
        let png = base64::engine::general_purpose::STANDARD.encode(png());
        let marker = InlineMarker {
            row: 3,
            column: 107,
            nonce: crate::inline_marker::InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap(),
        }
        .encode()
        .unwrap();
        let inputs: Vec<(u8, Vec<u8>)> = vec![
            (0, b"#0;2;100;0;0#1;2;0;100;0#0~~~~$#1!4@-#0??~~".to_vec()),
            (0, b"\"1;1;8;6#0~~~~~~~~".to_vec()),
            (1, format!("File=inline=1;width=2:{png}").into_bytes()),
            (
                2,
                b"a=T,f=32,s=1,v=1,i=1;AAAA/w==\0a=p,i=1\0a=d,d=a".to_vec(),
            ),
            (
                2,
                b"a=t,f=24,s=2,v=1,i=2,m=1;AAAAAA\0m=0;AAAA\0a=p,i=2,x=1,w=1".to_vec(),
            ),
            (
                2,
                b"a=T,f=100,i=4;"
                    .iter()
                    .copied()
                    .chain(png.bytes())
                    .collect(),
            ),
            (
                3,
                b"777;kettle-completion;2;show;5;completion;1;bash;0;3;a;;b;desc;c;".to_vec(),
            ),
            (3, b"777;kettle-completion;3;sync;9;1".to_vec()),
            (
                4,
                format!("x{marker}y{}\u{0305}z", PLACEHOLDER).into_bytes(),
            ),
        ];
        let mut seeds = Vec::new();
        for (pick, bytes) in inputs {
            for pick in [pick, pick | 0x80] {
                let mut seed = vec![pick];
                seed.extend_from_slice(&bytes);
                seeds.push(seed);
            }
        }
        let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
        save_seeds("images", &seeds);
        for case in cases(&seeds, 800, 0x5eed_0102) {
            images(&case);
        }
    }

    /// The `vt` target's first CI find, rebuilt: a completion message past
    /// the small 4 KiB sequence limit, with a progress report inside its
    /// recovery window. One read once bailed before the private prefix was
    /// seen, so the report escaped; three-byte reads kept it private.
    #[test]
    fn an_oversized_completion_message_reads_the_same_in_pieces() {
        let mut data = vec![1, 2];
        data.extend_from_slice(b"\x1b]777;kettle-completion;8;sohw;1;2;3;completion");
        data.extend(std::iter::repeat_n(b'x', 5_000));
        data.extend_from_slice(b"\x1b]9;4;1;50\x07");
        data.extend(std::iter::repeat_n(b'x', 100));
        vt(&data);
    }

    /// The checks are live: a stream read in pieces is compared with the
    /// whole one, and an image's pixel count is checked against its size.
    #[test]
    fn the_checks_catch_what_they_check() {
        let budget = GraphicsBudget::independent(GraphicsLimits::default()).unwrap();
        let steps = script(&streams()[5]);
        assert_eq!(run(&steps, &budget, None), run(&steps, &budget, Some(3)));
        assert!(!run(&steps, &budget, None).0.chunks.is_empty());
        let mut wrong = ImageData::new(2, 2, vec![0; 16]).unwrap();
        wrong.height = 3;
        let caught = std::panic::catch_unwind(|| image(&wrong, &GraphicsLimits::default()));
        assert!(caught.is_err());
    }
}
