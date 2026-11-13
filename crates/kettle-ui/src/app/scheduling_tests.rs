#[test]
fn input_rejection_feedback_is_visible_but_throttled() {
    let now = std::time::Instant::now();
    assert!(input_rejection_message(PaneInputResult::Queued).is_none());
    assert!(input_rejection_message(PaneInputResult::ReadOnly).is_none());
    assert!(input_rejection_message(PaneInputResult::Backpressured).is_some());
    assert!(should_notify_input_rejection(
        None,
        PaneInputResult::Backpressured,
        now
    ));
    assert!(!should_notify_input_rejection(
        Some((PaneInputResult::Backpressured, now)),
        PaneInputResult::Backpressured,
        now + std::time::Duration::from_secs(1)
    ));
    assert!(
        should_notify_input_rejection(
            Some((PaneInputResult::Backpressured, now)),
            PaneInputResult::Oversize,
            now + std::time::Duration::from_secs(1)
        ),
        "a different failure class must be surfaced immediately"
    );
    assert!(should_notify_input_rejection(
        Some((PaneInputResult::Backpressured, now)),
        PaneInputResult::Backpressured,
        now + std::time::Duration::from_secs(3)
    ));
}

#[test]
fn optional_throttle_is_safe_and_immediate_at_clock_origin() {
    use std::time::{Duration, Instant};

    let origin = Instant::now();
    let interval = Duration::from_millis(200);
    assert!(super::throttle_elapsed(None, origin, interval));
    assert!(!super::throttle_elapsed(Some(origin), origin, interval));
    assert!(!super::throttle_elapsed(
        Some(origin + Duration::from_millis(1)),
        origin,
        interval
    ));
    assert!(super::throttle_elapsed(
        Some(origin),
        origin + interval,
        interval
    ));
}

/// A `cd` whose prompt redraw lands inside the throttle window must still
/// be polled. Dropping that poll left new panes in the old directory until
/// something redrew, which on an unfocused window can be never.
#[test]
fn a_throttled_process_poll_is_deferred_not_dropped() {
    use super::{REMOTE_POLL_INTERVAL, remote_poll_deferred_until};
    use std::time::{Duration, Instant};

    let last = Instant::now();
    assert_eq!(remote_poll_deferred_until(None, last), None);
    assert_eq!(
        remote_poll_deferred_until(Some(last), last + Duration::from_millis(50)),
        Some(last + REMOTE_POLL_INTERVAL)
    );
    assert_eq!(
        remote_poll_deferred_until(Some(last), last + REMOTE_POLL_INTERVAL),
        None
    );

    let src = super::production_source();
    let wake = src
        .split("fn about_to_wait(&mut self")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("about_to_wait body");
    assert!(
        wake.contains("self.poll_remote_contexts(&mut ws)"),
        "about_to_wait must run the deferred poll"
    );
    assert!(
        wake.contains("if let Some(due) = self.remote_poll_due"),
        "about_to_wait must wake for the deferred poll"
    );
}

#[test]
fn animated_bg_redraw_is_edge_triggered() {
    let src = production_source();
    assert!(
        src.contains("let bg_frame_due = bg_frame.is_some() && bg_frame != ws.last_bg_frame;"),
        "the bg redraw must edge-trigger on a frame-index change"
    );
    // The redraw block must use the edge (bg_frame_due), not a level anim flag.
    assert!(
        src.contains("|| bg_frame_due\n"),
        "the redraw block must trigger on bg_frame_due, not level anim_active"
    );
    // The wake interval is the GIF's frame boundary, not a fixed 30 fps.
    assert!(
        src.contains("let mut wait = animation_wait("),
        "the scheduler must merge the background frame interval"
    );
    assert_eq!(
        super::animation_wait(false, Some(173), None),
        Some(std::time::Duration::from_millis(173)),
        "the wait must use the bg frame interval, not the fixed 33ms tick"
    );
}

