use super::{
    COMPLETION_HIDE_GRACE, CompletionSlot, MAX_COMPLETION_REQUEST, apply_completion_prompt_start,
    apply_completion_reader_prompt_start, apply_completion_update, arm_completion_refresh_slot,
    clear_completion_slot, completion_input_effect, completion_visible, hide_completion_view_slot,
    invalidate_completion_slot, poll_completion_hide_slot, prepare_optional_completion_resync,
    reset_completion_session, with_completion_input_admission_slot,
    with_completion_input_admission_slot_at,
};
use kettle_vt::{CompletionKind, CompletionList, CompletionUpdate};
use std::sync::Mutex;
use std::time::Instant;

fn list(generation: u64) -> CompletionList {
    CompletionList {
        session: None,
        generation,
        request: None,
        kind: CompletionKind::Completion,
        selected: None,
        total: 1,
        source: "test".to_string(),
        token: None,
        input_prefix: None,
        candidates: vec![kettle_vt::CompletionCandidate {
            label: "candidate".to_string(),
            description: String::new(),
            position: 0,
        }],
    }
}

fn sequenced_list(session: u64, generation: u64, request: u64) -> CompletionList {
    CompletionList {
        session: Some(session),
        request: Some(request),
        ..list(generation)
    }
}

#[test]
fn legacy_alt_tab_does_not_consume_a_completion_request() {
    let cell = Mutex::new(CompletionSlot::default());
    reset_completion_session(&cell, 0);
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 7,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );
    with_completion_input_admission_slot(
        &mut cell.lock().unwrap(),
        &[b"\x1b\t"],
        Instant::now(),
        || ((), true),
    );
    with_completion_input_admission_slot(
        &mut cell.lock().unwrap(),
        &[b"\t"],
        Instant::now(),
        || ((), true),
    );
    apply_completion_update(&cell, CompletionUpdate::Show(sequenced_list(7, 1, 1)));
    assert!(
        cell.lock().unwrap().list.is_some(),
        "plain Tab still accepts the shell's first completion reply"
    );
}

#[test]
fn a_new_prompt_accepts_a_nested_shells_fresh_generation() {
    let slot = Mutex::new(CompletionSlot::default());
    arm_completion_refresh_slot(&mut slot.lock().unwrap(), Instant::now());
    apply_completion_update(&slot, CompletionUpdate::Show(list(42)));
    reset_completion_session(&slot, 0);
    arm_completion_refresh_slot(&mut slot.lock().unwrap(), Instant::now());
    apply_completion_update(&slot, CompletionUpdate::Show(list(1)));

    let current = slot.lock().unwrap();
    assert_eq!(current.generation, 1);
    assert_eq!(current.list.as_ref().map(|list| list.generation), Some(1));
}

#[test]
fn a_duplicate_prompt_mark_preserves_sync_but_a_reused_row_after_output_does_not() {
    let cell = Mutex::new(CompletionSlot::default());
    reset_completion_session(&cell, 0);
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 7,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );

    apply_completion_prompt_start(&cell, 1, false);
    assert_eq!(
        cell.lock().unwrap().session,
        Some(7),
        "Fish's duplicate same-row prompt mark must not erase the sync it just sent"
    );

    cell.lock().unwrap().prompt_boundary_pending = true;
    apply_completion_prompt_start(&cell, 2, false);
    let slot = cell.lock().unwrap();
    assert_eq!(slot.session, None);
    assert!(
        slot.sync_expected,
        "a command can clear the screen and reuse the previous prompt row; its next sync must still be accepted"
    );
    assert!(!slot.prompt_boundary_pending);
}

