#[test]
fn pending_lua_fifo_preserves_calls_order_and_exact_byte_accounting() {
    let mut pending = PendingLuaCommands::default();
    assert_eq!(
        pending.push(PendingLuaCommand::SendText {
            origin_window: 1,
            target_pane: None,
            bytes: Arc::from(&b"first"[..]),
        }),
        PaneInputResult::Queued
    );
    assert_eq!(
        pending.push(PendingLuaCommand::Notify {
            origin_window: 1,
            title: "notice".into(),
            body: "body".into(),
        }),
        PaneInputResult::Queued
    );
    assert_eq!(
        pending.push(PendingLuaCommand::ExecAction {
            origin_window: 1,
            action: Action::NewTab,
        }),
        PaneInputResult::Queued
    );
    assert_eq!(
        pending.push(PendingLuaCommand::SetTheme {
            origin_window: 1,
            name: "Default".into(),
        }),
        PaneInputResult::Queued
    );
    assert_eq!(
        pending.push(PendingLuaCommand::SendText {
            origin_window: 1,
            target_pane: None,
            bytes: Arc::from(&b"second"[..]),
        }),
        PaneInputResult::Queued
    );
    assert_eq!(pending.len(), 5);
    assert_eq!(pending.send_bytes, 11);

    assert!(matches!(
        pending.pop_front(),
        Some(PendingLuaCommand::SendText { bytes, .. }) if bytes.as_ref() == b"first"
    ));
    assert_eq!(pending.send_bytes, 6);
    assert!(matches!(
        pending.pop_front(),
        Some(PendingLuaCommand::Notify {
            title,
            body,
            ..
        }) if title == "notice" && body == "body"
    ));
    assert!(matches!(
        pending.pop_front(),
        Some(PendingLuaCommand::ExecAction {
            action: Action::NewTab,
            ..
        })
    ));
    assert!(matches!(
        pending.pop_front(),
        Some(PendingLuaCommand::SetTheme { name, .. }) if name == "Default"
    ));
    assert!(matches!(
        pending.pop_front(),
        Some(PendingLuaCommand::SendText { bytes, .. }) if bytes.as_ref() == b"second"
    ));
    assert_eq!(pending.send_bytes, 0);
    assert!(pending.is_empty());
}

#[test]
fn pending_lua_fifo_keeps_large_sends_separate_and_bounded() {
    let mut pending = PendingLuaCommands::default();
    for _ in 0..8 {
        assert_eq!(
            pending.push(PendingLuaCommand::SendText {
                origin_window: 1,
                target_pane: None,
                bytes: Arc::from(vec![b'x'; 1 << 20]),
            }),
            PaneInputResult::Queued
        );
    }
    assert_eq!(pending.len(), 8);
    assert_eq!(pending.send_bytes, 8 << 20);
    assert_eq!(
        pending.push(PendingLuaCommand::SendText {
            origin_window: 1,
            target_pane: None,
            bytes: Arc::from(vec![b'y'; 1]),
        }),
        PaneInputResult::Backpressured
    );
    assert_eq!(
        pending.push(PendingLuaCommand::SendText {
            origin_window: 1,
            target_pane: None,
            bytes: Arc::from(vec![b'z'; (1 << 20) + 1]),
        }),
        PaneInputResult::Oversize
    );
    assert_eq!(pending.len(), 8);
    assert_eq!(pending.send_bytes, 8 << 20);
}

#[test]
fn automation_retry_is_deadline_driven_and_resets_after_progress() {
    let now = std::time::Instant::now();
    let mut retry = AutomationRetry::default();
    assert!(retry.is_ready(now));

    retry.backpressured(now);
    assert!(!retry.is_ready(now));
    assert_eq!(retry.remaining(now), Some(AUTOMATION_RETRY_MIN));
    assert!(retry.is_ready(now + AUTOMATION_RETRY_MIN));

    retry.backpressured(now + AUTOMATION_RETRY_MIN);
    assert_eq!(
        retry.remaining(now + AUTOMATION_RETRY_MIN),
        Some(AUTOMATION_RETRY_MIN * 2)
    );
    retry.progressed();
    assert!(retry.is_ready(now));
    retry.backpressured(now);
    assert_eq!(retry.remaining(now), Some(AUTOMATION_RETRY_MIN));
}

