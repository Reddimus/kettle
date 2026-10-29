//! Lua plugin hooks, the remote command spool and output triggers.

use super::*;

pub(super) fn trigger_snapshot(terminal: &kettle_core::Terminal) -> Option<String> {
    let t = terminal.term.lock().ok()?;
    use kettle_core::Dimensions;
    let rows = t.screen_lines();
    let cols = t.columns();
    let from = rows.saturating_sub(50);
    let mut s = String::with_capacity((rows - from) * cols);
    for r in from..rows {
        kettle_core::grid_text::append_row_text(t.grid(), r as i32, cols, &mut s);
        // Trim the row's grid padding BEFORE the newline, or a
        // capture group like `(.+)$` embeds up to a full row of
        // trailing spaces in the spawned command's argv. The
        // previous row already ends in '\n', so the trim never
        // eats earlier rows.
        let keep = s.trim_end_matches(' ').len();
        s.truncate(keep);
        s.push('\n');
    }
    Some(s)
}

impl App {
    /// Drain commands a Lua callback (event hook or menu-item) just enqueued.
    /// Handles every LuaCommand variant: SendText, ExecAction, Notify, SetTheme.
    ///
    /// This is the shared drain path. `fire_lua_event` routes every event hook
    /// here, and the URL handler and the lua menu-item click call it directly.
    /// Startup in `run_with` cannot use it (early init operates on locals
    /// before `self` exists); every other LuaCommand consumer routes here.
    ///
    /// `hook_name` is the hook label used in unknown-action warn
    /// messages (e.g. "tab_add hook", "lua menu-item").
    pub(super) fn drain_lua_hook_commands(&mut self, ws: &WindowState, hook_name: &str) {
        let commands = self
            .lua_engine
            .as_ref()
            .map(crate::LuaEngine::drain_commands)
            .unwrap_or_default();
        let mut admitted = false;
        for command in commands {
            let pending = match command {
                crate::LuaCommand::SendText(text) => PendingLuaCommand::SendText {
                    origin_window: ws.seq,
                    // Resolve only when this FIFO entry reaches the head: an
                    // earlier Lua action may intentionally change focus first.
                    // The first attempt latches the id so a retry cannot type
                    // into a different pane after a focus change.
                    target_pane: None,
                    bytes: Arc::from(text.into_bytes()),
                },
                crate::LuaCommand::ExecAction(name) => {
                    let Some(action) = kettle_config::Action::from_name(&name) else {
                        log::warn!("lua kettle.exec_action ({hook_name}): unknown action {name:?}");
                        continue;
                    };
                    PendingLuaCommand::ExecAction {
                        origin_window: ws.seq,
                        action,
                    }
                }
                crate::LuaCommand::Notify { title, body } => PendingLuaCommand::Notify {
                    origin_window: ws.seq,
                    title,
                    body,
                },
                crate::LuaCommand::CallbackStopped { event, dropped } => {
                    PendingLuaCommand::CallbackStopped {
                        origin_window: ws.seq,
                        event,
                        dropped,
                    }
                }
                crate::LuaCommand::SetTheme(name) => PendingLuaCommand::SetTheme {
                    origin_window: ws.seq,
                    name,
                },
            };
            let result = self.pending_lua_commands.push(pending);
            admitted |= result == PaneInputResult::Queued;
            self.report_input_result(result);
        }
        if admitted {
            self.lua_command_retry
                .expedite_if_idle(std::time::Instant::now());
        }
    }

    /// Fire one Lua event and immediately transfer every side effect it queued
    /// into App's bounded FIFO. Keeping the pair inseparable prevents a callback
    /// from accumulating commands until an unrelated event happens to drain it.
    pub(super) fn fire_lua_event(
        &mut self,
        ws: &WindowState,
        event: crate::LuaEvent,
        hook_name: &str,
    ) {
        if let Some(engine) = &self.lua_engine {
            engine.fire_event(&event);
        }
        self.drain_lua_hook_commands(ws, hook_name);
    }

    pub(super) fn report_lua_target_gone(&mut self, pane_id: u64) {
        let now = std::time::Instant::now();
        if should_notify_input_rejection(
            self.last_input_rejection_notice,
            PaneInputResult::Failed,
            now,
        ) {
            let message = format!(
                "A queued Lua send_text target pane ({pane_id}) closed before the command could be delivered. The command was dropped and was not rerouted."
            );
            log::warn!("{message}");
            fire_notify(
                self.ui_text
                    .text(kettle_i18n::Text::NotifyTitleLuaNotDelivered),
                &self.ui_text.notify_body_lua_not_delivered(pane_id),
            );
            self.last_input_rejection_notice = Some((PaneInputResult::Failed, now));
        }
    }