#[test]
fn reader_prompt_glue_deduplicates_the_ring_before_resetting_completion() {
    let completion = Mutex::new(CompletionSlot::default());
    let prompts = Mutex::new(std::collections::VecDeque::new());

    apply_completion_reader_prompt_start(&completion, &prompts, Some((10, 0)), 1);
    apply_completion_update(
        &completion,
        CompletionUpdate::Sync {
            session: 7,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );
    apply_completion_reader_prompt_start(&completion, &prompts, Some((10, 0)), 2);
    assert_eq!(completion.lock().unwrap().session, Some(7));
    assert_eq!(
        prompts.lock().unwrap().iter().copied().collect::<Vec<_>>(),
        vec![10]
    );

    completion.lock().unwrap().prompt_boundary_pending = true;
    apply_completion_reader_prompt_start(&completion, &prompts, Some((10, 0)), 3);
    assert!(completion.lock().unwrap().sync_expected);

    apply_completion_update(
        &completion,
        CompletionUpdate::Sync {
            session: 8,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );
    apply_completion_reader_prompt_start(&completion, &prompts, None, 4);
    let slot = completion.lock().unwrap();
    assert_eq!(
        slot.session, None,
        "alternate-screen or lock failure must reset"
    );
    assert!(slot.sync_expected);
}

#[test]
fn a_moved_prompt_without_a_command_preserves_or_replaces_its_session() {
    let completion = Mutex::new(CompletionSlot::default());
    let prompts = Mutex::new(std::collections::VecDeque::new());
    apply_completion_reader_prompt_start(&completion, &prompts, Some((10, 0)), 1);
    apply_completion_update(
        &completion,
        CompletionUpdate::Sync {
            session: 7,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );

    // Fish Ctrl-L moves the mark but emits no new sync. Its current
    // session must remain usable for the next request.
    apply_completion_reader_prompt_start(&completion, &prompts, Some((11, 0)), 2);
    assert_eq!(completion.lock().unwrap().session, Some(7));
    with_completion_input_admission_slot_at(
        &mut completion.lock().unwrap(),
        &[b"\t"],
        Instant::now(),
        Some(20),
        || ((), true),
    );
    apply_completion_update(&completion, CompletionUpdate::Show(sequenced_list(7, 1, 1)));
    assert!(completion.lock().unwrap().list.is_some());

    // PowerShell Ctrl-C can use the same window to announce a fresh
    // managed session before its next Tab.
    apply_completion_reader_prompt_start(&completion, &prompts, Some((12, 0)), 3);
    apply_completion_update(
        &completion,
        CompletionUpdate::Sync {
            session: 8,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );
    let slot = completion.lock().unwrap();
    assert_eq!(slot.session, Some(8));
    assert!(!slot.sync_expected);
}

#[test]
fn a_clear_keeps_older_updates_from_resurrecting() {
    let slot = Mutex::new(CompletionSlot::default());
    arm_completion_refresh_slot(&mut slot.lock().unwrap(), Instant::now());
    apply_completion_update(&slot, CompletionUpdate::Show(list(7)));
    apply_completion_update(
        &slot,
        CompletionUpdate::Clear {
            session: None,
            generation: 8,
            request: None,
        },
    );
    apply_completion_update(&slot, CompletionUpdate::Update(list(7)));

    let slot = slot.lock().unwrap();
    assert_eq!(slot.generation, 8);
    assert!(slot.list.is_none());
}

#[test]
fn a_pending_grace_window_keeps_the_card_up_until_it_lapses() {
    let now = Instant::now();
    let mut slot = CompletionSlot {
        generation: 3,
        list: Some(list(3)),
        accept_updates: true,
        hide_after: Some(now + COMPLETION_HIDE_GRACE),
        ..CompletionSlot::default()
    };
    assert!(completion_visible(&slot, now).is_some());
    assert!(
        completion_visible(&slot, now + COMPLETION_HIDE_GRACE).is_none(),
        "a Tab the shell never answered must stop showing stale candidates"
    );

    // The shell answering inside the window cancels the hide entirely.
    slot.hide_after = Some(now + COMPLETION_HIDE_GRACE);
    let cell = Mutex::new(slot);
    apply_completion_update(&cell, CompletionUpdate::Update(list(4)));
    let slot = cell.lock().unwrap();
    assert!(slot.hide_after.is_none());
    assert!(completion_visible(&slot, now + COMPLETION_HIDE_GRACE * 10).is_some());
}

#[test]
fn unrelated_input_rejects_a_response_already_in_flight() {
    let cell = Mutex::new(CompletionSlot::default());
    arm_completion_refresh_slot(&mut cell.lock().unwrap(), Instant::now());
    apply_completion_update(&cell, CompletionUpdate::Show(list(5)));
    clear_completion_slot(&mut cell.lock().unwrap());

    // The shell generated this after the preceding Tab, but it arrived
    // after the user had already typed a different character.
    apply_completion_update(&cell, CompletionUpdate::Update(list(6)));
    let slot = cell.lock().unwrap();
    assert_eq!(slot.generation, 5);
    assert!(slot.list.is_none());
    assert!(!slot.accept_updates);
}

#[test]
fn a_request_from_before_focus_loss_cannot_reopen_after_rearming() {
    let cell = Mutex::new(CompletionSlot::default());
    reset_completion_session(&cell, 0);
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 7,
            keys: kettle_vt::completion::KEY_TAB | kettle_vt::completion::KEY_BACKTAB,
        },
    );

    arm_completion_refresh_slot(&mut cell.lock().unwrap(), Instant::now());
    let first = sequenced_list(7, 1, 1);
    apply_completion_update(&cell, CompletionUpdate::Show(first.clone()));
    invalidate_completion_slot(&mut cell.lock().unwrap(), 0);

    arm_completion_refresh_slot(&mut cell.lock().unwrap(), Instant::now());
    apply_completion_update(&cell, CompletionUpdate::Update(first));
    assert!(
        cell.lock().unwrap().list.is_none(),
        "request 1 was buffered before focus loss and must not satisfy request 2"
    );

    let second = sequenced_list(7, 2, 2);
    apply_completion_update(&cell, CompletionUpdate::Update(second));
    let current = cell.lock().unwrap();
    assert_eq!(current.list.as_ref().and_then(|list| list.request), Some(2));
}

#[test]
fn the_first_managed_sync_may_finish_after_focus_loss_and_the_next_tab() {
    let cell = Mutex::new(CompletionSlot {
        startup_sync_pending: true,
        ..CompletionSlot::default()
    });
    // Live startup can lose focus before the parser reaches OSC 133 A.
    // The reader has admitted that prompt/sync chunk, so the prompt reset
    // reaches completion state only after the focus boundary.
    invalidate_completion_slot(&mut cell.lock().unwrap(), 1);
    reset_completion_session(&cell, 1);

    with_completion_input_admission_slot(
        &mut cell.lock().unwrap(),
        &[b"\t"],
        Instant::now(),
        || ((), true),
    );
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 71,
            keys: kettle_vt::completion::KEY_TAB | kettle_vt::completion::KEY_BACKTAB,
        },
    );
    apply_completion_update(&cell, CompletionUpdate::Show(sequenced_list(71, 1, 1)));

    let slot = cell.lock().unwrap();
    assert_eq!(slot.session, Some(71));
    assert_eq!(slot.next_request, 1);
    assert_eq!(slot.expected_request, Some(1));
    assert_eq!(slot.list.as_ref().and_then(|list| list.request), Some(1));
    assert!(!slot.startup_sync_pending);
}

