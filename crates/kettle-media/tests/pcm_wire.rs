use kettle_media::pcm::{BYTES, Block, Feedback, Message, Open};
use kettle_media::wire::{Direction, Frame, WireError, decode, decode_with_stats, encode};

fn messages() -> Vec<Message> {
    vec![
        Message::Open(Open {
            generation: 3,
            start_frame: 48_000,
            end_frame: 96_000,
            muted: true,
        }),
        Message::Block(Block {
            generation: 3,
            sequence: 1,
            at_frame: 48_960,
            samples: 0.125f32.to_le_bytes().repeat(BYTES / 4),
        }),
        Message::End {
            generation: 3,
            sequence: 2,
        },
        Message::Feedback(Feedback {
            generation: 3,
            consumed: 2,
            played_frame: Some(48_500),
            age_us: 13,
            done: false,
            failed: false,
        }),
    ]
}

#[test]
fn pcm_round_trips_on_private_directions_only() {
    for message in messages() {
        let frame = Frame::Pcm(message);
        for direction in [Direction::ParentToWorker, Direction::WorkerToParent] {
            let bytes = encode(&frame, direction).unwrap();
            assert_eq!(decode(&bytes, direction).unwrap(), frame);
            assert_eq!(
                decode(&bytes, Direction::ExternalToParent),
                Err(WireError::WrongDirection)
            );
        }
        assert_eq!(
            encode(&frame, Direction::ExternalToParent),
            Err(WireError::WrongDirection)
        );
    }
}

#[test]
fn nonfinite_or_oversized_pcm_is_rejected_before_allocation() {
    let frame = Frame::Pcm(messages().remove(1));
    let mut bytes = encode(&frame, Direction::WorkerToParent).unwrap();
    let size = bytes.len();
    bytes[size - 4..].copy_from_slice(&f32::NAN.to_le_bytes());
    let report = decode_with_stats(&bytes, Direction::WorkerToParent);
    assert!(report.result.is_err());
    assert_eq!(report.allocations.requested_bytes, 0);
    // A fake blob size is checked before reserving storage or reading samples.
    let length = size - BYTES - 4;
    bytes[length..length + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let report = decode_with_stats(&bytes, Direction::WorkerToParent);
    assert!(report.result.is_err());
    assert_eq!(report.allocations.requested_bytes, 0);
}

#[test]
fn playback_feedback_does_not_expand_the_command_payload_cap() {
    use kettle_media::wire::{HEADER_BYTES, read_play_control};
    use kettle_media::{BuildId, Hello};
    let direction = Direction::ParentToWorker;
    let hello = encode(
        &Frame::Hello(Hello {
            build_id: BuildId::from_embedded("1.0.0", &"a".repeat(40)).unwrap(),
        }),
        direction,
    )
    .unwrap();
    assert!((2..=64).contains(&(hello.len() - HEADER_BYTES)));
    // The oversized command is refused from its header, without its body.
    assert!(matches!(
        read_play_control(&mut &hello[..HEADER_BYTES], true),
        Err(WireError::Validation(
            kettle_media::ValidationError::TooLarge
        ))
    ));
    let feedback = encode(&Frame::Pcm(messages().remove(3)), direction).unwrap();
    assert!(matches!(
        read_play_control(&mut feedback.as_slice(), true),
        Ok(Some(Frame::Pcm(Message::Feedback(_))))
    ));
    assert!(read_play_control(&mut &feedback[..HEADER_BYTES], false).is_err());
    let block = encode(&Frame::Pcm(messages().remove(1)), direction).unwrap();
    assert!(read_play_control(&mut &block[..HEADER_BYTES], true).is_err());
}
