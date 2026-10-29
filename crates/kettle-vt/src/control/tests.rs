use super::*;

mod differential {
    use crate::AnsiStripper as SharedStripper;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/control/differential.rs"
    ));
}
use differential::CheckedStripper;

fn states() -> Vec<State> {
    let mut states = vec![
        State::Ground,
        State::Escape,
        State::EscapeIntermediate,
        State::Csi,
    ];
    for kind in [Kind::Osc, Kind::Dcs, Kind::Sos, Kind::Pm, Kind::Apc] {
        states.extend([State::String(kind), State::StringEscape(kind)]);
    }
    states
}

#[test]
fn state_event_conformance_matrix() {
    // Every column is a distinct byte class or dispatch in the kernel. The
    // rows specify transitions independently of the parser's match arms.
    #[derive(Clone, Copy, Debug)]
    enum Cell {
        Text,
        Hold,
        Execute,
        End,
        Cancel,
        Esc,
        Pending,
        Inter,
        Csi,
        Osc,
        Dcs,
        Sos,
        Pm,
        Apc,
        St,
    }
    use Cell::*;
    let bytes = [
        0x00, 0x18, 0x1a, 0x1b, 0x20, b'0', b'@', 0x07, 0x90, 0x98, 0x9b, 0x9c, 0x9d, 0x9e, 0x9f,
        0xc3, 0x80, 0x7f, b'[', b']', b'P', b'X', b'^', b'_', b'\\',
    ];
    let ground = [
        Text, Text, Text, Esc, Text, Text, Text, Text, Dcs, Sos, Csi, St, Osc, Pm, Apc, Text, Text,
        Text, Text, Text, Text, Text, Text, Text, Text,
    ];
    let escape = [
        Execute, Cancel, Cancel, Esc, Inter, End, End, Execute, End, End, End, End, End, End, End,
        End, End, Hold, Csi, Osc, Dcs, Sos, Pm, Apc, End,
    ];
    let inter = [
        Execute, Cancel, Cancel, Esc, Hold, End, End, Execute, Hold, Hold, Hold, Hold, Hold, Hold,
        Hold, Hold, Hold, Hold, End, End, End, End, End, End, End,
    ];
    let csi = [
        Execute, Cancel, Cancel, Esc, Hold, Hold, End, Execute, Hold, Hold, Hold, Hold, Hold, Hold,
        Hold, Hold, Hold, Hold, End, End, End, End, End, End, End,
    ];
    let string = [
        Hold, Cancel, Cancel, Pending, Hold, Hold, Hold, Hold, Hold, Hold, Hold, End, Hold, Hold,
        Hold, Hold, Hold, Hold, Hold, Hold, Hold, Hold, Hold, Hold, Hold,
    ];
    let mut covered = 0;
    for state in states() {
        let mut row = match state {
            State::Ground => ground,
            State::Escape | State::StringEscape(_) => escape,
            State::EscapeIntermediate => inter,
            State::Csi => csi,
            State::String(_) => string,
        };
        if state == State::String(Kind::Osc) {
            row[7] = End;
        }
        for (&byte, cell) in bytes.iter().zip(row) {
            let mut parser = ControlParser {
                state,
                ..ControlParser::default()
            };
            let current_kind = parser.kind().unwrap_or(Kind::Escape);
            let (next, event) = match cell {
                Text => (State::Ground, Event::Text),
                Hold | Execute => (
                    if matches!(state, State::StringEscape(_)) {
                        State::Escape
                    } else {
                        state
                    },
                    ControlParser::control(
                        if matches!(state, State::StringEscape(_)) {
                            Kind::Escape
                        } else {
                            current_kind
                        },
                        Action::Continue,
                        matches!(cell, Execute),
                    ),
                ),
                End | Cancel => (
                    State::Ground,
                    ControlParser::control(
                        if matches!(cell, End)
                            && matches!(state, State::StringEscape(_))
                            && byte != b'\\'
                        {
                            Kind::Escape
                        } else {
                            current_kind
                        },
                        if matches!(cell, Cancel) {
                            Action::Cancel
                        } else {
                            Action::End
                        },
                        false,
                    ),
                ),
                Esc => (
                    State::Escape,
                    ControlParser::control(Kind::Escape, Action::Start, false),
                ),
                Pending => (
                    State::StringEscape(current_kind),
                    ControlParser::control(current_kind, Action::Continue, false),
                ),
                Inter => (
                    State::EscapeIntermediate,
                    ControlParser::control(Kind::Escape, Action::Continue, false),
                ),
                St => (
                    State::Ground,
                    ControlParser::control(Kind::St, Action::End, false),
                ),
                Csi | Osc | Dcs | Sos | Pm | Apc => {
                    let kind = match cell {
                        Csi => Kind::Csi,
                        Osc => Kind::Osc,
                        Dcs => Kind::Dcs,
                        Sos => Kind::Sos,
                        Pm => Kind::Pm,
                        Apc => Kind::Apc,
                        _ => unreachable!(),
                    };
                    (
                        if kind == Kind::Csi {
                            State::Csi
                        } else {
                            State::String(kind)
                        },
                        ControlParser::control(kind, Action::Start, false),
                    )
                }
            };
            assert_eq!(parser.advance(byte), event, "{state:?} x {byte:#04x}");
            assert_eq!(parser.state(), next, "{state:?} x {byte:#04x}");
            assert_eq!(parser.utf8_continuation, u8::from(byte == 0xc3));
            covered += 1;
        }
    }
    assert_eq!(covered, 14 * 25);
}

