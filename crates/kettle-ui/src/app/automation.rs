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
                    PaneInputResult::Oversize | PaneInputResult::Failed => {
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
    pub(super) fn fire_tab_close_event(&mut self, ws: &WindowState, closing_idx: usize) {
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
                    PaneInputResult::Oversize | PaneInputResult::Failed => {
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
