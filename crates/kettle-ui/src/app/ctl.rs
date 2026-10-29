//! The control API: requests from `kettle ctl`, the MCP bridge and agents, and their replies.

use super::*;

/// One page of a `read_screen` snapshot: lines `page.offset..end` of `total`,
/// within `MAX_CTL_TEXT_PAGE_BYTES`. A first line longer than that is cut at a
/// character boundary (`text_truncated`). `whole` returns every line instead.
struct ScreenPage {
    text: String,
    end: usize,
    total: usize,
    text_truncated: bool,
}

fn screen_page(screen: &str, page: &kettle_ctl::protocol::PageRequest, whole: bool) -> ScreenPage {
    let total = screen.split_inclusive('\n').count();
    if whole {
        return ScreenPage {
            text: screen.to_owned(),
            end: total,
            total,
            text_truncated: false,
        };
    }
    let start = page.offset.min(total);
    let desired_end = start.saturating_add(page.limit).min(total);
    let mut end = start;
    let mut text = String::new();
    let mut text_truncated = false;
    for line in screen
        .split_inclusive('\n')
        .skip(start)
        .take(desired_end - start)
    {
        if !text.is_empty() && text.len().saturating_add(line.len()) > MAX_CTL_TEXT_PAGE_BYTES {
            break;
        }
        if text.is_empty() && line.len() > MAX_CTL_TEXT_PAGE_BYTES {
            let mut byte_end = MAX_CTL_TEXT_PAGE_BYTES;
            while !line.is_char_boundary(byte_end) {
                byte_end -= 1;
            }
            text.push_str(&line[..byte_end]);
            text_truncated = true;
            end += 1;
            break;
        }
        text.push_str(line);
        end += 1;
    }
    ScreenPage {
        text,
        end,
        total,
        text_truncated,
    }
}

fn command_submission(command: &str) -> String {
    let line = command.strip_suffix('\n').unwrap_or(command);
    let mut line = line.strip_suffix('\r').unwrap_or(line).to_owned();
    line.push('\r');
    line
}