#[test]
fn remote_batch_parser_preserves_operation_order_and_payload_boundaries() {
    let ParsedRemoteCommandBatch::Accepted {
        mut commands,
        unknown_lines,
    } = parse_remote_command_batch("send-text first\\nsecond\r\nnew-tab\ntoggle-window\n", 7)
    else {
        panic!("three operations are below the batch cap");
    };
    assert_eq!(unknown_lines, 0);
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::SendText {
            origin_window: 7,
            target_pane: None,
            bytes,
        }) if bytes.as_ref() == b"first\nsecond"
    ));
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::NewTab { origin_window: 7 })
    ));
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::ToggleWindow { origin_window: 7 })
    ));
    assert!(commands.is_empty());
}

#[test]
fn remote_batch_json_send_text_round_trips_adversarial_text_exactly() {
    let payload = "literal \\n; actual LF follows\nCR follows\rNUL follows\0toggle-window\nnew-tab\n\
             send-text injected";
    let batch = format!(
        "send-text-json {}\nnew-tab\n",
        serde_json::to_string(payload).unwrap()
    );

    let ParsedRemoteCommandBatch::Accepted {
        mut commands,
        unknown_lines,
    } = parse_remote_command_batch(&batch, 11)
    else {
        panic!("two operations are below the batch cap");
    };
    assert_eq!(unknown_lines, 0);
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::SendText {
            origin_window: 11,
            target_pane: None,
            bytes,
        }) if bytes.as_ref() == payload.as_bytes()
    ));
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::NewTab { origin_window: 11 })
    ));
    assert!(
        commands.is_empty(),
        "command-looking payload lines must not become spool operations"
    );
}

#[test]
fn remote_batch_parser_coalesces_malformed_json_as_unknown_lines() {
    let ParsedRemoteCommandBatch::Accepted {
        mut commands,
        unknown_lines,
    } = parse_remote_command_batch(
        "send-text-json \"unterminated\nsend-text-json 7\nnew-tab\n",
        12,
    )
    else {
        panic!("one valid operation is below the batch cap");
    };

    assert_eq!(unknown_lines, 2);
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::NewTab { origin_window: 12 })
    ));
    assert!(commands.is_empty());
}

#[test]
fn remote_batch_parser_accepts_exact_operation_cap_and_rejects_all_on_overflow() {
    let exact = "new-tab\n".repeat(MAX_REMOTE_COMMANDS_PER_BATCH);
    let ParsedRemoteCommandBatch::Accepted {
        commands,
        unknown_lines,
    } = parse_remote_command_batch(&exact, 7)
    else {
        panic!("the exact operation cap must be accepted");
    };
    assert_eq!(commands.len(), MAX_REMOTE_COMMANDS_PER_BATCH);
    assert_eq!(unknown_lines, 0);

    let mut over = String::from("unknown-before\nsend-text must-not-run\n");
    over.push_str(&exact);
    over.push_str("unknown-after\n");
    assert!(matches!(
        parse_remote_command_batch(&over, 7),
        ParsedRemoteCommandBatch::RejectedTooMany {
            operation_count,
            unknown_lines: 2,
        } if operation_count == MAX_REMOTE_COMMANDS_PER_BATCH + 1
    ));
}

#[test]
fn remote_batch_parser_coalesces_unknown_line_count() {
    let ParsedRemoteCommandBatch::Accepted {
        mut commands,
        unknown_lines,
    } = parse_remote_command_batch(
        "unknown-one\n# comment\nnew-tab\nunknown-two\nunknown-three\n",
        9,
    )
    else {
        panic!("one recognized operation is below the batch cap");
    };
    assert_eq!(unknown_lines, 3);
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::NewTab { origin_window: 9 })
    ));
    assert!(commands.is_empty());
}