    /// Run a bounded slice of the process-global Lua side-effect FIFO for this
    /// window. A stalled PTY retains the head message byte-for-byte and arms an
    /// exponential retry; later commands cannot overtake it.
    pub(super) fn poll_pending_lua_commands(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        if self.pending_lua_commands.is_empty() {
            self.lua_command_retry.progressed();
            return None;
        }
        if !self.lua_command_retry.is_ready(now) {
            return self.lua_command_retry.remaining(now);
        }

        let mut processed = 0usize;
        let mut processed_send_bytes = 0usize;
        while processed < AUTOMATION_WORK_ITEMS_PER_TURN
            && processed_send_bytes < AUTOMATION_WORK_BYTES_PER_TURN
        {
            let Some(origin_window) = self
                .pending_lua_commands
                .front()
                .map(PendingLuaCommand::origin_window)
            else {
                break;
            };
            if origin_window != ws.seq {
                if !self.windows.contains_key(&origin_window) {
                    log::warn!(
                        "dropping queued Lua command because its originating window \
                         {origin_window} no longer exists"
                    );
                    self.pending_lua_commands.pop_front();
                    self.lua_command_retry.progressed();
                    processed += 1;
                    continue;
                }
                // The outer multi-window pass will dispatch this FIFO head when
                // it reaches the owning window. A 1 ms deadline covers the case
                // where that window was already visited in this pass.
                return Some(AUTOMATION_YIELD);
            }

            if matches!(
                self.pending_lua_commands.front(),
                Some(PendingLuaCommand::SendText { .. })
            ) {
                let fallback = ws.mux.active_focus();
                let target_pane = match self.pending_lua_commands.front_mut() {
                    Some(PendingLuaCommand::SendText { target_pane, .. }) => {
                        if target_pane.is_none() {
                            *target_pane = fallback;
                        }
                        *target_pane
                    }
                    _ => unreachable!("front was checked as SendText"),
                };
                let Some(target_pane) = target_pane else {
                    self.lua_command_retry.backpressured(now);
                    return self.lua_command_retry.remaining(now);
                };
                let bytes = match self.pending_lua_commands.front() {
                    Some(PendingLuaCommand::SendText { bytes, .. }) => bytes.clone(),
                    _ => unreachable!("front was checked as SendText"),
                };
                // An action earlier in this batch may have moved focus.
                self.sync_pane_focus_reports(ws);
                let result = if let Some(pane) = ws.mux.panes.get(&target_pane) {
                    Some(pane.feed_input_shared(bytes.clone()))
                } else {
                    self.windows
                        .values()
                        .find_map(|window| window.mux.panes.get(&target_pane))
                        .map(|pane| pane.feed_input_shared(bytes.clone()))
                };
                let Some(result) = result else {
                    self.pending_lua_commands.pop_front();
                    self.lua_command_retry.progressed();
                    self.report_lua_target_gone(target_pane);
                    processed += 1;
                    processed_send_bytes += bytes.len();
                    continue;
                };
                match result {
                    PaneInputResult::Queued | PaneInputResult::ReadOnly => {
                        self.pending_lua_commands.pop_front();
                        self.lua_command_retry.progressed();
                        processed += 1;
                        processed_send_bytes += bytes.len();
                    }
                    PaneInputResult::Backpressured => {
                        self.report_input_result(result);
                        self.lua_command_retry.backpressured(now);
                        return self.lua_command_retry.remaining(now);
                    }
                    PaneInputResult::Oversize
                    | PaneInputResult::PathNotTyped
                    | PaneInputResult::Failed => {
                        self.pending_lua_commands.pop_front();
                        self.lua_command_retry.progressed();
                        self.report_input_result(result);
                        processed += 1;
                        processed_send_bytes += bytes.len();
                    }
                }
                continue;
            }

            let Some(command) = self.pending_lua_commands.pop_front() else {
                break;
            };
            self.lua_command_retry.progressed();
            processed += 1;
            match command {
                PendingLuaCommand::ExecAction { action, .. } => {
                    self.lua_driving = true;
                    self.handle_action(ws, action, event_loop);
                    self.lua_driving = false;
                    if self.window_close_pending(ws.seq) || self.quit_requested {
                        break;
                    }
                }
                PendingLuaCommand::Notify { title, body, .. } => {
                    fire_notify(&title, &body);
                }
                PendingLuaCommand::CallbackStopped { event, dropped, .. } => {
                    fire_notify(
                        self.ui_text
                            .text(kettle_i18n::Text::NotifyTitleLuaCallbackStopped),
                        &self
                            .ui_text
                            .notify_body_lua_callback_stopped(dropped, &event),
                    );
                }
                PendingLuaCommand::SetTheme { name, .. } => {
                    if let Some(canonical) = kettle_config::Theme::find_name(&name) {
                        self.set_runtime_theme_name(ws, canonical);
                    } else {
                        log::warn!("lua kettle.set_theme: unknown theme {name:?}");
                    }
                }
                PendingLuaCommand::SendText { .. } => {
                    unreachable!("SendText is handled without removing a retryable head")
                }
            }
        }

        if self.pending_lua_commands.is_empty() {
            self.lua_command_retry.progressed();
            None
        } else {
            self.lua_command_retry.yield_soon(now);
            self.lua_command_retry.remaining(now)
        }
    }

    /// Fire LuaEvent::TabAdd + drain commands.
    /// Every Mux::new_tab / new_tab_with call site that USER-VISIBLY
    /// creates a tab (i.e. NOT startup-time first-tab init before
    /// plugins load) should call this. Centralizes the plugin-
    /// contract dispatch so future new_tab callers can't drift. It also
    /// recomputes chrome hover from the new layout: on a vertical tab bar the
    /// `+` moves down one row when a tab is added, so retaining the old hover
    /// would leave the relocated button highlighted and the cursor as a hand
    /// while the pointer is actually over the new tab.
    pub(super) fn fire_tab_add_event(&mut self, ws: &mut WindowState) {
        if let Some(rec) = self.recorder.as_mut() {
            rec.record_marker("kettle:tab_add");
        }
        self.fire_lua_event(ws, crate::LuaEvent::TabAdd(ws.mux.active), "tab_add hook");
        self.sync_cursor_icon(ws);
    }

