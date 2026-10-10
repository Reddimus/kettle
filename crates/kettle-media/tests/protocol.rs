mod common;
use kettle_media::{wire::*, *};

fn bytes(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
fn vectors() -> Vec<(Frame, Direction, &'static str)> {
    vec![
        (
            Frame::Hello(common::hello()),
            Direction::ParentToWorker,
            include_str!("golden/hello.hex"),
        ),
        (
            Frame::Ready(common::ready()),
            Direction::WorkerToParent,
            include_str!("golden/ready.hex"),
        ),
        (
            Frame::ExternalRequest(common::external()),
            Direction::ExternalToParent,
            include_str!("golden/external.hex"),
        ),
        (
            Frame::Job(common::job()),
            Direction::ParentToWorker,
            include_str!("golden/job.hex"),
        ),
        (
            Frame::Rendered(common::rendered()),
            Direction::WorkerToParent,
            include_str!("golden/rendered.hex"),
        ),
        (
            Frame::Failure(Failure {
                code: FailureCode::IndexOutOfRange,
            }),
            Direction::WorkerToParent,
            include_str!("golden/failure.hex"),
        ),
    ]
}
fn roundtrip(frame: Frame, d: Direction) {
    let b = encode(&frame, d).unwrap();
    let report = decode_with_stats(&b, d);
    assert!(report.allocations.requested_bytes <= MAX_DECODE_ALLOCATION_BYTES);
    assert_eq!(report.result.unwrap(), frame);
}
#[test]
fn golden_bytes_each_frame_kind() {
    for (frame, d, golden) in vectors() {
        let expected = bytes(golden);
        assert_eq!(encode(&frame, d).unwrap(), expected);
        assert_eq!(decode(&expected, d).unwrap(), frame);
    }
}
#[test]
fn every_job_source_theme_target_and_result_roundtrips() {
    for kind in [
        JobKind::Mermaid,
        JobKind::Svg,
        JobKind::Raster,
        JobKind::MarkdownDiagrams { index: 31 },
        JobKind::VideoProbe,
        JobKind::VideoStills(VideoStills {
            count: 16,
            max_edge: 4096,
            start_s: 2.0,
            end_s: Some(8.0),
            at_s: None,
            layout: StillsLayout::Sheet {
                cols: 4,
                labels: true,
            },
        }),
        JobKind::VideoStills(VideoStills {
            count: 1,
            max_edge: 1568,
            start_s: 0.0,
            end_s: None,
            at_s: Some(0.25),
            layout: StillsLayout::Poster,
        }),
        JobKind::CachedThumbnail(ThumbnailOf {
            uri_sha256: [9; 32],
            mtime_seconds: -5,
            mtime_nanos: 999_999_999,
        }),
    ] {
        for canvas in [Canvas::Theme, Canvas::White, Canvas::Checker] {
            for source in [
                Source::Bytes(vec![7]),
                Source::Path {
                    path: common::path(),
                    authorization: Authorization::ExternalAttested(ExternalAttested {
                        dev: 4,
                        ino: 5,
                    }),
                },
                Source::user_pull(common::path(), GuiActionWitness::from_explicit_gui_action()),
            ] {
                let mut j = common::job();
                j.kind = kind;
                j.canvas = canvas;
                j.source = source;
                j.theme = Theme {
                    background: [1, 2, 3, 4],
                    foreground: [5, 6, 7, 8],
                    palette: [[16, 32, 64, 255]; 16],
                    accent: [0, 1, 2, 255],
                    is_dark: true,
                };
                j.target = Target {
                    width: 8,
                    height: 9,
                    scale: 2.0,
                    crop: Some(Crop {
                        x: 1,
                        y: 2,
                        width: 7,
                        height: 7,
                    }),
                };
                j.fallback_fonts = (0..8)
                    .map(|face_index| FallbackFont {
                        path: common::path(),
                        face_index,
                    })
                    .collect();
                roundtrip(Frame::Job(j), Direction::ParentToWorker);
            }
        }
    }
    for source in [
        ExternalSource::Bytes(vec![1]),
        ExternalSource::Path {
            path: common::path(),
            attestation: ExternalAttested {
                dev: u64::MAX,
                ino: 12,
            },
        },
    ] {
        let mut j = common::external();
        j.source = source;
        roundtrip(
            Frame::ExternalRequest(j.clone()),
            Direction::ExternalToParent,
        );
        roundtrip(Frame::Job(j.into()), Direction::ParentToWorker);
    }
    let mut r = common::rendered();
    r.source_text = vec!["diagram".into(), "".into()];
    r.fence_sources = vec!["graph LR; A-->B".into(), "graph LR; B-->C".into()];
    r.fence_count = 2;
    r.fence_index = Some(1);
    r.uncovered_scripts = vec!["Han".into(), "Arabic".into()];
    r.warnings = vec![
        Warning::SourceDisplayClipped,
        Warning::MissingGlyphs,
        Warning::FontFallback,
        Warning::SilentVideo,
    ];
    r.digest = content_digest(
        b"content",
        Some(PathIdentity {
            dev: 7,
            ino: 8,
            size: 7,
            mtime_seconds: -1,
            mtime_nanos: 999_999_999,
        }),
    )
    .unwrap();
    roundtrip(Frame::Rendered(r), Direction::WorkerToParent);
    for n in 0..=26 {
        let mut b = bytes(include_str!("golden/failure.hex"));
        b[11] = n;
        let f = decode(&b, Direction::WorkerToParent).unwrap();
        assert_eq!(encode(&f, Direction::WorkerToParent).unwrap(), b);
    }
}
#[test]
fn external_decode_never_yields_user_pull() {
    let request = common::external();
    let encoded = encode(
        &Frame::ExternalRequest(request.clone()),
        Direction::ExternalToParent,
    )
    .unwrap();
    assert_eq!(decode_external_request(&encoded).unwrap(), request);
    let mut j = common::job();
    j.source = Source::user_pull(common::path(), GuiActionWitness::from_explicit_gui_action());
    let mut b = encode(&Frame::Job(j), Direction::ParentToWorker).unwrap();
    assert_eq!(
        decode(&b, Direction::ExternalToParent),
        Err(WireError::WrongDirection)
    );
    b[6] = 3;
    assert_eq!(decode_external_request(&b), Err(WireError::UnknownEnum));
    assert_eq!(
        decode(&b, Direction::ExternalToParent),
        Err(WireError::UnknownEnum)
    );
    for (f, d, _) in vectors() {
        let b = encode(&f, d).unwrap();
        if let Ok(Frame::ExternalRequest(j)) = decode(&b, Direction::ExternalToParent) {
            match Source::from(j.source) {
                Source::Path { authorization, .. } => {
                    assert!(matches!(authorization, Authorization::ExternalAttested(_)))
                }
                Source::Bytes(_) => {}
            }
        }
    }
}
#[test]
fn handshake_and_reverse_skew_are_distinct() {
    let h = common::hello();
    let mut r = common::ready();
    assert_eq!(check_ready(&h, &r), HandshakeOutcome::Compatible);
    for field in 0..3 {
        r = common::ready();
        match field {
            0 => r.build_id.crate_version = format!("{}-skew", h.build_id.crate_version),
            1 => r.build_id.source_hash = "cafe".into(),
            _ => r.build_id.protocol_version = PROTOCOL_VERSION.wrapping_add(1),
        }
        assert_eq!(check_ready(&h, &r), HandshakeOutcome::RestartRequired);
    }
    assert_eq!(
        check_peer_failure(FailureCode::UnknownMethod),
        Some(HandshakeOutcome::ReverseSkew)
    );
    assert_eq!(
        check_peer_failure(FailureCode::RestartRequired),
        Some(HandshakeOutcome::RestartRequired)
    );
    assert_eq!(check_peer_failure(FailureCode::RenderParse), None);
    assert!(
        HandshakeOutcome::ReverseSkew
            .model_message()
            .contains("older")
    );
}
#[test]
fn path_guards() {
    #[cfg(unix)]
    {
        for b in [
            b"relative".to_vec(),
            vec![],
            b"/bad\0path".to_vec(),
            vec![b'/'; MAX_PATH_BYTES + 1],
        ] {
            assert!(NativePath::new(b).is_err());
        }
        assert!(NativePath::new(vec![b'/'; MAX_PATH_BYTES]).is_ok());
        assert!(NativePath::new(vec![b'/', 255]).is_ok());
        let mut j = common::job();
        j.source = Source::Path {
            path: NativePath::new(b"/valid".to_vec()).unwrap(),
            authorization: Authorization::ExternalAttested(ExternalAttested { dev: 0, ino: 0 }),
        };
        let mut b = encode(&Frame::Job(j), Direction::ParentToWorker).unwrap();
        b[18] = b'r';
        assert!(decode(&b, Direction::ParentToWorker).is_err());
    }
}
#[test]
fn rgba_and_crop_checked_products() {
    assert_eq!(
        rgba_len(4096, 4096, MAX_RENDERED_EDGE, MAX_RENDERED_BYTES).unwrap(),
        MAX_RENDERED_BYTES
    );
    assert!(rgba_len(8192, 8192, MAX_DECODED_EDGE, MAX_DECODED_BYTES).is_err());
    for (w, h) in [(0, 1), (1, 0), (4097, 1), (u32::MAX, u32::MAX)] {
        assert!(rgba_len(w, h, MAX_RENDERED_EDGE, MAX_RENDERED_BYTES).is_err());
    }
    assert!(rgba_len(u32::MAX, u32::MAX, u32::MAX, usize::MAX).is_err());
    let mut target = common::job().target;
    target.crop = Some(Crop {
        x: u32::MAX,
        y: 0,
        width: 2,
        height: 1,
    });
    assert!(target.validate().is_err());
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        target.crop = None;
        target.scale = scale;
        assert!(target.validate().is_err());
    }
}
#[test]
fn rgba_mismatch_before_allocation() {
    let mut b = bytes(include_str!("golden/rendered.hex"));
    b[11..15].copy_from_slice(&2u32.to_le_bytes());
    let report = decode_with_stats(&b, Direction::WorkerToParent);
    assert_eq!(report.result, Err(WireError::LengthMismatch));
    assert_eq!(report.allocations.requested_bytes, 0);
}
#[test]
fn strict_lengths_truncation_trailing_enums_utf8() {
    for (frame, d, _) in vectors() {
        let b = encode(&frame, d).unwrap();
        for end in 0..b.len() {
            assert!(decode(&b[..end], d).is_err());
        }
        let mut extra = b.clone();
        extra.push(0);
        assert_eq!(decode(&extra, d), Err(WireError::TrailingBytes));
        let n = u32::from_le_bytes(extra[7..11].try_into().unwrap());
        extra[7..11].copy_from_slice(&(n + 1).to_le_bytes());
        assert_eq!(decode(&extra, d), Err(WireError::TrailingBytes));
        let mut unknown = b.clone();
        unknown[6] = 255;
        assert_eq!(decode(&unknown, d), Err(WireError::UnknownEnum));
        let mut skew = b.clone();
        skew[4..6].copy_from_slice(&PROTOCOL_VERSION.wrapping_add(1).to_le_bytes());
        assert_eq!(decode(&skew, d), Err(WireError::RestartRequired));
        let mut magic = b.clone();
        magic[0] = 0;
        assert_eq!(decode(&magic, d), Err(WireError::BadMagic));
    }
    let mut b = bytes(include_str!("golden/hello.hex"));
    b[15] = 255;
    assert_eq!(
        decode(&b, Direction::ParentToWorker),
        Err(WireError::Validation(ValidationError::InvalidUtf8))
    );
    let mut b = bytes(include_str!("golden/job.hex"));
    b[11] = 0;
    b[17] = 255;
    assert_eq!(
        decode(&b, Direction::ParentToWorker),
        Err(WireError::Validation(ValidationError::InvalidUtf8))
    );
    let mut b = bytes(include_str!("golden/failure.hex"));
    b[11] = 255;
    assert_eq!(
        decode(&b, Direction::WorkerToParent),
        Err(WireError::UnknownEnum)
    );
}
#[test]
fn frame_cap_before_payload_allocation() {
    for d in [
        Direction::ParentToWorker,
        Direction::ExternalToParent,
        Direction::WorkerToParent,
    ] {
        let k = match d {
            Direction::ParentToWorker => 4,
            Direction::ExternalToParent => 3,
            Direction::WorkerToParent => 5,
        };
        let mut b = MAGIC.to_vec();
        b.extend(PROTOCOL_VERSION.to_le_bytes());
        b.push(k);
        b.extend(u32::MAX.to_le_bytes());
        let report = decode_with_stats(&b, d);
        assert_eq!(
            report.result,
            Err(WireError::Validation(ValidationError::TooLarge))
        );
        assert_eq!(report.allocations.requested_bytes, 0);
        assert_eq!(
            read_frame(&mut b.as_slice(), d),
            Err(WireError::Validation(ValidationError::TooLarge))
        );
    }
}
#[test]
fn fence_index_and_all_bounded_metadata() {
    let mut r = common::rendered();
    r.fence_count = 1;
    r.fence_index = Some(1);
    r.fence_sources = vec!["x".into()];
    assert!(r.validate().is_err());
    r.fence_index = Some(0);
    assert!(r.validate().is_ok());
    let mut b = encode(&Frame::Rendered(r.clone()), Direction::WorkerToParent).unwrap();
    // 8 dimensions + 4 length + 4 pixels + 32 digest + 1 option + 4 display-count + count + option.
    b[HEADER_BYTES + 55] = 1;
    assert_eq!(
        decode(&b, Direction::WorkerToParent),
        Err(WireError::Validation(ValidationError::IndexOutOfRange))
    );
    r.source_text = vec!["a".repeat(MAX_SOURCE_LINE_BYTES + 1)];
    assert!(r.validate().is_err());
    r.source_text = vec!["a\nb".into()];
    assert!(r.validate().is_err());
    r.source_text = vec![String::new(); MAX_SOURCE_LINES + 1];
    assert!(r.validate().is_err());
    r.source_text.clear();
    r.fence_sources = vec!["x".repeat(MAX_FENCE_BYTES + 1)];
    assert!(r.validate().is_err());
    r.fence_sources = vec!["".into(); MAX_FENCES + 1];
    r.fence_count = 33;
    assert!(r.validate().is_err());
    r = common::rendered();
    r.uncovered_scripts = vec!["x".repeat(MAX_SCRIPT_BYTES + 1)];
    assert!(r.validate().is_err());
    r.uncovered_scripts = vec![String::new(); MAX_UNCOVERED_SCRIPTS + 1];
    assert!(r.validate().is_err());
    r.uncovered_scripts.clear();
    r.warnings = vec![Warning::FontFallback; MAX_WARNINGS + 1];
    assert!(r.validate().is_err());
}
#[test]
fn input_caps_fonts_and_video_options() {
    for (kind, max) in [
        (JobKind::Mermaid, MAX_MERMAID_BYTES),
        (JobKind::Svg, MAX_SVG_BYTES),
        (JobKind::Raster, MAX_RASTER_BYTES),
        (JobKind::MarkdownDiagrams { index: 0 }, MAX_MARKDOWN_BYTES),
    ] {
        let mut j = common::job();
        j.kind = kind;
        j.source = Source::Bytes(vec![b'a'; max]);
        let valid = encode(&Frame::Job(j.clone()), Direction::ParentToWorker).unwrap();
        assert!(decode(&valid, Direction::ParentToWorker).is_ok());
        j.source = Source::Bytes(vec![b'a'; max + 1]);
        assert!(encode(&Frame::Job(j), Direction::ParentToWorker).is_err());
        // An over-limit blob is rejected even with a short physical frame.
        let offset = if matches!(kind, JobKind::MarkdownDiagrams { .. }) {
            14
        } else {
            13
        };
        let mut hostile = valid[..offset + 5].to_vec();
        hostile[offset..offset + 4].copy_from_slice(&u32::try_from(max + 1).unwrap().to_le_bytes());
        let len = u32::try_from(hostile.len() - HEADER_BYTES).unwrap();
        hostile[7..11].copy_from_slice(&len.to_le_bytes());
        let report = decode_with_stats(&hostile, Direction::ParentToWorker);
        assert_eq!(
            report.result,
            Err(WireError::Validation(ValidationError::TooLarge))
        );
        assert_eq!(report.allocations.requested_bytes, 0);
    }
    let mut j = common::job();
    j.fallback_fonts = vec![
        FallbackFont {
            path: common::path(),
            face_index: 0
        };
        9
    ];
    assert!(encode(&Frame::Job(j), Direction::ParentToWorker).is_err());
    let good = VideoStills {
        count: 9,
        max_edge: 1568,
        start_s: 0.0,
        end_s: Some(9.0),
        at_s: None,
        layout: StillsLayout::Sheet {
            cols: 3,
            labels: true,
        },
    };
    assert!(good.validate().is_ok());
    assert!(
        VideoStills {
            max_edge: kettle_media::MAX_VIDEO_JOB_EDGE,
            ..good
        }
        .validate()
        .is_ok(),
        "a lane's edge, past a model's"
    );
    for bad in [
        VideoStills { count: 0, ..good },
        VideoStills { count: 17, ..good },
        VideoStills {
            max_edge: kettle_media::MAX_VIDEO_JOB_EDGE + 1,
            ..good
        },
        VideoStills {
            layout: StillsLayout::Poster,
            ..good
        },
        VideoStills {
            layout: StillsLayout::Sheet {
                cols: 0,
                labels: false,
            },
            ..good
        },
        VideoStills {
            layout: StillsLayout::Sheet {
                cols: 10,
                labels: false,
            },
            ..good
        },
        VideoStills {
            start_s: f64::NAN,
            ..good
        },
        VideoStills {
            start_s: 10.0,
            ..good
        },
        VideoStills {
            at_s: Some(0.0),
            ..good
        },
        VideoStills {
            end_s: Some(f64::INFINITY),
            ..good
        },
    ] {
        assert!(bad.validate().is_err());
    }
}
#[test]
fn digest_content_and_each_identity_field_change_key() {
    let p = PathIdentity {
        dev: 1,
        ino: 2,
        size: 3,
        mtime_seconds: 4,
        mtime_nanos: 5,
    };
    let d = content_digest(b"abc", Some(p)).unwrap();
    assert_ne!(d, content_digest(b"abd", Some(p)).unwrap());
    assert_ne!(d, content_digest(b"abc", None).unwrap());
    for q in [
        PathIdentity { dev: 2, ..p },
        PathIdentity { ino: 3, ..p },
        PathIdentity { size: 4, ..p },
        PathIdentity {
            mtime_seconds: 5,
            ..p
        },
        PathIdentity {
            mtime_nanos: 6,
            ..p
        },
    ] {
        assert_ne!(d.sha256, content_digest(b"abc", Some(q)).unwrap().sha256);
    }
    assert!(
        content_digest(
            b"",
            Some(PathIdentity {
                mtime_nanos: 1_000_000_000,
                ..p
            })
        )
        .is_err()
    );
}
#[test]
fn build_id_bounds_and_wire_direction() {
    for b in [
        BuildId {
            crate_version: "".into(),
            ..common::build()
        },
        BuildId {
            source_hash: "not-hex".into(),
            ..common::build()
        },
        BuildId {
            source_hash: "a".repeat(65),
            ..common::build()
        },
        BuildId {
            crate_version: "a".repeat(65),
            ..common::build()
        },
    ] {
        assert!(b.validate().is_err());
    }
    for (f, d, _) in vectors() {
        for wrong in [
            Direction::ExternalToParent,
            Direction::ParentToWorker,
            Direction::WorkerToParent,
        ] {
            if wrong != d {
                assert_eq!(encode(&f, wrong), Err(WireError::WrongDirection));
            }
        }
    }
}
#[test]
fn deterministic_mutations_12000() {
    let mut seeds: Vec<_> = vectors()
        .into_iter()
        .map(|(f, d, _)| (encode(&f, d).unwrap(), d))
        .collect();
    let mut j = common::job();
    j.source = Source::user_pull(common::path(), GuiActionWitness::from_explicit_gui_action());
    seeds.push((
        encode(&Frame::Job(j), Direction::ParentToWorker).unwrap(),
        Direction::ParentToWorker,
    ));
    let mut r = common::rendered();
    r.fence_sources = vec!["graph TD;A-->B".into()];
    r.fence_count = 1;
    r.fence_index = Some(0);
    r.source_text = vec!["abc".into()];
    r.uncovered_scripts = vec!["Han".into()];
    r.warnings = vec![Warning::MissingGlyphs];
    seeds.push((
        encode(&Frame::Rendered(r), Direction::WorkerToParent).unwrap(),
        Direction::WorkerToParent,
    ));
    let mut state = 0x8ad9_137b_442c_ef01u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for i in 0..12_000 {
        let (seed, d) = &seeds[i % seeds.len()];
        let mut b = seed.clone();
        match i % 5 {
            0 => {
                let n = next() as usize % (b.len() + 1);
                b.truncate(n);
            }
            1 => {
                let n = next() as usize % b.len();
                b[n] ^= 1 << (next() % 8);
            }
            2 => {
                for _ in 0..1 + next() % 8 {
                    b.push(next() as u8);
                }
            }
            3 => {
                let n = next() as usize % (b.len() - 3);
                b[n..n + 4].copy_from_slice(&(next() as u32).to_le_bytes());
            }
            _ => {
                let n = next() as usize % b.len();
                b[n] = next() as u8;
            }
        }
        let report = decode_with_stats(&b, *d);
        assert!(
            report.allocations.requested_bytes <= d.max_decode_allocation_bytes(),
            "iteration {i}"
        );
        assert!(
            report.allocations.largest_request <= d.max_frame_bytes(),
            "iteration {i}"
        );
        match report.result {
            Ok(f) => assert_eq!(encode(&f, *d).unwrap(), b, "iteration {i}"),
            Err(_) => assert_eq!(report.allocations.requested_bytes, 0, "iteration {i}"),
        }
    }
}

