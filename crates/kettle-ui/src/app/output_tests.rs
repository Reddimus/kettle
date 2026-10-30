/// Only pane removals request a resize and redraw; idle reaps must not reflow.
/// The shared split fixture supplies the real PTYs required by `Pane`.
#[test]
fn reap_reports_whether_it_removed_a_pane() {
    let (mut mux, target, sibling) = title_test_split_mux();

    mux.panes.get_mut(&target).unwrap().closed = true;
    let first = mux.reap();
    assert!(
        first.layout_changed,
        "the dead pane's space was handed over"
    );
    assert!(!first.mux_empty, "the sibling keeps the mux alive");

    let second = mux.reap();
    assert!(
        !second.layout_changed,
        "a pass that reaps nothing must not ask for a resize, or every \
             idle frame re-drives resize_all"
    );
    assert!(!second.mux_empty);

    mux.panes.get_mut(&sibling).unwrap().closed = true;
    let last = mux.reap();
    assert!(last.layout_changed);
    assert!(last.mux_empty, "the last pane leaves nothing behind");

    // The degenerate case: empty is not the same fact as changed.
    let empty = Mux::new().reap();
    assert!(!empty.layout_changed);
    assert!(empty.mux_empty);
}

#[test]
fn output_generations_commit_only_after_presentation() {
    let mut committed = std::collections::HashMap::from([(1, 10), (2, 20)]);
    let mut candidate = std::collections::HashMap::from([(1, 11), (2, 21)]);

    for outcome in [
        FrameOutcome::RetryLater,
        FrameOutcome::Occluded,
        FrameOutcome::SurfaceLost,
    ] {
        assert!(!apply_output_generation_outcome(
            outcome,
            &mut committed,
            &mut candidate,
        ));
        assert_eq!(
            committed,
            std::collections::HashMap::from([(1, 10), (2, 20)])
        );
        assert_eq!(
            candidate,
            std::collections::HashMap::from([(1, 11), (2, 21)])
        );
    }

    assert!(apply_output_generation_outcome(
        FrameOutcome::Presented,
        &mut committed,
        &mut candidate,
    ));
    assert_eq!(
        committed,
        std::collections::HashMap::from([(1, 11), (2, 21)])
    );
    assert_eq!(
        candidate,
        std::collections::HashMap::from([(1, 10), (2, 20)])
    );
}