#[test]
fn the_first_reader_prompt_preserves_startup_sync_after_focus_loss() {
    let completion = Mutex::new(CompletionSlot {
        startup_sync_pending: true,
        ..CompletionSlot::default()
    });
    let prompts = Mutex::new(std::collections::VecDeque::new());

    // This is the production reader path: a new prompt mark, already
    // admitted at the focus-loss generation, reaches completion state.
    invalidate_completion_slot(&mut completion.lock().unwrap(), 1);
    apply_completion_reader_prompt_start(&completion, &prompts, Some((10, 0)), 1);
    with_completion_input_admission_slot(
        &mut completion.lock().unwrap(),
        &[b"\t"],
        Instant::now(),
        || ((), true),
    );
    apply_completion_update(
        &completion,
        CompletionUpdate::Sync {
            session: 75,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );
    apply_completion_update(
        &completion,
        CompletionUpdate::Show(sequenced_list(75, 1, 1)),
    );

    let slot = completion.lock().unwrap();
    assert_eq!(slot.session, Some(75));
    assert_eq!(slot.expected_request, Some(1));
    assert_eq!(slot.list.as_ref().and_then(|list| list.request), Some(1));
}

#[test]
fn prompt_input_start_hides_without_retiring_the_in_flight_request() {
    let mut slot = CompletionSlot {
        session: Some(7),
        sequenced: true,
        expected_request: Some(3),
        accept_updates: true,
        pending_cursor_col: Some(12),
        list: Some(sequenced_list(7, 2, 2)),
        hide_after: Some(Instant::now() + COMPLETION_HIDE_GRACE),
        ..CompletionSlot::default()
    };

    hide_completion_view_slot(&mut slot);

    assert!(slot.list.is_none());
    assert!(slot.hide_after.is_none());
    assert!(slot.accept_updates);
    assert_eq!(slot.expected_request, Some(3));
    assert_eq!(slot.pending_cursor_col, Some(12));
}

#[test]
fn ordinary_input_consumes_the_startup_sync_exception() {
    let cell = Mutex::new(CompletionSlot {
        startup_sync_pending: true,
        ..CompletionSlot::default()
    });
    invalidate_completion_slot(&mut cell.lock().unwrap(), 1);
    with_completion_input_admission_slot(
        &mut cell.lock().unwrap(),
        &[b"x"],
        Instant::now(),
        || ((), true),
    );
    reset_completion_session(&cell, 1);
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 71,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );

    let slot = cell.lock().unwrap();
    assert_eq!(slot.session, None);
    assert!(!slot.sync_expected);
    assert!(!slot.startup_sync_pending);
}