#[test]
fn maximum_reply_and_requested_allocation_budget() {
    let mut r = common::rendered();
    r.width = 4096;
    r.height = 4096;
    r.layout = common::layout(4096, 4096);
    r.exact_source = Some("e".repeat(MAX_EXACT_SOURCE_BYTES));
    r.rgba = vec![128; MAX_RENDERED_BYTES];
    r.source_text = vec!["s".repeat(MAX_SOURCE_LINE_BYTES); MAX_SOURCE_LINES];
    r.fence_count = 32;
    r.fence_index = Some(31);
    r.fence_sources = vec!["f".repeat(MAX_FENCE_BYTES); MAX_FENCES];
    r.uncovered_scripts = vec!["H".repeat(MAX_SCRIPT_BYTES); MAX_UNCOVERED_SCRIPTS];
    r.warnings = vec![Warning::MissingGlyphs; MAX_WARNINGS];
    let b = encode(&Frame::Rendered(r), Direction::WorkerToParent).unwrap();
    assert!(b.len() <= MAX_WORKER_FRAME_BYTES);
    let report = decode_with_stats(&b, Direction::WorkerToParent);
    assert!(report.result.is_ok());
    assert_eq!(report.allocations.largest_request, MAX_RENDERED_BYTES);
    assert!(report.allocations.requested_bytes <= MAX_DECODE_ALLOCATION_BYTES);
    assert!(
        report.allocations.requested_bytes
            >= MAX_RENDERED_BYTES
                + MAX_EXACT_SOURCE_BYTES
                + MAX_SOURCE_LINES * MAX_SOURCE_LINE_BYTES
    );
}

