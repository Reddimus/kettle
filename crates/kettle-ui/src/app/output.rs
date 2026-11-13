use super::*;

fn apply_pane_exit_action(
    action: kettle_config::ExitAction,
    pane_id: u64,
    pane: &mut Pane,
    pending_restarts: &mut Vec<u64>,
) {
    if pane.exit_observed {
        return;
    }
    pane.exit_observed = true;
    match action {
        kettle_config::ExitAction::Hold | kettle_config::ExitAction::CloseOnSuccess => {
            pane.held = true;
            // `try_wait` is now safe: the ordered PTY exit (or the bounded
            // fallback below) has finished consuming output. Cache the status
            // and release the Unix zombie while the held grid stays visible.
            pane.exit_code = pane.term.child_exit_code();
            pane.held_child_reaped = pane.exit_code.is_some();
            pane.close_on_success = action == kettle_config::ExitAction::CloseOnSuccess;
            if pane.close_on_success && pane.exit_code == Some(0) {
                pane.closed = true;
            }
        }
        kettle_config::ExitAction::Restart => {
            if !pending_restarts.contains(&pane_id) {
                pending_restarts.push(pane_id);
                log::info!("exit-action = restart: queued pane {pane_id} for respawn");
            }
            pane.closed = true;
        }
        kettle_config::ExitAction::Close => pane.closed = true,
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BoundedPtyExitAction {
    Wait,
    BeginClose,
    ApplyPolicy,
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BoundedPtyExitPhase {
    NotStarted,
    InProgress(std::time::Duration),
    Failed { retry_in: std::time::Duration },
}

#[cfg(any(windows, test))]
pub(super) fn bounded_pty_exit_action(
    child_exited_for: std::time::Duration,
    phase: BoundedPtyExitPhase,
) -> BoundedPtyExitAction {
    match phase {
        BoundedPtyExitPhase::NotStarted
            if child_exited_for >= kettle_core::term::PTY_CHILD_EXIT_EOF_TIMEOUT =>
        {
            // Even a badly delayed/coalesced first wake must start close before
            // policy. Applying Hold while the live master is still owned would
            // make the reader permanent.
            BoundedPtyExitAction::BeginClose
        }
        BoundedPtyExitPhase::InProgress(close_for)
            if close_for >= kettle_core::term::PTY_CHILD_EXIT_EOF_TIMEOUT =>
        {
            BoundedPtyExitAction::ApplyPolicy
        }
        BoundedPtyExitPhase::Failed { retry_in } if retry_in.is_zero() => {
            BoundedPtyExitAction::BeginClose
        }
        _ => BoundedPtyExitAction::Wait,
    }
}

pub(super) fn take_window_lifecycle_wakes(ws: &WindowState) -> bool {
    // Do not use `Iterator::any`: its short-circuit would leave later panes'
    // edges set and, because the process-wide Wakeup was already coalesced,
    // could strand their semantic events without another turn.
    let mut pending = false;
    for pane in ws.mux.panes.values() {
        pending |= pane.term.take_lifecycle_wake();
    }
    pending
}

#[cfg(windows)]
pub(super) fn next_bounded_pty_exit_wait(
    ws: &WindowState,
    now: std::time::Instant,
) -> Option<std::time::Duration> {
    ws.mux
        .panes
        .values()
        .filter(|pane| !pane.exit_observed)
        .filter_map(|pane| {
            let child_exit_at = pane.term.direct_child_exit_at()?;
            let deadline = match pane.pty_output_close_phase {
                PtyOutputClosePhase::NotStarted => {
                    child_exit_at + kettle_core::term::PTY_CHILD_EXIT_EOF_TIMEOUT
                }
                PtyOutputClosePhase::InProgress(started) => {
                    started + kettle_core::term::PTY_CHILD_EXIT_EOF_TIMEOUT
                }
                PtyOutputClosePhase::Failed { retry_at } => retry_at,
            };
            Some(
                deadline
                    .saturating_duration_since(now)
                    .max(std::time::Duration::from_millis(1)),
            )
        })
        .min()
}

pub(super) fn apply_output_generation_outcome(
    outcome: FrameOutcome,
    committed: &mut std::collections::HashMap<u64, u64>,
    candidate: &mut std::collections::HashMap<u64, u64>,
) -> bool {
    if outcome != FrameOutcome::Presented {
        return false;
    }
    std::mem::swap(committed, candidate);
    true
}

pub(super) fn stage_output_generations_for_frame(
    candidate: &mut std::collections::HashMap<u64, u64>,
    committed: &std::collections::HashMap<u64, u64>,
    cached_snapshot_keys: Option<&[(u64, u64, usize, usize)]>,
    current: impl IntoIterator<Item = (u64, u64)>,
) {
    candidate.clear();
    if let Some(cached_snapshot_keys) = cached_snapshot_keys {
        // A hover frame represents the cached active layout. Preserve the
        // already-consumed generations for background panes, then overwrite
        // visible panes with the exact generations in those snapshots. Output
        // racing on either kind of pane therefore remains pending.
        candidate.extend(committed.iter().map(|(id, generation)| (*id, *generation)));
        candidate.extend(
            cached_snapshot_keys
                .iter()
                .map(|(id, generation, _, _)| (*id, *generation)),
        );
    } else {
        candidate.extend(current);
    }
}

pub(super) fn prepare_output_frame<R>(
    ws: &mut WindowState,
    reuse_pane_snapshots: bool,
    refresh: impl FnOnce(&mut WindowState) -> R,
) -> R {
    // This cutoff covers cached grid readers as well as render snapshots.
    // A later sample could consume output that a link/search cache skipped
    // while the grid was new but its publication was still pending.
    stage_output_generations_for_frame(
        &mut ws.pending_output_gen,
        &ws.seen_output_gen,
        reuse_pane_snapshots.then_some(&ws.pane_snapshot_keys),
        ws.mux
            .panes
            .iter()
            .map(|(id, pane)| (*id, pane.term.output_generation())),
    );
    refresh(ws)
}

/// Does any pane in this window have PTY output newer than the
/// window's last paint? (`Terminal::output_generation` vs the snapshot
/// `redraw` records.) Plain output emits no TermEvent, so this counter
/// is the only reliable per-window "new output" signal for the fan-out
/// wakeup.
fn window_has_new_output(ws: &WindowState) -> bool {
    ws.mux.panes.iter().any(|(id, p)| {
        ws.seen_output_gen.get(id).copied().unwrap_or(0) != p.term.output_generation()
    })
}

/// Judge the typed-echo diagnostics' pending echo and frame against each
/// pane's current output. Returns the pane whose echo this found split.
pub(super) fn observe_typed_echo(ws: &mut WindowState, now: std::time::Instant) -> Option<u64> {
    let panes = &ws.mux.panes;
    ws.typed_echo.observe(now, ws.last_typed, |id| {
        panes.get(&id).map(|pane| {
            // Generation first: a stamp read after it is at least as new.
            let generation = pane.term.output_generation();
            (generation, pane.term.output_published_at())
        })
    })
}

/// Record an output wake inside the typing window with the panes whose
/// output this window has not painted. Returns whether it starts a new echo.
pub(super) fn note_typed_echo_wake(ws: &mut WindowState) -> bool {
    let seen = &ws.seen_output_gen;
    ws.typed_echo.typed_wake(
        ws.last_typed,
        ws.mux.panes.iter().filter_map(|(id, pane)| {
            let generation = pane.term.output_generation();
            (seen.get(id).copied().unwrap_or(0) != generation)
                .then(|| (*id, generation, pane.term.output_published_at()))
        }),
    )
}

pub(super) fn defer_output_racing_frame(ws: &mut WindowState) {
    // Publication during maintenance can be coalesced into an already-served
    // wake before redraw reopens the latch. Retain a paced retry even when no
    // further wake is queued. Publication after this check owns a fresh wake.
    if window_has_new_output(ws) {
        ws.output_pacer.defer();
    }
}

/// Decide whether an output wake still represents unpainted damage.
///
/// A wake queued by output racing a frame can become stale when that same
/// frame presents the new generation before the event is dispatched. Rearm
/// the per-pane latch only in that no-damage case, then sample again: output
/// racing between the first sample and rearm is handled by this event, while
/// output arriving after rearm owns a fresh wake. A wake that is still dirty
/// deliberately leaves the latch closed across the deferred paint budget.
pub(super) fn output_wakeup_needs_paint(
    mut has_new_output: impl FnMut() -> bool,
    rearm_stale_wakeup: impl FnOnce(),
) -> bool {
    if has_new_output() {
        return true;
    }
    rearm_stale_wakeup();
    has_new_output()
}

pub(super) fn output_wakeup_must_quiesce(
    render_hidden: bool,
    renderer_repair_pending: bool,
    renderer_unavailable: bool,
) -> bool {
    render_hidden || renderer_repair_pending || renderer_unavailable
}

pub(super) fn output_wake_transport_enabled(
    render_quiesced: bool,
    output_sidechannel: bool,
) -> bool {
    !render_quiesced || output_sidechannel
}

pub(super) fn output_generation_advanced(previous: Option<u64>, current: u64) -> bool {
    previous.is_some_and(|previous| previous != current)
}

pub(super) struct PaneEventBatch {
    bell: bool,
    bell_panes: Vec<u64>,
    output_chunks: Vec<(u64, Vec<u8>)>,
    command_finished_local: Vec<(u64, kettle_core::CommandFinished)>,
    correlated_command_finished_local: Vec<(u64, kettle_core::CorrelatedCommandFinished)>,
    protocol_notifications_local: Vec<(u64, kettle_core::ProtocolNotification)>,
    pending_restarts_local: Vec<u64>,
}

pub(super) fn drain_pane_events(
    ws: &mut WindowState,
    exit_action: kettle_config::ExitAction,
    force_no_bell: bool,
    mut reply_for_event: impl FnMut(&mut Pane, TermEvent) -> Option<String>,
) -> PaneEventBatch {
    let mut bell = false;
    let mut entered_hold = false;
    // Pane ids that rang the bell this drain
    // pass — latched onto their containing tabs *after* the
    // iter_mut() loop so we don't double-borrow mux.panes.
    let mut bell_panes: Vec<u64> = Vec::new();
    // Pane ids + raw-output chunks accumulated during this drain pass.
    // Keep the PTY reader's bounded chunks intact instead of concatenating
    // a whole backlog into a second potentially huge allocation.
    let mut output_chunks: Vec<(u64, Vec<u8>)> = Vec::new();
    // Collect OSC-133 CommandFinished events per
    // pane so the App is the SINGLE drainer — `drain_command_finished_events`
    // is destructive, so the command-notify, the run_command correlator, and
    // event subscribers must all be fed from one place, AFTER this pane loop.
    let mut command_finished_local: Vec<(u64, kettle_core::CommandFinished)> = Vec::new();
    let mut correlated_command_finished_local = Vec::new();
    // Protocol desktop notifications (OSC 9 / OSC 777) drained once per
    // pass and dispatched after the pane loop so the UI owns all toasts.
    let mut protocol_notifications_local: Vec<(u64, kettle_core::ProtocolNotification)> =
        Vec::new();
    // Pane ids whose shell exited with cfg.exit_action
    // = Restart. Queued during the drain; appended to
    // ws.pending_pane_restarts after the iteration so the
    // post-drain handler can process them with a fresh borrow.
    let mut pending_restarts_local: Vec<u64> = Vec::new();
    for (&pane_id, pane) in ws.mux.panes.iter_mut() {
        if !pane.held && pane.pty_input_failed() {
            log::error!("closing pane {pane_id}: its PTY input transport failed");
            pane.closed = true;
            while pane.rx.try_recv().is_ok() {}
            continue;
        }
        let was_held = pane.held;
        // Read the exit latch before anything else: the reader sets it
        // after the last of the PTY's output, so everything that output
        // latched or queued is taken below, before exit policy applies.
        // An exit latched after this read waits for the next drain.
        let exited = pane.term.take_exited();
        // Drain the optional output sidechannel before regular events.
        if let Some(out_rx) = &pane.output_rx {
            for chunk in output_chunks_ready(out_rx) {
                output_chunks.push((pane_id, chunk));
            }
        }
        // Bells are latched by the terminal, not queued with the events
        // below. Terminator parity (terminatorlib/config.py:103
        // `force_no_bell`): silence every bell flavor regardless of
        // cfg.bell mode.
        if pane.term.take_bell() && !force_no_bell {
            bell = true;
            bell_panes.push(pane_id);
        }
        // Titles and cursor-blink switches keep only their newest state.
        // Clipboard operations are drained in their journal order below.
        match pane.term.take_title() {
            Some(kettle_core::TitleChange::Set(t)) => {
                // Ignore the bogus full-exe-path title conhost/
                // ConPTY injects at startup for a native Windows shell —
                // keeping the "kettle" placeholder so the tab/window/pane
                // label falls back to the working directory (the OSC 7/9;9
                // cwd, else the natively-polled cwd) instead of showing an
                // ugly `…\pwsh.exe` path. Only a still-placeholder pane is
                // checked; ANY genuine OSC 2 title (incl. a later one from
                // the shell's own prompt) is stored and permanently ends
                // suppression, so a program-set title is never lost.
                if pane.title_is_placeholder
                    && suppress_startup_title(&t, &pane.argv, cfg!(windows))
                {
                    // keep the placeholder seed; do not store the exe path
                } else {
                    apply_shell_title(
                        &mut pane.title,
                        &mut pane.title_is_placeholder,
                        &mut pane.title_origin,
                        &mut pane.title_before_remote,
                        Some(&t),
                    );
                }
            }
            Some(kettle_core::TitleChange::Reset) => {
                apply_shell_title(
                    &mut pane.title,
                    &mut pane.title_is_placeholder,
                    &mut pane.title_origin,
                    &mut pane.title_before_remote,
                    None,
                );
            }
            None => {}
        }
        if pane.term.take_cursor_blinking_changed() {
            // DEC mode 12 (`CSI ?12 h/l`) just flipped. The next
            // redraw picks up the new state from
            // `Terminal::cursor_blinking()`, but reset the blink
            // phase now so *blink-on* starts visible and
            // *blink-off* makes the cursor solid right away, not on
            // whatever half-period we'd otherwise land in. It is
            // program output, so it does not restart the blink
            // timeout; only typing, focus, and settings do.
            ws.blink_on = true;
            ws.last_blink = std::time::Instant::now();
        }
        // A program that turned mouse reporting off ended the session the
        // Shift hint was shown for; the next one may show it again. Only a
        // pane that had the hint reads its mode here, and never waits for
        // it: the reader holding the lock wakes another drain when it is done.
        if pane.mouse_override_hinted
            && let Ok(term) = pane.term.term.try_lock()
            && !super::pointer::pane_tracks_mouse(*term.mode(), false)
        {
            pane.mouse_override_hinted = false;
        }
        let notice_revision = pane.osc52_notice.revision;
        let event_rx = pane.rx.clone();
        for ev in pane
            .term
            .take_clipboard_events()
            .into_iter()
            .chain(event_rx.try_iter())
        {
            if let Some(reply) = reply_for_event(pane, ev)
                && pane.queue_protocol_reply(reply.as_bytes()).is_failed()
            {
                if !pane.held {
                    log::error!("closing pane {pane_id}: bounded PTY input queue failed");
                    pane.closed = true;
                }
                break;
            }
        }
        if pane.osc52_notice.revision != notice_revision {
            ws.accessibility_pending = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        // Terminator parity (terminatorlib/config.py:118
        // `exit_action`): when the shell exits, close the pane,
        // restart the shell, or hold the dead shell visible so the
        // user can read its final output and scrollback.
        //
        // Hold: don't mark closed; the pane shows its last output
        // until the user closes it (Ctrl+Shift+W).
        // Restart: mark closed and queue the pane id so this loop
        // never double-borrows ws.mux; the post-drain handler
        // respawns the same argv and cwd in a new tab.
        // Close (default): mark closed.
        //
        // `exited` was read before this pane's latches and queue, so its
        // final title, clipboard writes and replies were handled above.
        // `Mux::reap` keys off this instead of polling the process, so a
        // fast child cannot disappear before its final output or exit
        // policy is applied.
        if exited && !pane.closed {
            apply_pane_exit_action(exit_action, pane_id, pane, &mut pending_restarts_local);
        }
        #[cfg(windows)]
        if !pane.exit_observed
            && let Some(child_exit_at) = pane.term.direct_child_exit_at()
        {
            let now = std::time::Instant::now();
            let phase = match pane.pty_output_close_phase {
                PtyOutputClosePhase::NotStarted => BoundedPtyExitPhase::NotStarted,
                PtyOutputClosePhase::InProgress(started) => {
                    BoundedPtyExitPhase::InProgress(now.saturating_duration_since(started))
                }
                PtyOutputClosePhase::Failed { retry_at } => BoundedPtyExitPhase::Failed {
                    retry_in: retry_at.saturating_duration_since(now),
                },
            };
            match bounded_pty_exit_action(now.saturating_duration_since(child_exit_at), phase) {
                BoundedPtyExitAction::Wait => {}
                BoundedPtyExitAction::BeginClose => {
                    log::warn!(
                        "PTY output stayed open after pane {pane_id}'s direct child exited; \
                         starting bounded ConPTY close"
                    );
                    match pane.term.begin_pty_output_close() {
                        Ok(_) => {
                            pane.pty_output_close_phase =
                                PtyOutputClosePhase::InProgress(std::time::Instant::now())
                        }
                        Err(error) => {
                            log::error!("cannot close pane {pane_id}'s ConPTY output: {error}");
                            pane.pty_output_close_phase = PtyOutputClosePhase::Failed {
                                retry_at: std::time::Instant::now()
                                    + std::time::Duration::from_secs(1),
                            };
                        }
                    }
                }
                BoundedPtyExitAction::ApplyPolicy => {
                    // Older ConPTY builds may park ClosePseudoConsole while
                    // an inherited client remains attached. The close
                    // worker is detached specifically for that platform
                    // behavior; do not let it also park GUI exit policy.
                    log::error!(
                        "pane {pane_id}'s PTY did not finish within the bounded drain; final \
                         output may be incomplete"
                    );
                    apply_pane_exit_action(exit_action, pane_id, pane, &mut pending_restarts_local);
                }
            }
        }
        if !pane.held && pane.pty_input_failed() {
            log::error!("closing pane {pane_id}: its PTY input transport failed");
            pane.closed = true;
            while pane.rx.try_recv().is_ok() {}
        }
        entered_hold |= !was_held && pane.held && !pane.closed;
        // Drain OSC 133 D (CommandEnd) events ONCE here and
        // defer processing to after the pane loop, where the App can fan a
        // single drain out to the command-notify, the run_command
        // correlator, and event subscribers (each consumer would otherwise
        // race for the destructive drain). Always drain (not gated on the
        // notify threshold) so a pending run_command still resolves when
        // notifications are off.
        for ev in pane.term.drain_command_finished_events() {
            command_finished_local.push((pane_id, ev));
        }
        for event in pane.term.drain_correlated_command_finished_events() {
            correlated_command_finished_local.push((pane_id, event));
        }
        for ev in pane.term.drain_protocol_notifications() {
            protocol_notifications_local.push((pane_id, ev));
        }
    }
    if entered_hold {
        // Hold changes window chrome without advancing the PTY output generation.
        ws.accessibility_pending = true;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }
    PaneEventBatch {
        bell,
        bell_panes,
        output_chunks,
        command_finished_local,
        correlated_command_finished_local,
        protocol_notifications_local,
        pending_restarts_local,
    }
}

impl App {
    fn pane_event_reply(&mut self, pane: &mut Pane, ev: TermEvent) -> Option<String> {
        match ev {
            TermEvent::PtyWrite(s) => Some(s),
            event @ (TermEvent::ClipboardStore(..) | TermEvent::ClipboardLoad(..)) => {
                apply_osc52_event(
                    &mut PaneClipboard {
                        clipboard: self.clipboard.as_mut(),
                        policy: self.cfg.osc52,
                        notify: self.cfg.osc52_notify,
                        notices: &mut pane.osc52_notice,
                        tr: &self.ui_text,
                    },
                    event,
                )
            }
            TermEvent::TextAreaSizeRequest(fmt) => {
                // Use the exact pixel totals published through PtySize so
                // fractional cell sizes do not accumulate rounding error.
                let (pixel_width, pixel_height) = pane.term.pty_pixel_size();
                Some(kettle_render::reply_for_text_area_size(
                    pixel_width,
                    pixel_height,
                    &*fmt,
                ))
            }
            TermEvent::ColorRequest(idx, fmt) => pane.term.term.lock().ok().and_then(|t| {
                kettle_render::reply_for_query(idx, &self.cfg.theme, t.colors(), &*fmt)
            }),
            // Process status is advisory. Only the ordered PTY exit latch
            // may apply policy after the final output has been drained.
            TermEvent::ChildExit(_) => None,
            _ => None,
        }
    }

    pub(super) fn waker(&self) -> kettle_core::Waker {
        let p = self.proxy.clone();
        let pending = self.wake_pending.clone();
        Arc::new(move || {
            // Enqueue at most ONE Wakeup per paint window. A queued Wakeup
            // re-checks every pane's output generation when it lands, so every
            // enqueue past the first is pure event-loop spam (a flood would
            // send one per 64KiB read). `swap` returning false means this call
            // owns the enqueue; the Wakeup arm reopens the latch before it
            // reads generations, so nothing is ever missed. Events the proxy
            // queues on the crossbeam channel before waking are drained by
            // that same pending Wakeup, so suppression never delays them.
            if !pending.swap(true, std::sync::atomic::Ordering::AcqRel) {
                let _ = p.send_event(UserEvent::Wakeup);
            }
        })
    }

    /// Fan one destructive drain of the shared PTY-output sidechannel out to
    /// every enabled consumer. Both redraw-time and close-time drains must use
    /// this path; otherwise whichever path receives a chunk first can starve
    /// recorder or Lua output hooks of the same bytes.
    pub(super) fn dispatch_output_chunks(
        &mut self,
        _ws: &mut WindowState,
        output_chunks: Vec<(u64, Vec<u8>)>,
    ) {
        for (pane_id, bytes) in output_chunks {
            if let Some(rec) = self.recorder.as_mut() {
                rec.record_output(&bytes);
            }
            self.fire_lua_event(_ws, crate::LuaEvent::Output(pane_id, bytes), "output hook");
        }
        let body = self
            .ui_text
            .text(kettle_i18n::Text::NotifyBodyRecordingWriteFailed);
        self.sync_recording_state(_ws, body);
    }

    pub(super) fn drain_events(&mut self, ws: &mut WindowState) {
        self.sync_session_log_failures(ws);
        let PaneEventBatch {
            bell,
            bell_panes,
            output_chunks,
            command_finished_local,
            correlated_command_finished_local,
            protocol_notifications_local,
            pending_restarts_local,
        } = drain_pane_events(
            ws,
            self.cfg.exit_action,
            self.cfg.force_no_bell,
            |pane, ev| self.pane_event_reply(pane, ev),
        );
        if bell {
            if self.cfg.bell.visual() {
                // Only the pane that rang flashes; a sibling split's bell
                // then tells you which one it was. Panes outside the current
                // layout keep the tab dot and titlebar glyph as their cue.
                let now = std::time::Instant::now();
                for id in &bell_panes {
                    ws.bell_flashes.insert(*id, now);
                }
            }
            if self.cfg.bell.attention()
                && !ws.window_focused
                && let Some(w) = &ws.window
            {
                w.request_user_attention(Some(UserAttentionType::Informational));
                ws.attention_active = true;
            }
        }
        // Latch any per-pane bells onto their tab's
        // activity flag so the tab-bar dot survives even on tabs the
        // user isn't currently looking at. Active-tab bells were
        // already handled visually (`bell_flashes` above drives the
        // per-pane flash); the latching helper skips the active
        // tab so we don't double-signal.
        //
        // Terminator plugin parity:
        // fire LuaEvent::Bell(pane_id) for every belled pane after
        // the kettle-side bell handling is done. Callbacks may queue
        // LuaCommands (kettle.notify, kettle.send_text); they get
        // transferred immediately into the bounded App FIFO by
        // `fire_lua_event`, then dispatched in order on the event loop.
        // `fire_lua_event` drains through `drain_lua_hook_commands`, the one
        // canonical LuaCommand-match path that every Lua hook shares.
        for id in bell_panes {
            ws.mux.touch_tab_bell(id);
            self.fire_lua_event(ws, crate::LuaEvent::Bell(id), "bell hook");
        }
        // The focused pane is being looked at, so its bell is answered.
        //
        // Deliberately here rather than on focus-change events: focus moves by
        // far more routes than those fire on — a tab switch, `reap` promoting a
        // sibling when the focused shell exits, the ctl API, a layout restore —
        // and every one of them would have to remember. This runs each drain
        // against the true current focus, so none of them can leave the latch
        // lit on a pane the user is staring at.
        ws.mux.clear_focused_pane_bell();
        // A single destructive drain fans out to Lua and the
        // optional recorder so neither consumer can steal bytes from the other.
        self.dispatch_output_chunks(ws, output_chunks);
        // Fan out the OSC-133 CommandFinished events
        // drained above — command-notify (existing behavior), the run_command
        // correlator, and event subscribers all from this single place.
        for (pane_id, ev) in command_finished_local {
            // (a) Desktop notification for a long background command.
            if self.cfg.command_notify_threshold_ms > 0 {
                let elapsed_ms = ev.duration.as_millis() as u64;
                if !ws.window_focused && elapsed_ms >= self.cfg.command_notify_threshold_ms {
                    let secs = ev.duration.as_secs();
                    let tr = &self.ui_text;
                    let exit_text = match ev.exit_code {
                        Some(0) => tr.text(kettle_i18n::Text::NotifyBodyCommandOk).to_string(),
                        Some(code) => tr.notify_body_command_exit(&code.to_string()),
                        None => String::new(),
                    };
                    let body = if exit_text.is_empty() {
                        tr.notify_body_command_ran(pane_id, secs)
                    } else {
                        tr.notify_body_command_result(pane_id, secs, &exit_text)
                    };
                    fire_notify(
                        tr.text(kettle_i18n::Text::NotifyTitleCommandFinished),
                        &body,
                    );
                }
            }
            // (b) Resolve a pending run_command for this pane.
            self.resolve_pending_run(ws, pane_id, &ev);
            // (c) Notify event subscribers.
            self.ctl_broadcast(
                "command_finished",
                Some(pane_id),
                serde_json::json!({
                    "exit_code": ev.exit_code,
                    "duration_ms": ev.duration.as_millis() as u64,
                }),
            );
        }
        for (pane_id, event) in correlated_command_finished_local {
            self.resolve_pending_correlated_run(ws, pane_id, &event);
        }
        for (pane_id, ev) in protocol_notifications_local {
            fire_notify(&ev.title, &ev.body);
            if !ws.window_focused
                && let Some(w) = &ws.window
            {
                w.request_user_attention(Some(UserAttentionType::Informational));
                ws.attention_active = true;
            }
            self.ctl_broadcast(
                "protocol_notification",
                Some(pane_id),
                serde_json::json!({
                    "title": ev.title,
                    "body": ev.body,
                }),
            );
        }
        // Stash the restart list on the window so the post-drain handler can
        // process it with a fresh &mut ws.mux borrow. The pane loop above holds a
        // &mut iterator into ws.mux.panes, so spawn_pane can't run there.
        if !pending_restarts_local.is_empty() {
            ws.pending_pane_restarts.extend(pending_restarts_local);
        }
    }

    pub(super) fn process_pending_pane_restarts(&mut self, ws: &mut WindowState) {
        if ws.pending_pane_restarts.is_empty() {
            return;
        }
        let pane_ids: Vec<u64> = std::mem::take(&mut ws.pending_pane_restarts);
        let waker = self.waker();
        let (cols, rows) = self.grid_of(ws, self.area(ws));
        let geometry = self.pty_geometry_for_grid(ws, cols, rows);
        for pane_id in pane_ids {
            let restart_info = ws.mux.panes.get(&pane_id).map(|pane| {
                (
                    pane.argv.clone(),
                    pane.term.current_dir_or_native(),
                    pane.reconnect_launch.clone(),
                )
            });
            if let Some((argv, cwd, launch)) = restart_info {
                let pre = self.focus_key(ws);
                if let Err(error) = ws.mux.new_tab_with_launch_settings_geometry(
                    &self.cfg,
                    geometry,
                    waker.clone(),
                    argv,
                    cwd,
                    launch.as_ref(),
                ) {
                    log::warn!("exit-action = restart: spawn failed for pane {pane_id}: {error}");
                } else {
                    self.note_focus_change(ws, pre);
                    self.fire_tab_add_event(ws);
                }
            }
        }
    }
    pub(super) fn sync_output_wake_gate(&self, ws: &WindowState) {
        let render_quiesced = output_wakeup_must_quiesce(
            window_is_render_hidden(ws),
            ws.frame_recovery.has_pending(),
            ws.renderer.is_none() || self.gpu.as_ref().is_some_and(|gpu| gpu.is_lost()),
        );
        for pane in ws.mux.panes.values() {
            // A hidden surface does not need paint wakeups, but an opt-in
            // recorder/Lua sidechannel still needs event-loop service. Its
            // bounded lossless queue otherwise fills after eight PTY chunks
            // and blocks parsing indefinitely while the window is occluded.
            pane.term
                .set_output_wake_enabled(output_wake_transport_enabled(
                    render_quiesced,
                    pane.output_rx.is_some(),
                ));
        }
    }

    pub(super) fn acknowledge_output_wakes(&self, ws: &WindowState) {
        for pane in ws.mux.panes.values() {
            pane.term.acknowledge_output_wake();
        }
    }

    pub(super) fn user_event_inner(
        &mut self,
        ws: &mut WindowState,
        _el: &ActiveEventLoop,
        ev: UserEvent,
    ) {
        ws.reuse_pane_snapshots_once = false;
        match ev {
            #[cfg(target_os = "macos")]
            UserEvent::ProfilesReady => {}
            #[cfg(target_os = "macos")]
            UserEvent::DockReopen
            | UserEvent::DockReopenReady(_)
            | UserEvent::DockReopenFinished(_) => {}
            #[cfg(target_os = "linux")]
            UserEvent::Wayland | UserEvent::BlurDiscovered => {}
            // Consumed by the outer window-less dispatch before a WindowState
            // is checked out.
            UserEvent::CardsTipClaimed(_)
            | UserEvent::GpusDetected(_)
            | UserEvent::ShellsDetected
            | UserEvent::SystemSettingsChanged(_)
            | UserEvent::ToggleQuickTerminal
            | UserEvent::Activation
            | UserEvent::PastedImageEncoded
            | UserEvent::NativeFullscreenExited { .. }
            | UserEvent::AccessibilityActivated { .. }
            | UserEvent::RemoteScanReady
            | UserEvent::MediaRendered
            | UserEvent::LanePlayback
            | UserEvent::UpdateToastClaimed { .. } => {}
            UserEvent::AccessibilityAction { request, .. } => {
                self.handle_accessibility_action(ws, request, _el);
            }
            UserEvent::VideoPreviewReady {
                window_seq,
                pane_id,
                generation,
                candidate,
                preview,
            } => {
                if window_seq != ws.seq {
                    return;
                }
                use crate::window_state::VideoPreviewOutcome;
                let poster = preview.is_some();
                let outcome = ws.finish_pending_video_preview(
                    pane_id,
                    generation,
                    candidate,
                    preview,
                    std::time::Instant::now(),
                );
                log::debug!("video paste receipt for pane {pane_id}: {outcome:?}, poster {poster}");
                if matches!(
                    outcome,
                    VideoPreviewOutcome::Failed | VideoPreviewOutcome::Shown
                ) && let Some(window) = &ws.window
                {
                    window.request_redraw();
                }
            }
            UserEvent::Wakeup => {
                for pane in ws.mux.panes.values() {
                    let _ = pane.retry_pending_reports();
                }
                // Semantic terminal events and direct-child lifecycle edges
                // are not paint damage. Drain them before the output-generation
                // gate so a quiet exit is applied even when the window is
                // hidden, minimized, or has cursor blinking disabled.
                if take_window_lifecycle_wakes(ws) {
                    self.drain_events(ws);
                }
                // Apply current visibility/recovery policy, but leave enabled
                // latches closed until redraw is ready to snapshot a frame.
                // This prevents one queued event from reopening the flood gate
                // before the deferred paint budget expires.
                self.sync_output_wake_gate(ws);
                // Wakeups fan out to every window; skip windows whose
                // panes produced no output since their last paint (plain
                // text emits no TermEvent — the generation counter is the
                // only reliable per-window signal).
                if !output_wakeup_needs_paint(
                    || window_has_new_output(ws),
                    || self.acknowledge_output_wakes(ws),
                ) {
                    return;
                }
                // Run output triggers before the redraw —
                // a match fires window urgency so the user notices the
                // event even if they're focused on another OS window.
                // Cheap when triggers are empty (which is the default).
                self.run_triggers(ws);
                if output_wakeup_must_quiesce(
                    window_is_render_hidden(ws),
                    ws.frame_recovery.has_pending(),
                    ws.renderer.is_none() || self.gpu.as_ref().is_some_and(|gpu| gpu.is_lost()),
                ) {
                    if ws.mux.panes.values().any(|pane| pane.output_rx.is_some()) {
                        // Rearm before draining. Output racing after this point
                        // owns a new wake; output already queued is consumed by
                        // this nonblocking drain. Reversing the order leaves a
                        // drain/ack race that can strand the next lossless
                        // chunk with no event-loop wake.
                        self.acknowledge_output_wakes(ws);
                        self.drain_recorder_output_once(ws);
                    }
                    // Keep terminal damage pending while the surface is
                    // occluded, minimized, explicitly invisible, or while its
                    // one-shot repair deadline is armed.
                    // Output bursts must not bypass timeout backoff.
                    ws.output_pacer.reset();
                    return;
                }
                // Coalesce rapid PTY-output paints to ~one per
                // frame budget so a non-atomic repaint burst settles before we
                // snapshot the grid. When deferred, `about_to_wait` schedules
                // the flush at the budget deadline so the final frame still
                // paints. Input/cursor paints don't come through here, so
                // typing and the cursor stay immediate.
                let now = std::time::Instant::now();
                // Keystroke ECHO paints immediately. request_redraw is
                // vsync-coalesced, so this can't outpace the display, and the
                // coalescer's job (letting a NON-atomic output burst settle
                // before the snapshot) doesn't apply to a few echoed cells.
                // Routing echo through the WaitUntil deadline (~16ms timer
                // granularity on Windows, frequently late) makes held-key
                // repeat visibly stutter.
                if typed_recently(now, ws.last_typed, TYPING_ECHO_WINDOW) {
                    ws.output_pacer.reset();
                    observe_typed_echo(ws, now);
                    note_typed_echo_wake(ws);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                } else if should_defer_output_paint(
                    now,
                    ws.last_paint,
                    effective_output_budget(ws.output_frame_budget, ws.flood_paints),
                ) {
                    ws.output_pacer.defer();
                } else if let Some(w) = &ws.window {
                    ws.output_pacer.queue_deferred_redraw();
                    w.request_redraw();
                }
            }
            // Consumed by the outer process-level debounce dispatch.
            UserEvent::ReloadConfig => {}
            UserEvent::RemoteCommand => self.drain_remote_commands(ws),
            UserEvent::Ctl => self.drain_ctl(ws, _el),
            UserEvent::UpdateAvailable { tag, notify } => {
                let _ = (tag, notify);
                self.request_update_facts(true);
            }
            UserEvent::UpdateCheckFinished => self.request_update_facts(true),
            UserEvent::UpdateFacts(facts) => {
                if self.update_facts_refresh.complete() {
                    self.apply_update_facts(ws, *facts);
                } else {
                    self.request_update_facts(true);
                }
            }
            UserEvent::UpdateInstalled {
                tag,
                staged,
                first_time,
                notify,
            } => {
                self.request_update_facts(true);
                let _ = (tag, staged, first_time, notify);
            }
            UserEvent::UpdateFailed { tag, notify } => {
                self.request_update_facts(true);
                let _ = (tag, notify);
            }
            UserEvent::UpdateRestartPending { installed } => {
                // Update facts project a foreground toast. Log each
                // version once rather than on every hourly check.
                if self.logged_restart_pending.as_deref() != Some(installed.as_str()) {
                    log::info!(
                        "kettle {installed} is installed; this process runs {} until it restarts",
                        env!("CARGO_PKG_VERSION")
                    );
                    self.logged_restart_pending = Some(installed);
                }
                self.request_update_facts(true);
            }
            UserEvent::UpdateCheckTick => {
                crate::update_check::maybe_spawn_check(
                    self.proxy.clone(),
                    env!("CARGO_PKG_VERSION"),
                    self.startup.update_install.clone(),
                    self.cfg.update_policy,
                    self.cfg.update_check_interval_hours,
                );
                // Another process may have installed, dismissed or failed.
                self.request_update_facts(true);
            }
            #[cfg(target_os = "macos")]
            UserEvent::MenuCommand(action) => {
                self.handle_action(ws, action, _el);
            }
            // Every Dock row maps onto an action kettle already has, so this
            // reuses the keybind path wholesale — including NewWindow's
            // fall-back to a new tab when a window cannot be created.
            #[cfg(target_os = "macos")]
            UserEvent::DockCommand(command) => {
                if command == crate::macos_dock::DockCommand::NewTab {
                    self.reveal_window(ws);
                }
                self.handle_action(ws, command.action(), _el);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod exit_behavior_tests {
    use super::*;

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready() {
            assert!(
                std::time::Instant::now() < deadline,
                "PTY fixture timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn held_window(policy: kettle_config::ExitAction) -> (WindowState, u64) {
        let cfg = Config {
            shell_integration: false,
            login_shell: false,
            ..Config::default()
        };
        let mut mux = Mux::new();
        mux.new_tab_with_geometry(
            &cfg,
            PtyGeometry::from_cell_size(80, 24, 8, 16),
            Arc::new(|| {}),
            &[
                "/bin/sh".into(),
                "-c".into(),
                "printf failure; exit 7".into(),
            ],
            None,
        )
        .unwrap();
        let id = mux.active_focus().unwrap();
        let mut ws = WindowState::new(1, mux);
        wait_until(|| {
            drain_pane_events(&mut ws, policy, false, |_, _| None);
            ws.mux.panes[&id].held
        });
        (ws, id)
    }

    #[test]
    fn held_pane_survives_late_input_failure_through_event_drain() {
        for policy in [
            kettle_config::ExitAction::Hold,
            kettle_config::ExitAction::CloseOnSuccess,
        ] {
            for key_release in [false, true] {
                let (mut ws, id) = held_window(policy);
                let output = ws.mux.panes[&id].term.screen_text(0).unwrap().text;
                let pane = ws.mux.panes.get_mut(&id).unwrap();
                pane.disconnect_pty_input_for_test();
                let result = if key_release {
                    use alacritty_terminal::vte::ansi::{Handler, KeyboardModes};
                    pane.term
                        .term
                        .lock()
                        .unwrap()
                        .push_keyboard_mode(KeyboardModes::REPORT_EVENT_TYPES);
                    pane.feed_key_release(
                        winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyA),
                        Arc::from(&b"\x1b[97;1:3u"[..]),
                    )
                } else {
                    pane.queue_protocol_reply(b"late-reply")
                };
                assert!(result.is_failed());
                assert!(pane.pty_input_failed());
                drain_pane_events(&mut ws, policy, false, |_, _| None);
                assert!(
                    !ws.mux.panes[&id].closed,
                    "held pane closed after late input failure"
                );
                assert!(!ws.mux.reap().layout_changed);
                assert_eq!(ws.mux.panes[&id].term.screen_text(0).unwrap().text, output);
            }
        }
    }

    #[test]
    fn held_pane_survives_reply_failure_during_event_drain() {
        for policy in [
            kettle_config::ExitAction::Hold,
            kettle_config::ExitAction::CloseOnSuccess,
        ] {
            let (mut ws, id) = held_window(policy);
            let output = ws.mux.panes[&id].term.screen_text(0).unwrap().text;
            let pane = ws.mux.panes.get_mut(&id).unwrap();
            pane.disconnect_pty_input_for_test();
            let (tx, rx) = crossbeam_channel::bounded(1);
            pane.rx = rx;
            tx.send(TermEvent::PtyWrite("late-reply".into())).unwrap();
            assert!(!pane.pty_input_failed());
            drain_pane_events(&mut ws, policy, false, |_, event| match event {
                TermEvent::PtyWrite(reply) => Some(reply),
                _ => None,
            });
            assert!(ws.mux.panes[&id].pty_input_failed());
            assert!(!ws.mux.panes[&id].closed);
            assert!(!ws.mux.reap().layout_changed);
            assert_eq!(ws.mux.panes[&id].term.screen_text(0).unwrap().text, output);
        }
    }

    #[test]
    fn quiet_exit_publishes_held_banner_without_new_output() {
        for policy in [
            kettle_config::ExitAction::Hold,
            kettle_config::ExitAction::CloseOnSuccess,
        ] {
            let cfg = Config {
                shell_integration: false,
                login_shell: false,
                ..Config::default()
            };
            let mut mux = Mux::new();
            mux.new_tab_with_geometry(
                &cfg,
                PtyGeometry::from_cell_size(80, 24, 8, 16),
                Arc::new(|| {}),
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "stty -echo; printf failure; read line; exit 7".into(),
                ],
                None,
            )
            .unwrap();
            let id = mux.active_focus().unwrap();
            let mut ws = WindowState::new(1, mux);
            wait_until(|| {
                ws.mux.panes[&id]
                    .term
                    .screen_text(0)
                    .unwrap()
                    .text
                    .contains("failure")
            });
            wait_until(|| ws.mux.panes[&id].term.output_generation() != 0);
            let generation = ws.mux.panes[&id].term.output_generation();
            ws.seen_output_gen.insert(id, generation);
            ws.accessibility_pending = false;
            assert!(ws.mux.panes[&id].feed_input(b"\n").is_queued());
            // Reap status first to cover EOF arriving with status already available.
            wait_until(|| ws.mux.panes[&id].term.child_exited());
            wait_until(|| {
                drain_pane_events(&mut ws, policy, false, |_, _| None);
                ws.mux.panes[&id].held
            });
            assert_eq!(ws.mux.panes[&id].exit_code, Some(7));
            assert_eq!(ws.mux.panes[&id].term.output_generation(), generation);
            assert!(!window_has_new_output(&ws));
            assert!(
                ws.accessibility_pending,
                "quiet hold must publish the banner and controls"
            );
            // Once published, another drain must settle rather than invalidate forever.
            ws.accessibility_pending = false;
            drain_pane_events(&mut ws, policy, false, |_, _| None);
            assert!(!ws.accessibility_pending);
        }
    }

    #[test]
    fn held_restart_preserves_completion_capability_after_reload() {
        use kettle_config::CompletionOverlayMode::{Auto, Off};
        for (original, reloaded) in [(Auto, Off), (Off, Auto)] {
            let cfg = Config {
                completion_overlay: original,
                shell_integration: false,
                login_shell: false,
                ..Config::default()
            };
            let mut mux = Mux::new();
            let geometry = PtyGeometry::from_cell_size(80, 24, 8, 16);
            mux.new_tab_with_geometry(
                &cfg,
                geometry,
                Arc::new(|| {}),
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'completion=%s' \"$KETTLE_COMPLETION_OVERLAY\"; exit 7".into(),
                ],
                None,
            )
            .unwrap();
            let id = mux.active_focus().unwrap();
            wait_until(|| mux.panes[&id].term.take_exited());
            let output = mux.panes[&id].term.screen_text(0).unwrap().text;
            let capability = mux.panes[&id].completion_overlay;
            assert_eq!(capability, original == Auto);
            assert!(output.contains(if capability {
                "completion=1"
            } else {
                "completion=0"
            }));
            apply_pane_exit_action(
                kettle_config::ExitAction::Hold,
                id,
                mux.panes.get_mut(&id).unwrap(),
                &mut Vec::new(),
            );
            wait_until(|| !mux.poll_held_child_statuses());
            let changed = Config {
                completion_overlay: reloaded,
                ..cfg
            };
            assert!(
                mux.restart_held(id, &changed, geometry, Arc::new(|| {}))
                    .unwrap()
            );
            wait_until(|| mux.panes[&id].term.take_exited());
            assert_eq!(
                mux.panes[&id].term.screen_text(0).unwrap().text,
                output,
                "replayed shell still owns its original completion handlers"
            );
            assert_eq!(
                mux.panes[&id].completion_overlay, capability,
                "UI must match the replayed shell capability"
            );
        }
    }

    #[test]
    fn shell_exit_policy_preserves_failure_output_and_restarts_original_launch_in_place() {
        let root = tempfile::tempdir().unwrap();
        for (policy, code, held) in [
            (kettle_config::ExitAction::CloseOnSuccess, 0, false),
            (kettle_config::ExitAction::CloseOnSuccess, 7, true),
            (kettle_config::ExitAction::Hold, 0, true),
            (kettle_config::ExitAction::Hold, 9, true),
            (kettle_config::ExitAction::Close, 9, false),
        ] {
            let cfg = Config {
                env: vec![("KETTLE_EXIT_FIXTURE".into(), "original".into())],
                shell_integration: false,
                login_shell: false,
                ..Config::default()
            };
            let argv = vec![
                "/bin/sh".into(),
                "-c".into(),
                format!(
                    "printf 'value=%s\\ncwd=%s\\n' \"$KETTLE_EXIT_FIXTURE\" \"$PWD\"; exit {code}"
                ),
            ];
            let mut mux = Mux::new();
            let geometry = PtyGeometry::from_cell_size(120, 24, 8, 16);
            mux.new_tab_with_geometry(&cfg, geometry, Arc::new(|| {}), &argv, root.path().to_str())
                .expect("PTY fixture");
            let id = mux.active_focus().unwrap();
            let wait = |pane: &Pane| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while !pane.term.take_exited() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "ordered PTY exit timed out"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            };
            wait(&mux.panes[&id]);
            let mut pending = Vec::new();
            apply_pane_exit_action(policy, id, mux.panes.get_mut(&id).unwrap(), &mut pending);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while mux.poll_held_child_statuses() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(!mux.panes[&id].closed, held);
            assert!(pending.is_empty());
            if held {
                assert_eq!(mux.panes[&id].exit_code, Some(code));
                let original = mux.panes[&id].term.screen_text(0).unwrap().text;
                assert!(original.contains("value=original"), "{original}");
                assert!(
                    original.contains(&format!(
                        "cwd={}",
                        root.path().canonicalize().unwrap().display()
                    )),
                    "{original}"
                );
                assert!(!mux.reap().layout_changed);
                let changed = Config {
                    env: vec![("KETTLE_EXIT_FIXTURE".into(), "changed".into())],
                    shell: Some("/no/such/program".into()),
                    ..Config::default()
                };
                assert!(
                    mux.restart_held(id, &changed, geometry, Arc::new(|| {}))
                        .unwrap()
                );
                assert_eq!(mux.active_focus(), Some(id));
                assert_eq!(mux.tabs.len(), 1);
                assert!(!mux.panes[&id].held);
                wait(&mux.panes[&id]);
                let restarted = mux.panes[&id].term.screen_text(0).unwrap().text;
                assert_eq!(
                    restarted, original,
                    "restart must preserve command, cwd and environment"
                );
                apply_pane_exit_action(policy, id, mux.panes.get_mut(&id).unwrap(), &mut pending);
                apply_pane_exit_action(policy, id, mux.panes.get_mut(&id).unwrap(), &mut pending);
                assert!(pending.is_empty(), "duplicate exit is idempotent");
            } else {
                assert!(mux.reap().mux_empty);
            }
        }
    }
}

#[cfg(all(test, unix))]
#[test]
fn a_failed_held_pane_restart_retains_output_and_pane_identity() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("once");
    std::fs::write(&script, "#!/bin/sh\nprintf 'final-output\\n'\nexit 3\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cfg = Config::default();
    let geometry = PtyGeometry::from_cell_size(80, 24, 8, 16);
    let mut mux = Mux::new();
    mux.new_tab_with_geometry(
        &cfg,
        geometry,
        Arc::new(|| {}),
        &[script.to_string_lossy().into_owned()],
        root.path().to_str(),
    )
    .unwrap();
    let id = mux.active_focus().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !mux.panes[&id].term.take_exited() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    apply_pane_exit_action(
        kettle_config::ExitAction::Hold,
        id,
        mux.panes.get_mut(&id).unwrap(),
        &mut Vec::new(),
    );
    while mux.poll_held_child_statuses() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let output = mux.panes[&id].term.screen_text(0).unwrap().text;
    std::fs::remove_file(script).unwrap();
    assert!(
        mux.restart_held(id, &cfg, geometry, Arc::new(|| {}))
            .is_err()
    );
    assert_eq!(mux.active_focus(), Some(id));
    assert!(mux.panes[&id].held);
    assert_eq!(mux.panes[&id].term.screen_text(0).unwrap().text, output);
}
