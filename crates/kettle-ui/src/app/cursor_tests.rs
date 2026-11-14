#[test]
fn cached_cursor_blink_lookup_tracks_the_active_snapshot() {
    let keys = [(11, 1, 80, 24), (22, 2, 100, 30)];
    let mut snapshots = [PaneSnapshot::default(), PaneSnapshot::default()];
    snapshots[0].cursor_blinking = true;

    assert_eq!(
        cached_pane_cursor_blinking(Some(11), &keys, &snapshots),
        Some(true)
    );
    assert_eq!(
        cached_pane_cursor_blinking(Some(22), &keys, &snapshots),
        Some(false)
    );
    assert_eq!(
        cached_pane_cursor_blinking(Some(99), &keys, &snapshots),
        None
    );
    assert_eq!(
        cached_pane_cursor_blinking(Some(22), &keys, &snapshots[..1]),
        None
    );

    let live_reads = std::cell::Cell::new(0);
    let blink = pane_cursor_blinking_with(Some(22), &keys, &snapshots, true, |_| {
        live_reads.set(live_reads.get() + 1);
        Some(true)
    });
    assert!(!blink);
    assert_eq!(
        live_reads.get(),
        0,
        "validated snapshot lookup must not fall through to the live Term"
    );
}

#[test]
fn cursor_blink_stops_after_the_timeout_only_while_visible() {
    use std::time::Duration;
    let timeout = Some(Duration::from_secs(10));
    assert!(!super::cursor_blink_timed_out(
        Duration::from_secs(9),
        timeout,
        true
    ));
    assert!(super::cursor_blink_timed_out(
        Duration::from_secs(10),
        timeout,
        true
    ));
    // A hidden phase keeps going until the next toggle shows the cursor.
    assert!(!super::cursor_blink_timed_out(
        Duration::from_secs(60),
        timeout,
        false
    ));
    // `cursor-blink-timeout = 0` blinks for as long as the window is focused.
    assert!(!super::cursor_blink_timed_out(
        Duration::from_secs(3600),
        None,
        true
    ));
}

/// Drives the scheduler's two pure steps through 20 simulated seconds of
/// idle time: the blink must toggle until the timeout, stop within one
/// half-period after it, and rest with the cursor visible.
#[test]
fn an_idle_blink_stops_visible_within_one_half_period_of_the_timeout() {
    use std::time::Duration;
    let interval = Duration::from_millis(530);
    let timeout = Some(Duration::from_secs(10));
    let tick = Duration::from_millis(10);
    let mut now = Duration::ZERO;
    let (mut blink_on, mut last_blink) = (true, Duration::ZERO);
    let mut last_toggle = Duration::ZERO;
    while now < Duration::from_secs(20) {
        let timed_out = super::cursor_blink_timed_out(now, timeout, blink_on);
        let active = !timed_out && super::cursor_blink_active(true, true, true);
        if let Some(next) =
            super::next_cursor_blink_phase(active, now - last_blink, interval, blink_on)
        {
            blink_on = next;
            last_blink = now;
            last_toggle = now;
        }
        now += tick;
    }
    assert!(blink_on, "an idle blink must rest visible");
    assert!(
        last_toggle >= Duration::from_secs(10) - interval
            && last_toggle <= Duration::from_secs(10) + interval,
        "last toggle at {last_toggle:?}"
    );
}