#[test]
fn hostile_metadata_counts_and_unknown_nested_enums_preflight() {
    let worker = Direction::WorkerToParent;
    let parent = Direction::ParentToWorker;
    let base = bytes(include_str!("golden/rendered.hex"));
    for offset in [
        HEADER_BYTES + 49,
        HEADER_BYTES + 55,
        HEADER_BYTES + 59,
        HEADER_BYTES + 63,
    ] {
        let mut b = base.clone();
        b[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let report = decode_with_stats(&b, worker);
        assert!(report.result.is_err());
        assert_eq!(report.allocations.requested_bytes, 0);
    }
    let base = bytes(include_str!("golden/job.hex"));
    for offset in [11, 12, 94, 95, 112] {
        // kind, source, dark flag, canvas, crop option
        let mut b = base.clone();
        b[offset] = 255;
        let report = decode_with_stats(&b, parent);
        assert_eq!(report.result, Err(WireError::UnknownEnum));
        assert_eq!(report.allocations.requested_bytes, 0);
    }
    let mut b = base.clone();
    b[113..117].copy_from_slice(&9u32.to_le_bytes());
    let report = decode_with_stats(&b, parent);
    assert_eq!(
        report.result,
        Err(WireError::Validation(ValidationError::TooLarge))
    );
    assert_eq!(report.allocations.requested_bytes, 0);
    let mut r = common::rendered();
    r.source_text = vec!["x".into()];
    let mut b = encode(&Frame::Rendered(r), worker).unwrap();
    b[HEADER_BYTES + 57] = 255;
    let report = decode_with_stats(&b, worker);
    assert_eq!(
        report.result,
        Err(WireError::Validation(ValidationError::InvalidUtf8))
    );
    assert_eq!(report.allocations.requested_bytes, 0);
}

#[test]
fn streaming_exact_boundaries_and_interrupted_reads() {
    struct InterruptedOnce<'a> {
        bytes: &'a [u8],
        interrupt: bool,
    }
    impl std::io::Read for InterruptedOnce<'_> {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            if self.interrupt {
                self.interrupt = false;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            // Read one byte at a time to exercise exact reads across fragmentation.
            let n = b.len().min(1);
            std::io::Read::read(&mut self.bytes, &mut b[..n])
        }
    }
    let h = Frame::Hello(common::hello());
    let j = Frame::Job(common::job());
    let mut stream = encode(&h, Direction::ParentToWorker).unwrap();
    stream.extend(encode(&j, Direction::ParentToWorker).unwrap());
    let mut reader = InterruptedOnce {
        bytes: &stream,
        interrupt: true,
    };
    assert_eq!(
        read_frame(&mut reader, Direction::ParentToWorker).unwrap(),
        Some(h)
    );
    assert_eq!(
        read_frame(&mut reader, Direction::ParentToWorker).unwrap(),
        Some(j)
    );
    assert_eq!(
        read_frame(&mut reader, Direction::ParentToWorker).unwrap(),
        None
    );
    let mut output = vec![];
    write_frame(
        &mut output,
        &Frame::Rendered(common::rendered()),
        Direction::WorkerToParent,
    )
    .unwrap();
    assert_eq!(output, bytes(include_str!("golden/rendered.hex")));
}