#[test]
fn delayed_remote_claim_is_owned_by_the_focused_window() {
    assert!(!should_poll_remote_window(false, true, false, 1, 2));
    assert!(should_poll_remote_window(false, true, false, 2, 2));
    assert!(!should_poll_remote_window(false, false, true, 1, 2));
    assert!(should_poll_remote_window(false, false, true, 2, 2));
    // Once the claimed FIFO has an origin, every window gets a chance to
    // reach the matching head regardless of which one is focused now.
    assert!(should_poll_remote_window(true, false, false, 1, 2));
}

#[test]
fn remote_batch_claim_leaves_spool_unchanged_while_sender_lock_is_held() {
    use std::io::{Read as _, Seek as _, Write as _};

    let dir = crate::test_tempdir();
    let path = dir.path().join("remote.cmd");
    let mut file = kettle_state::open_private_file(&path).unwrap();
    file.write_all(b"send-text exact\n").unwrap();
    file.flush().unwrap();
    drop(file);

    let lock_path = kettle_state::remote_command_lock_path(&path);
    let lock = kettle_state::ExclusiveFileLock::acquire(&lock_path).unwrap();
    assert_eq!(
        claim_remote_command_file(&path).unwrap(),
        RemoteBatchClaim::Busy
    );
    let mut preserved = kettle_state::open_existing_private_file(&path).unwrap();
    let mut bytes = Vec::new();
    preserved.seek(std::io::SeekFrom::Start(0)).unwrap();
    preserved.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"send-text exact\n");
    drop(preserved);
    drop(lock);

    assert_eq!(
        claim_remote_command_file(&path).unwrap(),
        RemoteBatchClaim::Claimed("send-text exact\n".to_string())
    );
    assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
}

#[test]
fn malformed_remote_batch_is_cleared_once_instead_of_polled_forever() {
    use std::io::Write as _;

    let dir = crate::test_tempdir();
    let path = dir.path().join("remote.cmd");
    let mut file = kettle_state::open_private_file(&path).unwrap();
    file.write_all(&[0xff, 0xfe]).unwrap();
    file.flush().unwrap();
    drop(file);

    assert_eq!(
        claim_remote_command_file(&path).unwrap(),
        RemoteBatchClaim::RejectedMalformed
    );
    assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
}

#[test]
fn every_lua_event_and_url_handler_dispatch_drains_commands() {
    let source = production_source();
    let raw_fire = [".fire_", "event("].concat();
    assert_eq!(
        source.matches(&raw_fire).count(),
        1,
        "all raw LuaEngine::fire_event calls must stay inside fire_lua_event"
    );
    let open_url = source
        .split("fn open_url(&mut self, ws: &WindowState, uri: &str, source: LinkSource) {")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("open_url helper");
    let handler = open_url
        .find("try_url_handler(uri)")
        .expect("Lua URL handler dispatch");
    let drain = open_url
        .find("self.drain_lua_hook_commands(ws, \"URL handler\")")
        .expect("Lua URL handler drain");
    let handled_return = open_url
        .find("UrlHandlerOutcome::Handled => return,")
        .expect("claimed URL early return");
    assert!(
        handler < drain && drain < handled_return,
        "URL-handler callback commands must drain before a claimed handler returns"
    );
    // A handler may hand back a DIFFERENT URL than the one clicked, and it
    // was fed untrusted terminal text to produce it — so the rewritten URL
    // has to clear the same allowlist the original did, not inherit its
    // approval.
    let rewritten = open_url
        .find("UrlHandlerOutcome::Open(target)")
        .expect("rewritten URL arm");
    let recheck = open_url
        .find("if !kettle_core::links::is_safe_url(target)")
        .expect("rewritten URL must be re-checked against the allowlist");
    assert!(
        rewritten < recheck,
        "a Lua-supplied URL must be allowlist-checked before it is opened"
    );
}

#[test]
fn automatic_and_explicit_lua_paths_keep_their_trust_provenance() {
    let root = kettle_test_support::private_tempdir("kettle-init-lua-source-");
    let default_config = root.join("config");
    let automatic = root.join("init.lua");
    std::fs::write(&automatic, "return true\n").unwrap();

    assert_eq!(
        super::resolved_init_lua_script(None, Some(default_config.clone())),
        Some((
            automatic.clone(),
            kettle_config::ConfigTrust::VerifyDirectory
        ))
    );

    let explicit = root.join("project.lua");
    assert_eq!(
        super::resolved_init_lua_script(Some(explicit.clone()), Some(default_config),),
        Some((explicit, kettle_config::ConfigTrust::ExplicitPath)),
        "an explicit path wins even when the automatic script exists"
    );
}

