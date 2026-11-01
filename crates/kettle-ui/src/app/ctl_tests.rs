#[test]
fn pane_snapshot_reuse_fails_closed_on_output_layout_or_order_changes() {
    let cached = [(1, 10, 120, 40), (2, 20, 80, 24)];
    assert!(pane_snapshot_keys_match(&cached, cached));
    assert!(!pane_snapshot_keys_match(
        &cached,
        [(1, 11, 120, 40), (2, 20, 80, 24)]
    ));
    assert!(!pane_snapshot_keys_match(
        &cached,
        [(1, 10, 121, 40), (2, 20, 80, 24)]
    ));
    assert!(!pane_snapshot_keys_match(
        &cached,
        [(2, 20, 80, 24), (1, 10, 120, 40)]
    ));
    assert!(!pane_snapshot_keys_match(&cached, [(1, 10, 120, 40)]));
}

/// `ui_geometry` reads draw no frame, and a connection that attached no
/// pane draws none when it closes, so `kettle ctl ui_geometry` can watch a
/// window-server blink without ending it.
#[test]
fn ui_geometry_reads_draw_no_frame() {
    use kettle_ctl::protocol::Method;
    for method in Method::ALL {
        assert_eq!(
            super::ctl_method_is_pure_read(method),
            matches!(
                method,
                Method::UiGeometry | Method::AgentLaunch | Method::CardPickup
            ),
            "{method:?}"
        );
    }
    let src = super::production_source();
    let drain = src
        .split_once("fn drain_ctl(&mut self, ws: &mut WindowState, event_loop: &ActiveEventLoop) {")
        .expect("drain_ctl")
        .1
        .split_once("\n    fn handle_ctl_request(")
        .expect("end of drain_ctl")
        .0;
    assert!(
        drain.contains(
            "if !request.internal_probe() && !ctl_method_is_pure_read(request.method()) {"
        )
    );
    assert!(drain.contains("needs_redraw |= !panes.is_empty();"));
    assert!(!drain.contains("the agent badge may have cleared"));
}

/// `get_state`'s caller is verified in the nearest live pane whose child
/// is one of its ancestors. Hints and failures never verify.
#[test]
fn caller_report_names_the_nearest_live_pane() {
    use crate::ctl_server::{CallerEvidence, PaneHint};
    use kettle_ctl::identity::UnverifiedReason;
    use kettle_ctl::process::ProcessIdentity;
    let id = |pid, start| ProcessIdentity::new_for_tests(pid, start);
    // caller 30 → tool 31 → inner pane's shell 20 → outer pane's shell 10.
    let chain = vec![id(30, 300), id(31, 290), id(20, 200), id(10, 100)];
    let root = |root, pane, window| crate::media::PaneRoot { root, pane, window };
    let roots = [root(id(10, 100), 1, 1), root(id(20, 200), 7, 2)];
    let panes = [(1, 1), (7, 2), (9, 2)];
    let report = |caller| super::ctl_caller_report(&caller, &roots, &panes, 4242);

    let verified = report(CallerEvidence::for_tests(Ok(chain.clone()), None));
    assert_eq!(verified["verified"], true);
    assert_eq!(verified["pane"], 7, "the nearest pane wins");
    assert_eq!(verified["window"], 2);
    assert_eq!(verified["reason"], serde_json::Value::Null);
    assert_eq!(verified["peer"]["pid"], 30);
    assert_eq!(verified["peer"]["start_token"], "300");

    // A root whose start differs is another process that reused the pid.
    let reused = super::ctl_caller_report(
        &CallerEvidence::for_tests(Ok(vec![id(30, 300), id(20, 201)]), None),
        &roots,
        &panes,
        4242,
    );
    assert_eq!(reused["verified"], false);
    assert_eq!(reused["reason"], "no_pane");
    assert_eq!(reused["pane"], serde_json::Value::Null);

    let failed = report(CallerEvidence::for_tests(
        Err(UnverifiedReason::ClaimMismatch),
        Some(PaneHint {
            pane: 9,
            kettle_pid: 4242,
        }),
    ));
    assert_eq!(failed["verified"], false);
    assert_eq!(failed["reason"], "claim_mismatch");
    assert_eq!(failed["peer"], serde_json::Value::Null);
    assert_eq!(
        failed["hint"]["pane"], 9,
        "a hint for this Kettle is reported"
    );
    assert_eq!(failed["hint"]["window"], 2);

    // A hint naming another Kettle, or a pane this one does not have, is
    // dropped; a hint never verifies.
    for hint in [
        PaneHint {
            pane: 9,
            kettle_pid: 1,
        },
        PaneHint {
            pane: 99,
            kettle_pid: 4242,
        },
    ] {
        let dropped = report(CallerEvidence::for_tests(
            Err(UnverifiedReason::MissingClaim),
            Some(hint),
        ));
        assert_eq!(dropped["hint"], serde_json::Value::Null, "{hint:?}");
    }
    let unchecked = report(CallerEvidence::default());
    assert_eq!(unchecked["verified"], false);
    assert_eq!(unchecked["reason"], "missing_claim");
}

