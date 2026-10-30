use super::*;

pub(super) fn completion_overlay_hit_test(
    completion: Option<&kettle_render::CompletionOverlay>,
    cell: (f32, f32),
    point: (f32, f32),
) -> bool {
    completion
        .and_then(|completion| kettle_render::completion_overlay_rect(completion, cell))
        .is_some_and(|(x, y, width, height)| {
            point.0 >= x && point.0 < x + width && point.1 >= y && point.1 < y + height
        })
}

pub(super) fn completion_surface_available(
    mode: kettle_core::TermMode,
    display_offset: usize,
) -> bool {
    !mode.contains(kettle_core::TermMode::ALT_SCREEN) && display_offset == 0
}

pub(super) fn completion_anchor_col(cursor_col: usize, input_prefix: &str) -> usize {
    cursor_col.saturating_sub(unicode_width::UnicodeWidthStr::width(input_prefix))
}

impl App {
    pub(super) fn completion_overlay_projection(
        &self,
        ws: &WindowState,
        terminal_surface: bool,
    ) -> Option<kettle_render::CompletionOverlay> {
        if !terminal_surface || !ws.window_focused || ws.ime_preedit.is_some() {
            return None;
        }
        let pane_id = ws.mux.active_focus()?;
        let pane = ws.mux.panes.get(&pane_id)?;
        if !pane.completion_overlay {
            return None;
        }
        let (mode, display_offset, columns, rows) = {
            let term = pane.term.term.lock().ok()?;
            (
                *term.mode(),
                term.grid().display_offset(),
                term.columns(),
                term.screen_lines(),
            )
        };
        if !completion_surface_available(mode, display_offset) {
            return None;
        }
        let command_rows = pane.term.completion_command_rows()?;
        let (list, completion_cursor_col) = pane.term.completion_projection()?;
        let anchor_col = completion_cursor_col
            .zip(list.input_prefix.as_deref())
            .map(|(cursor, prefix)| completion_anchor_col(cursor, prefix));
        let area = self.area(ws);
        let pane_rect = self.focused_terminal(ws, area)?;
        let grid_origin = (
            pane_rect.0 + self.cfg.padding_x,
            pane_rect.1 + self.cfg.padding_y,
        );
        let renderer = ws.renderer.as_ref()?;
        let kind = self.ui_text.text(match list.kind {
            kettle_core::CompletionKind::Completion => kettle_i18n::Text::CompletionKindCompletions,
            kettle_core::CompletionKind::Prediction => kettle_i18n::Text::CompletionKindPrediction,
        });
        Some(kettle_render::CompletionOverlay {
            tr: self.ui_text,
            pane_rect,
            grid_rect: (
                grid_origin.0,
                grid_origin.1,
                columns as f32 * renderer.cell_w,
                rows as f32 * renderer.cell_h,
            ),
            command_rows,
            anchor_col,
            kind: kind.to_string(),
            source: list.source,
            selected: list.selected,
            total: list.total,
            token: list.token,
            candidates: list
                .candidates
                .into_iter()
                .map(|candidate| kettle_render::CompletionOverlayRow {
                    label: candidate.label,
                    description: candidate.description,
                    position: candidate.position,
                })
                .collect(),
        })
    }
}
