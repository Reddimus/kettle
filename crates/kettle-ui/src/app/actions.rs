use super::*;

/// The group name Terminator's `group_all` uses (`window.py:936`, `_('All')`).
///
/// A named constant so the importer, the dispatch, and any future "is
/// everything grouped?" check cannot disagree about the spelling — the name is
/// user-visible in the titlebar pill.
fn kettle_ui_group_all_name() -> &'static str {
    "All"
}

pub(super) fn group_scope_is_globally_stale(
    autoclean: bool,
    scope: &BroadcastScope,
    populated_groups: &HashSet<&str>,
) -> bool {
    autoclean
        && matches!(scope, BroadcastScope::Group(name) if !populated_groups.contains(name.as_str()))
}

fn any_group_scope_needs_hoover<'a>(windows: impl IntoIterator<Item = &'a WindowState>) -> bool {
    windows.into_iter().any(|window| {
        window.mux.autoclean_groups && matches!(window.mux.broadcast, BroadcastScope::Group(_))
    })
}

/// Group assignment scope for the title-edit overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupBulkScope {
    /// Default: just the pane focused when the overlay opens (existing
    /// `EditPaneGroup` / `CreateGroup` behavior).
    #[default]
    Single,
    /// Every pane in the tab active when the overlay opens gets the typed name.
    Tab,
    /// Every pane in every tab.
    Window,
}

/// Which broadcast scope the group chord turns on, from `broadcast-default`.
///
/// Terminator stores this key as the *initial* groupsend mode
/// (`terminator.py`: `groupsend = groupsend_type[config['broadcast_default']]`).
/// kettle reads it as the scope the chord selects instead, because a window
/// that started in `all` would mirror every keystroke into every pane before
/// the user had touched anything.
///
/// For Terminator's own default the two readings are the same thing — its
/// `group` mode with no groups yet assigned sends only to the focused terminal
/// — so the divergence is visible only under `broadcast-default = all`, where
/// kettle waits to be asked. `group` maps to kettle's per-tab broadcast, which
/// is what a kettle "group" has always meant to the chord.
pub(super) fn broadcast_scope_for_default(
    default: kettle_config::BroadcastDefault,
) -> crate::mux::BroadcastScope {
    match default {
        kettle_config::BroadcastDefault::All => crate::mux::BroadcastScope::All,
        kettle_config::BroadcastDefault::Group => crate::mux::BroadcastScope::Tab,
        kettle_config::BroadcastDefault::Off => crate::mux::BroadcastScope::Off,
    }
}

/// A split that cannot spawn leaves the layout exactly as it was, so all the
/// user sees is a keystroke that did nothing. That is indistinguishable from a
/// split that appeared and never drew, and a `warn!` line is invisible at the
/// default log level, so the two reports arrive looking identical. Say it out
/// loud instead, the way a failed preference write already does.
pub(crate) fn report_split_failure(
    direction: &str,
    error: &dyn std::fmt::Display,
    tr: &kettle_i18n::Translator,
) {
    log::error!("could not split pane ({direction}): {error}");
    fire_notify(
        tr.text(kettle_i18n::Text::NotifyTitleSplitFailed),
        &error.to_string(),
    );
}

impl App {
    fn restore_scaled_zoom_font(&mut self, ws: &mut WindowState) {
        if let Some(prev) = ws.scaled_zoom_prev_font_size.take()
            && let Some(r) = ws.renderer.as_mut()
        {
            r.set_font_size(prev);
        }
    }

    /// Shared zoom transition for `Action::ToggleZoom` and
    /// `Action::ScaledZoom`. Both actions flip the SAME `ws.mux` zoom flag, so
    /// they must share `ws.scaled_zoom_prev_font_size`. Otherwise a plain
    /// `ToggleZoom` could un-zoom a pane that `ScaledZoom` had enlarged without
    /// restoring the font or clearing the stale baseline, leaving the pane
    /// un-zoomed but oversized and corrupting any later `ScaledZoom` call's
    /// save/restore pairing.
    ///
    /// `scale` is `Some(factor)` for `ScaledZoom`, which bumps the live font by
    /// `factor` on zoom-in and saves the pre-bump size the first time. It is
    /// `None` for plain `ToggleZoom`, which never bumps the font on zoom-in.
    /// Either action, on zoom-*out*, restores the font from
    /// `scaled_zoom_prev_font_size` and clears it if a scaled-zoom baseline is
    /// pending, so the pairing never desyncs whichever action toggled the flag.
    fn toggle_zoom_with_scale(&mut self, ws: &mut WindowState, scale: Option<f32>) {
        ws.mux.toggle_zoom();
        let now_zoomed = ws.mux.is_zoomed();
        if let Some(r) = ws.renderer.as_mut() {
            if now_zoomed {
                if let Some(factor) = scale {
                    // Baseline off the LIVE renderer size, not
                    // `self.cfg.font_size`. Increase/DecreaseFontSize only
                    // call `r.set_font_size` (they never write
                    // `cfg.font_size`), so a prior manual zoom left the
                    // config value stale — scaling from it would otherwise
                    // discard the user's manual zoom on restore.
                    let cur = r.font_size();
                    if ws.scaled_zoom_prev_font_size.is_none() {
                        ws.scaled_zoom_prev_font_size = Some(cur);
                    }
                    let new_size = (cur * factor).clamp(6.0, 96.0);
                    r.set_font_size(new_size);
                }
            } else if let Some(prev) = ws.scaled_zoom_prev_font_size.take() {
                r.set_font_size(prev);
            }
        }
        // Grid and pixel geometry must be derived after any scaled-zoom font
        // change. Resizing first would leave the PTY at the previous cell
        // metrics until an unrelated resize event, so TUIs and CSI 14 t would
        // disagree with the pixels actually rendered.
        self.resize_all(ws);
    }

    /// Open a new tab running `argv`, inheriting the focused tab's
    /// current working directory. Shared by the new-tab `▾` dropdown's mouse +
    /// keyboard dispatch. Mirrors the `Action::NewTab` handler's pattern: log
    /// on failure, fire the `TabAdd` plugin event only when a tab was
    /// actually created.
    pub(super) fn open_tab_with_argv(&mut self, ws: &mut WindowState, argv: &[String]) {
        let area = self.area(ws);
        let (cols, rows) = self.grid_of(ws, area);
        let geometry = self.pty_geometry_for_grid(ws, cols, rows);
        let waker = self.waker();
        let cwd = ws
            .mux
            .focused()
            .and_then(|p| p.term.current_dir_or_native());
        // Route through new_tab_with_launch so a WSL ▾-dropdown
        // entry's Linux cwd is carried via `wsl --cd` instead of being dropped
        // (a Windows spawn can't `cd` into a Linux path, so it fell back home).
        match ws
            .mux
            .new_tab_with_launch_geometry(&self.cfg, geometry, waker, argv.to_vec(), cwd)
        {
            Ok(()) => self.fire_tab_add_event(ws),
            Err(e) => log::warn!("could not open shell tab ({argv:?}): {e}"),
        }
    }

    /// Send a broadcast from `ws`, and then to the panes in every OTHER window
    /// that the same scope selects.
    ///
    /// A named broadcast group is a set the user declared, and `group_all`
    /// already spans every window the way Terminator's does. Stopping at the
    /// focused window would reach only part of a cross-window group while the
    /// other window's panes still show the group in their titlebars.
    ///
    /// Scopes that are defined by something window-local (`Tab`, and kettle's
    /// own window-wide `All`) do not walk, so this costs a single enum test
    /// when it does not apply.
    pub(super) fn broadcast_input(
        &mut self,
        ws: &mut WindowState,
        bytes: &[u8],
        scroll_to_bottom: bool,
        input_time: std::time::Instant,
    ) -> PaneInputResult {
        let receipt_pane = Self::media_paste_receipt_owners(ws);
        let local = ws
            .mux
            .broadcast_write_delivery(bytes, scroll_to_bottom, receipt_pane);
        Self::dismiss_media_paste_receipt_after_delivery(ws, local);
        stamp_accepted_input(&mut ws.last_typed, local, input_time);
        let mut result = local.result;
        if crate::mux::Mux::scope_crosses_windows(&ws.mux.broadcast) {
            let scope = ws.mux.broadcast.clone();
            for other in self.windows.values_mut() {
                let receipt_pane = Self::media_paste_receipt_owners(other);
                let delivery = other.mux.broadcast_write_foreign_delivery(
                    &scope,
                    bytes,
                    scroll_to_bottom,
                    receipt_pane,
                );
                if scroll_to_bottom
                    && delivery.accepted
                    && let Some(window) = &other.window
                {
                    window.request_redraw();
                }
                Self::dismiss_media_paste_receipt_after_delivery(other, delivery);
                stamp_accepted_input(&mut other.last_typed, delivery, input_time);
                result = result.merge(delivery.result);
            }
        }
        result
    }