#[test]
fn the_blink_scheduler_honors_the_idle_timeout_and_every_activity_source() {
    let production = super::production_source();
    assert_eq!(
        production.matches("last_blink_activity =").count(),
        1,
        "only reset_blink_phase may restart the blink timeout"
    );
    assert!(!production.contains("note_blink_output_activity"));
    // Every paste channel (keys, menu, middle click, right click, file
    // drop, broadcast) delivers through these two. A paste restarts the
    // blink even with the pointer, and without a keystroke.
    for delivery in ["fn deliver_text_paste(", "fn deliver_path_paste("] {
        let body = production
            .split_once(delivery)
            .expect("paste delivery")
            .1
            .split_once("\n    }\n")
            .expect("end of paste delivery")
            .0;
        assert!(
            body.contains("self.reset_blink_phase(ws);"),
            "{delivery} must restart the blink"
        );
    }
    let src = &super::app_source();
    let scheduler = src
        .split_once("let pane_blink = self.pane_cursor_blinking(ws, cached_menu_snapshots);")
        .expect("blink scheduler")
        .1
        .split_once("let blink_interval")
        .expect("end of blink gate")
        .0;
    assert!(scheduler.contains("cursor_blink_timed_out("));
    assert!(scheduler.contains("&& !blink_timed_out"));
    // The window server's blink replaces the scheduler's, never both.
    assert!(scheduler.contains("let layer_blink = ws.blink_layer.is_active();"));
    assert!(scheduler.contains("&& !layer_blink"));
    let reset = src
        .split_once("fn reset_blink_phase(&mut self, ws: &mut WindowState) {")
        .expect("reset_blink_phase")
        .1
        .split_once("\n    }\n")
        .expect("end of reset_blink_phase")
        .0;
    assert!(reset.contains("ws.last_blink_activity = now;"));
    // New activity ends a layer blink planned from the old activity.
    assert!(
        reset.contains("if ws.blink_layer.is_active()")
            && reset.contains("window.request_redraw();")
    );
    let toggle = src
        .split_once("Action::ToggleCursorBlink => {")
        .expect("blink toggle")
        .1
        .split_once("Action::ToggleCopyOnSelect")
        .expect("end of blink toggle")
        .0;
    assert!(toggle.contains("self.reset_blink_phase(ws);"));
    let reload = production
        .split_once(
            "fn apply_reloaded_config(&mut self, ws: &mut WindowState, font_size_changed: bool) {",
        )
        .expect("apply_reloaded_config")
        .1
        .split_once("\n    }\n")
        .expect("end of apply_reloaded_config")
        .0;
    assert!(reload.trim_start().starts_with("// A config change"));
    assert!(!reload.contains("fn reload_config("));
    assert!(reload.contains("self.reset_blink_phase(ws);"));
}

#[test]
fn cursor_blink_wakes_only_when_config_pane_and_focus_allow_it() {
    for configured in [false, true] {
        for pane_requests_blink in [false, true] {
            for focused in [false, true] {
                assert_eq!(
                    super::cursor_blink_active(configured, pane_requests_blink, focused),
                    configured && pane_requests_blink && focused
                );
            }
        }
    }
    let interval = std::time::Duration::from_millis(530);
    assert_eq!(
        super::next_cursor_blink_phase(
            true,
            interval - std::time::Duration::from_millis(1),
            interval,
            true
        ),
        None
    );
    assert_eq!(
        super::next_cursor_blink_phase(true, interval, interval, true),
        Some(false)
    );
    assert_eq!(
        super::next_cursor_blink_phase(true, interval, interval, false),
        Some(true)
    );
    assert_eq!(
        super::next_cursor_blink_phase(false, interval * 10, interval, true),
        None
    );
}