/// PERF (key-repeat stutter fix) drift guard: output inside the typing
/// window bypasses the coalescer (paints immediately); output after it
/// re-enters the frame-budget defer. Without the bypass, holding a key
/// stutters because echo paints ride the WaitUntil deadline (~16ms Windows
/// timer granularity) instead of the keystroke cadence.
#[test]
fn typed_echo_bypasses_the_output_coalescer() {
    use std::time::{Duration, Instant};
    let base = Instant::now();
    let now = base + Duration::from_millis(500);
    // Echo within the window → immediate (bypass).
    assert!(super::typed_recently(
        now,
        Some(base + Duration::from_millis(470)),
        super::TYPING_ECHO_WINDOW
    ));
    // Stale keystroke → back to the coalescer.
    assert!(!super::typed_recently(
        now,
        Some(base),
        super::TYPING_ECHO_WINDOW
    ));
    // Never typed → coalescer.
    assert!(!super::typed_recently(now, None, super::TYPING_ECHO_WINDOW));
    // The window must bridge OS key-repeat intervals (~33ms default) with
    // slack for ConPTY echo latency.
    assert!(super::TYPING_ECHO_WINDOW >= Duration::from_millis(100));
    // The Wakeup arm must consult it (source-level pin: the bypass calls
    // request_redraw and clears any pending coalesce).
    let src = production_source();
    assert!(
        src.contains(
            "if typed_recently(now, ws.last_typed, TYPING_ECHO_WINDOW) {\n                    ws.output_pacer.reset();"
        ),
        "the Wakeup arm must paint echo immediately"
    );
    // Immediately unless the echo settle holds it: the typed arm asks the
    // settle, and requests the redraw itself on `Now`.
    assert!(
        src.contains(
            "if ws.typed_echo.plan_paint(now, new_echo, ECHO_SETTLE_ENABLED)\n                        == crate::typed_echo::EchoPaint::Now\n                        && let Some(w) = &ws.window\n                    {\n                        w.request_redraw();"
        ),
        "the Wakeup arm must paint unsettled echo immediately"
    );
}

#[test]
fn output_paint_coalesces_within_frame_budget() {
    use super::{OUTPUT_FRAME_BUDGET, should_defer_output_paint};
    use std::time::{Duration, Instant};
    let now = Instant::now();
    // No prior paint → paint immediately (never defer the first frame).
    assert!(!should_defer_output_paint(now, None, OUTPUT_FRAME_BUDGET));
    // Painted just now → defer so a same-burst wakeup coalesces.
    assert!(should_defer_output_paint(
        now,
        Some(now),
        OUTPUT_FRAME_BUDGET
    ));
    // A frame's worth later → the budget elapsed, paint immediately.
    let later = now + OUTPUT_FRAME_BUDGET + Duration::from_millis(5);
    assert!(!should_defer_output_paint(
        later,
        Some(now),
        OUTPUT_FRAME_BUDGET
    ));
}

/// The output-paint budget must GROW under a sustained flood so fewer
/// per-frame snapshots take the `Term` lock the PTY reader needs. A brief
/// burst must stay at the responsive monitor-rate budget so keystroke echo
/// and short bursts don't get throttled.
#[test]
fn effective_output_budget_grows_under_sustained_flood() {
    use super::{
        DEEP_FLOOD_MAX_BUDGET, MID_FLOOD_MAX_BUDGET, OUTPUT_FRAME_BUDGET, effective_output_budget,
        output_frame_budget_for_refresh,
    };
    use std::time::Duration;
    // A brief burst stays at the window's responsive monitor rate.
    assert_eq!(
        effective_output_budget(OUTPUT_FRAME_BUDGET, 0),
        OUTPUT_FRAME_BUDGET
    );
    assert_eq!(
        effective_output_budget(OUTPUT_FRAME_BUDGET, 3),
        OUTPUT_FRAME_BUDGET
    );
    // A 60 Hz window retains the established ~30 fps, then ~20 fps
    // sustained-flood backoff.
    assert_eq!(
        effective_output_budget(OUTPUT_FRAME_BUDGET, 4),
        MID_FLOOD_MAX_BUDGET
    );
    assert_eq!(
        effective_output_budget(OUTPUT_FRAME_BUDGET, 15),
        MID_FLOOD_MAX_BUDGET
    );
    assert_eq!(
        effective_output_budget(OUTPUT_FRAME_BUDGET, 16),
        DEEP_FLOOD_MAX_BUDGET
    );
    assert_eq!(
        effective_output_budget(OUTPUT_FRAME_BUDGET, 10_000),
        DEEP_FLOOD_MAX_BUDGET
    );
    // A 165 Hz window's budget sits one small dispatch lead under its
    // 6.06 ms period. Under sustained output it stays bounded at 60/30 fps
    // instead of spending unbounded CPU repainting unreadable flood frames
    // at 82/55 fps.
    let high_refresh = output_frame_budget_for_refresh(Some(165_000));
    assert_eq!(high_refresh, Duration::from_nanos(5_810_607));
    assert_eq!(
        effective_output_budget(high_refresh, 4),
        Duration::from_nanos(16_666_667)
    );
    assert_eq!(
        effective_output_budget(high_refresh, 16),
        Duration::from_nanos(33_333_334)
    );
    // Monotonic non-decreasing: deeper flood never paints MORE often.
    let mut prev = Duration::ZERO;
    for n in 0..40u32 {
        let b = effective_output_budget(high_refresh, n);
        assert!(b >= prev, "budget must not shrink as flood deepens");
        prev = b;
    }
    // The throttle is bounded (never starves the settled frame indefinitely).
    assert!(effective_output_budget(OUTPUT_FRAME_BUDGET, u32::MAX) <= Duration::from_millis(50));
}

