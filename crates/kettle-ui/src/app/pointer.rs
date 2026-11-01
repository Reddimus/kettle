//! Mouse reporting to programs, the wheel, scrollbars and text selection.

use super::*;

/// A selection point from the pointer, kept on the pane's grid. The pointer
/// can sit past the last column, over a neighboring split; the point stays on
/// the last column, taking its right half, so a drag past the edge selects
/// through it. Then converted to grid-absolute, as `viewport_point_to_grid`.
fn selection_point_to_grid(
    mut viewport: kettle_core::Point,
    mut side: kettle_core::Side,
    dimensions: &impl Dimensions,
    display_offset: usize,
) -> (kettle_core::Point, kettle_core::Side) {
    if viewport.column > dimensions.last_column() {
        viewport.column = dimensions.last_column();
        side = kettle_core::Side::Right;
    }
    viewport.line.0 = viewport.line.0.clamp(0, dimensions.bottommost_line().0);
    (viewport_point_to_grid(viewport, display_offset), side)
}

impl App {
    pub(super) fn press_pane_drag(&mut self, ws: &mut WindowState, button: u8) -> bool {
        if button != 0 {
            return false;
        }
        let Some(pane) = self.pane_at_titlebar_click(ws, ws.cursor.x as f32, ws.cursor.y as f32)
        else {
            return false;
        };
        let pre = self.focus_key(ws);
        arm_pane_drag(ws, pane);
        self.note_focus_change(ws, pre);
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        true
    }

    pub(super) fn drag_pane(&self, ws: &mut WindowState) -> bool {
        if ws.pane_drag.is_none() {
            return false;
        }
        let area = self.area(ws);
        if update_pane_drag(ws, area)
            && let Some(window) = &ws.window
        {
            window.request_redraw();
        }
        true
    }

    pub(super) fn release_pane_drag(
        &mut self,
        ws: &mut WindowState,
        button: u8,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        if button != 0 {
            return false;
        }
        let Some((moved, rename)) = finish_pane_drag(ws) else {
            return false;
        };
        if moved {
            self.resize_all(ws);
            self.save_session(ws);
        } else if rename {
            self.handle_action(ws, Action::EditPaneTitle, event_loop);
        }
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        true
    }

    /// Clear the focused pane's selection (called when the user types —
    /// every modern terminal does this so a stale highlight doesn't
    /// confuse the next copy/paste). No-op when nothing is selected.
    pub(super) fn clear_selection_on_input(&mut self, ws: &mut WindowState) {
        if let Some(p) = ws.mux.focused()
            && let Ok(mut t) = p.term.term.lock()
            && t.selection.is_some()
        {
            t.selection = None;
        }
    }

    /// Terminal pane addressed by a wheel event. Pointer ownership is
    /// independent of keyboard focus: a split under the cursor wins, while
    /// chrome/gaps/out-of-window coordinates fall back to the focused pane.
    /// Modal, tab-bar and Ctrl-zoom handling run before this helper is used.
    pub(super) fn wheel_target(&self, ws: &WindowState) -> Option<(u64, Rect)> {
        // The pane under the pointer is found by its whole leaf; the wheel
        // then scrolls that pane's terminal.
        let area = self.area(ws);
        let layout = self.pane_layout(ws, area);
        let leaves: Vec<(u64, Rect)> = layout
            .iter()
            .map(|(id, partition)| (*id, partition.leaf))
            .collect();
        let (pane, _) = wheel_target_at(
            &leaves,
            ws.mux.active_focus(),
            ws.cursor.x as f32,
            ws.cursor.y as f32,
        )?;
        layout
            .into_iter()
            .find(|(id, _)| *id == pane)
            .map(|(id, partition)| (id, partition.terminal))
    }

    /// Start a scrollbar drag and return the pointer's offset inside the thumb.
    pub(super) fn scrollbar_jump(
        &mut self,
        ws: &mut WindowState,
        area: Rect,
        px: f32,
        py: f32,
    ) -> Option<f32> {
        self.scrollbar_at(ws, area, px, py, true, None)
    }

    /// Map a pointer position to a viewport jump on the focused pane's
    /// scrollbar. With `require_zone`, only the DPI-scaled invisible hit strip
    /// counts. During a drag X is ignored and `grab_offset` keeps the pointer at
    /// the same position inside the thumb instead of snapping to its top edge.
    pub(super) fn scrollbar_at(
        &mut self,
        ws: &mut WindowState,
        area: Rect,
        px: f32,
        py: f32,
        require_zone: bool,
        grab_offset: Option<f32>,
    ) -> Option<f32> {
        if self.cfg.scrollbar == kettle_config::ScrollbarMode::Never {
            return None;
        }
        let (rx, ry, rw, rh) = self.focused_terminal(ws, area)?;
        let scale = ws
            .window
            .as_ref()
            .map(|window| window.scale_factor() as f32)
            .unwrap_or(1.0);
        let painted = self.cfg.scrollbar_width.clamp(2.0, 40.0) * scale;
        let zone = painted.max(12.0 * scale);
        if require_zone && (px < rx + rw - zone || px > rx + rw || py < ry || py > ry + rh) {
            return None;
        }
        let p = ws.mux.focused()?;
        let Ok(mut t) = p.term.term.lock() else {
            return None;
        };
        use kettle_core::Dimensions;
        let g = t.grid();
        let (rows, hist, off) = (g.screen_lines(), g.history_size(), g.display_offset());
        // Interactable whenever there is something to scroll. `Auto` paints the
        // bar whenever history is present (not only while scrolled back), so a
        // click anywhere on the gutter jumps the viewport.
        if rows + hist <= rows {
            return None;
        }
        let (thumb_y, thumb_h) =
            kettle_core::scrollbar::thumb_with_min(rows, hist, off, rh, 24.0 * scale)?;
        let pointer_y = py - ry;
        let grab = grab_offset.unwrap_or_else(|| {
            if (thumb_y..=thumb_y + thumb_h).contains(&pointer_y) {
                pointer_y - thumb_y
            } else {
                thumb_h / 2.0
            }
        });
        let grab = grab.clamp(0.0, thumb_h);
        let target =
            kettle_core::scrollbar::target_offset_for_drag(pointer_y, grab, rh, thumb_h, hist);
        let delta = target as i32 - off as i32;
        if delta != 0 {
            t.scroll_display(kettle_core::Scroll::Delta(delta));
        }
        Some(grab)
    }

    /// Whether `(px, py)` is within the focused pane's scrollbar grab
    /// zone (right edge, painted width floored at 12 logical px). Pure geometry
    /// so it is cheap to call on every `CursorMoved` to drive the
    /// overlay scrollbar's hover-brighten without affecting the scroll state.
    pub(super) fn scrollbar_zone_contains(
        &self,
        ws: &WindowState,
        area: Rect,
        px: f32,
        py: f32,
    ) -> bool {
        if self.cfg.scrollbar == kettle_config::ScrollbarMode::Never {
            return false;
        }
        let Some((rx, ry, rw, rh)) = self.focused_terminal(ws, area) else {
            return false;
        };
        let scale = ws
            .window
            .as_ref()
            .map(|window| window.scale_factor() as f32)
            .unwrap_or(1.0);
        let zone = (self.cfg.scrollbar_width.clamp(2.0, 40.0) * scale).max(12.0 * scale);
        px >= rx + rw - zone && px <= rx + rw && py >= ry && py <= ry + rh
    }

    pub(super) fn px_to_point(
        &self,
        ws: &WindowState,
        rect: Rect,
        px: f32,
        py: f32,
    ) -> (kettle_core::Point, kettle_core::Side) {
        let (cw, ch) = ws
            .renderer
            .as_ref()
            .map(|r| (r.cell_w, r.cell_h))
            .unwrap_or((8.0, 16.0));
        // `rect` is the pane's terminal, clear of its titlebar and lane, so
        // the grid starts inside its padding, as the renderer draws it.
        let (col, line, side) = px_to_cell(
            px,
            py,
            rect,
            (cw, ch),
            (self.cfg.padding_x, self.cfg.padding_y),
        );
        (
            kettle_core::Point::new(kettle_core::Line(line), kettle_core::Column(col)),
            side,
        )
    }

    pub(super) fn smart_selection_for_cell(
        &self,
        ws: &WindowState,
        row: usize,
        col: usize,
    ) -> Option<(usize, usize)> {
        use kettle_core::Dimensions;
        let pane = ws.mux.panes.get(&ws.mux.active_focus()?)?;
        let term = pane.term.term.lock().ok()?;
        if row >= term.screen_lines() {
            return None;
        }
        let point = viewport_point_to_grid(
            kettle_core::Point::new(kettle_core::Line(row as i32), kettle_core::Column(col)),
            term.grid().display_offset(),
        );
        kettle_core::hints::selection_at(term.grid(), point.line.0, col)
    }

