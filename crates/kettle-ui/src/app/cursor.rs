use super::*;

/// The log target for the cursor blink layer: one line per window when it
/// falls back to GPU blink, and each exit frame's duration, printed only
/// under `RUST_LOG=warn,kettle::cursor_blink=info`.
pub(super) const CURSOR_BLINK_LOG: &str = "kettle::cursor_blink";

/// A modal whose input bar is an active text surface keeps the terminal
/// cursor steady, never mid-blink-off, while it is open.
pub(super) fn modal_holds_cursor_steady(ws: &WindowState) -> bool {
    ws.ssh_input.is_some()
        || ws.palette_input.is_some()
        || ws.layout_picker_input.is_some()
        || ws.theme_picker_input.is_some()
        || ws.hint_state.is_some()
        || ws.search.open
        // The title-edit and confirm-dialog input bars are also active text
        // surfaces, like the other modals.
        || ws.editing_title.is_some()
        || ws.confirm_dialog.is_some()
        || ws.settings_nav.is_some()
}

pub(super) fn cursor_blink_active(
    configured: bool,
    pane_requests_blink: bool,
    window_focused: bool,
) -> bool {
    configured && pane_requests_blink && window_focused
}

impl App {
    pub(super) fn pane_cursor_layer_eligible(ws: &WindowState) -> bool {
        use kettle_core::CursorShape;
        use kettle_core::TermMode;
        ws.vi_mode.is_none()
            && ws
                .mux
                .active_focus()
                .and_then(|id| ws.mux.panes.get(&id))
                .is_some_and(|pane| {
                    let Ok(term) = pane.term.term.try_lock() else {
                        return false;
                    };
                    let content = term.renderable_content();
                    let row = i64::from(content.cursor.point.line.0)
                        .saturating_add(i64::try_from(content.display_offset).unwrap_or(i64::MAX));
                    !content.mode.contains(TermMode::VI)
                        && content.cursor.shape != CursorShape::Hidden
                        && (0..i64::try_from(term.grid().screen_lines()).unwrap_or(0))
                            .contains(&row)
                })
    }

    pub(super) fn pane_cursor_blinking(&self, ws: &WindowState, use_pane_snapshots: bool) -> bool {
        pane_cursor_blinking_with(
            ws.mux.active_focus(),
            &ws.pane_snapshot_keys,
            &ws.pane_snapshots,
            use_pane_snapshots,
            |id| {
                ws.mux
                    .panes
                    .get(&id)
                    .map(|pane| pane.term.cursor_blinking())
            },
        )
    }

    /// Force the next redraw to render the cursor visible. Shared by:
    /// - the focus-change path (`note_focus_change`)
    /// - `Action::Reset` so a "fresh start" cursor is visible
    /// - `CursorBlinkingChange` events (DEC ?12 program-driven toggle)
    /// - the four modal Escape handlers so closing the
    ///   search/palette/hints/SSH overlay reveals the cursor immediately
    ///   instead of waiting up to one blink interval.
    pub(super) fn reset_blink_phase(&mut self, ws: &mut WindowState) {
        let now = std::time::Instant::now();
        ws.blink_on = true;
        ws.last_blink = now;
        ws.last_blink_activity = now;
        // The window server's blink was planned from the old activity time.
        // A frame ends it (see `redraw`), so it never outlives new activity.
        if ws.blink_layer.is_active()
            && let Some(window) = &ws.window
        {
            window.request_redraw();
        }
    }

    /// Bring the app's blink phase to where the window server's animation is.
    /// A writer since the hand-off (`reset_blink_phase`, a DEC mode 12
    /// change) moves `last_blink` off the anchor, and its phase wins.
    pub(super) fn materialize_layer_blink(ws: &mut WindowState, now: std::time::Instant) {
        let Some((on, edge)) = ws.blink_layer.materialize(ws.blink_on, ws.last_blink, now) else {
            return;
        };
        ws.blink_on = on;
        ws.last_blink = edge;
    }