#[test]
fn output_frame_budget_tracks_monitor_refresh_with_safe_bounds() {
    use super::{OUTPUT_FRAME_BUDGET, output_frame_budget_for_refresh};
    use std::time::Duration;

    assert_eq!(output_frame_budget_for_refresh(None), OUTPUT_FRAME_BUDGET);
    assert_eq!(
        output_frame_budget_for_refresh(Some(0)),
        OUTPUT_FRAME_BUDGET
    );
    assert_eq!(
        output_frame_budget_for_refresh(Some(60_000)),
        Duration::from_nanos(16_416_667)
    );
    assert_eq!(
        output_frame_budget_for_refresh(Some(120_000)),
        Duration::from_nanos(8_083_334)
    );
    assert_eq!(
        output_frame_budget_for_refresh(Some(165_000)),
        Duration::from_nanos(5_810_607)
    );
    assert_eq!(
        output_frame_budget_for_refresh(Some(240_000)),
        Duration::from_millis(4)
    );
    // Driver outliers cannot create a busy loop or a sluggish >33 ms
    // output deadline.
    assert_eq!(
        output_frame_budget_for_refresh(Some(500_000)),
        Duration::from_millis(4)
    );
    assert_eq!(
        output_frame_budget_for_refresh(Some(20_000)),
        Duration::from_nanos(33_333_334)
    );
}

#[test]
fn monitor_refresh_probe_is_rate_limited_with_a_trailing_deadline() {
    use super::{MONITOR_REFRESH_PROBE_INTERVAL, monitor_refresh_probe_wait};
    use std::time::{Duration, Instant};

    let start = Instant::now();
    assert_eq!(monitor_refresh_probe_wait(None, start, false), None);
    assert_eq!(
        monitor_refresh_probe_wait(Some(start), start + Duration::from_millis(1), false),
        Some(MONITOR_REFRESH_PROBE_INTERVAL - Duration::from_millis(1))
    );
    assert_eq!(
        monitor_refresh_probe_wait(Some(start), start + MONITOR_REFRESH_PROBE_INTERVAL, false),
        None,
        "the trailing probe becomes due even if a move storm stopped"
    );
    assert_eq!(
        monitor_refresh_probe_wait(Some(start), start, true),
        None,
        "creation and DPI events force an immediate refresh probe"
    );
}