    /// Install a `Simple` selection from `(row, start)` to
    /// `(row, end)` inclusive in the focused pane. Returns true if the
    /// selection was set, false if there's no focused pane or the lock
    /// failed (in which case the caller falls through to its normal
    /// `begin_selection` path).
    pub(super) fn apply_smart_selection(
        &mut self,
        ws: &mut WindowState,
        area: Rect,
        row: usize,
        start: usize,
        end: usize,
    ) -> bool {
        // `area` is unused but kept in the signature to mirror
        // `begin_selection`'s API; future viewport-aware variants may
        // need it for clamping.
        let _ = area;
        clear_selection_gesture(ws);
        let Some(pane) = ws.mux.focused() else {
            return false;
        };
        let Ok(mut t) = pane.term.term.lock() else {
            return false;
        };
        // The click `row` is viewport-relative; convert both ends to
        // grid-absolute so a double-click word-select while scrolled back
        // selects (and copies) the row the user actually clicked.
        let off = t.grid().display_offset();
        let anchor = viewport_point_to_grid(
            kettle_core::Point::new(kettle_core::Line(row as i32), kettle_core::Column(start)),
            off,
        );
        let end_pt = viewport_point_to_grid(
            kettle_core::Point::new(kettle_core::Line(row as i32), kettle_core::Column(end)),
            off,
        );
        let mut sel = kettle_core::Selection::new(
            kettle_core::SelectionType::Simple,
            anchor,
            kettle_core::Side::Left,
        );
        sel.update(end_pt, kettle_core::Side::Right);
        t.selection = Some(sel);
        // Like Semantic, a smart selection resolves on press; the
        // caller treats `selecting=false` so motion doesn't extend it.
        true
    }

    pub(super) fn begin_selection(
        &mut self,
        ws: &mut WindowState,
        area: Rect,
        ty: kettle_core::SelectionType,
    ) {
        let dragging = matches!(
            ty,
            kettle_core::SelectionType::Simple | kettle_core::SelectionType::Block
        );
        let Some(pane_id) = ws.mux.active_focus() else {
            clear_selection_gesture(ws);
            return;
        };
        let Some(rect) = self.pane_terminal(ws, area, pane_id) else {
            clear_selection_gesture(ws);
            return;
        };
        let mut installed = false;
        {
            let (vp, side) = self.px_to_point(ws, rect, ws.cursor.x as f32, ws.cursor.y as f32);
            if let Some(pane) = ws.mux.panes.get(&pane_id)
                && let Ok(mut t) = pane.term.term.lock()
            {
                // Store the grid-absolute point (viewport − display_offset)
                // so a selection started while scrolled back anchors on the
                // history row the user sees, not the active screen.
                let (p, side) = selection_point_to_grid(vp, side, &*t, t.grid().display_offset());
                // The anchor carries the pointer's sub-cell side so a Simple/Block
                // drag trims the start cell when the press lands in its right half
                // (Semantic/Lines ignore the side — they snap to token bounds).
                t.selection = Some(kettle_core::Selection::new(ty, p, side));
                installed = true;
            }
        }
        if installed && dragging {
            arm_selection_gesture(ws, pane_id, 0);
        } else {
            clear_selection_gesture(ws);
        }
    }

    /// Click count for the press at `(row,col)` within ~400 ms of the last.
    pub(super) fn click_count(ws: &mut WindowState, row: usize, col: usize) -> u8 {
        let now = std::time::Instant::now();
        let pane = ws.mux.active_focus();
        let n = match ws.last_click {
            Some((t, p, r, c, n))
                if p == pane
                    && r == row
                    && c == col
                    && now.duration_since(t) < std::time::Duration::from_millis(400) =>
            {
                n % 3 + 1
            }
            _ => 1,
        };
        ws.last_click = Some((now, pane, row, col, n));
        n
    }

    pub(super) fn copy_selection(&mut self, ws: &mut WindowState) {
        let target = ws.selecting_pane.or_else(|| ws.mux.active_focus());
        let sel = target
            .and_then(|id| ws.mux.panes.get(&id))
            .and_then(|p| {
                p.term
                    .term
                    .lock()
                    .ok()
                    .and_then(|t| t.selection_to_string())
            })
            .filter(|s| !s.is_empty());
        let sel = match copy_clipboard_decision(sel, true) {
            Ok(selection) => selection,
            Err(error) => {
                log::warn!("selection copy: could not prepare text: {error}");
                return;
            }
        };
        if let (Some(s), Some(cb)) = (sel, self.clipboard.as_mut()) {
            // On Linux also write the X11 PRIMARY selection that `paste_primary`
            // reads, so the select→middle-click-paste loop works. No PRIMARY on
            // Wayland/macOS/Windows; those keep clipboard-only.
            #[cfg(target_os = "linux")]
            {
                use arboard::{LinuxClipboardKind, SetExtLinux};
                if let Err(e) = cb
                    .set()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text(s.clone())
                {
                    log::warn!("clipboard set PRIMARY failed (selection copy): {e}");
                }
            }
            // Log clipboard failures so a broken clipboard is diagnosable,
            // matching the vi-mode yank path.
            if let Err(e) = cb.set_text(s) {
                log::warn!("clipboard set_text failed (selection copy): {e}");
            }
        }
    }

    /// Commit copy-on-select before another UI surface takes pointer ownership,
    /// then disarm every selection gesture latch. The ordering matters because
    /// `copy_selection` uses `selecting_pane` to preserve the pane the drag
    /// started in rather than whichever pane gained focus meanwhile.
    pub(super) fn finish_selection_gesture(&mut self, ws: &mut WindowState) {
        if self.cfg.copy_on_select
            && selection_copy_owner(ws.selecting, ws.selecting_pane).is_some()
        {
            self.copy_selection(ws);
        }
        clear_selection_gesture(ws);
    }

    pub(super) fn update_selection(&mut self, ws: &mut WindowState, area: Rect) {
        if !ws.selecting {
            return;
        }
        let Some(pane_id) = ws.selecting_pane else {
            self.finish_selection_gesture(ws);
            return;
        };
        let Some(rect) = self.pane_terminal(ws, area, pane_id) else {
            // A tab switch can make the owning pane temporarily absent from
            // the active layout while its selection is still available in
            // the mux. Commit copy-on-select before dropping that ownership.
            self.finish_selection_gesture(ws);
            return;
        };
        let (vp, side) = self.px_to_point(ws, rect, ws.cursor.x as f32, ws.cursor.y as f32);
        let Some(pane) = ws.mux.panes.get(&pane_id) else {
            // A removed pane has nothing left to copy, but using the shared
            // transition keeps invalidation ordered with every other exit.
            self.finish_selection_gesture(ws);
            return;
        };
        if let Ok(mut t) = pane.term.term.lock() {
            // Convert to grid-absolute before the mutable `selection`
            // borrow so the dragged end-point tracks scrollback too.
            let (p, side) = selection_point_to_grid(vp, side, &*t, t.grid().display_offset());
            if let Some(sel) = t.selection.as_mut() {
                // The drag end carries the pointer's sub-cell side so the
                // boundary cell is included only once the pointer crosses its
                // midpoint (xterm/Alacritty parity) — not always.
                sel.update(p, side);
            }
        }
    }

    pub(super) fn begin_or_extend_mouse_selection(&mut self, ws: &mut WindowState, area: Rect) {
        // The grid press is the newer gesture: drop any editor selection so
        // the search bar's Copy shortcut reaches the grid selection instead.
        if ws.search.open {
            ws.search.editor.clear_selection();
        }
        if ws.mods.shift_key() && !ws.mods.alt_key() && self.extend_selection_to_cursor(ws, area, 0)
        {
            return;
        }
        let cell = self.cursor_cell(ws);
        let clicks = cell.map(|(r, c)| Self::click_count(ws, r, c)).unwrap_or(1);
        let kind = selection_kind(clicks, ws.mods.alt_key());
        let mut smart_selected = false;
        if clicks == 2
            && !ws.mods.alt_key()
            && let Some((row, col)) = cell
            && let Some((start, end)) = self.smart_selection_for_cell(ws, row, col)
            && self.apply_smart_selection(ws, area, row, start, end)
        {
            smart_selected = true;
        }
        if !smart_selected {
            self.begin_selection(ws, area, kind);
        }
        if self.cfg.copy_on_select
            && (smart_selected
                || matches!(
                    kind,
                    kettle_core::SelectionType::Semantic | kettle_core::SelectionType::Lines
                ))
        {
            self.copy_selection(ws);
        }
    }

