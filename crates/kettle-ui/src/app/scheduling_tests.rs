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

