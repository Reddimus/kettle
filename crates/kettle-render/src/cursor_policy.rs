use alacritty_terminal::term::cell::Flags;

use crate::{PaneSnapshot, SnapCell};

/// Final visual cursor gate for renderer-only compatibility shims.
///
/// This must not mutate terminal state: DEC ?25, `read_screen.cursor`, input
/// routing, and blink timing all remain owned by the terminal/UI layers.
pub(crate) fn cursor_draw_allowed(
    snap: &PaneSnapshot,
    cursor_viewport_row: i32,
    base_draw_allowed: bool,
    native_windows: bool,
) -> bool {
    base_draw_allowed
        && !suppress_windows_codex_footer_cursor(snap, cursor_viewport_row, native_windows)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CodexFooter {
    active_status_row: i32,
    composer_row: i32,
    model_row: i32,
}

fn suppress_windows_codex_footer_cursor(
    snap: &PaneSnapshot,
    cursor_viewport_row: i32,
    native_windows: bool,
) -> bool {
    if !native_windows || cursor_viewport_row < 0 {
        return false;
    }

    if let Some(footer) = active_codex_footer_near(snap, cursor_viewport_row) {
        if cursor_viewport_row == footer.active_status_row
            || cursor_viewport_row == footer.model_row
        {
            return true;
        }
        if cursor_viewport_row == footer.composer_row {
            // Codex keeps its composer editable while a task runs. Its empty
            // placeholder is DIM, while real queued input is not. Hide only
            // the cursor over the placeholder so queued input retains a
            // visible caret.
            return cursor_cell_has_flag(snap, cursor_viewport_row, Flags::DIM);
        }
    }

    let Some(current) = viewport_row_cells(snap, cursor_viewport_row) else {
        return false;
    };
    if !is_codex_model_status_row(current) {
        return false;
    }

    // Compatibility with older Codex layouts whose idle model row was the
    // final cursor location. Keep the parsed cursor state intact and suppress
    // only when the surrounding screen proves this is a Codex UI.
    has_nearby_codex_composer(snap, cursor_viewport_row) || has_codex_header(snap)
}

fn active_codex_footer_near(snap: &PaneSnapshot, cursor_row: i32) -> Option<CodexFooter> {
    let first = cursor_row.saturating_sub(4).max(0);
    let last = (cursor_row + 4).min(snap.screen_lines.saturating_sub(1) as i32);

    (first..=last).find_map(|composer_row| {
        let composer = viewport_row_cells(snap, composer_row)?;
        if !is_codex_composer_row(composer) {
            return None;
        }

        let active_status_row = (composer_row.saturating_sub(4).max(0)..composer_row)
            .rev()
            .find(|row| viewport_row_cells(snap, *row).is_some_and(is_codex_active_status_row))?;
        let model_row = ((composer_row + 1)
            ..=(composer_row + 3).min(snap.screen_lines.saturating_sub(1) as i32))
            .find(|row| viewport_row_cells(snap, *row).is_some_and(is_codex_model_status_row))?;

        Some(CodexFooter {
            active_status_row,
            composer_row,
            model_row,
        })
    })
}

/// `PaneSnapshot::capture` stores every viewport row in row-major order and
/// every row has exactly `columns` cells, so footer classification is O(1) per
/// candidate row and does not allocate or rescan the full grid.
fn viewport_row_cells(snap: &PaneSnapshot, viewport_row: i32) -> Option<&[SnapCell]> {
    if viewport_row < 0 || viewport_row >= snap.screen_lines as i32 || snap.columns == 0 {
        return None;
    }
    let start = viewport_row as usize * snap.columns;
    let end = start.checked_add(snap.columns)?;
    let row = snap.cells.get(start..end)?;
    let expected_line = viewport_row - snap.display_offset as i32;
    row.first()
        .is_some_and(|cell| cell.line == expected_line)
        .then_some(row)
}

fn row_starts_with_trimmed(row: &[SnapCell], prefix: &str) -> bool {
    let start = row
        .iter()
        .position(|cell| !cell.c.is_whitespace())
        .unwrap_or(row.len());
    let mut cells = row[start..].iter().map(|cell| cell.c);
    prefix.chars().all(|want| cells.next() == Some(want))
}

fn row_contains(row: &[SnapCell], needle: &str) -> bool {
    let len = needle.chars().count();
    len != 0
        && row
            .windows(len)
            .any(|window| window.iter().map(|cell| cell.c).eq(needle.chars()))
}

fn row_ends_with_trimmed(row: &[SnapCell], want: char) -> bool {
    row.iter()
        .rev()
        .find(|cell| !cell.c.is_whitespace())
        .is_some_and(|cell| cell.c == want)
}

fn is_codex_model_status_row(row: &[SnapCell]) -> bool {
    row_starts_with_trimmed(row, "gpt-")
        && (row_contains(row, " · ") || row_ends_with_trimmed(row, '~'))
}

fn is_codex_composer_row(row: &[SnapCell]) -> bool {
    row_starts_with_trimmed(row, "›")
}

fn is_codex_active_status_row(row: &[SnapCell]) -> bool {
    row_contains(row, "to interrupt")
}

fn cursor_cell_has_flag(snap: &PaneSnapshot, cursor_viewport_row: i32, flag: Flags) -> bool {
    viewport_row_cells(snap, cursor_viewport_row)
        .and_then(|row| row.get(snap.cursor.point.column.0))
        .is_some_and(|cell| cell.flags.contains(flag))
}

fn has_nearby_codex_composer(snap: &PaneSnapshot, cursor_viewport_row: i32) -> bool {
    let start = cursor_viewport_row.saturating_sub(4).max(0);
    let end = (cursor_viewport_row + 1).min(snap.screen_lines.saturating_sub(1) as i32);
    (start..=end)
        .filter(|row| *row != cursor_viewport_row)
        .any(|row| viewport_row_cells(snap, row).is_some_and(is_codex_composer_row))
}

fn has_codex_header(snap: &PaneSnapshot) -> bool {
    (0..snap.screen_lines as i32).any(|row| {
        viewport_row_cells(snap, row).is_some_and(|cells| row_contains(cells, "OpenAI Codex"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::Term;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::Config as TermConfig;
    use alacritty_terminal::vte::ansi::Processor;
    use kettle_core::{EventProxy, Waker};

    struct Size {
        cols: usize,
        rows: usize,
    }

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.rows
        }

        fn screen_lines(&self) -> usize {
            self.rows
        }

        fn columns(&self) -> usize {
            self.cols
        }
    }

    fn term(cols: usize, rows: usize) -> Term<EventProxy> {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let proxy = EventProxy::new(tx, waker);
        Term::new(TermConfig::default(), &Size { cols, rows }, proxy)
    }

    fn captured_snap(output: &str, cols: usize, rows: usize) -> PaneSnapshot {
        let mut term = term(cols, rows);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, output.as_bytes());
        let mut snap = PaneSnapshot::default();
        snap.capture(&term);
        snap
    }

    fn current_codex_footer(cursor: &str, composer: &str) -> String {
        format!(
            "\x1b[HOpenAI Codex (v0.144.0)\
             \x1b[10;2H\u{2022} Working (2s \u{2022} esc to interrupt)\
             \x1b[13;1H\x1b[1m\u{203a}\x1b[22m {composer}\x1b[15;3Hgpt-5.5 high \u{b7} ~{cursor}"
        )
    }

    #[test]
    fn windows_codex_active_status_cursor_is_suppressed() {
        let snap = captured_snap(
            &current_codex_footer(
                "\x1b[10;2H\x1b[?25h",
                "\x1b[2mExplain this codebase\x1b[22m",
            ),
            80,
            20,
        );
        let row = snap.cursor.point.line.0 + snap.display_offset as i32;

        assert_eq!(row, 9);
        assert!(!cursor_draw_allowed(&snap, row, true, true));
    }

    #[test]
    fn windows_codex_dim_placeholder_cursor_is_suppressed_while_active() {
        let snap = captured_snap(
            &current_codex_footer(
                "\x1b[13;3H\x1b[?25h",
                "\x1b[2mExplain this codebase\x1b[22m",
            ),
            80,
            20,
        );
        let row = snap.cursor.point.line.0 + snap.display_offset as i32;

        assert_eq!(row, 12);
        assert!(cursor_cell_has_flag(&snap, row, Flags::DIM));
        assert!(!cursor_draw_allowed(&snap, row, true, true));
    }

    #[test]
    fn windows_codex_queued_input_cursor_still_draws_while_active() {
        let snap = captured_snap(
            &current_codex_footer("\x1b[13;8H\x1b[?25h", "queued work"),
            80,
            20,
        );
        let row = snap.cursor.point.line.0 + snap.display_offset as i32;

        assert_eq!(row, 12);
        assert!(!cursor_cell_has_flag(&snap, row, Flags::DIM));
        assert!(cursor_draw_allowed(&snap, row, true, true));
    }

    #[test]
    fn windows_codex_idle_placeholder_cursor_still_draws() {
        let snap = captured_snap(
            "\x1b[HOpenAI Codex (v0.144.0)\x1b[13;1H\u{203a} \
             \x1b[2mExplain this codebase\x1b[22m\x1b[15;3Hgpt-5.5 high \u{b7} ~\
             \x1b[13;3H\x1b[?25h",
            80,
            20,
        );
        let row = snap.cursor.point.line.0 + snap.display_offset as i32;

        assert!(cursor_draw_allowed(&snap, row, true, true));
    }

    #[test]
    fn windows_codex_model_status_row_cursor_is_suppressed() {
        let snap = captured_snap(
            "\x1b[?25l\x1b[HOpenAI Codex (v0.142.5)\r\n\r\n\
             \u{203a} Find and fix a bug in @filename\r\n  gpt-5.5 high \u{b7} ~\x1b[4;3H\x1b[?25h",
            80,
            8,
        );

        let row = snap.cursor.point.line.0 + snap.display_offset as i32;
        assert_eq!(row, 3);
        assert_eq!(snap.cursor.point.column, Column(2));
        assert!(!cursor_draw_allowed(&snap, row, true, true));
    }

    #[test]
    fn normal_shell_prompt_cursor_still_draws_on_windows() {
        let snap = captured_snap(
            "PS C:\\repo> gpt-5.5 high \u{b7} ~\x1b[1;6H\x1b[?25h",
            80,
            4,
        );
        let row = snap.cursor.point.line.0 + snap.display_offset as i32;

        assert!(cursor_draw_allowed(&snap, row, true, true));
    }

    #[test]
    fn windows_degraded_codex_status_row_cursor_is_suppressed_with_header() {
        let snap = captured_snap(
            "\x1b[HOpenAI Codex (v0.142.5)\r\n\r\n\
             Find and fix a bug in @filename\r\n  gpt-5.5 high  ~\x1b[4;30H\x1b[?25h",
            80,
            8,
        );
        let row = snap.cursor.point.line.0 + snap.display_offset as i32;

        assert_eq!(row, 3);
        assert!(!cursor_draw_allowed(&snap, row, true, true));
    }

    #[test]
    fn codex_footer_is_not_suppressed_on_linux_or_wsl() {
        let snap = captured_snap(
            &current_codex_footer(
                "\x1b[13;3H\x1b[?25h",
                "\x1b[2mExplain this codebase\x1b[22m",
            ),
            80,
            20,
        );
        let row = snap.cursor.point.line.0 + snap.display_offset as i32;

        assert!(cursor_draw_allowed(&snap, row, true, false));
    }

    #[test]
    fn base_draw_gate_still_wins() {
        let snap = captured_snap(
            &current_codex_footer(
                "\x1b[13;3H\x1b[?25h",
                "\x1b[2mExplain this codebase\x1b[22m",
            ),
            80,
            20,
        );
        let row = snap.cursor.point.line.0 + snap.display_offset as i32;

        assert!(!cursor_draw_allowed(&snap, row, false, true));
    }

    #[test]
    fn scrubbed_cast_regression_suppresses_both_observed_cursor_positions() {
        let mut term = term(80, 20);
        let mut processor: Processor = Processor::new();
        let initial = current_codex_footer("", "\x1b[2mExplain this codebase\x1b[22m");
        processor.advance(&mut term, initial.as_bytes());

        // Scrubbed from the recorded Codex 0.144 stream: the synchronized
        // repaint ends on the active row, then a second ConPTY read places the
        // cursor over the DIM placeholder 12 ms later.
        processor.advance(
            &mut term,
            b"\x1b[?2026h\x1b[0 q\x1b[?25l\x1b[10;2H\x1b[?25h\x1b[?2026l",
        );
        let mut snap = PaneSnapshot::default();
        snap.capture(&term);
        assert_eq!(snap.cursor.point, Point::new(Line(9), Column(1)));
        assert!(!cursor_draw_allowed(&snap, 9, true, true));

        processor.advance(&mut term, b"\x1b[?25l\x1b[13;3H\x1b[?25h");
        snap.capture(&term);
        assert_eq!(snap.cursor.point, Point::new(Line(12), Column(2)));
        assert!(
            snap.cells
                .iter()
                .any(|cell| cell.flags.contains(Flags::DIM))
        );
        assert!(!cursor_draw_allowed(&snap, 12, true, true));
    }

    #[test]
    fn row_lookup_rejects_malformed_snapshot_layout() {
        let mut snap = captured_snap("ok", 4, 2);
        snap.cells.pop();
        assert!(viewport_row_cells(&snap, 1).is_none());
    }
}
