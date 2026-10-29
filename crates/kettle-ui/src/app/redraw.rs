use super::*;

/// Output that lands within this window of a keystroke is ECHO and paints
/// immediately, skipping the coalescer, so key repeat doesn't stutter. Long
/// enough to bridge OS key-repeat intervals (~33ms at default rates) plus
/// ConPTY echo latency; short enough that an unrelated burst (a build log
/// kicking in a beat after you pressed Enter twice) re-enters the coalescer
/// quickly.
pub(super) const TYPING_ECHO_WINDOW: std::time::Duration = std::time::Duration::from_millis(150);

/// Pure (drift-tested in `typed_echo_bypasses_the_output_coalescer`): is
/// `now` inside the typing-echo window after the last keystroke?
pub(super) fn typed_recently(
    now: std::time::Instant,
    last_typed: Option<std::time::Instant>,
    window: std::time::Duration,
) -> bool {
    last_typed.is_some_and(|t| now.saturating_duration_since(t) < window)
}

pub(super) fn pane_snapshot_keys_match(
    cached: &[(u64, u64, usize, usize)],
    current: impl IntoIterator<Item = (u64, u64, usize, usize)>,
) -> bool {
    let mut current = current.into_iter();
    for cached_key in cached {
        match current.next() {
            Some(current_key) if *cached_key == current_key => {}
            _ => return false,
        }
    }
    current.next().is_none()
}

pub(super) fn cached_pane_cursor_blinking(
    active_pane: Option<u64>,
    keys: &[(u64, u64, usize, usize)],
    snapshots: &[PaneSnapshot],
) -> Option<bool> {
    let active_pane = active_pane?;
    keys.iter()
        .position(|(pane_id, ..)| *pane_id == active_pane)
        .and_then(|index| snapshots.get(index))
        .map(|snapshot| snapshot.cursor_blinking)
}

pub(super) fn pane_cursor_blinking_with<F>(
    active_pane: Option<u64>,
    keys: &[(u64, u64, usize, usize)],
    snapshots: &[PaneSnapshot],
    use_pane_snapshots: bool,
    live: F,
) -> bool
where
    F: FnOnce(u64) -> Option<bool>,
{
    let Some(active_pane) = active_pane else {
        return true;
    };
    if use_pane_snapshots {
        return cached_pane_cursor_blinking(Some(active_pane), keys, snapshots).unwrap_or(true);
    }
    live(active_pane).unwrap_or(true)
}

/// Snapshot reuse is safe only when the context menu owns the pointer.
///
/// These gesture states can mutate terminal selection, scrollback, or layout
/// from a `CursorMoved` event. Cursor movement is intentionally allowed to
/// preserve the one-shot menu-hover hint, so an active gesture must block the
/// fast path even when output generation and grid dimensions still match.
pub(super) fn context_menu_snapshot_reuse_safe(ws: &WindowState) -> bool {
    !ws.selecting
        && ws.scrollbar_drag_offset.is_none()
        && ws.dragging_split.is_none()
        && !ws.held_buttons.any()
        && !ws.tab_drag_active
        && ws.tab_drag_press.is_none()
        && ws.drag_press.is_none()
        && ws.pane_drag.is_none()
        && matches!(&ws.detach_drag, crate::detach::DragState::Idle)
}

