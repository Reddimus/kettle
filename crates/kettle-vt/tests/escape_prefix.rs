use kettle_vt::{Chunk, Extractor, PrivateOutputFilter};

const PRIVATE: &[u8] = b"]777;kettle-completion;1;show;3;completion;0;fish;git;command\x1b\\";

#[test]
fn escape_controls_keep_the_osc_introducer_active() {
    for prefix in [
        b"\x1b\x1b".as_slice(),
        b"\x1b\n",
        b"\x1b\x7f",
        b"\x1b\r\x1b\n",
    ] {
        let mut wire = prefix.to_vec();
        wire.extend_from_slice(b"]7;file:///tmp\x1b\\");
        for split in 0..=wire.len() {
            let mut extractor = Extractor::new();
            let mut chunks = extractor.feed(&wire[..split]);
            chunks.extend(extractor.feed(&wire[split..]));
            assert!(
                chunks
                    .iter()
                    .any(|chunk| matches!(chunk, Chunk::Cwd(path) if path == "/tmp")),
                "prefix={prefix:?}, split={split}"
            );
        }
    }
}

#[test]
fn private_osc_drops_redundant_escapes_but_keeps_executed_controls() {
    for (prefix, controls) in [
        (b"\x1b\x1b".as_slice(), b"".as_slice()),
        (b"\x1b\n", b"\n"),
        (b"\x1b\x7f", b""),
        (b"\x1b\r\x1b\n", b"\r\n"),
    ] {
        let mut wire = prefix.to_vec();
        wire.extend_from_slice(PRIVATE);
        wire.extend_from_slice(b"visible");
        let mut expected = controls.to_vec();
        expected.extend_from_slice(b"visible");
        for split in 0..=wire.len() {
            let mut filter = PrivateOutputFilter::default();
            let mut output = filter.feed(&wire[..split], true);
            output.extend(filter.feed(&wire[split..], true));
            output.extend(filter.finish(true));
            assert_eq!(output, expected, "prefix={prefix:?}, split={split}");
        }
    }
}

#[test]
fn public_escape_prefixes_round_trip_even_at_eof() {
    let mut extractor = Extractor::new();
    assert!(
        extractor
            .feed(b"\x1b\n]7;file:///tmp\x07")
            .iter()
            .any(|chunk| matches!(chunk, Chunk::Cwd(_)))
    );
    for wire in [
        b"\x1b\x1b]0;title\x07".as_slice(),
        b"\x1b\n[31mred",
        b"\x1b\r\x1b\n",
        b"\x1b\x18]7;ordinary",
    ] {
        for split in 0..=wire.len() {
            let mut filter = PrivateOutputFilter::default();
            let mut output = filter.feed(&wire[..split], true);
            output.extend(filter.feed(&wire[split..], true));
            output.extend(filter.finish(true));
            assert_eq!(output, wire, "split={split}");
        }
    }
}

#[derive(Default)]
struct Observer {
    controls: Vec<u8>,
    oscs: Vec<Vec<u8>>,
}

impl vte::Perform for Observer {
    fn execute(&mut self, byte: u8) {
        self.controls.push(byte);
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _: bool) {
        self.oscs.push(params.join(&b';'));
    }
}

#[test]
fn escape_prefixes_match_vte_across_random_splits() {
    use kettle_test_support::mutate::Rng;
    let mut rng = Rng::new(345);
    for byte in (0..=31)
        .chain(127..=255)
        .filter(|byte| !matches!(byte, 24 | 26))
    {
        for body in [b"]7;file:///tmp\x07".as_slice(), b"]133;A\x1b\\", PRIVATE] {
            let mut wire = vec![0x1b, byte];
            wire.extend_from_slice(body);
            for _ in 0..8 {
                let mut parser = vte::Parser::new();
                let mut oracle = Observer::default();
                let mut terminal = vte::Parser::new();
                let mut observed = Observer::default();
                let mut extractor = Extractor::new();
                let mut count = 0;
                let mut at = 0;
                while at < wire.len() {
                    let end = (at + 1 + rng.below(12)).min(wire.len());
                    parser.advance(&mut oracle, &wire[at..end]);
                    for chunk in extractor.feed(&wire[at..end]) {
                        match chunk {
                            Chunk::Pass(bytes) | Chunk::Terminal(bytes) => {
                                terminal.advance(&mut observed, &bytes)
                            }
                            Chunk::Cwd(_) | Chunk::Prompt(_) | Chunk::Completion(_) => count += 1,
                            _ => {}
                        }
                    }
                    at = end;
                }
                assert_eq!(count, oracle.oscs.len(), "byte={byte}, wire={wire:?}");
                assert_eq!(observed.controls, oracle.controls, "byte={byte}");
            }
        }
    }
}

#[test]
fn escape_prefix_raw_tap_matches_the_independent_filter() {
    for body in [b"]7;file:///tmp\x07".as_slice(), b"]0;title\x07", PRIVATE] {
        let mut wire = b"\x1b\r\x1b\n".to_vec();
        wire.extend_from_slice(body);
        for split in 0..=wire.len() {
            let mut extractor = Extractor::new();
            extractor.set_raw_tap(true);
            let mut chunks = extractor.feed(&wire[..split]);
            chunks.extend(extractor.feed(&wire[split..]));
            let actual: Vec<u8> = chunks
                .into_iter()
                .filter_map(|chunk| match chunk {
                    Chunk::Pass(bytes) | Chunk::Raw(bytes) => Some(bytes),
                    _ => None,
                })
                .flatten()
                .collect();
            let mut filter = PrivateOutputFilter::default();
            if body == PRIVATE {
                assert_eq!(actual, b"\r\n", "private prefix, split={split}");
            } else {
                assert_eq!(actual, wire, "public prefix, split={split}");
            }
            assert_eq!(
                actual,
                filter.feed(&wire, true),
                "body={body:?}, split={split}"
            );
        }
    }
}

#[test]
fn an_overlong_escape_prefix_keeps_controls_and_private_filtering() {
    let mut wire = b"\x1b".to_vec();
    wire.extend(std::iter::repeat_n(b'\n', 128 * 1024));
    wire.extend_from_slice(PRIVATE);
    wire.extend_from_slice(b"visible");
    let mut filter = PrivateOutputFilter::default();
    let output = filter.feed(&wire, true);
    assert_eq!(
        output,
        [vec![b'\n'; 128 * 1024], b"visible".to_vec()].concat()
    );
    let mut filter = PrivateOutputFilter::default();
    assert!(filter.feed(&wire[..1024], false).is_empty());
    assert_eq!(
        filter.feed(&wire[1024..], true),
        [vec![b'\n'; 127 * 1024 + 1], b"visible".to_vec()].concat()
    );
}

#[test]
fn normalized_public_escapes_remain_pass_chunks_without_a_raw_tap() {
    for wire in [b"\x1b\x1b[31m".as_slice(), b"\x1b\n[31m"] {
        let mut extractor = Extractor::new();
        let chunks = extractor.feed(wire);
        assert!(
            chunks.iter().all(|chunk| matches!(chunk, Chunk::Pass(_))),
            "{chunks:?}"
        );
        let bytes: Vec<u8> = chunks
            .into_iter()
            .flat_map(|chunk| match chunk {
                Chunk::Pass(bytes) => bytes,
                _ => unreachable!(),
            })
            .collect();
        let expected: &[u8] = if wire.contains(&b'\n') {
            b"\n\x1b[31m"
        } else {
            b"\x1b[31m"
        };
        assert_eq!(bytes, expected);
    }
}
