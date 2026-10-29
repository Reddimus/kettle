use super::*;

pub(super) fn clear_completions_on_focus_loss<T>(
    focused: bool,
    panes: impl IntoIterator<Item = T>,
    mut clear: impl FnMut(T),
) {
    if !focused {
        for pane in panes {
            clear(pane);
        }
    }
}

/// `(active-tab-index, focused-leaf-id)` — the value `App::focus_key` returns.
pub(crate) type FocusKey = (usize, Option<u64>);

/// Which pane loses and which gains the focus report when the pane holding
/// the keyboard changes from `reported` to `focused` (`None`: no pane, as
/// while the window lacks OS focus). No change, no reports.
pub(super) fn focus_report_transition(
    reported: Option<u64>,
    focused: Option<u64>,
) -> (Option<u64>, Option<u64>) {
    if reported == focused {
        (None, None)
    } else {
        (reported, focused)
    }
}

impl App {
    /// Keep focus-event reports (DEC mode 1004) in step with the pane that
    /// holds the keyboard: the focused pane of the active tab while the window
    /// has OS focus. A pane that enabled the mode hears `CSI O` when that
    /// stops being it, whether the window lost focus or focus moved to
    /// another pane or tab (a key, a click, a control request or a close),
    /// and `CSI I` when it becomes it again. tmux `focus-events`, Neovim's
    /// `FocusGained`/`FocusLost` and the agent CLIs' notification state all
    /// rely on that. Every dispatch passes through `finish_window_dispatch`,
    /// so one check there covers every route, and a pane hears each change
    /// once.
    pub(super) fn sync_pane_focus_reports(&mut self, ws: &mut WindowState) {
        let focused = (ws.os_focus_known && ws.window_focused)
            .then(|| ws.mux.active_focus())
            .flatten();
        let (lost, gained) = focus_report_transition(ws.focus_observed_pane, focused);
        ws.focus_observed_pane = focused;
        for (pane_id, focus) in [(lost, false), (gained, true)] {
            let Some(pane_id) = pane_id else {
                continue;
            };
            if !self
                .pane_mode(ws, pane_id)
                .is_some_and(|mode| mode.contains(kettle_core::TermMode::FOCUS_IN_OUT))
            {
                continue;
            }
            if let Some(pane) = ws.mux.panes.get(&pane_id) {
                let result = pane.feed_focus_report(focus);
                if !result.is_queued() {
                    self.report_input_result(result);
                }
            }
        }
    }

    /// A tab leaving this window takes its panes with it. If one of them holds
    /// the reported focus, tell it focus left now, through the detached tab,
    /// so the window that adopts it starts from "not focused" and its first
    /// `CSI I` follows a `CSI O`. Called only after a detach succeeded, so a
    /// failed transfer reports nothing.
    pub(super) fn release_focus_report_to(
        &mut self,
        ws: &mut WindowState,
        dt: &crate::mux::DetachedTab,
    ) {
        let Some(reported) = ws.focus_observed_pane else {
            return;
        };
        let Some((_, pane)) = dt.panes.iter().find(|(id, _)| *id == reported) else {
            return;
        };
        ws.focus_observed_pane = None;
        let reporting = pane
            .term
            .term
            .lock()
            .ok()
            .is_some_and(|t| t.mode().contains(kettle_core::TermMode::FOCUS_IN_OUT));
        if reporting {
            let result = pane.feed_focus_report(false);
            if !result.is_queued() {
                self.report_input_result(result);
            }
        }
    }

    /// Snapshot the focused `(tab, leaf)` pair. Paired with
    /// `note_focus_change` to detect whether an operation moved focus
    /// and, if so, reset the cursor blink phase so the new pane's
    /// cursor is visible immediately. Keyboard and mouse paths share it.
    pub(super) fn focus_key(&self, ws: &WindowState) -> (usize, Option<u64>) {
        (ws.mux.active, ws.mux.active_focus())
    }

    /// If the focused `(tab, leaf)` changed since `pre`, land the
    /// cursor visible on the new pane right away.
    pub(super) fn note_focus_change(&mut self, ws: &mut WindowState, pre: (usize, Option<u64>)) {
        if self.focus_key(ws) != pre {
            end_modal_composition(ws);
            // Focus moved to a pane, so not to a card any more.
            if ws.card_focus.take().is_some() {
                ws.accessibility_pending = true;
            }
            // Vi mode belongs to the pane that owns alacritty_terminal's
            // engine cursor. A focus change must not leave that old pane in
            // TermMode::VI while the UI routes keys to a different pane.
            self.exit_vi_mode(ws);
            self.reset_blink_phase(ws);
            // An open search bar follows focus (see `retarget_search_to_focus`).
            self.retarget_search_to_focus(ws);
            // Repaint immediately so the focused-pane border and the cursor's
            // solid/hollow state track the new pane. Otherwise a
            // focus-follows-mouse (`focus = sloppy`) change leaves the pane
            // under the cursor looking unfocused until some other event
            // triggers a redraw.
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
    }

    /// Terminator plugin parity (`LuaEvent::PaneFocus`).
    /// Detect focus boundary crossings —
    /// from any source (keybind, mouse click, new tab, close tab,
    /// remote-control IPC) — and emit a single `PaneFocus` event
    /// per crossing.
    ///
    /// Polled from `redraw()` per tick rather than wiring every
    /// focus-changing call site because (a) there are 6+
    /// such sites today and (b) future changes will add more.
    /// One diff site = one drift guard, not N.
    ///
    /// First tick after startup emits with `previous = None` so
    /// plugins can seed their state.
    pub(super) fn poll_focus_event(&mut self, ws: &mut WindowState) {
        let current = ws.mux.active_focus();
        let Some(cur_id) = current else { return };
        if ws.last_emitted_focus == Some(cur_id) {
            return;
        }
        let prev = ws.last_emitted_focus;
        ws.last_emitted_focus = Some(cur_id);
        self.fire_lua_event(
            ws,
            crate::LuaEvent::PaneFocus(prev, cur_id),
            "pane_focus hook",
        );
        // Agent-first A2: mirror the focus change to ctl subscribers.
        self.ctl_broadcast(
            "pane_focus",
            Some(cur_id),
            serde_json::json!({"previous": prev}),
        );
    }
}