#[test]
fn every_continuation_is_shielded_in_every_state() {
    for state in states() {
        for byte in 0x80..=0xbf {
            for owed in 1..=3 {
                let mut parser = ControlParser {
                    state,
                    utf8_continuation: owed,
                    ..ControlParser::default()
                };
                parser.utf8_kind = parser.kind();
                let expected = match parser.utf8_kind {
                    None => Event::Text,
                    Some(kind) => ControlParser::control(kind, Action::Continue, false),
                };
                assert_eq!(
                    parser.advance(byte),
                    expected,
                    "{state:?}, {byte:#04x}, owed={owed}"
                );
                assert_eq!(parser.state(), state);
                assert_eq!(parser.utf8_continuation, owed - 1);
            }
        }
    }
}

#[test]
fn every_open_state_recovers_at_the_bound() {
    for state in states().into_iter().filter(|state| *state != State::Ground) {
        for byte in [
            0, 0x07, 0x18, 0x1a, b' ', b'0', b'@', 0x9c, 0xc3, 0x80, 0x7f,
        ] {
            let mut parser = ControlParser {
                state,
                control_bytes: MAX_CONTROL_SEQUENCE_BYTES,
                ..ControlParser::default()
            };
            let kind = parser.kind().unwrap();
            assert_eq!(
                parser.advance(byte),
                ControlParser::control(kind, Action::Resync, false)
            );
            assert_eq!(parser.state(), State::Ground);
        }
        let mut parser = ControlParser {
            state,
            control_bytes: MAX_CONTROL_SEQUENCE_BYTES,
            ..ControlParser::default()
        };
        parser.advance(0x1b);
        assert_ne!(parser.state(), State::Ground);
        assert_eq!(parser.control_bytes, 0);
    }
}

fn compare_splits(input: &[u8], expected: &[u8]) {
    for split in 0..=input.len() {
        let mut parser = CheckedStripper::new();
        let mut out = parser.strip(&input[..split]);
        out.extend(parser.strip(&input[split..]));
        assert_eq!(out, expected, "split={split}, input={input:?}");
        parser.assert_matches_original();
    }
}

#[test]
fn differential_edge_table_at_every_split() {
    for (wire, expected) in [
        (b"text\t\r\n\x7f".as_slice(), b"text\t\r\n\x7f".as_slice()),
        (b"a\x1b[31mb\x1b[0m", b"ab"),
        (b"a\x1b]title\x07b", b"ab"),
        (b"a\x1b]title\x1b\\b", b"ab"),
        (b"a\x1bPbody\x07hidden\x9cb", b"ab"),
        (b"a\x1b]old\x1b]new\x07b", b"ab"),
        (b"a\x1b]old\x1b\x1b[31mb", b"ab"),
        (b"a\x1b]old\x1bcb", b"ab"),
        (b"a\x1b]old\x18b", b"ab"),
        (b"a\x1bPold\x1ab", b"ab"),
        (b"a\x1b[0\x18b", b"ab"),
        (b"a\x1b(\x1ab", b"ab"),
        (b"a\x1b[0\t\nmb", b"a\t\nb"),
        (b"a\x1b]old\x1b\x07]new\x07b", b"a\x07b"),
        (
            b"\xc3\x9b\xe2\x80\x98\xf0\x9f\x90\x9d",
            b"\xc3\x9b\xe2\x80\x98\xf0\x9f\x90\x9d",
        ),
        (b"a\x1b]x\xe6\x9c\xabhidden\x07b", b"ab"),
        (b"a\x1b\xe6\x9c\xabb", b"ab"),
        (b"a\x1b[\xe6\x9c\xabmb", b"ab"),
        (b"a\xe2\x41\x9b31mb", b"a\xe2Ab"),
        (
            b"a\x90x\x9cb\x98x\x9cc\x9e\x9cd\x9fx\x9ce\x9dx\x07f\x9b31mg\x9c",
            b"abcdefg",
        ),
    ] {
        compare_splits(wire, expected);
    }
}