/// Behavioral pin for the deferred -> queued -> presenting transaction.
#[test]
fn output_coalescer_retains_flood_signal_without_busy_waiting() {
    let mut pacer = crate::window_state::OutputPaintPacer::default();
    pacer.defer();
    assert!(pacer.is_deferred());
    assert!(!pacer.redraw_queued());
    assert!(pacer.queue_deferred_redraw());
    assert!(pacer.redraw_queued());
    assert!(
        !pacer.queue_deferred_redraw(),
        "one due deadline queues at most one redraw"
    );
    assert!(
        pacer.begin_frame(),
        "queued output retains the flood signal"
    );
    pacer.presentation_failed();
    assert!(pacer.is_deferred());
    assert!(!pacer.redraw_queued());
    assert!(pacer.queue_deferred_redraw());
    assert!(pacer.begin_frame());
    pacer.presented();
    assert!(!pacer.is_deferred());

    // Keep a small source pin only for sub-millisecond deadline precision;
    // lifecycle behavior above is exercised through the real state machine.
    let src = production_source();
    let start = src
        .find("fn about_to_wait_inner(")
        .expect("about_to_wait_inner present");
    let rest = &src[start..];
    // Bound the scan to the about_to_wait_inner body (it is the last method
    // in the impl, so the next column-0 `}` closes the impl).
    let end = rest.find("\n}\n").map(|i| i + 2).unwrap_or(rest.len());
    let body = &rest[..end];
    assert!(
        body.contains("remaining.max(std::time::Duration::from_nanos(1))")
            && !body.contains("remaining.as_millis()"),
        "fractional monitor deadlines must reach ControlFlow without millisecond truncation"
    );
}

#[test]
fn output_frame_transition_follows_every_renderability_guard() {
    let source = production_source();
    let redraw_start = source.find("fn redraw(").expect("redraw present");
    let redraw_tail = &source[redraw_start..];
    let redraw_end = redraw_tail
        .find("\n    }\n")
        .expect("redraw end");
    let redraw = &redraw_tail[..redraw_end];
    let renderability_guard = redraw
        .find("if !ws.dpi_resize.render_allowed()")
        .expect("DPI/recovery/visibility guard present");
    let screenshot_policy = redraw
        .find("let screenshot_may_render_offscreen")
        .expect("offscreen screenshot policy present");
    let renderer_guard = redraw
        .find("if ws.renderer.is_none()")
        .expect("renderer-availability guard present");
    let frame_transition = redraw
        .find("let was_coalescing_paint = ws.output_pacer.begin_frame();")
        .expect("output frame transition present");
    let combined_guard = &redraw[screenshot_policy..renderer_guard];

    assert!(
        combined_guard.contains("ws.frame_recovery.has_pending()")
            && combined_guard.contains("ws.frame_recovery.renderer_rebuild_pending()")
            && combined_guard.contains("!screenshot_may_render_offscreen")
            && combined_guard.contains("window_is_render_hidden(ws)")
            && renderability_guard < frame_transition
            && renderer_guard < frame_transition,
        "only an explicit offscreen capture may bypass transient surface recovery; renderer \
             rebuilds and ordinary hidden output must stay blocked before Presenting"
    );
}

#[test]
fn terminal_animation_redraw_waits_for_frame_deadline() {
    let src = production_source();
    let redraw = src
        .split("if (bell_active")
        .nth(1)
        .unwrap()
        .split("let blink_wait")
        .next()
        .unwrap();
    assert!(
        !redraw.contains("|| term_anim\n"),
        "terminal animations must not request redraw on turns between frame deadlines"
    );
    assert!(redraw.contains("|| term_anim_due"));
}

#[test]
fn inactive_tab_and_zoomed_out_animations_do_not_schedule_frames() {
    let (mux, panes) = title_test_tab_mux(2);
    let mut ws = WindowState::new(1, mux);
    let image = kettle_core::ImageData::new(1, 1, vec![1, 0, 0, 255]).unwrap();
    let started = std::time::Instant::now();
    ws.mux.panes[&panes[0]].term.anims.lock().unwrap().insert(
        1,
        kettle_core::images::AnimEntry {
            imgs: vec![image.clone(), image],
            gaps: vec![1000, 1000],
            state: kettle_core::images::AnimationState {
                running: true,
                ..Default::default()
            },
            started,
        },
    );
    let active_while_hidden = super::visible_terminal_animation(&ws.mux);
    assert!(ws.mux.focus_pane(panes[0]));
    assert!(super::visible_terminal_animation(&ws.mux));
    assert_eq!(
        ws.mux.panes[&panes[0]].term.anims.lock().unwrap()[&1].started,
        started
    );
    let cfg = title_test_config();
    ws.mux
        .split_with(
            Dir::Horizontal,
            &cfg,
            80,
            24,
            8,
            16,
            Arc::new(|| {}),
            title_test_argv(),
            None,
        )
        .unwrap();
    ws.mux.toggle_zoom();
    let active_behind_zoom = super::visible_terminal_animation(&ws.mux);
    ws.mux.kill_children();
    assert!(
        !active_while_hidden,
        "inactive-tab animations must not schedule frames"
    );
    assert!(
        !active_behind_zoom,
        "animations hidden by zoom must not schedule frames"
    );
}