    /// The window's cursor layer, installed and given to the renderer at the
    /// first hand-off, so startup never pays for it. A failure keeps the GPU
    /// blink for this window and logs once.
    pub(super) fn ensure_cursor_layer(ws: &mut WindowState) -> bool {
        if ws.cursor_layer.is_some() {
            return true;
        }
        // A window between renderers (recovery) tries again at a later edge.
        let (Some(window), Some(renderer)) = (ws.window.as_deref(), ws.renderer.as_mut()) else {
            return false;
        };
        let installed = crate::macos_cursor_layer::CursorLayer::install(window).and_then(|layer| {
            layer.attach(renderer)?;
            Ok(layer)
        });
        match installed {
            Ok(layer) => {
                ws.cursor_layer = Some(layer);
                true
            }
            Err(reason) => {
                if ws.blink_layer.fall_back(reason) {
                    log::info!(
                        target: CURSOR_BLINK_LOG,
                        "window {}: the cursor blinks on the GPU: {reason}",
                        ws.seq
                    );
                }
                false
            }
        }
    }

    /// A hand-off frame reached the screen: start the window server's blink
    /// over the patch it drew, or keep the GPU blink for this edge.
    pub(super) fn start_layer_blink(
        &self,
        ws: &mut WindowState,
        patch: CursorPatchOutcome,
        window: Option<&winit::window::Window>,
    ) {
        ws.blink_layer.failed_handoff();
        // A failed patch present may leave old contents on the layer.
        // Keep them hidden until both the patch and the animation are ready.
        if let Some(layer) = &ws.cursor_layer {
            layer.hide();
        }
        let rect = match patch {
            CursorPatchOutcome::Presented(rect) => rect,
            CursorPatchOutcome::Ineligible(reason) => {
                log::debug!(
                    target: CURSOR_BLINK_LOG,
                    "window {}: cursor patch ineligible: {reason:?}",
                    ws.seq
                );
                return;
            }
            CursorPatchOutcome::Failed(reason) => {
                log::debug!(
                    target: CURSOR_BLINK_LOG,
                    "window {}: cursor patch failed: {}",
                    ws.seq,
                    reason.as_str()
                );
                if reason == CursorPatchFailure::Lost {
                    Self::retire_cursor_layer(ws);
                }
                return;
            }
        };
        let rect_px = [rect.x, rect.y, rect.width, rect.height];
        let Some(plan) = crate::cursor_blink::layer_plan(
            ws.blink_on,
            ws.last_blink,
            ws.last_blink_activity,
            std::time::Duration::from_millis(self.cfg.cursor_blink_interval),
            self.cfg.cursor_blink_timeout(),
        ) else {
            return;
        };
        let (Some(layer), Some(window)) = (&ws.cursor_layer, window) else {
            return;
        };
        match layer.show(
            rect_px,
            window.scale_factor(),
            &plan,
            std::time::Instant::now(),
        ) {
            Ok(()) => ws.blink_layer.started(plan, rect_px),
            Err(reason) => log::debug!(
                target: CURSOR_BLINK_LOG,
                "window {}: the cursor layer did not start: {reason}",
                ws.seq
            ),
        }
    }

    /// Hide the cursor layer without a frame: the window lost focus, was
    /// occluded, changed size or scale, or is rebuilding its renderer. The Metal
    /// layer beneath shows the off phase, which is right for each of these
    /// until the next frame draws the phase materialized here.
    pub(super) fn hide_cursor_layer(ws: &mut WindowState) {
        if !ws.blink_layer.is_active() {
            return;
        }
        Self::materialize_layer_blink(ws, std::time::Instant::now());
        if let Some(layer) = &ws.cursor_layer {
            layer.hide();
        }
        ws.blink_layer.hidden();
        if ws.cursor_exit_log.enabled {
            ws.cursor_exit_log.hidden();
        }
    }

    /// Remove the cursor layer and the renderer's surface on it. A rebuilt
    /// surface adds its Metal layer above the old one, so the next hand-off
    /// installs a fresh cursor layer above that.
    pub(super) fn retire_cursor_layer(ws: &mut WindowState) {
        Self::hide_cursor_layer(ws);
        let layer = ws.cursor_layer.take();
        if layer.is_some()
            && let Some(renderer) = ws.renderer.as_mut()
        {
            renderer.detach_cursor_layer();
        }
    }
}