#[test]
fn streamed_auto_reply_keeps_ready_pixels_and_actual_kind() {
    for kind in [
        MediaKind::Raster,
        MediaKind::Svg,
        MediaKind::Mermaid,
        MediaKind::Markdown,
        MediaKind::Video,
    ] {
        let ready = Frame::Ready(common::ready());
        let rendered = Frame::DetectedRendered {
            kind,
            rendered: common::rendered_as(kind),
        };
        let mut stream = Vec::new();
        write_frame(&mut stream, &ready, Direction::WorkerToParent).unwrap();
        write_frame(&mut stream, &rendered, Direction::WorkerToParent).unwrap();
        let mut reader = stream.as_slice();
        assert_eq!(
            read_frame(&mut reader, Direction::WorkerToParent).unwrap(),
            Some(ready)
        );
        assert_eq!(
            read_frame(&mut reader, Direction::WorkerToParent).unwrap(),
            Some(rendered)
        );
        assert_eq!(
            read_frame(&mut reader, Direction::WorkerToParent).unwrap(),
            None
        );
    }
}

#[test]
fn auto_job_has_its_own_tag() {
    let mut job = common::job();
    job.kind = JobKind::Auto;
    let frame = Frame::Job(job);
    let encoded = encode(&frame, Direction::ParentToWorker).unwrap();
    assert_eq!(encoded[HEADER_BYTES], 6);
    assert_eq!(decode(&encoded, Direction::ParentToWorker).unwrap(), frame);
}

/// A cached thumbnail job is tag 7, then the URI's digest, the seconds as
/// a signed little-endian integer and the nanoseconds; nanoseconds past a
/// second do not decode. It renders as a raster, with a raster's cap and
/// deadline.
#[test]
fn cached_thumbnail_job_has_its_own_tag() {
    let of = ThumbnailOf {
        uri_sha256: [0xab; 32],
        mtime_seconds: -2,
        mtime_nanos: 7,
    };
    let mut job = common::job();
    job.kind = JobKind::CachedThumbnail(of);
    let frame = Frame::Job(job);
    let encoded = encode(&frame, Direction::ParentToWorker).unwrap();
    let kind = &encoded[HEADER_BYTES..HEADER_BYTES + 45];
    assert_eq!(kind[0], 7);
    assert_eq!(&kind[1..33], &[0xab; 32]);
    assert_eq!(&kind[33..41], &(-2_i64).to_le_bytes());
    assert_eq!(&kind[41..45], &7_u32.to_le_bytes());
    assert_eq!(decode(&encoded, Direction::ParentToWorker).unwrap(), frame);
    let mut late = encoded.clone();
    late[HEADER_BYTES + 41..HEADER_BYTES + 45].copy_from_slice(&1_000_000_000_u32.to_le_bytes());
    assert_eq!(
        decode(&late, Direction::ParentToWorker),
        Err(WireError::Validation(ValidationError::BadParams))
    );
    let raster = JobKind::Raster;
    let thumbnail = JobKind::CachedThumbnail(of);
    assert_eq!(thumbnail.input_cap(), raster.input_cap());
    assert_eq!(thumbnail.render_deadline(), raster.render_deadline());
}

