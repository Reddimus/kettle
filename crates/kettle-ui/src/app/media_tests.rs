#[test]
fn run_command_output_budget_includes_json_escaping_and_metadata() {
    use kettle_ctl::protocol::{MAX_RESPONSE_LINE_BYTES, Response, to_json_vec_bounded};
    for timed_out in [false, true] {
        for unit in ["\"", "\\", "\0", "\n", "e\u{301}"] {
            let original = unit.repeat(450_000);
            let mut result = serde_json::json!({
                "pane": u64::MAX,
                "exit_code": if timed_out { None } else { Some(17) },
                "timed_out": timed_out,
                "output": original,
                "output_truncated": false,
            });
            if timed_out {
                result["hint"] = serde_json::json!(
                    "no OSC 133 command-end seen; enable shell integration for exit codes"
                );
            } else {
                result["duration_ms"] = serde_json::json!(u64::MAX);
            }
            let response = super::cap_ctl_command_response(Response::ok(u64::MAX, result));
            let bytes = to_json_vec_bounded(&response, MAX_RESPONSE_LINE_BYTES).unwrap();
            let decoded: Response = serde_json::from_slice(&bytes).unwrap();
            assert!(decoded.ok);
            assert_eq!(decoded.result["timed_out"], timed_out);
            assert_eq!(
                decoded.result["exit_code"],
                if timed_out {
                    serde_json::Value::Null
                } else {
                    serde_json::json!(17)
                }
            );
            assert_eq!(decoded.result["output_truncated"], true);
            let output = decoded.result["output"].as_str().unwrap();
            assert!(!output.is_empty());
            assert!(original.starts_with(output));
            if !timed_out {
                assert_eq!(decoded.result["duration_ms"], u64::MAX);
            }
        }
    }
}

#[test]
fn finding_facts_preserve_local_dismissal() {
    let body = finding_body(include_str!("chrome.rs"), "apply_update_facts");
    assert!(
        body.contains("self.update_local_dismissals"),
        "incoming disk facts must preserve local dismissals"
    );
}
#[test]
fn finding_stale_crop_failure_cannot_cancel_reload() {
    let source = include_str!("media.rs");
    let body = finding_body(source, "finish_lane_crop");
    let before_result = body.split("let verdict = match &result").next().unwrap();
    assert!(
        before_result.contains("item.generation == render.generation"),
        "crop failures must reject obsolete item generations before changing notices or jobs"
    );
    let mark = finding_body(source, "mark_preview_changed");
    assert!(
        mark.contains("withdraw_lane_renders(render.pane, render.item, render.generation)"),
        "withdrawal must be limited to the failed generation"
    );
}

#[test]
fn review_idle_media_tick_consumes_late_file_facts() {
    let (mut media, mut ws, pane, query, woke) =
        review_file_facts_fixture(Some(review_file_identity()));
    let _ = media.request_facts(query, None, Arc::new(|| {}));
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert!(media.is_idle(), "rendering has already finished");
    assert!(!super::media::media_tick_ready(
        &mut media,
        &mut ws,
        &mut Default::default(),
        #[cfg(target_os = "linux")]
        &mut Default::default(),
    ));
    let facts = ws.mux.panes[&pane]
        .media_shelf
        .get(73)
        .unwrap()
        .kind
        .file()
        .expect("late metadata must complete the placeholder");
    assert_eq!(facts.bytes, 12345);
}

#[test]
fn review_file_show_reply_waits_for_metadata() {
    let (mut media, mut ws, pane, mut query, woke) =
        review_file_facts_fixture(Some(review_file_identity()));
    let (reply, received) = crossbeam_channel::bounded(1);
    let origin = crate::media::Origin {
        conn_id: 1,
        request_id: 91,
        reply,
    };
    let mut result = kettle_ctl::show::ShowResult::new(
        (pane, true, 1),
        73,
        kettle_media::MediaKind::Raster,
        (0, 0),
        &[],
    );
    result.kind = kettle_ctl::show::FILE_KIND.into();
    super::media::prepare_file_reply(&mut query, origin, result);
    assert!(
        received.is_empty(),
        "show must await its file metadata while the placeholder is visible"
    );
    let _ = media.request_facts(query, None, Arc::new(|| {}));
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    super::media::media_tick_ready(
        &mut media,
        &mut ws,
        &mut Default::default(),
        #[cfg(target_os = "linux")]
        &mut Default::default(),
    );
    let response = received
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert_eq!(response.id, 91);
    let result: kettle_ctl::show::ShowResult = serde_json::from_value(response.result).unwrap();
    assert_eq!(result.file.as_ref().unwrap().bytes, 12345);
    assert_eq!(result.file.as_ref().unwrap().reason, "unsupported_media");
    assert_eq!(result.summary(), "file 12 kB, a kind Kettle does not draw");
    assert_eq!(result.kind, "file");
}

