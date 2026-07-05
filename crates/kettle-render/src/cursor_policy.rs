use crate::PaneSnapshot;

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
        && !suppress_windows_codex_status_cursor(snap, cursor_viewport_row, native_windows)
}

fn suppress_windows_codex_status_cursor(
    snap: &PaneSnapshot,
    cursor_viewport_row: i32,
    native_windows: bool,
) -> bool {
    if !native_windows || cursor_viewport_row < 0 {
        return false;
    }
    let current = viewport_row_text(snap, cursor_viewport_row);
    if !is_codex_model_status_row(&current) {
        return false;
    }

    // Native Windows Codex goes through ConPTY. The captured regression stream
    // ended a synchronized Codex repaint with `CSI 17;3 H` + `CSI ?25 h`,
    // placing the visible cursor on the model/status row (`gpt-5.5 high · ~`).
    // Keep the parsed cursor state intact, but avoid painting that status-row
    // cursor when the surrounding screen has Codex's composer shape.
    has_nearby_codex_composer(snap, cursor_viewport_row) || has_codex_header(snap)
}

fn viewport_row_text(snap: &PaneSnapshot, viewport_row: i32) -> String {
    let display_off = snap.display_offset as i32;
    let mut chars = vec![' '; snap.columns];
    for cell in &snap.cells {
        if cell.line + display_off == viewport_row && cell.col < chars.len() {
            chars[cell.col] = cell.c;
        }
    }
    chars.into_iter().collect::<String>()
}

fn is_codex_model_status_row(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("gpt-") && (trimmed.contains(" · ") || trimmed.ends_with('~'))
}

fn is_codex_composer_row(text: &str) -> bool {
    text.trim_start().starts_with('›')
}

fn has_nearby_codex_composer(snap: &PaneSnapshot, cursor_viewport_row: i32) -> bool {
    let start = cursor_viewport_row.saturating_sub(4);
    let end = (cursor_viewport_row + 1).min(snap.screen_lines.saturating_sub(1) as i32);
    (start..=end)
        .filter(|row| *row != cursor_viewport_row)
        .any(|row| is_codex_composer_row(&viewport_row_text(snap, row)))
}

fn has_codex_header(snap: &PaneSnapshot) -> bool {
    (0..snap.screen_lines as i32).any(|row| viewport_row_text(snap, row).contains("OpenAI Codex"))
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

    fn captured_snap(output: &str, cols: usize, rows: usize) -> PaneSnapshot {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let proxy = EventProxy::new(tx, waker);
        let mut term = Term::new(TermConfig::default(), &Size { cols, rows }, proxy);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, output.as_bytes());
        let mut snap = PaneSnapshot::default();
        snap.capture(&term);
        snap
    }

    #[test]
    fn windows_codex_status_row_cursor_is_suppressed() {
        let snap = captured_snap(
            "\x1b[?25l\x1b[HOpenAI Codex (v0.142.5)\r\n\r\n\
             \u{203a} Find and fix a bug in @filename\r\n  gpt-5.5 high · ~\x1b[4;3H\x1b[?25h",
            80,
            8,
        );

        let row = snap.cursor.point.line.0 + snap.display_offset as i32;
        assert_eq!(row, 3);
        assert_eq!(snap.cursor.point.column, Column(2));
        assert!(!cursor_draw_allowed(&snap, row, true, true));
    }

    #[test]
    fn windows_codex_composer_cursor_still_draws() {
        let snap = captured_snap(
            "\x1b[HOpenAI Codex (v0.142.5)\r\n\r\n\
             \u{203a} Find and fix a bug in @filename\r\n  gpt-5.5 high · ~\x1b[3;3H\x1b[?25h",
            80,
            8,
        );
        let row = snap.cursor.point.line.0 + snap.display_offset as i32;

        assert_eq!(row, 2);
        assert!(cursor_draw_allowed(&snap, row, true, true));
    }

    #[test]
    fn normal_shell_prompt_cursor_still_draws_on_windows() {
        let snap = captured_snap("PS C:\\repo> gpt-5.5 high · ~\x1b[1;6H\x1b[?25h", 80, 4);
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
    fn codex_status_row_is_not_suppressed_on_linux_or_wsl() {
        let snap = captured_snap(
            "\x1b[HOpenAI Codex (v0.142.5)\r\n\r\n\
             \u{203a} Find and fix a bug in @filename\r\n  gpt-5.5 high · ~\x1b[4;3H\x1b[?25h",
            80,
            8,
        );
        let row = snap.cursor.point.line.0 + snap.display_offset as i32;

        assert!(cursor_draw_allowed(&snap, row, true, false));
    }

    #[test]
    fn base_draw_gate_still_wins() {
        let snap = captured_snap(
            "\x1b[HOpenAI Codex (v0.142.5)\r\n\r\n\
             \u{203a} Find and fix a bug in @filename\r\n  gpt-5.5 high · ~\x1b[4;3H\x1b[?25h",
            80,
            8,
        );
        let row = snap.cursor.point.line.0 + snap.display_offset as i32;

        assert!(!cursor_draw_allowed(&snap, row, false, true));
    }

    #[test]
    fn scrubbed_cast_regression_preserves_terminal_cursor_state() {
        let cast = concat!(
            "{\"version\":2,\"width\":80,\"height\":8}\n",
            "[0.10, \"o\", \"\\u001b[?25l\\u001b[HOpenAI Codex (v0.142.5)\\r\\n\\r\\n\"]\n",
            "[0.20, \"o\", \"\\u203a Find and fix a bug in @filename\\r\\n  gpt-5.5 high · ~\"]\n",
            "[0.30, \"o\", \"\\u001b[4;3H\\u001b[?25h\"]\n",
        );
        let (tx, _rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let proxy = EventProxy::new(tx, waker);
        let mut term = Term::new(TermConfig::default(), &Size { cols: 80, rows: 8 }, proxy);
        let mut processor: Processor = Processor::new();
        for line in cast.lines().skip(1) {
            let v: serde_json::Value = serde_json::from_str(line).expect("valid cast event");
            if v[1] == "o" {
                processor.advance(&mut term, v[2].as_str().unwrap_or("").as_bytes());
            }
        }

        let mut snap = PaneSnapshot::default();
        snap.capture(&term);
        assert_eq!(snap.cursor.point, Point::new(Line(3), Column(2)));
        let row = snap.cursor.point.line.0 + snap.display_offset as i32;
        assert!(!cursor_draw_allowed(&snap, row, true, true));
    }
}