#[test]
fn a_pre_focus_bootstrap_reply_cannot_satisfy_the_post_focus_tab() {
    let cell = Mutex::new(CompletionSlot {
        startup_sync_pending: true,
        ..CompletionSlot::default()
    });
    reset_completion_session(&cell, 1);
    with_completion_input_admission_slot(
        &mut cell.lock().unwrap(),
        &[b"\t"],
        Instant::now(),
        || ((), true),
    );
    invalidate_completion_slot(&mut cell.lock().unwrap(), 1);
    with_completion_input_admission_slot(
        &mut cell.lock().unwrap(),
        &[b"\t"],
        Instant::now(),
        || ((), true),
    );
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 72,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );

    apply_completion_update(&cell, CompletionUpdate::Show(sequenced_list(72, 1, 1)));
    assert!(
        cell.lock().unwrap().list.is_none(),
        "the response to the pre-focus Tab must stay retired"
    );
    apply_completion_update(&cell, CompletionUpdate::Show(sequenced_list(72, 2, 2)));
    let slot = cell.lock().unwrap();
    assert_eq!(slot.next_request, 2);
    assert_eq!(slot.list.as_ref().and_then(|list| list.request), Some(2));
}

#[test]
fn delayed_sync_counts_only_keys_the_adapter_manages() {
    let cell = Mutex::new(CompletionSlot {
        startup_sync_pending: true,
        ..CompletionSlot::default()
    });
    reset_completion_session(&cell, 1);
    invalidate_completion_slot(&mut cell.lock().unwrap(), 1);
    with_completion_input_admission_slot(
        &mut cell.lock().unwrap(),
        &[b"\x1b[Z"],
        Instant::now(),
        || ((), true),
    );
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 73,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );
    {
        let slot = cell.lock().unwrap();
        assert_eq!(slot.next_request, 0);
        assert_eq!(slot.expected_request, None);
        assert!(!slot.accept_updates);
    }

    with_completion_input_admission_slot(
        &mut cell.lock().unwrap(),
        &[b"\t"],
        Instant::now(),
        || ((), true),
    );
    apply_completion_update(&cell, CompletionUpdate::Show(sequenced_list(73, 1, 1)));
    assert_eq!(
        cell.lock()
            .unwrap()
            .list
            .as_ref()
            .and_then(|list| list.request),
        Some(1)
    );
}