#[test]
fn animation_ticks_keep_remaining_deadline_between_turns() {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    let mut next = None;
    assert_eq!(
        super::terminal_animation_tick(true, now, &mut next),
        (true, Some(Duration::from_millis(33)))
    );
    assert_eq!(
        super::terminal_animation_tick(true, now + Duration::from_millis(10), &mut next),
        (false, Some(Duration::from_millis(23)))
    );
    assert_eq!(
        super::terminal_animation_tick(true, now + Duration::from_millis(33), &mut next),
        (true, Some(Duration::from_millis(33)))
    );
    assert_eq!(
        super::terminal_animation_tick(true, now + Duration::from_secs(10), &mut next),
        (true, Some(Duration::from_millis(33)))
    );
    assert_eq!(
        super::terminal_animation_tick(false, now + Duration::from_secs(10), &mut next),
        (false, None)
    );
    assert_eq!(next, None);
    assert!(super::terminal_animation_tick(true, now + Duration::from_secs(10), &mut next).0);
}

/// The typed-echo diagnostics must sample each pane's generation while the
/// frame's terminal lock is held (the staged generation predates the
/// capture), observe at every typed wake and after every present, and judge
/// the previous frame before a new one replaces it.
#[test]
fn typed_echo_diagnostics_sample_under_the_lock_and_observe_each_present() {
    let src = production_source();
    let capture = src
        .split_once("snaps[si].capture_with_card_marks(&g, p.inline_cards.needs_marks());")
        .expect("redraw captures pane snapshots")
        .1;
    let unlock = capture.find("drop(g);").expect("capture drops the lock");
    assert!(
        capture[..unlock].contains(".note_capture(*id, p.term.output_generation());"),
        "the frame's generation must be sampled under the terminal lock"
    );
    let presented = src
        .split_once("ws.output_pacer.presented();")
        .expect("redraw commits a presented frame")
        .1;
    let judged = presented
        .find("observe_typed_echo(ws, presented_at);")
        .expect("a present judges the previous typed frame");
    let watched = presented
        .find("ws.typed_echo.frame_presented(")
        .expect("a present records the new typed frame");
    assert!(judged < watched, "judge the previous frame before replacing it");
    assert!(src.contains(
        "defer_output_racing_frame(ws);\n            // Output published while this frame rendered supersedes it.\n            observe_typed_echo(ws, std::time::Instant::now());"
    ));
    assert!(src.contains(
        "ws.output_pacer.reset();\n                    observe_typed_echo(ws, now);\n                    let new_echo = note_typed_echo_wake(ws);"
    ));
}

/// A typed-text key into quiet `pane`.
fn typed_into(pane: u64) -> crate::typed_echo::KeyTarget {
    crate::typed_echo::KeyTarget {
        pane,
        typed_text: true,
        quiet: true,
    }
}

/// A window armed with a settled echo the way the typed Wakeup arm arms it:
/// pane 1's previous echo split, and this keystroke's echo just woke.
fn armed_echo_window(key: std::time::Instant) -> WindowState {
    use crate::typed_echo::EchoPaint;
    use std::time::Duration;
    let mut ws = WindowState::new(0, crate::mux::Mux::new());
    ws.typed_echo.key_sent(key, [typed_into(1)]);
    ws.typed_echo.typed_wake(Some(key), [(1, 11, Some(key))]);
    let second = key + Duration::from_millis(1);
    ws.typed_echo
        .observe(second, Some(key), |_| Some((12, Some(second))));
    let next = key + Duration::from_millis(50);
    ws.last_typed = Some(next);
    ws.typed_echo.key_sent(next, [typed_into(1)]);
    let new_echo = ws.typed_echo.typed_wake(Some(next), [(1, 13, Some(next))]);
    assert_eq!(
        ws.typed_echo.plan_paint(next, new_echo, true),
        EchoPaint::Settle
    );
    ws
}

