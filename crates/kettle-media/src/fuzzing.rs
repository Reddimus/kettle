//! Entry points for fuzzing what reads untrusted bytes here: the frames,
//! a playback session's stream, and a video's sniffed first bytes. Each
//! takes any bytes and must return without panicking, allocating past its
//! direction's budget, or reading past what it was given; a frame that
//! decodes must encode back to the same bytes and read the same from a
//! stream, and one read from a stream is what its own bytes decode to. The
//! fuzz targets (feature `fuzzing`) and the in-process sweeps below call the
//! same functions.

use std::io::Cursor;

use crate::wire::{self, Direction};

const DIRECTIONS: [Direction; 3] = [
    Direction::ExternalToParent,
    Direction::ParentToWorker,
    Direction::WorkerToParent,
];

/// Decode `data` past its first byte as a frame in the direction that byte
/// picks.
pub fn wire(data: &[u8]) {
    let Some((&pick, frame)) = data.split_first() else {
        return;
    };
    let direction = DIRECTIONS[usize::from(pick) % DIRECTIONS.len()];
    let report = wire::decode_with_stats(frame, direction);
    assert!(
        report.allocations.requested_bytes <= direction.max_decode_allocation_bytes(),
        "decoding held {} bytes",
        report.allocations.requested_bytes
    );
    let mut stream = Cursor::new(frame);
    let read = wire::read_frame(&mut stream, direction);
    if let Ok(decoded) = report.result {
        assert_eq!(
            wire::encode(&decoded, direction).ok().as_deref(),
            Some(frame),
            "a frame decoded that does not encode back"
        );
        assert_eq!(
            read.as_ref().ok(),
            Some(&Some(decoded)),
            "read and decoded apart"
        );
    }
    // A frame read from a stream is what its own bytes decode to; bytes past
    // it are the next frame's.
    if let Ok(Some(read)) = read {
        let consumed = &frame[..stream.position() as usize];
        assert_eq!(wire::decode(consumed, direction).ok(), Some(read));
    }
}

/// Read `data` past its first two bytes as a playback session's stream, at
/// the size those bytes give (each 1 to 256), until it ends or breaks.
pub fn session(data: &[u8]) {
    let [width, height, rest @ ..] = data else {
        return;
    };
    let (width, height) = (u32::from(*width) + 1, u32::from(*height) + 1);
    let mut reader = Cursor::new(rest);
    let mut pixels = Vec::new();
    for _ in 0..64 {
        let start = reader.position() as usize;
        let read = wire::read_session_frame(&mut reader, width, height, &mut pixels, 256);
        let consumed = &rest[start..reader.position() as usize];
        // What the session reader takes is what those bytes decode to.
        match read {
            Ok(Some(wire::SessionRead::Pixels { seq, pts_ms })) => {
                let decoded = wire::decode(consumed, Direction::WorkerToParent);
                let expected = crate::VideoFrame {
                    seq,
                    pts_ms,
                    width,
                    height,
                    rgba: pixels.clone(),
                };
                assert_eq!(decoded.ok(), Some(wire::Frame::VideoFrame(expected)));
            }
            Ok(Some(wire::SessionRead::Frame(frame))) => {
                assert_eq!(
                    wire::decode(consumed, Direction::WorkerToParent).ok(),
                    Some(*frame)
                );
            }
            Ok(None) | Err(_) => return,
        }
    }
}

/// Sniff `data` as a video's first bytes: as the whole file, as the start
/// of the longest one, and as the start of an empty one, which the bytes
/// contradict. The answer may differ with the length (an atom of size zero
/// runs to the end of the file), but none may panic or read past `data`.
pub fn sniff(data: &[u8]) {
    for size in [data.len() as u64, u64::MAX, 0] {
        let _ = crate::video::sniff_video_container(data, size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Frame;
    use crate::{PlayState, VideoFrame};
    use kettle_test_support::mutate::{cases, save_seeds};

    fn hex(text: &str) -> Vec<u8> {
        let text = text.trim();
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    /// Every frame kind's golden bytes behind each direction byte, and a
    /// session's frames, mutated: nothing panics, holds past its budget, or
    /// decodes without encoding back.
    #[test]
    fn mutated_frames_never_break_the_decoder() {
        let goldens = [
            include_str!("../tests/golden/hello.hex"),
            include_str!("../tests/golden/ready.hex"),
            include_str!("../tests/golden/external.hex"),
            include_str!("../tests/golden/job.hex"),
            include_str!("../tests/golden/rendered.hex"),
            include_str!("../tests/golden/failure.hex"),
        ];
        let mut seeds: Vec<Vec<u8>> = Vec::new();
        for golden in goldens {
            for direction in 0..3u8 {
                let mut seed = vec![direction];
                seed.extend(hex(golden));
                seeds.push(seed);
            }
        }
        let frame = VideoFrame {
            seq: 3,
            pts_ms: 120,
            width: 2,
            height: 1,
            rgba: vec![1; 8],
        };
        for frame in [
            Frame::VideoFrame(frame),
            Frame::PlayState(PlayState::Paused),
        ] {
            let mut seed = vec![2];
            seed.extend(wire::encode(&frame, Direction::WorkerToParent).unwrap());
            seeds.push(seed);
        }
        let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
        save_seeds("wire", &seeds);
        for mut case in cases(&seeds, 6000, 0x5eed_0001) {
            wire(&case);
            // The same case with its header's length fixed to what follows,
            // so mutations inside a frame reach the payload's parser.
            if case.len() > wire::HEADER_BYTES {
                let payload = (case.len() - 1 - wire::HEADER_BYTES) as u32;
                case[8..12].copy_from_slice(&payload.to_le_bytes());
                wire(&case);
            }
        }
    }

    /// A session's stream of frames, states and failures, mutated: every
    /// pixel read is the size asked, and nothing panics.
    #[test]
    fn mutated_session_streams_never_break_the_reader() {
        let mut stream = vec![1, 0];
        for seq in 0..3 {
            let frame = VideoFrame {
                seq,
                pts_ms: seq * 40,
                width: 2,
                height: 1,
                rgba: vec![seq as u8; 8],
            };
            stream.extend(
                wire::encode(&Frame::VideoFrame(frame), Direction::WorkerToParent).unwrap(),
            );
        }
        stream.extend(
            wire::encode(
                &Frame::PlayState(PlayState::Ended),
                Direction::WorkerToParent,
            )
            .unwrap(),
        );
        save_seeds("session", &[&stream]);
        for case in cases(&[&stream], 3000, 0x5eed_0002) {
            session(&case);
        }
    }

    /// Containers' first bytes, mutated: the sniff never panics, whatever
    /// length the file is said to have.
    #[test]
    fn mutated_prefixes_never_break_the_sniff() {
        let mp4: &[u8] = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2\x00\x00\x00\x08free";
        let webm: &[u8] = b"\x1a\x45\xdf\xa3\x9f\x42\x86\x81\x01\x42\xf7\x81\x01\x42\x82\x84webm";
        let avi: &[u8] = b"RIFF\x24\x00\x00\x00AVI LIST";
        let ts: &[u8] = &[0x47, 0x40, 0x00, 0x10, 0x00, 0x00, 0xb0, 0x0d];
        // An atom of size zero runs to the end of the file: a short file
        // holds the brand, a long one does not yet.
        sniff(b"\0\0\0\0ftypisom\0\0\0\0");
        save_seeds("sniff", &[mp4, webm, avi, ts]);
        for case in cases(&[mp4, webm, avi, ts], 4000, 0x5eed_0003) {
            sniff(&case);
        }
    }
}
