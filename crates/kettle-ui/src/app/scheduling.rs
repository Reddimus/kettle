use super::*;

pub(super) fn render_hidden_from_observations(
    occluded: bool,
    minimized: bool,
    window_shown: bool,
    visible: bool,
) -> bool {
    occluded || minimized || (window_shown && !visible)
}

/// Whether a theme schedule has a decision that the last redraw did not apply.
pub(super) fn theme_schedule_redraw_due(last: Option<bool>, now: Option<bool>) -> bool {
    now.is_some() && last != now
}

/// The wait until the next whole wall-clock minute, when a clock or sunrise
/// schedule can next flip; never zero.
pub(super) fn until_next_wall_minute() -> std::time::Duration {
    let into_minute = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() % 60_000)
        .unwrap_or(0) as u64;
    std::time::Duration::from_millis(60_000 - into_minute)
}

pub(super) fn window_is_render_hidden(ws: &WindowState) -> bool {
    ws.window.as_ref().is_some_and(|window| {
        render_hidden_from_observations(
            ws.window_occluded,
            window.is_minimized().unwrap_or(false),
            ws.window_shown,
            window.is_visible().unwrap_or(true),
        )
    }) || ws.window_occluded
}

/// Fallback output frame budget when the platform cannot identify the current
/// monitor's refresh rate. This is the ceiling-rounded period of a 60 Hz
/// display; a flat 16 ms would schedule slightly faster than a 60 Hz
/// compositor can present.
pub(super) const OUTPUT_FRAME_BUDGET: std::time::Duration =
    std::time::Duration::from_nanos(16_666_667);
const MIN_OUTPUT_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(4);
const MAX_OUTPUT_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_nanos(33_333_334);
/// Small dispatch/preparation lead so a deadline does not first become runnable
/// exactly at vblank and miss the compositor's next present opportunity.
/// Deliberately sub-millisecond; comparative live validation still decides
/// whether a backend/display needs a different policy.
const OUTPUT_PRESENT_HEADROOM: std::time::Duration = std::time::Duration::from_micros(250);
pub(super) const MONITOR_REFRESH_PROBE_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(250);
const MID_FLOOD_MIN_BUDGET: std::time::Duration = std::time::Duration::from_nanos(16_666_667);
pub(super) const MID_FLOOD_MAX_BUDGET: std::time::Duration =
    std::time::Duration::from_nanos(33_333_334);
const DEEP_FLOOD_MIN_BUDGET: std::time::Duration = std::time::Duration::from_nanos(33_333_334);
pub(super) const DEEP_FLOOD_MAX_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

/// Convert winit's monitor refresh rate (millihertz) into a per-window output
/// deadline one small dispatch lead before the frame period. The 4–33.33 ms
/// bounds cover 30–250 Hz displays while
/// preventing an implausible/driver-corrupt mode from creating a busy loop or
/// making output feel stalled. A window updates this whenever it is created,
/// moved, resized, or changes DPI, so two Kettle windows on different monitors
/// can pace independently.
pub(super) fn output_frame_budget_for_refresh(
    refresh_millihertz: Option<u32>,
) -> std::time::Duration {
    let Some(refresh_millihertz) = refresh_millihertz.filter(|rate| *rate > 0) else {
        return OUTPUT_FRAME_BUDGET;
    };
    const NANOS_PER_MILLIHERTZ_PERIOD: u64 = 1_000_000_000_000;
    let nanos = NANOS_PER_MILLIHERTZ_PERIOD.div_ceil(u64::from(refresh_millihertz));
    std::time::Duration::from_nanos(nanos)
        .saturating_sub(OUTPUT_PRESENT_HEADROOM)
        .clamp(MIN_OUTPUT_FRAME_BUDGET, MAX_OUTPUT_FRAME_BUDGET)
}

pub(super) fn monitor_refresh_probe_wait(
    last_probe: Option<std::time::Instant>,
    now: std::time::Instant,
    force: bool,
) -> Option<std::time::Duration> {
    if force {
        return None;
    }
    last_probe
        .map(|last| {
            MONITOR_REFRESH_PROBE_INTERVAL.saturating_sub(now.saturating_duration_since(last))
        })
        .filter(|remaining| !remaining.is_zero())
}

/// Whether an output-driven repaint should be DEFERRED
/// (coalesced) rather than painted now — true when the previous frame painted
/// less than `budget` ago. Capping PTY-output paints to one per budget lets a
/// non-atomic repaint burst (an app that doesn't bracket frames with DEC 2026
/// synchronized output, e.g. Claude Code) settle before kettle snapshots the
/// grid, avoiding the transient mid-repaint cursor jump. Pure (drift-tested in
/// `output_paint_coalesces_within_frame_budget`).
pub(super) fn should_defer_output_paint(
    now: std::time::Instant,
    last_paint: Option<std::time::Instant>,
    budget: std::time::Duration,
) -> bool {
    match last_paint {
        Some(t) => now.saturating_duration_since(t) < budget,
        None => false,
    }
}