    /// Fire LuaEvent::TabClose + drain commands.
    /// Every close_tab call site should call this so plugins
    /// listening for tab_close see every close regardless of
    /// trigger source.
    pub(super) fn fire_tab_close_event(&mut self, ws: &mut WindowState, closing_idx: usize) {
        // Its panes go first: `pane_close` reaches every pane that ends.
        self.fire_closed_pane_events(ws);
        if let Some(rec) = self.recorder.as_mut() {
            rec.record_marker("kettle:tab_close");
        }
        self.fire_lua_event(ws, crate::LuaEvent::TabClose(closing_idx), "tab_close hook");
    }

    /// Fire LuaEvent::PaneClose + drain commands — the pane analog
    /// of `fire_tab_close_event`. Every close-pane call site calls this with
    /// the id captured *before* `Mux::close_focused` removes the pane, so
    /// plugins listening for `pane_close` see the right id regardless of
    /// trigger source (keybind, confirm dialog, menu).
    pub(super) fn fire_pane_close_event(&mut self, ws: &WindowState, pane_id: u64) {
        self.fire_lua_event(ws, crate::LuaEvent::PaneClose(pane_id), "pane_close hook");
    }

    /// Fire `pane_close` for the panes the mux destroyed itself: a closed
    /// tab's, and ones reaped after their shell exited. Plugins that keep
    /// per-pane state rely on seeing every pane end.
    pub(super) fn fire_closed_pane_events(&mut self, ws: &mut WindowState) {
        for pane_id in ws.mux.take_closed_panes() {
            self.fire_pane_close_event(ws, pane_id);
        }
    }

    pub(super) fn sync_lua_active_theme(&self) {
        if let Some(engine) = &self.lua_engine
            && let Err(error) = engine.set_active_theme(&self.cfg.theme_name)
        {
            log::warn!(
                "active theme changed, but kettle.theme() could not be synchronized: {error:#}"
            );
        }
    }

    /// Claim one bounded remote-command batch and stage it for ordered
    /// dispatch. Sender and receiver share a cross-process lock; PTY
    /// backpressure retains the FIFO head and every suffix for a later retry.
    ///
    /// Current senders emit `send-text-json JSON_STRING`, whose decoded string
    /// is written byte-for-byte to the focused pane's PTY. The legacy
    /// `send-text TEXT` form remains accepted and decodes each `\n` character
    /// pair to newline. Unknown or malformed lines contribute only to one
    /// payload-free warning for the batch and do not count as operations.
    pub(super) fn claim_remote_command_batch(
        &mut self,
        origin_window: u64,
        now: std::time::Instant,
    ) {
        let Some(path) = self.startup.remote_file.clone() else {
            return;
        };
        if !self.pending_remote_commands.is_empty() {
            return;
        }
        match claim_remote_command_file(&path) {
            Ok(RemoteBatchClaim::Empty) => {
                self.remote_spool_claim_pending = false;
                self.remote_command_retry.progressed();
            }
            Ok(RemoteBatchClaim::Busy) => {
                self.remote_command_retry.backpressured(now);
            }
            Ok(RemoteBatchClaim::RejectedMalformed) => {
                self.remote_spool_claim_pending = false;
                log::warn!(
                    "remote-command file at {} was not valid UTF-8 and was cleared without processing",
                    path.display()
                );
                self.remote_command_retry.progressed();
            }
            Ok(RemoteBatchClaim::RejectedOversize) => {
                self.remote_spool_claim_pending = false;
                log::warn!(
                    "remote-command file at {} exceeded the {}-byte \
                     safety limit and was cleared without processing",
                    path.display(),
                    kettle_state::MAX_REMOTE_COMMAND_BYTES
                );
                self.remote_command_retry.progressed();
            }
            Ok(RemoteBatchClaim::Claimed(text)) => {
                self.remote_spool_claim_pending = false;
                self.remote_command_retry.progressed();
                match parse_remote_command_batch(&text, origin_window) {
                    ParsedRemoteCommandBatch::Accepted {
                        commands,
                        unknown_lines,
                    } => {
                        if unknown_lines != 0 {
                            log::warn!(
                                "remote-command batch at {} ignored {unknown_lines} \
                                 unrecognized line(s)",
                                path.display()
                            );
                        }
                        self.pending_remote_commands = commands;
                        if !self.pending_remote_commands.is_empty() {
                            self.remote_command_retry.expedite_if_idle(now);
                        }
                    }
                    ParsedRemoteCommandBatch::RejectedTooMany {
                        operation_count,
                        unknown_lines,
                    } => {
                        log::warn!(
                            "remote-command batch at {} contained {operation_count} operations \
                             (cap {MAX_REMOTE_COMMANDS_PER_BATCH}) and {unknown_lines} \
                             unrecognized line(s); the whole batch was rejected before dispatch",
                            path.display()
                        );
                    }
                }
            }
            Err(error) => {
                log::warn!(
                    "remote-command file at {} could not be claimed: {error}",
                    path.display()
                );
                // A path/permission/type/read failure is not transient queue
                // backpressure. Stop polling at 4 Hz; a later watcher event
                // represents an external repair and will arm a fresh claim.
                self.remote_spool_claim_pending = false;
                self.remote_command_retry.progressed();
            }
        }
    }