#[test]
fn echo_settle_paints_once_at_an_unextended_deadline() {
    use crate::typed_echo::{ECHO_SETTLE, EchoPaint, SettlePoll};
    use std::time::{Duration, Instant};
    let key = Instant::now();
    let armed = key + Duration::from_millis(50);
    let mut ws = armed_echo_window(key);
    // A second wake 1 ms later (another pane) keeps the first deadline.
    let later = armed + Duration::from_millis(1);
    assert!(!ws.typed_echo.typed_wake(Some(armed), [(2, 3, Some(later))]));
    assert_eq!(
        ws.typed_echo.plan_paint(later, false, true),
        EchoPaint::Settle
    );
    assert_eq!(
        super::poll_echo_settle(&mut ws, later),
        SettlePoll::Wait(ECHO_SETTLE - Duration::from_millis(1))
    );
    assert!(ws.typed_echo.settle_pending());
    // Due: one request, then nothing more while it is queued.
    assert_eq!(
        super::poll_echo_settle(&mut ws, armed + ECHO_SETTLE),
        SettlePoll::Request
    );
    for turn in 1..4 {
        assert_eq!(
            super::poll_echo_settle(&mut ws, armed + ECHO_SETTLE + Duration::from_millis(turn)),
            SettlePoll::Idle,
            "a queued settle must not request or wake again"
        );
    }
    // The frame paints it.
    ws.typed_echo.redraw_started();
    ws.typed_echo.frame_sampled();
    assert!(!ws.typed_echo.settle_pending());
    assert_eq!(
        super::poll_echo_settle(&mut ws, armed + Duration::from_millis(10)),
        SettlePoll::Idle
    );
}

#[test]
fn echo_settle_neither_fires_nor_spins_while_hidden_or_recovering() {
    use crate::typed_echo::{ECHO_SETTLE, SettlePoll};
    use std::time::{Duration, Instant};
    let key = Instant::now();
    let due = key + Duration::from_millis(50) + ECHO_SETTLE;
    for blocked in ["hidden", "recovering"] {
        let mut ws = armed_echo_window(key);
        match blocked {
            "hidden" => ws.window_occluded = true,
            _ => ws.frame_recovery.schedule_transient_retry(due),
        }
        assert!(super::window_is_render_hidden(&ws) || ws.frame_recovery.has_pending());
        // Past the deadline: no redraw that would return early, and no
        // zero wait that would spin the loop.
        for turn in 0..3 {
            assert_eq!(
                super::poll_echo_settle(&mut ws, due + Duration::from_millis(turn)),
                SettlePoll::Idle,
                "{blocked}: the settle must cancel, not fire or wait"
            );
        }
        // Nor does it hold the blink handoff while the window cannot paint.
        assert!(!ws.typed_echo.settle_pending(), "{blocked}");
    }
}