/// A typed reply is the kind's tag and then exactly the plain reply's
/// payload.
#[test]
fn typed_reply_is_the_kind_then_the_plain_payload() {
    let original = bytes(include_str!("golden/rendered.hex"));
    let mut expected = original[..HEADER_BYTES].to_vec();
    expected[6] = 7;
    let size = u32::from_le_bytes(expected[7..11].try_into().unwrap()) + 1;
    expected[7..11].copy_from_slice(&size.to_le_bytes());
    expected.push(1); // SVG
    expected.extend_from_slice(&original[HEADER_BYTES..]);
    let frame = Frame::DetectedRendered {
        kind: MediaKind::Svg,
        rendered: common::rendered(),
    };
    assert_eq!(encode(&frame, Direction::WorkerToParent).unwrap(), expected);
    assert_eq!(decode(&expected, Direction::WorkerToParent).unwrap(), frame);
}

#[test]
fn typed_reply_refuses_unknown_kinds_and_other_directions() {
    let frame = Frame::DetectedRendered {
        kind: MediaKind::Raster,
        rendered: common::rendered_as(MediaKind::Raster),
    };
    let mut encoded = encode(&frame, Direction::WorkerToParent).unwrap();
    for direction in [Direction::ParentToWorker, Direction::ExternalToParent] {
        assert_eq!(
            encode(&frame, direction).unwrap_err(),
            WireError::WrongDirection
        );
        assert_eq!(
            decode(&encoded, direction).unwrap_err(),
            WireError::WrongDirection
        );
    }
    encoded[HEADER_BYTES] = 5;
    assert_eq!(
        decode(&encoded, Direction::WorkerToParent).unwrap_err(),
        WireError::UnknownEnum
    );
}

#[test]
fn actual_kind_roundtrips_without_altering_pixels_or_source() {
    for kind in [
        MediaKind::Raster,
        MediaKind::Svg,
        MediaKind::Mermaid,
        MediaKind::Markdown,
        MediaKind::Video,
    ] {
        roundtrip(
            Frame::DetectedRendered {
                kind,
                rendered: common::rendered_as(kind),
            },
            Direction::WorkerToParent,
        );
    }
}

/// A reply's source goes with its kind: a textual kind returns its source
/// as read, within that kind's input cap, and no other kind returns any,
/// on encode and decode alike.
#[test]
fn a_replys_source_goes_with_its_kind() {
    let with = |kind, source: Option<String>| {
        let mut rendered = common::rendered();
        rendered.exact_source = source;
        Frame::DetectedRendered { kind, rendered }
    };
    let svg = Some("<svg/>".to_string());
    for frame in [
        with(MediaKind::Raster, svg.clone()),
        with(MediaKind::Video, svg.clone()),
        with(MediaKind::Svg, None),
        with(MediaKind::Mermaid, None),
        with(MediaKind::Mermaid, Some("x".repeat(MAX_MERMAID_BYTES + 1))),
    ] {
        assert!(frame.validate().is_err(), "{frame:?}");
        assert!(encode(&frame, Direction::WorkerToParent).is_err());
    }
    // A raster reply that carries a source anyway is refused on decode.
    let mut bytes = encode(&with(MediaKind::Svg, svg), Direction::WorkerToParent).unwrap();
    let kind_at = HEADER_BYTES;
    assert_eq!(
        bytes[kind_at], 1,
        "the Svg media-kind byte leads the payload"
    );
    bytes[kind_at] = 0;
    assert!(decode(&bytes, Direction::WorkerToParent).is_err());
    // And an SVG reply without its source is refused too.
    let mut bytes = encode(&with(MediaKind::Raster, None), Direction::WorkerToParent).unwrap();
    bytes[kind_at] = 1;
    assert!(decode(&bytes, Direction::WorkerToParent).is_err());
}

/// Neither end reads the other version's frames: a version 5 worker or
/// parent is restarted, never half understood, and so is a newer one.
#[test]
fn version_skew_either_way_is_restart_required() {
    assert_eq!(PROTOCOL_VERSION, 6);
    for (_, direction, golden) in vectors() {
        for version in [1u16, 2, 3, 4, 5, 7] {
            let mut skewed = bytes(golden);
            skewed[4..6].copy_from_slice(&version.to_le_bytes());
            assert_eq!(
                decode(&skewed, direction).unwrap_err(),
                WireError::RestartRequired
            );
        }
    }
}

/// A stills reply carries what the video is and each frame's times, round
/// trip and all; only a video reply does, and a video reply always does.
/// Its values are checked on encode and on decode alike: a rotation off the
/// right angles, an empty or oversized frame list, a time past the end, a
/// zero or excessive frame rate, a tolerance past its bound and an unknown
/// codec or container are refused.
#[test]
fn a_stills_reply_carries_its_video_and_frame_times() {
    let video = common::rendered_as(MediaKind::Video);
    roundtrip(
        Frame::DetectedRendered {
            kind: MediaKind::Video,
            rendered: video.clone(),
        },
        Direction::WorkerToParent,
    );
    roundtrip(Frame::Rendered(video.clone()), Direction::WorkerToParent);
    assert!(video.validate_as(MediaKind::Video).is_ok());
    assert!(
        common::rendered_as(MediaKind::Raster)
            .validate_as(MediaKind::Video)
            .is_err(),
        "a video reply always has its video"
    );
    let mut raster = common::rendered_as(MediaKind::Raster);
    raster.video = Some(common::video_result());
    assert!(
        raster.validate_as(MediaKind::Raster).is_err(),
        "and only it"
    );
    let typed = |rendered: Rendered, kind| {
        encode(
            &Frame::DetectedRendered { kind, rendered },
            Direction::WorkerToParent,
        )
    };
    assert!(typed(raster, MediaKind::Raster).is_err());
    assert!(typed(common::rendered_as(MediaKind::Raster), MediaKind::Video).is_err());
    let with = |change: &dyn Fn(&mut VideoStillsResult)| {
        let mut rendered = common::rendered_as(MediaKind::Video);
        change(rendered.video.as_mut().unwrap());
        rendered
    };
    for (name, bad) in [
        ("rotation", with(&|v| v.info.rotation = 45)),
        ("no frames", with(&|v| v.samples.clear())),
        (
            "too many frames",
            with(&|v| v.samples = vec![v.samples[0]; 17]),
        ),
        ("past the end", with(&|v| v.samples[1].actual_ms = 12_001)),
        (
            "asked past the end",
            with(&|v| v.samples[1].requested_ms = 13_000),
        ),
        ("zero fps", with(&|v| v.info.fps_milli = Some(0))),
        (
            "fps",
            with(&|v| v.info.fps_milli = Some(MAX_VIDEO_FPS_MILLI + 1)),
        ),
        ("a frame past its tolerance", with(&|v| v.tolerance_ms = 27)),
        (
            "tolerance past the duration",
            with(&|v| v.tolerance_ms = 12_001),
        ),
        ("zero width", with(&|v| v.info.width = 0)),
        ("wide", with(&|v| v.info.height = MAX_VIDEO_SIDE + 1)),
        (
            "long",
            with(&|v| v.info.duration_ms = MAX_VIDEO_DURATION_MS + 1),
        ),
    ] {
        assert!(
            typed(bad.clone(), MediaKind::Video).is_err(),
            "{name} encodes"
        );
        // Written past the encoder's checks, the decoder refuses it too.
        let mut bytes = typed(common::rendered_as(MediaKind::Video), MediaKind::Video).unwrap();
        let good = common::rendered_as(MediaKind::Video).video.unwrap();
        let tail = video_tail(&good);
        let at = bytes.len() - tail.len();
        assert_eq!(
            &bytes[at..],
            tail.as_slice(),
            "{name}: the tail is the video"
        );
        let worse = video_tail(bad.video.as_ref().unwrap());
        bytes.truncate(at);
        bytes.extend_from_slice(&worse);
        let length = u32::try_from(bytes.len() - HEADER_BYTES).unwrap();
        bytes[7..11].copy_from_slice(&length.to_le_bytes());
        let report = decode_with_stats(&bytes, Direction::WorkerToParent);
        assert!(report.result.is_err(), "{name} decodes");
        assert_eq!(
            report.allocations.requested_bytes, 0,
            "{name}: refused before any pixel is copied"
        );
    }
    // Unknown codec and container tags are refused.
    let mut bytes = typed(common::rendered_as(MediaKind::Video), MediaKind::Video).unwrap();
    let tail = video_tail(&common::video_result());
    let codec_at = bytes.len() - tail.len() + 1 + 8 + 4 + 4 + 2;
    let mut unknown_codec = bytes.clone();
    unknown_codec[codec_at] = VideoCodec::ALL.len() as u8;
    assert_eq!(
        decode(&unknown_codec, Direction::WorkerToParent).unwrap_err(),
        WireError::UnknownEnum
    );
    let container_at = codec_at + 1 + 5 + 1;
    bytes[container_at] = 12;
    assert_eq!(
        decode(&bytes, Direction::WorkerToParent).unwrap_err(),
        WireError::UnknownEnum
    );
}