    pub(super) fn report_remote_target_gone(&mut self, pane_id: u64) {
        let now = std::time::Instant::now();
        if should_notify_input_rejection(
            self.last_input_rejection_notice,
            PaneInputResult::Failed,
            now,
        ) {
            let message = format!(
                "A queued remote-file send target pane ({pane_id}) closed before delivery. The command was dropped and was not rerouted."
            );
            log::warn!("{message}");
            fire_notify(
                self.ui_text
                    .text(kettle_i18n::Text::NotifyTitleRemoteNotDelivered),
                &self.ui_text.notify_body_remote_not_delivered(pane_id),
            );
            self.last_input_rejection_notice = Some((PaneInputResult::Failed, now));
        }
    }

    pub(super) fn poll_pending_remote_commands(
        &mut self,
        ws: &mut WindowState,
        now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        if !self.remote_command_retry.is_ready(now) {
            return self.remote_command_retry.remaining(now);
        }
        if self.pending_remote_commands.is_empty() && self.remote_spool_claim_pending {
            self.claim_remote_command_batch(ws.seq, now);
        }
        if self.pending_remote_commands.is_empty() {
            return self.remote_command_retry.remaining(now);
        }

        let mut processed = 0usize;
        let mut processed_send_bytes = 0usize;
        while processed < AUTOMATION_WORK_ITEMS_PER_TURN
            && processed_send_bytes < AUTOMATION_WORK_BYTES_PER_TURN
        {
            let Some(origin_window) = self
                .pending_remote_commands
                .front()
                .map(PendingRemoteCommand::origin_window)
            else {
                break;
            };
            if origin_window != ws.seq {
                if !self.windows.contains_key(&origin_window) {
                    log::warn!(
                        "dropping queued remote command because its originating window \
                         {origin_window} no longer exists"
                    );
                    self.pending_remote_commands.pop_front();
                    self.remote_command_retry.progressed();
                    processed += 1;
                    continue;
                }
                return Some(AUTOMATION_YIELD);
            }

            if matches!(
                self.pending_remote_commands.front(),
                Some(PendingRemoteCommand::SendText { .. })
            ) {
                let fallback = ws.mux.active_focus();
                let target_pane = match self.pending_remote_commands.front_mut() {
                    Some(PendingRemoteCommand::SendText { target_pane, .. }) => {
                        if target_pane.is_none() {
                            *target_pane = fallback;
                        }
                        *target_pane
                    }
                    _ => unreachable!("front was checked as SendText"),
                };
                let Some(target_pane) = target_pane else {
                    self.remote_command_retry.backpressured(now);
                    return self.remote_command_retry.remaining(now);
                };
                let bytes = match self.pending_remote_commands.front() {
                    Some(PendingRemoteCommand::SendText { bytes, .. }) => bytes.clone(),
                    _ => unreachable!("front was checked as SendText"),
                };
                // An action earlier in this batch may have moved focus.
                self.sync_pane_focus_reports(ws);
                let result = if let Some(pane) = ws.mux.panes.get(&target_pane) {
                    Some(pane.feed_input_shared(bytes.clone()))
                } else {
                    self.windows
                        .values()
                        .find_map(|window| window.mux.panes.get(&target_pane))
                        .map(|pane| pane.feed_input_shared(bytes.clone()))
                };
                let Some(result) = result else {
                    self.pending_remote_commands.pop_front();
                    self.remote_command_retry.progressed();
                    self.report_remote_target_gone(target_pane);
                    processed += 1;
                    processed_send_bytes += bytes.len();
                    continue;
                };
                match result {
                    PaneInputResult::Queued | PaneInputResult::ReadOnly => {
                        self.pending_remote_commands.pop_front();
                        self.remote_command_retry.progressed();
                        processed += 1;
                        processed_send_bytes += bytes.len();
                    }
                    PaneInputResult::Backpressured => {
                        self.report_input_result(result);
                        self.remote_command_retry.backpressured(now);
                        return self.remote_command_retry.remaining(now);
                    }
                    PaneInputResult::Oversize
                    | PaneInputResult::PathNotTyped
                    | PaneInputResult::Failed => {
                        self.pending_remote_commands.pop_front();
                        self.remote_command_retry.progressed();
                        self.report_input_result(result);
                        processed += 1;
                        processed_send_bytes += bytes.len();
                    }
                }
                continue;
            }

            let Some(command) = self.pending_remote_commands.pop_front() else {
                break;
            };
            self.remote_command_retry.progressed();
            processed += 1;
            match command {
                PendingRemoteCommand::ToggleWindow { .. } => {
                    // Tri-state dropdown toggle (the Quake / Yakuake / Tilda
                    // tradition). A plain hide/show toggle would hide a visible
                    // but unfocused kettle, though the user usually wants it
                    // back in focus.
                    //
                    //   hidden            → show + raise + focus
                    //   visible + focused → hide
                    //   visible + !focused → raise + focus (don't hide)
                    if let Some(w) = &ws.window {
                        let visible = w.is_visible().unwrap_or(true);
                        let focused = w.has_focus();
                        if !visible {
                            w.set_visible(true);
                            w.focus_window();
                        } else if focused {
                            w.set_visible(false);
                        } else {
                            // Visible but unfocused — bring to front +
                            // focus, don't hide. The common "I clicked
                            // away" case.
                            w.focus_window();
                        }
                    }
                }
                PendingRemoteCommand::NewTab { .. } => {
                    // Terminator parity (remote-control verb):
                    // open a new tab via the remote-control IPC channel.
                    // Mirrors the Action::NewTab dispatch and
                    // also fires LuaEvent::TabAdd so plugins
                    // listening for tab_add see remote-triggered tabs
                    // the same as keyboard ones.
                    let pre = self.focus_key(ws);
                    let area = self.area(ws);
                    let (cols, rows) = self.grid_of(ws, area);
                    let geometry = self.pty_geometry_for_grid(ws, cols, rows);
                    let waker = self.waker();
                    if let Err(e) = ws.mux.new_tab_geometry(&self.cfg, geometry, waker) {
                        log::warn!("remote-control: new-tab failed: {e}");
                    } else {
                        self.note_focus_change(ws, pre);
                        self.fire_tab_add_event(ws);
                    }
                }
                PendingRemoteCommand::SendText { .. } => {
                    unreachable!("SendText is handled without removing a retryable head")
                }
            }
        }
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
        if self.pending_remote_commands.is_empty() {
            self.remote_command_retry.progressed();
            // A watcher event can be consumed while an older suffix is
            // retained. Probe the spool once after progress so that later batch
            // cannot be stranded merely because no new filesystem event fires.
            if self.remote_spool_claim_pending {
                self.claim_remote_command_batch(ws.seq, now);
            }
        }
        if self.pending_remote_commands.is_empty() {
            self.remote_command_retry.remaining(now)
        } else {
            self.remote_command_retry.yield_soon(now);
            self.remote_command_retry.remaining(now)
        }
    }