#[test]
fn review_failed_file_facts_finish_the_placeholder() {
    let (mut media, mut ws, pane, query, woke) = review_file_facts_fixture(None);
    let _ = media.request_facts(query, None, Arc::new(|| {}));
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    super::media::media_tick_ready(
        &mut media,
        &mut ws,
        &mut Default::default(),
        #[cfg(target_os = "linux")]
        &mut Default::default(),
    );
    let item = ws.mux.panes[&pane].media_shelf.get(73).unwrap();
    assert!(
        !matches!(item.kind, crate::media::ItemKind::CheckingFile(_)),
        "failed metadata must stop checking"
    );
    assert!(
        item.kind.file().is_none(),
        "failure must not invent a file identity or size"
    );
}

#[test]
fn review_missing_facts_worker_completes_with_failure() {
    let (_, _, _, query, _) = review_file_facts_fixture(None);
    let mut media = crate::media::MediaService::default();
    assert!(
        media.request_facts(query, None, Arc::new(|| {})).is_err(),
        "an unavailable worker must return its request for failure completion"
    );
}

#[test]
fn review_full_facts_queue_returns_the_request_for_failure_completion() {
    let (entered, started) = std::sync::mpsc::channel();
    let (release, waiting) = std::sync::mpsc::channel::<()>();
    let lane = crate::media::FactsLane::start(
        move |_| {
            let _ = entered.send(());
            let _ = waiting.recv();
            None
        },
        Arc::new(|| {}),
    )
    .unwrap();
    let mut media = crate::media::MediaService::default();
    media.set_facts_lane(lane);
    let query = || crate::media::FactsQuery {
        window: 1,
        pane: 2,
        item: 3,
        generation: 0,
        control: Default::default(),
        source: kettle_media::Source::Bytes(Vec::new()),
        why: crate::media::FileWhy::Unsupported,
        reply: None,
    };
    assert!(media.request_facts(query(), None, Arc::new(|| {})).is_ok());
    started
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    for _ in 0..32 {
        assert!(media.request_facts(query(), None, Arc::new(|| {})).is_ok());
    }
    let rejected = media.request_facts(query(), None, Arc::new(|| {}));
    drop(release);
    assert!(
        rejected.is_err(),
        "a saturated lane must not silently lose a placeholder or reply"
    );
}

#[test]
fn failed_facts_refuse_the_pending_show_reply() {
    let (mut media, mut ws, pane, mut query, woke) = review_file_facts_fixture(None);
    let (reply, received) = crossbeam_channel::bounded(1);
    let origin = crate::media::Origin {
        conn_id: 1,
        request_id: 92,
        reply,
    };
    let result = kettle_ctl::show::ShowResult::file((pane, true, 1), 73, 0, "unsupported_media");
    super::media::prepare_file_reply(&mut query, origin, result);
    assert!(received.is_empty());
    assert!(media.request_facts(query, None, Arc::new(|| {})).is_ok());
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    super::media::media_tick_ready(
        &mut media,
        &mut ws,
        &mut Default::default(),
        #[cfg(target_os = "linux")]
        &mut Default::default(),
    );
    let response = received
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert!(!response.ok);
    assert_eq!(response.id, 92);
    assert_eq!(
        ws.mux.panes[&pane].media_shelf.get(73).unwrap().kind,
        crate::media::ItemKind::FailedFile
    );
}