impl App {
    pub(super) fn overlay(&self, ws: &WindowState, reuse_pane_snapshots: bool) -> Overlay {
        let preedit_state = ws.ime_preedit.as_ref().filter(|(text, _)| !text.is_empty());
        let preedit = preedit_state.map(|(text, _)| text.as_str());
        let with_preedit = |input: &crate::text_editor::TextEditor| {
            let mut projected = input.text().to_string();
            if let Some(preedit) = preedit {
                let range = input.selection().unwrap_or(input.cursor()..input.cursor());
                projected.replace_range(range, preedit);
            }
            projected
        };
        // Hovering any segment of a link wrapped across rows lights them all.
        let hovered_group = self.link_at_cursor(ws).map(|link| link.group);
        let links = ws
            .links
            .iter()
            .map(|l| kettle_render::LinkRect {
                col: l.start_col,
                row: l.row,
                width: (l.end_col + 1).saturating_sub(l.start_col).max(1),
                hover: hovered_group == Some(l.group),
            })
            .collect();

        use kettle_i18n::Text as T;
        let tr = &self.ui_text;
        let (ssh_query, ssh_hint, ssh_picker) = match &ws.ssh_input {
            Some((query, selected)) => (
                Some(with_preedit(query)),
                picker_hint(
                    tr,
                    &[
                        ("Enter", T::PickerHintConnect),
                        ("Tab", T::PickerHintComplete),
                        ("↑↓", T::PickerHintSelect),
                        ("Esc", T::PickerHintCancel),
                    ],
                ),
                Some(ssh_picker_list(query, *selected, &self.cfg.ssh_hosts, tr)),
            ),
            None => (None, String::new(), None),
        };

        let (palette_query, palette_hint, palette_picker) = match &ws.palette_input {
            Some((query, selected)) => (
                Some(with_preedit(query)),
                picker_hint(
                    tr,
                    &[
                        ("Enter", T::PickerHintRun),
                        ("Tab/↑↓", T::PickerHintSelect),
                        ("Esc", T::PickerHintCancel),
                    ],
                ),
                Some(command_picker_list(
                    query,
                    *selected,
                    &self.cfg.keybinds,
                    tr,
                    self.update_notice().as_ref(),
                )),
            ),
            None => (None, String::new(), None),
        };

        // Terminator parity (layoutlauncher.py): build the layout picker's
        // query and hint the same way as the command palette. An empty
        // layouts dir is fine; the picker shows a "No saved layouts" row
        // hinting `kettle --layout NAME`.
        let (layout_picker_query, layout_picker_hint, layout_picker) = match &ws.layout_picker_input
        {
            Some((query, selected)) => (
                Some(with_preedit(query)),
                picker_hint(
                    tr,
                    &[
                        ("Enter", T::PickerHintSpawn),
                        ("Tab/↑↓", T::PickerHintSelect),
                        ("Esc", T::PickerHintCancel),
                    ],
                ),
                Some(layout_picker_list(
                    query,
                    *selected,
                    &ws.layout_picker_entries,
                    tr,
                )),
            ),
            None => (None, String::new(), None),
        };
        let (theme_picker_query, theme_picker_hint, theme_picker) = match &ws.theme_picker_input {
            Some(state) => (
                Some(with_preedit(&state.query)),
                picker_hint(
                    tr,
                    &[
                        ("Enter", T::PickerHintKeep),
                        ("↑↓", T::PickerHintSelect),
                        ("Esc", T::PickerHintRestore),
                    ],
                ),
                Some(theme_picker_list(
                    &state.query,
                    state.selected,
                    &state.opened_on,
                    tr,
                )),
            ),
            None => (None, String::new(), None),
        };
        let hint_labels: Vec<HintLabel> = match &ws.hint_state {
            Some((targets, typed)) => targets
                .iter()
                .map(|t| HintLabel {
                    row: t.row,
                    col: t.col,
                    label: t.label.clone(),
                    dim: !typed.is_empty() && !t.label.starts_with(typed.as_str()),
                })
                .collect(),
            None => Vec::new(),
        };
        let terminal_surface = terminal_surface_available(ws);
        let completion = self.completion_overlay_projection(ws, terminal_surface);
        let media_paste_receipt = self.media_paste_receipt_overlay_projection(
            ws,
            terminal_surface,
            std::time::Instant::now(),
        );
        // Lanes stay drawn under every modal, which paints over them; their
        // room in the pane stays carved either way.
        let media_lanes = self.preview_lane_projections(ws);
        let media_viewer_rect = ws.preview_overlay.and_then(|open| {
            media_lanes
                .iter()
                .find(|lane| lane.pane == open.pane)
                .map(|lane| lane.rect)
        });
        let ime_preedit = preedit.filter(|_| terminal_surface).and_then(|text| {
            let (row, col) = self.terminal_cursor_cell(ws)?;
            Some(ImePreedit {
                text: text.to_string(),
                row,
                col,
            })
        });

        let window_focused = ws.window_focused;
        // Brighten the focused pane's scrollbar while the pointer is on the
        // gutter or the thumb is being dragged (overlay-scrollbar feel). The
        // scrolled-back case is decided per-pane in the renderer from the snapshot.
        let scrollbar_active = ws.scrollbar_hover || ws.scrollbar_drag_offset.is_some();
        // Cursor blink is the *intersection* of the user config and the
        // running app's wishes — programs flip it via DEC private mode 12
        // (`CSI ?12 h/l`), which the engine tracks per-pane on its
        // `cursor_style().blinking`. Read the active pane's live state so
        // editors like vim that disable blink for their own cursor are
        // honored even when the global config wants blink. (Goes through
        // `active_focus` + `panes.get` so the `overlay()` builder stays a
        // pure `&self` reader.)
        let pane_blink = self.pane_cursor_blinking(ws, reuse_pane_snapshots);
        let blink_enabled = self.cfg.cursor_blink && pane_blink;
        let cursor_visible = if !blink_enabled || !window_focused || modal_holds_cursor_steady(ws) {
            true
        } else {
            ws.blink_on
        };
        // The transient resize chip. `about_to_wait` drives the expiry
        // repaint and clears the state.
        let resize_overlay = ws
            .resize_overlay
            .and_then(|(c, r, t)| (t.elapsed() < RESIZE_OVERLAY_DURATION).then_some((c, r)));

        let context_menu = self.context_menu_overlay(ws).or_else(|| {
            let surface = ws
                .renderer
                .as_ref()
                .map(|renderer| {
                    let (width, height) = renderer.surface_size();
                    (width as f32, height as f32)
                })
                .unwrap_or((800.0, 600.0));
            picker_overlay_context_menu(
                [palette_picker, theme_picker, layout_picker, ssh_picker],
                surface,
                self.menu_cell(ws),
            )
        });
        // Marshal the in-progress Edit-title state for
        // the render layer so the user sees what they're typing. The
        // title-edit rect is app chrome, never a bottom overlay over terminal
        // rows.
        let edit_title: Option<kettle_render::TitleEditOverlay> =
            ws.editing_title.as_ref().map(|s| {
                let label = title_edit_label(s.scope, &self.ui_text);
                kettle_render::TitleEditOverlay {
                    label: label.to_string(),
                    input: with_preedit(&s.input),
                    hint: picker_hint(
                        &self.ui_text,
                        &[
                            ("Enter", kettle_i18n::Text::EditHintApply),
                            ("Esc", kettle_i18n::Text::PickerHintCancel),
                        ],
                    ),
                    rect: self.title_edit_rect(ws),
                }
            });
        // Project the App's confirm_dialog into the
        // renderer's projection (so it shows even when no
        // search is open — confirm modals are independent).
        let confirm_dialog_early = ws.confirm_dialog.as_ref().map(|dialog| {
            let mut overlay = confirm_dialog_overlay(dialog, tr);
            if let Some(layout) = self.confirmation_layout(ws) {
                let (x, y) = (ws.cursor.x as f32, ws.cursor.y as f32);
                overlay.hover_idx = ws
                    .native_pointer
                    .and_then(|_| confirm_dialog_button_hit(&layout, x, y));
                if overlay.pressed_idx.is_none() {
                    overlay.pressed_idx = confirm_release_target(dialog.armed_press, &layout, x, y);
                }
            }
            overlay
        });
        // Project the settings overlay (independent of search, like
        // the confirm dialog). Values are read from the live Config so the
        // panel reflects the current state (incl. external reloads).
        let settings_overlay = self.settings_overlay_projection(ws);
        // Terminator parity: only a LIVE drag with a latched target paints a
        // hint. An armed-but-unmoved press is still a click, and a live drag
        // over a seam has no target to preview.
        let pane_drop_hint = ws
            .pane_drag
            .as_ref()
            .filter(|drag| drag.live)
            .and_then(|drag| drag.target)
            .map(|(_, rect, dir, before)| crate::mux::pane_drop_preview(rect, dir, before));
        let card_hover = ws.card_hover;
        let s = &ws.search;
        // Both literals list every field: a defaulted overlay would paint
        // English and drop state such as the pane drop hint.
        if !s.open {
            return Overlay {
                tr: self.ui_text,
                text_field: self.modal_text_projection(ws),
                search: None,
                highlights: Vec::new(),
                links,
                ssh_query,
                ssh_hint,
                palette_query,
                palette_hint,
                layout_picker_query,
                layout_picker_hint,
                theme_picker_query,
                theme_picker_hint,
                edit_title,
                hint_labels,
                ime_preedit,
                completion,
                media_paste_receipt,
                media_lanes,
                media_viewer_rect,
                media_pointer: (!self.non_search_modal_open(ws))
                    .then_some(ws.native_pointer)
                    .flatten()
                    .map(|p| (p.x as f32, p.y as f32)),
                media_scrubbing: ws.lane_drag.is_some_and(|drag| drag.scrub),
                card_hover,
                window_focused,
                scrollbar_active,
                cursor_visible,
                resize_overlay,
                update_toast: self.update_toast_projection(ws),
                context_menu,
                confirm_dialog: confirm_dialog_early,
                settings: settings_overlay,
                pane_drop_hint,
            };
        }
        let mut highlights = Vec::new();
        if let Some(pane_id) = s.target_pane
            && let Some(pane) = ws.mux.panes.get(&pane_id)
            && let Ok(term) = pane.term.term.lock()
        {
            use kettle_core::Dimensions as _;
            let display_offset = term.grid().display_offset();
            let screen_lines = term.screen_lines();
            let last_column = term.last_column().0;
            let offset = i32::try_from(display_offset).unwrap_or(i32::MAX);
            let visible_top = -offset;
            let visible_bottom = i32::try_from(screen_lines.saturating_sub(1))
                .unwrap_or(i32::MAX)
                .saturating_sub(offset);
            for span in &s.visible {
                let first_line = span.start.line.max(visible_top);
                let last_line = span.end.line.min(visible_bottom);
                if first_line > last_line {
                    continue;
                }
                let active = s.focused == Some(*span);
                for line in first_line..=last_line {
                    let start_col = if line == span.start.line {
                        span.start.column
                    } else {
                        0
                    };
                    let end_col = if line == span.end.line {
                        span.end.column.min(last_column)
                    } else {
                        last_column
                    };
                    if end_col < start_col {
                        continue;
                    }
                    if let Some(rect) = HighlightRect::from_grid_span(
                        line,
                        display_offset,
                        screen_lines,
                        start_col,
                        end_col.saturating_sub(start_col).saturating_add(1),
                        active,
                    ) {
                        highlights.push(rect);
                    }
                }
            }
        }
        let confirm_dialog = confirm_dialog_early;
        let mut search_query = s.query().to_string();
        let mut search_cursor = s.editor.cursor();
        let mut search_selection = s.editor.selection().map(|range| (range.start, range.end));
        if let Some((preedit, selection)) = preedit_state {
            let replace = s.editor.selection().unwrap_or(search_cursor..search_cursor);
            search_query.replace_range(replace.clone(), preedit);
            search_cursor = replace
                .start
                .saturating_add(ime_preedit_focus(preedit, *selection));
            search_selection = None;
        }
        let search_horizontal_scroll = self
            .search_geometry(ws)
            .and_then(|geometry| {
                let renderer = ws.renderer.as_ref()?;
                let columns = kettle_render::search_bar_columns(
                    geometry.editor.2,
                    renderer.overlay_text_cell_width(),
                )
                .max(1);
                Some(crate::search_input::SearchEditor::visible_scroll_for(
                    &search_query,
                    search_cursor,
                    s.editor.horizontal_scroll(),
                    columns.saturating_sub(2),
                ))
            })
            .unwrap_or_else(|| s.editor.horizontal_scroll());
        Overlay {
            tr: self.ui_text,
            text_field: self.modal_text_projection(ws),
            search: Some(kettle_render::SearchOverlay {
                tr: self.ui_text,
                target_pane: s.target_pane,
                query: search_query,
                cursor_byte: search_cursor,
                selection: search_selection,
                horizontal_scroll: search_horizontal_scroll,
                wrap: s.wrap,
                case_mode: map_search_case_mode(s.case_mode),
                invert: s.invert,
                status: displayed_search_status(s, std::time::Instant::now()),
                focused: s.focused_control,
                hovered: s.hovered_control,
                pressed: s.pressed_control,
            }),
            highlights,
            links,
            ssh_query,
            ssh_hint,
            palette_query,
            palette_hint,
            layout_picker_query,
            layout_picker_hint,
            theme_picker_query,
            theme_picker_hint,
            edit_title,
            hint_labels,
            ime_preedit,
            completion: None,
            media_paste_receipt: None,
            media_lanes,
            media_viewer_rect: None,
            media_pointer: None,
            media_scrubbing: false,
            card_hover,
            window_focused,
            scrollbar_active,
            cursor_visible,
            resize_overlay,
            update_toast: None,
            context_menu,
            confirm_dialog,
            settings: settings_overlay,
            pane_drop_hint,
        }
    }

