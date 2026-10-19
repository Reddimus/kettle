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
            pane.feed_input(&seq)
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
                .map(|pane| pane.feed_input(&bytes))
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