/// The output-paint budget GROWS under a sustained flood.
/// Each output-driven frame that had to be coalesced — i.e. output arriving
/// faster than the monitor-derived base budget — bumps the window's
/// `flood_paints` counter. A high-refresh window steps down no further than
/// 60 fps and then 30 fps; a 60 Hz window retains the established 30/20 fps
/// backoff. On-screen content during a flood is unreadable scrolling anyway,
/// and every frame NOT painted is one fewer O(cells) `PaneSnapshot::capture`
/// taken under the pane's `Term` mutex — the SAME mutex the PTY reader thread
/// must hold to run `Processor::advance`. At 60 fps the main thread grabs that
/// lock ~60×/s under flood, throttling the parser on a CPU-contended box;
/// stretching the budget hands the lock (and the cores) back to the reader, so
/// flood throughput rises. A brief burst (< 4 coalesced frames) never
/// throttles, so keystroke echo and short bursts stay at the monitor rate, and the
/// counter resets the instant output drops below the budget (see `redraw`), so
/// the settled post-flood frame paints within one budget. Pure; drift-tested in
/// `effective_output_budget_grows_under_sustained_flood`.
pub(super) fn effective_output_budget(
    base: std::time::Duration,
    flood_paints: u32,
) -> std::time::Duration {
    match flood_paints {
        0..=3 => base,
        4..=15 => base
            .saturating_mul(2)
            .clamp(MID_FLOOD_MIN_BUDGET, MID_FLOOD_MAX_BUDGET),
        _ => base
            .saturating_mul(3)
            .clamp(DEEP_FLOOD_MIN_BUDGET, DEEP_FLOOD_MAX_BUDGET),
    }
}

pub(super) fn animation_wait(
    animation_active: bool,
    background_ms: Option<u64>,
    blink_wait: Option<std::time::Duration>,
) -> Option<std::time::Duration> {
    [
        animation_active.then(|| std::time::Duration::from_millis(33)),
        background_ms.map(|ms| std::time::Duration::from_millis(ms.clamp(16, 1000))),
        blink_wait,
    ]
    .into_iter()
    .flatten()
    .min()
}

/// Whether terminal-owned transient chrome may be projected. Keeping this
/// predicate shared prevents a hidden receipt from retaining a hover pause
/// behind a modal surface.
pub(super) fn terminal_surface_available(ws: &WindowState) -> bool {
    ws.context_menu.is_none()
        && ws.vi_mode.is_none()
        && ws.hint_state.is_none()
        && ws.palette_input.is_none()
        && ws.settings_nav.is_none()
        && ws.layout_picker_input.is_none()
        && ws.theme_picker_input.is_none()
        && ws.ssh_input.is_none()
        && ws.confirm_dialog.is_none()
        && ws.editing_title.is_none()
        && !ws.search.open
        && ws.preview_overlay.is_none()
}

impl App {
    pub(super) fn sync_output_frame_budget(&self, ws: &mut WindowState, force: bool) {
        let Some(window) = ws.window.clone() else {
            ws.output_monitor = None;
            ws.output_frame_budget = OUTPUT_FRAME_BUDGET;
            return;
        };
        // `current_monitor()` is the cheap identity probe. Querying the mode's
        // refresh rate is the expensive part on Windows, so same-monitor move
        // and resize storms share the rate limiter while a true same-DPI
        // monitor transition refreshes immediately.
        let monitor = window.current_monitor();
        let monitor_changed = monitor != ws.output_monitor;
        if monitor_changed {
            ws.output_monitor = monitor.clone();
        }
        let force = force || monitor_changed;
        let now = std::time::Instant::now();
        if monitor_refresh_probe_wait(ws.output_refresh_probe_at, now, force).is_some() {
            // Preserve one trailing probe so a quick same-DPI move from a
            // 60 Hz to a 165 Hz monitor cannot be lost to the rate limiter.
            ws.output_refresh_probe_pending = true;
            return;
        }
        ws.output_refresh_probe_at = Some(now);
        ws.output_refresh_probe_pending = false;
        let refresh_millihertz = monitor.and_then(|monitor| monitor.refresh_rate_millihertz());
        let next = output_frame_budget_for_refresh(refresh_millihertz);
        if next != ws.output_frame_budget {
            log::debug!(
                "window {} output pacing changed: refresh={refresh_millihertz:?} mHz, \
                 budget={next:?}",
                ws.seq
            );
            ws.output_frame_budget = next;
            // A monitor move begins a new pacing regime. Do not carry the old
            // monitor's sustained-flood tier into the first frame on the new
            // display.
            ws.flood_paints = 0;
        }
    }