#[test]
fn failed_facts_from_an_old_generation_leave_the_new_placeholder_alone() {
    let (mut media, mut ws, pane, query, woke) = review_file_facts_fixture(None);
    ws.mux
        .panes
        .get_mut(&pane)
        .unwrap()
        .media_shelf
        .get_mut(73)
        .unwrap()
        .generation = 1;
    assert!(media.request_facts(query, None, Arc::new(|| {})).is_ok());
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    super::media::media_tick_ready(
        &mut media,
        &mut ws,
        &mut Default::default(),
        #[cfg(target_os = "linux")]
        &mut Default::default(),
    );
    let item = ws.mux.panes[&pane].media_shelf.get(73).unwrap();
    assert!(matches!(item.kind, crate::media::ItemKind::CheckingFile(_)));
    assert_eq!(item.generation, 1);
}

#[test]
fn followup_replaced_restore_snapshot_disables_saving() {
    let temp = kettle_test_support::private_tempdir("kettle-replaced-restore-");
    let path = temp.path().join("session.json");
    std::fs::write(&path, br#"{"tabs":[],"active":0}"#).unwrap();
    let (_, identity) = crate::session::load_snapshot_result(&path)
        .unwrap()
        .unwrap();
    let newer = temp.path().join("newer.json");
    let contents = br#"{"tabs":[],"active":1}"#;
    std::fs::write(&newer, contents).unwrap();
    std::fs::rename(&newer, &path).unwrap();
    let mut failed = false;
    super::sessions::recover_startup_snapshot(
        Some(&path),
        Some(&identity),
        &mut failed,
        "restore rejected",
    );
    assert!(failed, "failed recovery must disable automatic saves");
    assert_eq!(std::fs::read(&path).unwrap(), contents);
}

#[test]
fn followup_completed_file_card_uses_facts_in_message_and_registration() {
    let (mut media, mut ws, pane, mut query, woke) =
        review_file_facts_fixture(Some(review_file_identity()));
    let (reply, received) = crossbeam_channel::bounded(1);
    let origin = crate::media::Origin {
        conn_id: 1,
        request_id: 93,
        reply,
    };
    let mut result = kettle_ctl::show::ShowResult::new(
        (pane, true, 1),
        73,
        kettle_media::MediaKind::Raster,
        (0, 0),
        &[],
    );
    result.kind = kettle_ctl::show::FILE_KIND.into();
    let state = ws.mux.panes.get_mut(&pane).unwrap();
    let item = state.media_shelf.get(73).unwrap();
    let image = item.card_image().unwrap();
    let (delivery, pending) = super::media::prepare_card(
        &mut media.cards,
        (pane, &mut state.inline_cards, (80, 24), 0),
        (8.0, 16.0),
        (73, 0),
        (item.kind, None, None),
        &image,
        crate::media::InlineDraft {
            #[cfg(target_os = "linux")]
            launch: None,
            owner: kettle_ctl::process::ProcessIdentity::new_for_tests(40, 400),
            harness: kettle_render::CardHarness::Cli,
            name: Some("report.docx".into()),
            printer: None,
        },
        std::time::Instant::now(),
    )
    .unwrap();
    result.inline = delivery;
    assert!(
        result.inline.is_none(),
        "checking files must not have printable text"
    );
    assert_eq!(media.cards.len(), 0, "registration must wait for the facts");
    super::media::prepare_file_reply(&mut query, origin, result);
    query.reply.as_mut().unwrap().inline = pending;
    assert!(media.request_facts(query, None, Arc::new(|| {})).is_ok());
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    super::media::media_tick_ready(
        &mut media,
        &mut ws,
        &mut Default::default(),
        #[cfg(target_os = "linux")]
        &mut Default::default(),
    );
    let response = received
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let result: kettle_ctl::show::ShowResult = serde_json::from_value(response.result).unwrap();
    let message = &result
        .inline
        .expect("completed file must have an inline card")
        .message;
    assert_eq!(message.lines().last(), Some("    report.docx - file 12 kB"));
    assert!(!message.contains("checking"));
    let nonces = media.cards.cards_of(pane, (73, 0));
    assert_eq!(nonces.len(), 1);
    let state = ws.mux.panes.get_mut(&pane).unwrap();
    let mut term = state.term.term.lock().unwrap();
    let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
    parser.advance(&mut *term, message.as_bytes());
    let mut snapshot = kettle_render::PaneSnapshot::default();
    snapshot.capture_with_card_marks(&term, true);
    assert_eq!(
        state.inline_cards.placements(&snapshot).len(),
        1,
        "the registered caption must match the printed completed caption"
    );
}

#[test]
fn regression_waiting_file_card_uses_current_pane_geometry() {
    let (mut media, mut ws, pane, mut query, woke) =
        review_file_facts_fixture(Some(review_file_identity()));
    let (reply, received) = crossbeam_channel::bounded(1);
    let origin = crate::media::Origin {
        conn_id: 1,
        request_id: 93,
        reply,
    };
    let mut result = kettle_ctl::show::ShowResult::new(
        (pane, true, 1),
        73,
        kettle_media::MediaKind::Raster,
        (0, 0),
        &[],
    );
    result.kind = kettle_ctl::show::FILE_KIND.into();
    let state = ws.mux.panes.get_mut(&pane).unwrap();
    let item = state.media_shelf.get(73).unwrap();
    let image = item.card_image().unwrap();
    let (delivery, pending) = super::media::prepare_card(
        &mut media.cards,
        (pane, &mut state.inline_cards, (80, 24), 0),
        (8.0, 16.0),
        (73, 0),
        (item.kind, None, None),
        &image,
        crate::media::InlineDraft {
            #[cfg(target_os = "linux")]
            launch: None,
            owner: kettle_ctl::process::ProcessIdentity::new_for_tests(40, 400),
            harness: kettle_render::CardHarness::Cli,
            name: Some("report.docx".into()),
            printer: None,
        },
        std::time::Instant::now(),
    )
    .unwrap();
    result.inline = delivery;
    assert!(
        result.inline.is_none(),
        "checking files must not have printable text"
    );
    assert_eq!(media.cards.len(), 0, "registration must wait for the facts");
    super::media::prepare_file_reply(&mut query, origin, result);
    query.reply.as_mut().unwrap().inline = pending;
    assert!(media.request_facts(query, None, Arc::new(|| {})).is_ok());
    ws.mux
        .panes
        .get_mut(&pane)
        .unwrap()
        .term
        .resize(22, 12, 8, 16)
        .unwrap();
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    super::media::media_tick_ready(
        &mut media,
        &mut ws,
        &mut Default::default(),
        #[cfg(target_os = "linux")]
        &mut Default::default(),
    );
    let response = received
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let result: kettle_ctl::show::ShowResult = serde_json::from_value(response.result).unwrap();
    let message = &result
        .inline
        .expect("completed file must have an inline card")
        .message;

    assert!(!message.contains("checking"));
    let nonces = media.cards.cards_of(pane, (73, 0));
    assert_eq!(nonces.len(), 1);
    let state = ws.mux.panes.get_mut(&pane).unwrap();
    let mut term = state.term.term.lock().unwrap();
    let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
    parser.advance(&mut *term, message.as_bytes());
    let mut snapshot = kettle_render::PaneSnapshot::default();
    snapshot.capture_with_card_marks(&term, true);
    assert_eq!(
        state.inline_cards.placements(&snapshot).len(),
        1,
        "the registered caption must match the printed completed caption"
    );
}

#[test]
fn regression_waiting_file_facts_expire_while_another_check_is_stalled() {
    let (mut media, mut ws, pane, mut query, _) =
        review_file_facts_fixture(Some(review_file_identity()));
    let (entered, started) = std::sync::mpsc::channel();
    let (release, waiting) = std::sync::mpsc::channel::<()>();
    media.set_facts_lane(
        crate::media::FactsLane::start(
            move |_| {
                entered.send(()).unwrap();
                let _ = waiting.recv();
                Some(review_file_identity())
            },
            Arc::new(|| {}),
        )
        .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    query.control = kettle_media::client::RenderControl::with_deadline(deadline);
    let blocker = crate::media::FactsQuery {
        window: 1,
        pane,
        item: 99,
        generation: 0,
        control: query.control.clone(),
        source: kettle_media::Source::Bytes(Vec::new()),
        why: crate::media::FileWhy::Unsupported,
        reply: None,
    };
    // Both running and queued jobs share the deadline; neither may publish late.
    assert!(media.request_facts(blocker, None, Arc::new(|| {})).is_ok());
    started
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let (reply, received) = crossbeam_channel::bounded(1);
    super::media::prepare_file_reply(
        &mut query,
        crate::media::Origin {
            conn_id: 1,
            request_id: 94,
            reply,
        },
        kettle_ctl::show::ShowResult::file((pane, true, 1), 73, 0, "unsupported_media"),
    );
    assert!(media.request_facts(query, None, Arc::new(|| {})).is_ok());
    let scheduled = media.next_deadline();
    std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
    super::media::media_tick_ready(
        &mut media,
        &mut ws,
        &mut Default::default(),
        #[cfg(target_os = "linux")]
        &mut Default::default(),
    );
    let response = received.try_recv();
    drop(release);
    assert_eq!(
        scheduled,
        Some(deadline),
        "idle UI must wake for waiting facts"
    );
    let response =
        response.expect("queued facts must expire without waiting for the blocked check");
    assert!(!response.ok);
    assert_eq!(
        serde_json::to_value(response).unwrap(),
        serde_json::to_value(kettle_ctl::show::show_failure(
            94,
            kettle_media::FailureCode::RenderTimeout
        ))
        .unwrap()
    );
    assert_eq!(
        ws.mux.panes[&pane].media_shelf.get(73).unwrap().kind,
        crate::media::ItemKind::FailedFile
    );
}

#[test]
fn regression_failed_wsl_translation_does_not_deliver_a_space() {
    let (mut mux, pane, _) = title_test_split_mux();
    mux.panes.get_mut(&pane).unwrap().argv = vec!["wsl.exe".into(), "-d".into(), "Ubuntu".into()];
    let paths = [std::path::PathBuf::from(
        r"\\wsl.localhost\KettleMissingDistribution\etc\hosts",
    )];
    let before = mux.panes[&pane].input_generation();
    let delivery = super::paste_paths_into_target(&mut mux, Some(pane), &paths, true);
    let after = mux.panes[&pane].input_generation();
    mux.kill_children();
    let delivery = delivery.expect("target still exists");
    assert!(!delivery.accepted, "a failed lookup must refuse delivery");
    assert_eq!(delivery.result, PaneInputResult::PathNotTyped);
    assert_eq!(
        super::input_rejection_text(delivery.result, &kettle_i18n::Translator::default()),
        Some(kettle_i18n::Translator::default().text(kettle_i18n::Text::NotifyBodyPathNotTyped))
    );
    assert_eq!(
        before, after,
        "no empty paste or trailing space may reach the pane"
    );
}

#[test]
fn regression_file_pickup_lifetime_starts_at_publication() {
    use kettle_ctl::agent_launch::{PICKUP_LIFETIME, PickupBook, PickupPermit};
    let (_, mut ws, pane, _, _) = review_file_facts_fixture(Some(review_file_identity()));
    let state = ws.mux.panes.get_mut(&pane).unwrap();
    let item = state.media_shelf.get(73).unwrap();
    let image = item.card_image().unwrap();
    let (delivery, pending) = super::media::prepare_card(
        &mut Default::default(),
        (pane, &mut state.inline_cards, (80, 24), 0),
        (8.0, 16.0),
        (73, 0),
        (item.kind, None, None),
        &image,
        crate::media::InlineDraft {
            #[cfg(target_os = "linux")]
            launch: None,
            owner: kettle_ctl::process::ProcessIdentity::new_for_tests(40, 400),
            harness: kettle_render::CardHarness::CodexHook,
            name: None,
            printer: None,
        },
        std::time::Instant::now(),
    )
    .unwrap();
    let admitted = std::time::Instant::now();
    let mut book = PickupBook::default();
    let permit = PickupPermit::new([17; 16]);
    let mut issued_early = false;
    let (_, pending) = super::media::publish_pickup(delivery, pending, || {
        issued_early = true;
        assert!(book.issue(permit, (), admitted));
        Some(permit)
    });
    assert!(
        pending.is_some(),
        "waiting card must retain its launch witness"
    );
    assert!(
        !issued_early,
        "metadata wait must not consume the pickup lifetime"
    );
    let published = admitted + PICKUP_LIFETIME + std::time::Duration::from_secs(1);
    let (delivery, _) = super::media::publish_pickup(
        Some(kettle_ctl::show::InlineDelivery {
            permit: None,
            message: "card".into(),
        }),
        None,
        || {
            assert!(book.issue(permit, (), published));
            Some(permit)
        },
    );
    let key = delivery.unwrap().permit.unwrap();
    assert_eq!(
        book.take(key, published + std::time::Duration::from_secs(1)),
        Some(())
    );
    assert_eq!(
        book.take(key, published + std::time::Duration::from_secs(1)),
        None,
        "one use"
    );
}