/// The bytes a video result takes at the end of a reply, as the codec
/// writes them.
fn video_tail(video: &VideoStillsResult) -> Vec<u8> {
    let mut out = vec![1];
    out.extend(video.info.duration_ms.to_le_bytes());
    out.extend(video.info.width.to_le_bytes());
    out.extend(video.info.height.to_le_bytes());
    out.extend(video.info.rotation.to_le_bytes());
    out.push(video.info.codec as u8);
    out.push(u8::from(video.info.fps_milli.is_some()));
    out.extend(video.info.fps_milli.unwrap_or(0).to_le_bytes());
    out.push(u8::from(video.info.has_audio));
    out.push(match video.info.container {
        None => 0,
        Some(kettle_media::video::VideoContainer::IsoBmff) => 1,
        Some(other) => panic!("fixture container {other:?}"),
    });
    out.extend(u32::try_from(video.samples.len()).unwrap().to_le_bytes());
    for sample in &video.samples {
        out.extend(sample.requested_ms.to_le_bytes());
        out.extend(sample.actual_ms.to_le_bytes());
    }
    out.extend(video.tolerance_ms.to_le_bytes());
    out
}

/// A reply's layout and exact source are checked before anything trusts
/// them: finite positive extents, nonempty rectangles, pixels no more than
/// the rectangle they cover, and source within the textual cap, on encode
/// and on decode alike.
#[test]
fn rendered_layout_and_exact_source_are_checked() {
    let good = common::rendered();
    good.validate().unwrap();
    let mut sparse = good.clone();
    sparse.layout.result_in_target.width = 3;
    sparse.layout.image_in_target.width = 3;
    sparse
        .validate()
        .expect("fewer pixels than the rectangle they cover");
    let mut none = good.clone();
    none.exact_source = None;
    roundtrip(Frame::Rendered(none), Direction::WorkerToParent);
    type Spoil = fn(&mut Rendered);
    let bad: [(&str, Spoil); 8] = [
        ("NaN width", |r| r.layout.source_width = f64::NAN),
        ("infinite height", |r| {
            r.layout.source_height = f64::INFINITY
        }),
        ("zero width", |r| r.layout.source_width = 0.0),
        ("empty image", |r| r.layout.image_in_target.height = 0),
        ("empty result", |r| r.layout.result_in_target.width = 0),
        ("pixels past the result", |r| {
            r.width = 2;
            r.rgba = vec![0; 8];
        }),
        ("rectangle past u32", |r| {
            r.layout.image_in_target.x = u32::MAX
        }),
        ("oversized source", |r| {
            r.exact_source = Some("x".repeat(MAX_EXACT_SOURCE_BYTES + 1))
        }),
    ];
    for (name, spoil) in bad {
        let mut rendered = good.clone();
        spoil(&mut rendered);
        assert!(rendered.validate().is_err(), "{name}");
        assert!(
            encode(
                &Frame::Rendered(rendered.clone()),
                Direction::WorkerToParent
            )
            .is_err(),
            "{name} is not encoded"
        );
    }
    // A reply that skips the encoder's checks is refused on decode too.
    let mut bytes = encode(&Frame::Rendered(good.clone()), Direction::WorkerToParent).unwrap();
    let nan = f64::NAN.to_le_bytes();
    let at = bytes.len() - (8 * 2 + 16 * 2 + 1 + 4 + "<svg/>".len());
    bytes[at..at + 8].copy_from_slice(&nan);
    assert!(decode(&bytes, Direction::WorkerToParent).is_err());
}

fn video_info() -> VideoInfo {
    VideoInfo {
        duration_ms: 9_000,
        width: 640,
        height: 360,
        rotation: 90,
        codec: VideoCodec::H264,
        fps_milli: Some(30_000),
        has_audio: true,
        container: Some(kettle_media::video::VideoContainer::IsoBmff),
    }
}

/// A playback session's frames cross the wire as they were, each only in
/// its own direction.
#[test]
fn a_playback_session_s_frames_roundtrip_in_their_direction() {
    let open = Frame::PlayOpen(PlayOpen {
        source: Source::user_pull(common::path(), GuiActionWitness::from_explicit_gui_action()),
        max_width: 1280,
        max_height: 720,
        start_ms: 4_500,
        max_fps_milli: 30_000,
    });
    let opened = Frame::Opened(Opened {
        info: video_info(),
        width: 640,
        height: 360,
        fps_milli: 30_000,
        start_ms: 4_500,
    });
    let frame = Frame::VideoFrame(VideoFrame {
        seq: 7,
        pts_ms: 4_733,
        width: 2,
        height: 1,
        rgba: vec![1, 2, 3, 255, 4, 5, 6, 255],
    });
    for (frame, direction) in [
        (open.clone(), Direction::ParentToWorker),
        (
            Frame::PlayCommand(PlayCommand::Play),
            Direction::ParentToWorker,
        ),
        (
            Frame::PlayCommand(PlayCommand::Pause),
            Direction::ParentToWorker,
        ),
        (
            Frame::PlayCommand(PlayCommand::Heartbeat),
            Direction::ParentToWorker,
        ),
        (
            Frame::PlayCommand(PlayCommand::Close),
            Direction::ParentToWorker,
        ),
        (opened.clone(), Direction::WorkerToParent),
        (frame.clone(), Direction::WorkerToParent),
        (
            Frame::PlayState(PlayState::Playing),
            Direction::WorkerToParent,
        ),
        (
            Frame::PlayState(PlayState::Paused),
            Direction::WorkerToParent,
        ),
        (
            Frame::PlayState(PlayState::Ended),
            Direction::WorkerToParent,
        ),
    ] {
        roundtrip(frame.clone(), direction);
        for other in [
            Direction::ExternalToParent,
            Direction::ParentToWorker,
            Direction::WorkerToParent,
        ] {
            if other != direction {
                assert!(encode(&frame, other).is_err(), "{frame:?} as {other:?}");
            }
        }
    }
}