    /// Returns the absolute wake-up deadline this window wants
    /// (`None` = wait for events). The dispatch wrapper merges every window's
    /// request to the earliest deadline and sets the control flow once.
    pub(super) fn about_to_wait_inner(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
    ) -> Option<std::time::Instant> {
        if take_window_lifecycle_wakes(ws) {
            self.drain_events(ws);
        }
        #[cfg(windows)]
        if ws
            .mux
            .panes
            .values()
            .any(|pane| !pane.exit_observed && pane.term.direct_child_exit_at().is_some())
        {
            // Direct-child timestamps remain level-triggered through both
            // deadlines. This runs even for hidden windows and does not depend
            // on output or cursor animation to keep the policy clock moving.
            self.drain_events(ws);
        }
        self.process_pending_pane_restarts(ws);
        let held_child_reap_pending = ws.mux.poll_held_child_statuses();
        // A log worker can fail after PTY output becomes silent. Its direct
        // event-loop wake reaches this poll even when output-generation gating
        // correctly decides there is no frame to paint.
        self.sync_session_log_failures(ws);
        // Drain trailing recorder output before reap removes a
        // just-exited pane (covers the shell-exit → process-exit close path,
        // e.g. a fast `-e cmd` session).
        self.flush_recorder_output(ws);
        let reaped = ws.mux.reap();
        if reaped.layout_changed {
            ws.dragging_split = None;
        }
        self.fire_closed_pane_events(ws);
        // Binding, not an inline call on the right of `||`: see the matching
        // reap in `redraw`.
        let stale_title = clear_stale_title_edit(&ws.mux, &mut ws.editing_title);
        if reaped.layout_changed || stale_title {
            ws.pending_resize = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        self.hoover_groups(ws);
        if reaped.mux_empty && ws.window.is_some() {
            self.save_session(ws);
            self.request_window_close(ws.seq);
            return None;
        }
        ws.search_queries
            .retain(|pane_id, _| ws.mux.panes.contains_key(pane_id));
        if ws
            .media_paste_receipt
            .as_ref()
            .is_some_and(|receipt| !ws.mux.panes.contains_key(&receipt.pane_id))
        {
            ws.media_paste_receipt = None;
            ws.accessibility_pending = true;
        }
        if ws
            .pending_video_paste_receipt
            .as_ref()
            .is_some_and(|receipt| !ws.mux.panes.contains_key(&receipt.pane_id))
        {
            ws.pending_video_paste_receipt = None;
        }
        if !self.cfg.paste_video_preview {
            ws.pending_video_paste_receipt = None;
        }
        let now = std::time::Instant::now();
        if let Some(Err(error)) = poll_screenshot_completion(ws) {
            fire_notify(
                self.ui_text.text(kettle_i18n::Text::PaletteTakeScreenshot),
                &error,
            );
        }
        let screenshot_wait = ws
            .screenshot_completion
            .as_ref()
            .map(|_| std::time::Duration::from_millis(50));
        if ws
            .pending_video_paste_receipt
            .as_ref()
            .is_some_and(|pending| pending.expired(now))
        {
            ws.pending_video_paste_receipt = None;
            ws.accessibility_pending = true;
            log::debug!("video paste receipt abandoned: preview worker response timed out");
        }
        let pending_video_receipt_wait = ws.pending_video_paste_receipt.as_ref().map(|pending| {
            pending
                .deadline()
                .saturating_duration_since(now)
                .max(std::time::Duration::from_millis(1))
        });
        #[cfg(windows)]
        let bounded_pty_exit_wait = next_bounded_pty_exit_wait(ws, now);
        // Catch the session changes no gesture announces — see `SESSION_SWEEP`.
        // Deliberately NOT part of the returned deadline: this must never be a
        // reason to wake up.
        if session_sweep_due(self.last_session_sweep, now, !ws.mux.tabs.is_empty()) {
            self.last_session_sweep = Some(now);
            self.save_session(ws);
        }
        let lua_wait = self.poll_pending_lua_commands(ws, event_loop, now);
        let remote_wait = if should_poll_remote_window(
            !self.pending_remote_commands.is_empty(),
            self.remote_spool_claim_pending,
            self.remote_command_retry.next_at.is_some(),
            ws.seq,
            self.focused_seq,
        ) {
            // An unclaimed spool belongs to the window focused when it is
            // finally claimed. Do not let the first BTreeMap window steal the
            // origin merely because a busy lock deferred the watcher event.
            self.poll_pending_remote_commands(ws, now)
        } else {
            None
        };
        let automation_wait = match (lua_wait, remote_wait) {
            (Some(lua), Some(remote)) => Some(lua.min(remote)),
            (Some(wait), None) | (None, Some(wait)) => Some(wait),
            (None, None) => None,
        };
        self.check_pending_run_deadlines(ws);
        let pending_run_wait =
            self.pending_runs
                .values()
                .map(|p| p.deadline)
                .min()
                .map(|soonest| {
                    soonest.saturating_duration_since(now).clamp(
                        std::time::Duration::from_millis(1),
                        std::time::Duration::from_millis(500),
                    )
                });
        let automation_wait = automation_wait
            .into_iter()
            .chain(screenshot_wait)
            .chain(pending_run_wait)
            .min();
        if self.window_close_pending(ws.seq) || self.quit_requested {
            return None;
        }
        // Once the GPU device is lost we no longer paint (see the redraw guard).
        // Drive renderer recovery from here instead of scheduling animation
        // wakes that would only re-enter the dead-device redraw path.
        if self.gpu.as_ref().is_some_and(|g| g.is_lost()) {
            // The global device-loss callback can wake the loop without a
            // window event. Quiesce pane output here before any recovery
            // backoff returns so a hot PTY cannot spin the dead render path.
            self.sync_output_wake_gate(ws);
            self.start_gpu_incident_if_needed();
            match self.gpu_recovery.poll(now, GPU_RECOVERY_SETTLE) {
                RecoveryAction::Wait(wait) => {
                    let next = wait.max(std::time::Duration::from_millis(1));
                    let next = automation_wait.map_or(next, |automation| automation.min(next));
                    let next = pending_video_receipt_wait.map_or(next, |pending| pending.min(next));
                    return Some(now + next);
                }
                RecoveryAction::Attempt { attempt_index } => {
                    let attempt = attempt_index.saturating_add(1);
                    let escalation = kettle_render::escalation_for_attempt(attempt_index);
                    let escalation_label = gpu_escalation_label(escalation);
                    if let Some(log) = self.gpu_incident.as_mut()
                        && let Err(error) = log.record_attempt(attempt, escalation_label)
                    {
                        log::warn!("GPU fault diagnostic write failed: {error}");
                    }
                    match self.try_recover_gpu(ws, event_loop, escalation, attempt) {
                        Ok(outcome) => {
                            if let Some(log) = self.gpu_incident.as_mut()
                                && let Err(error) = log.record_recovered(
                                    attempt,
                                    escalation_label,
                                    outcome.adapter,
                                    outcome.secondary_window_failures,
                                )
                            {
                                log::warn!("GPU fault diagnostic write failed: {error}");
                            }
                            self.gpu_incident = None;
                            self.gpu_incident_started_for_loss = false;
                            self.gpu_recovery.recovered();
                        }
                        Err(error) => {
                            if let Some(log) = self.gpu_incident.as_mut()
                                && let Err(write_error) =
                                    log.record_failure(attempt, escalation_label, &error)
                            {
                                log::warn!("GPU fault diagnostic write failed: {write_error}");
                            }
                            let backoff = gpu_recovery_backoff(attempt);
                            let now = self.gpu_recovery.failed(backoff);
                            let next = backoff.max(std::time::Duration::from_millis(1));
                            let next =
                                automation_wait.map_or(next, |automation| automation.min(next));
                            let next = pending_video_receipt_wait
                                .map_or(next, |pending| pending.min(next));
                            return Some(now + next);
                        }
                    }
                }
            }
        }
        if ws.pty_resize_retry.take_due(now) {
            if ws.renderer.is_some() {
                self.resize_all(ws);
            } else {
                // Preserve a bounded retry while a renderer rebuild temporarily
                // makes layout metrics unavailable.
                ws.pty_resize_retry.record_result(now, true);
            }
        }
        let pty_resize_retry_wait = ws
            .pty_resize_retry
            .remaining(now)
            .map(|wait| wait.max(std::time::Duration::from_millis(1)));
        let monitor_refresh_wait = if ws.output_refresh_probe_pending {
            match monitor_refresh_probe_wait(ws.output_refresh_probe_at, now, false) {
                Some(remaining) => Some(remaining),
                None => {
                    self.sync_output_frame_budget(ws, true);
                    None
                }
            }
        } else {
            None
        };
        let render_hidden = window_is_render_hidden(ws);
        self.sync_output_wake_gate(ws);
        let mut frame_recovery_wait = None;
        match ws.frame_recovery.poll(now, render_hidden) {
            FrameRecoveryPoll::Idle | FrameRecoveryPoll::Quiescent => {}
            FrameRecoveryPoll::Wait(wait) => {
                frame_recovery_wait = Some(wait.max(std::time::Duration::from_millis(1)));
            }
            FrameRecoveryPoll::Ready(FrameRecoveryAction::Redraw) => {
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            FrameRecoveryPoll::Ready(FrameRecoveryAction::RebuildRenderer) => {
                match self.try_recover_window_renderer(ws) {
                    Ok(()) => {
                        log::info!("window {} renderer/surface recovery succeeded", ws.seq);
                        if let Some(window) = &ws.window {
                            window.request_redraw();
                        }
                    }
                    Err(error) => {
                        log::warn!(
                            "window {} renderer/surface recovery failed: {error}",
                            ws.seq
                        );
                        if self.gpu.as_ref().is_some_and(|gpu| gpu.is_lost()) {
                            // The device callback owns escalation. Ensure the
                            // event loop turns once more even if no animation
                            // or PTY event is active.
                            frame_recovery_wait = Some(std::time::Duration::from_millis(1));
                        } else {
                            ws.frame_recovery.schedule_renderer_rebuild(now);
                            match ws.frame_recovery.poll(now, render_hidden) {
                                FrameRecoveryPoll::Wait(wait) => {
                                    frame_recovery_wait =
                                        Some(wait.max(std::time::Duration::from_millis(1)));
                                }
                                FrameRecoveryPoll::Quiescent => {}
                                FrameRecoveryPoll::Idle | FrameRecoveryPoll::Ready(_) => {
                                    // A failed rebuild after the first
                                    // immediate attempt always arms a positive
                                    // backoff. Keep a defensive one-turn wake
                                    // if that invariant changes.
                                    frame_recovery_wait = Some(std::time::Duration::from_millis(1));
                                }
                            }
                        }
                    }
                }
            }
        }
        // Most backends pair ScaleFactorChanged with Resized. If one does not,
        // commit the pending DPI layout from the window's final inner size
        // here. A minimized 0x0 window and any renderer/GPU repair keep the
        // transition pending until a usable surface exists.
        let mut dpi_fallback_wait = None;
        if let Some((width, height)) = ws
            .window
            .as_ref()
            .map(|window| window.inner_size())
            .map(|size| (size.width, size.height))
        {
            let renderer_ready = self.dpi_resize_renderer_ready(ws)
                && !render_hidden
                && !ws.dpi_resize_surface_suspended;
            match ws.dpi_resize.on_event(DpiResizeEvent::AboutToWait {
                width,
                height,
                renderer_ready,
            }) {
                DpiResizeAction::ApplyLayout => self.commit_window_resize(ws, width, height),
                DpiResizeAction::DeferAndWake => {
                    // Do not render a transient new-scale/old-size frame just
                    // to turn the loop. A precise short deadline gives the
                    // backend's paired Resized event one more dispatch turn.
                    dpi_fallback_wait = Some(std::time::Duration::from_millis(1));
                }
                DpiResizeAction::Defer => {}
            }
        }
        let (completion_hide_due, completion_hide_wait) = ws
            .mux
            .active_focus()
            .and_then(|pane_id| ws.mux.panes.get(&pane_id))
            .map(|pane| pane.term.poll_completion_hide(now))
            .unwrap_or((false, None));
        // Drive cursor blink + visual-bell decay without busy-looping: only
        // schedule wake-ups while something is actually animating. A flash
        // that has fully decayed is dropped here and gets one more repaint to
        // erase its last frame, like the resize chip below.
        let bells_expired = ws
            .bell_flashes
            .values()
            .any(|rang| rang.elapsed() >= BELL_FLASH_DURATION);
        if bells_expired {
            ws.bell_flashes
                .retain(|_, rang| rang.elapsed() < BELL_FLASH_DURATION);
            if !render_hidden && let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
        let bell_active = !render_hidden && !ws.bell_flashes.is_empty();
        // The resize chip needs repaints until it expires (then one more to
        // erase it); clear the state once it has.
        let update_toast_wait = self.tick_update_toast(ws, now);
        let resize_chip_live = ws
            .resize_overlay
            .map(|(_, _, t)| t.elapsed() < RESIZE_OVERLAY_DURATION)
            .unwrap_or(false);
        let resize_chip_active = resize_chip_live && !render_hidden;
        if !resize_chip_live && ws.resize_overlay.is_some() {
            ws.resize_overlay = None;
            if !render_hidden && let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
        let receipt_enabled = ws
            .media_paste_receipt
            .as_ref()
            .is_some_and(|receipt| match &receipt.kind {
                crate::window_state::MediaPasteReceiptKind::Image { .. } => {
                    self.cfg.paste_image_preview
                }
                crate::window_state::MediaPasteReceiptKind::Video { .. } => {
                    self.cfg.paste_video_preview
                }
            });
        let receipt_owner_visible = receipt_enabled
            && ws.window_focused
            && terminal_surface_available(ws)
            && ws
                .media_paste_receipt
                .as_ref()
                .is_some_and(|receipt| ws.mux.active_focus() == Some(receipt.pane_id));
        let mut media_receipt_redraw = false;
        if !receipt_enabled && ws.media_paste_receipt.take().is_some() {
            // A live config change to `off` is an ownership boundary, not only
            // a paint toggle. Release the bounded pixels and retained path
            // immediately instead of keeping hidden preview state until its
            // original lifetime expires.
            ws.accessibility_pending = true;
            media_receipt_redraw = true;
        }
        if !receipt_owner_visible
            && let Some(receipt) = ws.media_paste_receipt.as_mut()
            && receipt.set_hover(false, now)
        {
            ws.accessibility_pending = true;
            media_receipt_redraw = true;
        }
        if ws
            .media_paste_receipt
            .as_ref()
            .is_some_and(|receipt| !receipt.live(now))
        {
            ws.media_paste_receipt = None;
            ws.accessibility_pending = true;
            media_receipt_redraw = true;
        } else if let Some(receipt) = ws.media_paste_receipt.as_mut() {
            media_receipt_redraw |= receipt.mark_collapsed_if_due(now);
        }
        // Cards that moved under a still pointer, or one under it settling,
        // change the cursor and the hover outline without a pointer event.
        if ws.card_hover_stale || ws.card_settle_wake.is_some_and(|at| at <= now) {
            ws.card_hover_stale = false;
            self.sync_cursor_icon(ws);
        }
        // The tip leaves its card when its time is up.
        if self.cards_tip.until().is_some_and(|until| until <= now) {
            self.end_cards_tip(ws);
        }
        // A lane that can no longer use the keyboard gives it back.
        self.check_preview_focus(ws);
        // Zoomed lanes whose view settled ask for sharper pixels, and a
        // lane's edge drag that paused resizes its PTYs.
        let lane_crop_wait = self.ask_due_lane_crops(ws, now);
        let lane_resize_wait = self.flush_lane_resize(ws, now);
        // Lane previews step their frames, time out, or start on a rest.
        let lane_preview_wait = self.tick_lane_previews(ws, now);
        // Lane playbacks keep their sessions, and pause while hidden.
        let lane_playback_wait = self.tick_lane_playbacks(ws, now);
        // A menu or dialog opened from the keyboard ends a lane's drag now,
        // not at the pointer's next motion.
        if (ws.lane_drag.is_some() || ws.lane_resize.is_some())
            && (ws.context_menu.is_some() || self.non_search_modal_open(ws))
        {
            ws.lane_drag = None;
            self.end_lane_resize(ws);
        }
        let cards_tip_wait = self.cards_tip.until().map(|until| {
            until
                .saturating_duration_since(now)
                .max(std::time::Duration::from_millis(1))
        });
        let card_settle_wait = ws.card_settle_wake.map(|at| {
            at.saturating_duration_since(now)
                .max(std::time::Duration::from_millis(1))
        });
        let media_receipt_wait = ws
            .media_paste_receipt
            .as_ref()
            .and_then(|receipt| receipt.next_deadline(now))
            .map(|deadline| {
                deadline
                    .saturating_duration_since(now)
                    .max(std::time::Duration::from_millis(1))
            });
        // `about_to_wait` sits on the same input-to-present path as a queued
        // menu-hover redraw on winit backends that deliver RedrawRequested in
        // the next turn. Reuse the validated snapshot's DEC blink bit while a
        // context menu is open; otherwise this scheduler quietly reacquires the
        // focused Term mutex even though `redraw` itself is lock-free.
        let cached_menu_snapshots = ws.context_menu.is_some()
            && context_menu_snapshot_reuse_safe(ws)
            && self.torn_drag.is_none()
            && self.pane_snapshots_match_current_layout(ws);
        let pane_blink = self.pane_cursor_blinking(ws, cached_menu_snapshots);
        let blink_timed_out = cursor_blink_timed_out(
            now.saturating_duration_since(ws.last_blink_activity),
            self.cfg.cursor_blink_timeout(),
            ws.blink_on,
        );
        // While the window server blinks the cursor, the scheduler neither
        // flips the phase nor wakes for it; `redraw` materializes the phase
        // from the layer's plan instead.
        let layer_blink = ws.blink_layer.is_active();
        let blink_active = !render_hidden
            && !layer_blink
            && !blink_timed_out
            && cursor_blink_active(self.cfg.cursor_blink, pane_blink, ws.window_focused);
        let blink_interval = std::time::Duration::from_millis(self.cfg.cursor_blink_interval);
        let blink_elapsed = now.saturating_duration_since(ws.last_blink);
        let next_blink =
            next_cursor_blink_phase(blink_active, blink_elapsed, blink_interval, ws.blink_on);
        let blink_due = next_blink.is_some();
        if let Some(next_blink) = next_blink {
            // Advance at the TIMER edge, before request_redraw(). Wayland may
            // defer RedrawRequested until a compositor frame callback; leaving
            // the old timestamp in place during that gap repeatedly enqueues
            // the same blink and drains the queue at full frame rate.
            ws.blink_on = next_blink;
            ws.last_blink = now;
        }
        // Whether only the blink drew in the half-period this edge ends.
        let blink_quiet = blink_due
            && ws
                .blink_layer
                .note_edge(now, crate::cursor_blink::quiet_window(blink_interval));
        let term_anim = !render_hidden
            && ws
                .mux
                .panes
                .values()
                .any(|p| p.term.has_running_animation());
        let term_anim_finished = std::mem::replace(&mut ws.term_animation_active, term_anim)
            && !term_anim
            && !render_hidden;
        // Wake at background frame boundaries, subject to the focus policy.
        // Output-driven repaints may still advance a focus-paused background.
        let bg_anim_interval_raw = ws
            .renderer
            .as_ref()
            .and_then(|r| r.bg_anim_interval_ms(&self.cfg, ws.window_focused));
        // Hidden windows add no background-animation wakes.
        let bg_hidden = bg_anim_interval_raw.is_some() && render_hidden;
        let bg_anim_interval = if bg_hidden {
            None
        } else {
            bg_anim_interval_raw
        };
        // Edge-trigger the bg redraw. Request it ONLY when the displayed frame
        // index changes, since requesting it on every `about_to_wait` makes
        // winit redraw continuously. This mirrors how `blink_due` gates the
        // cursor-blink redraw.
        let bg_frame = if bg_hidden {
            None
        } else {
            ws.renderer
                .as_ref()
                .and_then(|r| r.bg_current_frame_index(&self.cfg, ws.window_focused))
        };
        let bg_frame_due = bg_frame.is_some() && bg_frame != ws.last_bg_frame;
        if bg_frame_due {
            ws.last_bg_frame = bg_frame;
        }
        if bg_frame.is_none() {
            ws.last_bg_frame = None;
        }
        // Selection-autoscroll runs at the same ~30 fps as bell / image
        // animation. Without an active wake-up the edge drag freezes until
        // the user wiggles the mouse.
        let autoscroll_active = !render_hidden && ws.selecting && {
            let area = self.area(ws);
            let scale = ws
                .window
                .as_ref()
                .map_or(1.0, |window| window.scale_factor() as f32);
            ws.selecting_pane
                .and_then(|id| self.pane_terminal(ws, area, id))
                .map(|r| {
                    ws.selection_autoscroll_edge != 0
                        || selection_autoscroll_lines(
                            ws.cursor.y as f32,
                            r.1,
                            r.1 + r.3,
                            scale,
                            ws.selection_dragged,
                        ) != 0
                })
                .unwrap_or(false)
        };
        // A deferred (coalesced) output paint becomes due
        // `OUTPUT_FRAME_BUDGET` after the last frame. Until then it stays
        // pending and we wake at its deadline so the burst paints exactly once.
        let output_budget = effective_output_budget(ws.output_frame_budget, ws.flood_paints);
        let coalesce_due = !render_hidden
            && !ws.frame_recovery.has_pending()
            && ws.output_pacer.is_deferred()
            && !ws.output_pacer.redraw_queued()
            && ws
                .last_paint
                .map(|t| now.saturating_duration_since(t) >= output_budget)
                .unwrap_or(true);
        // At an edge that hides the cursor, a quiet window with nothing else
        // to draw hands its blink to the window server (macOS). The blink
        // redraw requested below becomes the hand-off frame.
        let cursor_eligible = crate::macos_cursor_layer::SUPPORTED
            && self.cfg.macos_cursor_blink_layer
            && next_blink == Some(false)
            && Self::pane_cursor_layer_eligible(ws);
        if crate::cursor_blink::handoff_allowed(crate::cursor_blink::HandoffInputs {
            enabled: crate::macos_cursor_layer::SUPPORTED
                && self.cfg.macos_cursor_blink_layer
                && ws.blink_layer.fallback().is_none(),
            edge_hides_cursor: next_blink == Some(false),
            blink_active,
            cursor_eligible,
            quiet: blink_quiet,
            other_animation: bell_active
                || term_anim
                || bg_anim_interval.is_some()
                || autoscroll_active
                || resize_chip_active
                || media_receipt_redraw
                || media_receipt_wait.is_some()
                || pending_video_receipt_wait.is_some()
                || completion_hide_due
                || completion_hide_wait.is_some(),
            ime_preedit: ws.ime_preedit.is_some(),
            modal_steady: modal_holds_cursor_steady(ws),
            work_pending: coalesce_due
                || ws.output_pacer.is_deferred()
                || ws.frame_recovery.has_pending()
                || ws.pending_resize
                || ws.accessibility_pending
                || dpi_fallback_wait.is_some()
                || pty_resize_retry_wait.is_some()
                || ws
                    .renderer
                    .as_ref()
                    .is_some_and(Renderer::has_pending_screenshot),
        }) {
            ws.blink_layer.request_handoff();
        }
        // A theme schedule flips at a wall-clock boundary, and only a redraw
        // applies it. A window that draws nothing (idle, or with its cursor
        // blinking on the window server's layer) would otherwise keep the old
        // theme until something else changes, so check the schedule at each
        // wall-clock minute and draw only when it has flipped.
        let theme_schedule_due = !render_hidden
            && theme_schedule_redraw_due(
                self.last_schedule_decision,
                self.theme_schedule_is_dark(),
            );
        let theme_schedule_wait =
            (!render_hidden && self.cfg.theme_schedule.is_some()).then(until_next_wall_minute);
        if (bell_active
            || blink_due
            || term_anim
            || term_anim_finished
            || bg_frame_due
            || autoscroll_active
            || coalesce_due
            || resize_chip_active
            || media_receipt_redraw
            || completion_hide_due
            || theme_schedule_due)
            && let Some(w) = &ws.window
        {
            w.request_redraw();
            if coalesce_due {
                ws.output_pacer.queue_deferred_redraw();
            }
        }
        // Pick the earliest wake we still need: ~30 fps for bell / animation /
        // autoscroll / the resize chip, the cursor-blink half-period deadline,
        // or the pending coalesced output paint's deadline.
        let blink_wait = blink_active.then(|| {
            let remaining = blink_interval.saturating_sub(blink_elapsed);
            remaining.max(std::time::Duration::from_millis(1))
        });
        let mut wait = animation_wait(
            bell_active || term_anim || autoscroll_active || resize_chip_active,
            bg_anim_interval,
            blink_wait,
        );
        if let Some(next) = frame_recovery_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = theme_schedule_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = dpi_fallback_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = pty_resize_retry_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = monitor_refresh_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = automation_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = completion_hide_wait {
            let next = next.max(std::time::Duration::from_millis(1));
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = update_toast_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = media_receipt_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = card_settle_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = cards_tip_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = lane_crop_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = lane_resize_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = lane_preview_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = lane_playback_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = pending_video_receipt_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        #[cfg(windows)]
        if let Some(next) = bounded_pty_exit_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if held_child_reap_pending {
            let next = std::time::Duration::from_secs(1);
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if ws.output_pacer.is_deferred()
            && !ws.output_pacer.redraw_queued()
            && !render_hidden
            && !ws.frame_recovery.has_pending()
        {
            let remaining = ws
                .last_paint
                .map(|t| output_budget.saturating_sub(now.saturating_duration_since(t)))
                .unwrap_or_default();
            let next = remaining.max(std::time::Duration::from_nanos(1));
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        // Search's deferred unlimited pass is deadline-driven and chunked.
        // Wake exactly when the 500 ms typing debounce expires; while a chunk
        // job remains, schedule the next event-loop turn without polling at a
        // frame-rate forever (the job clears at a match or terminal edge).
        if ws.search.open {
            // Repaint once when the bar should start saying `Searching…`.
            if let Some(at) = ws.search.searching_label_at() {
                if now < at {
                    ws.search.searching_label_wake = Some(at);
                    let next = at.saturating_duration_since(now);
                    wait = Some(wait.map_or(next, |current| current.min(next)));
                } else if ws.search.searching_label_wake.take().is_some()
                    && let Some(window) = &ws.window
                {
                    window.request_redraw();
                }
            }
            if let Some(deadline) = ws.search.unlimited_retry_at {
                let due = now >= deadline;
                let next = if due {
                    std::time::Duration::from_millis(1)
                } else {
                    deadline
                        .saturating_duration_since(now)
                        .max(std::time::Duration::from_millis(1))
                };
                wait = Some(wait.map_or(next, |current| current.min(next)));
                if due && let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            if ws.search.background.is_some() {
                let next = std::time::Duration::from_millis(1);
                wait = Some(wait.map_or(next, |current| current.min(next)));
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
        }
        if ws.accessibility_pending {
            let remaining =
                ws.accessibility_updated_at
                    .map_or(std::time::Duration::ZERO, |updated| {
                        ACCESSIBILITY_UPDATE_INTERVAL
                            .saturating_sub(now.saturating_duration_since(updated))
                    });
            let due = remaining.is_zero();
            let next = if due {
                std::time::Duration::from_millis(1)
            } else {
                remaining.max(std::time::Duration::from_millis(1))
            };
            wait = Some(wait.map_or(next, |current| current.min(next)));
            if due && let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        // `show` pushes waiting past their deadline are answered, and those
        // whose pane closed are dropped, even when nothing else wakes us.
        self.media_tick(ws);
        self.prune_previews(ws);
        self.open_ready_previews(ws);
        if let Some(soonest) = self.media.next_deadline() {
            let next = soonest.saturating_duration_since(now).clamp(
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(500),
            );
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        // A worker failure may already be visible before this callback begins,
        // so comparing status only within this turn would miss the edge. The
        // notifier is edge-triggered; this unconditional state sync is cheap.
        if self.recorder.is_some() {
            let body = self
                .ui_text
                .text(kettle_i18n::Text::NotifyBodyRecordingFlushFailed);
            self.sync_recording_state(ws, body);
        }
        wait.map(|remaining| now + remaining)
    }
}