#[test]
fn delayed_sync_reconciles_mixed_completion_directions_in_order() {
    let cell = Mutex::new(CompletionSlot::default());
    reset_completion_session(&cell, 1);
    with_completion_input_admission_slot(
        &mut cell.lock().unwrap(),
        &[b"\t", b"\x1b[Z"],
        Instant::now(),
        || ((), true),
    );
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 74,
            keys: kettle_vt::completion::KEY_TAB | kettle_vt::completion::KEY_BACKTAB,
        },
    );
    let slot = cell.lock().unwrap();
    assert_eq!(slot.next_request, 2);
    assert_eq!(slot.expected_request, Some(2));
    assert!(slot.accept_updates);
}

#[test]
fn stale_sequenced_clear_and_sync_cannot_cross_a_focus_boundary() {
    let cell = Mutex::new(CompletionSlot::default());
    reset_completion_session(&cell, 0);
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 7,
            keys: kettle_vt::completion::KEY_TAB | kettle_vt::completion::KEY_BACKTAB,
        },
    );
    arm_completion_refresh_slot(&mut cell.lock().unwrap(), Instant::now());
    apply_completion_update(&cell, CompletionUpdate::Show(sequenced_list(7, 1, 1)));
    invalidate_completion_slot(&mut cell.lock().unwrap(), 0);
    arm_completion_refresh_slot(&mut cell.lock().unwrap(), Instant::now());

    apply_completion_update(
        &cell,
        CompletionUpdate::Clear {
            session: Some(7),
            generation: 99,
            request: Some(1),
        },
    );
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 8,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );
    apply_completion_update(&cell, CompletionUpdate::Show(sequenced_list(7, 2, 2)));

    let current = cell.lock().unwrap();
    assert_eq!(current.session, Some(7));
    assert_eq!(current.generation, 2);
    assert_eq!(current.list.as_ref().and_then(|list| list.request), Some(2));
}

#[test]
fn a_prompt_already_in_the_reader_queue_cannot_lift_focus_quarantine() {
    let cell = Mutex::new(CompletionSlot::default());
    reset_completion_session(&cell, 4);
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 7,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );
    invalidate_completion_slot(&mut cell.lock().unwrap(), 10);

    // The pump admitted generation 10 before focus was lost, but its
    // PromptStart and sync were still waiting on the parser thread.
    reset_completion_session(&cell, 10);
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 8,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );
    assert_eq!(cell.lock().unwrap().session, Some(7));

    // A later PTY read is a new ordered boundary and may establish the
    // shell's next prompt session.
    reset_completion_session(&cell, 11);
    apply_completion_update(
        &cell,
        CompletionUpdate::Sync {
            session: 9,
            keys: kettle_vt::completion::KEY_TAB,
        },
    );
    assert_eq!(cell.lock().unwrap().session, Some(9));
}