#[test]
fn echo_settle_redraw_that_returns_early_is_not_retried_or_stuck() {
    use crate::typed_echo::{ECHO_SETTLE, EchoPaint, SettlePoll};
    use std::time::{Duration, Instant};
    let key = Instant::now();
    let due = key + Duration::from_millis(50) + ECHO_SETTLE;
    let mut ws = armed_echo_window(key);
    assert_eq!(super::poll_echo_settle(&mut ws, due), SettlePoll::Request);
    // The callback returns before acknowledging output (a DPI transition,
    // say): it consumed the request, and the next turns stay quiet.
    ws.typed_echo.redraw_started();
    for turn in 1..4 {
        assert_eq!(
            super::poll_echo_settle(&mut ws, due + Duration::from_millis(turn)),
            SettlePoll::Idle
        );
    }
    // The next keystroke's echo arms again.
    let next = due + Duration::from_millis(40);
    ws.last_typed = Some(next);
    ws.typed_echo.key_sent(next, [typed_into(1)]);
    let new_echo = ws.typed_echo.typed_wake(Some(next), [(1, 20, Some(next))]);
    assert_eq!(
        ws.typed_echo.plan_paint(next, new_echo, true),
        EchoPaint::Settle
    );

    // A request winit never answers does not hold later echoes back.
    let mut ws = armed_echo_window(key);
    assert_eq!(super::poll_echo_settle(&mut ws, due), SettlePoll::Request);
    let next = due + Duration::from_millis(40);
    ws.last_typed = Some(next);
    ws.typed_echo.key_sent(next, [typed_into(1)]);
    let new_echo = ws.typed_echo.typed_wake(Some(next), [(1, 20, Some(next))]);
    assert_eq!(
        ws.typed_echo.plan_paint(next, new_echo, true),
        EchoPaint::Now
    );
}

#[test]
fn echo_settle_is_wired_with_the_coalescer_guards() {
    let src = production_source();
    // The quiesce branch clears it with the coalescer.
    assert!(src.contains(
        "ws.output_pacer.reset();\n                    ws.typed_echo.quiesce();\n                    return;"
    ));
    // about_to_wait polls it, requests on Request, folds its wait, and holds
    // the blink handoff while it is pending.
    assert!(src.contains("let echo_settle = poll_echo_settle(ws, now);"));
    assert!(src.contains("|| coalesce_due\n            || echo_settle_due"));
    assert!(src.contains(
        "if let crate::typed_echo::SettlePoll::Wait(next) = echo_settle {"
    ));
    assert!(src.contains(
        "|| ws.output_pacer.is_deferred()\n                || ws.typed_echo.settle_pending()"
    ));
    // redraw answers the request on entry and consumes the settle where it
    // samples output, after every early return.
    let redraw = src
        .split_once("fn redraw(&mut self, ws: &mut WindowState) {")
        .expect("redraw")
        .1;
    let started = redraw
        .find("ws.typed_echo.redraw_started();")
        .expect("redraw answers a queued settle");
    let first_return = redraw.find("return;").expect("redraw has early returns");
    assert!(
        started < first_return,
        "the settle request must be answered before any early return"
    );
    assert!(redraw.contains(
        "self.acknowledge_output_wakes(ws);\n        // A settled echo paints in this frame, whatever requested it.\n        ws.typed_echo.frame_sampled();"
    ));
}

/// The settle is macOS only; every other platform paints echo at once.
#[test]
fn echo_settle_is_enabled_on_macos_only() {
    assert_eq!(super::ECHO_SETTLE_ENABLED, cfg!(target_os = "macos"));
}

/// Every keystroke stamp goes through `note_keystroke`, which reads the
/// pane's typed-text flag and quiet state per recipient; the echo wake keeps
/// only that keystroke's panes; and an idle settle poll returns before the
/// visibility guards.
#[test]
fn typed_echo_follows_the_keystrokes_own_panes() {
    let src = production_source();
    let note = src
        .split_once("fn note_keystroke(")
        .expect("keystrokes are noted in one place")
        .1;
    let note = &note[..note.find("\n}\n").expect("note_keystroke end")];
    assert!(note.contains("typed_text: pane.last_input_was_typed_text(),"));
    assert!(note.contains(
        "quiet: seen.get(id).copied().unwrap_or(0) == pane.term.output_generation(),"
    ));
    assert_eq!(
        src.matches("last_typed = Some(").count(),
        1,
        "only note_keystroke opens the typing window"
    );
    assert!(note.contains("ws.last_typed = Some(at);"));
    let wake = src
        .split_once("fn note_typed_echo_wake(")
        .expect("typed wakes are noted")
        .1;
    let wake = &wake[..wake.find("\n}\n").expect("note_typed_echo_wake end")];
    assert!(wake.contains("ws.typed_echo.typed_wake(\n        ws.last_typed,"));
    // Pastes and keys replayed after one are never typed text.
    for paste in [
        "Some(pane.feed_paste(&bytes))",
        "let result = pane.feed_paste(&bytes);",
        "let delivered = pane.feed_paste(bytes);",
    ] {
        assert!(src.contains(paste), "{paste}");
    }
    let poll = src
        .split_once("fn poll_echo_settle(")
        .expect("settle poll")
        .1;
    let idle = poll
        .find("if !ws.typed_echo.settle_pending() {")
        .expect("an idle settle returns early");
    let hidden = poll
        .find("window_is_render_hidden(ws)")
        .expect("the poll checks visibility");
    assert!(idle < hidden, "check idle before any visibility call");
}