#[test]
fn control_pages_are_bounded_and_snapshot_consistent() {
    let values: Vec<serde_json::Value> = (0..5).map(serde_json::Value::from).collect();
    let first = kettle_ctl::protocol::Request {
        v: 1,
        id: 1,
        method: "list_panes".into(),
        params: serde_json::json!({"limit": 2}),
        caller: None,
    };
    let response = super::ctl_page_values(&first, "panes", values.clone());
    assert!(response.ok);
    assert_eq!(response.result["panes"].as_array().unwrap().len(), 2);
    assert_eq!(response.result["next_cursor"], "2");
    assert_eq!(response.result["truncated"], true);

    let second = kettle_ctl::protocol::Request {
        id: 2,
        params: serde_json::json!({
            "cursor": response.result["next_cursor"],
            "snapshot": response.result["snapshot"],
            "limit": 2,
        }),
        ..first
    };
    let response = super::ctl_page_values(&second, "panes", values);
    assert_eq!(response.result["panes"], serde_json::json!([2, 3]));

    let stale = kettle_ctl::protocol::Request {
        id: 3,
        params: serde_json::json!({"cursor": "2", "snapshot": "stale"}),
        ..second
    };
    let response = super::ctl_page_values(&stale, "panes", vec![serde_json::json!(1)]);
    assert_eq!(
        response.error.unwrap().code,
        kettle_ctl::protocol::error_codes::STALE_SNAPSHOT
    );
}

#[test]
fn control_inventory_limit_stops_enumeration_at_the_first_overage() {
    let pulled = std::cell::Cell::new(0usize);
    let counts = std::iter::from_fn(|| {
        pulled.set(pulled.get() + 1);
        Some(super::MAX_CTL_COLLECTION_ITEMS + 1)
    });
    assert_eq!(
        super::ctl_bounded_sum(counts, super::MAX_CTL_COLLECTION_ITEMS),
        None
    );
    assert_eq!(
        pulled.get(),
        1,
        "the collection cap must stop the producer, not just reject after collection"
    );
    assert_eq!(super::ctl_bounded_sum([2, 3, 5], 10), Some(10));
}

/// `send_mouse`'s `back` and `forward` report what the physical buttons
/// do, so a mouse-tracking program cannot tell them apart; xterm's button
/// numbers 8 and 9 would read as a modified left or middle press.
#[test]
fn ctl_side_buttons_report_like_the_physical_ones() {
    use super::{ctl_mouse_button, extra_mouse_sgr};
    use winit::event::MouseButton;
    let button = |name: &str| ctl_mouse_button(&serde_json::json!({ "button": name }));
    assert_eq!(button("back").ok(), extra_mouse_sgr(MouseButton::Back));
    assert_eq!(
        button("forward").ok(),
        extra_mouse_sgr(MouseButton::Forward)
    );
    assert_eq!(button("left"), Ok(0));
    let sgr = |code| crate::input::mouse_encode(true, code, true, false, 0, 0, Default::default());
    assert_eq!(sgr(button("back").unwrap()), b"\x1b[<128;1;1M");
}

#[test]
fn run_command_ignores_queued_completion_from_before_submission() {
    use super::{PendingRun, take_pending_run};
    let (reply, replies) = crossbeam_channel::bounded(1);
    let run = PendingRun {
        conn_id: 1,
        req_id: 2,
        start_line: 0,
        command_generation: 7,
        token: None,
        deadline: std::time::Instant::now(),
        reply,
    };
    let mut runs = std::collections::HashMap::from([(3, run)]);
    let mut event = kettle_core::CommandFinished {
        generation: 7,
        duration: std::time::Duration::from_secs(5),
        exit_code: Some(9),
    };
    assert!(take_pending_run(&mut runs, 3, &event).is_none());
    assert!(runs.contains_key(&3));
    assert!(replies.try_recv().is_err());
    event.generation = 8;
    let run = take_pending_run(&mut runs, 3, &event).unwrap();
    assert_eq!(run.req_id, 2);
    assert!(runs.is_empty());
}