#[test]
fn output_activity_uses_the_lock_free_generation_edge() {
    assert!(!output_generation_advanced(None, 7));
    assert!(!output_generation_advanced(Some(7), 7));
    assert!(output_generation_advanced(Some(7), 8));
    assert!(
        output_generation_advanced(Some(u64::MAX), 0),
        "generation wrap is still a change"
    );
}
#[test]
#[cfg(unix)]
fn output_frame_preserves_publication_after_cached_grid_reads() {
    let (mux, ids) = title_test_tab_mux(1);
    let pane = ids[0];
    let mut ws = WindowState::new(1, mux);
    let write_url = |ws: &WindowState, url: &str| {
        let mut term = ws.mux.panes[&pane].term.term.lock().unwrap();
        for (column, c) in url.chars().enumerate() {
            term.grid_mut()[kettle_core::Line(0)][kettle_core::Column(column)].c = c;
        }
    };
    write_url(&ws, "https://old.example");
    super::scan_links(&mut ws);
    assert_eq!(ws.links[0].uri, "https://old.example");
    let generation = ws.mux.panes[&pane].term.output_generation();
    ws.seen_output_gen.insert(pane, generation);

    super::prepare_output_frame(&mut ws, false, |ws| {
        // Interleave a cached reader after the grid mutation but before the
        // parser publishes output. The core commit-hook test covers this
        // boundary in the real parser; here it exercises the real link cache.
        write_url(ws, "https://new.example");
        super::scan_links(ws);
        assert_eq!(ws.links[0].uri, "https://old.example");
        assert!(
            ws.mux.panes[&pane]
                .term
                .screen_text(0)
                .unwrap()
                .text
                .contains("https://new.example")
        );

        // A no-text local parser turn drives the real publication helper
        // after the paused reader, without sending bytes through the child.
        assert!(ws.mux.panes[&pane].term.apply_local_output(b"\x1b[0m"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while ws.mux.panes[&pane].term.output_generation() == generation {
            assert!(
                std::time::Instant::now() < deadline,
                "parser did not publish"
            );
            std::thread::yield_now();
        }
    });
    let published = ws.mux.panes[&pane].term.output_generation();
    assert_ne!(published, generation);
    let mut snapshot = PaneSnapshot::default();
    snapshot.capture(&ws.mux.panes[&pane].term.term.lock().unwrap());
    ws.pane_snapshot_keys = vec![(
        pane,
        ws.pending_output_gen[&pane],
        snapshot.columns,
        snapshot.screen_lines,
    )];
    assert!(!pane_snapshot_keys_match(
        &ws.pane_snapshot_keys,
        [(pane, published, snapshot.columns, snapshot.screen_lines)]
    ));
    // The normal frame acknowledges late. A publication coalesced into the
    // wake that scheduled this frame need not have another event queued.
    ws.mux.panes[&pane].term.acknowledge_output_wake();
    ws.output_pacer.begin_frame();
    assert!(apply_output_generation_outcome(
        FrameOutcome::Presented,
        &mut ws.seen_output_gen,
        &mut ws.pending_output_gen,
    ));
    assert_ne!(
        ws.seen_output_gen[&pane], published,
        "presenting must not consume the invalidation needed by the stale link cache"
    );
    ws.output_pacer.presented();
    super::defer_output_racing_frame(&mut ws);
    assert!(ws.output_pacer.is_deferred());
    super::prepare_output_frame(&mut ws, false, super::scan_links);
    assert_eq!(ws.links[0].uri, "https://new.example");
    ws.output_pacer.begin_frame();
    assert!(apply_output_generation_outcome(
        FrameOutcome::Presented,
        &mut ws.seen_output_gen,
        &mut ws.pending_output_gen,
    ));
    ws.output_pacer.presented();
    super::defer_output_racing_frame(&mut ws);
    assert!(!ws.output_pacer.is_deferred(), "quiet output must settle");
}

#[test]
fn hover_generation_candidate_preserves_racing_output_as_pending() {
    let committed = std::collections::HashMap::from([(1, 10), (2, 20)]);
    let cached = [(1, 10, 120, 40)];
    let live_after_race = [(1, 11), (2, 21)];
    let mut candidate = std::collections::HashMap::new();

    stage_output_generations_for_frame(&mut candidate, &committed, Some(&cached), live_after_race);
    assert_eq!(candidate, committed);

    stage_output_generations_for_frame(&mut candidate, &committed, None, live_after_race);
    assert_eq!(candidate, std::collections::HashMap::from(live_after_race));
}

#[test]
fn stale_presented_output_wakeup_rearms_without_losing_a_race() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let wake_count = wakes.clone();
    let gate = OutputWakeGate::new(Arc::new(move || {
        wake_count.fetch_add(1, Ordering::SeqCst);
    }));
    let generation = AtomicU64::new(0);

    // The first output schedules the redraw. The frame reopens the latch,
    // then output racing that frame schedules another wake but is included
    // in the generation that successfully presents.
    assert!(gate.request());
    gate.acknowledge();
    generation.store(1, Ordering::Release);
    assert!(gate.request());
    let seen_generation = 1;
    assert_eq!(wakes.load(Ordering::SeqCst), 2);

    assert!(
        !output_wakeup_needs_paint(
            || generation.load(Ordering::Acquire) != seen_generation,
            || gate.acknowledge(),
        ),
        "the queued event is stale because its generation already presented"
    );

    generation.store(2, Ordering::Release);
    assert!(
        gate.request(),
        "rearming the stale event must let future output own a wake"
    );
    assert_eq!(wakes.load(Ordering::SeqCst), 3);

    // Output between the first no-damage sample and rearm cannot enqueue a
    // duplicate while the old wake is pending. The post-rearm sample must
    // therefore retain it for this event instead of returning stale.
    let raced_generation = AtomicU64::new(2);
    let raced_seen = 2;
    assert!(output_wakeup_needs_paint(
        || raced_generation.load(Ordering::Acquire) != raced_seen,
        || {
            raced_generation.store(3, Ordering::Release);
            assert!(!gate.request());
            gate.acknowledge();
        },
    ));
}

#[test]
fn dirty_output_wakeup_keeps_latch_closed_until_frame_snapshot() {
    let rearms = AtomicUsize::new(0);
    assert!(output_wakeup_needs_paint(
        || true,
        || {
            rearms.fetch_add(1, Ordering::SeqCst);
        },
    ));
    assert_eq!(
        rearms.load(Ordering::SeqCst),
        0,
        "dirty output must remain latched across the deferred paint budget"
    );
}

#[test]
fn output_wakeup_quiesces_for_every_render_hidden_state_and_repair() {
    let hidden = |occluded, minimized, shown, visible| {
        super::render_hidden_from_observations(occluded, minimized, shown, visible)
    };

    assert!(!hidden(false, false, true, true));
    assert!(hidden(true, false, true, true), "occluded");
    assert!(hidden(false, true, true, true), "minimized");
    assert!(hidden(false, false, true, false), "explicitly invisible");
    assert!(
        !hidden(false, false, false, false),
        "startup windows intentionally begin invisible before first reveal"
    );

    for render_hidden in [
        hidden(true, false, true, true),
        hidden(false, true, true, true),
        hidden(false, false, true, false),
    ] {
        assert!(super::output_wakeup_must_quiesce(
            render_hidden,
            false,
            false
        ));
    }
    assert!(super::output_wakeup_must_quiesce(false, true, false));
    assert!(
        super::output_wakeup_must_quiesce(false, false, true),
        "renderer/GPU unavailable"
    );
    assert!(!super::output_wakeup_must_quiesce(false, false, false));
}

#[test]
fn hidden_output_sidechannels_keep_transport_wakes_without_enabling_paints() {
    assert!(!super::output_wake_transport_enabled(true, false));
    assert!(super::output_wake_transport_enabled(true, true));
    assert!(super::output_wake_transport_enabled(false, false));
    assert!(super::output_wake_transport_enabled(false, true));

    let wakes = Arc::new(AtomicUsize::new(0));
    let wakes_for_gate = wakes.clone();
    let gate = OutputWakeGate::new(Arc::new(move || {
        wakes_for_gate.fetch_add(1, Ordering::SeqCst);
    }));
    gate.set_enabled(super::output_wake_transport_enabled(true, true));
    for expected in 1..=32 {
        assert!(gate.request());
        assert_eq!(wakes.load(Ordering::SeqCst), expected);
        // Model the hidden-window handler's rearm-before-drain order:
        // every later lossless chunk receives another event-loop wake.
        gate.acknowledge();
    }
}

#[test]
fn pane_reaping_cancels_divider_drags() {
    let src = production_source();
    let mut count = 0;
    for after_reap in src.split("let reaped = ws.mux.reap();").skip(1) {
        let flat: String = after_reap.split_whitespace().collect();
        assert!(flat.starts_with("ifreaped.layout_changed{ws.dragging_split=None;}"));
        count += 1;
    }
    assert_eq!(count, 2);
}

#[test]
fn process_status_cannot_reap_a_pane_before_the_ordered_pty_exit() {
    let src = production_source();
    let drain = src
        .split("fn drain_events(&mut self, ws: &mut WindowState) {")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("drain_events body");
    let exit_latch = drain
        .split("if exited && !pane.closed {")
        .nth(1)
        .and_then(|body| body.split("\n            }\n").next())
        .expect("ordered exit latch");
    assert!(
        exit_latch.contains("apply_pane_exit_action(")
            && exit_latch.contains("self.cfg.exit_action"),
        "the reader's ordered exit must latch reaping and apply exit-action policy"
    );
    // The exit latch is read before the pane's other latches and queue, and
    // acted on after them: output the reader published before the exit is
    // never left behind it.
    let read = drain.find("let exited = pane.term.take_exited();");
    let taken = drain.find("pane.term.take_title()");
    let queue = drain.find(".take_clipboard_events()");
    assert!(
        drain.contains(".chain(pane.rx.try_iter())"),
        "drain both clipboard and reply queues before applying exit policy"
    );
    let applied = drain.find("if exited && !pane.closed {");
    assert!(
        read.is_some() && read < taken && taken < queue && queue < applied,
        "exit read first, applied last: {read:?} {taken:?} {queue:?} {applied:?}"
    );

    let status_arm = drain
        .split("TermEvent::ChildExit(_) => {")
        .nth(1)
        .and_then(|body| body.split("\n                    _ => {}").next())
        .expect("advisory ChildExit arm");
    for forbidden in [
        "exit_observed",
        "pane.closed",
        "pane.held",
        "pending_restarts",
        "apply_pane_exit_action",
    ] {
        assert!(
            !status_arm.contains(forbidden),
            "ChildExit status must not perform lifecycle action {forbidden:?} \
                 before the final PTY drain"
        );
    }
    assert!(
        drain.contains("pane.term.direct_child_exit_at()")
            && drain.contains("pane.term.begin_pty_output_close()")
            && drain.contains("bounded_pty_exit_action(")
            && drain.matches("apply_pane_exit_action(").count() >= 2,
        "the ConPTY path must have a bounded post-child drain and fallback"
    );
}

#[test]
fn windows_child_exit_drain_has_two_distinct_bounds() {
    use super::BoundedPtyExitPhase::{Failed, InProgress, NotStarted};
    let bound = kettle_core::term::PTY_CHILD_EXIT_EOF_TIMEOUT;
    assert_eq!(
        super::bounded_pty_exit_action(bound - std::time::Duration::from_millis(1), NotStarted,),
        super::BoundedPtyExitAction::Wait
    );
    assert_eq!(
        super::bounded_pty_exit_action(bound, NotStarted),
        super::BoundedPtyExitAction::BeginClose
    );
    assert_eq!(
        super::bounded_pty_exit_action(bound * 2, NotStarted),
        super::BoundedPtyExitAction::BeginClose,
        "a delayed first wake must close before applying policy"
    );
    assert_eq!(
        super::bounded_pty_exit_action(
            bound * 2,
            InProgress(bound - std::time::Duration::from_millis(1)),
        ),
        super::BoundedPtyExitAction::Wait,
        "the second bound starts when close actually starts"
    );
    assert_eq!(
        super::bounded_pty_exit_action(bound, InProgress(bound)),
        super::BoundedPtyExitAction::ApplyPolicy,
        "a parked close cannot hold GUI policy forever"
    );
    assert_eq!(
        super::bounded_pty_exit_action(
            bound * 2,
            Failed {
                retry_in: std::time::Duration::from_millis(1),
            },
        ),
        super::BoundedPtyExitAction::Wait,
        "a failed worker start must not be recorded as an in-progress close"
    );
    assert_eq!(
        super::bounded_pty_exit_action(
            bound * 2,
            Failed {
                retry_in: std::time::Duration::ZERO,
            },
        ),
        super::BoundedPtyExitAction::BeginClose,
        "worker-start failure is retried instead of applying Hold to a live master"
    );
}

#[test]
fn lifecycle_wakes_are_drained_before_the_output_generation_gate() {
    let src = production_source();
    let inner = src
        .split("fn user_event_inner(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("user_event_inner body");
    let wake = inner
        .split("UserEvent::Wakeup => {")
        .nth(1)
        .and_then(|body| body.split("UserEvent::ReloadConfig").next())
        .expect("Wakeup arm");
    let lifecycle = wake
        .find("take_window_lifecycle_wakes(ws)")
        .expect("lifecycle edge consumption");
    let drain = wake[lifecycle..]
        .find("self.drain_events(ws);")
        .map(|offset| lifecycle + offset)
        .expect("semantic event drain");
    let output_gate = wake
        .find("output_wakeup_needs_paint(")
        .expect("output generation gate");
    assert!(
        lifecycle < drain && drain < output_gate,
        "quiet/hidden lifecycle events must be applied before a no-output wake can return"
    );
}

#[test]
fn held_child_status_poll_keeps_an_idle_deadline_until_reaped() {
    let src = production_source();
    let about = src
        .split("fn about_to_wait_inner(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n}").next())
        .expect("about_to_wait_inner body");
    assert!(
        about.contains("let held_child_reap_pending = ws.mux.poll_held_child_statuses();")
            && about.contains("if held_child_reap_pending {")
            && about.contains("std::time::Duration::from_secs(1)"),
        "a held pane whose status lagged EOF needs a bounded idle wake until the zombie is collected"
    );
}

#[test]
fn restarts_use_launch_directory_routing() {
    let src = production_source();
    let body = src
        .split("fn process_pending_pane_restarts(")
        .nth(1)
        .unwrap()
        .split("\n    }\n")
        .next()
        .unwrap();
    assert!(
        body.contains("new_tab_with_launch_settings_geometry("),
        "restarts must route reported WSL directories through the launcher"
    );
    assert!(body.contains("pane.reconnect_launch.clone()"));
    assert!(body.contains("launch.as_ref()"));
}