    pub(super) fn drain_remote_commands(&mut self, ws: &mut WindowState) {
        self.remote_spool_claim_pending = true;
        let _ = self.poll_pending_remote_commands(ws, std::time::Instant::now());
    }

    pub(super) fn run_triggers(&mut self, ws: &mut WindowState) {
        if self.compiled_triggers.is_empty() {
            return;
        }
        // An unset throttle means no trigger has fired since construct/reload,
        // so the first match is immediately eligible.
        let now = std::time::Instant::now();
        if !throttle_elapsed(
            self.last_trigger_fire,
            now,
            std::time::Duration::from_secs(2),
        ) {
            return;
        }
        // Don't pulse the user's own window when it's already focused.
        if ws.window_focused {
            return;
        }
        // Pull each pane's bottom-of-screen text. Visible viewport
        // only — scanning the whole scrollback every wakeup would
        // burn CPU on a chatty pane. Last 50 rows is the typical
        // "what just happened" window.
        let snapshots: Vec<String> = ws
            .mux
            .panes
            .values()
            .filter_map(|pane| trigger_snapshot(&pane.term))
            .collect();
        for snap in &snapshots {
            if let Some(action) = match_triggers(snap, &self.compiled_triggers) {
                match action {
                    kettle_config::TriggerAction::Urgency => {
                        if let Some(w) = &ws.window {
                            use winit::window::UserAttentionType;
                            w.request_user_attention(Some(UserAttentionType::Critical));
                            ws.attention_active = true;
                        }
                    }
                    kettle_config::TriggerAction::RunCommand(argv) => {
                        // Terminator parity, `plugins/run_cmd_on_match.py`:
                        // fire-and-forget spawn. Argv form means no
                        // shell expansion at kettle's layer; the
                        // configured command is treated as data, not
                        // a shell string. Spawn errors are logged
                        // but otherwise ignored so a missing binary
                        // doesn't loop spawn-fail every trigger tick.
                        spawn_trigger_command(&argv);
                    }
                }
                self.last_trigger_fire = Some(std::time::Instant::now());
                break;
            }
        }
    }
}

/// Terminator parity (`plugins/run_cmd_on_match.py`):
/// fire the configured argv as a fire-and-forget subprocess.
///
/// Security posture:
///   - argv form (no shell). The configured command is treated as
///     data, not as a shell string — kettle never invokes `sh -c`
///     on it. A configured `trigger = .* :: rm -rf $HOME` would
///     spawn `rm -rf $HOME` *literally*, with `$HOME` as a literal
///     argv element, not as the user's home dir.
///   - empty argv ⇒ no-op (parser already rejects, but be safe).
///   - spawn errors logged but otherwise ignored so a missing
///     binary doesn't loop-fail.
///
/// Stripped into a free fn so the dispatch arm stays small.
pub(super) fn spawn_trigger_command(argv: &[String]) {
    if argv.is_empty() {
        return;
    }
    static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let Some(slot) = TriggerCommandSlot::acquire(&ACTIVE) else {
        log::warn!("trigger run-command skipped: too many running commands");
        return;
    };
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    // Don't capture stdout/stderr; the child inherits kettle's
    // file descriptors (where applicable). On Unix this means
    // stdout goes to wherever kettle was launched from — which
    // is acceptable for a user-configured fire-and-forget hook.
    if let Err(error) = std::thread::Builder::new()
        .name("kettle-trigger".into())
        .spawn(move || {
            let _slot = slot;
            if let Err(error) = cmd.spawn().and_then(|mut child| child.wait()) {
                log::warn!("trigger run-command spawn/wait failed: {error}");
            }
        })
    {
        log::warn!("trigger run-command reaper failed to start: {error}");
    }
}