/// The scheduler hands the blink to the window server only at an edge
/// that hid the cursor, after it advanced the phase and recorded whether
/// the half-period was quiet, and before it requests the edge's redraw,
/// which becomes the hand-off frame.
#[test]
fn the_scheduler_hands_off_only_at_an_edge_that_hides_the_cursor() {
    let src = super::production_source();
    let body = src
        .split_once("fn about_to_wait_inner(")
        .expect("about_to_wait_inner")
        .1;
    let edge = body.find("ws.last_blink = now;").expect("blink edge");
    let quiet = body
        .find(".note_edge(now, crate::cursor_blink::quiet_window(blink_interval));")
        .expect("quiet half-period");
    let gate = body
        .find("crate::cursor_blink::handoff_allowed(")
        .expect("hand-off gate");
    let request = body
        .find("ws.blink_layer.request_handoff();")
        .expect("hand-off request");
    let redraw = body.find("|| blink_due").expect("blink redraw");
    assert!(edge < quiet && quiet < gate && gate < request && request < redraw);
    let eligibility = src
        .split_once("fn pane_cursor_layer_eligible(")
        .expect("cursor eligibility")
        .1
        .split_once("fn pane_cursor_blinking(")
        .expect("end of eligibility")
        .0;
    for needle in [
        "ws.vi_mode.is_none()",
        "TermMode::VI",
        "CursorShape::Hidden",
        "content.display_offset",
        ".contains(&row)",
    ] {
        assert!(
            eligibility.contains(needle),
            "cursor eligibility lost {needle}"
        );
    }
    let start = src
        .split_once("fn start_layer_blink(")
        .expect("start layer")
        .1
        .split_once("fn hide_cursor_layer(")
        .expect("end of start layer")
        .0;
    assert!(start.contains("ws.blink_layer.failed_handoff();"));
    assert!(body[..gate].contains("&& Self::pane_cursor_layer_eligible(ws);"));
    let inputs = &body[gate..request];
    for needle in [
        "self.cfg.macos_cursor_blink_layer",
        "edge_hides_cursor: next_blink == Some(false),",
        "blink_active,",
        "cursor_eligible,",
        "|| pending_video_receipt_wait.is_some()",
        "quiet: blink_quiet,",
        "ime_preedit: ws.ime_preedit.is_some(),",
        "modal_steady: cursor_held_steady(ws),",
        "|| ws.accessibility_pending",
        "is_some_and(Renderer::has_pending_screenshot)",
    ] {
        assert!(inputs.contains(needle), "the hand-off gate lost {needle:?}");
    }
}

/// A frame ends the window server's blink only when it presents: the
/// layer hides in the `Presented` arm, inside the transaction that
/// `begin_exit_frame` opens before the render call and `end_exit_frame`
/// commits after the outcome match, so the frame and the hide land
/// together. The phase is materialized before anything reads it.
#[test]
fn a_layer_blink_ends_only_with_a_presented_frame() {
    let src = super::production_source();
    let redraw = src
        .split_once("fn redraw(&mut self, ws: &mut WindowState) {")
        .expect("redraw")
        .1
        .split_once("\n    }\n")
        .expect("end of redraw")
        .0;
    assert!(
        redraw
            .trim_start()
            .starts_with("// A frame while the window server blinks")
    );
    assert!(redraw.contains("Self::materialize_layer_blink(ws, std::time::Instant::now());"));
    let handoff = redraw
        .split_once("let blink_handoff =")
        .expect("redraw handoff recheck")
        .1;
    assert!(
        handoff
            .split_once("let Some(renderer) = ws.renderer.as_mut()")
            .expect("renderer borrow")
            .0
            .contains("&& self.cfg.macos_cursor_blink_layer")
    );
    let phase = src
        .split_once("fn materialize_layer_blink(")
        .expect("materialize helper")
        .1
        .split_once("fn ensure_cursor_layer(")
        .expect("end of materialize helper")
        .0;
    assert!(phase.contains(".materialize(ws.blink_on, ws.last_blink, now)"));
    let begin = redraw
        .find("layer.begin_exit_frame();")
        .expect("exit frame opens");
    let render = redraw
        .find("render_frame_with_status_and_pre_present(")
        .expect("render call");
    let presented = redraw
        .find("FrameOutcome::Presented => {")
        .expect("presented arm");
    let retry = redraw
        .find("FrameOutcome::RetryLater => {")
        .expect("retry arm");
    let lost = redraw
        .find("FrameOutcome::SurfaceLost => {")
        .expect("lost arm");
    let end = redraw
        .find("layer.end_exit_frame();")
        .expect("exit frame closes");
    assert!(begin < render && render < presented && presented < retry && lost < end);
    assert_eq!(redraw.matches("layer.hide();").count(), 1);
    let hide = redraw.find("layer.hide();").expect("hide");
    assert!(
        presented < hide && hide < retry,
        "hide() only for a presented frame"
    );
    let record = redraw
        .find("ws.blink_layer.exited(exit_time);")
        .expect("total exit time");
    let start = redraw
        .find("let exit_started =")
        .expect("exit timer starts");
    let measured = redraw
        .find("let exit_time = exit_started.elapsed();")
        .expect("exit timer ends");
    assert!(start < begin && end < measured && measured < record);
    assert!(redraw.contains("let exit_time = exit_started.elapsed();"));
    assert!(redraw[record..].contains("Self::retire_cursor_layer(ws);"));
    let reload = src
        .split_once("fn apply_reloaded_config(")
        .expect("config reload")
        .1
        .split_once("\n    }\n")
        .expect("end of config reload")
        .0;
    assert!(reload.contains("!self.cfg.macos_cursor_blink_layer && !ws.blink_layer.is_active()"));
    assert_eq!(
        redraw
            .matches("ws.blink_layer.take_pending_handoff()")
            .count(),
        1
    );
}

