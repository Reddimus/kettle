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
        "modal_steady: modal_holds_cursor_steady(ws),",
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
    assert!(
        body.contains("blink_interval.saturating_sub(blink_elapsed)")
            && !body.contains("Some(120)"),
        "blink wait time must be the remaining configured interval, not a fixed poll"
    );
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