#[test]
fn differential_64k_bound_at_two_chunk_splits() {
    let mut wire = b"\x1b]".to_vec();
    wire.extend(std::iter::repeat_n(b'x', MAX_CONTROL_SEQUENCE_BYTES));
    wire.extend_from_slice(b"VISIBLE");
    // Every split near the introducer and the bound, plus a prime stride
    // through the payload, where all three implementations hold only a count.
    let edge = 0..=64;
    let bound = wire.len() - 80..=wire.len();
    let stride = (64..wire.len() - 80).step_by(4093);
    for split in edge.chain(stride).chain(bound) {
        let mut parser = CheckedStripper::new();
        let mut out = parser.strip(&wire[..split]);
        out.extend(parser.strip(&wire[split..]));
        assert_eq!(out, b"VISIBLE", "split at {split}");
        assert_eq!(
            parser.original_outputs(),
            (b"VISIBLE".as_slice(), b"VISIBLE".as_slice())
        );
    }
}

#[test]
fn c1_policy_is_the_only_log_oracle_adaptation() {
    for (intro, terminator) in [
        (0x90, 0x9c),
        (0x98, 0x9c),
        (0x9e, 0x9c),
        (0x9f, 0x9c),
        (0x9d, 0x07),
        (0x9b, b'm'),
    ] {
        let wire = [b'a', intro, b'1', terminator, b'b'];
        let mut parser = CheckedStripper::new();
        assert_eq!(parser.strip(&wire), b"ab");
        let (exec, log) = parser.original_outputs();
        assert_eq!(exec, b"ab");
        assert_eq!(log, wire);
    }
    let mut parser = CheckedStripper::new();
    assert_eq!(parser.strip(b"a\x9cb"), b"ab");
    assert_eq!(
        parser.original_outputs(),
        (b"ab".as_slice(), b"a\x9cb".as_slice())
    );
}

#[test]
fn bound_differences_have_exact_outputs() {
    let n = MAX_CONTROL_SEQUENCE_BYTES;
    let cases = [
        // ESC with only ignored/executed followers was unbounded in exec.
        (
            b"\x1b".as_slice(),
            vec![0x7f; n + 1],
            b"VISIBLE".as_slice(),
            b"VISIBLE".as_slice(),
            b"ISIBLE".as_slice(),
        ),
        // The dispatch no longer gives a long escape prefix a second budget.
        (
            b"\x1b".as_slice(),
            vec![0x7f; n],
            b"[31mVISIBLE".as_slice(),
            b"31mVISIBLE".as_slice(),
            b"VISIBLE".as_slice(),
        ),
        // The byte that exceeds the bound is dropped even if it executes C0.
        (
            b"\x1b[".as_slice(),
            vec![b'0'; n - 1],
            b"\nVISIBLE".as_slice(),
            b"VISIBLE".as_slice(),
            b"\nVISIBLE".as_slice(),
        ),
        // Continuations count too: 32768 scalars exhaust the whole budget.
        (
            b"\x1b]".as_slice(),
            b"\xc3\xa9".repeat(n / 2),
            b"VISIBLE".as_slice(),
            b"VISIBLE".as_slice(),
            b"".as_slice(),
        ),
    ];
    for (intro, payload, suffix, expected, old_exec) in cases {
        let wire = [intro, &payload, suffix].concat();
        let mut parser = CheckedStripper::new();
        let mut out = Vec::new();
        // Every byte boundary, including the recovery byte, is a feed boundary.
        for byte in &wire {
            parser.push(&[*byte], &mut out);
        }
        assert_eq!(out, expected);
        assert_eq!(parser.original_outputs(), (old_exec, expected));
        parser.assert_matches_original();
    }
}

#[test]
fn esc_at_string_bound_starts_a_fresh_budget() {
    let mut wire = b"\x1b]".to_vec();
    wire.extend(std::iter::repeat_n(b'x', MAX_CONTROL_SEQUENCE_BYTES - 1));
    let restart = wire.len();
    wire.extend_from_slice(b"\x1b]secret\x07VISIBLE");
    for split in restart - 2..=wire.len() {
        let mut parser = CheckedStripper::new();
        let mut out = parser.strip(&wire[..split]);
        out.extend(parser.strip(&wire[split..]));
        assert_eq!(out, b"VISIBLE");
        assert_eq!(
            parser.original_outputs(),
            (b"]secret\x07VISIBLE".as_slice(), b"VISIBLE".as_slice())
        );
    }
}