/// C1 can retain old layer contents after failed patch presentation.
/// Only the latest Presented patch may show, and Lost must reattach later.
#[test]
fn cursor_patch_uses_the_presented_scene_and_hides_failed_contents() {
    let src = super::production_source();
    let redraw = src
        .split_once("fn redraw(&mut self, ws: &mut WindowState) {")
        .expect("redraw")
        .1
        .split_once("\n    }\n")
        .expect("end redraw")
        .0;
    let render = redraw
        .find("let frame_result = renderer.render_frame_with_status_and_pre_present(")
        .expect("render");
    let gate = redraw
        .find("matches!(&frame_result, Ok(FrameOutcome::Presented))")
        .expect("present gate");
    let patch = redraw
        .find("renderer.present_cursor_patch(&self.cfg)")
        .expect("patch");
    let outcome = redraw.find("match frame_result {").expect("outcome");
    assert!(render < gate && gate < patch && patch < outcome);
    assert!(!redraw[render..patch].contains("renderer.set_"));
    let start = src
        .split_once("fn start_layer_blink(")
        .expect("start")
        .1
        .split_once("fn hide_cursor_layer(")
        .expect("end start")
        .0;
    let hide = start.find("layer.hide();").expect("hide old contents");
    let result = start.find("match patch {").expect("patch outcome");
    let show = start.find("match layer.show(").expect("show");
    assert!(hide < result && result < show);
    assert!(start.contains("CursorPatchOutcome::Presented(rect) => rect,"));
    let ineligible = start
        .split_once("CursorPatchOutcome::Ineligible(reason) => {")
        .expect("ineligible")
        .1
        .split_once("CursorPatchOutcome::Failed(reason) => {")
        .expect("failure");
    assert!(ineligible.0.contains("return;"));
    let failed = ineligible.1.split_once("let rect_px =").expect("rect").0;
    assert!(failed.contains("return;"));
    assert!(failed.contains("if reason == CursorPatchFailure::Lost {\n                    Self::retire_cursor_layer(ws);"));
    assert!(start.contains("let rect_px = [rect.x, rect.y, rect.width, rect.height];"));
    let retire = src
        .split_once("fn retire_cursor_layer(")
        .expect("retire")
        .1
        .split_once("\n    }\n")
        .expect("end retire")
        .0;
    assert!(
        retire.find("Self::hide_cursor_layer(ws);").expect("hide")
            < retire
                .find("renderer.detach_cursor_layer();")
                .expect("detach")
    );
    assert!(retire.contains("let layer = ws.cursor_layer.take();"));
}