#[test]
fn input_requests_advance_only_after_admission_and_keep_key_boundaries() {
    let mut slot = CompletionSlot {
        session: Some(3),
        sequenced: true,
        managed_keys: kettle_vt::completion::KEY_TAB | kettle_vt::completion::KEY_BACKTAB,
        ..CompletionSlot::default()
    };
    let now = Instant::now();
    let rejected =
        with_completion_input_admission_slot(&mut slot, &[b"\t"], now, || ("backpressured", false));
    assert_eq!(rejected, "backpressured");
    assert_eq!(slot.next_request, 0);
    assert_eq!(slot.expected_request, None);

    with_completion_input_admission_slot(&mut slot, &[b"\t", b"\x1b[Z"], now, || ((), true));
    assert_eq!(slot.next_request, 2);
    assert_eq!(slot.expected_request, Some(2));
    assert!(slot.accept_updates);

    with_completion_input_admission_slot(&mut slot, &[b"\x1b[9;1:3u"], now, || ((), true));
    assert_eq!(slot.next_request, 2, "a Kitty Tab release is not a request");
    assert_eq!(slot.expected_request, Some(2));
    assert_eq!(
        completion_input_effect(b"\x1b[9;1:2u"),
        super::CompletionInputEffect::Refresh(kettle_vt::completion::KEY_TAB),
        "a Kitty key repeat invokes the shell binding and is a request"
    );

    slot.managed_keys = kettle_vt::completion::KEY_TAB;
    with_completion_input_admission_slot(&mut slot, &[b"\x1b[Z"], now, || ((), true));
    assert_eq!(
        slot.next_request, 2,
        "a preserved custom Shift-Tab must not consume the shell's request id"
    );
    assert!(!slot.accept_updates);
    let cell = Mutex::new(slot);
    apply_completion_update(
        &cell,
        CompletionUpdate::Keymap {
            session: 3,
            keys: kettle_vt::completion::KEY_TAB | kettle_vt::completion::KEY_BACKTAB,
        },
    );
    let mut slot = cell.into_inner().unwrap();
    with_completion_input_admission_slot(&mut slot, &[b"\x1b[Z"], now, || ((), true));
    assert_eq!(
        slot.next_request, 3,
        "the active keymap may re-enable Shift-Tab"
    );
}

#[test]
fn the_command_anchor_stays_with_its_prefix_and_updates_on_recapture() {
    let mut slot = CompletionSlot {
        session: Some(3),
        sequenced: true,
        managed_keys: kettle_vt::completion::KEY_TAB,
        ..CompletionSlot::default()
    };
    let now = Instant::now();

    with_completion_input_admission_slot_at(&mut slot, &[b"\t"], now, Some(19), || ((), true));
    assert_eq!(
        slot.anchor_cursor_col, None,
        "a request is not yet a prefix pair"
    );

    let cell = Mutex::new(slot);
    let mut first = sequenced_list(3, 1, 1);
    first.input_prefix = Some("git ch".to_string());
    apply_completion_update(&cell, CompletionUpdate::Show(first));
    let mut slot = cell.into_inner().unwrap();
    assert_eq!(slot.anchor_cursor_col, Some(19));
    assert_eq!(slot.anchor_input_prefix, Some(Some("git ch".to_string())));
    with_completion_input_admission_slot_at(&mut slot, &[b"\t"], now, Some(25), || ((), true));
    let mut cycled = sequenced_list(3, 2, 2);
    cycled.input_prefix = Some("git ch".to_string());
    let cell = Mutex::new(slot);
    apply_completion_update(&cell, CompletionUpdate::Update(cycled));
    let mut slot = cell.into_inner().unwrap();
    assert_eq!(
        slot.anchor_cursor_col,
        Some(19),
        "candidate insertion moves the cursor, but must not move the card"
    );

    // Fish and PowerShell retain their cycle across Ctrl-L, and focus loss
    // is another display-only boundary. The list disappears, but the next
    // reply carrying the same prefix must keep the original pair.
    with_completion_input_admission_slot_at(&mut slot, &[b"\x0c"], now, Some(25), || ((), true));
    assert!(slot.list.is_none());
    assert_eq!(slot.anchor_cursor_col, Some(19));
    with_completion_input_admission_slot_at(&mut slot, &[b"\t"], now, Some(25), || ((), true));
    let mut after_redraw = sequenced_list(3, 3, 3);
    after_redraw.input_prefix = Some("git ch".to_string());
    let cell = Mutex::new(slot);
    apply_completion_update(&cell, CompletionUpdate::Update(after_redraw));
    let mut slot = cell.into_inner().unwrap();
    assert_eq!(
        slot.anchor_cursor_col,
        Some(19),
        "a hidden card must not forget which cursor produced its retained prefix"
    );

    invalidate_completion_slot(&mut slot, 0);
    with_completion_input_admission_slot_at(&mut slot, &[b"\t"], now, Some(25), || ((), true));
    let mut after_focus = sequenced_list(3, 4, 4);
    after_focus.input_prefix = Some("git ch".to_string());
    let cell = Mutex::new(slot);
    apply_completion_update(&cell, CompletionUpdate::Update(after_focus));
    let mut slot = cell.into_inner().unwrap();
    assert_eq!(slot.anchor_cursor_col, Some(19));

    with_completion_input_admission_slot_at(&mut slot, &[b"\t"], now, Some(31), || ((), true));
    let mut recaptured = sequenced_list(3, 5, 5);
    recaptured.input_prefix = Some("git checkout/".to_string());
    let cell = Mutex::new(slot);
    apply_completion_update(&cell, CompletionUpdate::Show(recaptured));
    let mut slot = cell.into_inner().unwrap();
    assert_eq!(
        slot.anchor_cursor_col,
        Some(31),
        "a newly captured prefix must pair with its own request cursor"
    );

    clear_completion_slot(&mut slot);
    assert_eq!(slot.anchor_cursor_col, None);
    assert_eq!(slot.anchor_input_prefix, None);
    assert_eq!(slot.pending_cursor_col, None);
}