pub(super) struct TriggerCommandSlot<'a>(&'a std::sync::atomic::AtomicUsize);

impl<'a> TriggerCommandSlot<'a> {
    pub(super) fn acquire(active: &'a std::sync::atomic::AtomicUsize) -> Option<Self> {
        use std::sync::atomic::Ordering::Relaxed;

        // Rust 1.99 renames fetch_update to try_update; the MSRV (1.95) predates it.
        #[allow(deprecated)]
        let acquired =
            active.fetch_update(Relaxed, Relaxed, |count| (count < 32).then_some(count + 1));
        acquired.ok().map(|_| Self(active))
    }
}

impl Drop for TriggerCommandSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Compile each configured `OutputTrigger`'s pattern to a
/// `regex::Regex`, log + drop invalid patterns. Pure helper so the
/// App constructor + `reload_config` path use exactly the same
/// regex set after a config edit. An empty input returns an empty
/// vec — `match_triggers` short-circuits on that.
pub(super) fn compile_triggers(
    triggers: &[kettle_config::OutputTrigger],
) -> Vec<(regex::Regex, kettle_config::TriggerAction)> {
    let mut out = Vec::with_capacity(triggers.len());
    for t in triggers {
        // Multi-line: the snapshot joins viewport rows with '\n', and `^` / `$`
        // should anchor to each row.
        match regex::RegexBuilder::new(&t.pattern)
            .multi_line(true)
            .build()
        {
            Ok(re) => out.push((re, t.action.clone())),
            Err(e) => {
                log::warn!("trigger pattern {:?} failed to compile: {e}", t.pattern);
            }
        }
    }
    out
}

pub(super) fn resolved_init_lua_script(
    explicit: Option<std::path::PathBuf>,
    default_config: Option<std::path::PathBuf>,
) -> Option<(std::path::PathBuf, ConfigTrust)> {
    explicit
        .map(|path| (path, ConfigTrust::ExplicitPath))
        .or_else(|| {
            default_config
                .and_then(|path| path.parent().map(|dir| dir.join("init.lua")))
                .filter(|path| path.exists())
                .map(|path| (path, ConfigTrust::VerifyDirectory))
        })
}

/// Scan `text` for any compiled trigger match, returning the
/// first action that fires. Pure helper used by `App::run_triggers` and
/// the drift guard. Returns `None` when no trigger fires so the caller
/// can skip the urgency-attention call.
pub(super) fn match_triggers(
    text: &str,
    triggers: &[(regex::Regex, kettle_config::TriggerAction)],
) -> Option<kettle_config::TriggerAction> {
    triggers.iter().find_map(|(re, action)| {
        let caps = re.captures(text)?;
        Some(match action {
            // The matched pattern's capture groups substitute into the
            // command's argv (`{0}` whole match, `{1}`… numbered groups),
            // like `cmd.format(*groups)` in Terminator's `run_cmd_on_match.py`.
            kettle_config::TriggerAction::RunCommand(argv) => {
                kettle_config::TriggerAction::RunCommand(substitute_trigger_groups(argv, &caps))
            }
            other => other.clone(),
        })
    })
}