/// Idle blink must wake at the configured half-period deadline, not at a
/// fixed sub-interval. A 120 ms poll would request four mostly-no-op
/// redraws before each default 530 ms blink toggle.
#[test]
fn cursor_blink_waits_until_the_actual_deadline() {
    let src = production_source();
    let start = src
        .find("fn about_to_wait_inner(")
        .expect("about_to_wait_inner present");
    let rest = &src[start..];
    let end = rest.find("\n}\n").map(|i| i + 2).unwrap_or(rest.len());
    let body = &rest[..end];
    assert!(
        body.contains("next_cursor_blink_phase(")
            && body.contains("let blink_due = next_blink.is_some()"),
        "about_to_wait must request a blink redraw only when the half-period elapsed"
    );
    let blink = body
        .find("let next_blink =")
        .expect("blink phase scheduling must exist");
    let blink_body = &body[blink..];
    assert!(
        blink_body.find("ws.last_blink = now") < blink_body.find("w.request_redraw()"),
        "advance the blink deadline before requesting a compositor frame so repeated \
             about_to_wait callbacks cannot enqueue the same blink"
    );
    let wait = body
        .find(
            "crate::cursor_blink::wait_for_edge(blink_active, ws.last_blink, blink_interval, now);",
        )
        .expect("the blink wake is the time left to the next edge, not a fixed poll");
    assert!(!body.contains("Some(120)"));
    assert!(
        blink + blink_body.find("ws.last_blink = now").expect("blink edge") < wait,
        "the wake counts from the edge this pass recorded"
    );
    assert!(
        !body.contains("blink_interval.saturating_sub("),
        "a half-period from now would wake in the middle of a restart's hold"
    );
}

/// One scheduler pass at `now` over a window's blink state: the pure rules
/// `about_to_wait_inner` applies, returning the sleep it would ask for.
fn blink_pass(
    ws: &mut WindowState,
    configured: bool,
    interval: std::time::Duration,
    timeout: Option<std::time::Duration>,
    now: std::time::Instant,
) -> Option<std::time::Duration> {
    let timed_out = super::cursor_blink_timed_out(
        now.saturating_duration_since(ws.last_blink_activity),
        timeout,
        ws.blink_on,
    );
    let active = !timed_out && super::cursor_blink_active(configured, true, true);
    if let Some(next) = super::next_cursor_blink_phase(
        active,
        now.saturating_duration_since(ws.last_blink),
        interval,
        ws.blink_on,
    ) {
        ws.blink_on = next;
        ws.last_blink = now;
    }
    crate::cursor_blink::wait_for_edge(active, ws.last_blink, interval, now)
}