/// Through the real keystroke helper on a real pane: a typed key into a quiet
/// pane learns its split and arms the next typed key; Enter, a paste, a
/// pane with unpainted output and a sibling recipient neither learn nor arm.
#[cfg(unix)]
#[test]
fn the_keystroke_helper_arms_only_typed_text_into_a_quiet_pane() {
    use crate::typed_echo::EchoPaint;
    use std::time::Duration;
    let cfg = kettle_config::Config {
        shell_integration: false,
        login_shell: false,
        ..kettle_config::Config::default()
    };
    let mut mux = Mux::new();
    mux.new_tab_with_geometry(
        &cfg,
        kettle_core::PtyGeometry::from_cell_size(80, 24, 8, 16),
        std::sync::Arc::new(|| {}),
        &["/bin/cat".to_owned()],
        None,
    )
    .expect("PTY fixture");
    let id = mux.active_focus().expect("pane");
    // Keep input off the PTY so the pane's output stays still.
    let _input = mux
        .panes
        .get_mut(&id)
        .expect("pane")
        .capture_input_for_test();
    let mut ws = WindowState::new(0, mux);
    let base = ws.mux.panes[&id].term.output_generation();
    ws.seen_output_gen.insert(id, base);
    let ms = Duration::from_millis;
    let send = |ws: &mut WindowState, bytes: &[u8], paste: bool, at| {
        let pane = &ws.mux.panes[&id];
        let result = if paste {
            pane.feed_paste(bytes)
        } else {
            pane.feed_input(bytes)
        };
        assert!(result.is_queued());
        super::note_keystroke(ws, at, &[id]);
    };
    let wake = |ws: &mut WindowState, at, generation| {
        let new_echo = ws.typed_echo.typed_wake(Some(at), [(id, generation, Some(at))]);
        ws.typed_echo.plan_paint(at, new_echo, true)
    };

    // A typed key whose echo splits teaches the pane.
    let key = std::time::Instant::now();
    send(&mut ws, b"g", false, key);
    assert_eq!(ws.last_typed, Some(key));
    assert_eq!(wake(&mut ws, key, base + 1), EchoPaint::Now);
    let second = key + ms(1);
    assert_eq!(
        ws.typed_echo
            .observe(second, Some(key), |_| Some((base + 2, Some(second)))),
        Some(id)
    );

    // Enter, a short unbracketed paste, and a sibling recipient: at once.
    let mut at = key;
    for (bytes, paste) in [(&b"\r"[..], false), (b"ab", true)] {
        at += ms(50);
        send(&mut ws, bytes, paste, at);
        assert_eq!(wake(&mut ws, at, base + 3), EchoPaint::Now, "{bytes:?}");
    }
    at += ms(50);
    assert!(ws.mux.panes[&id].feed_input(b"h").is_queued());
    super::note_keystroke(&mut ws, at, &[id + 1]);
    assert_eq!(wake(&mut ws, at, base + 3), EchoPaint::Now, "sibling");

    // A typed key while the pane has unpainted output: at once.
    ws.seen_output_gen.insert(id, base + 1);
    at += ms(50);
    send(&mut ws, b"h", false, at);
    assert_eq!(wake(&mut ws, at, base + 3), EchoPaint::Now, "streaming");
    assert!(!ws.typed_echo.settle_pending());

    // A typed key into the quiet, learned pane settles.
    ws.seen_output_gen.insert(id, base);
    at += ms(50);
    send(&mut ws, b"i", false, at);
    assert_eq!(wake(&mut ws, at, base + 3), EchoPaint::Settle);
    ws.mux.kill_children();
}