    fn populated_groups<'a>(&'a self, ws: Option<&'a WindowState>) -> HashSet<&'a str> {
        ws.into_iter()
            .chain(self.windows.values())
            .flat_map(|window| window.mux.panes.values())
            .filter_map(|pane| pane.group_name.as_deref())
            .collect()
    }

    pub(super) fn hoover_groups(&mut self, ws: &mut WindowState) {
        if !any_group_scope_needs_hoover(std::iter::once(&*ws).chain(self.windows.values())) {
            return;
        }
        let populated = self.populated_groups(Some(ws));
        let reset_current =
            group_scope_is_globally_stale(ws.mux.autoclean_groups, &ws.mux.broadcast, &populated);
        let reset_windows: Vec<u64> = self
            .windows
            .iter()
            .filter_map(|(&seq, target)| {
                group_scope_is_globally_stale(
                    target.mux.autoclean_groups,
                    &target.mux.broadcast,
                    &populated,
                )
                .then_some(seq)
            })
            .collect();
        drop(populated);
        if reset_current {
            ws.mux.broadcast = BroadcastScope::Off;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        for seq in reset_windows {
            if let Some(target) = self.windows.get_mut(&seq) {
                target.mux.broadcast = BroadcastScope::Off;
                if let Some(window) = &target.window {
                    window.request_redraw();
                }
            }
        }
    }

    pub(super) fn hoover_mapped_groups(&mut self) {
        if !any_group_scope_needs_hoover(self.windows.values()) {
            return;
        }
        let populated = self.populated_groups(None);
        let reset_windows: Vec<u64> = self
            .windows
            .iter()
            .filter_map(|(&seq, target)| {
                group_scope_is_globally_stale(
                    target.mux.autoclean_groups,
                    &target.mux.broadcast,
                    &populated,
                )
                .then_some(seq)
            })
            .collect();
        drop(populated);
        for seq in reset_windows {
            if let Some(target) = self.windows.get_mut(&seq) {
                target.mux.broadcast = BroadcastScope::Off;
                if let Some(window) = &target.window {
                    window.request_redraw();
                }
            }
        }
    }

    pub(super) fn handle_action(
        &mut self,
        ws: &mut WindowState,
        action: Action,
        event_loop: &ActiveEventLoop,
    ) {
        self.sync_pane_focus_reports(ws);
        let area = self.area(ws);
        let (cols, rows) = self.grid_of(ws, area);
        let tab_geometry = self.pty_geometry_for_grid(ws, cols, rows);
        let horizontal_split_geometry = self.prospective_split_geometry(ws, Dir::Horizontal, false);
        let vertical_split_geometry = self.prospective_split_geometry(ws, Dir::Vertical, false);
        let waker = self.waker();
        // Snapshot the (tab, pane-leaf) the cursor lives in so we can detect
        // any focus change the action causes.
        let pre_focus = self.focus_key(ws);
        let save_session = !matches!(
            action,
            Action::ScrollPageUp
                | Action::ScrollPageDown
                | Action::ScrollLineUp
                | Action::ScrollLineDown
                | Action::ScrollToTop
                | Action::ScrollToBottom
        );
        match action {
            Action::NewTab => {
                // Fire LuaEvent::TabAdd with the new active tab index (via
                // fire_tab_add_event) only when a tab was actually created, so
                // Lua never hears of a tab that doesn't exist. A PTY-spawn
                // failure is logged.
                match ws.mux.new_tab_geometry(&self.cfg, tab_geometry, waker) {
                    Ok(()) => self.fire_tab_add_event(ws),
                    Err(e) => log::warn!("could not open a new tab (shell spawn failed?): {e}"),
                }
            }
            Action::NewWindow => {
                // Open a real second window in this process, so tabs can move
                // live between windows and all windows share the GPU device. If
                // the window can't be created, fall back to a new tab (degraded
                // but useful).
                if let Err(_unopened) =
                    self.open_window(event_loop, WindowOpen::Fresh { cwd: None }, None, None)
                {
                    match ws.mux.new_tab_geometry(&self.cfg, tab_geometry, waker) {
                        Ok(()) => self.fire_tab_add_event(ws),
                        Err(e) => {
                            log::warn!("new-window fallback: could not open a tab: {e}")
                        }
                    }
                }
            }
            Action::SplitRight | Action::SplitLeft => {
                // Split Left puts the new pane first, as Ghostty's
                // `new_split:left` does.
                let new_first = action == Action::SplitLeft;
                let geometry = if new_first {
                    self.prospective_split_geometry(ws, Dir::Horizontal, true)
                } else {
                    horizontal_split_geometry
                };
                // If the focused pane has entered a shell it launched
                // (e.g. typed `wsl` in pwsh), clone THAT shell + its dir; else
                // clone the pane's own launch command.
                let detected = self.focused_foreground_shell(ws);
                let res = match detected {
                    Some(s) => ws.mux.split_with_geometry(
                        Dir::Horizontal,
                        new_first,
                        &self.cfg,
                        geometry,
                        waker,
                        s.argv,
                        s.cwd,
                    ),
                    None => ws.mux.split_geometry(
                        Dir::Horizontal,
                        new_first,
                        &self.cfg,
                        geometry,
                        waker,
                    ),
                };
                if let Err(e) = res {
                    report_split_failure(
                        if new_first { "left" } else { "right" },
                        &e,
                        &self.ui_text,
                    );
                } else {
                    self.restore_scaled_zoom_font(ws);
                }
            }
            Action::SplitDown | Action::SplitUp | Action::SplitAuto => {
                // `split_auto` cuts along the pane's LONGER axis, the way
                // Terminator does; `split_down` and `split_up` are always
                // vertical, and Split Up puts the new pane first.
                let dir = if action == Action::SplitAuto {
                    self.auto_split_dir(ws)
                } else {
                    Dir::Vertical
                };
                let new_first = action == Action::SplitUp;
                let geometry = if new_first {
                    self.prospective_split_geometry(ws, Dir::Vertical, true)
                } else if dir == Dir::Horizontal {
                    horizontal_split_geometry
                } else {
                    vertical_split_geometry
                };
                let detected = self.focused_foreground_shell(ws);
                let res = match detected {
                    Some(s) => ws.mux.split_with_geometry(
                        dir, new_first, &self.cfg, geometry, waker, s.argv, s.cwd,
                    ),
                    None => ws
                        .mux
                        .split_geometry(dir, new_first, &self.cfg, geometry, waker),
                };
                if let Err(e) = res {
                    report_split_failure(if new_first { "up" } else { "down" }, &e, &self.ui_text);
                } else {
                    self.restore_scaled_zoom_font(ws);
                }
            }
            Action::ClosePane => {
                // Prompt per pane when ask_before_closing = Always.
                // MultipleTerminals doesn't prompt (single pane); see
                // `should_prompt` for the matrix. Ghostty
                // `confirm-close-surface` parity: a pane idle at an
                // integrated-shell prompt has no work to lose, so skip the
                // confirm. Plain shells (no OSC 133 marks) never report idle.
                let busy = usize::from(
                    ws.mux
                        .focused()
                        .map(|p| !p.term.shell_idle())
                        .unwrap_or(true),
                );
                // Name the pane now — by the time the prompt is answered,
                // "focused" may mean a different one.
                if let Some(target) = ws.mux.active_focus()
                    && self.confirm_close(ws, 1, busy, ConfirmAction::ClosePane(target))
                {
                    return;
                }
                // Capture the focused pane id BEFORE the close —
                // afterward active_focus() returns the promoted sibling.
                let closing_pane = ws.mux.active_focus();
                if let Some(id) = closing_pane {
                    self.fire_pane_close_event(ws, id);
                }
                let was_last = ws.mux.close_focused();
                if was_last {
                    self.request_window_close(ws.seq);
                } else {
                    // Redraw and re-emit the focus event now. The split tree
                    // has collapsed (sibling promoted to root), but the
                    // renderer cache and the PaneFocus event's last-fired pane
                    // id stay stale until something schedules a frame. On
                    // Windows under wgpu DX12 this stale state was reported as
                    // a crash, most likely the tab-bar render path indexing a
                    // removed pane and panicking on its dropped
                    // Arc<Mutex<Terminal>> lock.
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    // Fire PaneFocus for the promoted sibling so plugins that
                    // observe focus drop stale per-pane state.
                    self.poll_focus_event(ws);
                }
            }
            Action::CloseTab => {
                // Close the
                // active tab via the modal when ask_before_closing
                // says so. scope_count = leaves in the active tab
                // (panes_in_tab below).
                let (panes_in_tab, busy_in_tab) = tab_close_scope(ws, ws.mux.active);
                if self.confirm_close(
                    ws,
                    panes_in_tab,
                    busy_in_tab,
                    ConfirmAction::CloseTabHolding(ws.mux.tab_anchor_panes(ws.mux.active)),
                ) {
                    return;
                }
                // Capture the active index BEFORE close
                // so the LuaEvent::TabClose payload is meaningful
                // (after close, ws.mux.active points at a
                // different tab), via the shared fire_tab_close_event
                // helper.
                let closing_idx = ws.mux.active;
                if ws.mux.close_tab() {
                    self.request_window_close(ws.seq);
                }
                self.fire_tab_close_event(ws, closing_idx);
            }
            Action::CloseWindow => {
                // Phase 5 of the confirm-dialog design:
                // intercept via the `should_prompt` helper.
                // When ask-before-closing fires, open the modal
                // with on_confirm=CloseWindow; the modal's Confirm
                // dispatch (in the key handler) re-runs the close
                // path below.
                let (scope, busy) = window_close_scope(ws);
                if self.confirm_close(
                    ws,
                    scope,
                    busy,
                    ConfirmAction::CloseWindow {
                        drop_panes: DropPanes::Yes,
                    },
                ) {
                    return;
                }
                // Unlike `CloseTab`, drop *every* tab and pane in this window,
                // not just the focused tab.
                self.close_window_now(ws, DropPanes::Yes);
            }
            Action::NextTab => ws.mux.next_tab(),
            Action::PrevTab => ws.mux.prev_tab(),
            Action::FocusNext => ws.mux.focus_cycle(area, true),
            Action::FocusPrev => ws.mux.focus_cycle(area, false),
            Action::FocusLeft => ws.mux.focus_dir(area, -1, 0),
            Action::FocusRight => ws.mux.focus_dir(area, 1, 0),
            // Terminator rearranges panes by dragging a terminal onto another.
            // This is the same rearrangement addressed by direction: take the
            // focused pane and put it beside whichever pane focus would have
            // moved to, on that side. The neighbour search is the one
            // `goto_split` uses, so the pane lands where you were looking.
            Action::MovePaneLeft
            | Action::MovePaneRight
            | Action::MovePaneUp
            | Action::MovePaneDown => {
                let (dx, dy, dir, before) = match action {
                    Action::MovePaneLeft => (-1, 0, Dir::Horizontal, true),
                    Action::MovePaneRight => (1, 0, Dir::Horizontal, false),
                    Action::MovePaneUp => (0, -1, Dir::Vertical, true),
                    _ => (0, 1, Dir::Vertical, false),
                };
                if let Some(target) = ws.mux.pane_in_direction(area, dx, dy)
                    && let Some(moving) = ws.mux.active_focus()
                    && ws.mux.move_pane_beside(moving, target, dir, before)
                {
                    // The rearrangement changes every pane's rect, so the PTYs
                    // have to hear about it, and the layout is worth keeping.
                    self.resize_all(ws);
                    self.save_session(ws);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
            Action::FocusUp => ws.mux.focus_dir(area, 0, -1),
            Action::FocusDown => ws.mux.focus_dir(area, 0, 1),
            Action::ResizeLeft => ws.mux.resize_focus(Dir::Horizontal, -0.03),
            Action::ResizeRight => ws.mux.resize_focus(Dir::Horizontal, 0.03),
            Action::ResizeUp => ws.mux.resize_focus(Dir::Vertical, -0.03),
            Action::ResizeDown => ws.mux.resize_focus(Dir::Vertical, 0.03),
            Action::Copy => {
                // Terminator parity (terminatorlib/config.py
                // `smart_copy` + terminal.py:real_copy_clipboard):
                //   smart_copy = true  (default) → if no selection, skip
                //     the clipboard write so the existing clipboard
                //     content is preserved. Lets a user Ctrl+Shift+C
                //     without losing a previous copy when they forgot
                //     to highlight first.
                //   smart_copy = false → always trigger the copy. If
                //     there's no selection, write empty — this CLOBBERS
                //     the clipboard. Deliberate UX choice for users who
                //     want Ctrl+Shift+C to consistently mean "the
                //     clipboard now reflects the current selection
                //     (empty or not)" without smart heuristics.
                let selection_text = ws.mux.focused().and_then(|p| {
                    p.term
                        .term
                        .lock()
                        .ok()
                        .and_then(|t| t.selection_to_string())
                });
                let payload = match copy_clipboard_decision(selection_text, self.cfg.smart_copy) {
                    Ok(payload) => payload,
                    Err(error) => {
                        log::warn!("copy: could not prepare selection text: {error}");
                        return;
                    }
                };
                let mut copied = false;
                if let Some(s) = payload
                    && let Some(cb) = &mut self.clipboard
                {
                    let had_selection = !s.is_empty();
                    match cb.set_text(s) {
                        Ok(()) => copied = had_selection,
                        Err(error) => log::warn!("clipboard set_text failed (copy): {error}"),
                    }
                }
                // Terminator parity (terminatorlib/config.py:91
                // `clear_select_on_copy`): if the config asked, drop the
                // selection so the user sees the copy "took". Default
                // false matches Terminator's default — the selection
                // stays so re-Copy still works.
                if copied && self.cfg.clear_select_on_copy {
                    self.clear_selection_on_input(ws);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
            Action::Paste => self.paste_clipboard(ws),
            // The font size is window-wide, so zooming all panes is the same
            // as zooming one.
            Action::IncreaseFontSize
            | Action::DecreaseFontSize
            | Action::ResetFontSize
            | Action::ZoomInAll
            | Action::ZoomOutAll
            | Action::ZoomNormalAll => {
                if let Some(r) = ws.renderer.as_mut() {
                    // Step the logical font size directly. Back-deriving it
                    // from `r.cell_h` (physical px) would double-apply the
                    // scale factor on HiDPI.
                    let new = match action {
                        Action::IncreaseFontSize | Action::ZoomInAll => r.font_size() + 1.0,
                        // `set_font_size` clamps to the renderer's 5..=72
                        // range; a floor of 6 here turned "decrease" at a
                        // configured 5 into an increase.
                        Action::DecreaseFontSize | Action::ZoomOutAll => r.font_size() - 1.0,
                        _ => self.cfg.font_size,
                    };
                    r.set_font_size(new);
                }
                // The user changed the font OUTSIDE a scaled-zoom
                // enter/exit, so the saved pre-zoom size is now stale — drop it
                // so a later zoom-out keeps this new chosen size instead of
                // reverting to the old one.
                ws.scaled_zoom_prev_font_size = None;
            }
            Action::StartSearch => {
                self.open_search(ws);
            }
            Action::ToggleBroadcastAll => {
                // Toggle: turn on the `broadcast-default` scope, or turn
                // broadcast off when that scope is already on. The explicit off
                // chord (`ToggleBroadcastOff`) still works.
                let on = broadcast_scope_for_default(self.cfg.broadcast_default);
                ws.mux.broadcast = if ws.mux.broadcast == on {
                    crate::mux::BroadcastScope::Off
                } else {
                    on
                };
            }
            Action::ToggleBroadcastOff => {
                ws.mux.broadcast = crate::mux::BroadcastScope::Off;
            }
            Action::ToggleBroadcastGroup => {
                // Toggle broadcast scope between Off and
                // Group(focused_pane.group_name). If focused
                // pane has no group, log + no-op.
                let focused_group = ws.mux.focused().and_then(|p| p.group_name.clone());
                let Some(group) = focused_group else {
                    log::info!(
                        "toggle-broadcast-group: focused pane has no group_name; \
                         use Action::CreateGroup or Action::GroupTab first"
                    );
                    return;
                };
                ws.mux.broadcast = match &ws.mux.broadcast {
                    crate::mux::BroadcastScope::Group(name) if name == &group => {
                        crate::mux::BroadcastScope::Off
                    }
                    _ => crate::mux::BroadcastScope::Group(group),
                };
            }
            Action::ToggleBroadcastWindow => {
                // Toggle window-wide broadcast on/off. Unlike
                // ToggleBroadcastAll, whose scope follows `broadcast-default`
                // (per-tab by default), this always targets every pane in every
                // tab.
                ws.mux.broadcast = match &ws.mux.broadcast {
                    crate::mux::BroadcastScope::All => crate::mux::BroadcastScope::Off,
                    _ => crate::mux::BroadcastScope::All,
                };
            }
            Action::ToggleZoom => {
                // A tab with one pane has nothing to expand: entering zoom
                // there would only leave a hidden flag that the first split
                // then silently cleared. A zoomed tab can always leave zoom.
                // ScaledZoom keeps its font step on a single pane.
                if ws.mux.is_zoomed() || ws.mux.active_pane_count() > 1 {
                    // Route through the helper ScaledZoom uses, so un-zooming
                    // via the plain toggle still restores or clears a pending
                    // scaled-zoom font baseline. See `toggle_zoom_with_scale`.
                    self.toggle_zoom_with_scale(ws, None);
                }
            }
            // Ghostty `equalize_splits` parity: rebalance the active tab's
            // split tree to equal pane areas, then push the new geometry into
            // the PTYs.
            Action::EqualizeSplits => {
                if let Some(t) = ws.mux.tabs.get_mut(ws.mux.active) {
                    t.root.equalize();
                }
                self.resize_all(ws);
                self.save_session(ws);
            }
            // Ghostty's `text:` action, and what gives `⌘⌫` something to mean.
            // Unlike the five other text-writing actions this goes through
            // `write_terminal_input` rather than `feed_input`: it stands in for
            // a keystroke, so it should clear the selection, scroll on
            // keystroke and reach every pane of a broadcast, exactly as typing
            // the same bytes would. That helper also invalidates the affected
            // pane's media-paste receipt, which every authored-input route owes.
            Action::SendText(text) => {
                let _ = self.write_terminal_input(ws, text.as_bytes());
            }
            Action::SendNewline => {
                // Terminator parity (`key_send_newline`): write a literal
                // `\n` for shell line-editors that consume Enter normally
                // but expect an explicit `\n` for line continuation
                // (multi-line readline prompts). Typed-input semantics, so
                // read-only drops it.
                self.write_focused_terminal_input(ws, b"\n");
            }
            // Terminator parity (`key_preferences` /
            // `key_preferences_keybindings`). Terminator opens a GUI
            // Preferences dialog; kettle's preferences live in its config file,
            // so the preferences keybind opens that file with the OS's
            // registered handler. If no config file is loaded (kettle started
            // with `--config` pointing at a missing path, or no profile
            // resolved), it falls back to `Config::default_path()`.
            Action::EditConfig => {
                let path = self
                    .config_path
                    .clone()
                    .or_else(kettle_config::Config::default_path);
                if let Some(path) = path {
                    let tr = self.ui_text;
                    system_opener::open(path.clone(), move |e| {
                        log::warn!("Action::EditConfig: failed to open {}: {e}", path.display());
                        fire_notify(
                            tr.text(kettle_i18n::Text::NotifyTitleConfigNotOpened),
                            tr.text(kettle_i18n::Text::NotifyBodyConfigOpenFailed),
                        );
                    });
                } else {
                    log::warn!(
                        "Action::EditConfig: no config path resolved \
                         (set $XDG_CONFIG_HOME or pass --config)"
                    );
                    fire_notify(
                        self.ui_text
                            .text(kettle_i18n::Text::NotifyTitleConfigNotOpened),
                        self.ui_text.text(kettle_i18n::Text::NotifyBodyConfigNoPath),
                    );
                }
            }
            // Preferences submenu toggles. Each dispatch updates `self.cfg` so
            // the change applies now, and writes `key = value` back to the
            // user's config file via the `persist_config_toggle` helper (atomic
            // temp+rename, comment-preserving, with a first-write backup at
            // `<config>.bak`). Re-opening the menu picks up the new state.
            Action::SetScrollbarAlways => {
                self.cfg.scrollbar = kettle_config::ScrollbarMode::Always;
                self.persist_pref("scrollbar", "always");
            }
            Action::SetScrollbarAuto => {
                self.cfg.scrollbar = kettle_config::ScrollbarMode::Auto;
                self.persist_pref("scrollbar", "auto");
            }
            Action::SetScrollbarNever => {
                self.cfg.scrollbar = kettle_config::ScrollbarMode::Never;
                self.persist_pref("scrollbar", "never");
            }
            // Close-confirmation policy. The confirm bar deliberately does NOT
            // carry a "don't ask again" answer: it decides the close in front
            // of you, while standing policy lives here, where the selected
            // radio IS the feedback and picking another row is the undo.
            Action::SetAskBeforeClosingAlways => {
                self.cfg.ask_before_closing = kettle_config::AskBeforeClosing::Always;
                self.persist_pref("ask-before-closing", "always");
            }
            Action::SetAskBeforeClosingMultiple => {
                self.cfg.ask_before_closing = kettle_config::AskBeforeClosing::MultipleTerminals;
                self.persist_pref("ask-before-closing", "multiple-terminals");
            }
            Action::SetAskBeforeClosingNever => {
                self.cfg.ask_before_closing = kettle_config::AskBeforeClosing::Never;
                self.persist_pref("ask-before-closing", "never");
            }
            Action::ToggleCursorBlink => {
                self.cfg.cursor_blink = !self.cfg.cursor_blink;
                // Turning blink on from the menu after the idle timeout
                // must show it blinking, not rest steady until a keystroke.
                self.reset_blink_phase(ws);
                self.persist_pref(
                    "cursor-blink",
                    if self.cfg.cursor_blink {
                        "true"
                    } else {
                        "false"
                    },
                );
            }
            Action::ToggleCopyOnSelect => {
                self.cfg.copy_on_select = !self.cfg.copy_on_select;
                self.persist_pref(
                    "copy-on-select",
                    if self.cfg.copy_on_select {
                        "true"
                    } else {
                        "false"
                    },
                );
            }
            Action::SetBellOff => {
                self.cfg.bell = kettle_config::BellMode::Off;
                self.persist_pref("bell", "off");
            }
            Action::SetBellVisual => {
                self.cfg.bell = kettle_config::BellMode::Visual;
                self.persist_pref("bell", "visual");
            }
            Action::SetBellAttention => {
                self.cfg.bell = kettle_config::BellMode::Attention;
                self.persist_pref("bell", "attention");
            }
            Action::SetBellBoth => {
                self.cfg.bell = kettle_config::BellMode::Both;
                self.persist_pref("bell", "both");
            }
            Action::ToggleMouseHide => {
                self.cfg.mouse_hide_while_typing = !self.cfg.mouse_hide_while_typing;
                self.persist_pref(
                    "mouse-hide-while-typing",
                    if self.cfg.mouse_hide_while_typing {
                        "true"
                    } else {
                        "false"
                    },
                );
            }
            // Open the README through the same background launcher as URL clicks.
            Action::ShowHelp => {
                let url = "https://github.com/Reddimus/kettle#readme";
                system_opener::open(url, move |e| {
                    log::warn!("Action::ShowHelp: failed to open {url}: {e}");
                });
            }
            // Terminator parity (`key_scaled_zoom`). Toggle pane zoom and scale
            // the font 1.5x so glyphs grow with the enlarged pane, then restore
            // the saved size on exit. `ToggleZoom` shares this save/restore via
            // `toggle_zoom_with_scale`, which reads the post-toggle zoom flag
            // and pairs save/restore through one `Option<f32>`, so mixing the
            // two actions keeps the state in step.
            Action::ScaledZoom => {
                self.toggle_zoom_with_scale(ws, Some(1.5));
            }
            Action::ToggleFullscreen => {
                if let Some(w) = &ws.window {
                    w.set_fullscreen(if w.fullscreen().is_some() {
                        None
                    } else {
                        Some(Fullscreen::Borderless(None))
                    });
                }
            }
            Action::ClearHistory => {
                // CSI 3 J (ED 3) clears scrollback only and keeps the visible
                // screen and grid state, unlike Reset (`\e c`, RIS), which
                // wipes everything. kitty / iTerm2 / WezTerm all expose this as
                // "Clear Scrollback" or similar. With group input on, clear
                // every pane the user is typing into, in every window the
                // broadcast scope reaches, not just the focused one.
                const CLEAR_SCROLLBACK: &[u8] = b"\x1b[3J";
                if ws.mux.is_broadcast_on() {
                    let targets = ws.mux.broadcast_target_ids();
                    apply_terminal_control(&ws.mux, &targets, CLEAR_SCROLLBACK);
                    if crate::mux::Mux::scope_crosses_windows(&ws.mux.broadcast) {
                        let scope = ws.mux.broadcast.clone();
                        for other in self.windows.values() {
                            let targets = other.mux.foreign_target_ids(&scope);
                            apply_terminal_control(&other.mux, &targets, CLEAR_SCROLLBACK);
                        }
                    }
                } else if let Some(pane_id) = ws.mux.active_focus() {
                    apply_terminal_control(&ws.mux, &[pane_id], CLEAR_SCROLLBACK);
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::Reset => {
                // RIS (`ESC c`) full-resets the engine: clears the grid,
                // restores DEC modes to defaults, drops the alt-screen back.
                // The PTY child then sees a fresh prompt next time it draws.
                // Kettle also owns UI state OUTSIDE the engine (selection,
                // scrollback display-offset, and the search, command palette,
                // hint mode and SSH launcher overlays), so sweep that too, or a
                // stale highlight or open modal survives the reset.
                if let Some(pane_id) = ws.mux.active_focus() {
                    // The reset turns mouse reporting off, so a held button's
                    // release goes to the program first.
                    self.release_held_buttons(ws, |held| held.pane == Some(pane_id));
                    apply_terminal_control(&ws.mux, &[pane_id], b"\x1bc");
                }
                self.clear_selection_on_input(ws);
                self.close_all_modals(ws);
                // Reset the blink phase so the cursor shows at once. Otherwise
                // a Reset during the blink-off phase hides the cursor for up to
                // one blink interval, and Reset is the chord users hit to
                // recover a visually-jammed terminal. Shares `reset_blink_phase`
                // with the focus-change and modal-close paths.
                self.reset_blink_phase(ws);
            }
            Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollLineUp
            | Action::ScrollLineDown
            | Action::ScrollToTop
            | Action::ScrollToBottom => {
                if let Some(p) = ws.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    t.scroll_display(match action {
                        Action::ScrollPageUp => Scroll::PageUp,
                        Action::ScrollPageDown => Scroll::PageDown,
                        // `Scroll::Delta(+n)` scrolls *back* (toward older
                        // lines) — same sign convention the mouse-wheel
                        // path uses; line-up = +1, line-down = -1.
                        Action::ScrollLineUp => Scroll::Delta(1),
                        Action::ScrollLineDown => Scroll::Delta(-1),
                        Action::ScrollToTop => Scroll::Top,
                        _ => Scroll::Bottom,
                    });
                }
            }
            Action::SelectAll => {
                // Select the entire buffer (scrollback + screen). Mirrors
                // `apply_smart_selection`'s Selection-building, spanning the
                // grid-absolute buffer bounds.
                if let Some(p) = ws.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    use kettle_core::Dimensions;
                    let (top, bottom) = selection_buffer_bounds(
                        t.grid().history_size(),
                        t.grid().screen_lines(),
                        t.grid().columns(),
                    );
                    let mut sel = kettle_core::Selection::new(
                        kettle_core::SelectionType::Simple,
                        top,
                        kettle_core::Side::Left,
                    );
                    sel.update(bottom, kettle_core::Side::Right);
                    t.selection = Some(sel);
                }
            }
            Action::SelectToTop | Action::SelectToBottom => {
                // Extend the selection to the first line / last cell of the
                // buffer (the AskUbuntu "select all in terminator" gesture,
                // Shift+Home / Shift+End), then scroll to reveal the new extent.
                let to_top = matches!(action, Action::SelectToTop);
                if let Some(p) = ws.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    use kettle_core::Dimensions;
                    let (top, bottom) = selection_buffer_bounds(
                        t.grid().history_size(),
                        t.grid().screen_lines(),
                        t.grid().columns(),
                    );
                    let (edge, side) = if to_top {
                        (top, kettle_core::Side::Left)
                    } else {
                        (bottom, kettle_core::Side::Right)
                    };
                    match t.selection.as_mut() {
                        // Grow an existing selection's active end toward the
                        // edge, keeping its anchor (editor-style range growth).
                        Some(sel) => sel.update(edge, side),
                        // No selection yet: anchor at the cursor, extend to edge.
                        None => {
                            let anchor = t.grid().cursor.point;
                            let mut sel = kettle_core::Selection::new(
                                kettle_core::SelectionType::Simple,
                                anchor,
                                kettle_core::Side::Left,
                            );
                            sel.update(edge, side);
                            t.selection = Some(sel);
                        }
                    }
                    t.scroll_display(if to_top { Scroll::Top } else { Scroll::Bottom });
                }
            }
            Action::JumpPrevPrompt | Action::JumpNextPrompt => {
                let prev = matches!(action, Action::JumpPrevPrompt);
                if let Some(p) = ws.mux.focused() {
                    p.term.jump_to_prompt(prev);
                }
            }
            Action::OpenSsh => {
                self.close_all_modals(ws);
                ws.ssh_input = Some((Default::default(), 0));
            }
            Action::CommandPalette => {
                self.close_all_modals(ws);
                ws.palette_input = Some((Default::default(), 0));
            }
            // Open the in-app settings overlay (Ctrl+, / right-click
            // → Settings / palette "Open settings").
            Action::OpenSettings => {
                self.close_all_modals(ws);
                // Enumerate GPUs once for the Graphics device picker and cache
                // them for the session; a wgpu adapter walk is too heavy to
                // repeat per frame.
                if self.gpu_choices.is_empty() {
                    self.gpu_choices = kettle_render::detect_gpus()
                        .into_iter()
                        .map(|g| {
                            (
                                format!("{:x}:{:x}:{}", g.vendor, g.device, g.name),
                                format!("{} ({})", g.name, g.kind),
                            )
                        })
                        .collect();
                }
                ws.settings_restart_pending = false;
                ws.settings_nav = Some(crate::settings::SettingsNav::default());
            }
            // Terminator parity (layoutlauncher.py): open the runtime layout
            // picker. With no saved layouts the modal still opens, with a hint
            // to save one with `kettle --layout NAME`.
            Action::OpenLayoutPicker => {
                self.close_all_modals(ws);
                ws.layout_picker_entries = crate::session::Session::list_layouts();
                ws.layout_picker_input = Some((Default::default(), 0));
            }
            Action::OpenThemePicker => self.open_theme_picker(ws),
            Action::OpenMediaShelf => self.open_media_shelf(ws),
            Action::PreviewClipboardPath => self.preview_copied_file(ws),
            Action::FocusPreview => self.focus_preview(ws),
            // The silent preview, labelled as such, never the video itself.
            Action::PreviewPlay => {
                if let Some(pane) = ws.mux.active_focus()
                    && ws.preview_panels.contains_key(&pane)
                {
                    self.toggle_lane_preview(ws, pane);
                }
            }
            Action::RenderClipboardAsDiagram => self.preview_copied_diagram(ws, false),
            Action::RenderSelectionAsDiagram => self.preview_copied_diagram(ws, true),
            Action::CopyAgentSetup => self.copy_agent_setup(),
            Action::PreviewNext
            | Action::PreviewPrevious
            | Action::PreviewNextDiagram
            | Action::PreviewPreviousDiagram
            | Action::ClosePreview
            | Action::PreviewSource
            | Action::PreviewCanvas
            | Action::PreviewCopy
            | Action::PreviewReload
            | Action::VideoPlay
            | Action::VideoMute
            | Action::PreviewZoomIn
            | Action::PreviewZoomOut
            | Action::PreviewFit => {
                if let Some(hit) = lane_action_hit(&action)
                    && let Some(pane) = ws.mux.active_focus()
                    && ws.preview_panels.contains_key(&pane)
                {
                    self.act_on_preview(ws, pane, hit);
                }
            }
            Action::HintMode | Action::PreviewLink => {
                let preview = matches!(action, Action::PreviewLink);
                let targets = self.collect_hints(ws, preview);
                if targets.is_empty() {
                    // Nothing in the pane to preview: say so, as the plain
                    // quick select's empty pane needs no word. A quick select
                    // already open stays as it was.
                    if preview {
                        let tr = self.ui_text;
                        fire_notify(
                            tr.text(kettle_i18n::Text::NotifyTitlePreviewNothing),
                            tr.text(kettle_i18n::Text::NotifyBodyPreviewNothing),
                        );
                    }
                } else {
                    self.close_all_modals(ws);
                    // The purpose goes with the targets it chose.
                    ws.hint_state = Some((targets, String::new()));
                    ws.hint_preview = preview;
                }
            }
            Action::ToggleViMode => {
                if ws.vi_mode.is_some() {
                    self.exit_vi_mode(ws);
                } else {
                    self.enter_vi_mode(ws);
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::OpenContextMenu => {
                if let Some(focus) = ws.preview_focus {
                    self.open_preview_menu(ws, focus.pane, None);
                } else {
                    let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    self.open_context_menu(ws, px, py, false);
                }
            }
            Action::UndoCloseTab => {
                let waker = self.waker();
                match ws
                    .mux
                    .undo_close_tab_geometry(&self.cfg, tab_geometry, waker)
                {
                    Ok(true) => {
                        self.resize_all(ws);
                        self.save_session(ws);
                    }
                    Ok(false) => {
                        log::debug!("undo_close_tab: ring is empty, nothing to restore");
                    }
                    Err(e) => log::error!("undo_close_tab failed: {e}"),
                }
            }
            Action::DuplicateTab => {
                let waker = self.waker();
                if let Err(e) =
                    ws.mux
                        .duplicate_focused_tab_geometry(&self.cfg, tab_geometry, waker)
                {
                    log::error!("duplicate_tab failed: {e}");
                } else {
                    self.resize_all(ws);
                    self.save_session(ws);
                }
            }
            Action::DuplicatePane => {
                let waker = self.waker();
                if let Err(e) = ws.mux.duplicate_focused_pane_geometry(
                    crate::mux::Dir::Horizontal,
                    &self.cfg,
                    horizontal_split_geometry,
                    waker,
                ) {
                    log::error!("duplicate_pane failed: {e}");
                } else {
                    self.restore_scaled_zoom_font(ws);
                    self.resize_all(ws);
                }
            }
            Action::NextTheme | Action::PrevTheme => {
                let fwd = matches!(action, Action::NextTheme);
                // Stay with the current theme's appearance, one similar
                // look at a time; switching light and dark is its own action.
                let name = kettle_config::Theme::cycle_by_look(&self.cfg.theme_name, fwd);
                self.set_runtime_theme_name(ws, name);
                if !self.persist_pref("theme", name) {
                    // Config-governed; notify on failure.
                    fire_notify(
                        self.ui_text
                            .text(kettle_i18n::Text::NotifyTitleThemeNotSaved),
                        self.ui_text.text(kettle_i18n::Text::NotifyBodyNotSaved),
                    );
                }
            }
            Action::ToggleLightDark => {
                if let Some(next) = pick_light_dark_target(
                    &self.cfg.theme_name,
                    &self.cfg.light_theme,
                    &self.cfg.dark_theme,
                ) {
                    self.set_runtime_theme_name(ws, &next);
                    if !self.persist_pref("theme", &next) {
                        // Config-governed; notify on failure.
                        fire_notify(
                            self.ui_text
                                .text(kettle_i18n::Text::NotifyTitleThemeNotSaved),
                            self.ui_text.text(kettle_i18n::Text::NotifyBodyNotSaved),
                        );
                    }
                } else {
                    log::warn!(
                        "toggle-light-dark: needs both light-theme and dark-theme in config (current: light={:?} dark={:?})",
                        self.cfg.light_theme,
                        self.cfg.dark_theme,
                    );
                }
            }
            Action::ToggleSessionLog => {
                // Terminator parity (`plugins/logger.py`):
                // toggle the focused pane's session log. This arm computes the
                // path; the persistence worker performs all filesystem I/O.
                // Routed through `Terminal::set_log_path` so secure file
                // preparation, the reader's lock-skip flag, and the bounded
                // writer stay in one transition without filesystem I/O here.
                if let Some(pane) = ws.mux.focused() {
                    if pane.term.log_enabled() {
                        match pane.term.set_log_path(None) {
                            Ok(_) => log::info!("toggle-session-log: stopped"),
                            Err(error) => {
                                log::warn!("toggle-session-log: stop failed: {error}");
                                fire_notify(
                                    self.ui_text
                                        .text(kettle_i18n::Text::NotifyTitleSessionLogError),
                                    self.ui_text
                                        .text(kettle_i18n::Text::NotifyBodySessionLogStopFailed),
                                );
                            }
                        }
                    } else {
                        let secs = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let cache = cache_dir_from_env(|k| std::env::var(k).ok());
                        let path = session_log_path(secs, std::process::id(), cache.as_deref());
                        // Propagate the config's strip-ANSI choice before the
                        // log goes live, so the first admitted read honors it.
                        if let Ok(mut strip) = pane.term.log_strip_ansi.lock() {
                            *strip = self.cfg.log_strip_ansi;
                        }
                        match pane.term.set_log_path(Some(path.clone())) {
                            Ok(_) => log::info!(
                                "toggle-session-log: queued asynchronous open for {}",
                                path.display()
                            ),
                            Err(error) => {
                                log::warn!(
                                    "toggle-session-log: start {} failed: {error}",
                                    path.display()
                                );
                                fire_notify(
                                    self.ui_text
                                        .text(kettle_i18n::Text::NotifyTitleSessionLogError),
                                    self.ui_text
                                        .text(kettle_i18n::Text::NotifyBodySessionLogStartFailed),
                                );
                            }
                        }
                    }
                }
            }
            Action::TakeScreenshot => {
                // Compute the output path, queue the
                // request, then fire a desktop notification so the
                // user knows where to look. Also compute
                // the focused pane's rect at dispatch time so the
                // screenshot crops to just that pane. Computed
                // BEFORE the `&mut ws.renderer` borrow to keep
                // the borrow window narrow.
                let area = self.area(ws);
                let active = ws.mux.active;
                let focus_id = ws.mux.tabs.get(active).map(|t| t.focus).unwrap_or(0);
                let crop = ws
                    .mux
                    .leaves(active, area)
                    .into_iter()
                    .find(|(id, _)| *id == focus_id)
                    .map(|(_, rect)| rect);
                if let Some(renderer) = ws.renderer.as_mut() {
                    let secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let cache = cache_dir_from_env(|k| std::env::var(k).ok());
                    let path = session_screenshot_path(secs, std::process::id(), cache.as_deref());
                    let path_str = path.display().to_string();
                    let proxy = self.proxy.clone();
                    let (completion, receiver) = std::sync::mpsc::channel();
                    let request = kettle_render::ScreenshotRequest {
                        out_path: path,
                        output_policy: kettle_render::ScreenshotOutputPolicy::PrivateState,
                        crop,
                        completion: Some(completion),
                        cancellation: None,
                        recovery_wake: Some(kettle_render::ScreenshotRecoveryWake::new(
                            move || {
                                let _ = proxy.send_event(UserEvent::Wakeup);
                            },
                        )),
                    };
                    if ws.screenshot_completion.is_none()
                        && renderer.set_pending_screenshot(request).is_ok()
                    {
                        ws.screenshot_completion = Some(receiver);
                        log::info!("take_screenshot: queued -> {path_str}");
                        fire_notify(
                            self.ui_text
                                .text(kettle_i18n::Text::NotifyTitleScreenshotQueued),
                            &path_str,
                        );
                    } else {
                        log::warn!("take_screenshot: another capture is in progress");
                        fire_notify(
                            self.ui_text
                                .text(kettle_i18n::Text::NotifyTitleScreenshotBusy),
                            self.ui_text
                                .text(kettle_i18n::Text::NotifyBodyScreenshotWait),
                        );
                    }
                }
            }
            Action::ReloadConfig => self.reload_config(ws),
            // Terminator's `move_tab` wraps at both ends of the bar; the drag
            // path clamps instead. See `Mux::nudge_active_tab`.
            Action::MoveTabLeft | Action::MoveTabRight => {
                let delta = if action == Action::MoveTabLeft { -1 } else { 1 };
                if ws.mux.nudge_active_tab(delta) {
                    self.save_session(ws);
                }
            }
            Action::NewTabShell(n) => {
                let shells = kettle_core::term::cached_shells();
                if let Some(argv) =
                    self.pending_shell_action
                        .request(ws.seq, n as usize, shells.as_deref())
                {
                    self.open_tab_with_argv(ws, &argv);
                }
            }
            Action::About => self.open_about_panel(ws),
            Action::GotoTab(n) => {
                let i = n as usize;
                if i < ws.mux.tabs.len() {
                    ws.mux.active = i;
                    ws.mux.touch_active_tab_seen();
                }
            }
            // Persistent update actions for bindings and the palette.
            // Both are a debug-logged no-op while no notice is active, so a
            // bound key is harmless the rest of the time.
            Action::OpenUpdate => {
                if !self.open_update_release(ws) {
                    log::debug!("open_update: no update notice is active");
                }
            }
            Action::DismissUpdate => {
                if !self.dismiss_update_notice(ws) {
                    log::debug!("dismiss_update: no update notice is active");
                }
            }
            // Terminator parity: the Edit-title overlay. Each action opens it
            // pre-filled with the current title; Enter applies via the matching
            // setter, Esc cancels. It renders as a thin bar at the top of the
            // window, shaped like the `Action::CommandPalette` overlay.
            Action::EditWindowTitle => {
                self.close_all_modals(ws);
                ws.editing_title = begin_title_edit(
                    &ws.mux,
                    TitleEditScope::Window,
                    GroupBulkScope::Single,
                    &ws.last_title,
                );
                ws.pending_resize = true;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::EditTabTitle => {
                self.close_all_modals(ws);
                ws.editing_title = begin_title_edit(
                    &ws.mux,
                    TitleEditScope::Tab,
                    GroupBulkScope::Single,
                    &ws.last_title,
                );
                ws.pending_resize = true;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::EditPaneTitle => {
                self.close_all_modals(ws);
                ws.editing_title = begin_title_edit(
                    &ws.mux,
                    TitleEditScope::Pane,
                    GroupBulkScope::Single,
                    &ws.last_title,
                );
                ws.pending_resize = true;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::EditPaneGroup | Action::CreateGroup => {
                // Edit the focused pane's
                // broadcast-group name. Empty input → clear the
                // group. Same overlay mechanism as
                // EditPaneTitle. `CreateGroup` (Terminator name)
                // and `EditPaneGroup` (kettle name) share dispatch.
                self.close_all_modals(ws);
                ws.editing_title = begin_title_edit(
                    &ws.mux,
                    TitleEditScope::Group,
                    GroupBulkScope::Single,
                    &ws.last_title,
                );
                ws.pending_resize = true;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::GroupTab | Action::GroupWindow => {
                // Open the
                // title-edit overlay with `bulk` set to Tab/Window
                // so on Apply the typed name writes to every pane
                // in scope.
                self.close_all_modals(ws);
                let bulk = if matches!(action, Action::GroupTab) {
                    GroupBulkScope::Tab
                } else {
                    GroupBulkScope::Window
                };
                ws.editing_title =
                    begin_title_edit(&ws.mux, TitleEditScope::Group, bulk, &ws.last_title);
                ws.pending_resize = true;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::GroupAll | Action::UngroupAll | Action::ToggleGroupAll => {
                // Terminator parity (window.py:933 / :947). `group_all` puts
                // every terminal into a group literally named "All"; there is
                // no prompt, which is what separates it from `GroupWindow`.
                // `ungroup_all` is its partner.
                let all = kettle_ui_group_all_name();
                // `group_all_toggle` asks the INVOKING terminal, not the whole
                // set (window.py:940-945: `if widget.group == 'All'`). Testing
                // "is everything already grouped" instead inverted the answer
                // whenever one sibling had been ungrouped by hand: Terminator
                // ungroups everything, kettle grouped the straggler.
                let group = match action {
                    Action::GroupAll => true,
                    Action::UngroupAll => false,
                    _ => ws.mux.focused().and_then(|p| p.group_name.as_deref()) != Some(all),
                }
                .then(|| all.to_string());
                // Terminator's scope is its whole terminal collection, across
                // every window (`self.terminator.terminals`), so a second
                // kettle window must not be left out.
                for target in std::iter::once(&mut *ws).chain(self.windows.values_mut()) {
                    let pane_ids: Vec<u64> = target.mux.panes.keys().copied().collect();
                    for id in pane_ids {
                        if let Some(p) = target.mux.panes.get_mut(&id) {
                            p.group_name = group.clone();
                        }
                    }
                    if let Some(w) = &target.window {
                        w.request_redraw();
                    }
                }
            }
            Action::ToggleGroupTab | Action::ToggleGroupWindow => {
                // Terminator parity (window.py:959, :987). It tests the
                // INVOKING terminal's group and generates the name rather than
                // asking for one — `Tab N` / `Window group N` — which is what
                // makes these a toggle at all, where kettle's `GroupTab` and
                // `GroupWindow` open the name prompt.
                let tab_scope = matches!(action, Action::ToggleGroupTab);
                let name = if tab_scope {
                    format!("Tab {}", ws.mux.active + 1)
                } else {
                    format!("Window group {}", ws.seq)
                };
                let grouped = ws
                    .mux
                    .focused()
                    .and_then(|p| p.group_name.as_deref())
                    .is_some_and(|g| g == name);
                let pane_ids: Vec<u64> = if tab_scope {
                    ws.mux
                        .tabs
                        .get(ws.mux.active)
                        .map(|t| t.root.leaf_ids())
                        .unwrap_or_default()
                } else {
                    ws.mux.panes.keys().copied().collect()
                };
                for id in pane_ids {
                    if let Some(p) = ws.mux.panes.get_mut(&id) {
                        p.group_name = (!grouped).then(|| name.clone());
                    }
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::UngroupTab | Action::UngroupWindow => {
                // Bulk-
                // clear the group on every pane in scope. No
                // overlay needed — empty input is the "clear"
                // signal, and the action carries the scope.
                let pane_ids: Vec<u64> = if matches!(action, Action::UngroupTab) {
                    ws.mux
                        .tabs
                        .get(ws.mux.active)
                        .map(|t| t.root.leaf_ids())
                        .unwrap_or_default()
                } else {
                    ws.mux.panes.keys().copied().collect()
                };
                for id in pane_ids {
                    if let Some(p) = ws.mux.panes.get_mut(&id) {
                        p.group_name = None;
                    }
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            // Terminator parity (terminatorlib/terminal.py:
            // key_next_profile + key_previous_profile): runtime cycle
            // through profile files at <config-dir>/profiles/.
            //
            // Enumerates the profiles directory each dispatch (cheap;
            // typically <10 entries), sorts deterministically, finds
            // the current entry by basename match against the loaded
            // config-path, and loads the next/prev. Falls back to
            // log::info when no profiles directory or no entries.
            Action::NextProfile | Action::PrevProfile => {
                // Delegate listing + name extraction to
                // kettle-config so the same path math has a single
                // home (and drift guards on it). Empty list → no-op
                // with a one-line info nudge.
                let names = kettle_config::Config::list_profiles();
                if names.is_empty() {
                    log::info!(
                        "{action:?}: no profiles in <config-dir>/profiles/ — \
                         create one with `kettle --print-default-config > \
                         ~/.config/kettle/profiles/dev.config`"
                    );
                } else {
                    let current = self
                        .config_path
                        .as_deref()
                        .and_then(kettle_config::Config::profile_name_from_path);
                    let next = pick_next_profile(
                        current.as_deref(),
                        &names,
                        matches!(action, Action::NextProfile),
                    );
                    if let Some(p) = kettle_config::Config::path_for_profile(&next) {
                        self.switch_profile(ws, p);
                    }
                }
            }
            // Split-tree rotation (Terminator `rotate_cw` / `rotate_ccw`):
            // turn the active tab's whole layout a quarter turn. No-op on a
            // single-pane tab. Rotating changes every pane's rect, so the PTYs
            // have to be resized and the new arrangement saved — a redraw alone
            // would leave every child process believing its old geometry.
            Action::RotateCw | Action::RotateCcw => {
                if ws.mux.rotate_layout(action == Action::RotateCw) {
                    self.restore_scaled_zoom_font(ws);
                    self.resize_all(ws);
                    self.save_session(ws);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
            // Runtime scrollbar toggle. Cycles
            // ScrollbarMode through Never → Always → Auto → Never.
            // Three-state cycle (vs binary) because Auto is the
            // useful steady state for most users; explicit toggle
            // is for power users with a specific preference. Same
            // shape as the `NextTheme` cycle.
            Action::ToggleScrollbar => {
                use kettle_config::ScrollbarMode::*;
                self.cfg.scrollbar = match self.cfg.scrollbar {
                    Never => Always,
                    Always => Auto,
                    Auto => Never,
                };
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            // Terminator parity: toggle the focused pane's read-only
            // state. While on, user input (keystrokes / paste / broadcast) is
            // dropped before it reaches the PTY; the child keeps producing
            // output. A `[RO]` titlebar badge shows the state.
            Action::TogglePaneReadOnly => {
                // A pane turned read-only gets no more input, so a held
                // button's release goes to it first.
                if let Some(pane) = ws.mux.active_focus()
                    && ws.mux.focused().is_some_and(|focused| !focused.read_only)
                {
                    self.release_held_buttons(ws, |held| held.pane == Some(pane));
                }
                let _ = ws.mux.toggle_focused_read_only();
                // The `[RO]` titlebar badge reflects the new state.
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            // Insert pane index. Pane index is 1-based
            // (matches Terminator's GotoTab + every user-facing
            // numbering). InsertPanePadded uses 2-digit zero-padded
            // form (Terminator default).
            Action::InsertPaneNumber => {
                let idx = ws
                    .mux
                    .focused_pane_index_in_tab()
                    .map(|i| i + 1)
                    .unwrap_or(1);
                self.write_focused_terminal_input(ws, idx.to_string().as_bytes());
            }
            Action::InsertPanePadded => {
                let idx = ws
                    .mux
                    .focused_pane_index_in_tab()
                    .map(|i| i + 1)
                    .unwrap_or(1);
                self.write_focused_terminal_input(ws, format!("{idx:02}").as_bytes());
            }
            // Terminator parity (`insert_term_name.py`
            // plugin → `InsertTermName` menu item + keybind). Send
            // the focused pane's title (Pane::title — same string
            // the chrome shows in the per-pane titlebar) to the
            // PTY. Useful for scripts that want to label their
            // output by source pane, or for keyboard workflows
            // that re-type the current title into the command line.
            Action::InsertPaneName => {
                if let Some(title) = ws.mux.focused().map(|pane| pane.title.clone()) {
                    self.write_focused_terminal_input(ws, title.as_bytes());
                }
            }
            // Terminator parity (`dir_open.py` plugin → `CurrDirOpen` menu
            // item). Open the focused pane's cwd in the OS file manager by
            // routing `file://<cwd>` through `open_url`, like a clicked
            // `file://` hyperlink, so the `LuaEvent::UrlClicked` dispatch, the
            // `custom_url_handler` config and the `is_safe_url` allowlist
            // (which accepts `file://` without `..`) all apply.
            Action::OpenCwdInFileManager => {
                match ws
                    .mux
                    .focused()
                    .and_then(|p| p.term.current_dir_or_native())
                {
                    // Refuse a non-local OSC 7 cwd before
                    // building/opening the URL (it's untrusted PTY input — a
                    // UNC path would trigger an SMB/NTLM leak on Windows). A
                    // remote pane's directory is on another machine.
                    // The URL is encoded from the path, so a directory with a
                    // space or `#` in its name opens rather than failing the
                    // URL check or naming another folder.
                    Some(cwd) if cwd_is_local(&cwd) => {
                        let pane = ws.mux.active_focus();
                        match kettle_core::links::file_url_for_path(std::path::Path::new(&cwd)) {
                            Some(url) => self.open_pane_link(ws, pane, &url),
                            None => log::warn!(
                                "Action::OpenCwdInFileManager: the cwd has no local file URL"
                            ),
                        }
                    }
                    Some(_) => {
                        log::warn!(
                            "Action::OpenCwdInFileManager: refusing a non-local cwd \
                             reported via OSC 7 (possible UNC/SSRF payload)"
                        );
                    }
                    None => {
                        log::info!("Action::OpenCwdInFileManager: focused pane has no known cwd");
                    }
                }
            }
            // Half-page scroll. Same shape as the
            // ScrollPageUp/Down handler but with half the row count.
            // Pull the row count from the focused pane's grid
            // dimensions (works for any pane size).
            Action::ScrollPageUpHalf | Action::ScrollPageDownHalf => {
                if let Some(p) = ws.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    use kettle_core::Dimensions;
                    let rows = t.screen_lines() as i32;
                    let half = (rows / 2).max(1);
                    let dir = if matches!(action, Action::ScrollPageUpHalf) {
                        half
                    } else {
                        -half
                    };
                    t.scroll_display(Scroll::Delta(dir));
                }
            }
            // Paste the X11 PRIMARY selection (middle-click). On X11
            // PRIMARY is the last mouse-highlighted text, distinct from the
            // CLIPBOARD; `paste_primary` reads PRIMARY on Linux and falls back
            // to the clipboard on Wayland/macOS/Windows. It shares `paste_text`
            // so the LOCAL_PASTE_MAX clamp, bracketed-paste wrap, and broadcast
            // scoping all match Action::Paste.
            Action::PastePrimary => self.paste_primary(ws),
            // In-process Quake toggle. Same tri-state
            // logic as the `--toggle` remote command:
            //   hidden → show + focus
            //   visible + focused → hide
            //   visible + !focused → focus (don't hide)
            Action::ToggleWindowVisibility => {
                if let Some(w) = &ws.window {
                    let visible = w.is_visible().unwrap_or(true);
                    let focused = w.has_focus();
                    if !visible {
                        w.set_visible(true);
                        w.focus_window();
                    } else if focused {
                        w.set_visible(false);
                    } else {
                        w.focus_window();
                    }
                }
            }
            // Terminator parity (`detachable_tabs`): move the active tab into a
            // new window.
            Action::MoveTabToWindow => self.open_tab_window_chooser(ws),
            Action::MoveTabToNewWindow => {
                // Move the tab LIVE, in process. Its panes (PTYs, scrollback,
                // running programs) transfer untouched to a brand-new window
                // via detach_tab → open_window(AdoptTab). The deprecated
                // receive-side `--tab-handoff` parsing remains for an old sender
                // caught mid-upgrade.
                if !self.cfg.detachable_tabs {
                    log::info!("move_tab_to_new_window ignored because detachable-tabs = false");
                    return;
                }
                if ws.mux.tabs.len() <= 1 {
                    // Moving a lone tab out of its window is a no-op: you'd
                    // get the same window back.
                    return;
                }
                let closing_idx = ws.mux.active;
                self.release_held_buttons_in_tab(ws, closing_idx);
                let Some(dt) = ws.mux.detach_tab(closing_idx) else {
                    return;
                };
                self.release_focus_report_to(ws, &dt);
                let paste = ws.take_moved_paste_state(&dt.tab, std::time::Instant::now());
                match self
                    .open_window(
                        event_loop,
                        WindowOpen::AdoptTab {
                            tab: dt,
                            source_color: self.tear_off_source_color(ws),
                            font_size: ws.renderer.as_ref().map(|r| r.font_size()),
                            scaled_zoom_prev_font_size: ws.scaled_zoom_prev_font_size,
                            paste: Box::new(paste),
                        },
                        None,
                        None,
                    )
                    .map_err(|open| *open)
                {
                    Ok(_) => {
                        // The tab LEFT this window, so plugins see a tab-close
                        // event.
                        self.fire_tab_close_event(ws, closing_idx);
                        // Agents subscribed to the event feed see moves.
                        self.ctl_broadcast(
                            "tab_moved",
                            None,
                            serde_json::json!({
                                "from_window": ws.seq,
                                "to_window": self.focused_seq,
                                "tab": closing_idx,
                            }),
                        );
                        self.resize_all(ws);
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                    }
                    Err(WindowOpen::AdoptTab { tab: dt, paste, .. }) => {
                        // Window creation failed — put the live tab back
                        // exactly where it was; nothing is lost.
                        log::warn!(
                            "MoveTabToNewWindow: open_window failed; tab kept in source window"
                        );
                        ws.mux.attach_tab(dt, Some(closing_idx));
                        ws.restore_moved_paste_state(*paste);
                    }
                    Err(_) => unreachable!("open_window returns the WindowOpen it was given"),
                }
            }
            Action::ResetAndClear => {
                // Terminator parity (key_reset_clear): Reset (RIS, \ec) and
                // ClearHistory (CSI 3 J) in one keybind, with Reset's sweep of
                // Kettle's own UI state.
                if let Some(pane_id) = ws.mux.active_focus() {
                    // As for Reset, a held button's release goes first.
                    self.release_held_buttons(ws, |held| held.pane == Some(pane_id));
                    // One request, so a full queue cannot accept RIS but reject
                    // the paired history clear.
                    apply_terminal_control(&ws.mux, &[pane_id], b"\x1bc\x1b[3J");
                }
                self.clear_selection_on_input(ws);
                self.close_all_modals(ws);
                self.reset_blink_phase(ws);
            }
        }
        // If focus moved as a result of the action,
        // land the cursor visible on the new pane right away.
        self.hoover_groups(ws);
        self.note_focus_change(ws, pre_focus);
        // Coalesce into the per-frame flush when a frame is actually coming.
        //
        // This tail runs after EVERY action. Resizing here eagerly meant a
        // queued pair of geometry-affecting actions -- a Lua hook dispatching
        // `EditWindowTitle` then `CommandPalette`, processed before any redraw
        // -- resized twice: shrink, then grow, for a net-zero frame. The user
        // sees nothing, but the PTY child gets two `SIGWINCH`s and vim, tmux or
        // htop redraw for no reason.
        //
        // `save_session` runs immediately below and does NOT depend on this:
        // it records the OS window rect and the split tree, never the PTY grid.
        //
        // With no window there is no frame to flush on, so resize now — a
        // deferred resize that never runs is worse than an eager one that runs
        // twice.
        if ws.window.is_some() {
            ws.pending_resize = true;
        } else {
            self.resize_all(ws);
        }
        if save_session {
            self.save_session(ws);
        }
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }
}