    pub(super) fn redraw(&mut self, ws: &mut WindowState) {
        // A frame while the window server blinks the cursor ends that blink.
        // Bring the app's phase to where the animation is first, so the frame
        // draws what the screen shows. The harness timer includes this work.
        let cursor_exit_frame = if ws.cursor_exit_log.enabled {
            ws.cursor_exit_log.begin_frame(
                ws.blink_layer.is_active(),
                crate::cursor_exit_log::raw_now_ns(),
            )
        } else {
            None
        };
        Self::materialize_layer_blink(ws, std::time::Instant::now());
        // Flush any pending chrome-geometry change before painting. A title
        // edit materialises a chrome strip (see `tab_bar_h`), so opening or
        // closing one changes the content rectangle and the PTYs must follow.
        // Doing it here rather than at each transition coalesces the
        // close-then-open pair that every modal opener performs: the child sees
        // one resize to the final geometry, or none at all when the geometry is
        // unchanged end to end, because `try_resize_geometry` already skips a
        // no-op.
        // Consume the flag only when the resize can happen. `resize_all`
        // returns early while the renderer is absent (GPU recovery), so
        // taking the flag then would drop the geometry change and the PTYs
        // would never hear of it. Leaving it set defers the resize to the
        // first frame after the renderer is rebuilt.
        if ws.pending_resize && ws.renderer.is_some() {
            ws.pending_resize = false;
            self.resize_all(ws);
        }
        // Keep the OS titlebar in step with the active palette.
        // Compare-only when nothing changed; independent of GPU health, so it
        // runs before the device-lost early-return below.
        self.maybe_sync_native_theme(ws);
        self.maybe_sync_color_scheme(ws);
        if let (Some(material), Some(window)) = (&ws.native_material, &ws.window) {
            material.sync(window, &self.cfg);
        }
        // If the GPU device was lost (a driver TDR/reset) or hit an uncaptured
        // error (VRAM exhaustion under memory pressure), rendering against it
        // cannot succeed. The wgpu error handlers installed by
        // `kettle_render::install_gpu_error_handlers` turn the fault into a
        // logged event and this flag instead of a `panic=abort` crash. Stop
        // painting so the surface-reconfigure loop does not spin;
        // about_to_wait_inner drives renderer recovery on a bounded backoff.
        if self.gpu.as_ref().is_some_and(|g| g.is_lost()) {
            let title = self.ui_text.text(kettle_i18n::Text::WindowTitleGpuLost);
            if ws.last_title != title {
                if let Some(w) = &ws.window {
                    w.set_title(title);
                }
                ws.last_title = title.to_string();
            }
            // Nothing is painted on the dead device, but pending PTY output
            // must still be marked seen and the output pacer reset. Otherwise
            // `window_has_new_output` stays true forever and a streaming pane
            // spins the event loop at 30-60 Hz during a TDR, burning CPU and
            // battery. With both cleared, the loop quiesces to
            // ControlFlow::Wait between output bursts. (`about_to_wait_inner`
            // also short-circuits animation wakes while lost.)
            ws.output_pacer.reset();
            ws.seen_output_gen.clear();
            for (id, p) in &ws.mux.panes {
                ws.seen_output_gen.insert(*id, p.term.output_generation());
            }
            return;
        }
        // Scheduled theme transitions are time-driven rather than terminal
        // output. Keep this lock-free maintenance outside the snapshot-reuse
        // branch so a stream of menu-hover frames cannot defer a boundary
        // crossing indefinitely.
        self.poll_theme_schedule(ws);
        // B (Peacock): resolve/refresh this window's accent claim (cheap in
        // the steady state; full pool walk only on first frame/theme switch).
        self.sync_window_accent(ws);
        // A context-menu hover changes only one highlight quad. When no output,
        // pane order, or grid dimensions changed, keep the prior render
        // snapshots and bypass every terminal-locking maintenance pass below.
        // The one-shot hint is invalidated by any intervening input/user event.
        let reuse_pane_snapshots = std::mem::take(&mut ws.reuse_pane_snapshots_once)
            && ws.context_menu.is_some()
            && context_menu_snapshot_reuse_safe(ws)
            && self.torn_drag.is_none()
            && self.pane_snapshots_match_current_layout(ws);
        if !prepare_output_frame(ws, reuse_pane_snapshots, |ws| {
            if !reuse_pane_snapshots {
                self.drain_events(ws);
                self.poll_remote_contexts(ws);
                self.poll_focus_event(ws);
                self.poll_title_event(ws);
                if ws.ime_preedit.is_some() || modal_editor(ws).is_some() {
                    self.update_ime_cursor_area(ws);
                }
                // Reflect the focused pane's OSC 9;4 progress onto the OS
                // taskbar button (pwsh 7 / Windows Terminal parity). No-op off Windows.
                self.poll_taskbar_progress(ws);
                // Process restart requests only after the pane iteration releases
                // its mutable borrow. Lifecycle-only wakes use this same helper in
                // about-to-wait, so hidden windows do not strand a Restart action.
                self.process_pending_pane_restarts(ws);
                // Reflect the active pane (incl. after tab/focus switches).
                self.sync_window_title(ws);
                // Capture a just-exited pane's final output before reap drops
                // its sidechannel — otherwise the shell's last line is lost from the trace.
                self.flush_recorder_output(ws);
                let reaped = ws.mux.reap();
                if reaped.layout_changed {
                    ws.dragging_split = None;
                }
                self.fire_closed_pane_events(ws);
                // `clear_stale_title_edit` mutates `ws.editing_title`, so it is
                // evaluated into a binding rather than placed on the right of the
                // `||` where a reaped layout would short-circuit past it and
                // strand a title edit pinned to a pane that no longer exists.
                let stale_title = clear_stale_title_edit(&ws.mux, &mut ws.editing_title);
                if reaped.layout_changed || stale_title {
                    ws.pending_resize = true;
                    if let Some(window) = &ws.window {
                        window.request_redraw();
                    }
                }
                self.hoover_groups(ws);
                if reaped.mux_empty {
                    return false;
                }
                ws.search_queries
                    .retain(|pane_id, _| ws.mux.panes.contains_key(pane_id));
                // Track output without locking every terminal on every blink or
                // UI-only frame. The reader publishes `output_generation` after its
                // terminal mutation, so a changed generation is cheap and, unlike
                // `history_size()`, also catches in-place and alternate-screen
                // output. Only the opt-in scroll-on-output action takes a grid lock.
                let want_sob = self.cfg.scroll_on_output;
                // Track which panes produced output this frame so
                // we can latch their tab's `last_output_at`. Collected here
                // and dispatched after the borrow ends — same shape as the
                // `bell_panes` collection in `drain_events`.
                let mut output_panes: Vec<u64> = Vec::new();
                for (&pane_id, pane) in ws.mux.panes.iter_mut() {
                    let generation = pane.term.output_generation();
                    let advanced =
                        output_generation_advanced(pane.last_output_generation, generation);
                    if advanced {
                        output_panes.push(pane_id);
                    }
                    if want_sob
                        && advanced
                        && let Ok(mut t) = pane.term.term.lock()
                    {
                        t.scroll_display(Scroll::Bottom);
                    }
                    pane.last_output_generation = Some(generation);
                }
                for id in output_panes {
                    ws.mux.touch_tab_output(id);
                }
                // Auto-scroll while a selection stays at or beyond the focused
                // pane's top or bottom edge.
                // Pure `selection_autoscroll_lines` chooses the per-frame rate;
                // scrolling the viewport here naturally re-fires `update_selection`
                // below to anchor the selection's end to the new visible line.
                if ws.selecting {
                    let area = self.area(ws);
                    if let Some(pane_id) = ws.selecting_pane
                        && let Some(rect) = self.pane_terminal(ws, area, pane_id)
                    {
                        let scale = ws
                            .window
                            .as_ref()
                            .map_or(1.0, |window| window.scale_factor() as f32);
                        let lines = if ws.selection_autoscroll_edge != 0 {
                            i32::from(ws.selection_autoscroll_edge)
                        } else {
                            selection_autoscroll_lines(
                                ws.cursor.y as f32,
                                rect.1,
                                rect.1 + rect.3,
                                scale,
                                ws.selection_dragged,
                            )
                        };
                        if lines != 0
                            && let Some(p) = ws.mux.panes.get(&pane_id)
                            && let Ok(mut t) = p.term.term.lock()
                        {
                            t.scroll_display(Scroll::Delta(lines));
                        }
                        if lines != 0 {
                            // Re-anchor the selection's end at the (now-moved)
                            // cursor row so the highlight grows in step with the
                            // scroll, not stuck on the original click-time row.
                            self.update_selection(ws, area);
                        }
                    } else {
                        // The owner may still exist in an inactive tab. Preserve
                        // copy-on-select before invalidating the gesture.
                        self.finish_selection_gesture(ws);
                    }
                }
                self.update_search(ws);
                self.update_links(ws);
                // Link set may have changed (scroll, output, mode flip) — re-sync
                // the cursor icon so a URL scrolling out from under a held Ctrl
                // doesn't leave the pointer-hand icon stuck on a now-empty cell.
                // Deduped via `last_cursor_icon` so this is a cheap per-frame
                // recheck when nothing changed.
                self.sync_cursor_icon(ws);
            }
            true
        }) {
            return;
        }
        let overlay = self.overlay(ws, reuse_pane_snapshots);
        // Search and completion state are semantic accessibility state. Reuse
        // the exact projection this frame will draw instead of rebuilding the
        // full overlay twice for the accessibility key and tree. Snapshot reuse
        // skips pane maintenance, not semantic publication: a pending update
        // must not be stranded by a run of context-menu hover frames.
        self.publish_accessibility_if_due(ws, &overlay);
        let area = self.area(ws);
        let tabbar = self.tab_bar(ws);
        // Build status bar BEFORE the &mut renderer borrow
        // since `build_status_bar` reads ws.mux / self.cfg
        // immutably.
        let status = self.build_status_bar(ws);
        let layout = self.pane_layout(ws, area);
        let focus = ws.mux.active_focus();
        let window = ws.window.clone();

        // A failed acquire/render owns the next GPU attempt until its one-shot
        // deadline is polled in about_to_wait. This gate also stops unrelated
        // cursor/input/output redraw requests from bypassing the backoff. Do
        // the non-GPU state maintenance above, but neither snapshot terminal
        // contents nor touch an invisible surface except for one explicit
        // offscreen capture. That exception may bypass a transient retry, never
        // a pending renderer rebuild.
        let screenshot_may_render_offscreen = ws.renderer.as_ref().is_some_and(|renderer| {
            ws.window.as_ref().is_some_and(|window| {
                pending_screenshot_may_render_offscreen(
                    renderer.has_pending_screenshot(),
                    window.is_minimized(),
                    ws.window_shown,
                    window.is_visible(),
                )
            })
        });
        let recovery_blocks_render = ws.frame_recovery.renderer_rebuild_pending()
            || (ws.frame_recovery.has_pending() && !screenshot_may_render_offscreen);
        if !ws.dpi_resize.render_allowed()
            || recovery_blocks_render
            || (window_is_render_hidden(ws) && !screenshot_may_render_offscreen)
        {
            return;
        }

        if ws.renderer.is_none() {
            return;
        }

        // Re-open each pane's output latch only when this window is about to
        // sample generations for a real frame. Keeping the latch closed across
        // the entire deferred frame budget collapses a fast reader's many
        // chunks into one event-loop wake without losing output that races the
        // snapshot below. This must follow every early-return guard: entering
        // Presenting without attempting a frame would strand the pacer and
        // schedule a near-zero recovery loop.
        self.acknowledge_output_wakes(ws);
        // Is this paint flushing coalesced PTY output? Retained until the
        // frame outcome so the flood detector advances only after
        // presentation (see `effective_output_budget`).
        let was_coalescing_paint = ws.output_pacer.begin_frame();

        // Capture each visible pane's renderable state into a pooled snapshot
        // under the Term lock, then drop the guard at once (a microsecond-scale
        // flat copy per pane). The renderer works from the snapshots, so the
        // GPU frame (shaping, surface acquire, submit, present; milliseconds)
        // never holds a Term mutex. Holding them across the frame starves the
        // PTY parser on `term.lock()` under output flood. Also pass the pane's
        // title so the per-pane titlebar can render it.
        let mut snaps = std::mem::take(&mut ws.pane_snapshots);
        let mut snapshot_keys = std::mem::take(&mut ws.pane_snapshot_keys);
        if !reuse_pane_snapshots {
            snapshot_keys.clear();
        }
        let mut metas = Vec::with_capacity(layout.len());
        let home = crate::mux::home_dir_string();
        for (id, r) in &layout {
            if let Some(p) = ws.mux.panes.get(id) {
                // Each protocol registry has its own defensive cap. Interleave
                // all three before renderer-side visibility culling so a busy
                // class cannot hide the others, while offscreen entries do not
                // consume the smaller frame-wide draw quota.
                let per_class_limit = kettle_core::GraphicsLimits::default().placements;
                let imgs = round_robin_bounded(
                    [
                        p.term.placements(),
                        p.term.placeholder_tiles(),
                        p.term.relative_tiles(),
                    ],
                    per_class_limit.saturating_mul(3),
                );
                let si = metas.len();
                let snapshot_ready = if reuse_pane_snapshots {
                    snaps.get(si).is_some()
                } else {
                    let output_generation = ws.pending_output_gen.get(id).copied().unwrap_or(0);
                    let Ok(g) = p.term.term.lock() else {
                        continue;
                    };
                    if snaps.len() <= si {
                        snaps.push(PaneSnapshot::default());
                    }
                    snaps[si].capture_with_card_marks(&g, p.inline_cards.needs_marks());
                    drop(g); // lock released — the render below is lock-free
                    snapshot_keys.push((
                        *id,
                        output_generation,
                        snaps[si].columns,
                        snaps[si].screen_lines,
                    ));
                    true
                };
                if snapshot_ready {
                    let cwd = p.term.current_dir_or_native();
                    let mut title_parts = pane_title_parts(
                        &self.cfg.agent_badge,
                        p.agent_attached,
                        p.read_only,
                        &p.title,
                        p.title_is_placeholder,
                        cwd.as_deref(),
                        home.as_deref(),
                    );
                    title_parts
                        .prefix
                        .push_str(&media_shelf_badge(p.media_shelf.unseen()));
                    metas.push((
                        *id,
                        *r,
                        Some(*id) == focus,
                        imgs,
                        title_parts.prefix,
                        title_parts.title,
                        title_parts.path,
                        snaps[si].columns as u16,
                        snaps[si].screen_lines as u16,
                        // Terminator parity (`icon_bell`): the pane's own bell
                        // latch drives its titlebar bell indicator.
                        p.bell,
                        ws.bell_flashes
                            .get(id)
                            .map_or(0.0, |rang| bell_flash_ramp(rang.elapsed())),
                        p.group_name.clone(),
                    ));
                }
            }
        }
        snaps.truncate(metas.len());
        snapshot_keys.truncate(metas.len());
        // Hand the renderer borrows into `metas` (which lives
        // for the whole frame) instead of a second per-pane clone of the images
        // Vec / title String / group_name. `snap` borrows the pooled snapshot
        // the same way; both outlive `panes`, which drops before the pool
        // returns to `ws.pane_snapshots`.
        // An on-to-off edge the scheduler marked hands the blink to the window
        // server (macOS) if this frame still draws the off phase with nothing
        // holding the cursor. Only a successful main present lets the renderer
        // draw the patch into the layer's surface afterward.
        let blink_handoff = ws.blink_layer.take_pending_handoff()
            && self.cfg.macos_cursor_blink_layer
            && !overlay.cursor_visible
            && overlay.window_focused
            && overlay.ime_preedit.is_none()
            && Self::pane_cursor_layer_eligible(ws)
            && Self::ensure_cursor_layer(ws);
        let panes: Vec<PaneView> = metas
            .iter()
            .zip(snaps.iter())
            .map(
                |(
                    (id, r, f, imgs, prefix, title, path, cols, rows, bell, bell_flash, group_name),
                    snap,
                )| {
                    PaneView {
                        id: *id,
                        rect: r.leaf,
                        terminal: r.terminal,
                        titlebar: r.titlebar,
                        snap,
                        focused: *f,
                        images: imgs.as_slice(),
                        inline_cards: ws.mux.panes.get(id).map(|pane| &pane.inline_cards),
                        tr: self.ui_text,
                        title_prefix: prefix.as_str(),
                        title: title.as_str(),
                        title_path: path.as_deref(),
                        size_cols: *cols,
                        size_rows: *rows,
                        bell: *bell,
                        bell_flash: *bell_flash,
                        group_name: group_name.as_deref(),
                    }
                },
            )
            .collect();
        let live_opacity_floor = ws
            .native_material
            .as_ref()
            .and_then(|material| material.live_opacity_floor());
        // Whether anything covers the cards, for the tip: no dialog, menu or
        // viewer over them.
        let cards_uncovered = !self.pointer_modal_open(ws);
        // Status bar and native fallback state are built BEFORE the &mut
        // renderer borrow (the helpers read other window state immutably).
        let Some(renderer) = ws.renderer.as_mut() else {
            drop(panes);
            ws.pane_snapshots = snaps;
            ws.pane_snapshot_keys = snapshot_keys;
            ws.output_pacer.presentation_failed();
            return;
        };
        // AppKit clips a decorated window's bottom corners after the Metal
        // scene is drawn. Tell the renderer to replace the square pane-border
        // strips at those two outer corners with one concentric antialiased
        // outline. Borderless/fullscreen windows have square content surfaces;
        // every non-macOS platform deliberately stays on the existing path.
        // Use live native state: AppKit's green button bypasses our actions,
        // and a config reload cannot retrofit an existing window's decorations.
        let live_fullscreen = window
            .as_ref()
            .is_some_and(|window| window.fullscreen().is_some());
        let live_decorated = window.as_ref().is_some_and(|window| window.is_decorated());
        renderer.set_rounded_window_corners(rounded_window_corner_policy(
            cfg!(target_os = "macos"),
            live_decorated,
            live_fullscreen,
        ));
        renderer.set_live_background_opacity_floor(live_opacity_floor);
        // Any frame while the window server blinks the cursor ends that blink.
        // The frame presents in one Core Animation transaction with hiding the
        // layer, so the screen never shows new content under a stale cursor,
        // or the old frame with no cursor.
        let layer_exit = ws.blink_layer.is_active();
        let exit_started = std::time::Instant::now();
        let mut layer_exit_presented = false;
        if layer_exit && let Some(layer) = &ws.cursor_layer {
            layer.begin_exit_frame();
        }
        if let Some(window) = window.as_ref() {
            let window = Arc::downgrade(window);
            renderer.ensure_background_redraw(move || {
                if let Some(window) = window.upgrade() {
                    window.request_redraw();
                }
            });
        }
        let frame_started = std::time::Instant::now();
        let frame_result = renderer.render_frame_with_status_and_pre_present(
            &panes,
            &tabbar,
            &self.cfg,
            &overlay,
            &status,
            || {
                if let Some(window) = &window {
                    window.pre_present_notify();
                }
            },
        );
        let frame_time = frame_started.elapsed();
        // A card has been seen once a frame showing it reached the screen.
        let presented = matches!(&frame_result, Ok(FrameOutcome::Presented));
        if presented
            && ws
                .card_sightings
                .note(renderer.painted_cards(), std::time::Instant::now())
        {
            // Assistive technology names each card on screen, and the
            // pointer may rest on one that moved or arrived.
            ws.accessibility_pending = true;
            ws.card_hover_stale = true;
        }
        if presented && overlay.update_toast.is_some() {
            self.update_toast_presented(ws.seq);
        }
        // The first card a user ever sees says a click opens it, once its
        // image is on screen with nothing over it: a card that loads in
        // place, or that a dialog or the viewer uncovers, qualifies then.
        let tip_begun = if presented && cards_uncovered {
            self.cards_tip.begin(
                renderer.shown_cards().map(|card| (card.pane, card.nonce)),
                std::time::Instant::now(),
            )
        } else {
            None
        };
        // C1 records only a presented scene. Reuse that scene immediately,
        // before any renderer setter can invalidate it, and with the same cfg.
        let cursor_patch = if blink_handoff && matches!(&frame_result, Ok(FrameOutcome::Presented))
        {
            Some(renderer.present_cursor_patch(&self.cfg))
        } else {
            None
        };
        // Return the snapshot pool (cell-Vec capacity recycles next frame).
        drop(panes);
        if let Some((pane, nonce)) = tip_begun
            && let Some(state) = ws.mux.panes.get_mut(&pane)
        {
            state.inline_cards.set_tip(Some(nonce));
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        ws.pane_snapshots = snaps;
        ws.pane_snapshot_keys = snapshot_keys;
        match frame_result {
            Ok(outcome) => {
                let presented = apply_output_generation_outcome(
                    outcome,
                    &mut ws.seen_output_gen,
                    &mut ws.pending_output_gen,
                );
                match outcome {
                    FrameOutcome::Presented => {
                        debug_assert!(presented);
                        // Commit the rest of the frame transaction only after
                        // presentation. Updating the output generation before
                        // presenting would lose terminal damage whenever
                        // surface acquisition times out.
                        ws.frame_recovery.presented();

                        // Fallback reveal: normal startup reveals as soon as
                        // renderer init succeeds, then paints immediately.
                        // Keep this guard so any future path that reaches a
                        // rendered frame while still hidden cannot leave a
                        // visible-state window invisible forever. No-op after
                        // the first reveal and for `window_state = hidden`.
                        if !ws.window_shown {
                            if let Some(w) = &ws.window {
                                w.set_visible(true);
                            }
                            ws.window_shown = true;
                            if ws.startup_window {
                                crate::startup_trace::mark(
                                    crate::startup_trace::Phase::WindowRevealed,
                                );
                            }
                            log::info!(
                                "startup: window revealed at first paint, working set {:.1} MiB",
                                process_working_set_mb()
                            );
                        }
                        // The first frame is the first one the startup window
                        // presents: a frame that timed out or found the window
                        // occluded is not.
                        if ws.startup_window {
                            crate::startup_trace::mark(crate::startup_trace::Phase::FirstFrame);
                            crate::startup_trace::flush();
                        }
                        ws.last_paint = Some(std::time::Instant::now());
                        ws.output_pacer.presented();
                        // Track sustained-flood depth. A frame that flushed
                        // coalesced output bumps the counter; any other paint
                        // resets it. Failed presentation does neither because
                        // no output was actually flushed to the display.
                        ws.flood_paints = if was_coalescing_paint {
                            ws.flood_paints.saturating_add(1)
                        } else {
                            0
                        };
                        ws.blink_layer.note_presented(std::time::Instant::now());
                        if layer_exit {
                            // Commits with this frame's present in
                            // `end_exit_frame` below.
                            if let Some(layer) = &ws.cursor_layer {
                                layer.hide();
                            }
                            layer_exit_presented = true;
                        } else if blink_handoff {
                            if let Some(patch) = cursor_patch {
                                self.start_layer_blink(ws, patch, window.as_deref());
                            } else {
                                ws.blink_layer.failed_handoff();
                            }
                        }
                    }
                    FrameOutcome::RetryLater => {
                        debug_assert!(!presented);
                        ws.output_pacer.presentation_failed();
                        // A backend acquire timeout may already have blocked
                        // for close to a second; Outdated has just
                        // reconfigured the existing surface. In either case,
                        // retain damage and let the one-shot backoff schedule
                        // the next acquire.
                        ws.frame_recovery
                            .schedule_transient_retry(std::time::Instant::now());
                    }
                    FrameOutcome::Occluded => {
                        debug_assert!(!presented);
                        ws.output_pacer.presentation_failed();
                        // The wgpu result can precede winit's OS occlusion
                        // event. Keep a slow self-healing deadline for a
                        // spurious/transient result, while the state machine
                        // becomes fully quiescent as soon as the window system
                        // confirms hidden/minimized/occluded state.
                        ws.frame_recovery
                            .schedule_occluded_retry(std::time::Instant::now());
                    }
                    FrameOutcome::SurfaceLost => {
                        debug_assert!(!presented);
                        ws.output_pacer.presentation_failed();
                        // wgpu 30 requires Instance::create_surface here.
                        // Rebuild only this renderer on the healthy shared
                        // device; genuine device loss is handled separately by
                        // the process-wide recovery guard.
                        ws.frame_recovery
                            .schedule_renderer_rebuild(std::time::Instant::now());
                    }
                }
            }
            Err(e) => {
                ws.output_pacer.presentation_failed();
                log::warn!("render error: {e}");
                // Fallible glyph preparation can advance retained renderer
                // damage keys before returning (for example AtlasFull). A
                // plain redraw could therefore reuse inconsistent caches and
                // strand terminal damage. Rebuild the affected renderer on a
                // bounded schedule unless the device callback already routed
                // this failure into process-wide recovery.
                if !self.gpu.as_ref().is_some_and(|gpu| gpu.is_lost()) {
                    ws.frame_recovery
                        .schedule_renderer_rebuild(std::time::Instant::now());
                }
            }
        }
        if presented {
            defer_output_racing_frame(ws);
        }
        // A frame that did not present leaves the layer blinking over the
        // off-phase frame beneath it, which is still what the screen shows.
        if layer_exit && let Some(layer) = &ws.cursor_layer {
            layer.end_exit_frame();
        }
        let cursor_exit_end = if cursor_exit_frame.is_some() && layer_exit_presented {
            crate::cursor_exit_log::raw_now_ns()
        } else {
            None
        };
        if layer_exit_presented {
            let exit_time = exit_started.elapsed();
            ws.blink_layer.exited(exit_time);
            if ws.cursor_exit_log.enabled {
                if let Some(line) = ws.cursor_exit_log.finish_frame(
                    cursor_exit_frame,
                    layer_exit_presented,
                    cursor_exit_end,
                ) {
                    crate::cursor_exit_log::emit_line(&line);
                }
            } else {
                log::info!(target: CURSOR_BLINK_LOG, "exit_frame_us={} render_frame_us={}",
                    exit_time.as_micros(), frame_time.as_micros());
            }
            if !self.cfg.macos_cursor_blink_layer {
                Self::retire_cursor_layer(ws);
            }
        }
    }

    pub(super) fn request_all_window_redraws(&self, ws: &WindowState) {
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        for (&seq, window) in &self.windows {
            if seq != ws.seq
                && let Some(native) = &window.window
            {
                native.request_redraw();
            }
        }
    }
}