#[test]
fn trigger_command_slots_are_bounded_and_released() {
    use super::TriggerCommandSlot;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    let active = AtomicUsize::new(0);
    let mut slots: Vec<_> = (0..32)
        .map(|_| TriggerCommandSlot::acquire(&active).unwrap())
        .collect();
    assert!(TriggerCommandSlot::acquire(&active).is_none());
    drop(slots.pop());
    assert!(TriggerCommandSlot::acquire(&active).is_some());
    drop(slots);
    assert_eq!(active.load(Relaxed), 0);
}

#[cfg(unix)]
#[test]
fn trigger_command_reaps_exited_child() {
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pid");
    super::spawn_trigger_command(&[
        "/bin/sh".into(),
        "-c".into(),
        "printf '%s' \"$$\" > \"$1\"".into(),
        "kettle-trigger-test".into(),
        path.to_str().unwrap().into(),
    ]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid: libc::pid_t = loop {
        if let Some(pid) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.parse().ok())
        {
            break pid;
        }
        assert!(Instant::now() < deadline, "trigger did not publish its pid");
        std::thread::sleep(Duration::from_millis(10));
    };
    let reaped = loop {
        // SAFETY: Signal zero probes the child PID without delivering a signal or accessing
        // pointers.
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !reaped {
        // SAFETY: pid names the test child; a null status pointer is allowed and WNOHANG avoids
        // blocking.
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    }
    assert!(reaped, "the completed trigger child was left unreaped");
}

#[test]
fn match_triggers_finds_pattern_anywhere_in_text() {
    // The matching engine fires on the first regex hit, returns its
    // action, and no-ops when nothing matches. Patterns are unanchored.
    use super::{compile_triggers, match_triggers};
    use kettle_config::{OutputTrigger, TriggerAction};
    let cfg = vec![
        OutputTrigger {
            pattern: r"error.*panic".into(),
            action: TriggerAction::Urgency,
        },
        OutputTrigger {
            pattern: r"(BUILD SUCCESSFUL|FAILED)".into(),
            action: TriggerAction::Urgency,
        },
    ];
    let compiled = compile_triggers(&cfg);
    assert_eq!(compiled.len(), 2);

    // Bare match anywhere in the snapshot.
    assert!(match_triggers("thread 'main' panicked: error panic", &compiled).is_some());
    // Alternation pattern still matches both branches.
    assert!(match_triggers("BUILD SUCCESSFUL in 1m23s", &compiled).is_some());
    assert!(match_triggers("BUILD FAILED with 3 errors", &compiled).is_some());
    // Non-matching text: no fire.
    assert!(match_triggers("just normal output, nothing here", &compiled).is_none());
    assert!(match_triggers("", &compiled).is_none());

    // Empty trigger set never fires.
    assert!(match_triggers("error.*panic any text", &[]).is_none());

    // Invalid regex is dropped at compile time (warn-logged); the
    // remaining valid ones still work.
    let cfg_mixed = vec![
        OutputTrigger {
            pattern: r"valid_pattern".into(),
            action: TriggerAction::Urgency,
        },
        OutputTrigger {
            pattern: r"unclosed[".into(), // invalid regex
            action: TriggerAction::Urgency,
        },
    ];
    let compiled_mixed = compile_triggers(&cfg_mixed);
    assert_eq!(
        compiled_mixed.len(),
        1,
        "invalid regex should be dropped at compile time"
    );
    assert!(match_triggers("here is the valid_pattern token", &compiled_mixed).is_some());
}

/// Viewport snapshots join rows with '\n', so `^` and `$` must anchor to a
/// row, not to the whole snapshot.
#[test]
fn trigger_anchors_match_each_row() {
    use super::{compile_triggers, match_triggers};
    use kettle_config::{OutputTrigger, TriggerAction};
    let compiled = compile_triggers(&[OutputTrigger {
        pattern: r"^Build failed: (.+)$".into(),
        action: TriggerAction::Urgency,
    }]);
    let snapshot = "$ make\nBuild failed: missing header\n$ \n";
    assert!(match_triggers(snapshot, &compiled).is_some());
    assert!(match_triggers("$ make\nok: Build failed: no\n", &compiled).is_none());
}

/// Terminator `run_cmd_on_match.py` parity: a trigger's capture groups
/// substitute into the spawned argv. `{0}` is the whole match and `{1}`…
/// are numbered groups. An out-of-range reference stays LITERAL so the
/// typo stays visible. Substitution can change a VALUE but never add
/// arguments.
#[test]
fn trigger_capture_groups_substitute_into_argv() {
    use super::{compile_triggers, match_triggers};
    use kettle_config::{OutputTrigger, TriggerAction};
    let cfg = vec![OutputTrigger {
        pattern: r"ERROR in ([\w./-]+) line (\d+)".into(),
        action: TriggerAction::RunCommand(vec![
            "notify-send".into(),
            "build error".into(),
            "{1}:{2}".into(),
            "match={0}".into(),
            "{9}".into(), // out of range → stays literal
        ]),
    }];
    let compiled = compile_triggers(&cfg);
    let action =
        match_triggers("xx ERROR in src/main.rs line 42 yy", &compiled).expect("trigger fires");
    let TriggerAction::RunCommand(argv) = action else {
        panic!("expected RunCommand");
    };
    assert_eq!(
        argv,
        vec![
            "notify-send".to_string(),
            "build error".to_string(),
            "src/main.rs:42".to_string(),
            "match=ERROR in src/main.rs line 42".to_string(),
            "{9}".to_string(),
        ]
    );
    // Argv arity is unchanged — a match can never ADD arguments.
    assert_eq!(argv.len(), 5);
    // A capture whose MATCHED TEXT contains a placeholder must not expand
    // again (single template pass, no re-scan of substituted output).
    let nested = vec![OutputTrigger {
        pattern: r"E:(\S+):(\S+)".into(),
        action: TriggerAction::RunCommand(vec!["log".into(), "{1}".into()]),
    }];
    let action =
        match_triggers("E:{2}:secret", &compile_triggers(&nested)).expect("nested trigger fires");
    let TriggerAction::RunCommand(argv) = action else {
        panic!("expected RunCommand");
    };
    assert_eq!(
        argv[1], "{2}",
        "substituted text must be emitted verbatim, never re-expanded"
    );
    // Urgency triggers are untouched by capture plumbing.
    let urgency = vec![OutputTrigger {
        pattern: r"(panic)".into(),
        action: TriggerAction::Urgency,
    }];
    assert!(matches!(
        match_triggers("a panic b", &compile_triggers(&urgency)),
        Some(TriggerAction::Urgency)
    ));
}

#[test]
fn finding_trigger_snapshots_reconstruct_graphemes() {
    let source = include_str!("automation.rs");
    let body = finding_body(source, "trigger_snapshot");
    assert!(
        body.contains("append_row_text"),
        "trigger snapshots must skip wide spacers and retain combining marks"
    );
}

#[test]
fn trigger_grid_snapshot_preserves_wide_and_combining_text() {
    let (mux, panes) = title_test_tab_mux(1);
    let pane = &mux.panes[&panes[0]];
    let text = "\u{4e16}\u{754c} e\u{301}";
    assert!(pane.term.apply_local_output(text.as_bytes()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let snapshot = loop {
        let snapshot = super::automation::trigger_snapshot(&pane.term).unwrap();
        if snapshot.contains(text) {
            break snapshot;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "snapshot lost Unicode: {snapshot:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let triggers = super::compile_triggers(&[kettle_config::OutputTrigger {
        pattern: format!("({text})$"),
        action: kettle_config::TriggerAction::RunCommand(vec!["notify".into(), "{1}".into()]),
    }]);
    assert_eq!(
        super::match_triggers(&snapshot, &triggers),
        Some(kettle_config::TriggerAction::RunCommand(vec![
            "notify".into(),
            text.into()
        ]))
    );
    mux.kill_children();
}