#[test]
fn c1_and_counted_utf8_differences_can_coexist() {
    let mut wire = b"\x9d".to_vec();
    wire.extend(b"\xc3\xa9".repeat(MAX_CONTROL_SEQUENCE_BYTES / 2));
    wire.extend_from_slice(b"VISIBLE");
    let mut parser = CheckedStripper::combining_intended_differences();
    assert_eq!(parser.strip(&wire), b"VISIBLE");
    let (exec, log) = parser.original_outputs();
    assert!(exec.is_empty());
    assert_eq!(log, wire);
}

#[test]
fn deterministic_random_mebibyte_in_random_chunks() {
    fn random(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }
    let mut seed = 0x51a7_c0de_1234_5678;
    let wire: Vec<u8> = (0..1 << 20).map(|_| random(&mut seed) as u8).collect();
    let mut parser = CheckedStripper::new();
    let mut offset = 0;
    while offset < wire.len() {
        let end = (offset + 1 + random(&mut seed) as usize % 8192).min(wire.len());
        parser.strip(&wire[offset..end]);
        offset = end;
    }
    parser.assert_matches_original();
    // No recovery bound is reached in this seed. The unmodified exec oracle
    // therefore agrees for the entire stream, not just at selected boundaries.
    assert_eq!(parser.shared_output(), parser.original_outputs().0);
}

fn passed(chunks: Vec<crate::Chunk>) -> Vec<u8> {
    chunks
        .into_iter()
        .flat_map(|chunk| match chunk {
            crate::Chunk::Pass(bytes) => bytes,
            other => panic!("unexpected semantic chunk: {other:?}"),
        })
        .collect()
}

#[test]
fn stage_two_c1_and_escape_policies_need_separate_hooks() {
    let wire = b"\x9d0;title\x07visible";
    assert_eq!(passed(crate::Extractor::new().feed(wire)), wire);
    assert_eq!(crate::PrivateOutputFilter::default().feed(wire, true), wire);
    assert_eq!(AnsiStripper::new().strip(wire), b"visible");

    // High bytes keep the extractor's escape prefix pending, including when
    // the character is split. The stripper ends an escaped scalar as a unit.
    let wire = b"\x1b\xc3\xa9]777;kettle-completion;private\x07visible";
    let mut extractor = crate::Extractor::new();
    let mut filter = crate::PrivateOutputFilter::default();
    assert!(passed(extractor.feed(&wire[..2])).is_empty());
    assert!(filter.feed(&wire[..2], true).is_empty());
    assert_eq!(passed(extractor.feed(&wire[2..])), b"visible");
    assert_eq!(filter.feed(&wire[2..], true), b"visible");
    assert_eq!(
        AnsiStripper::new().strip(wire),
        b"]777;kettle-completion;private\x07visible"
    );
}

#[test]
fn stage_two_private_non_st_escape_must_not_declassify_payload() {
    let wire = b"\x1b]777;kettle-completion;private\x1bcSECRET\x07visible";
    for split in 0..=wire.len() {
        let mut extractor = crate::Extractor::new();
        let mut filter = crate::PrivateOutputFilter::default();
        let mut chunks = passed(extractor.feed(&wire[..split]));
        chunks.extend(passed(extractor.feed(&wire[split..])));
        let mut filtered = filter.feed(&wire[..split], true);
        filtered.extend(filter.feed(&wire[split..], true));
        assert_eq!(chunks, b"visible");
        assert_eq!(filtered, b"visible");
    }
    assert_eq!(AnsiStripper::new().strip(wire), b"SECRET\x07visible");
}

#[test]
fn stage_two_private_recovery_depends_on_bulk_run_boundaries() {
    let n = crate::completion::MAX_MESSAGE_BYTES;
    let prefix = crate::completion::PREFIX;
    let mut wire = b"\x1b]".to_vec();
    wire.extend_from_slice(prefix);
    wire.extend(std::iter::repeat_n(b'x', 3 * n));
    wire.extend_from_slice(b"\x07visible");

    let whole = crate::PrivateOutputFilter::default().feed(&wire, true);
    assert_eq!(whole, b"visible");
    let mut filter = crate::PrivateOutputFilter::default();
    let mut split = Vec::new();
    for byte in &wire {
        split.extend(filter.feed(&[*byte], true));
    }
    let mut expected = vec![b'x'; n + prefix.len() - 1];
    expected.extend_from_slice(b"\x07visible");
    assert_eq!(split, expected);

    // The kernel itself is independent of input chunking. Preserving both
    // filter outputs requires a run-aware recovery hook, not a byte counter.
    let whole = AnsiStripper::new().strip(&wire);
    let mut stripper = AnsiStripper::new();
    let mut split = Vec::new();
    for byte in &wire {
        stripper.push(&[*byte], &mut split);
    }
    assert_eq!(whole, split);
}