    /// Extend the existing selection to the cursor point (Shift+Click).
    /// Returns `true` when a selection was present *and* extended — the
    /// caller falls back to a fresh `begin_selection` when no selection
    /// existed (so Shift+Click on empty space starts a normal selection).
    /// Matches xterm / Alacritty / iTerm2: Shift+Click anchors the
    /// existing selection's start and pulls the end to the click.
    /// Where the pointer is against the focused pane's selection: `None`
    /// when nothing is selected (the empty selection a click leaves is
    /// nothing), else whether the pointer is on it.
    pub(super) fn pointer_on_selection(&self, ws: &WindowState, area: Rect) -> Option<bool> {
        let rect = self.focused_terminal(ws, area)?;
        let (vp, _) = self.px_to_point(ws, rect, ws.cursor.x as f32, ws.cursor.y as f32);
        let pane = ws.mux.active_focus().and_then(|id| ws.mux.panes.get(&id))?;
        let term = pane.term.term.lock().ok()?;
        let point = viewport_point_to_grid(vp, term.grid().display_offset());
        let range = term.selection.as_ref()?.to_range(&term)?;
        Some(range.contains(point))
    }

    pub(super) fn extend_selection_to_cursor(
        &mut self,
        ws: &mut WindowState,
        area: Rect,
        button: u8,
    ) -> bool {
        let rect = match self.focused_terminal(ws, area) {
            Some(r) => r,
            None => return false,
        };
        let (vp, side) = self.px_to_point(ws, rect, ws.cursor.x as f32, ws.cursor.y as f32);
        if let Some(pane_id) = ws.mux.active_focus() {
            let extended = if let Some(pane) = ws.mux.panes.get(&pane_id)
                && let Ok(mut t) = pane.term.term.lock()
            {
                // Grid-absolute end-point so Shift+Click extends to the
                // right history row while scrolled back.
                let (p, side) = selection_point_to_grid(vp, side, &*t, t.grid().display_offset());
                if let Some(sel) = t.selection.as_mut() {
                    // Sub-cell side so Shift+Click lands the boundary on the
                    // same half-cell rule as a drag.
                    sel.update(p, side);
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if extended {
                // Enter drag mode so a follow-up mouse-move keeps extending —
                // matches every Mac/Linux text-control: shift-click, then drag.
                arm_selection_gesture(ws, pane_id, button);
                return true;
            }
        }
        false
    }

    /// Forward a mouse event to the app via the active tracking protocol.
    /// Returns `true` when it was consumed (so kettle skips local handling).
    /// Whether a mouse event at `cur` should be reported to a tracking app,
    /// given the `last` reported cell. Motion (1002/1003) coalesces to cell
    /// crossings; a press/release (`motion == false`) always reports. Pure so
    /// the coalescing rule is unit-tested without a live PTY.
    pub(super) fn motion_should_report(
        motion: bool,
        last: Option<(usize, usize)>,
        cur: (usize, usize),
    ) -> bool {
        !(motion && last == Some(cur))
    }

    pub(super) fn send_mouse(
        &mut self,
        ws: &mut WindowState,
        btn: u8,
        pressed: bool,
        motion: bool,
    ) -> bool {
        self.report_mouse(ws, btn, pressed, motion).consumed()
    }

    /// Send a mouse event to the terminal it belongs to and say what it did.
    /// A press goes to the focused pane, and a program that gets one keeps
    /// it: that button's drag and its one release go to the same pane,
    /// wherever focus has gone since, so no program is left holding half a
    /// click and no other gets a release it never had the press for. The
    /// newest press owns the drag, so a gesture of Kettle's own started
    /// since takes it. Hover goes to the focused pane.
    pub(super) fn report_mouse(
        &mut self,
        ws: &mut WindowState,
        btn: u8,
        pressed: bool,
        motion: bool,
    ) -> MouseReport {
        let held = match (pressed, motion) {
            (false, _) => match ws.held_buttons.take(btn) {
                Some(held) if held.pane.is_some() => Some(held),
                // No program had its press: Kettle's own gesture did, or
                // nothing did, so no program gets its release.
                _ => return MouseReport::Declined,
            },
            (true, true) => ws.held_buttons.dragging(),
            (true, false) => None,
        };
        if let Some(held) = held {
            let Some(pane) = held.pane else {
                return MouseReport::Declined;
            };
            let rect = self.pane_terminal(ws, self.area(ws), pane);
            let at = MouseAt::Held {
                rect,
                cell: held.cell,
            };
            let button = if motion { held.button } else { btn };
            return self.send_mouse_to(ws, pane, at, button, pressed, motion);
        }
        let Some(pane_id) = ws.mux.active_focus() else {
            return MouseReport::Declined;
        };
        let Some(rect) = self.focused_terminal(ws, self.area(ws)) else {
            return MouseReport::Declined;
        };
        // Motion over a lane is the lane's: only a drag the program's own
        // press began follows the pointer there (above).
        if motion && self.preview_lane_under_pointer(ws) {
            return MouseReport::Declined;
        }
        let report = self.send_mouse_to(ws, pane_id, MouseAt::Pointer(rect), btn, pressed, motion);
        if pressed && !motion {
            // The press is the button's gesture now: the program's if it got
            // it, else Kettle's own.
            let (pane, cell) = match (report, ws.last_mouse_cell) {
                (MouseReport::Written, Some((pane, row, col))) => (Some(pane), (row, col)),
                _ => (None, (0, 0)),
            };
            if let Some(stale) = ws.held_buttons.pressed(btn, pane, cell) {
                self.release_held(ws, stale);
            }
        }
        report
    }

    /// Send each held button `which` names its release now, in the pane its
    /// press went to, at the cell that pane last saw. Kettle is ending the
    /// gesture (focus left the window, a menu or search took the pointer,
    /// the pane's tab is leaving, or the button was pressed again after a
    /// release that went elsewhere), and the program must not be left
    /// holding a press. Kettle's own gestures just end.
    pub(super) fn release_held_buttons(
        &mut self,
        ws: &mut WindowState,
        which: impl FnMut(&crate::held_buttons::Held) -> bool,
    ) {
        for held in ws.held_buttons.take_where(which) {
            self.release_held(ws, held);
        }
    }

    /// Send `held`'s program its release at the cell it last saw.
    pub(super) fn release_held(&mut self, ws: &mut WindowState, held: crate::held_buttons::Held) {
        let Some(pane) = held.pane else {
            return;
        };
        let at = MouseAt::Held {
            rect: None,
            cell: held.cell,
        };
        self.send_mouse_to(ws, pane, at, held.button, false, false);
    }

    /// Release the held buttons of every pane in tab `idx` before it leaves
    /// this window, whose events can no longer reach it.
    pub(super) fn release_held_buttons_in_tab(&mut self, ws: &mut WindowState, idx: usize) {
        let panes = ws.mux.tab_pane_ids(idx);
        self.release_held_buttons(ws, |held| {
            held.pane.is_some_and(|pane| panes.contains(&pane))
        });
    }

    pub(super) fn send_mouse_to(
        &mut self,
        ws: &mut WindowState,
        pane_id: u64,
        at: MouseAt,
        btn: u8,
        pressed: bool,
        motion: bool,
    ) -> MouseReport {
        if motion && self.pointer_modal_open(ws) {
            return MouseReport::Swallowed;
        }
        let held = matches!(at, MouseAt::Held { .. });
        // A held button's pane that left the screen gets its release, but
        // no drag: the pointer is nowhere over it.
        if matches!(at, MouseAt::Held { rect: None, .. }) && pressed {
            return MouseReport::Swallowed;
        }
        self.sync_pane_focus_reports(ws);
        if search_bar_blocks_mouse_report(
            self.cursor_in_search_bar(ws),
            pressed,
            motion,
            ws.held_buttons.any(),
        ) {
            return MouseReport::Swallowed;
        }
        // Shift bypasses mouse tracking so kettle handles the event locally
        // (the xterm convention). Without it, a TUI in mouse mode (htop, vim,
        // tmux) consumes every click and the user cannot select text.
        // Returning `false` lets the caller run selection, scrollbar, and
        // extend logic as if tracking were off. A button whose press the
        // program got keeps its motion and release there, Shift or not.
        if ws.mods.shift_key() && !held {
            return MouseReport::Declined;
        }
        // VTE input-enabled parity: a read-only pane gets
        // no mouse-tracking reports either. Returning false falls through to
        // kettle-local handling, so selection / scrollback still work for the
        // user — the same degradation VTE applies when input is disabled.
        if ws.mux.panes.get(&pane_id).is_some_and(|p| p.read_only) {
            return MouseReport::Declined;
        }
        let mode = self
            .pane_mode(ws, pane_id)
            .unwrap_or(kettle_core::TermMode::empty());
        let (track, sgr) = input::mouse_tracking(mode);
        if track == input::MouseTracking::Off {
            return MouseReport::Declined;
        }
        // `motion_is_reported` owns the per-mode rule; see its comment for why
        // it is stated positively.
        if motion && !input::motion_is_reported(track, held) {
            {
                // A held button means the application owns the gesture even
                // though this mode does not report the motion itself, so
                // consume it. With no button held the pointer is only
                // hovering, and kettle's own hover, scrollbar-drag and
                // link-hover handling must still run.
                return if held {
                    MouseReport::Swallowed
                } else {
                    MouseReport::Declined
                };
            }
        }
        let (row, col) = match at {
            MouseAt::Pointer(rect)
            | MouseAt::Held {
                rect: Some(rect), ..
            } => self.cursor_cell_in_rect(ws, rect),
            MouseAt::Held { rect: None, cell } => cell,
        };
        // Cell-motion coalescing: a drag that stays inside one cell must not
        // re-report. xterm fires a 1002/1003 motion event only when the
        // pointer crosses into a new cell; without this a fast drag emits one
        // SGR report per pixel of travel, flooding the TUI.
        // Press/release always report (the guard is motion-only) and refresh
        // the baseline, so the next motion is compared against the right cell.
        let last = ws
            .last_mouse_cell
            .and_then(|(pane, row, col)| (pane == pane_id).then_some((row, col)));
        if !Self::motion_should_report(motion, last, (row, col)) {
            return MouseReport::Swallowed;
        }
        let seq = input::mouse_encode(sgr, btn, pressed, motion, col, row, ws.mods);
        let Some(pane) = ws.mux.panes.get(&pane_id) else {
            return MouseReport::Declined;
        };
        // A release takes the room the program's input keeps for releases,
        // so it always lands behind its press.
        let result = if pressed || motion {
            pane.feed_pointer_input(&seq)
        } else {
            pane.feed_mouse_release(&seq)
        };
        Self::dismiss_media_paste_receipt_after_input(ws, pane_id, result);
        if !result.is_queued() {
            // The program never got it: a press it missed leaves no button
            // held, so no release follows.
            self.report_input_result(result);
            return MouseReport::Swallowed;
        }
        ws.last_mouse_cell = Some((pane_id, row, col));
        ws.held_buttons.told(pane_id, (row, col));
        MouseReport::Written
    }

    /// The shared wheel ladder, downstream of quantization.
    ///
    /// Both the real winit `MouseWheel` event and the ctl/MCP `send_mouse`
    /// injections land here so the two cannot drift apart, and automation
    /// exercises the same terminal the user does.
    ///
    /// `steps.notches` drives discrete consumers (one action per physical
    /// detent, independent of `scroll-multiplier`); `steps.lines` drives
    /// continuous ones. Returns whether the wheel was consumed.
    pub(super) fn dispatch_wheel(
        &mut self,
        ws: &mut WindowState,
        steps: input::WheelSteps,
    ) -> bool {
        self.sync_pane_focus_reports(ws);
        if self.confirm_wheel(ws, steps.lines) {
            return true;
        }
        // Wheel over an open context menu scrolls its rows, one row per notch
        // (Terminator menu UX). It pre-empts every other wheel dispatch so a
        // 512-entry Theme submenu scrolls instead of leaking through to the
        // pane, tab bar, or font zoom. Swallowed even mid-detent, so a slow
        // touchpad gesture can't leak past the menu while its residue builds.
        if ws.context_menu.is_some() {
            if steps.notches != 0 {
                // Wheel up = notches > 0 = scroll up = decrement
                // offset; wheel down = notches < 0 = scroll down.
                self.scroll_context_menu(ws, -(steps.notches as isize));
            }
            return true;
        }
        if self.picker_pointer(ws, PickerPointer::Wheel(-(steps.notches as isize))) {
            return true;
        }
        // A wheel over a lane is the lane's: it pans a rendered item or
        // scrolls a source, and reaches nothing behind it.
        if self.lane_takes_wheel(ws) {
            self.wheel_steps_over_lane(ws, steps);
            return true;
        }
        // Wheel over a settings field adjusts it (up = forward, down = backward).
        // A wheel outside the panel does not dismiss it; it falls through to the
        // modal swallow below. One field step per detent, so a touchpad can't
        // blow through a 512-entry Theme list.
        if ws.settings_nav.is_some()
            && steps.notches != 0
            && self.settings_mouse(ws, steps.notches.signum(), false)
        {
            return true;
        }
        // Search owns the wheel, but uses it for local scrollback
        // browsing so users can inspect nearby historical matches.
        // Never encode tracking/alternate-scroll bytes into the TUI
        // behind the lane.
        if ws.search.open {
            if steps.lines != 0 {
                self.scroll_search_viewport(ws, steps.lines);
            }
            return true;
        }
        // Any other open modal swallows the wheel too, so Ctrl+wheel can't zoom
        // the font and a plain or Shift wheel can't scroll the pane or cycle tabs
        // behind it. The context menu consumed its wheel above, so it is `None`
        // here and `modal_swallows_pointer` reduces to "any modal open".
        if modal_swallows_pointer(self.any_modal_open(ws), ws.context_menu.is_some()) {
            return true;
        }
        // Wheel over the tab bar cycles tabs (kitty / iTerm2 /
        // Ghostty parity). Each detent moves one
        // tab regardless of `scroll-multiplier` so the gesture
        // stays predictable — multiple lines from a fast scroll
        // collapse to a single tab change, like the real apps.
        //
        // A horizontal swipe over the bar cycles too: the strip has no
        // independent scroll offset (`tab_strip_layout` derives its window from
        // the ACTIVE tab), so moving the selection IS how the bar scrolls.
        // Positive = "backwards" on both axes, matching winit's documented
        // convention that +Y scrolls up and +X scrolls left.
        if self.cursor_in_tab_bar(ws) && ws.mux.tabs.len() > 1 {
            let step = if steps.notches != 0 {
                steps.notches
            } else {
                steps.cols
            };
            if step != 0 {
                if step > 0 {
                    ws.mux.prev_tab();
                } else {
                    ws.mux.next_tab();
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            return true;
        }
        // Terminator parity: Ctrl+wheel resizes the font. It runs BEFORE the
        // mouse-tracking pass-through so it works even when a TUI like tmux or
        // htop has mouse tracking on, as in gnome-terminal, Terminator, and
        // xterm. `cfg.disable_mousewheel_zoom = true` opts out for users who
        // scroll-zoom by accident on a touchpad. The step size matches the
        // keyboard IncreaseFontSize / DecreaseFontSize actions.
        let zoom_owns_wheel = ws.mods.control_key() && !self.cfg.disable_mousewheel_zoom;
        if let Some(sign) = should_zoom_font(
            ws.mods.control_key(),
            steps.notches,
            self.cfg.disable_mousewheel_zoom,
        ) && let Some(r) = ws.renderer.as_mut()
        {
            // Step logical size, not the now-physical
            // cell_h (which would double-apply the DPI scale).
            let new = if sign > 0 {
                r.font_size() + 1.0
            } else {
                r.font_size() - 1.0
            };
            r.set_font_size(new);
            self.resize_all(ws);
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
            return true;
        }
        if zoom_owns_wheel {
            // Ctrl is held and zoom is enabled, but this event was only a
            // fraction of a detent. Swallow it rather than letting a
            // mid-gesture Ctrl+wheel scroll the pane instead of zooming it.
            return true;
        }
        let Some((wheel_pane, wheel_rect)) = self.wheel_target(ws) else {
            return true;
        };
        // Shift+wheel always scrolls the kettle scrollback even
        // when a TUI has mouse-tracking on (xterm convention).
        // Without this bypass, you can't scroll back through
        // your tmux/htop session — the TUI swallows every wheel
        // notch.
        let mode = self
            .pane_mode(ws, wheel_pane)
            .unwrap_or(kettle_core::TermMode::empty());
        let (track, _) = input::mouse_tracking(mode);
        let track_active = track != input::MouseTracking::Off && !ws.mods.shift_key();
        if track_active {
            // Horizontal wheel reports as xterm buttons 6/7 (encoded 66/67)
            // alongside the vertical 64/65. `mouse_encode` passes any
            // `btn >= 64` through unchanged, so no encoder change is needed.
            let mut reported = false;
            if steps.lines != 0 {
                let btn = if steps.lines > 0 { 64 } else { 65 };
                for _ in 0..steps.lines.unsigned_abs().min(8) {
                    reported |= self
                        .send_mouse_to(
                            ws,
                            wheel_pane,
                            MouseAt::Pointer(wheel_rect),
                            btn,
                            true,
                            false,
                        )
                        .consumed();
                }
            }
            if steps.cols != 0 {
                let btn = if steps.cols > 0 { 66 } else { 67 };
                for _ in 0..steps.cols.unsigned_abs().min(8) {
                    reported |= self
                        .send_mouse_to(
                            ws,
                            wheel_pane,
                            MouseAt::Pointer(wheel_rect),
                            btn,
                            true,
                            false,
                        )
                        .consumed();
                }
            }
            if reported {
                return true;
            }
            // `send_mouse_to` declined every report because the pane is read-only
            // (or Shift re-entered mid-gesture). Fall through to local scrollback
            // so a read-only pane running a mouse-tracking TUI still scrolls.
        } else if !ws.mods.shift_key()
            && steps.lines != 0
            && let Some(bytes) = input::alternate_scroll_key(steps.lines, mode)
        {
            let result = ws
                .mux
                .panes
                .get(&wheel_pane)
                .map(|pane| pane.feed_pointer_input(&bytes))
                .unwrap_or(PaneInputResult::Queued);
            self.report_input_result(result);
            Self::dismiss_media_paste_receipt_after_input(ws, wheel_pane, result);
            if result != PaneInputResult::ReadOnly {
                return true;
            }
        }
        if steps.lines != 0 {
            if let Some(pane) = ws.mux.panes.get(&wheel_pane)
                && let Ok(mut t) = pane.term.term.lock()
            {
                t.scroll_display(Scroll::Delta(steps.lines));
            }
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
        true
    }
}

#[cfg(test)]
mod selection_point_tests {
    use super::*;

    struct Grid(usize, usize);

    impl Dimensions for Grid {
        fn total_lines(&self) -> usize {
            self.1
        }
        fn screen_lines(&self) -> usize {
            self.1
        }
        fn columns(&self) -> usize {
            self.0
        }
    }

    /// A drag past the right edge, over a neighboring split, stays on the
    /// last column and takes its right half.
    #[test]
    fn a_point_past_the_last_column_stays_on_the_grid() {
        use kettle_core::{Column, Line, Point, Side};
        let grid = Grid(80, 24);
        for side in [Side::Left, Side::Right] {
            assert_eq!(
                selection_point_to_grid(Point::new(Line(6), Column(81)), side, &grid, 0),
                (Point::new(Line(6), Column(79)), Side::Right)
            );
        }
        assert_eq!(
            selection_point_to_grid(Point::new(Line(30), Column(3)), Side::Left, &grid, 0),
            (Point::new(Line(23), Column(3)), Side::Left)
        );
    }
}

/// Live state for a split-divider mouse drag — the addressed
/// split (`path`) and its orientation. The split's rect is re-fetched from
/// `split_seams` on every move (keyed by `path`) so a layout change mid-drag
/// (another pane closing) can't desync the ratio math.
pub(crate) struct SplitDrag {
    pub(super) path: Vec<bool>,
    pub(super) dir: Dir,
}

/// Terminator parity: live state for dragging a terminal to another position
/// inside its tab, armed by a left-press on that pane's own titlebar.
///
/// The press is deliberately ambiguous until the pointer moves. A titlebar
/// click means "focus this pane" and a second click on the focused pane means
/// "edit its title"; both must keep working. So the press records what it
/// *would* do, `live` records whether movement has since turned it into a drag,
/// and the RELEASE picks between the two. Opening the title editor on press
/// would put an editor over the pane the user just started dragging.
pub(crate) struct PaneDrag {
    /// The pane being moved.
    pub(crate) pane: u64,
    /// Surface position of the press, the origin the slop radius is measured
    /// from.
    pub(crate) origin: (f32, f32),
    /// Movement has cleared the slop radius: this is a drag, not a click.
    pub(crate) live: bool,
    /// A release that never became a drag opens the title editor. True only
    /// when the press landed on the ALREADY-focused pane: a press that merely
    /// moved focus is not by itself a request to rename anything.
    pub(crate) rename_on_click: bool,
    /// Latched drop target while `live`: the pane under the cursor, the half of
    /// it the drop would take, and the `(dir, before)` pair that describes that
    /// half to `Mux::move_pane_beside`. `None` while the cursor is over no pane
    /// — over a seam, or outside the content area.
    pub(crate) target: Option<(u64, Rect, Dir, bool)>,
}

/// Grab tolerance (px) for the thin split divider line, so
/// it's easy to hit with the mouse without pixel-perfect aim.
const SPLIT_SEAM_TOL: f32 = 5.0;

/// Per-pane titlebar hit test (Terminator parity): returns the id of the pane
/// whose titlebar band contains the click, or `None`. Pure so the hit test
/// behind `App::pane_at_titlebar_click` can be drift-guarded.
#[allow(clippy::type_complexity)]
pub(crate) fn pane_titlebar_hit(
    px: f32,
    py: f32,
    pane_rects: &[(u64, (f32, f32, f32, f32))],
    title_at_bottom: bool,
    bar_h: f32,
) -> Option<u64> {
    for (id, (rx, ry, rw, rh)) in pane_rects {
        let (bar_top, bar_bot) = if title_at_bottom {
            (*ry + *rh - bar_h, *ry + *rh)
        } else {
            (*ry + 1.0, *ry + 1.0 + bar_h)
        };
        if px >= *rx && px < *rx + *rw && py >= bar_top && py < bar_bot {
            return Some(*id);
        }
    }
    None
}

/// SGR mouse base code for the extra "side" mouse buttons, or `None` for any
/// button kettle handles locally (left/middle/right) or doesn't forward.
///
/// Forwarding these lets a 5-button mouse's Back / Forward reach a
/// mouse-tracking TUI (tmux/vim bindings, pagers). xterm encodes buttons 8–11
/// as `128 + (button - 8)`; winit's `Back` is XBUTTON1 (button 8 → 128) and
/// `Forward` is XBUTTON2 (button 9 → 129). These have no local UI meaning, so
/// they only do anything while mouse tracking is on.
pub(super) fn extra_mouse_sgr(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Back => Some(MOUSE_BACK),
        MouseButton::Forward => Some(MOUSE_FORWARD),
        _ => None,
    }
}

/// The report code of xterm's button 8, the Back button.
pub(super) const MOUSE_BACK: u8 = 128;
/// The report code of xterm's button 9, the Forward button.
pub(super) const MOUSE_FORWARD: u8 = 129;

/// What a mouse event did on its way to a pane's terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MouseReport {
    /// A report went to the terminal.
    Written,
    /// Kettle kept the event from the terminal, and nothing else handles it.
    Swallowed,
    /// No report was due; Kettle handles the event itself.
    Declined,
}

impl MouseReport {
    /// Whether nothing else should handle the event.
    pub(super) fn consumed(self) -> bool {
        self != Self::Declined
    }
}

/// Where a mouse report lands in its pane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum MouseAt {
    /// The pointer's cell in the pane's rect.
    Pointer(Rect),
    /// The drag or release of a held button whose press the pane's program
    /// got: the pointer's cell in its rect while it is on screen, else the
    /// cell it was last told of.
    Held {
        rect: Option<Rect>,
        cell: (usize, usize),
    },
}

/// Pure pointer → `(col, line, side)` math for a pane, shared by `px_to_point` so
/// the per-pane-titlebar inset is drift-tested.
///
/// The renderer and hit testing share [`kettle_render::pane_grid_origin`] (a top
/// titlebar moves row zero down; a bottom one leaves it at the pane padding), so
/// selection, link targeting, mouse reporting, and IME projection never drift a
/// row from the drawn grid. Col/line clamp to ≥ 0 so a click in the
/// chrome/padding doesn't underflow.
///
/// `side` is which half of the hit cell the pointer sits in, derived from the
/// sub-cell x offset: the left half is `Side::Left`, the right half (and the
/// exact midpoint) is `Side::Right`. This matches xterm / Alacritty / iTerm2 and
/// is exactly what alacritty's `Selection::to_range` (`range_simple`/`range_block`)
/// needs to decide whether each boundary cell is included. Without it a drag is
/// biased one cell wide. The side is computed from the SAME clamped non-negative
/// offset as `col`, so a pointer left of the content origin maps to
/// `(col 0, Side::Left)` and the first cell is included, never trimmed. NOTE:
/// only Simple/Block *drag* selections consume this side; word / line /
/// smart-select snap to token boundaries and ignore it (see `begin_selection` /
/// `apply_smart_selection`).
pub(super) fn px_to_cell(
    px: f32,
    py: f32,
    terminal: Rect,
    cell: (f32, f32),
    pad: (f32, f32),
) -> (usize, i32, kettle_core::Side) {
    let (cw, ch) = cell;
    let (ox, oy) = (terminal.0 + pad.0, terminal.1 + pad.1);
    // Derive BOTH col and side from the same non-negative offset so they agree at
    // the left clamp: a pointer in the left padding (offset < 0) maps to
    // (col 0, Side::Left) — the first cell is included, not trimmed. Computing the
    // side from the raw (negative) offset via `rem_euclid` would wrap it into the
    // cell's right half and wrongly drop column 0 from a drag.
    let offx = (px - ox).max(0.0);
    let col = (offx / cw).floor() as usize;
    let line = ((py - oy) / ch).floor().max(0.0) as i32;
    let side = if offx.rem_euclid(cw) < cw / 2.0 {
        kettle_core::Side::Left
    } else {
        kettle_core::Side::Right
    };
    (col, line, side)
}

/// Map a VIEWPORT-relative point (line 0 = top visible row, what `px_to_point`
/// returns) to the GRID-ABSOLUTE point that alacritty's `Selection` /
/// `selection_to_string` / `to_range` and `grid[..]` indexing expect, by
/// subtracting the focused pane's `display_offset` via alacritty's own
/// `viewport_to_point`. Without it, a selection or grid-row read taken while
/// scrolled into history addresses the active-screen row instead of the
/// scrolled-to row, so the copy returns wrong or empty text and the highlight
/// slips down by the scroll amount. The bug is invisible at the bottom, where
/// `display_offset == 0` makes viewport and absolute coincide. Pure
/// (drift-tested in `viewport_point_to_grid_applies_display_offset`).
pub(super) fn viewport_point_to_grid(
    viewport: kettle_core::Point,
    display_offset: usize,
) -> kettle_core::Point {
    let line = viewport.line.0.max(0) as usize;
    kettle_core::viewport_to_point(
        display_offset,
        kettle_core::Point::new(line, viewport.column),
    )
}

/// Grid-absolute `(top-left, bottom-right)` points spanning the whole buffer —
/// scrollback included — for keyboard "select to top/bottom" and "select all".
/// Alacritty's convention: the active screen is `Line(0)..Line(screen_lines)` and
/// history is *negative* down to `Line(-history_size)`, so the oldest line is
/// `Line(-(history_size))` col 0 and the last cell is `Line(screen_lines-1)` at the
/// final column. Pure so the boundary math is unit-testable without a GPU/Term.
pub(super) fn selection_buffer_bounds(
    history_size: usize,
    screen_lines: usize,
    columns: usize,
) -> (kettle_core::Point, kettle_core::Point) {
    let top = kettle_core::Point::new(
        kettle_core::Line(-(history_size as i32)),
        kettle_core::Column(0),
    );
    let bottom = kettle_core::Point::new(
        kettle_core::Line(screen_lines.saturating_sub(1) as i32),
        kettle_core::Column(columns.saturating_sub(1)),
    );
    (top, bottom)
}

/// Auto-scroll rate when a selection reaches or passes a pane's vertical edge.
/// A six-point inner drag zone keeps full-height panes scrollable even though
/// the pointer cannot move below their client area.
///
/// Speed scales with how far past the edge the cursor sits — a small
/// overshoot crawls (1 line/frame), a big one (40+ px) chases at 3 lines
/// per frame. Pure so the cadence is unit-tested without spinning up a
/// renderer or PTY.
pub(super) fn selection_autoscroll_lines(
    y: f32,
    rect_top: f32,
    rect_bottom: f32,
    scale_factor: f32,
    selection_dragged: bool,
) -> i32 {
    const EDGE_ZONE_PT: f32 = 6.0;

    let pane_height = rect_bottom - rect_top;
    if !selection_dragged || pane_height <= 0.0 {
        return 0;
    }
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let edge_zone = (EDGE_ZONE_PT * scale_factor).min(pane_height * 0.25);
    let (overshoot, direction) = if y < rect_top || y <= rect_top + edge_zone {
        ((rect_top - y).max(0.0), 1)
    } else if y > rect_bottom || y >= rect_bottom - edge_zone {
        ((y - rect_bottom).max(0.0), -1)
    } else {
        return 0;
    };
    // Magnitude → 1..=3 lines/frame ladder.
    let mag = if overshoot >= 40.0 {
        3
    } else if overshoot >= 10.0 {
        2
    } else {
        1
    };
    direction * mag
}

/// Whether pointer travel since the selection press is large enough to count
/// as a drag. Native backends may emit a cursor-moved event after mouse-down at
/// the same (or fractionally rounded) coordinate, so event arrival alone is
/// not evidence that the user dragged. The threshold is in logical points and
/// converted to physical pixels for the addressed window.
pub(super) fn selection_drag_motion_started(
    origin: Option<winit::dpi::PhysicalPosition<f64>>,
    cursor: winit::dpi::PhysicalPosition<f64>,
    scale_factor: f64,
) -> bool {
    const DRAG_START_PT: f64 = 2.0;

    let Some(origin) = origin else {
        return false;
    };
    if !(origin.x.is_finite()
        && origin.y.is_finite()
        && cursor.x.is_finite()
        && cursor.y.is_finite())
    {
        return false;
    }
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let threshold = DRAG_START_PT * scale_factor;
    let dx = cursor.x - origin.x;
    let dy = cursor.y - origin.y;
    dx * dx + dy * dy >= threshold * threshold
}

pub(super) fn note_selection_drag_motion(ws: &mut WindowState) {
    if ws.selection_dragged {
        return;
    }
    let scale_factor = ws
        .window
        .as_ref()
        .map_or(1.0, |window| window.scale_factor());
    ws.selection_dragged =
        selection_drag_motion_started(ws.selection_drag_origin, ws.cursor, scale_factor);
}

/// Which vertical edge an in-progress selection most likely crossed when the
/// backend reports `CursorLeft` without an out-of-client coordinate. Corners
/// resolve vertically on ties because horizontal selection overflow does not
/// scroll, while losing the vertical intent freezes the gesture entirely. A
/// window exit only latches after measured drag travel; otherwise a press on
/// the last client pixel plus sub-threshold outward jitter could scroll.
pub(super) fn selection_leave_edge(
    selection_dragged: bool,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> i8 {
    if !selection_dragged || !(x.is_finite() && y.is_finite() && width > 0.0 && height > 0.0) {
        return 0;
    }
    let top = y.max(0.0);
    let bottom = (height - y).max(0.0);
    let left = x.max(0.0);
    let right = (width - x).max(0.0);
    if top <= bottom && top <= left && top <= right {
        1
    } else if bottom <= top && bottom <= left && bottom <= right {
        -1
    } else {
        0
    }
}

pub(super) fn arm_selection_gesture(ws: &mut WindowState, pane_id: u64, button: u8) {
    // A fresh grid gesture owns Copy, including Shift+right-click extension.
    if ws.search.open {
        ws.search.editor.clear_selection();
    }
    ws.selecting = true;
    ws.selecting_pane = Some(pane_id);
    ws.selection_autoscroll_edge = 0;
    ws.selection_dragged = false;
    ws.selection_drag_origin = Some(ws.cursor);
    ws.selection_button = Some(button);
}

pub(super) fn selection_release_matches(owner: Option<u8>, button: u8) -> bool {
    owner == Some(button)
}

pub(super) fn selection_copy_owner(selecting: bool, pane_id: Option<u64>) -> Option<u64> {
    if selecting { pane_id } else { None }
}

pub(super) fn clear_selection_gesture(ws: &mut WindowState) {
    ws.selecting = false;
    ws.selecting_pane = None;
    ws.selection_autoscroll_edge = 0;
    ws.selection_dragged = false;
    ws.selection_drag_origin = None;
    ws.selection_button = None;
}

/// Select the terminal pane addressed by a wheel event without mutating
/// keyboard focus. A pane under the pointer wins; chrome, gaps and coordinates
/// outside the client area fall back to the focused pane.
pub(super) fn wheel_target_at(
    layout: &[(u64, Rect)],
    focused: Option<u64>,
    x: f32,
    y: f32,
) -> Option<(u64, Rect)> {
    layout
        .iter()
        .copied()
        .find(|&(_, (rx, ry, rw, rh))| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
        .or_else(|| {
            let focused = focused?;
            layout.iter().copied().find(|(pane, _)| *pane == focused)
        })
}

/// Map a click count + the Alt modifier to a selection type: double =
/// word, triple = line, single = a normal drag, and Alt+single =
/// rectangular/block selection (iTerm2/Alacritty/WezTerm parity).
pub(super) fn selection_kind(clicks: u8, alt: bool) -> kettle_core::SelectionType {
    use kettle_core::SelectionType::*;
    match clicks {
        2 => Semantic,
        3 => Lines,
        _ if alt => Block,
        _ => Simple,
    }
}

/// Pixel distance a held per-pane-titlebar press must move before it becomes a
/// pane drag rather than a click.
///
/// A flat constant, unlike [`tab_reorder_drag_threshold_px`], because the
/// competing gesture is different: a tab press competes with a reorder along a
/// strip whose height sets the natural scale, while a titlebar press competes
/// with "click again to rename". Renaming is the destructive-to-undo one, so the
/// slop is generous enough that ordinary hand jitter during a double click never
/// picks the pane up.
const PANE_DRAG_THRESHOLD_PX: f32 = 12.0;

pub(super) fn update_pane_drag(ws: &mut WindowState, area: Rect) -> bool {
    if ws.pane_drag.is_none() {
        return false;
    }
    let (cx, cy) = (ws.cursor.x as f32, ws.cursor.y as f32);
    let (ox, oy) = ws.pane_drag.as_ref().map(|d| d.origin).unwrap_or((cx, cy));
    let (dx, dy) = (cx - ox, cy - oy);
    let live = ws.pane_drag.as_ref().is_some_and(|d| d.live)
        || (dx * dx + dy * dy).sqrt() > PANE_DRAG_THRESHOLD_PX;
    let moving = ws.pane_drag.as_ref().map(|d| d.pane);
    // Computed before the &mut borrow below: `pane_rect_at`
    // needs `&ws.mux` and the write needs `&mut ws.pane_drag`.
    let target = if live {
        ws.mux.pane_rect_at(area, cx, cy).and_then(|(id, rect)| {
            // A pane cannot be dropped onto itself: the tree
            // half refuses it, so offering the hint would
            // promise a move that never happens.
            (Some(id) != moving)
                .then(|| {
                    crate::mux::pane_drop_zone(rect, cx, cy)
                        .map(|(dir, before)| (id, rect, dir, before))
                })
                .flatten()
        })
    } else {
        None
    };
    if let Some(drag) = ws.pane_drag.as_mut() {
        let changed = drag.live != live
            || drag.target.map(|t| t.0) != target.map(|t| t.0)
            || drag.target.map(|t| (t.2, t.3)) != target.map(|t| (t.2, t.3));
        drag.live = live;
        drag.target = target;
        return changed;
    }
    false
}

pub(super) fn arm_pane_drag(ws: &mut WindowState, pane: u64) {
    let rename_on_click = ws.mux.active_focus() == Some(pane);
    ws.mux.focus_pane(pane);
    ws.pane_drag = Some(PaneDrag {
        pane,
        origin: (ws.cursor.x as f32, ws.cursor.y as f32),
        live: false,
        rename_on_click,
        target: None,
    });
}

pub(super) fn finish_pane_drag(ws: &mut WindowState) -> Option<(bool, bool)> {
    let drag = ws.pane_drag.take()?;
    let moved = drag.live
        && drag.target.is_some_and(|(target, _, dir, before)| {
            ws.mux.move_pane_beside(drag.pane, target, dir, before)
        });
    Some((moved, !drag.live && drag.rename_on_click))
}

/// Should an open modal swallow a pointer event (mouse press / wheel)
/// instead of letting it fall through to the tab bar, pane focus, or
/// mouse-tracking *behind* the dialog? Otherwise a click could switch tabs or
/// focus a pane, and a wheel could zoom the font or scroll the pane, while a
/// dialog that seems to capture input sits on top. True whenever
/// `any_modal_open` is set, *except* for a lone context menu, which owns its
/// own click/scroll paths and is re-opened (relocated) by a right-click, so
/// gating it here would break that.
pub(super) fn modal_swallows_pointer(any_modal_open: bool, context_menu_open: bool) -> bool {
    any_modal_open && !context_menu_open
}

impl App {
    /// Hide the OS mouse cursor; idempotent. Called when the user starts
    /// typing if `mouse-hide-while-typing` is on. The cursor reappears on
    /// the next mouse move or window-enter event.
    pub(super) fn hide_mouse_cursor(&mut self, ws: &mut WindowState) {
        if ws.mouse_hidden || !self.cfg.mouse_hide_while_typing {
            return;
        }
        if let Some(w) = &ws.window {
            w.set_cursor_visible(false);
            ws.mouse_hidden = true;
        }
    }

    /// Show the OS mouse cursor; idempotent. Called whenever the mouse
    /// moves or re-enters the window.
    pub(super) fn show_mouse_cursor(&mut self, ws: &mut WindowState) {
        if !ws.mouse_hidden {
            return;
        }
        if let Some(w) = &ws.window {
            w.set_cursor_visible(true);
            ws.mouse_hidden = false;
        }
    }

    /// The resize cursor for a split of the given
    /// orientation. A Horizontal split places panes side-by-side (a vertical
    /// divider you drag left/right → column resize); a Vertical split stacks
    /// them (a horizontal divider you drag up/down → row resize).
    fn resize_cursor_for(dir: Dir) -> CursorIcon {
        match dir {
            Dir::Horizontal => CursorIcon::ColResize,
            Dir::Vertical => CursorIcon::RowResize,
        }
    }

    /// A SplitDrag for the divider seam under content-area pixel
    /// `(px, py)`, or None. Used to start a drag-to-resize on left-press.
    fn split_drag_at(&self, ws: &WindowState, area: Rect, px: f32, py: f32) -> Option<SplitDrag> {
        let seams = ws.mux.split_seams(ws.mux.active, area);
        let i = crate::mux::seam_at(&seams, px, py, SPLIT_SEAM_TOL)?;
        let s = &seams[i];
        Some(SplitDrag {
            path: s.path.clone(),
            dir: s.dir,
        })
    }

    pub(super) fn press_split_divider(&self, ws: &mut WindowState, button: u8) -> bool {
        if button != 0 || self.pointer_modal_open(ws) {
            return false;
        }
        let Some(drag) =
            self.split_drag_at(ws, self.area(ws), ws.cursor.x as f32, ws.cursor.y as f32)
        else {
            return false;
        };
        let dir = drag.dir;
        ws.dragging_split = Some(drag);
        if let Some(window) = &ws.window {
            window.set_cursor(Self::resize_cursor_for(dir));
            window.request_redraw();
        }
        true
    }

    pub(super) fn drag_split_divider(&mut self, ws: &mut WindowState) -> bool {
        if ws.dragging_split.is_none() {
            return false;
        }
        let (path, dir) = {
            let d = ws.dragging_split.as_ref().unwrap();
            (d.path.clone(), d.dir)
        };
        let area = self.area(ws);
        let seams = ws.mux.split_seams(ws.mux.active, area);
        match seams.iter().find(|s| s.path == path).map(|s| s.rect) {
            Some(rect) => {
                let ratio =
                    crate::mux::ratio_from_pos(rect, dir, ws.cursor.x as f32, ws.cursor.y as f32);
                ws.mux.set_split_ratio(ws.mux.active, &path, ratio);
                // The layout-tree
                // ratio changed, but each pane's PTY grid is resized
                // only by resize_all (the keyboard resize path reaches
                // it via handle_action's tail). Without this the child
                // TUIs keep their old cols/rows — rendering clipped —
                // until some unrelated event fires a resize.
                self.resize_all(ws);
                if let Some(w) = &ws.window {
                    w.set_cursor(Self::resize_cursor_for(dir));
                    w.request_redraw();
                }
            }
            None => ws.dragging_split = None,
        }
        true
    }

    /// The resize cursor to show while merely HOVERING a divider
    /// seam (no drag yet), or None when the cursor isn't over one. Suppressed
    /// while a modal owns the pointer. Cheap in the common single-pane case —
    /// `split_seams` returns empty when the tab root is a lone leaf.
    fn split_seam_hover_icon(&self, ws: &WindowState) -> Option<CursorIcon> {
        if self.any_modal_open(ws) {
            return None;
        }
        let area = self.area(ws);
        let seams = ws.mux.split_seams(ws.mux.active, area);
        let i = crate::mux::seam_at(
            &seams,
            ws.cursor.x as f32,
            ws.cursor.y as f32,
            SPLIT_SEAM_TOL,
        )?;
        Some(Self::resize_cursor_for(seams[i].dir))
    }

    /// Set the OS mouse-cursor icon, deduped against the last value pushed
    /// to the window. Called on CursorMoved (position changes the
    /// hit-test) and on ModifiersChanged (the modifier state gates the
    /// click-to-open affordance). The icon, and the hover state these hit
    /// tests set, are the physical pointer's: every hit test reads where it
    /// is, not where a control client last moved the cursor, and with the
    /// pointer outside the window there is nothing to set.
    pub(super) fn sync_cursor_icon(&mut self, ws: &mut WindowState) {
        let Some(pointer) = ws.native_pointer else {
            return;
        };
        let control = std::mem::replace(&mut ws.cursor, pointer);
        let media_receipt_hovered = self.media_paste_receipt_hovered(ws);
        self.sync_cursor_icon_with_media_receipt_hover(ws, media_receipt_hovered);
        ws.cursor = control;
    }

    /// CursorMoved already computed the receipt hit test to update its hover
    /// timer. Reuse that answer instead of building a second full overlay for
    /// the same physical event.
    pub(super) fn sync_cursor_icon_with_media_receipt_hover(
        &mut self,
        ws: &mut WindowState,
        media_receipt_action_hovered: bool,
    ) {
        // Browser / iTerm2 / Ghostty convention: the OS cursor turns into
        // a "pointing hand" while the user holds the same modifier that
        // would open a URL on click (Ctrl on Linux/Windows, Cmd on
        // macOS — winit's `super_key` maps Cmd) and the pointer sits on
        // a clickable link. Otherwise show the standard text-I-beam, the
        // affordance every modern terminal uses for "this surface accepts
        // mouse selection."
        //
        // Chrome surfaces (tab bar, open modal overlays) override that —
        // they're clickable, not selectable, so the I-beam there is
        // visually misleading. `chrome_cursor_icon` is the pure decision.
        //
        // Tab close-buttons (`✕`) override the chrome `Default` with
        // `Pointer` so the trailing glyph reads as a clickable
        // affordance rather than a character in the title. Recomputed
        // here every cursor-move so the chip-hover state in the
        // renderer stays in sync (the renderer reads
        // `tabbar.hovered_close_idx`, which we pass through in
        // `tab_bar()`).
        let bar = self.tab_bar(ws);
        let prior_hover = (
            ws.hovered_close_idx,
            ws.hovered_new_tab,
            ws.hovered_new_tab_menu,
        );
        ws.hovered_close_idx =
            hovered_close_button(&bar.segments, ws.cursor.x as f32, ws.cursor.y as f32);
        let (cursor_x, cursor_y) = (ws.cursor.x as f32, ws.cursor.y as f32);
        let new_tab_hover = new_tab_hover_target(&bar, cursor_x, cursor_y);
        ws.hovered_new_tab = new_tab_hover == Some(NewTabHoverTarget::Plus);
        ws.hovered_new_tab_menu = new_tab_hover == Some(NewTabHoverTarget::Menu);
        if prior_hover
            != (
                ws.hovered_close_idx,
                ws.hovered_new_tab,
                ws.hovered_new_tab_menu,
            )
            && let Some(window) = &ws.window
        {
            window.request_redraw();
        }
        let close_hover = tab_close_hover_icon(ws.hovered_close_idx.is_some());
        let new_tab_hover_icon =
            (ws.hovered_new_tab || ws.hovered_new_tab_menu).then_some(CursorIcon::Pointer);
        let confirm_hover = if self.confirm_button_hovered(ws) {
            Some(CursorIcon::Pointer)
        } else {
            None
        };
        let search_hover = self.search_geometry(ws).and_then(|geometry| {
            let (x, y) = (ws.cursor.x as f32, ws.cursor.y as f32);
            geometry
                .hit_test(x, y)
                .map(|control| {
                    if control == kettle_render::SearchControl::Editor {
                        CursorIcon::Text
                    } else {
                        CursorIcon::Pointer
                    }
                })
                .or_else(|| rect_contains(geometry.rect, x, y).then_some(CursorIcon::Default))
        });
        let media_receipt_hover = media_receipt_action_hovered.then_some(CursorIcon::Pointer);
        // A settled card under the pointer takes a click: the pointing hand.
        let card_hover = self.note_card_hover(ws).then_some(CursorIcon::Pointer);
        let chrome =
            chrome_cursor_icon(self.cursor_in_chrome_band(ws), self.pointer_modal_open(ws));
        // A live tab-drag owns the cursor, FIRST in the chain. Mid-drag the
        // pointer can briefly cross another tab's ✕ hit-zone or a split seam,
        // and without the priority the icon would flicker Pointer/resize while
        // the user is holding a tab. Restore is automatic because `want` is
        // recomputed from live state on every cursor move, so the next event
        // after the FSM returns to `Idle` falls through to the rules below.
        let drag_icon = tab_drag_cursor_icon(&ws.detach_drag);
        // A split divider under the cursor shows the resize cursor
        // (after chrome/close-button, before the link-pointer / I-beam default).
        let toast_hover = self.update_toast_projection(ws).and_then(|toast| {
            let (x, y) = (ws.cursor.x as f32, ws.cursor.y as f32);
            toast.geometry.hit_test(x, y).map(|hit| match hit {
                kettle_render::UpdateToastHit::Details | kettle_render::UpdateToastHit::Close => {
                    CursorIcon::Pointer
                }
                kettle_render::UpdateToastHit::Body => CursorIcon::Default,
            })
        });
        let want = drag_icon
            .or(toast_hover)
            .or(close_hover)
            .or(new_tab_hover_icon)
            .or(confirm_hover)
            .or(search_hover)
            .or(media_receipt_hover)
            .or(chrome)
            // A card takes a press before a split seam does, so its hand
            // wins where the two overlap.
            .or(card_hover)
            // A lane takes every press over it.
            .or_else(|| self.lane_cursor_icon(ws))
            .or_else(|| self.split_seam_hover_icon(ws))
            .unwrap_or_else(|| {
                let want_pointer = (ws.mods.control_key() || ws.mods.super_key())
                    && self.link_at_cursor(ws).is_some();
                if want_pointer {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Text
                }
            });
        let want = if ws.preview_overlay.is_some() && !self.non_search_modal_open(ws) {
            self.lane_cursor_icon(ws).unwrap_or(CursorIcon::Default)
        } else {
            want
        };
        if ws.last_cursor_icon != Some(want)
            && let Some(w) = &ws.window
        {
            w.set_cursor(want);
            ws.last_cursor_icon = Some(want);
        }
    }

    /// Content area for panes (excludes the tab bar, status bar, and
    /// passive update banner), in physical pixels.
    /// Terminator parity: hit-test for a click in any pane's per-pane titlebar region.
    /// Returns the pane id whose titlebar was clicked, or None if
    /// the click fell outside titlebar regions. Honors the
    /// `cfg.show_titlebar` gate + the `cfg.title_at_bottom` flip.
    /// Used by the click handler to dispatch EditPaneTitle on the
    /// clicked pane.
    fn pane_at_titlebar_click(&self, ws: &WindowState, px: f32, py: f32) -> Option<u64> {
        if !self.cfg.show_titlebar {
            return None;
        }
        let active = ws.mux.active;
        let rects = ws.mux.leaves(active, self.area(ws));
        if rects.len() < 2 {
            // Single-pane tab: titlebar isn't rendered (the renderer's
            // titlebar gate requires `show_titlebar` and more than one pane).
            return None;
        }
        let bar_h = ws.renderer.as_ref().map(|r| r.cell_h + 6.0).unwrap_or(20.0);
        pane_titlebar_hit(px, py, &rects, self.cfg.title_at_bottom, bar_h)
    }

    /// `(row, col)` of the mouse within `rect`.
    ///
    /// Clamped to the focused pane's live grid: a click in the right/bottom
    /// padding (the rect rounds up past an exact cell multiple) must report the
    /// LAST cell, never one past the edge. A mouse-tracking app that sees
    /// `col == cols` or `row == rows` mis-renders — xterm
    /// itself clamps the reported coordinate to the window.
    fn cursor_cell_in_rect(&self, ws: &WindowState, rect: Rect) -> (usize, usize) {
        // Mouse-tracking reports a cell, not a selection boundary — the sub-cell
        // side is irrelevant here, so discard it.
        let (p, _) = self.px_to_point(ws, rect, ws.cursor.x as f32, ws.cursor.y as f32);
        let (row, col) = (p.line.0.max(0) as usize, p.column.0);
        // Clamp to the terminal's grid, the size `resize_all` gave the pane's
        // PTY.
        let (cols, rows) = self.grid_of(ws, rect);
        (
            row.min(rows.saturating_sub(1)),
            col.min(cols.saturating_sub(1)),
        )
    }

    /// `(row, col)` of the mouse within the focused pane, if any.
    fn cursor_cell(&self, ws: &WindowState) -> Option<(usize, usize)> {
        let rect = self.focused_terminal(ws, self.area(ws))?;
        Some(self.cursor_cell_in_rect(ws, rect))
    }

    pub(super) fn link_at_cursor<'a>(&self, ws: &'a WindowState) -> Option<&'a kettle_core::Link> {
        let rect = self.focused_terminal(ws, self.area(ws))?;
        if !rect_contains(rect, ws.cursor.x as f32, ws.cursor.y as f32) {
            return None;
        }
        let (row, col) = self.cursor_cell_in_rect(ws, rect);
        ws.links
            .iter()
            .find(|l| l.row == row && col >= l.start_col && col <= l.end_col)
    }

    pub(super) fn pane_mode(
        &self,
        ws: &WindowState,
        pane_id: u64,
    ) -> Option<kettle_core::TermMode> {
        ws.mux
            .panes
            .get(&pane_id)
            .and_then(|p| p.term.term.lock().ok().map(|t| *t.mode()))
    }
}