#[test]
fn raw_batches_count_tabs_in_order_but_keep_bracketed_paste_opaque() {
    let now = Instant::now();
    let mut slot = CompletionSlot {
        session: Some(3),
        sequenced: true,
        managed_keys: kettle_vt::completion::KEY_TAB | kettle_vt::completion::KEY_BACKTAB,
        ..CompletionSlot::default()
    };

    with_completion_input_admission_slot(&mut slot, &[b"x\t\t"], now, || ((), true));
    assert_eq!(slot.next_request, 2);
    assert_eq!(slot.expected_request, Some(2));
    assert!(slot.accept_updates, "the final byte was a completion key");

    with_completion_input_admission_slot(&mut slot, &[b"\tword"], now, || ((), true));
    assert_eq!(slot.next_request, 3);
    assert_eq!(slot.expected_request, None);
    assert!(!slot.accept_updates, "text after Tab retires its response");

    with_completion_input_admission_slot(
        &mut slot,
        &[b"\x1b[200~pasted\t\ttext\x1b[201~"],
        now,
        || ((), true),
    );
    assert_eq!(slot.next_request, 3, "paste payload Tabs are not bindings");
    assert_eq!(slot.expected_request, None);

    let mut anchor_slot = CompletionSlot {
        session: Some(3),
        sequenced: true,
        managed_keys: kettle_vt::completion::KEY_TAB,
        ..CompletionSlot::default()
    };
    with_completion_input_admission_slot_at(
        &mut anchor_slot,
        &[b"typed", b"\t"],
        now,
        Some(17),
        || ((), true),
    );
    assert_eq!(
        anchor_slot.anchor_cursor_col, None,
        "a Tab after text in the same remote batch must not use the pre-typing cursor"
    );
}

#[test]
fn request_exhaustion_fails_closed_until_the_next_prompt() {
    let now = Instant::now();
    let mut slot = CompletionSlot {
        session: Some(9),
        sequenced: true,
        next_request: MAX_COMPLETION_REQUEST - 1,
        ..CompletionSlot::default()
    };
    arm_completion_refresh_slot(&mut slot, now);
    assert_eq!(slot.expected_request, Some(MAX_COMPLETION_REQUEST));

    arm_completion_refresh_slot(&mut slot, now);
    assert_eq!(slot.next_request, MAX_COMPLETION_REQUEST);
    assert_eq!(slot.expected_request, None);
    assert!(!slot.accept_updates);
}