/// A session opens only within its bounds: a room of at most 1920 on a
/// side, one to thirty frames a second, a start within any video's length,
/// an opened size of at most 1920 on a side and 1080p's pixels; and an
/// unknown command or state does not decode.
#[test]
fn a_playback_session_opens_only_within_its_bounds() {
    let good = PlayOpen {
        source: Source::user_pull(common::path(), GuiActionWitness::from_explicit_gui_action()),
        max_width: 1920,
        max_height: 1080,
        start_ms: 0,
        max_fps_milli: MAX_PLAY_FPS_MILLI,
    };
    assert!(good.validate().is_ok());
    // The room is a bound only: more pixels than a frame may have is fine.
    assert!(
        PlayOpen {
            max_width: 1920,
            max_height: 1920,
            ..good.clone()
        }
        .validate()
        .is_ok()
    );
    // A decoder holds an open to the same bounds: the source (its tag, the
    // path's tag, length and bytes, the authorization tag) comes before the
    // width.
    let mut wide = encode(&Frame::PlayOpen(good.clone()), Direction::ParentToWorker).unwrap();
    let at = HEADER_BYTES + 1 + 1 + 4 + common::path().as_bytes().len() + 1;
    assert_eq!(wide[at..at + 4], 1920_u32.to_le_bytes());
    wide[at..at + 4].copy_from_slice(&1921_u32.to_le_bytes());
    assert!(decode(&wide, Direction::ParentToWorker).is_err());
    // A session plays a file, never inline bytes, on either side.
    let inline = PlayOpen {
        source: Source::Bytes(vec![0; 16]),
        ..good.clone()
    };
    assert!(inline.validate().is_err());
    let mut bytes = encode(&Frame::PlayOpen(good.clone()), Direction::ParentToWorker).unwrap();
    let source = HEADER_BYTES..HEADER_BYTES + at - HEADER_BYTES;
    let mut blob = vec![0u8];
    blob.extend_from_slice(&16_u32.to_le_bytes());
    blob.extend_from_slice(&[0; 16]);
    bytes.splice(source, blob);
    let payload = u32::try_from(bytes.len() - HEADER_BYTES).unwrap();
    bytes[7..HEADER_BYTES].copy_from_slice(&payload.to_le_bytes());
    assert_eq!(
        decode(&bytes, Direction::ParentToWorker),
        Err(WireError::Validation(ValidationError::BadParams))
    );
    for bad in [
        PlayOpen {
            max_width: 1921,
            max_height: 1,
            ..good.clone()
        },
        PlayOpen {
            max_width: 1,
            max_height: 1921,
            ..good.clone()
        },
        PlayOpen {
            max_width: 0,
            ..good.clone()
        },
        PlayOpen {
            max_fps_milli: 0,
            ..good.clone()
        },
        PlayOpen {
            max_fps_milli: MAX_PLAY_FPS_MILLI + 1,
            ..good.clone()
        },
        PlayOpen {
            start_ms: MAX_VIDEO_DURATION_MS + 1,
            ..good.clone()
        },
    ] {
        assert!(
            bad.validate().is_err(),
            "{:?}",
            (bad.max_width, bad.max_height, bad.max_fps_milli)
        );
        assert!(encode(&Frame::PlayOpen(bad), Direction::ParentToWorker).is_err());
    }
    let opened = Opened {
        info: video_info(),
        width: 640,
        height: 360,
        fps_milli: 30_000,
        start_ms: 9_000,
    };
    assert!(opened.validate().is_ok());
    assert!(
        Opened {
            start_ms: 9_001,
            ..opened
        }
        .validate()
        .is_err()
    );
    assert!(
        Opened {
            fps_milli: 30_001,
            ..opened
        }
        .validate()
        .is_err()
    );
    // And an opened session too: the video's 26 bytes, then the size and
    // rate before the start.
    let mut late = encode(&Frame::Opened(opened), Direction::WorkerToParent).unwrap();
    let at = HEADER_BYTES + 26 + 12;
    late[at..at + 8].copy_from_slice(&9_001_u64.to_le_bytes());
    assert!(decode(&late, Direction::WorkerToParent).is_err());
    for (frame, direction, tag) in [
        (
            Frame::PlayCommand(PlayCommand::Close),
            Direction::ParentToWorker,
            4,
        ),
        (
            Frame::PlayState(PlayState::Ended),
            Direction::WorkerToParent,
            3,
        ),
    ] {
        let mut bytes = encode(&frame, direction).unwrap();
        *bytes.last_mut().unwrap() = tag;
        assert_eq!(decode(&bytes, direction), Err(WireError::UnknownEnum));
    }
}

/// A frame's pixels are exactly its size, and a header claiming more than a
/// frame may hold is refused before any pixel is held.
#[test]
fn a_video_frame_is_exactly_its_size() {
    let frame = |width: u32, height: u32, len: usize| VideoFrame {
        seq: 0,
        pts_ms: 0,
        width,
        height,
        rgba: vec![0; len],
    };
    roundtrip(
        Frame::VideoFrame(frame(1920, 1080, 1920 * 1080 * 4)),
        Direction::WorkerToParent,
    );
    for bad in [
        frame(1921, 1, 1921 * 4),
        frame(1920, 1081, 1920 * 1081 * 4),
        frame(2, 1, 7),
        frame(2, 1, 9),
        frame(0, 1, 0),
    ] {
        assert!(bad.validate().is_err());
        assert!(encode(&Frame::VideoFrame(bad), Direction::WorkerToParent).is_err());
    }
    // A header that says 1920 by 1080 but a length past it.
    let mut bytes = encode(
        &Frame::VideoFrame(frame(2, 1, 8)),
        Direction::WorkerToParent,
    )
    .unwrap();
    let at = HEADER_BYTES + 16;
    bytes[at..at + 4].copy_from_slice(&1920_u32.to_le_bytes());
    bytes[at + 4..at + 8].copy_from_slice(&1080_u32.to_le_bytes());
    bytes[at + 8..at + 12].copy_from_slice(&u32::MAX.to_le_bytes());
    let report = decode_with_stats(&bytes, Direction::WorkerToParent);
    assert!(report.result.is_err());
    assert_eq!(report.allocations.requested_bytes, 0);
    // A size whose pixels are more than the blob holds.
    let mut short = encode(
        &Frame::VideoFrame(frame(2, 1, 8)),
        Direction::WorkerToParent,
    )
    .unwrap();
    short[at..at + 4].copy_from_slice(&3_u32.to_le_bytes());
    assert!(decode(&short, Direction::WorkerToParent).is_err());
    // And a frame past what one may be, even with exactly its pixels; the
    // same bytes at the largest width decode.
    let sized = |width: u32| {
        let mut bytes = encode(
            &Frame::VideoFrame(frame(1920, 1, 1920 * 4)),
            Direction::WorkerToParent,
        )
        .unwrap();
        bytes.resize(at + 12 + width as usize * 4, 0);
        bytes[at..at + 4].copy_from_slice(&width.to_le_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&(width * 4).to_le_bytes());
        let payload = u32::try_from(bytes.len() - HEADER_BYTES).unwrap();
        bytes[7..HEADER_BYTES].copy_from_slice(&payload.to_le_bytes());
        bytes
    };
    assert!(decode(&sized(1920), Direction::WorkerToParent).is_ok());
    assert!(decode(&sized(1921), Direction::WorkerToParent).is_err());
}