#[test]
fn a_correlated_run_ignores_legacy_completion_even_at_the_expected_generation() {
    use super::{PendingRun, take_pending_run};
    let (reply, _replies) = crossbeam_channel::bounded(1);
    let token = kettle_core::CommandToken::parse(b"0123456789abcdef0123456789abcdef").unwrap();
    let run = PendingRun {
        conn_id: 1,
        req_id: 2,
        start_line: 0,
        command_generation: 7,
        token: Some(token),
        deadline: std::time::Instant::now(),
        reply,
    };
    let mut runs = std::collections::HashMap::from([(3, run)]);
    let event = kettle_core::CommandFinished {
        generation: 8,
        duration: std::time::Duration::from_secs(5),
        exit_code: Some(9),
    };
    assert!(take_pending_run(&mut runs, 3, &event).is_none());
    assert!(runs.contains_key(&3));
}

#[test]
fn correlated_completion_matches_only_its_token_and_uses_its_begin_anchor() {
    use super::{PendingRun, take_pending_correlated_run};
    let (reply, _replies) = crossbeam_channel::bounded(1);
    let token = kettle_core::CommandToken::parse(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    let stale = kettle_core::CommandToken::parse(b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
    let run = PendingRun {
        conn_id: 1,
        req_id: 2,
        start_line: 10,
        command_generation: 7,
        token: Some(token),
        deadline: std::time::Instant::now(),
        reply,
    };
    let mut runs = std::collections::HashMap::from([(3, run)]);
    let mut event = kettle_core::CorrelatedCommandFinished {
        token: stale,
        start_line: 42,
        command: kettle_core::CommandFinished {
            generation: 99,
            duration: std::time::Duration::from_secs(5),
            exit_code: Some(9),
        },
    };
    assert!(take_pending_correlated_run(&mut runs, 3, &event).is_none());
    assert!(runs.contains_key(&3));
    event.token = token;
    let run = take_pending_correlated_run(&mut runs, 3, &event).unwrap();
    assert_eq!(run.req_id, 2);
    assert_eq!(run.start_line, 42);
    assert!(take_pending_correlated_run(&mut runs, 3, &event).is_none());
}

#[test]
fn run_command_line_cap_reports_a_dropped_head() {
    use super::{MAX_CTL_SCROLLBACK_LINES, ctl_capture_bounds};

    let history_size = MAX_CTL_SCROLLBACK_LINES + 5_000;
    let (start, end, head_dropped) = ctl_capture_bounds(
        0,
        history_size,
        0,
        history_size as u64,
        MAX_CTL_SCROLLBACK_LINES + 24,
    );
    assert_eq!(start, 0);
    assert!(end > start);
    assert!(
        head_dropped,
        "a command that outruns the retained-line capture must mark its output incomplete"
    );

    let (start, end, head_dropped) = ctl_capture_bounds(0, 100, 50, 100, MAX_CTL_SCROLLBACK_LINES);
    assert!(end > start);
    assert!(!head_dropped, "a fully retained capture is complete");
}

#[test]
fn run_command_capture_survives_full_scrollback() {
    use super::ctl_capture_bounds;

    // Two retained history rows and two screen rows; the command began at
    // document row 3, then printed through row 5 while history stayed full.
    let lines = ["command", "first", "second"];
    let (start, end, truncated) = ctl_capture_bounds(2, 2, 3, 5, lines.len());
    assert_eq!(lines[start..end].join("\n"), "command\nfirst\nsecond");
    assert!(!truncated);

    // More output evicts the command's first rows.
    let lines = ["second", "third", "fourth", "prompt"];
    let (start, end, truncated) = ctl_capture_bounds(5, 2, 3, 8, lines.len());
    assert_eq!(
        lines[start..end].join("\n"),
        "second\nthird\nfourth\nprompt"
    );
    assert!(truncated);
}

#[test]
fn finding_settings_accepts_control_space() {
    let (_, key) = super::parse_ui_key("space").unwrap();
    assert!(super::confirmation_space_key(&key));
    let body = finding_body(include_str!("modals.rs"), "settings_key");
    assert!(
        body.contains("confirmation_space_key(key)"),
        "Settings must activate character-form Space from dispatch_ui_key"
    );
}

#[test]
fn control_pane_drag_arms_moves_and_drops_a_pane() {
    let (mux, moving, target) = title_test_split_mux();
    let mut ws = WindowState::new(1, mux);
    let area = (0.0, 0.0, 800.0, 600.0);
    let before = ws.mux.leaves(0, area);
    let source = before.iter().find(|(id, _)| *id == moving).unwrap().1;
    let destination = before.iter().find(|(id, _)| *id == target).unwrap().1;
    ws.cursor =
        winit::dpi::PhysicalPosition::new((source.0 + 10.0) as f64, (source.1 + 10.0) as f64);
    super::arm_pane_drag(&mut ws, moving);
    assert!(ws.pane_drag.is_some());
    ws.cursor.x += 1.0;
    super::update_pane_drag(&mut ws, area);
    assert!(
        !ws.pane_drag.as_ref().unwrap().live,
        "jitter must remain a click"
    );
    ws.cursor = winit::dpi::PhysicalPosition::new(
        (destination.0 + destination.2 / 2.0) as f64,
        (destination.1 + destination.3 - 2.0) as f64,
    );
    super::update_pane_drag(&mut ws, area);
    assert!(ws.pane_drag.as_ref().unwrap().live);
    assert_eq!(
        ws.pane_drag.as_ref().unwrap().target.map(|target| target.0),
        Some(target)
    );
    assert_eq!(
        super::finish_pane_drag(&mut ws),
        Some((true, false)),
        "control release must commit the pane move"
    );
    assert!(ws.pane_drag.is_none());
    assert_ne!(ws.mux.leaves(0, area), before);
    ws.mux.kill_children();
    let src = production_source();
    for (method, call) in [
        ("fn ctl_mouse_press(", "self.press_pane_drag(ws, bcode)"),
        ("fn ctl_mouse_move(", "self.drag_pane(ws)"),
        (
            "fn ctl_mouse_release(",
            "self.release_pane_drag(ws, bcode, event_loop)",
        ),
    ] {
        let body = src
            .split(method)
            .nth(1)
            .unwrap()
            .split("\n    }\n")
            .next()
            .unwrap();
        assert!(
            body.contains(call),
            "control input must use shared pane gesture handling: {call}"
        );
    }
}

#[test]
fn command_correlation_frames_original_global_shell_input() {
    use super::correlated_command_submission;
    use kettle_core::CommandShell::{Bash, Fish, PowerShell, Zsh};
    let token = kettle_core::CommandToken::parse(b"0123456789abcdef0123456789abcdef").unwrap();
    for shell in [Bash, Zsh, Fish, PowerShell] {
        let original = "value='quoted text'\nprintf '%s' \"$value\"\n";
        let input = correlated_command_submission(original, shell, token, true).unwrap();
        assert_eq!(input.last(), Some(&b'\r'));
        let unbracketed = correlated_command_submission(original, shell, token, false);
        if shell == Bash {
            assert!(
                input.ends_with(b"\x1b[200~value='quoted text'\nprintf '%s' \"$value\"\x1b[201~\r")
            );
            assert!(unbracketed.is_none());
        } else {
            assert_eq!(unbracketed.unwrap(), input);
            let expected: &[u8] = if shell == PowerShell {
                b"value='quoted text'\x18iprintf '%s' \"$value\"\r"
            } else {
                b"value='quoted text'\x18jprintf '%s' \"$value\"\r"
            };
            assert!(input.ends_with(expected));
        }
        for control in ["literal\x1b[201~", "first\rsecond", "interrupt\x03"] {
            assert!(correlated_command_submission(control, shell, token, true).is_none());
        }
        let simple = correlated_command_submission("false\r\n", shell, token, false).unwrap();
        assert!(simple.ends_with(b"false\r"));
    }
}

#[test]
fn a_single_line_correlated_command_keeps_the_normal_editor_input_path() {
    use super::correlated_command_submission;
    use kettle_core::CommandShell::{Bash, Fish, PowerShell, Zsh};
    let token = kettle_core::CommandToken::parse(b"0123456789abcdef0123456789abcdef").unwrap();
    for shell in [Bash, Zsh, Fish, PowerShell] {
        let enabled =
            correlated_command_submission("printf 'TARGET\\n'; false", shell, token, true).unwrap();
        let disabled =
            correlated_command_submission("printf 'TARGET\\n'; false", shell, token, false)
                .unwrap();
        assert_eq!(enabled, disabled);
        assert!(!enabled.windows(6).any(|part| part == b"\x1b[200~"));
    }
}