impl App {
    /// Start the control server if the policy allows anything, the first pane
    /// exists, and no start has been tried. A failed start is logged and not
    /// retried by unrelated reloads.
    pub(super) fn ensure_ctl_started(&mut self) {
        if self.ctl_start != CtlStart::Ready || self.ctl.is_some() || !self.ctl_policy.runs_server()
        {
            return;
        }
        self.ctl_start = CtlStart::Attempted;
        let proxy = self.proxy.clone();
        let wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
            let _ = proxy.send_event(UserEvent::Ctl);
        });
        let started_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.ctl = crate::ctl_server::CtlServer::start(
            self.ctl_policy,
            std::process::id(),
            env!("CARGO_PKG_VERSION"),
            started_unix,
            wake,
        );
    }

    /// Scan every pane's recent output for configured
    /// trigger patterns. On the first match in this tick, raise the
    /// OS window's attention indicator (taskbar flash / dock bounce
    /// / WM_HINTS urgency) — but only if the window isn't focused,
    /// AND throttle to one fire per 2 seconds so a build script
    /// printing 100 error lines doesn't pulse the taskbar 100×.
    /// Agent-first A2: drain the control-server channel and
    /// dispatch each message on the main thread (the only place `ws.mux` is
    /// touched). Mirrors `drain_remote_commands` but with per-connection
    /// replies + a connection table.
    pub(super) fn drain_ctl(&mut self, ws: &mut WindowState, event_loop: &ActiveEventLoop) {
        use crate::ctl_server::CtlServerMsg;
        let mut budget = crate::ctl_server::DrainBudget::new();
        // Only repaint when the batch could have changed visible state.
        // `wait_for` probes are pure reads arriving at up to 20/s for the
        // whole wait, and repainting an idle window for each one wastes CPU.
        let mut needs_redraw = false;
        while budget.take() {
            let Some(msg) = self.ctl.as_ref().and_then(|ctl| ctl.try_recv()) else {
                break;
            };
            match msg {
                CtlServerMsg::NewConn { conn_id, event_tx } => {
                    if let Some(ctl) = &mut self.ctl {
                        ctl.add_conn(conn_id, event_tx);
                    }
                    log::info!("agent-server: connection {conn_id} opened");
                }
                CtlServerMsg::Request {
                    conn_id,
                    request,
                    reply,
                } => {
                    if !self.ctl.as_ref().is_some_and(|ctl| ctl.connected(conn_id)) {
                        continue;
                    }
                    // Mutations + first-time attaches change visible state
                    // (pane content, agent badge); wait_for's internal
                    // probes and `ui_geometry` reads never do.
                    if !request.internal_probe() && !ctl_method_is_pure_read(request.method()) {
                        needs_redraw = true;
                    }
                    // Nothing a control client drives stands for the user's
                    // own gesture (see `pull_preview`).
                    self.ctl_driving = true;
                    self.handle_ctl_request(ws, event_loop, conn_id, *request, reply);
                    self.ctl_driving = false;
                }
                CtlServerMsg::Disconnect { conn_id } => {
                    let panes = self
                        .ctl
                        .as_mut()
                        .map(|c| c.remove_conn(conn_id))
                        .unwrap_or_default();
                    // Only a connection that held panes can clear a badge.
                    needs_redraw |= !panes.is_empty();
                    // Clear the agent badge for panes no connection holds now.
                    for pane in panes {
                        let still = self
                            .ctl
                            .as_ref()
                            .map(|c| c.pane_is_attached(pane))
                            .unwrap_or(false);
                        if !still {
                            self.set_pane_agent_attached(ws, pane, false);
                        }
                    }
                    // Drop any pending run owned by this connection.
                    self.pending_runs
                        .retain(|_, p: &mut PendingRun| p.conn_id != conn_id);
                    // Its `show` pushes are no longer wanted.
                    self.media.disconnected(conn_id);
                    log::info!("agent-server: connection {conn_id} closed");
                }
            }
        }
        budget.finish(
            self.ctl.as_ref().is_some_and(|ctl| ctl.has_pending()),
            || {
                let _ = self.proxy.send_event(UserEvent::Ctl);
            },
        );
        if needs_redraw && let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// Dispatch one control request against the App and reply over `reply`.
    /// Most methods reply immediately; `run_command` stores `reply` and resolves
    /// it later (OSC-133 completion or the deadline).
    ///
    /// The request was admitted on its connection thread: the policy allowed
    /// its method and its params are an object or absent.
    pub(super) fn handle_ctl_request(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        conn_id: u64,
        mut request: crate::ctl_server::AdmittedRequest,
        reply: crate::ctl_server::ReplyTx,
    ) {
        use kettle_ctl::protocol::{Method, Response, error_codes as ec};
        let req = request.request();
        let method = request.method();
        // Annotate the session trace with each agent action when recording
        // (cheap; only when a recorder is active). Skip wait_for's internal
        // read_screen probes, since a 300s wait at 50ms polls would add ~6000
        // markers. The client's own calls already show the wait.
        if !request.internal_probe()
            && let Some(rec) = self.recorder.as_mut()
        {
            rec.record_marker(&format!("kettle:agent {} conn={conn_id}", method.as_str()));
        }
        let resp = match method {
            Method::AgentDisplayStatus => {
                if req.params.is_null()
                    || req
                        .params
                        .as_object()
                        .is_some_and(|params| params.is_empty())
                {
                    Response::ok(
                        req.id,
                        serde_json::json!({"agy": self.cfg.agent_display_agy}),
                    )
                } else {
                    Response::err(
                        req.id,
                        ec::BAD_PARAMS,
                        "agent_display_status takes no parameters",
                    )
                }
            }
            Method::GetState => {
                let policy = self.ctl.as_ref().map(|c| c.policy()).unwrap_or_default();
                Response::ok(req.id, self.ctl_get_state(ws, policy, request.caller()))
            }
            Method::ListTabs => self.ctl_list_tabs(ws, req),
            Method::ListPanes => self.ctl_list_panes(ws, req),
            Method::ReadScreen => self.ctl_read_screen(ws, req, request.internal_probe()),
            Method::ReadCells => self.ctl_read_cells(ws, req),
            Method::UiGeometry => self.ctl_ui_geometry(ws, req),
            Method::Screenshot => {
                self.ctl_screenshot(ws, req, reply);
                return;
            }
            Method::Subscribe => {
                if let Some(c) = &mut self.ctl {
                    c.set_subscribed(conn_id);
                }
                Response::ok(req.id, serde_json::json!({"subscribed": true}))
            }
            Method::SendText => self.ctl_send_text(ws, conn_id, req),
            Method::SendKeys => self.ctl_send_keys(ws, conn_id, req),
            Method::DispatchUiKey => self.ctl_dispatch_ui_key(ws, event_loop, req),
            Method::DispatchKeybind => self.ctl_dispatch_keybind(ws, event_loop, req),
            Method::SendMouse => self.ctl_send_mouse(ws, event_loop, req),
            Method::ResizeWindow => self.ctl_resize_window(ws, req),
            Method::PerformAction => self.ctl_perform_action(ws, event_loop, req),
            Method::RunCommand => {
                // Deferred: store `reply`; resolved on completion/deadline.
                self.ctl_run_command(ws, conn_id, req, reply);
                return;
            }
            Method::WaitFor => {
                Response::err(req.id, ec::INTERNAL, "wait_for was routed to the UI thread")
            }
            Method::AgentLaunch | Method::CardPickup => self.ctl_agent_request(ws, &mut request),
            Method::Show => {
                // Deferred: answered once the item is on its shelf.
                self.ctl_show(ws, conn_id, &mut request, reply);
                return;
            }
        };
        let _ = reply.send(resp);
    }

    pub(super) fn ctl_agent_request(
        &mut self,
        ws: &WindowState,
        request: &mut crate::ctl_server::AdmittedRequest,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let id = request.request().id;
        let refused =
            || Response::err(id, ec::BAD_PARAMS, "launch-scoped card request was refused");
        #[cfg(not(target_os = "linux"))]
        {
            let _ = ws;
            refused()
        }
        #[cfg(target_os = "linux")]
        {
            use crate::ctl_server::AgentAdmission;
            let Some(admission) = request.take_agent() else {
                return refused();
            };
            match admission {
                AgentAdmission::Launch { root, files } => {
                    let Some(chain) = request.caller().chain().and_then(Result::ok) else {
                        return refused();
                    };
                    if chain.first() != Some(&root) {
                        return refused();
                    }
                    let (roots, _) = self.caller_roots(ws);
                    let Some((pane, _)) = crate::media::nearest_pane(chain, &roots) else {
                        return refused();
                    };
                    let Some(pane_root) = roots
                        .iter()
                        .find(|entry| entry.pane == pane)
                        .map(|entry| entry.root)
                    else {
                        return refused();
                    };
                    if !self.agent_launches.register(
                        root,
                        pane,
                        pane_root,
                        files,
                        std::time::Instant::now(),
                    ) {
                        return refused();
                    }
                    Response::ok(id, serde_json::json!({"enabled": true}))
                }
                AgentAdmission::Pickup {
                    sender,
                    request: pickup,
                } => {
                    let live = self
                        .scoped_card_caller(ws, request.caller(), &sender, pickup.target)
                        .is_some_and(|caller| {
                            self.agent_launches.redeem(
                                pickup.permit,
                                caller,
                                std::time::Instant::now(),
                            )
                        });
                    Response::ok(id, serde_json::json!({"live": live}))
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn scoped_card_caller<'a>(
        &self,
        ws: &WindowState,
        evidence: &'a crate::ctl_server::CallerEvidence,
        sender: &crate::ctl_server::SenderIdentity,
        target: kettle_ctl::show::InlineTarget,
    ) -> Option<kettle_ctl::agent_launch::CardCaller<'a>> {
        let chain = evidence.chain()?.ok()?;
        let owner = sender.asker.process;
        if owner == sender.process
            || chain.first() != Some(&sender.process)
            || chain.get(1) != Some(&owner)
        {
            return None;
        }
        let (roots, _) = self.caller_roots(ws);
        let (pane, _) = crate::media::nearest_pane(chain, &roots)?;
        let pane_root = roots.iter().find(|entry| entry.pane == pane)?.root;
        Some(kettle_ctl::agent_launch::CardCaller {
            target,
            owner,
            chain,
            pane,
            pane_root,
        })
    }

    /// Every pane as `(pane, window)`, and the live ones' own children that
    /// a caller can descend from. A closed or held pane's child no longer
    /// hosts anyone who could be calling.
    pub(super) fn caller_roots(
        &self,
        ws: &WindowState,
    ) -> (Vec<crate::media::PaneRoot>, Vec<(u64, u64)>) {
        let mut roots = Vec::new();
        let mut panes = Vec::new();
        for window in self.all_windows(ws) {
            for (&id, pane) in &window.mux.panes {
                panes.push((id, window.seq));
                if let Some(root) = pane.caller_root
                    && !pane.closed
                    && !pane.held
                {
                    roots.push(crate::media::PaneRoot {
                        root,
                        pane: id,
                        window: window.seq,
                    });
                }
            }
        }
        (roots, panes)
    }

    /// `get_state`: version, theme, pid, server mode, focused pane.
    pub(super) fn ctl_get_state(
        &self,
        ws: &WindowState,
        policy: kettle_ctl::CtlPolicy,
        caller: &crate::ctl_server::CallerEvidence,
    ) -> serde_json::Value {
        let (roots, panes) = self.caller_roots(ws);
        serde_json::json!({
            "caller": ctl_caller_report(caller, &roots, &panes, std::process::id()),
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            // `mode` keeps its original spelling (`readonly`); `policy` uses
            // the config spellings and the display grant itself.
            "mode": format!("{:?}", policy.server()).to_lowercase(),
            "policy": {
                "server": policy.server().config_token(),
                "display": policy.display(),
            },
            "theme": self.cfg.theme_name,
            // `tabs` / `focused_pane` describe the FOCUSED window (back-
            // compat); `windows` / `focused_window` add the window dimension.
            "tabs": ws.mux.tabs.len(),
            "focused_pane": ws.mux.tabs.get(ws.mux.active).map(|t| t.focus),
            "windows": 1 + self.windows.len(),
            "focused_window": self.focused_seq,
            "window_title": self.desired_window_title(ws),
            "media": ctl_media_state(self.startup.media.as_deref()),
        })
    }

    /// `list_tabs`: index, title, active flag, pane ids.
    pub(super) fn ctl_list_tabs(
        &self,
        ws: &WindowState,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{PageRequest, Response, error_codes as ec};

        let page = match PageRequest::from_params(&req.params) {
            Ok(page) => page,
            Err(message) => return Response::err(req.id, ec::BAD_PARAMS, message),
        };
        let Some(capacity) = self.ctl_window_inventory_size(ws) else {
            return Response::err(
                req.id,
                ec::RESPONSE_TOO_LARGE,
                "window/tab/pane inventory exceeds the control enumeration budget",
            );
        };
        // Tabs across EVERY window, ordered by window seq. `index` and
        // `active` are in-window values; `window` disambiguates.
        let mut tabs = Vec::with_capacity(capacity.min(MAX_CTL_COLLECTION_ITEMS));
        for w in self.all_windows(ws) {
            let titles = w.mux.tab_titles();
            for (i, t) in w.mux.tabs.iter().enumerate() {
                tabs.push(serde_json::json!({
                    "window": w.seq,
                    "index": i,
                    "title": titles.get(i).cloned().unwrap_or_default(),
                    "active": i == w.mux.active,
                    "focused_pane": t.focus,
                    "panes": t.root.leaf_ids(),
                }));
            }
        }
        ctl_page_values_with_page(req, "tabs", tabs, page)
    }

    /// `list_panes`: id, tab, title, cwd, size, focused, argv, child_pid,
    /// agent_attached.
    pub(super) fn ctl_list_panes(
        &self,
        ws: &WindowState,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{PageRequest, Response, error_codes as ec};

        let page = match PageRequest::from_params(&req.params) {
            Ok(page) => page,
            Err(message) => return Response::err(req.id, ec::BAD_PARAMS, message),
        };
        let Some(capacity) = self.ctl_window_inventory_size(ws) else {
            return Response::err(
                req.id,
                ec::RESPONSE_TOO_LARGE,
                "window/tab/pane inventory exceeds the control enumeration budget",
            );
        };
        // Panes across EVERY window; `tab` is the in-window tab index,
        // `window` the owning window's seq, `focused` means focused WITHIN
        // its window.
        let mut panes = Vec::with_capacity(capacity.min(MAX_CTL_COLLECTION_ITEMS));
        for w in self.all_windows(ws) {
            self.ctl_list_panes_of(w, &mut panes);
        }
        ctl_page_values_with_page(req, "panes", panes, page)
    }

    pub(super) fn ctl_list_panes_of(&self, ws: &WindowState, panes: &mut Vec<serde_json::Value>) {
        let focused = ws.mux.tabs.get(ws.mux.active).map(|t| t.focus);
        for (ti, tab) in ws.mux.tabs.iter().enumerate() {
            for id in tab.root.leaf_ids() {
                let Some(pane) = ws.mux.panes.get(&id) else {
                    continue;
                };
                let (cols, rows) = pane
                    .term
                    .term
                    .lock()
                    .ok()
                    .map(|t| {
                        use kettle_core::Dimensions;
                        (t.columns(), t.screen_lines())
                    })
                    .unwrap_or((0, 0));
                let attached = self
                    .ctl
                    .as_ref()
                    .map(|c| c.pane_is_attached(id))
                    .unwrap_or(false);
                panes.push(serde_json::json!({
                    "id": id,
                    "window": ws.seq,
                    "tab": ti,
                    "title": pane.title,
                    "cwd": pane.term.current_dir_or_native(),
                    "cols": cols,
                    "rows": rows,
                    "focused": Some(id) == focused,
                    "argv": pane.argv,
                    "child_pid": pane.term.child_pid(),
                    "agent_attached": attached,
                    // Surfaced so an agent can SEE the user's
                    // read-only lock before a send_text/run_command bounces
                    // with the `read_only` error code.
                    "read_only": pane.read_only,
                    // What `show` put on this pane's shelf, newest first.
                    "media_shelf": crate::media::shelf_report(&pane.media_shelf),
                }));
            }
        }
    }

    /// `read_screen`: a plain-text snapshot of a pane (default: focused).
    /// `read_screen`. A `wait_for` probe (`internal_probe`) gets the whole
    /// snapshot in one answer, so it matches text past the first page; the
    /// snapshot is already bounded by `MAX_CTL_SCREEN_SNAPSHOT_BYTES`, and the
    /// answer never leaves this process.
    pub(super) fn ctl_read_screen(
        &mut self,
        ws: &mut WindowState,
        req: &kettle_ctl::protocol::Request,
        internal_probe: bool,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{PageRequest, Response, error_codes as ec};
        let page = match PageRequest::from_params(&req.params) {
            Ok(page) => page,
            Err(message) => return Response::err(req.id, ec::BAD_PARAMS, message),
        };
        let pane = match self.ctl_resolve_pane(ws, &req.params) {
            Ok(p) => p,
            Err((code, message)) => return Response::err(req.id, code, message),
        };
        let scrollback = match req.params.get("scrollback_lines") {
            None | Some(serde_json::Value::Null) => 0,
            Some(value) => match value.as_u64().and_then(|n| usize::try_from(n).ok()) {
                Some(lines) => lines.min(MAX_CTL_SCROLLBACK_LINES),
                None => {
                    return Response::err(
                        req.id,
                        ec::BAD_PARAMS,
                        "'scrollback_lines' must be an unsigned integer",
                    );
                }
            },
        };
        let include_selection = match req.params.get("include_selection") {
            None | Some(serde_json::Value::Null) => false,
            Some(value) => match value.as_bool() {
                Some(include) => include,
                None => {
                    return Response::err(
                        req.id,
                        ec::BAD_PARAMS,
                        "'include_selection' must be a boolean",
                    );
                }
            },
        };
        let Some(p) = Self::ctl_pane_ref(ws, &self.windows, pane) else {
            return Response::err(req.id, ec::NO_SUCH_PANE, "pane vanished");
        };
        let Ok(term) = p.term.term.lock() else {
            return Response::err(req.id, ec::INTERNAL, "could not read the grid");
        };
        use kettle_core::Dimensions as _;
        let range = term
            .selection
            .as_ref()
            .and_then(|selection| selection.to_range(&term));

        // `selection_to_string` owns an unbounded String API. Prove a
        // conservative byte upper bound from the selected grid cells first;
        // if it does not fit, omit the content and report truncation without
        // ever materializing the oversized selection.
        let selection_fits = !include_selection
            || range.as_ref().is_none_or(|range| {
                let columns = term.columns();
                let mut bytes = 0usize;
                for line in range.start.line.0..=range.end.line.0 {
                    // Newline plus wide-cell boundary behavior. Eight bytes is
                    // a conservative allowance for both.
                    bytes = bytes.saturating_add(8);
                    if bytes > MAX_CTL_SELECTION_BYTES || columns == 0 {
                        return false;
                    }
                    let start_col = (if range.is_block || line == range.start.line.0 {
                        range.start.column.0
                    } else {
                        0
                    })
                    // Alacritty includes the leading wide character when a
                    // selection starts on its spacer. Count that whole cell,
                    // including any combining marks, before allocation.
                    .saturating_sub(1)
                    .min(columns - 1);
                    let end_col = if range.is_block || line == range.end.line.0 {
                        range.end.column.0
                    } else {
                        columns - 1
                    }
                    .min(columns - 1);
                    if start_col > end_col {
                        continue;
                    }
                    for column in start_col..=end_col {
                        let cell = &term.grid()[kettle_core::Point::new(
                            kettle_core::Line(line),
                            kettle_core::Column(column),
                        )];
                        bytes = bytes.saturating_add(cell.c.len_utf8());
                        for mark in cell.zerowidth().into_iter().flatten() {
                            bytes = bytes.saturating_add(mark.len_utf8());
                        }
                        if bytes > MAX_CTL_SELECTION_BYTES {
                            return false;
                        }
                    }
                }
                true
            });
        let raw_selection = (include_selection && selection_fits)
            .then(|| term.selection_to_string())
            .flatten();
        let omitted_selection = include_selection && !selection_fits && range.is_some();
        let selection_present = term.selection.is_some();
        let selection_range = range.as_ref().map(|range| {
            serde_json::json!({
                "start": [range.start.line.0, range.start.column.0],
                "end": [range.end.line.0, range.end.column.0],
                "block": range.is_block,
            })
        });

        // Account for every cell before `screen_text` reserves and fills its
        // complete snapshot String. Each visited cell consumes at least one
        // byte, so the scan itself is bounded by the same byte budget.
        let columns = term.columns();
        let rows = term.screen_lines();
        let take = scrollback
            .min(term.grid().history_size())
            .min(MAX_CTL_SCROLLBACK_LINES);
        let display_adjust = if take == 0 {
            term.grid().display_offset() as i32
        } else {
            0
        };
        let mut screen_bytes = 0usize;
        let mut screen_fits = columns > 0;
        'screen: for row in -(take as i32)..rows as i32 {
            screen_bytes = screen_bytes.saturating_add(1);
            for column in 0..columns {
                let cell = &term.grid()[kettle_core::Point::new(
                    kettle_core::Line(row - display_adjust),
                    kettle_core::Column(column),
                )];
                screen_bytes = screen_bytes.saturating_add(cell.c.len_utf8());
                for mark in cell.zerowidth().into_iter().flatten() {
                    screen_bytes = screen_bytes.saturating_add(mark.len_utf8());
                }
                if screen_bytes > MAX_CTL_SCREEN_SNAPSHOT_BYTES {
                    screen_fits = false;
                    break 'screen;
                }
            }
        }
        if !screen_fits {
            return Response::err(
                req.id,
                ec::RESPONSE_TOO_LARGE,
                "screen snapshot exceeds the control byte budget",
            );
        }
        // Materialize under the same lock used for the byte preflight so PTY
        // output cannot race a small checked grid into an oversized snapshot.
        let s = kettle_core::term::screen_text_of(&term, scrollback);
        drop(term);
        let (selection, selection_truncated) = match raw_selection
            .map(kettle_core::scrub_card_markers)
            .transpose()
        {
            Ok(Some(text)) => {
                let (text, truncated) = cap_ctl_selection(text);
                (Some(text), truncated)
            }
            Ok(None) => (None, omitted_selection),
            Err(_) => {
                return Response::err(req.id, ec::INTERNAL, "could not prepare selection text");
            }
        };
        let snapshot = ctl_snapshot("screen", &s.text);
        if page
            .snapshot
            .as_deref()
            .is_some_and(|want| want != snapshot)
        {
            return Response::err(
                req.id,
                ec::STALE_SNAPSHOT,
                "screen changed between pages; restart without cursor/snapshot",
            );
        }
        let ScreenPage {
            text,
            end,
            total,
            text_truncated,
        } = screen_page(&s.text, &page, internal_probe);
        let truncated = end < total;
        Response::ok(
            req.id,
            serde_json::json!({
                "pane": pane,
                "text": text,
                "cols": s.cols,
                "rows": s.rows,
                "history_size": s.history_size,
                "display_offset": s.display_offset,
                "cursor": [s.cursor.0, s.cursor.1],
                "cursor_visible": s.cursor_visible,
                "selection_present": selection_present,
                "selection": selection,
                "selection_truncated": selection_truncated,
                "selection_range": selection_range,
                "snapshot": snapshot,
                "next_cursor": truncated.then(|| end.to_string()),
                "truncated": truncated,
                "text_truncated": text_truncated,
                "total": total,
            }),
        )
    }

    /// `read_cells`: visible cell snapshot with selected render attributes.
    /// Intended for deterministic renderer/UI diagnostics without OCR.
    pub(super) fn ctl_read_cells(
        &mut self,
        ws: &mut WindowState,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{PageRequest, Response, error_codes as ec};
        let page = match PageRequest::from_params(&req.params) {
            Ok(page) => page,
            Err(message) => return Response::err(req.id, ec::BAD_PARAMS, message),
        };
        let pane = match self.ctl_resolve_pane(ws, &req.params) {
            Ok(p) => p,
            Err((code, message)) => return Response::err(req.id, code, message),
        };
        let Some(p) = Self::ctl_pane_ref(ws, &self.windows, pane) else {
            return Response::err(req.id, ec::NO_SUCH_PANE, "pane vanished");
        };
        let Ok(t) = p.term.term.lock() else {
            return Response::err(req.id, ec::INTERNAL, "could not read the grid");
        };
        use kettle_core::Dimensions as _;
        let Some(cell_count) = t.columns().checked_mul(t.screen_lines()) else {
            return Response::err(
                req.id,
                ec::RESPONSE_TOO_LARGE,
                "visible grid dimensions overflow the control snapshot budget",
            );
        };
        if cell_count > MAX_CTL_COLLECTION_ITEMS {
            return Response::err(
                req.id,
                ec::RESPONSE_TOO_LARGE,
                "visible grid exceeds the control snapshot budget",
            );
        }
        let mut snap = PaneSnapshot::default();
        snap.capture(&t);
        drop(t);

        let mut cells = Vec::with_capacity(snap.cells.len());
        let display_off = snap.display_offset as i32;
        for sc in &snap.cells {
            let row = sc.line + display_off;
            if row < 0 || row >= snap.screen_lines as i32 {
                continue;
            }
            let flags = sc.flags.bits();
            cells.push(serde_json::json!({
                "row": row,
                "col": sc.col,
                "ch": if sc.is_card_placeholder() { ' ' } else { sc.c }.to_string(),
                "underline": flags & CELL_FLAG_UNDERLINE != 0,
                "double_underline": flags & CELL_FLAG_DOUBLE_UNDERLINE != 0,
                "undercurl": flags & CELL_FLAG_UNDERCURL != 0,
                "dotted_underline": flags & CELL_FLAG_DOTTED_UNDERLINE != 0,
                "dashed_underline": flags & CELL_FLAG_DASHED_UNDERLINE != 0,
                "any_underline": flags & CELL_FLAG_ALL_UNDERLINES != 0,
                "strikeout": flags & CELL_FLAG_STRIKEOUT != 0,
                "underline_color": sc.underline_color.is_some(),
            }));
        }
        let mut response = ctl_page_values_with_page(req, "cells", cells, page);
        if let Some(result) = response.result.as_object_mut() {
            result.insert("pane".into(), pane.into());
            result.insert("cols".into(), snap.columns.into());
            result.insert("rows".into(), snap.screen_lines.into());
            result.insert("history_size".into(), snap.history_size.into());
            result.insert("display_offset".into(), snap.display_offset.into());
            result.insert(
                "cursor".into(),
                serde_json::json!([snap.cursor.point.line.0, snap.cursor.point.column.0]),
            );
        }
        response
    }

    /// `ui_geometry`: read-only live geometry for chrome diagnostics.
    pub(super) fn ctl_ui_geometry(
        &self,
        ws: &WindowState,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{Response, error_codes as ec};

        let want_window = match optional_u64_param(&req.params, "window") {
            Ok(window) => window,
            Err(message) => return Response::err(req.id, ec::BAD_PARAMS, message),
        };
        let target = match want_window {
            None => ws,
            Some(seq) if seq == ws.seq => ws,
            Some(seq) => match self.windows.get(&seq) {
                Some(target) => target,
                None => {
                    return Response::err(
                        req.id,
                        ec::BAD_PARAMS,
                        format!("no window with id {seq}"),
                    );
                }
            },
        };
        let surface = target
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        let cell = target
            .renderer
            .as_ref()
            .map(|r| {
                serde_json::json!({
                    "width": r.cell_w,
                    "height": r.cell_h,
                    "font_size": r.font_size(),
                })
            })
            .unwrap_or_else(|| {
                serde_json::json!({
                    "width": 8.0,
                    "height": 16.0,
                    "font_size": 13.0,
                })
            });
        let cell_w = target.renderer.as_ref().map(|r| r.cell_w).unwrap_or(8.0);
        let cell_h = target.renderer.as_ref().map(|r| r.cell_h).unwrap_or(16.0);
        let bar = self.tab_bar(target);
        let segments: Vec<serde_json::Value> = bar
            .segments
            .iter()
            .map(|seg| {
                let fitted_title = kettle_render::fit_tab_segment_title(
                    &seg.title,
                    seg.path.as_deref(),
                    seg.idx,
                    &self.cfg.tab_format,
                    seg.title_rect.2,
                    cell_w,
                );
                serde_json::json!({
                    "index": seg.idx,
                    "title": seg.title,
                    "path": seg.path.as_deref(),
                    "fitted_title": fitted_title,
                    "active": seg.active,
                    "rect": rect_json(seg.rect),
                    "title_rect": rect_json(seg.title_rect),
                    "close": rect_json(seg.close),
                })
            })
            .collect();
        let pane_layout = self.pane_layout(target, self.area(target));
        let active_search_match_rects: Vec<serde_json::Value> = target
            .search
            .target_pane
            .filter(|_| target.search.open)
            .and_then(|pane_id| {
                pane_layout
                    .iter()
                    .find(|(candidate, _)| *candidate == pane_id)
                    .map(|(_, partition)| (pane_id, partition.terminal))
            })
            .map(|(pane_id, terminal)| {
                let origin = (
                    terminal.0 + self.cfg.padding_x,
                    terminal.1 + self.cfg.padding_y,
                );
                self.overlay(target, false)
                    .highlights
                    .into_iter()
                    .filter(|highlight| highlight.active)
                    .map(|highlight| {
                        serde_json::json!({
                            "pane": pane_id,
                            "rect": rect_json((
                                origin.0 + highlight.col as f32 * cell_w,
                                origin.1 + highlight.row as f32 * cell_h,
                                highlight.width as f32 * cell_w,
                                cell_h,
                            )),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Each pane's preview lane, by pane: where it is and how it shows.
        // Geometry only; never a path, title or pixel.
        let preview_lanes: Vec<serde_json::Value> = pane_layout
            .iter()
            .filter_map(|(id, partition)| {
                let share = partition.lane?;
                let state = match share {
                    crate::pane_partition::LaneShare::Expanded(_) => "expanded",
                    crate::pane_partition::LaneShare::Strip(_) => "strip",
                    crate::pane_partition::LaneShare::Badge => "badge",
                };
                let collapsed = matches!(share, crate::pane_partition::LaneShare::Strip(_));
                Some(self.preview_diagnostics(target, *id, share.rect(), collapsed, state))
            })
            .collect();
        let preview_overlay = target.preview_overlay.and_then(|open| {
            let partition = pane_layout.iter().find(|(pane, _)| *pane == open.pane)?.1;
            let (rect, collapsed) = self.preview_placement(target, open.pane, partition)?;
            let mut report =
                self.preview_diagnostics(target, open.pane, Some(rect), collapsed, "overlay");
            report["focused_control"] = open.control.into();
            report["modal"] = true.into();
            Some(report)
        });
        let home = crate::mux::home_dir_string();
        let focus = target.mux.active_focus();
        let pane_titlebars: Vec<serde_json::Value> = if pane_layout
            .iter()
            .any(|(_, partition)| partition.titlebar.is_some())
        {
            pane_layout
                .iter()
                .filter_map(|(pane_id, partition)| {
                    let titlebar_rect = partition.titlebar?;
                    let rect = &partition.leaf;
                    let pane = target.mux.panes.get(pane_id)?;
                    let cwd = pane.term.current_dir_or_native();
                    let mut title_parts = pane_title_parts(
                        &self.cfg.agent_badge,
                        pane.agent_attached,
                        pane.read_only,
                        &pane.title,
                        pane.title_is_placeholder,
                        cwd.as_deref(),
                        home.as_deref(),
                    );
                    title_parts
                        .prefix
                        .push_str(&media_shelf_badge(pane.media_shelf.unseen()));
                    let (cols, rows) = self.grid_of(target, partition.terminal);
                    let size_text =
                        (!self.cfg.title_hide_sizetext).then(|| format!("{}x{}", cols, rows));
                    let bell: Option<&str> = None;
                    let budget = (rect.2 / cell_w.max(1.0)).floor().max(1.0) as usize;
                    let group = pane.group_name.as_deref().filter(|g| !g.is_empty());
                    let fitted_title = kettle_render::fit_pane_titlebar_title(
                        group,
                        &title_parts.prefix,
                        &title_parts.title,
                        title_parts.path.as_deref(),
                        size_text.as_deref(),
                        bell,
                        budget,
                    );
                    Some(serde_json::json!({
                        "pane": pane_id,
                        "focused": Some(*pane_id) == focus,
                        "rect": rect_json(titlebar_rect),
                        "pane_rect": rect_json(*rect),
                        "raw_title": pane.title,
                        "title": title_parts.title,
                        "title_prefix": title_parts.prefix,
                        "path": title_parts.path,
                        "fitted_title": fitted_title,
                        "cols": cols,
                        "rows": rows,
                    }))
                })
                .collect()
        } else {
            Vec::new()
        };
        let context_menu = target.context_menu.as_ref().and_then(|menu| {
            let ((ax, ay), (panel_w, panel_h)) = self.context_menu_geometry(target)?;
            let (_, ch) = self.menu_cell(target);
            let row_h = ch + kettle_render::menu::ROW_PAD;
            let sep_h = kettle_render::menu::SEP_H;
            let (inset, content_h) =
                context_menu_content_geometry(&menu.items, panel_h, row_h, sep_h);
            let mut row_y = ay + inset;
            let start = menu.scroll_offset.min(menu.items.len());
            let visible = count_rows_fitting(&menu.items, start, content_h, row_h, sep_h);
            let rows: Vec<serde_json::Value> = menu
                .items
                .iter()
                .enumerate()
                .skip(start)
                .take(visible)
                .map(|(idx, item)| {
                    let separator = matches!(item, ContextMenuItem::Separator);
                    let h = if separator { sep_h } else { row_h };
                    let label = match item {
                        ContextMenuItem::Item { label, .. } => *label,
                        ContextMenuItem::DynamicItem { label, .. } => label.as_str(),
                        ContextMenuItem::LuaItem { label, .. } => label.as_str(),
                        ContextMenuItem::ConfigItem { label, .. }
                        | ContextMenuItem::ReconnectItem { label, .. } => label.as_str(),
                        ContextMenuItem::Submenu { label, .. } => label.as_str(),
                        ContextMenuItem::ProfileChoice { label, .. }
                        | ContextMenuItem::WindowChoice { label, .. } => label.as_str(),
                        ContextMenuItem::NewTabShell { label, .. } => label.as_str(),
                        ContextMenuItem::UrlItem { label, .. } => *label,
                        ContextMenuItem::Media { label, .. }
                        | ContextMenuItem::Preview { label, .. } => *label,
                        ContextMenuItem::Info { label } => label.as_str(),
                        ContextMenuItem::Separator => "",
                    };
                    let row = serde_json::json!({
                        "index": idx,
                        "label": label,
                        "separator": separator,
                        "dispatchable": item_is_dispatchable(item),
                        "rect": rect_json((ax, row_y, panel_w, h)),
                    });
                    row_y += h;
                    row
                })
                .collect();
            Some(serde_json::json!({
                "anchor": {"x": ax, "y": ay},
                "rect": rect_json((ax, ay, panel_w, panel_h)),
                "highlight": menu.highlight,
                "scroll_offset": menu.scroll_offset,
                "rows": rows,
            }))
        });
        let title_edit = target.editing_title.as_ref().map(|state| {
            let scope = match state.scope {
                TitleEditScope::Window => "window",
                TitleEditScope::Tab => "tab",
                TitleEditScope::Pane => "pane",
                TitleEditScope::Group => "group",
            };
            serde_json::json!({
                "scope": scope,
                "rect": rect_json(self.title_edit_rect(target)),
            })
        });
        let search = target.search.open.then(|| {
            // The geometry is what is on screen, in the UI's language. The
            // labels below are control-protocol fields and stay English in
            // every language, like the rest of this JSON.
            let geometry = kettle_render::search_bar_geometry(
                surface.0 as f32,
                surface.1 as f32,
                target
                    .renderer
                    .as_ref()
                    .map_or(8.0, |renderer| renderer.overlay_text_cell_width()),
                cell_h,
                &self.ui_text,
            );
            let english = kettle_i18n::Translator::new(kettle_i18n::Language::En);
            let controls = kettle_render::SearchControl::ALL.map(|control| {
                serde_json::json!({
                    "name": match control {
                        kettle_render::SearchControl::Editor => "editor",
                        kettle_render::SearchControl::Previous => "previous",
                        kettle_render::SearchControl::Next => "next",
                        kettle_render::SearchControl::Wrap => "wrap",
                        kettle_render::SearchControl::Case => "case",
                        kettle_render::SearchControl::Invert => "invert",
                        kettle_render::SearchControl::Close => "close",
                    },
                    "label": control.accessible_label(&english),
                    "rect": rect_json(geometry.control_rect(control)),
                    "focused": control == target.search.focused_control,
                })
            });
            serde_json::json!({
                // The query is deliberately omitted: geometry diagnostics must
                // not expose terminal-derived or user-entered search text.
                "target_pane": target.search.target_pane,
                "rect": rect_json(geometry.rect),
                "reserved_height": geometry.reserved_height,
                "rows": geometry.rows,
                "status": effective_search_status(&target.search).label(&english),
                "has_match": target.search.focused.is_some(),
                "match_rects": active_search_match_rects,
                "visible_truncated": target.search.visible_truncated,
                "wrap": target.search.wrap,
                "case": map_search_case_mode(target.search.case_mode).label(&english),
                "invert": target.search.invert,
                "status_rect": rect_json(geometry.status),
                "controls": controls,
            })
        });
        // Geometry diagnostics expose only the receipt's presentation state.
        // The retained path and thumbnail pixels stay private to the app; UI
        // automation needs the bounds to verify hover/collapse behavior, not
        // the clipboard content itself.
        let media_paste_receipt = self
            .media_paste_receipt_geometry(target)
            .and_then(|geometry| {
                let receipt = target.media_paste_receipt.as_ref()?;
                let (kind, count, original_width, original_height) = match &receipt.kind {
                    crate::window_state::MediaPasteReceiptKind::Image {
                        original_width,
                        original_height,
                    } => ("image", 1, Some(*original_width), Some(*original_height)),
                    crate::window_state::MediaPasteReceiptKind::Video { candidate, .. } => {
                        ("video", candidate.count, None, None)
                    }
                };
                Some(serde_json::json!({
                    "rect": rect_json(geometry.rect),
                    "preview_rect": geometry.preview_rect.map(rect_json),
                    "image_rect": (kind == "image").then(|| geometry.preview_rect.map(rect_json)).flatten(),
                    "dismiss_rect": rect_json(geometry.dismiss_rect),
                    "expanded": !geometry.compact,
                    "kind": kind,
                    "openable": geometry.openable,
                    "count": count,
                    "preview_ready": receipt.image.is_some(),
                    "original_width": original_width,
                    "original_height": original_height,
                    "remote": receipt.remote,
                }))
            });
        // Additive compatibility for automation written against the original
        // bitmap-only receipt. Video receipts appear only in the generic key.
        let image_paste_receipt = media_paste_receipt
            .as_ref()
            .filter(|receipt| {
                receipt.get("kind").and_then(serde_json::Value::as_str) == Some("image")
            })
            .cloned();
        let diagnostic_overlay = self.overlay(target, false);
        let settings = diagnostic_overlay.settings.as_ref().map(|settings| {
            let geometry = kettle_render::settings_panel_geometry(
                settings,
                cell_w,
                cell_h,
                surface.0 as f32,
                surface.1 as f32,
            );
            serde_json::json!({
                "rect": rect_json(geometry.rect),
                "focused_rect": rect_json(geometry.focused_rect),
                "focused_row": settings.focused_row,
                "row_count": settings.rows.len(),
                "active_category": settings.active_category,
                "first_line": geometry.first_line,
                "visible_lines": geometry.visible_lines,
            })
        });
        let confirmation = diagnostic_overlay.confirm_dialog.as_ref().map(|dialog| {
            let geometry = kettle_render::ConfirmDialogLayout::new(
                (surface.0 as f32, surface.1 as f32),
                (self.overlay_text_cell_width(target), self.overlay_text_line_height(target)),
                target.window.as_ref().map_or(1.0, |window| window.scale_factor() as f32), dialog,
            );
            serde_json::json!({
                "rect": rect_json(geometry.rect),
                "heading_rect": rect_json(geometry.heading_rect),
                "body_rect": rect_json(geometry.body_rect),
                "max_scroll": geometry.max_scroll,
                "scroll_offset": geometry.scroll_offset,
                "whole_scroll": geometry.whole_scroll,
                "buttons": geometry.buttons.iter().enumerate().map(|(index, rect)| serde_json::json!({"rect":rect_json(*rect), "label": dialog.buttons[index].label, "focused":index == dialog.focus_idx, "destructive":dialog.buttons[index].destructive, "hovered":dialog.hover_idx == Some(index), "pressed":dialog.pressed_idx == Some(index)})).collect::<Vec<_>>(),
            })
        });
        let modal_name = if diagnostic_overlay.confirm_dialog.is_some() {
            Some("confirm_dialog")
        } else if diagnostic_overlay.palette_query.is_some() {
            Some("palette")
        } else if diagnostic_overlay.theme_picker_query.is_some() {
            Some("theme_picker")
        } else if diagnostic_overlay.settings.is_some() {
            Some("settings")
        } else if diagnostic_overlay.layout_picker_query.is_some() {
            Some("layout_picker")
        } else if diagnostic_overlay.ssh_query.is_some() {
            Some("ssh_launcher")
        } else if diagnostic_overlay.edit_title.is_some() {
            Some("title_edit")
        } else if !diagnostic_overlay.hint_labels.is_empty() {
            Some("hint_mode")
        } else if diagnostic_overlay.context_menu.is_some() {
            Some("context_menu")
        } else {
            None
        };
        let modal_projection = modal_accessibility_projection(
            &diagnostic_overlay,
            (surface.0 as f32, surface.1 as f32),
            (
                self.overlay_text_cell_width(target),
                self.overlay_text_line_height(target),
            ),
            target
                .window
                .as_ref()
                .map_or(1.0, |window| window.scale_factor() as f32),
            &self.ui_text,
        );
        let accessibility = modal_name.map(|modal| {
            let focused = modal_projection.focus.and_then(|focus| {
                modal_projection
                    .nodes
                    .iter()
                    .find(|(id, _)| *id == focus)
                    .map(|(_, node)| {
                        serde_json::json!({
                            "role": format!("{:?}", node.role()).to_ascii_lowercase(),
                            "label": node.label(),
                        })
                    })
            });
            serde_json::json!({
                "modal": modal,
                "root_count": modal_projection.roots.len(),
                "focused": focused,
            })
        });
        // The cards on screen, by instance, never by card id, so automation
        // can find and click one as a user would.
        let now = std::time::Instant::now();
        let inline_cards: Vec<serde_json::Value> = target
            .renderer
            .as_ref()
            .map_or(&[][..], |renderer| renderer.painted_cards())
            .iter()
            .map(|card| {
                let item = self.media.cards.item(card.pane, card.nonce).and_then(|id| {
                    target
                        .mux
                        .panes
                        .get(&card.pane)?
                        .media_shelf
                        .items()
                        .iter()
                        .find(|item| item.id == id)
                });
                let [x, y, width, height] = card.rect;
                serde_json::json!({
                    "pane": card.pane,
                    "instance": target.card_sightings.instance(card),
                    "rect": {"x": x, "y": y, "width": width, "height": height},
                    "settled": target.card_sightings.settled(Some(*card), now).is_some(),
                    "item": item.map(|item| item.id),
                    "label": item.map(|item| {
                        without_card_marks(
                            self.ui_text
                                .card_a11y(&item.title, &self.card_accessibility_detail(item)),
                        )
                    }),
                })
            })
            .collect();
        let update_toast = self.update_toast_projection(target).map(|toast| serde_json::json!({
            "rect": rect_json(toast.geometry.rect), "label": toast.message,
            "message": rect_json(toast.geometry.message),
            "details": rect_json(toast.geometry.details), "dismiss": rect_json(toast.geometry.close),
        }));
        let update_notice = self.update_notice().map(|notice| {
            serde_json::json!({
                "state": match notice.kind {
                    crate::update_notice::NoticeKind::Available => "available",
                    crate::update_notice::NoticeKind::RestartPending => "restart_pending",
                    crate::update_notice::NoticeKind::Failed => "failed",
                },
                "tag": notice.tag,
                "facts": crate::update_notice::fact_rows(&self.update_facts, &self.ui_text),
            })
        });
        let mut geometry = serde_json::json!({
                "window": target.seq,
                "window_focused": target.window_focused,
                "scale_factor": target.window.as_ref().map_or(1.0, |w| w.scale_factor()),
                "monitor": target.window.as_ref().and_then(|w| w.current_monitor()).map(|m| {
                    let size = m.size().to_logical::<f64>(m.scale_factor());
                    serde_json::json!({"width": size.width, "height": size.height})
                }),
                "surface": {"width": surface.0, "height": surface.1},
                "cell": cell,
                // Which face serves codepoints Unicode renders as text, or
                // null when this system has none that carry them. A scenario
                // checking the bullet is monochrome needs to know whether the
                // host can produce that at all.
                "text_presentation_face": target
                    .renderer
                    .as_ref()
                    .and_then(|r| r.text_presentation_face()),
                "padding": {"x": self.cfg.padding_x, "y": self.cfg.padding_y},
                "content": rect_json(self.area(target)),
                // The active tab's pane rects, in the same surface coordinates
                // `send_mouse` takes. Without these, automation can address the
                // tab bar and the modals but has no way to point at a PANE, so
                // anything pane-geometric (the drag gesture, split ratios after
                // a resize) could only be checked by eye.
                // `rect` is the pane's whole leaf and `terminal` its grid with
                // the padding, clear of its titlebar and lane.
                "panes": pane_layout
                    .iter()
                    .map(|(id, partition)| serde_json::json!({
                        "id": id,
                        "rect": rect_json(partition.leaf),
                        "terminal": rect_json(partition.terminal),
                        "focused": target.mux.active_focus() == Some(*id),
                    }))
                    .collect::<Vec<_>>(),
                "modals": {
                    "search": target.search.open,
                    "palette": target.palette_input.is_some(),
                    "settings": target.settings_nav.is_some(),
                    "settings_text_edit": target.settings_text_edit.is_some(),
                    "layout_picker": target.layout_picker_input.is_some(),
                    "theme_picker": target.theme_picker_input.is_some(),
                    "hint_mode": target.hint_state.is_some(),
                    "ssh_launcher": target.ssh_input.is_some(),
                    "context_menu": target.context_menu.is_some(),
                    "title_edit": target.editing_title.is_some(),
                    "confirm_dialog": target.confirm_dialog.is_some(),
                    "vi_mode": target.vi_mode.is_some(),
                },
                "context_menu": context_menu,
                "settings": settings,
                "accessibility": accessibility,
                "search": search,
                "image_paste_receipt": image_paste_receipt,
                "media_paste_receipt": media_paste_receipt,
                "title_edit": title_edit,
                "resize_overlay": target.resize_overlay.map(|(cols, rows, _)| serde_json::json!({
                    "cols": cols,
                    "rows": rows,
                })),
                "cursor": [target.cursor.x, target.cursor.y],
                "tab_drag_active": target.tab_drag_active,
                "tab_drag_armed": target.tab_drag_press.is_some(),
                "tab_drag_visible": target.tab_drag_active && target.tab_drag_press.is_none(),
                // Terminator parity: the pane-drag gesture, reported in the
                // same armed/live shape as the tab drag above so a headless
                // check can tell "the press was noticed" from "the pane is
                // being carried" from "releasing here would move it".
                "pane_drag_armed": target.pane_drag.is_some(),
                "pane_drag_live": target.pane_drag.as_ref().is_some_and(|d| d.live),
                "pane_drag_pane": target.pane_drag.as_ref().map(|d| d.pane),
                "pane_drag_target": target.pane_drag.as_ref().and_then(|d| d.target).map(
                    |(id, _, dir, before)| serde_json::json!({
                        "pane": id,
                        "edge": match (dir, before) {
                            (Dir::Horizontal, true) => "left",
                            (Dir::Horizontal, false) => "right",
                            (Dir::Vertical, true) => "top",
                            (Dir::Vertical, false) => "bottom",
                        },
                    }),
                ),
                "tab_bar": {
                    "height": bar.height,
                    "y": bar.y,
                    "new_tab": rect_json(bar.new_tab),
                    "new_tab_menu": rect_json(bar.new_tab_menu),
                    "drag_cursor_x": bar.drag_cursor_x,
                    // A vertical strip reports the drag on its own axis;
                    // additive, so a reader that only knows the x key still
                    // sees exactly what it saw before on a horizontal bar.
                    "drag_cursor_y": bar.drag_cursor_y,
                    // Tear-off state (pre-tear ghost escalation and dock
                    // highlight) for live-UI smokes.
                    "tear_lift": bar.tear_lift,
                    "insert_marker": bar.insert_marker,
                    "dock_highlighted": bar.insert_marker.is_some(),
                    "band": rect_json(bar.band),
                    // The overflow arrows, back then forward (`‹ ›` across,
                    // `⌃ ⌄` down a side bar); zero-sized while it all fits.
                    "scroll_left": rect_json(bar.scroll_left),
                    "scroll_right": rect_json(bar.scroll_right),
                    "segments": segments,
                },
                "pane_titlebars": pane_titlebars,
                // What this window's renderer has sent to the GPU: counts
                // only, never content. A window that only blinks should add
                // frames here but no writes, and one printing on shared memory
                // mapped writes but no buffer writes (see kettle-render's
                // upload.rs).
                "render_uploads": target.renderer.as_ref().map(|r| {
                    let u = r.render_uploads();
                    serde_json::json!({
                        "frames_presented": u.frames_presented,
                        "buffer_writes": u.buffer_writes,
                        "buffer_bytes": u.buffer_bytes,
                        "texture_writes": u.texture_writes,
                        "text_prepares": u.text_prepares,
                        "chrome_prepares": u.chrome_prepares,
                        "skipped_writes": u.skipped_writes,
                        "mapped_uploads": u.mapped_uploads,
                        "mapped_writes": u.mapped_writes,
                        "mapped_bytes": u.mapped_bytes,
                    })
                }),
                // Who draws the blink: the GPU, or the window server's layer
                // (macOS). Timing, geometry and counts only.
                "cursor_blink": self.cursor_blink_diagnostics(target),
        });
        geometry["confirmation"] = serde_json::json!(confirmation);
        if let serde_json::Value::Object(fields) = &mut geometry {
            if let Some(serde_json::Value::Object(tab_bar)) = fields.get_mut("tab_bar") {
                tab_bar.insert("update_chip".into(), serde_json::Value::Null);
            }
            // The update notice in its own field, so live smokes can check its
            // state independently of toast visibility.
            fields.insert("update_notice".into(), update_notice.into());
            fields.insert("update_toast".into(), update_toast.into());
            fields.insert("update_toast_delivery".into(), serde_json::json!({
                "eligible": self.toast_visible(target),
                "shown": target.window_shown,
                "occluded": target.window_occluded,
                "minimized": target.window.as_ref().and_then(|w| w.is_minimized()),
                "claiming": self.update_toast_claiming,
                "handled": self.update_toast_handled,
                "unavailable": self.update_toast_unavailable,
                "retry_in_ms": self.update_toast_retry.map(|at| at.saturating_duration_since(std::time::Instant::now()).as_millis()),
                "receipt_pending": self.update_toast_receipts.as_ref().map_or(0, crate::update_toast::ReceiptWriter::pending_count),
                "active": self.update_toast.as_ref().map(|t| serde_json::json!({"notice": t.claim.notice,"window":t.window,"presented":t.presented,"remaining_ms":t.remaining().as_millis()})),
            }));
            fields.insert(
                "fullscreen".into(),
                serde_json::Value::Bool(
                    target
                        .window
                        .as_ref()
                        .is_some_and(|window| window.fullscreen().is_some()),
                ),
            );
            fields.insert(
                "inline_cards".into(),
                serde_json::Value::Array(inline_cards),
            );
            fields.insert(
                "preview_lanes".into(),
                serde_json::Value::Array(preview_lanes),
            );
            fields.insert("preview_overlay".into(), preview_overlay.into());
            fields.insert(
                "desktop".into(),
                serde_json::json!({
                    "client_origin": target.window.as_ref()
                        .and_then(|w| w.inner_position().ok())
                        .map(|p| serde_json::json!({"x": p.x, "y": p.y})),
                    "frame_origin": target.window.as_ref()
                        .and_then(|w| w.outer_position().ok())
                        .map(|p| serde_json::json!({"x": p.x, "y": p.y})),
                }),
            );
        }
        Response::ok(req.id, geometry)
    }

    /// `ui_geometry.cursor_blink`: the phase the screen shows, when it next
    /// changes, and the layer's hand-offs, exits and exit-frame times.
    pub(super) fn cursor_blink_diagnostics(&self, ws: &WindowState) -> serde_json::Value {
        let now = std::time::Instant::now();
        let layer = &ws.blink_layer;
        let (phase_on, next_edge) = match layer.active() {
            Some((anchor, plan, _)) if ws.last_blink == anchor => {
                let (on, edge) = crate::cursor_blink::materialize(
                    ws.blink_on,
                    ws.last_blink,
                    plan.activity,
                    plan.interval,
                    plan.timeout,
                    now,
                );
                let next = crate::cursor_blink::next_edge(
                    on,
                    edge,
                    plan.activity,
                    plan.interval,
                    plan.timeout,
                    now,
                );
                (on, next)
            }
            _ => {
                let running = ws.window_focused
                    && !window_is_render_hidden(ws)
                    && cursor_blink_active(
                        self.cfg.cursor_blink,
                        self.pane_cursor_blinking(ws, false),
                        ws.window_focused,
                    );
                let next = running
                    .then(|| {
                        crate::cursor_blink::next_edge(
                            ws.blink_on,
                            ws.last_blink,
                            ws.last_blink_activity,
                            std::time::Duration::from_millis(self.cfg.cursor_blink_interval),
                            self.cfg.cursor_blink_timeout(),
                            now,
                        )
                    })
                    .flatten();
                (ws.blink_on, next)
            }
        };
        let exit_frames = &layer.exit_frames;
        serde_json::json!({
            "renderer": if layer.is_active() { "layer" } else { "gpu" },
            "layer_rect": layer.active().map(|(_, _, rect)| rect),
            "phase_on": phase_on,
            "next_edge_ms": next_edge
                .map(|at| u64::try_from(at.saturating_duration_since(now).as_millis()).unwrap_or(u64::MAX)),
            "handoffs": layer.handoffs,
            "exits": layer.exits,
            "hides": layer.hides,
            "exit_frame_us": {
                "count": exit_frames.count(),
                "p50": exit_frames.percentile_us(50),
                "p95": exit_frames.percentile_us(95),
                "max": exit_frames.max_us(),
            },
            "fallback": layer.fallback(),
        })
    }

    /// `perform_action`: dispatch a named kettle app action against the focused
    /// window. This complements `send_keys`, which intentionally writes PTY
    /// bytes to the pane instead of firing global shortcuts, so diagnostics can
    /// open app chrome such as Search / Palette / Settings deterministically.
    pub(super) fn ctl_perform_action(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let Some(name) = req.params.get("action").and_then(|v| v.as_str()) else {
            return Response::err(req.id, ec::BAD_PARAMS, "missing 'action' string");
        };
        // Diagnostic automation needs an idempotent activation primitive.
        // Reusing the user-facing visibility toggle is racy: if winit's focus
        // event trails the native focus state, a smoke can hide the exact
        // window it is trying to raise. This control-only action never hides.
        if name == "focus_window" {
            let Some(window) = ws.window.as_ref() else {
                return Response::err(req.id, ec::INTERNAL, "target window is unavailable");
            };
            window.set_visible(true);
            window.focus_window();
            window.request_redraw();
            return Response::ok(
                req.id,
                serde_json::json!({
                    "action": "FocusWindow",
                    "requested": name,
                    "window": ws.seq,
                }),
            );
        }
        let Some(action) = kettle_config::Action::from_name(name) else {
            return Response::err(req.id, ec::BAD_PARAMS, format!("unknown action '{name}'"));
        };
        let resolved = format!("{action:?}");
        self.handle_action(ws, action, event_loop);
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
        Response::ok(
            req.id,
            serde_json::json!({
                "action": resolved,
                "requested": name,
                "window": ws.seq,
            }),
        )
    }

    /// `dispatch_keybind`: diagnostic route for app-level keybind matching.
    /// Unlike `send_keys`, this does not write PTY bytes; it exercises the same
    /// resolver as the real window keyboard path and dispatches the matched app
    /// action when no modal owns the keyboard. A chord the real keyboard path
    /// would hand to the terminal (an adaptive `Alt+Arrow` with no visible
    /// neighbour) is reported as `terminal_fallthrough` and not dispatched;
    /// the PTY write itself stays exclusive to `send_keys`.
    pub(super) fn ctl_dispatch_keybind(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let logical = match parse_ctl_logical_key(req.params.get("logical")) {
            Ok(k) => k,
            Err(e) => return Response::err(req.id, ec::BAD_PARAMS, e),
        };
        let physical = match parse_ctl_physical_key(req.params.get("physical")) {
            Ok(k) => k,
            Err(e) => return Response::err(req.id, ec::BAD_PARAMS, e),
        };
        if logical.is_none() && physical.is_none() {
            return Response::err(
                req.id,
                ec::BAD_PARAMS,
                "dispatch_keybind requires 'logical' or 'physical'",
            );
        }
        let mods = match parse_ctl_mods(req.params.get("mods")) {
            Ok(m) => m,
            Err(e) => return Response::err(req.id, ec::BAD_PARAMS, e),
        };
        let candidates = keybind_candidates(logical.as_ref(), physical.as_ref(), mods, None);
        let candidate_labels: Vec<String> = candidates.iter().map(Trigger::label).collect();
        let modal_blocked = self.any_modal_open(ws);
        if modal_blocked {
            return Response::ok(
                req.id,
                serde_json::json!({
                    "window": ws.seq,
                    "dispatched": false,
                    "modal_blocked": true,
                    "candidates": candidate_labels,
                }),
            );
        }
        let Some((trigger, action)) = resolve_keybind_action(
            &self.cfg.keybinds,
            logical.as_ref(),
            physical.as_ref(),
            mods,
            None,
        ) else {
            return Response::ok(
                req.id,
                serde_json::json!({
                    "window": ws.seq,
                    "dispatched": false,
                    "modal_blocked": false,
                    "candidates": candidate_labels,
                }),
            );
        };
        let trigger_label = trigger.label();
        let action_name = kettle_config::keybinds::action_label(&action);
        if self.chord_falls_through(ws, trigger, &action) {
            return Response::ok(
                req.id,
                serde_json::json!({
                    "window": ws.seq,
                    "dispatched": false,
                    "modal_blocked": false,
                    "terminal_fallthrough": true,
                    "trigger": trigger_label,
                    "action": action_name,
                    "candidates": candidate_labels,
                }),
            );
        }
        let font_size_before = ws.renderer.as_ref().map(|r| r.font_size());
        self.handle_action(ws, action, event_loop);
        let font_size_after = ws.renderer.as_ref().map(|r| r.font_size());
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
        Response::ok(
            req.id,
            serde_json::json!({
                "window": ws.seq,
                "dispatched": true,
                "modal_blocked": false,
                "terminal_fallthrough": false,
                "trigger": trigger_label,
                "action": action_name,
                "font_size_before": font_size_before,
                "font_size_after": font_size_after,
                "candidates": candidate_labels,
            }),
        )
    }

    /// `dispatch_ui_key`: drive the currently open Kettle-owned modal without
    /// ever encoding a byte for the PTY. This is intentionally separate from
    /// `send_keys`: diagnostics can exercise Search even when the focused pane
    /// is running an input-sensitive TUI, and a closed modal is a hard error.
    /// The batch and token caps keep one control request bounded on the UI
    /// thread. All tokens are parsed before state is changed.
    pub(super) fn ctl_dispatch_ui_key(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{Response, error_codes as ec};

        const MAX_UI_KEYS: usize = 64;
        const MAX_UI_KEY_BYTES: usize = 64;

        // Route to whichever text modal is open, in the same precedence the
        // real `KeyboardInput` arm uses, so every modal's text-entry rules get
        // end-to-end coverage, not just search's.
        let Some(modal) = open_text_modal(ws) else {
            return Response::err(req.id, ec::BAD_PARAMS, "no supported UI modal is open");
        };
        let Some(keys) = req.params.get("keys").and_then(|value| value.as_array()) else {
            return Response::err(req.id, ec::BAD_PARAMS, "missing 'keys' array");
        };
        if keys.is_empty() || keys.len() > MAX_UI_KEYS {
            return Response::err(
                req.id,
                ec::BAD_PARAMS,
                format!("'keys' must contain 1..={MAX_UI_KEYS} entries"),
            );
        }

        let mut parsed = Vec::with_capacity(keys.len());
        for (index, value) in keys.iter().enumerate() {
            let Some(token) = value.as_str() else {
                return Response::err(req.id, ec::BAD_PARAMS, "non-string entry in 'keys'");
            };
            if token.is_empty() || token.len() > MAX_UI_KEY_BYTES {
                return Response::err(
                    req.id,
                    ec::BAD_PARAMS,
                    format!("UI key tokens must be 1..={MAX_UI_KEY_BYTES} bytes"),
                );
            }
            let Some((mods, key)) = parse_ui_key(token) else {
                return Response::err(
                    req.id,
                    ec::BAD_PARAMS,
                    format!("unrecognized UI key token at index {index}"),
                );
            };
            parsed.push((mods, key));
        }

        let previous_mods = ws.mods;
        #[cfg(target_os = "macos")]
        let previous_raw_mods = ws.macos_raw_mods;
        let mut applied = 0usize;
        for (mods, key) in &parsed {
            ws.mods = *mods;
            // The word-edit gates read the raw Option shadow rather than the
            // masked value (`option_physically_held`), so a control-plane
            // `alt+backspace` has to set both. Setting only `ws.mods` would let
            // a live smoke pass against code that never restores Option.
            #[cfg(target_os = "macos")]
            {
                ws.macos_raw_mods = *mods;
            }
            // Mirror `winit`: `KeyEvent::text` is present for a character key
            // under every modifier except Control, Command included.
            // Suppressing Super here would let a `cmd+v` test pass against a
            // handler that types a literal `v`. The modifier rule belongs in
            // `modal_input::accept_text`, not in the transport.
            let text = match key {
                Key::Character(text) if !mods.control_key() => Some(text.as_str()),
                _ => None,
            };
            match modal {
                TextModal::Confirm => {
                    self.confirm_dialog_key(ws, key, event_loop);
                    if confirmation_space_key(key) {
                        self.confirm_space_release(ws, true);
                    }
                }
                TextModal::Hint => self.hint_key(ws, key, text),
                TextModal::Palette => self.palette_key(ws, key, text, event_loop),
                TextModal::ThemePicker => self.theme_picker_key(ws, key, text),
                TextModal::SettingsText => self.settings_text_key(ws, key, text),
                TextModal::Settings => self.settings_key(ws, key, event_loop),
                TextModal::LayoutPicker => self.layout_picker_key(ws, key, text),
                TextModal::Ssh => self.ssh_key(ws, key, text),
                TextModal::TitleEdit => self.title_edit_key(ws, key, text),
                TextModal::Search => {
                    if !self.search_key(ws, key, None, text, None) {
                        self.search_bar_shortcut(ws, key, None, false, event_loop, None);
                    }
                }
                TextModal::Preview => self.preview_focus_key(ws, key, false),
            }
            applied += 1;
            // Stop as soon as the modal this batch addressed is gone (Esc,
            // Enter, or an action that replaced it). Continuing would type the
            // remaining keys into whatever took its place — including the PTY.
            if open_text_modal(ws) != Some(modal) {
                end_modal_composition(ws);
                break;
            }
        }
        ws.mods = previous_mods;
        #[cfg(target_os = "macos")]
        {
            ws.macos_raw_mods = previous_raw_mods;
        }
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        Response::ok(
            req.id,
            serde_json::json!({
                "window": ws.seq,
                "modal": modal.as_str(),
                "keys": applied,
                "requested_keys": parsed.len(),
                "open": open_text_modal(ws) == Some(modal),
            }),
        )
    }

    /// `resize_window`: request a live OS-window client-area resize. The real
    /// `WindowEvent::Resized` path applies renderer/surface/PTY grid changes.
    pub(super) fn ctl_resize_window(
        &mut self,
        ws: &mut WindowState,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let target_seq = match optional_u64_param(&req.params, "window") {
            Ok(window) => window.unwrap_or(ws.seq),
            Err(message) => return Response::err(req.id, ec::BAD_PARAMS, message),
        };
        let Some(width) = req.params.get("width").and_then(|v| v.as_u64()) else {
            return Response::err(req.id, ec::BAD_PARAMS, "missing integer 'width'");
        };
        let Some(height) = req.params.get("height").and_then(|v| v.as_u64()) else {
            return Response::err(req.id, ec::BAD_PARAMS, "missing integer 'height'");
        };
        if width == 0 || height == 0 || width > u32::MAX as u64 || height > u32::MAX as u64 {
            return Response::err(
                req.id,
                ec::BAD_PARAMS,
                "'width' and 'height' must be in 1..=u32::MAX",
            );
        }
        if target_seq == ws.seq {
            return self.ctl_resize_window_for_target(ws, req.id, width as u32, height as u32);
        }
        let Some(mut target) = self.windows.remove(&target_seq) else {
            return Response::err(
                req.id,
                ec::BAD_PARAMS,
                format!("no window with id {target_seq}"),
            );
        };
        let resp =
            self.ctl_resize_window_for_target(&mut target, req.id, width as u32, height as u32);
        self.windows.insert(target_seq, target);
        resp
    }

    pub(super) fn ctl_resize_window_for_target(
        &mut self,
        target: &mut WindowState,
        request_id: u64,
        width: u32,
        height: u32,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let Some(window) = target.window.clone() else {
            return Response::err(request_id, ec::INTERNAL, "window is not available");
        };
        let requested = winit::dpi::PhysicalSize::new(width, height);
        let applied = window.request_inner_size(requested).unwrap_or(requested);
        self.apply_window_resize(target, applied.width, applied.height);
        window.request_redraw();
        Response::ok(
            request_id,
            serde_json::json!({
                "window": target.seq,
                "requested": {"width": width, "height": height},
                "applied": {"width": applied.width, "height": applied.height},
            }),
        )
    }

    /// `send_mouse`: deterministic local-user mouse input for diagnostics and
    /// end-to-end agent/TUI tests. Coordinates are physical pixels relative to
    /// the target kettle window's client area.
    pub(super) fn ctl_send_mouse(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let target_seq = match optional_u64_param(&req.params, "window") {
            Ok(window) => window.unwrap_or(ws.seq),
            Err(message) => return Response::err(req.id, ec::BAD_PARAMS, message),
        };
        if target_seq == ws.seq {
            return self.ctl_send_mouse_for_window(ws, event_loop, req);
        }
        let Some(mut target) = self.windows.remove(&target_seq) else {
            return Response::err(
                req.id,
                ec::BAD_PARAMS,
                format!("no window with id {target_seq}"),
            );
        };
        let resp = self.ctl_send_mouse_for_window(&mut target, event_loop, req);
        // A synthetic event can close this mapped target's last tab. Consume
        // only that window's request here; the caller remains checked out and
        // must not inherit the target's close.
        if take_window_close_request(&mut self.pending_window_closes, target_seq) {
            self.close_update_toast_window(target_seq);
            if self
                .torn_drag
                .as_ref()
                .is_some_and(|t| t.seq == target_seq || t.carrier == target_seq)
            {
                self.abandon_torn_drag(None);
            }
            drop(target);
        } else {
            self.windows.insert(target_seq, target);
        }
        resp
    }

    pub(super) fn ctl_send_mouse_for_window(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let Some(event) = req.params.get("event").and_then(|v| v.as_str()) else {
            return Response::err(req.id, ec::BAD_PARAMS, "missing 'event' string");
        };
        let event = event.to_ascii_lowercase();
        if !matches!(
            event.as_str(),
            "move" | "press" | "release" | "click" | "wheel"
        ) {
            return Response::err(
                req.id,
                ec::BAD_PARAMS,
                "event must be move, press, release, click, or wheel",
            );
        }
        let button = if matches!(event.as_str(), "press" | "release" | "click") {
            match ctl_mouse_button(&req.params) {
                Ok(button) => Some(button),
                Err(error) => return Response::err(req.id, ec::BAD_PARAMS, error),
            }
        } else {
            None
        };
        // A wheel event takes EITHER a pre-quantized whole-line count
        // (`wheel_lines`, the historical form) or a raw, possibly fractional
        // detent count (`wheel_delta`, the units winit actually reports).
        // `wheel_delta` is the only synthetic path that runs the real
        // sub-detent accumulator, so it is what lets a test reproduce a
        // precision-touchpad gesture.
        let mut wheel_lines = None;
        let mut wheel_delta = None;
        if event == "wheel" {
            match (
                req.params.get("wheel_delta").and_then(|v| v.as_f64()),
                req.params.get("wheel_lines").and_then(|v| v.as_i64()),
            ) {
                (Some(_), Some(_)) => {
                    return Response::err(
                        req.id,
                        ec::BAD_PARAMS,
                        "pass either 'wheel_delta' or 'wheel_lines', not both",
                    );
                }
                (Some(delta), None) => {
                    if !delta.is_finite() {
                        return Response::err(
                            req.id,
                            ec::BAD_PARAMS,
                            "'wheel_delta' must be finite",
                        );
                    }
                    wheel_delta = Some(delta);
                }
                (None, Some(lines)) => {
                    wheel_lines = Some(lines.clamp(i32::MIN as i64, i32::MAX as i64) as i32);
                }
                (None, None) => {
                    return Response::err(
                        req.id,
                        ec::BAD_PARAMS,
                        "missing 'wheel_lines' integer or 'wheel_delta' number",
                    );
                }
            }
        }
        let event_mods = if req.params.get("mods").is_some() {
            match parse_ctl_mods(req.params.get("mods")) {
                Ok(mods) => mods,
                Err(error) => return Response::err(req.id, ec::BAD_PARAMS, error),
            }
        } else {
            ws.mods
        };
        let x = req.params.get("x").and_then(|v| v.as_f64());
        let y = req.params.get("y").and_then(|v| v.as_f64());
        if !matches!(event.as_str(), "wheel") || x.is_some() || y.is_some() {
            let Some(x) = x else {
                return Response::err(req.id, ec::BAD_PARAMS, "missing numeric 'x'");
            };
            let Some(y) = y else {
                return Response::err(req.id, ec::BAD_PARAMS, "missing numeric 'y'");
            };
            if !x.is_finite() || !y.is_finite() {
                return Response::err(req.id, ec::BAD_PARAMS, "'x' and 'y' must be finite");
            }
            ws.cursor = winit::dpi::PhysicalPosition::new(x, y);
        }

        // Synthetic modifiers apply only to this control event; real winit
        // modifier state resumes immediately afterward.
        let previous_mods = std::mem::replace(&mut ws.mods, event_mods);
        let mut handled = true;
        match event.as_str() {
            "move" => {
                self.ctl_mouse_move(ws);
            }
            "press" => {
                handled = self.ctl_mouse_press(ws, event_loop, button.expect("validated button"));
            }
            "release" => {
                handled = self.ctl_mouse_release(ws, event_loop, button.expect("validated button"));
            }
            "click" => {
                let bcode = button.expect("validated button");
                let pressed = self.ctl_mouse_press(ws, event_loop, bcode);
                let released = self.ctl_mouse_release(ws, event_loop, bcode);
                handled = pressed || released;
            }
            "wheel" => {
                handled = match (wheel_delta, wheel_lines) {
                    (Some(delta), _) => self.ctl_mouse_wheel_delta(ws, delta),
                    (None, Some(lines)) => self.ctl_mouse_wheel(ws, lines),
                    (None, None) => unreachable!("wheel params validated above"),
                };
            }
            _ => unreachable!("event validated above"),
        }
        ws.mods = previous_mods;
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
        Response::ok(
            req.id,
            serde_json::json!({
                "window": ws.seq,
                "event": event,
                "cursor": [ws.cursor.x, ws.cursor.y],
                "handled": handled,
            }),
        )
    }

    pub(super) fn ctl_mouse_move(&mut self, ws: &mut WindowState) {
        // A control-driven move back inside the client supersedes a native
        // `CursorLeft` edge latch. Without this reset, mixing the two input
        // sources keeps scrolling even though the latest pointer coordinate is
        // back over the pane.
        ws.selection_autoscroll_edge = 0;
        if self.resize_preview_lane(ws) || self.drag_preview_lane(ws) {
            return;
        }
        // Like the media receipt below, the search lane's hover follows
        // control input so automation can exercise it.
        self.update_search_hover(ws);
        if search_pointer_route(ws.search.open, None, ws.search.pointer_captured())
            == SearchPointerRoute::Bar
        {
            self.search_mouse_drag(ws);
            return;
        }
        if self.drag_pane(ws) {
            return;
        }
        if self.drag_split_divider(ws) {
            return;
        }
        if self.picker_pointer(ws, PickerPointer::Hover) {
            return;
        }
        self.promote_tab_drag_if_needed(ws);
        if ws.tab_drag_active && ws.tab_drag_press.is_none() {
            self.reorder_active_tab_for_cursor(ws);
        }
        // `None` is xterm's "no button" code 3, which with the motion bit is
        // the `CSI < 35 ; x ; y M` report DEC 1003 exists to deliver. Motion
        // reaches `send_mouse` without a held button too; otherwise 1003 acts
        // like 1002 while DECRQM still reports it set, and hover highlighting
        // in Neovim, lazygit, btop and fzf goes dead. `send_mouse` decides per
        // mode whether to report.
        // A held button's drag goes where its press did; `report_mouse`
        // picks the button.
        let _ = self.send_mouse(ws, input::MOUSE_NO_BUTTON, true, true);
        if ws.selecting {
            note_selection_drag_motion(ws);
            let area = self.area(ws);
            self.update_selection(ws, area);
        }
        // Control input may expand the receipt for deterministic automation,
        // but it must not retarget the physical cursor or unrelated tab-bar
        // hover state. Only a native CursorMoved event owns those affordances.
        self.update_media_paste_receipt_hover(ws);
    }

    pub(super) fn ctl_mouse_press(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        bcode: u8,
    ) -> bool {
        // As natively: a card press whose release never came is over, as is
        // a lane's drag, a program still holding this button gets its
        // release, and the press is Kettle's own gesture until a program
        // receives it. Focus leaves any card.
        self.end_lane_resize(ws);
        ws.lane_drag = None;
        self.leave_preview_focus_for_press(ws);
        if bcode == 0 {
            ws.card_press = None;
        }
        if ws.card_focus.take().is_some() {
            ws.accessibility_pending = true;
        }
        self.release_held_buttons(ws, |held| held.button == bcode);
        ws.held_buttons.pressed(bcode, None, (0, 0));
        if self.confirm_mouse_press(ws, event_loop, bcode) {
            return true;
        }
        let bar = self.tab_bar(ws);
        let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
        if bcode == 0 && self.picker_pointer(ws, PickerPointer::Press) {
            return true;
        }
        if bcode == 0
            && let Some(delta) = self.context_menu_scroll_edge(ws)
        {
            self.scroll_context_menu(ws, delta);
            return true;
        }
        if ws.context_menu.is_some()
            && let Some(click) = self.context_menu_click_action(ws, bcode)
        {
            self.dispatch_context_menu_click(ws, click, event_loop);
            return true;
        }
        if ws.context_menu.is_some() && bcode != 2 {
            ws.context_menu = None;
            return true;
        }
        if search_pointer_route(
            ws.search.open,
            Some(self.cursor_in_search_bar(ws)),
            ws.search.dragging_editor,
        ) == SearchPointerRoute::Bar
        {
            if bcode == 0 {
                self.search_mouse_press(ws);
            }
            return true;
        }
        if ws.settings_nav.is_some()
            && (bcode == 0 || bcode == 2)
            && self.settings_mouse(ws, if bcode == 2 { -1 } else { 1 }, true)
        {
            return true;
        }
        if ws.preview_overlay.is_some() && !self.non_search_modal_open(ws) {
            return self.press_preview_lane(ws, px, py, bcode);
        }
        if modal_swallows_pointer(self.pointer_modal_open(ws), ws.context_menu.is_some()) {
            return true;
        }
        if self.press_update_toast(ws, px, py, bcode) {
            return true;
        }
        if self.press_preview_lane(ws, px, py, bcode) {
            return true;
        }
        if self.press_card(ws, px, py, bcode) {
            return true;
        }
        if tab_bar_pointer_region_contains(&bar, px, py) && (bcode == 0 || bcode == 1) {
            if bcode == 0 && bar.new_tab_menu.2 > 0.0 && rect_contains(bar.new_tab_menu, px, py) {
                let (ax, ay, _, ah) = bar.new_tab_menu;
                self.open_new_tab_menu(ws, ax, ay + ah);
            } else if bcode == 0 && rect_contains(bar.new_tab, px, py) {
                let area = self.area(ws);
                let (cols, rows) = self.grid_of(ws, area);
                let geometry = self.pty_geometry_for_grid(ws, cols, rows);
                match ws.mux.new_tab_geometry(&self.cfg, geometry, self.waker()) {
                    // Fire TabAdd so Lua / dev-record see tabs opened via the
                    // agent `+` click, like every other new-tab path.
                    Ok(()) => self.fire_tab_add_event(ws),
                    Err(e) => log::warn!("could not open a new tab (agent send_mouse): {e}"),
                }
            } else if bcode == 0
                && bar.scroll_left.2 > 0.0
                && rect_contains(bar.scroll_left, px, py)
            {
                ws.mux.prev_tab();
            } else if bcode == 0
                && bar.scroll_right.2 > 0.0
                && rect_contains(bar.scroll_right, px, py)
            {
                ws.mux.next_tab();
            } else if let Some(seg) = bar
                .segments
                .iter()
                .find(|seg| rect_contains(seg.rect, px, py))
            {
                let close = bcode == 1 || rect_contains(seg.close, px, py);
                if close {
                    // Same close as the GUI ✕/middle-click above, so the same
                    // `ask-before-closing` question — an agent driving the tab
                    // bar must not be a way around the user's own policy.
                    let (panes, busy) = tab_close_scope(ws, seg.idx);
                    if let Some(anchor) =
                        Some(ws.mux.tab_anchor_panes(seg.idx)).filter(|a| !a.is_empty())
                        && self.confirm_close(
                            ws,
                            panes,
                            busy,
                            ConfirmAction::CloseTabHolding(anchor),
                        )
                    {
                        // Handled: the press raised the prompt, so it must not
                        // also reach the pane underneath.
                        return true;
                    }
                    let pre = self.focus_key(ws);
                    let closing_idx = seg.idx;
                    if ws.mux.close_tab_at(seg.idx) {
                        self.fire_tab_close_event(ws, closing_idx);
                        self.save_session(ws);
                        self.request_window_close(ws.seq);
                    } else {
                        self.fire_tab_close_event(ws, closing_idx);
                        self.note_focus_change(ws, pre);
                    }
                } else {
                    let pre = self.focus_key(ws);
                    ws.mux.active = seg.idx;
                    ws.mux.touch_active_tab_seen();
                    self.note_focus_change(ws, pre);
                    if bcode == 0 {
                        ws.tab_drag_active = true;
                        ws.tab_drag_press = Some((px, py));
                        ws.tab_pressed_idx = Some(seg.idx);
                        if self.cfg.detachable_tabs {
                            ws.detach_drag =
                                crate::detach::DragState::on_mouse_down_on_tab(seg.idx);
                            ws.drag_press = Some((px, py));
                        }
                    }
                }
            }
            self.resize_all(ws);
            return true;
        }

        let area = self.area(ws);
        if self.activate_media_paste_receipt_at(ws, px, py, bcode) {
            return true;
        }
        if self.press_pane_drag(ws, bcode) {
            return true;
        }
        if self.press_split_divider(ws, bcode) {
            return true;
        }
        let pre = self.focus_key(ws);
        ws.mux.focus_at(area, px, py);
        self.note_focus_change(ws, pre);
        if self.send_mouse(ws, bcode, true, false) {
            return true;
        }
        if bcode == 2 {
            let offer_copied = ws.mods.shift_key();
            self.open_context_menu(ws, px, py, offer_copied);
            return true;
        }
        if bcode == 0 {
            self.begin_or_extend_mouse_selection(ws, area);
            return true;
        }
        false
    }

    pub(super) fn ctl_mouse_release(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        bcode: u8,
    ) -> bool {
        // As natively: a gesture of Kettle's own ends with its release.
        ws.held_buttons.end_own(bcode);
        if bcode == 0 {
            ws.dragging_split = None;
        }
        if self.confirm_mouse_release(ws, bcode) {
            return true;
        }
        if bcode == 0 && ws.lane_resize.is_some() {
            self.end_lane_resize(ws);
            return true;
        }
        if bcode == 0
            && let Some(drag) = ws.lane_drag.take()
        {
            self.end_lane_drag(ws, drag);
            return true;
        }
        if bcode == 0
            && search_pointer_route(ws.search.open, None, ws.search.pointer_captured())
                == SearchPointerRoute::Bar
        {
            ws.search.dragging_editor = false;
            self.search_button_release(ws);
            return true;
        }
        let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
        if self.release_card(ws, px, py, bcode) {
            return true;
        }
        if self.release_pane_drag(ws, bcode, event_loop) {
            return true;
        }
        // The release goes where its press did, if a program got it.
        let mut handled = self.send_mouse(ws, bcode, false, false);
        if selection_release_matches(ws.selection_button, bcode) {
            self.finish_selection_gesture(ws);
            handled = true;
        }
        if bcode == 0 {
            ws.scrollbar_drag_offset = None;
            ws.tab_drag_active = false;
            ws.tab_drag_press = None;
            ws.tab_pressed_idx = None;
            ws.detach_drag = std::mem::take(&mut ws.detach_drag).on_mouse_up();
            ws.drag_press = None;
            handled = true;
        }
        handled
    }

    /// ctl/MCP `send_mouse {event:"wheel"}` with a pre-quantized whole-line
    /// count. The caller has already decided how many lines to move, so this
    /// skips the accumulator and derives detents from the historical
    /// 3-lines-per-notch ratio.
    pub(super) fn ctl_mouse_wheel(&mut self, ws: &mut WindowState, lines: i32) -> bool {
        if lines == 0 {
            return false;
        }
        // At least one detent in the same direction, so callers that inject a
        // single line still cycle exactly one tab / one font step.
        let notches = match lines / 3 {
            0 => lines.signum(),
            n => n,
        };
        self.dispatch_wheel(
            ws,
            input::WheelSteps {
                notches,
                lines,
                cols: 0,
            },
        )
    }

    /// ctl/MCP `send_mouse {event:"wheel", wheel_delta:<f64>}` with a RAW,
    /// possibly fractional detent count — the same units winit reports.
    ///
    /// This is the only synthetic path that exercises the real accumulator, so
    /// it is what lets an automated test reproduce a precision-touchpad gesture
    /// (a stream of ~0.08-detent events). The integer `wheel_lines` form cannot,
    /// because it enters downstream of quantization.
    pub(super) fn ctl_mouse_wheel_delta(&mut self, ws: &mut WindowState, notches: f64) -> bool {
        if self.lane_takes_wheel(ws) {
            ws.wheel.reset();
            self.wheel_over_lane(
                ws,
                &winit::event::MouseScrollDelta::LineDelta(0.0, notches as f32),
            );
            return true;
        }
        ws.lane_wheel.reset();
        let steps = ws.wheel.feed(
            &winit::event::MouseScrollDelta::LineDelta(0.0, notches as f32),
            self.cfg.scroll_multiplier,
        );
        if steps.is_zero() {
            // Banked as residue — a real gesture needs several of these before
            // the first whole step comes out.
            return false;
        }
        self.dispatch_wheel(ws, steps)
    }

    /// `screenshot`: queue a live-surface PNG capture and reply when the next
    /// rendered frame saves it. Defaults to the focused pane crop; pass
    /// `{full_window:true}` to capture the whole window, pass all four `crop_*`
    /// values for a physical-pixel window region, or use `{path:"…"}` to choose
    /// the output file.
    pub(super) fn ctl_screenshot(
        &mut self,
        ws: &mut WindowState,
        req: &kettle_ctl::protocol::Request,
        reply: crate::ctl_server::ReplyTx,
    ) {
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let full_window = req
            .params
            .get("full_window")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let explicit_crop = match screenshot_crop(&req.params) {
            Ok(crop) => crop,
            Err(message) => {
                let _ = reply.send(Response::err(req.id, ec::BAD_PARAMS, message));
                return;
            }
        };
        if explicit_crop.is_some() && req.params.get("pane").is_some() {
            let _ = reply.send(Response::err(
                req.id,
                ec::BAD_PARAMS,
                "an explicit crop cannot be combined with pane; crop coordinates are window-relative",
            ));
            return;
        }
        if explicit_crop.is_some() && full_window {
            let _ = reply.send(Response::err(
                req.id,
                ec::BAD_PARAMS,
                "an explicit crop cannot be combined with full_window=true",
            ));
            return;
        }
        let pane = if (full_window || explicit_crop.is_some()) && req.params.get("pane").is_none() {
            None
        } else {
            match self.ctl_resolve_pane(ws, &req.params) {
                Ok(p) => Some(p),
                Err((code, message)) => {
                    let _ = reply.send(Response::err(req.id, code, message));
                    return;
                }
            }
        };
        let target_seq = match pane {
            Some(id) if ws.mux.panes.contains_key(&id) => Some(ws.seq),
            Some(id) => self
                .windows
                .values()
                .find(|w| w.mux.panes.contains_key(&id))
                .map(|w| w.seq),
            None => Some(ws.seq),
        };
        let Some(target_seq) = target_seq else {
            let _ = reply.send(Response::err(req.id, ec::NO_SUCH_PANE, "pane vanished"));
            return;
        };
        let target_accepts_capture = {
            let target = if target_seq == ws.seq {
                Some(&*ws)
            } else {
                self.windows.get(&target_seq)
            };
            target
                .and_then(|target| {
                    target
                        .window
                        .as_ref()
                        .map(|window| (window, target.window_shown))
                })
                .is_some_and(|(window, shown)| {
                    screenshot_window_accepts_control_capture(
                        window.is_minimized(),
                        shown,
                        window.is_visible(),
                    )
                })
        };
        if !target_accepts_capture {
            let _ = reply.send(Response::err(
                req.id,
                ec::BUSY,
                "target window is minimized, hidden, or not yet shown; restore it before capturing",
            ));
            return;
        }
        let crop = if let Some(crop) = explicit_crop {
            Some(crop)
        } else if full_window {
            None
        } else {
            let Some(pane_id) = pane else {
                let _ = reply.send(Response::err(req.id, ec::BAD_PARAMS, "missing pane"));
                return;
            };
            let target = if target_seq == ws.seq {
                Some(&*ws)
            } else {
                self.windows.get(&target_seq)
            };
            target.and_then(|target| {
                let area = self.area(target);
                let active = target.mux.active;
                // A pane screenshot shows the whole pane, its titlebar and
                // lane included.
                target
                    .mux
                    .leaves(active, area)
                    .into_iter()
                    .find(|(id, _)| *id == pane_id)
                    .map(|(_, rect)| rect)
            })
        };
        if !full_window && crop.is_none() {
            let _ = reply.send(Response::err(
                req.id,
                ec::BUSY,
                self.ui_text
                    .text(kettle_i18n::Text::CtlScreenshotPaneNotVisible),
            ));
            return;
        }
        // A pane in a background tab or behind another pane's zoom has no
        // rectangle to crop; refuse rather than capture the whole window.
        if !full_window && crop.is_none() {
            let _ = reply.send(Response::err(
                req.id,
                ec::BUSY,
                self.ui_text
                    .text(kettle_i18n::Text::CtlScreenshotPaneNotVisible),
            ));
            return;
        }
        let (path, output_policy) = match req.params.get("path").and_then(|v| v.as_str()) {
            // Reject an existing destination up front for a prompt response; the
            // renderer independently enforces create-new, owner-only publication
            // after this metadata probe. See `validate_screenshot_path` for the
            // full threat model.
            Some(s) => match validate_screenshot_path(s) {
                Ok(p) => (p, kettle_render::ScreenshotOutputPolicy::UserSelected),
                Err(message) => {
                    let _ = reply.send(Response::err(req.id, ec::BAD_PARAMS, message));
                    return;
                }
            },
            None => {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let cache = cache_dir_from_env(|k| std::env::var(k).ok());
                (
                    session_screenshot_path(secs, std::process::id(), cache.as_deref()),
                    kettle_render::ScreenshotOutputPolicy::PrivateState,
                )
            }
        };
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let cancellation = kettle_render::ScreenshotCancellation::default();
        let renderer = if target_seq == ws.seq {
            ws.renderer.as_mut()
        } else {
            self.windows
                .get_mut(&target_seq)
                .and_then(|w| w.renderer.as_mut())
        };
        let Some(renderer) = renderer else {
            let _ = reply.send(Response::err(
                req.id,
                ec::INTERNAL,
                "target window has no renderer",
            ));
            return;
        };
        let screenshot = kettle_render::ScreenshotRequest {
            out_path: path.clone(),
            output_policy,
            crop,
            completion: Some(done_tx),
            cancellation: Some(cancellation.clone()),
            recovery_wake: {
                let proxy = self.proxy.clone();
                Some(kettle_render::ScreenshotRecoveryWake::new(move || {
                    let _ = proxy.send_event(UserEvent::Wakeup);
                }))
            },
        };
        if renderer.set_pending_screenshot(screenshot).is_err() {
            let _ = reply.send(Response::err(
                req.id,
                ec::BUSY,
                "another screenshot capture is already in progress",
            ));
            return;
        }
        if target_seq == ws.seq {
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
        } else if let Some(target) = self.windows.get(&target_seq)
            && let Some(w) = &target.window
        {
            w.request_redraw();
        }
        let request_id = req.id;
        std::thread::spawn(move || {
            let response_for_result = |result| match result {
                Ok(saved) => Response::ok(
                    request_id,
                    serde_json::json!({
                        "path": saved,
                        "pane": pane,
                        "window": target_seq,
                        "full_window": full_window,
                        "crop": explicit_crop,
                    }),
                ),
                Err(error) => Response::err(request_id, ec::INTERNAL, error),
            };
            let resp = match done_rx.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(result) => response_for_result(result),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if cancellation.cancel() => {
                    Response::err(request_id, ec::INTERNAL, "screenshot capture timed out")
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // The encoder already won the final publication race. Give
                    // the atomic link/rename and directory sync one finite
                    // grace period, but never turn the advertised ten-second
                    // deadline into an unbounded control thread. If even this
                    // expires, report the only honest outcome: publication may
                    // have happened and the caller must inspect the path.
                    match done_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                        Ok(result) => response_for_result(result),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Response::err(
                            request_id,
                            ec::INTERNAL,
                            "screenshot publication did not finish within the extended deadline; the destination may exist",
                        ),
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Response::err(
                            request_id,
                            ec::INTERNAL,
                            "screenshot worker disconnected after publication began",
                        ),
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = cancellation.cancel();
                    Response::err(
                        request_id,
                        ec::INTERNAL,
                        "screenshot capture worker disconnected",
                    )
                }
            };
            let _ = reply.send(resp);
        });
    }

    /// `send_text`: write text to a pane's PTY (default: focused).
    pub(super) fn ctl_send_text(
        &mut self,
        ws: &mut WindowState,
        conn_id: u64,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        self.sync_pane_focus_reports(ws);
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let pane = match self.ctl_resolve_pane(ws, &req.params) {
            Ok(p) => p,
            Err((code, message)) => return Response::err(req.id, code, message),
        };
        let Some(text) = req.params.get("text").and_then(|v| v.as_str()) else {
            return Response::err(req.id, ec::BAD_PARAMS, "missing 'text' string");
        };
        if self.preview_holds_keyboard(ws, pane) {
            return Response::err(req.id, ec::BUSY, PREVIEW_HOLDS_KEYBOARD);
        }
        let Some(p) = Self::ctl_pane_ref(ws, &self.windows, pane) else {
            return Response::err(req.id, ec::NO_SUCH_PANE, "pane vanished");
        };
        // An agent acts as the user — the per-pane read-only
        // toggle (Terminator parity) blocks it like any other input, with an
        // explicit error instead of a silent drop.
        let result = p.feed_input(text.as_bytes());
        if let Some((code, message)) = ctl_input_error(result) {
            return Response::err(req.id, code, message);
        }
        if !text.is_empty() {
            self.dismiss_media_paste_receipt_for_pane(ws, pane);
        }
        log::info!(
            "agent-server: send_text conn={conn_id} pane={pane} ({} bytes)",
            text.len()
        );
        self.ctl_attach(ws, conn_id, pane);
        Response::ok(
            req.id,
            serde_json::json!({"pane": pane, "bytes": text.len()}),
        )
    }

    /// `send_keys`: press named keys and chords in a pane. `send_text` only
    /// types literal characters (CR included), so it cannot press Escape,
    /// arrows, Ctrl-chords, or F-keys (the keys that drive vim, htop, fzf, and
    /// tmux). Tokens go through the SAME `input::encode` path as GUI keystrokes,
    /// against the pane's LIVE terminal mode, so arrows honor DECCKM
    /// application-cursor mode like a human's key press.
    ///
    /// Params: `{pane?: u64, keys: ["escape", "ctrl+c", "down", "G", …]}`.
    /// All tokens are parsed BEFORE any byte is written, so a typo mid-sequence
    /// cannot leave the target app half-keyed.
    pub(super) fn ctl_send_keys(
        &mut self,
        ws: &mut WindowState,
        conn_id: u64,
        req: &kettle_ctl::protocol::Request,
    ) -> kettle_ctl::protocol::Response {
        self.sync_pane_focus_reports(ws);
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let pane = match self.ctl_resolve_pane(ws, &req.params) {
            Ok(p) => p,
            Err((code, message)) => return Response::err(req.id, code, message),
        };
        let Some(keys) = req.params.get("keys").and_then(|v| v.as_array()) else {
            return Response::err(
                req.id,
                ec::BAD_PARAMS,
                "missing 'keys' array of key tokens (e.g. [\"escape\",\"ctrl+c\"])",
            );
        };
        let parsed = match parse_ctl_send_key_batch(keys) {
            Ok(parsed) => parsed,
            Err(message) => return Response::err(req.id, ec::BAD_PARAMS, message),
        };
        // The user gave the keyboard to a preview in that window: nothing
        // is typed into it until they take it back.
        if self.preview_holds_keyboard(ws, pane) {
            return Response::err(req.id, ec::BUSY, PREVIEW_HOLDS_KEYBOARD);
        }
        let Some(p) = Self::ctl_pane_ref(ws, &self.windows, pane) else {
            return Response::err(req.id, ec::NO_SUCH_PANE, "pane vanished");
        };
        let mut bytes = Vec::new();
        let mut encoded_keys = Vec::with_capacity(parsed.len());
        for (token, mods, key) in &parsed {
            // The live mode decides the byte form (app-cursor arrows etc.).
            // Only modified Enter needs the termios/OSC 133 context sample.
            let mode = p.effective_key_mode(
                self.cfg.modify_other_keys,
                key_is_modified_enter(key, *mods),
            );
            if let Some(b) = crate::input::encode_key_press(key, *mods, mode) {
                // Honor the user's backspace-binding / delete-binding remap,
                // exactly like the GUI key path.
                let b = if crate::input::key_press_uses_kitty_sequence(key, *mods, mode) {
                    b
                } else {
                    apply_bs_del_binding(&self.cfg, key, b)
                };
                if let Err(message) = extend_ctl_send_key_bytes(&mut bytes, &b) {
                    return Response::err(req.id, ec::BAD_PARAMS, message);
                }
                encoded_keys.push(b);
            } else {
                let message = if mode.intersects(kettle_core::TermMode::KITTY_KEYBOARD_PROTOCOL) {
                    format!(
                        "'{token}' has no encoding in this pane's negotiated Kitty keyboard mode"
                    )
                } else {
                    format!(
                        "'{token}' has no legacy encoding; this pane has not negotiated the Kitty keyboard protocol"
                    )
                };
                return Response::err(req.id, ec::BAD_PARAMS, message);
            }
        }
        // The per-pane read-only toggle blocks agents
        // like any other input, with an explicit error.
        let result = p.feed_key_inputs(&encoded_keys);
        if let Some((code, message)) = ctl_input_error(result) {
            return Response::err(req.id, code, message);
        }
        if !encoded_keys.is_empty() {
            self.dismiss_media_paste_receipt_for_pane(ws, pane);
        }
        log::info!(
            "agent-server: send_keys conn={conn_id} pane={pane} ({} keys, {} bytes)",
            parsed.len(),
            bytes.len()
        );
        self.ctl_attach(ws, conn_id, pane);
        Response::ok(
            req.id,
            serde_json::json!({"pane": pane, "keys": parsed.len(), "bytes": bytes.len()}),
        )
    }

    /// Resolve the `pane` param to a pane id; default to the focused pane.
    /// `Err` with a message when neither resolves.
    pub(super) fn ctl_resolve_pane(
        &self,
        ws: &WindowState,
        params: &serde_json::Value,
    ) -> Result<u64, (&'static str, String)> {
        use kettle_ctl::protocol::error_codes as ec;

        let pane =
            optional_u64_param(params, "pane").map_err(|message| (ec::BAD_PARAMS, message))?;
        if let Some(p) = pane {
            // An explicit pane id may live in ANY window (pane ids are
            // process-global).
            if ws.mux.panes.contains_key(&p)
                || self.windows.values().any(|w| w.mux.panes.contains_key(&p))
            {
                return Ok(p);
            }
            return Err((ec::NO_SUCH_PANE, format!("no pane with id {p}")));
        }
        ws.mux
            .tabs
            .get(ws.mux.active)
            .map(|t| t.focus)
            .filter(|f| ws.mux.panes.contains_key(f))
            .ok_or_else(|| (ec::NO_SUCH_PANE, "no focused pane".to_string()))
    }

    /// Validate the complete window/tab/pane inventory before any control
    /// method allocates a JSON collection or walks pane layout trees.
    pub(super) fn ctl_window_inventory_size(&self, ws: &WindowState) -> Option<usize> {
        ctl_bounded_sum(
            self.all_windows(ws)
                .flat_map(|window| [1, window.mux.tabs.len(), window.mux.panes.len()]),
            MAX_CTL_COLLECTION_ITEMS,
        )
    }

    /// Every window, the checked-out one plus the map, ordered by seq, for the
    /// ctl read paths. It iterates without allocating, so the control-plane cap
    /// can be checked before anything is allocated.
    pub(super) fn all_windows<'a>(
        &'a self,
        ws: &'a WindowState,
    ) -> impl Iterator<Item = &'a WindowState> + 'a {
        use std::ops::Bound::{Excluded, Unbounded};

        self.windows
            .range(..ws.seq)
            .map(|(_, window)| window)
            .chain(std::iter::once(ws))
            .chain(
                self.windows
                    .range((Excluded(ws.seq), Unbounded))
                    .map(|(_, window)| window),
            )
    }

    /// Borrow a pane from the checked-out window or any other window in the map
    /// (pane ids are process-global).
    pub(super) fn ctl_pane_ref<'a>(
        ws: &'a WindowState,
        windows: &'a std::collections::BTreeMap<u64, WindowState>,
        id: u64,
    ) -> Option<&'a crate::mux::Pane> {
        ws.mux
            .panes
            .get(&id)
            .or_else(|| windows.values().find_map(|w| w.mux.panes.get(&id)))
    }

    /// Mark `conn_id` attached to `pane` + fire the badge transition once.
    pub(super) fn ctl_attach(&mut self, ws: &mut WindowState, conn_id: u64, pane: u64) {
        let newly = self
            .ctl
            .as_mut()
            .map(|c| c.attach_pane(conn_id, pane))
            .unwrap_or(false);
        if newly {
            self.set_pane_agent_attached(ws, pane, true);
        }
    }

    /// Flip a pane's agent badge and emit the `agent_attached` event so
    /// subscribers and the titlebar update.
    pub(super) fn set_pane_agent_attached(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        attached: bool,
    ) {
        // The pane may live in any window.
        let in_ws = ws.mux.panes.contains_key(&pane);
        {
            let p = if in_ws {
                ws.mux.panes.get_mut(&pane)
            } else {
                self.windows
                    .values_mut()
                    .find_map(|w| w.mux.panes.get_mut(&pane))
            };
            let Some(p) = p else { return };
            if p.agent_attached == attached {
                return;
            }
            p.agent_attached = attached;
        }
        self.ctl_broadcast(
            "agent_attached",
            Some(pane),
            serde_json::json!({"attached": attached}),
        );
        // Repaint the owning window so the titlebar badge updates.
        let owner = if in_ws {
            ws.window.as_ref()
        } else {
            self.windows
                .values()
                .find(|w| w.mux.panes.contains_key(&pane))
                .and_then(|w| w.window.as_ref())
        };
        if let Some(w) = owner {
            w.request_redraw();
        }
    }

    /// Broadcast an event to subscribed control connections (no-op when the
    /// server is off or nobody subscribed).
    pub(super) fn ctl_broadcast(&self, kind: &str, pane: Option<u64>, data: serde_json::Value) {
        if let Some(ctl) = &self.ctl
            && ctl.has_subscribers()
        {
            ctl.broadcast(&kettle_ctl::protocol::Event::new(kind, pane, data));
        }
    }

    /// `run_command`: write `cmd\n` to a pane and defer the reply (stored in a
    /// `PendingRun`) until its OSC-133 `CommandFinished` generation, or
    /// the deadline. A bad request replies immediately over `reply`.
    pub(super) fn ctl_run_command(
        &mut self,
        ws: &mut WindowState,
        conn_id: u64,
        req: &kettle_ctl::protocol::Request,
        reply: crate::ctl_server::ReplyTx,
    ) {
        self.sync_pane_focus_reports(ws);
        use kettle_ctl::protocol::{Response, error_codes as ec};
        let pane = match self.ctl_resolve_pane(ws, &req.params) {
            Ok(p) => p,
            Err((code, message)) => {
                let _ = reply.send(Response::err(req.id, code, message));
                return;
            }
        };
        let Some(command) = req.params.get("command").and_then(|v| v.as_str()) else {
            let _ = reply.send(Response::err(
                req.id,
                ec::BAD_PARAMS,
                "missing 'command' string",
            ));
            return;
        };
        if self.pending_runs.contains_key(&pane) {
            let _ = reply.send(Response::err(
                req.id,
                ec::BUSY,
                "a run_command is already pending on this pane",
            ));
            return;
        }
        let command_generation = match Self::ctl_pane_ref(ws, &self.windows, pane) {
            Some(p) => match p.term.idle_command_generation() {
                Some(generation) => generation,
                None => {
                    let _ = reply.send(Response::err(
                        req.id,
                        ec::BUSY,
                        self.ui_text.text(kettle_i18n::Text::CtlRunCommandBusy),
                    ));
                    return;
                }
            },
            None => {
                let _ = reply.send(Response::err(req.id, ec::NO_SUCH_PANE, "pane vanished"));
                return;
            }
        };
        let timeout_s = req
            .params
            .get("timeout_s")
            .and_then(|v| v.as_f64())
            .unwrap_or(15.0)
            .clamp(0.1, 600.0);
        // Include evicted rows so the anchor survives a full scrollback ring.
        let start_line = Self::ctl_pane_ref(ws, &self.windows, pane)
            .and_then(|p| {
                let term = p.term.term.lock().ok()?;
                let grid = term.grid();
                Some(
                    grid.history_origin()
                        .saturating_add(grid.history_size() as u64)
                        .saturating_add(grid.cursor.point.line.0.max(0) as u64),
                )
            })
            .unwrap_or(0);
        if let Some(p) = Self::ctl_pane_ref(ws, &self.windows, pane) {
            let line = command_submission(command);
            // An agent acts as the user — the per-pane read-only
            // toggle (Terminator parity) blocks it, with an explicit error
            // (no PendingRun is registered; nothing was written).
            let result = p.feed_input(line.as_bytes());
            if let Some((code, message)) = ctl_input_error(result) {
                let _ = reply.send(Response::err(req.id, code, message));
                return;
            }
            self.dismiss_media_paste_receipt_for_pane(ws, pane);
        }
        log::info!("agent-server: run_command conn={conn_id} pane={pane}: {command:?}");
        self.ctl_attach(ws, conn_id, pane);
        self.pending_runs.insert(
            pane,
            PendingRun {
                conn_id,
                req_id: req.id,
                start_line,
                command_generation,
                deadline: std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout_s),
                reply,
            },
        );
    }

    /// An OSC-133 `CommandFinished` arrived for `pane` — if a
    /// `run_command` is pending there, reply with the exit code, duration, and
    /// the output captured since the command started.
    pub(super) fn resolve_pending_run(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        ev: &kettle_core::CommandFinished,
    ) {
        let Some(run) = take_pending_run(&mut self.pending_runs, pane, ev) else {
            return;
        };
        let (output, output_truncated) = self.ctl_capture_output_since(ws, pane, run.start_line);
        let resp = kettle_ctl::protocol::Response::ok(
            run.req_id,
            serde_json::json!({
                "pane": pane,
                "exit_code": ev.exit_code,
                "duration_ms": ev.duration.as_millis() as u64,
                "timed_out": false,
                "output": output,
                "output_truncated": output_truncated,
            }),
        );
        let _ = run.reply.send(cap_ctl_command_response(resp));
    }

    /// Reply `timed_out` to any pending run whose deadline passed
    /// (no OSC-133 completion — usually the shell lacks shell integration), and
    /// immediately resolve any pending run whose pane has CLOSED, so
    /// the agent isn't blocked for the full timeout after a pane vanishes.
    /// Called each event-loop tick from the redraw scheduler.
    pub(super) fn check_pending_run_deadlines(&mut self, ws: &mut WindowState) {
        if self.pending_runs.is_empty() {
            return;
        }
        // A pending run whose pane no longer exists (closed/reaped) is resolved
        // at once with an error, freeing the blocked connection thread.
        let pending_panes: Vec<u64> = self.pending_runs.keys().copied().collect();
        let orphaned: Vec<u64> = pending_panes
            .into_iter()
            .filter(|pane| Self::ctl_pane_ref(ws, &self.windows, *pane).is_none())
            .collect();
        for pane in orphaned {
            if let Some(run) = self.pending_runs.remove(&pane) {
                let resp = kettle_ctl::protocol::Response::err(
                    run.req_id,
                    kettle_ctl::protocol::error_codes::NO_SUCH_PANE,
                    "pane closed before the command completed",
                );
                let _ = run.reply.send(resp);
            }
        }
        let now = std::time::Instant::now();
        let expired: Vec<u64> = self
            .pending_runs
            .iter()
            .filter(|(_, p)| now >= p.deadline)
            .map(|(&pane, _)| pane)
            .collect();
        for pane in expired {
            let Some(run) = self.pending_runs.remove(&pane) else {
                continue;
            };
            let (output, output_truncated) =
                self.ctl_capture_output_since(ws, pane, run.start_line);
            let resp = kettle_ctl::protocol::Response::ok(
                run.req_id,
                serde_json::json!({
                    "pane": pane,
                    "exit_code": serde_json::Value::Null,
                    "timed_out": true,
                    "output": output,
                    "output_truncated": output_truncated,
                    "hint": "no OSC 133 command-end seen; for exit codes, add the output of \
                             `kettle --shell-integration <shell>` to the shell's startup file \
                             and start a new shell",
                }),
            );
            let _ = run.reply.send(cap_ctl_command_response(resp));
        }
    }

    /// Capture a pane's output produced since absolute line `start_line` (the
    /// lines a single `run_command` added). Best-effort; empty on a vanished
    /// pane.
    ///
    /// Slices the ABSOLUTE line range `[start_line, cursor]` out of the grid —
    /// NOT "the last N grid lines". A fresh shell's content sits at the TOP of
    /// the screen with blank rows below it, so taking the grid tail would return
    /// those trailing blanks; addressing by absolute line lands on the real
    /// content wherever it is. The cursor's absolute line is the end of the
    /// content the command produced.
    pub(super) fn ctl_capture_output_since(
        &self,
        ws: &WindowState,
        pane: u64,
        start_line: u64,
    ) -> (String, bool) {
        // The pane may have moved to another window mid-run.
        let Some(p) = Self::ctl_pane_ref(ws, &self.windows, pane) else {
            return (String::new(), false);
        };
        let Ok(term) = p.term.term.lock() else {
            return (String::new(), false);
        };
        let grid = term.grid();
        let origin = grid.history_origin();
        let hist = grid.history_size();
        let screen_top = origin.saturating_add(hist as u64);
        let cursor_abs = screen_top.saturating_add(grid.cursor.point.line.0.max(0) as u64);
        // Request at least one history row so a scrolled viewport cannot shift
        // the scrape away from the active screen. Zero history clamps to zero.
        let take = screen_top
            .saturating_sub(start_line)
            .min(MAX_CTL_SCROLLBACK_LINES as u64)
            .max(1) as usize;
        use kettle_core::Dimensions as _;
        let take = take.min(hist);
        let (start_idx, end_idx, head_dropped) = ctl_capture_bounds(
            origin,
            hist,
            start_line,
            cursor_abs,
            take + term.screen_lines(),
        );
        if start_idx >= end_idx {
            return (String::new(), head_dropped);
        }
        let first_row = -(take as i32);
        let (output, byte_truncated) = kettle_core::grid_text::rows_text_bounded(
            grid,
            first_row + start_idx as i32,
            first_row + end_idx as i32,
            MAX_CTL_COMMAND_OUTPUT_BYTES,
        );
        (output, head_dropped || byte_truncated)
    }
}

#[cfg(test)]
mod screen_page_tests {
    use super::*;

    #[test]
    fn command_terminators_submit_one_carriage_return() {
        for command in [
            "echo hello",
            "echo hello\r",
            "echo hello\n",
            "echo hello\r\n",
        ] {
            assert_eq!(command_submission(command), "echo hello\r", "{command:?}");
        }
        assert_eq!(command_submission("first\nsecond\n"), "first\nsecond\r");
    }

    fn default_page() -> kettle_ctl::protocol::PageRequest {
        kettle_ctl::protocol::PageRequest::from_params(&serde_json::json!({})).unwrap()
    }

    /// A `wait_for` probe sees the whole snapshot. A client page stops at
    /// `DEFAULT_PAGE_ITEMS` lines or `MAX_CTL_TEXT_PAGE_BYTES`, so text past
    /// either was never matched.
    #[test]
    fn a_wait_for_probe_reads_past_the_first_page() {
        let many_lines = format!(
            "{}needle\n",
            "x\n".repeat(kettle_ctl::protocol::DEFAULT_PAGE_ITEMS)
        );
        let many_bytes = format!("{}\nneedle\n", "y".repeat(MAX_CTL_TEXT_PAGE_BYTES));
        for screen in [many_lines, many_bytes] {
            let client = screen_page(&screen, &default_page(), false);
            assert!(!client.text.contains("needle"));
            assert!(client.end < client.total);

            let probe = screen_page(&screen, &default_page(), true);
            assert_eq!(probe.text, screen);
            assert_eq!(probe.end, probe.total);
        }
        // And `handle_ctl_request` passes the probe flag through. Split, so
        // this line does not match itself.
        let source = include_str!("ctl.rs");
        assert!(source.contains(concat!(
            "self.ctl_read_screen(ws, req, ",
            "request.internal_probe())"
        )));
    }
}