/// Every restart of the blink holds the cursor visible for one whole period:
/// `restart_blink` is the only writer that shows the cursor, `reset_blink_phase`
/// and the DEC mode 12 path both go through it, and from a window's state it
/// leaves, the scheduler's rules draw the hold and then the old cadence.
#[test]
fn every_blink_restart_holds_the_cursor_for_a_whole_period() {
    use std::time::{Duration, Instant};
    let ms = Duration::from_millis(1);
    let half = Duration::from_millis(kettle_config::Config::default().cursor_blink_interval);
    assert_eq!(half, ms * 530);
    let timeout = kettle_config::Config::default().cursor_blink_timeout();
    let t0 = Instant::now();
    let mut ws = WindowState::new(1, Mux::new());
    // Mid off phase, an edge 200 ms ago.
    ws.blink_on = false;
    ws.last_blink = t0 - ms * 200;
    super::restart_blink(&mut ws, t0, half);
    ws.last_blink_activity = t0;
    assert!(ws.blink_on);
    assert_eq!(ws.last_blink, t0 + half);
    assert_eq!(
        blink_pass(&mut ws, true, half, timeout, t0),
        Some(half * 2),
        "one wake, at the end of the hold"
    );
    for at in 0..=3180u32 {
        blink_pass(&mut ws, true, half, timeout, t0 + ms * at);
        let expected = match at {
            0..1060 => true,
            1060..1590 => false,
            1590..2120 => true,
            2120..2650 => false,
            2650..3180 => true,
            _ => false,
        };
        assert_eq!(ws.blink_on, expected, "{at} ms after the restart");
    }
    // A second restart inside a hold starts the hold again.
    let again = t0 + ms * 4000;
    super::restart_blink(&mut ws, again, half);
    for at in 0..1760u32 {
        if at == 700 {
            super::restart_blink(&mut ws, again + ms * at, half);
        }
        blink_pass(&mut ws, true, half, timeout, again + ms * at);
        assert!(ws.blink_on, "{at} ms after the first of two restarts");
    }
    blink_pass(&mut ws, true, half, timeout, again + ms * 1760);
    assert!(!ws.blink_on);

    let src = super::production_source();
    assert_eq!(
        src.matches("ws.blink_on = true;").count(),
        1,
        "only restart_blink shows the cursor"
    );
    // restart_blink, the scheduler's edge and the layer's materialized edge.
    assert_eq!(src.matches("ws.last_blink = ").count(), 3);
    let restart = finding_body(&src, "restart_blink");
    assert!(restart.contains("ws.last_blink = crate::cursor_blink::restart_edge(now, interval);"));
    let reset: String = finding_body(&src, "reset_blink_phase")
        .split_whitespace()
        .collect();
    assert!(
        reset.contains(
            "restart_blink(ws,now,std::time::Duration::from_millis(self.cfg.cursor_blink_interval)"
        ),
        "{reset}"
    );
    // DEC mode 12: the drain reports the flip and its caller restarts the
    // phase with the configured interval. It is output, so the idle timeout
    // keeps counting (`last_blink_activity` has its one writer above).
    let drain = src
        .split_once("fn drain_pane_events(")
        .expect("pane drain")
        .1
        .split_once("\n}\n")
        .expect("end of pane drain")
        .0;
    assert!(drain.contains("cursor_blinking_changed = true;"));
    assert!(!drain.contains("ws.last_blink") && !drain.contains("ws.blink_on"));
    let app_drain: String = finding_body(&src, "drain_events")
        .split_whitespace()
        .collect();
    let flipped = app_drain
        .split_once("ifcursor_blinking_changed{")
        .expect("the drain's caller handles the flip")
        .1;
    assert!(
        flipped.starts_with(
            "restart_blink(ws,std::time::Instant::now(),\
             std::time::Duration::from_millis(self.cfg.cursor_blink_interval)"
        ),
        "{flipped}"
    );
}

/// `cursor-blink = false` is untouched by the hold: a restart schedules no
/// edge and no wake, the phase never leaves visible, and the frame draws the
/// cursor solid whatever the phase.
#[test]
fn a_restart_with_cursor_blink_off_schedules_nothing() {
    use std::time::{Duration, Instant};
    let ms = Duration::from_millis(1);
    let half = ms * 530;
    let timeout = Some(Duration::from_secs(10));
    let t0 = Instant::now();
    let mut ws = WindowState::new(1, Mux::new());
    super::restart_blink(&mut ws, t0, half);
    ws.last_blink_activity = t0;
    for at in (0..15_000u32).step_by(10) {
        assert_eq!(
            blink_pass(&mut ws, false, half, timeout, t0 + ms * at),
            None,
            "a wake {at} ms after the restart"
        );
        assert!(ws.blink_on);
        assert_eq!(ws.last_blink, t0 + half);
    }
    assert_eq!(
        super::shown_blink_phase(
            &ws,
            || super::cursor_blink_active(false, true, true),
            half,
            timeout,
            t0 + ms * 700,
        ),
        (true, None)
    );
    let src = super::production_source();
    let frame: String = src
        .split_once("let blink_enabled = self.cfg.cursor_blink && pane_blink;")
        .expect("the frame's blink gate")
        .1
        .split_once("ws.blink_on")
        .expect("the frame's phase read")
        .0
        .split_whitespace()
        .collect();
    assert_eq!(
        frame,
        "letcursor_visible=if!blink_enabled||!window_focused||cursor_held_steady(ws){true}else{"
    );
}