/// A frame written from its buffer is the frame encoded, and refused as it
/// is: the same bytes, the same bounds.
#[test]
fn a_video_frame_written_from_its_buffer_is_the_frame_encoded() {
    let frame = VideoFrame {
        seq: 41,
        pts_ms: 1_366,
        width: 3,
        height: 2,
        rgba: (0..24).collect(),
    };
    let mut written = Vec::new();
    write_video_frame(&mut written, frame.borrowed()).unwrap();
    assert_eq!(
        written,
        encode(&Frame::VideoFrame(frame.clone()), Direction::WorkerToParent).unwrap()
    );
    assert_eq!(
        decode(&written, Direction::WorkerToParent).unwrap(),
        Frame::VideoFrame(frame.clone())
    );
    let short = VideoFrameRef {
        rgba: &frame.rgba[..23],
        ..frame.borrowed()
    };
    let wide = VideoFrameRef {
        width: 1921,
        height: 1,
        rgba: &[0; 1921 * 4],
        ..frame.borrowed()
    };
    for bad in [short, wide] {
        let mut written = Vec::new();
        assert!(write_video_frame(&mut written, bad).is_err());
        assert!(written.is_empty());
    }
}

/// A session's picture is the video as shown, fitted to the room it has with
/// its shape kept, never enlarged, and never past 1920 on a side or 1080p's
/// pixels.
#[test]
fn a_session_fits_its_picture_to_the_room() {
    for (shown, room, fitted) in [
        // Smaller than the room: as it is.
        ((640, 360), (1280, 720), Some((640, 360))),
        // Wider than the room: its width, the height kept in shape.
        ((3840, 2160), (1280, 1280), Some((1280, 720))),
        // Taller: its height.
        ((1080, 1920), (1280, 960), Some((540, 960))),
        // A large room is held to the session's edge and pixels.
        ((3840, 2160), (4000, 4000), Some((1920, 1080))),
        ((4000, 4000), (1920, 1920), Some((1440, 1440))),
        // Held to the pixels after the room: rounded once, from the video.
        ((1139, 2001), (1920, 1920), Some((1086, 1908))),
        ((7680, 100), (1920, 1080), Some((1920, 25))),
        // A sliver keeps at least a pixel.
        ((10_000, 1), (100, 100), Some((100, 1))),
        ((0, 360), (100, 100), None),
        ((640, 360), (0, 100), None),
    ] {
        assert_eq!(
            play_size(shown.0, shown.1, room.0, room.1),
            fitted,
            "{shown:?} in {room:?}"
        );
    }
    // Every fit is within its room and the session's bounds.
    for shown in [
        (1, 1),
        (1919, 1081),
        (2001, 999),
        (1139, 2001),
        (2001, 1139),
        (16_384, 16_384),
        (3, 16_384),
        (4097, 2161),
    ] {
        for room in [(1, 1), (1920, 1080), (1920, 1920), (333, 777)] {
            let (width, height) = play_size(shown.0, shown.1, room.0, room.1).unwrap();
            assert!(width <= room.0.min(shown.0).max(1) && height <= room.1.min(shown.1).max(1));
            assert!(width <= MAX_PLAY_EDGE && height <= MAX_PLAY_EDGE);
            assert!(u64::from(width) * u64::from(height) <= u64::from(MAX_PLAY_PIXELS));
            // Its shape kept to within a pixel on either side.
            let (shown_width, shown_height) = (u64::from(shown.0), u64::from(shown.1));
            let drift = (u64::from(width) * shown_height).abs_diff(u64::from(height) * shown_width);
            assert!(
                drift <= shown_width.max(shown_height),
                "{shown:?} in {room:?} drew {width}x{height}"
            );
        }
    }
}

/// A session's reader takes a frame's pixels straight into the buffer it is
/// given, at the size the session opened at; other frames come whole, under
/// their cap; a frame of another size is refused before its pixels are read.
#[test]
fn a_session_reads_its_frames_into_one_buffer() {
    use std::io::Cursor;

    let frame = VideoFrame {
        seq: 3,
        pts_ms: 300,
        width: 2,
        height: 1,
        rgba: vec![9, 8, 7, 6, 5, 4, 3, 2],
    };
    let mut stream = Vec::new();
    write_video_frame(&mut stream, frame.borrowed()).unwrap();
    write_frame(
        &mut stream,
        &Frame::PlayState(PlayState::Ended),
        Direction::WorkerToParent,
    )
    .unwrap();
    let mut reader = Cursor::new(stream);
    let mut pixels = Vec::with_capacity(64);
    assert_eq!(
        read_session_frame(&mut reader, 2, 1, &mut pixels, 256).unwrap(),
        Some(SessionRead::Pixels {
            seq: 3,
            pts_ms: 300
        })
    );
    assert_eq!(pixels, frame.rgba);
    assert_eq!(
        read_session_frame(&mut reader, 2, 1, &mut pixels, 256).unwrap(),
        Some(SessionRead::Frame(Box::new(Frame::PlayState(
            PlayState::Ended
        ))))
    );
    assert_eq!(
        read_session_frame(&mut reader, 2, 1, &mut pixels, 256).unwrap(),
        None
    );

    // Another size, by its length or only by its fields: nothing of its
    // pixels is read.
    let other = VideoFrame {
        width: 1,
        height: 2,
        ..frame.clone()
    };
    let mut swapped = Vec::new();
    write_video_frame(&mut swapped, other.borrowed()).unwrap();
    let mut reader = Cursor::new(swapped);
    assert_eq!(
        read_session_frame(&mut reader, 2, 1, &mut pixels, 256),
        Err(WireError::Validation(ValidationError::BadParams))
    );
    assert_eq!(reader.position() as usize, HEADER_BYTES + 28);
    let wider = VideoFrame {
        width: 3,
        rgba: vec![0; 12],
        ..frame.clone()
    };
    let mut longer = Vec::new();
    write_video_frame(&mut longer, wider.borrowed()).unwrap();
    let mut reader = Cursor::new(longer);
    assert_eq!(
        read_session_frame(&mut reader, 2, 1, &mut pixels, 256),
        Err(WireError::Validation(ValidationError::BadParams))
    );
    assert_eq!(reader.position() as usize, HEADER_BYTES);

    // Pixels cut short; another frame over its cap; a size no session has.
    let mut cut = Vec::new();
    write_video_frame(&mut cut, frame.borrowed()).unwrap();
    cut.truncate(cut.len() - 1);
    assert_eq!(
        read_session_frame(&mut Cursor::new(cut), 2, 1, &mut pixels, 256),
        Err(WireError::Truncated)
    );
    let opened = encode(
        &Frame::Opened(Opened {
            info: video_info(),
            width: 2,
            height: 1,
            fps_milli: 30_000,
            start_ms: 0,
        }),
        Direction::WorkerToParent,
    )
    .unwrap();
    assert_eq!(
        read_session_frame(&mut Cursor::new(opened.clone()), 2, 1, &mut pixels, 32),
        Err(WireError::Validation(ValidationError::TooLarge))
    );
    assert!(read_session_frame(&mut Cursor::new(opened), 2, 1, &mut pixels, 256).is_ok());
    assert!(read_session_frame(&mut Cursor::new(Vec::new()), 1921, 1, &mut pixels, 256).is_err());
}