#[test]
fn only_tab_presses_and_repeats_refresh_completion() {
    use super::CompletionInputEffect::{Clear, Dismiss, Ignore, Refresh};

    for input in [
        b"\t".as_slice(),
        b"\x1b[9u",
        b"\x1b[9;1u",
        b"\x1b[9;1:1u",
        b"\x1b[9;1:2u",
        b"\x1b[9;65u",
        b"\x1b[9;129u",
    ] {
        assert_eq!(
            completion_input_effect(input),
            Refresh(kettle_vt::completion::KEY_TAB),
            "{input:?}"
        );
    }
    for input in [
        b"\x1b[Z".as_slice(),
        b"\x1b[9;2:1u",
        b"\x1b[9;2:2;9u",
        b"\x1b[9;66u",
        b"\x1b[9;130u",
    ] {
        assert_eq!(
            completion_input_effect(input),
            Refresh(kettle_vt::completion::KEY_BACKTAB),
            "{input:?}"
        );
    }
    for release in [b"\x1b[9;1:3u".as_slice(), b"\x1b[9;2:3u"] {
        assert_eq!(completion_input_effect(release), Ignore, "{release:?}");
    }
    assert_eq!(completion_input_effect(b""), Ignore);
    assert_eq!(completion_input_effect(b"\x0c"), Dismiss);
    for input in [
        b"x".as_slice(),
        b"\r",
        b"\x1b[A",
        b"\x1b[1;2Z",
        b"\x1b[9;0u",
        b"\x1b[9;3u",
        b"\x1b[9;5u",
        b"\x1b[9;6u",
        b"\x1b[9;9u",
        b"\x1b[90u",
        b"\x1b[9;u",
        b"\x1b[9;2Xu",
    ] {
        assert_eq!(completion_input_effect(input), Clear, "{input:?}");
    }
}

#[test]
fn legacy_publishers_stay_quarantined_after_focus_loss_until_a_prompt() {
    let cell = Mutex::new(CompletionSlot::default());
    arm_completion_refresh_slot(&mut cell.lock().unwrap(), Instant::now());
    apply_completion_update(&cell, CompletionUpdate::Show(list(1)));
    invalidate_completion_slot(&mut cell.lock().unwrap(), 0);
    arm_completion_refresh_slot(&mut cell.lock().unwrap(), Instant::now());
    apply_completion_update(&cell, CompletionUpdate::Show(list(2)));
    assert!(cell.lock().unwrap().list.is_none());

    prepare_optional_completion_resync(&cell, 1);
    arm_completion_refresh_slot(&mut cell.lock().unwrap(), Instant::now());
    apply_completion_update(&cell, CompletionUpdate::Show(list(1)));
    let slot = cell.lock().unwrap();
    assert!(slot.list.is_some());
    assert!(
        !slot.legacy_quarantined,
        "a prompt admitted after the focus boundary must release a legacy publisher"
    );
}

#[test]
fn grace_expiry_requests_exactly_one_erase_frame() {
    let now = Instant::now();
    let mut slot = CompletionSlot {
        generation: 3,
        list: Some(list(3)),
        accept_updates: true,
        hide_after: Some(now + COMPLETION_HIDE_GRACE),
        ..CompletionSlot::default()
    };
    assert_eq!(
        poll_completion_hide_slot(&mut slot, now),
        (false, Some(COMPLETION_HIDE_GRACE))
    );
    assert_eq!(
        poll_completion_hide_slot(&mut slot, now + COMPLETION_HIDE_GRACE),
        (true, None)
    );
    assert_eq!(
        poll_completion_hide_slot(&mut slot, now + COMPLETION_HIDE_GRACE * 2),
        (false, None),
        "the event loop must quiesce after the erase redraw"
    );
}

#[test]
fn a_stale_update_neither_reopens_the_card_nor_extends_the_window() {
    let now = Instant::now();
    let deadline = now + COMPLETION_HIDE_GRACE;
    let cell = Mutex::new(CompletionSlot {
        generation: 9,
        list: None,
        accept_updates: true,
        hide_after: Some(deadline),
        ..CompletionSlot::default()
    });
    apply_completion_update(&cell, CompletionUpdate::Update(list(8)));

    let slot = cell.lock().unwrap();
    assert_eq!(slot.generation, 9);
    assert!(slot.list.is_none());
    assert_eq!(slot.hide_after, Some(deadline));
}