/// Replace `{0}`/`{1}`… in each argv element with the trigger match's
/// capture groups. `{0}` is the whole match and a non-participating group
/// substitutes empty. An out-of-range reference like `{9}` with two groups
/// stays literal, so the config typo shows in the spawned command instead of
/// silently vanishing. Substitution is per element, so argv stays argv.
/// Matched output can inject an argument's value but never new arguments or
/// shell metacharacters (the spawn is `std::process::Command`, no shell).
/// Pure (unit-tested).
fn substitute_trigger_groups(argv: &[String], caps: &regex::Captures) -> Vec<String> {
    argv.iter()
        .map(|a| {
            if !a.contains('{') {
                return a.clone();
            }
            // Single left-to-right pass over the template. Substituted text is
            // emitted verbatim and never re-scanned, so a capture whose matched
            // text contains `{2}` cannot expand again with attacker-controlled
            // content.
            let bytes = a.as_bytes();
            let mut out = String::with_capacity(a.len());
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'{' {
                    let mut j = i + 1;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j > i + 1
                        && j < bytes.len()
                        && bytes[j] == b'}'
                        && let Ok(idx) = a[i + 1..j].parse::<usize>()
                        && idx < caps.len()
                    {
                        out.push_str(caps.get(idx).map(|m| m.as_str()).unwrap_or(""));
                        i = j + 1;
                        continue;
                    }
                    // Not a valid in-range placeholder: the `{` stays
                    // literal (an out-of-range `{9}` keeps the typo visible).
                    out.push('{');
                    i += 1;
                } else {
                    let ch = a[i..].chars().next().expect("in-bounds char");
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
            out
        })
        .collect()
}
const MAX_PENDING_LUA_COMMANDS: usize = 1024;
const MAX_PENDING_LUA_SEND_BYTES: usize = 8 << 20;
const MAX_LUA_SEND_TEXT_BYTES: usize = 1 << 20;
const AUTOMATION_WORK_ITEMS_PER_TURN: usize = 16;
const AUTOMATION_WORK_BYTES_PER_TURN: usize = 1 << 20;
pub(super) const AUTOMATION_RETRY_MIN: std::time::Duration = std::time::Duration::from_millis(10);
const AUTOMATION_RETRY_MAX: std::time::Duration = std::time::Duration::from_millis(250);
const AUTOMATION_YIELD: std::time::Duration = std::time::Duration::from_millis(1);
pub(super) const MAX_REMOTE_COMMANDS_PER_BATCH: usize = 1024;

#[derive(Debug)]
pub(super) enum PendingLuaCommand {
    SendText {
        origin_window: u64,
        target_pane: Option<u64>,
        bytes: Arc<[u8]>,
    },
    ExecAction {
        origin_window: u64,
        action: Action,
    },
    Notify {
        origin_window: u64,
        title: String,
        body: String,
    },
    /// Kettle's own notice that it retired runaway Lua callbacks.
    CallbackStopped {
        origin_window: u64,
        event: String,
        dropped: u64,
    },
    SetTheme {
        origin_window: u64,
        name: String,
    },
}

impl PendingLuaCommand {
    fn origin_window(&self) -> u64 {
        match self {
            Self::SendText { origin_window, .. }
            | Self::ExecAction { origin_window, .. }
            | Self::Notify { origin_window, .. }
            | Self::CallbackStopped { origin_window, .. }
            | Self::SetTheme { origin_window, .. } => *origin_window,
        }
    }

    fn send_len(&self) -> usize {
        match self {
            Self::SendText { bytes, .. } => bytes.len(),
            _ => 0,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct PendingLuaCommands {
    commands: VecDeque<PendingLuaCommand>,
    pub(super) send_bytes: usize,
}

#[derive(Debug)]
pub(super) enum PendingRemoteCommand {
    SendText {
        origin_window: u64,
        target_pane: Option<u64>,
        bytes: Arc<[u8]>,
    },
    ToggleWindow {
        origin_window: u64,
    },
    NewTab {
        origin_window: u64,
    },
}

impl PendingRemoteCommand {
    fn origin_window(&self) -> u64 {
        match self {
            Self::SendText { origin_window, .. }
            | Self::ToggleWindow { origin_window }
            | Self::NewTab { origin_window } => *origin_window,
        }
    }
}

#[derive(Debug)]
pub(super) enum ParsedRemoteCommandBatch {
    Accepted {
        commands: VecDeque<PendingRemoteCommand>,
        unknown_lines: usize,
    },
    RejectedTooMany {
        operation_count: usize,
        unknown_lines: usize,
    },
}

/// Parse one already byte-bounded spool claim without allowing short control
/// lines to amplify into an unbounded operation queue. The parser scans the
/// full batch to produce one coalesced unknown-line count, but retains at most
/// `MAX_REMOTE_COMMANDS_PER_BATCH` operations. Exceeding that count rejects the
/// whole batch before any retained prefix can be dispatched.
pub(super) fn parse_remote_command_batch(
    text: &str,
    origin_window: u64,
) -> ParsedRemoteCommandBatch {
    let mut commands = VecDeque::new();
    let mut operation_count = 0usize;
    let mut unknown_lines = 0usize;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(payload) = line.strip_prefix("send-text-json ") {
            match serde_json::from_str::<String>(payload) {
                Ok(payload) => {
                    operation_count = operation_count.saturating_add(1);
                    if operation_count <= MAX_REMOTE_COMMANDS_PER_BATCH {
                        commands.push_back(PendingRemoteCommand::SendText {
                            origin_window,
                            target_pane: None,
                            bytes: Arc::from(payload.into_bytes()),
                        });
                    }
                }
                Err(_) => {
                    unknown_lines = unknown_lines.saturating_add(1);
                }
            }
        } else if let Some(payload) = line.strip_prefix("send-text ") {
            operation_count = operation_count.saturating_add(1);
            if operation_count <= MAX_REMOTE_COMMANDS_PER_BATCH {
                commands.push_back(PendingRemoteCommand::SendText {
                    origin_window,
                    target_pane: None,
                    bytes: Arc::from(payload.replace("\\n", "\n").into_bytes()),
                });
            }
        } else if line == "toggle-window" {
            operation_count = operation_count.saturating_add(1);
            if operation_count <= MAX_REMOTE_COMMANDS_PER_BATCH {
                commands.push_back(PendingRemoteCommand::ToggleWindow { origin_window });
            }
        } else if line == "new-tab" {
            operation_count = operation_count.saturating_add(1);
            if operation_count <= MAX_REMOTE_COMMANDS_PER_BATCH {
                commands.push_back(PendingRemoteCommand::NewTab { origin_window });
            }
        } else {
            unknown_lines = unknown_lines.saturating_add(1);
        }
    }
    if operation_count > MAX_REMOTE_COMMANDS_PER_BATCH {
        ParsedRemoteCommandBatch::RejectedTooMany {
            operation_count,
            unknown_lines,
        }
    } else {
        ParsedRemoteCommandBatch::Accepted {
            commands,
            unknown_lines,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum RemoteBatchClaim {
    Empty,
    Busy,
    Claimed(String),
    RejectedMalformed,
    RejectedOversize,
}

pub(super) fn claim_remote_command_file(
    path: &std::path::Path,
) -> std::io::Result<RemoteBatchClaim> {
    use std::io::{Read as _, Seek as _};

    let lock_path = kettle_state::remote_command_lock_path(path);
    let Some(_lock) = kettle_state::ExclusiveFileLock::try_acquire(&lock_path)? else {
        return Ok(RemoteBatchClaim::Busy);
    };
    let mut file = kettle_state::open_existing_private_file(path)?;
    let size = file.metadata()?.len();
    if size > kettle_state::MAX_REMOTE_COMMAND_BYTES {
        file.set_len(0)?;
        return Ok(RemoteBatchClaim::RejectedOversize);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    file.seek(std::io::SeekFrom::Start(0))?;
    file.by_ref()
        .take(kettle_state::MAX_REMOTE_COMMAND_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > kettle_state::MAX_REMOTE_COMMAND_BYTES {
        file.set_len(0)?;
        return Ok(RemoteBatchClaim::RejectedOversize);
    }
    if bytes.is_empty() {
        return Ok(RemoteBatchClaim::Empty);
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            file.set_len(0)?;
            return Ok(RemoteBatchClaim::RejectedMalformed);
        }
    };
    file.set_len(0)?;
    Ok(RemoteBatchClaim::Claimed(text))
}

pub(super) fn should_poll_remote_window(
    has_claimed_fifo: bool,
    claim_pending: bool,
    retry_pending: bool,
    window_seq: u64,
    focused_seq: u64,
) -> bool {
    has_claimed_fifo || ((claim_pending || retry_pending) && window_seq == focused_seq)
}

impl PendingLuaCommands {
    pub(super) fn push(&mut self, command: PendingLuaCommand) -> PaneInputResult {
        let send_len = command.send_len();
        if send_len > MAX_LUA_SEND_TEXT_BYTES {
            return PaneInputResult::Oversize;
        }
        if self.commands.len() >= MAX_PENDING_LUA_COMMANDS
            || self
                .send_bytes
                .checked_add(send_len)
                .is_none_or(|next| next > MAX_PENDING_LUA_SEND_BYTES)
        {
            return PaneInputResult::Backpressured;
        }
        self.send_bytes += send_len;
        self.commands.push_back(command);
        PaneInputResult::Queued
    }

    fn front(&self) -> Option<&PendingLuaCommand> {
        self.commands.front()
    }

    fn front_mut(&mut self) -> Option<&mut PendingLuaCommand> {
        self.commands.front_mut()
    }

    pub(super) fn pop_front(&mut self) -> Option<PendingLuaCommand> {
        let command = self.commands.pop_front()?;
        self.send_bytes = self.send_bytes.saturating_sub(command.send_len());
        Some(command)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.commands.len()
    }
}

#[derive(Debug)]
pub(super) struct AutomationRetry {
    pub(super) next_at: Option<std::time::Instant>,
    backoff: std::time::Duration,
}

impl Default for AutomationRetry {
    fn default() -> Self {
        Self {
            next_at: None,
            backoff: AUTOMATION_RETRY_MIN,
        }
    }
}

impl AutomationRetry {
    pub(super) fn is_ready(&self, now: std::time::Instant) -> bool {
        self.next_at.is_none_or(|deadline| now >= deadline)
    }

    pub(super) fn backpressured(&mut self, now: std::time::Instant) {
        self.next_at = Some(now + self.backoff);
        self.backoff = self.backoff.saturating_mul(2).min(AUTOMATION_RETRY_MAX);
    }

    pub(super) fn progressed(&mut self) {
        self.next_at = None;
        self.backoff = AUTOMATION_RETRY_MIN;
    }

    fn yield_soon(&mut self, now: std::time::Instant) {
        let deadline = now + AUTOMATION_YIELD;
        if self.next_at.is_none_or(|current| deadline < current) {
            self.next_at = Some(deadline);
        }
    }

    pub(super) fn expedite_if_idle(&mut self, now: std::time::Instant) {
        if self.next_at.is_none() {
            self.next_at = Some(now);
        }
    }

    pub(super) fn remaining(&self, now: std::time::Instant) -> Option<std::time::Duration> {
        self.next_at.map(|deadline| {
            deadline
                .saturating_duration_since(now)
                .max(std::time::Duration::from_nanos(1))
        })
    }
}

/// Open the remote-command spool for watching. Commands left after a crash
/// must not replay into a new run, so the spool is emptied, unless another
/// build's primary is still running: the spool is shared, and a command it
/// has not claimed yet is still meant for it.
pub(super) fn initialize_remote_spool(
    path: &std::path::Path,
    another_build_running: bool,
) -> std::io::Result<()> {
    let file = kettle_state::open_private_file(path)?;
    if another_build_running {
        return Ok(());
    }
    file.set_len(0)
}