/// `ui_geometry.cursor_blink` reports a hold as the visible phase with its
/// next edge at the hold's end, then each phase with the edge that ends it,
/// whether the scheduler or the window server draws the blink.
#[test]
fn blink_diagnostics_report_the_hold_and_the_phases_after_it() {
    use std::time::{Duration, Instant};
    let ms = Duration::from_millis(1);
    let half = ms * 530;
    let timeout = Some(Duration::from_secs(10));
    let t0 = Instant::now();
    let mut ws = WindowState::new(1, Mux::new());
    super::restart_blink(&mut ws, t0, half);
    ws.last_blink_activity = t0;
    for at in 0..3180u32 {
        let now = t0 + ms * at;
        blink_pass(&mut ws, true, half, timeout, now);
        let (on, next_ms) = match at {
            0..1060 => (true, 1060),
            1060..1590 => (false, 1590),
            1590..2120 => (true, 2120),
            2120..2650 => (false, 2650),
            _ => (true, 3180),
        };
        assert_eq!(
            super::shown_blink_phase(&ws, || true, half, timeout, now),
            (on, Some(t0 + ms * next_ms)),
            "{at} ms after the restart"
        );
        // Blink off, an unfocused or a hidden window: the phase, no edge.
        assert_eq!(
            super::shown_blink_phase(&ws, || false, half, timeout, now),
            (on, None)
        );
    }

    // The window server takes the blink at the end of a hold. The report then
    // follows its plan with no scheduler pass at all.
    let mut ws = WindowState::new(1, Mux::new());
    super::restart_blink(&mut ws, t0, half);
    ws.last_blink_activity = t0;
    blink_pass(&mut ws, true, half, timeout, t0 + ms * 1060);
    let plan = crate::cursor_blink::layer_plan(
        ws.blink_on,
        ws.last_blink,
        ws.last_blink_activity,
        half,
        timeout,
    )
    .expect("the end of a hold hides the cursor");
    ws.blink_layer.started(plan, [0, 0, 8, 16]);
    for (at, on, next_ms) in [
        (1060u32, false, 1590u32),
        (1589, false, 1590),
        (1590, true, 2120),
        (2119, true, 2120),
        (2120, false, 2650),
    ] {
        assert_eq!(
            super::shown_blink_phase(
                &ws,
                || unreachable!("the layer's plan needs no scheduler"),
                half,
                timeout,
                t0 + ms * at,
            ),
            (on, Some(t0 + ms * next_ms)),
            "{at} ms after the restart, on the layer"
        );
    }
    // A restart while the layer blinks: until its exit frame presents, the
    // report is already the restart's hold, not the layer's old plan.
    let restart = t0 + ms * 2300;
    super::restart_blink(&mut ws, restart, half);
    assert!(ws.blink_layer.is_active());
    assert_eq!(
        super::shown_blink_phase(&ws, || true, half, timeout, restart + ms * 900),
        (true, Some(restart + half * 2))
    );
    // The control reply is this function's answer, taken once per read.
    let src = super::production_source();
    let diagnostics = finding_body(&src, "cursor_blink_diagnostics");
    assert_eq!(diagnostics.matches("shown_blink_phase(").count(), 1);
    assert!(diagnostics.contains("let (phase_on, next_edge) = shown_blink_phase("));
    assert!(!diagnostics.contains("cursor_blink::"));
}

#[test]
fn background_frames_do_not_delay_cursor_blink() {
    use std::time::Duration;
    assert_eq!(
        super::animation_wait(false, Some(1000), Some(Duration::from_millis(330))),
        Some(Duration::from_millis(330)),
        "a later background frame must not delay the cursor edge"
    );
    assert_eq!(
        super::animation_wait(false, Some(100), Some(Duration::from_millis(530))),
        Some(Duration::from_millis(100))
    );
    assert_eq!(
        super::animation_wait(true, Some(1000), Some(Duration::from_millis(5))),
        Some(Duration::from_millis(5))
    );
    assert_eq!(super::animation_wait(false, None, None), None);
}


/// A selection drag holds the cursor solid, as a modal's input bar does,
/// and nothing else about the pointer does: the frame's cursor and the
/// window server's hand-off both ask `cursor_held_steady`.
#[test]
fn a_selection_drag_holds_the_cursor_solid() {
    let mut ws = WindowState::new(1, Mux::new());
    assert!(!super::cursor_held_steady(&ws));
    super::pointer::arm_selection_gesture(&mut ws, 7, 0);
    assert!(super::cursor_held_steady(&ws), "a drag in progress");
    super::pointer::clear_selection_gesture(&mut ws);
    assert!(!super::cursor_held_steady(&ws), "the drag ended");
    // Hovering, scrolling or a link under the pointer holds nothing.
    ws.scrollbar_hover = Some(7);
    ws.selection_dragged = true;
    assert!(!super::cursor_held_steady(&ws));
    // A modal still holds it, with or without a drag.
    ws.search.open = true;
    assert!(super::cursor_held_steady(&ws));
    let src = super::production_source();
    let redraw = src
        .split_once("let cursor_visible = if")
        .expect("the frame's cursor")
        .1
        .split_once('{')
        .expect("end of the condition")
        .0;
    assert!(redraw.contains("|| cursor_held_steady(ws)"), "{redraw}");
}

/// A pointer press in the terminal restarts the cursor blink from its
/// visible phase, natively and over ctl, whoever takes the press (a program
/// reporting the mouse, a selection, the context menu), and the release
/// that ends a selection drag restarts it again. Pointer motion and the
/// wheel never touch the blink; only a focus change they cause does.
#[test]
fn a_terminal_press_restarts_the_blink_and_hover_and_the_wheel_do_not() {
    let src = super::production_source();
    let press = |body: &str, label: &str| {
        let after_focus = body
            .split_once(".focus_at(area,")
            .unwrap_or_else(|| panic!("{label}: the terminal press"))
            .1;
        let (before_report, _) = after_focus
            .split_once("if self.send_mouse(ws, bcode, true, false)")
            .unwrap_or_else(|| panic!("{label}: the mouse report"));
        assert!(
            before_report.contains("self.reset_blink_phase(ws);"),
            "{label}: a terminal press must restart the blink"
        );
    };
    let native = src
        .split_once("state: ElementState::Pressed,")
        .expect("the native press")
        .1
        .split_once("state: ElementState::Released,")
        .expect("end of the native press")
        .0;
    press(native, "native");
    press(finding_body(&src, "ctl_mouse_press"), "ctl");
    let finish = finding_body(&src, "finish_selection_gesture");
    let reset = finish
        .find("self.reset_blink_phase(ws);")
        .expect("the end of a drag restarts the blink");
    assert!(finish[..reset].contains("if ws.selecting {"));
    assert!(reset < finish.find("clear_selection_gesture(ws);").unwrap());
    let moved = src
        .split_once("WindowEvent::CursorMoved { position, .. } => {")
        .expect("pointer motion")
        .1
        .split_once("WindowEvent::MouseInput {")
        .expect("end of pointer motion")
        .0;
    let wheel = src
        .split_once("WindowEvent::MouseWheel { delta, phase, .. } => {")
        .expect("the wheel")
        .1;
    let wheel = &wheel[..wheel.find("\n            WindowEvent::").unwrap_or(wheel.len())];
    for (label, body) in [
        ("motion", moved),
        ("the wheel", wheel),
        ("dispatch_wheel", finding_body(&src, "dispatch_wheel")),
        ("ctl_mouse_move", finding_body(&src, "ctl_mouse_move")),
    ] {
        assert!(
            !body.contains("reset_blink_phase"),
            "{label} must leave the blink alone"
        );
    }
}
