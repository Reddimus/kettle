//! Bounded command outcomes anchored to primary-grid prompt rows.
use std::collections::VecDeque;

use alacritty_terminal::grid::Dimensions;

use super::{GraphicsEvent, GraphicsEventBatch, GraphicsScroll, GraphicsScrollDirection};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommandMark {
    pub row: u64,
    pub exit_code: i32,
}

pub(super) enum Boundary {
    PromptStart,
    Begin,
    End(Option<i32>),
    Cancel,
}

#[derive(Default)]
pub(super) struct CommandMarks {
    // Separate from prompt navigation: this anchor follows committed row moves
    // even when the shell moves its cursor between A and C.
    prompt: Option<u64>,
    // A running command can lose its row. Keep that state until D so a
    // duplicate bash C cannot resurrect the discarded anchor.
    pending: Option<Option<u64>>,
    marks: VecDeque<CommandMark>,
    alternate: bool,
}

impl CommandMarks {
    pub(super) fn apply_boundary(
        &mut self,
        boundary: Boundary,
        term: &super::Term<crate::EventProxy>,
    ) {
        if term
            .mode()
            .contains(alacritty_terminal::term::TermMode::ALT_SCREEN)
        {
            return;
        }
        let grid = term.grid();
        match boundary {
            Boundary::PromptStart => {
                self.cancel_pending();
                self.prompt = Some(super::stable_grid_line_id(
                    grid.history_origin(),
                    grid.history_size(),
                    grid.cursor.point.line.0,
                ));
            }
            Boundary::Begin => {
                self.begin(self.prompt.filter(|row| *row >= grid.history_origin()));
            }
            Boundary::End(code) => self.end(code),
            Boundary::Cancel => self.cancel_pending(),
        }
    }

    pub(super) fn discard_boundaries(&mut self) {
        self.prompt = None;
        self.pending = Some(None);
    }

    pub(super) fn begin(&mut self, row: Option<u64>) {
        if self.pending.is_none() {
            self.pending = Some(row);
        }
    }

    pub(super) fn end(&mut self, code: Option<i32>) {
        if let Some(Some(row)) = self.pending.take()
            && let Some(exit_code) = code
        {
            if self.marks.len() == 2048 {
                self.marks.pop_front();
            }
            self.marks.push_back(CommandMark { row, exit_code });
        }
    }

    pub(super) fn retained(&mut self, origin: u64, end: u64) -> Vec<CommandMark> {
        self.prompt = self.prompt.filter(|row| (origin..end).contains(row));
        if let Some(row) = self.pending.as_mut() {
            *row = row.filter(|row| (origin..end).contains(row));
        }
        self.marks.retain(|mark| (origin..end).contains(&mark.row));
        self.marks.iter().copied().collect()
    }

    pub(super) fn apply_batch(&mut self, batch: &GraphicsEventBatch) {
        if batch.overflowed {
            self.invalidate();
            self.alternate = batch.alternate_screen;
            return;
        }
        for &event in &batch.events {
            match event {
                GraphicsEvent::EnterAlternate { .. } => self.alternate = true,
                GraphicsEvent::LeaveAlternate { .. } => self.alternate = false,
                GraphicsEvent::Reset => {
                    self.clear();
                    self.alternate = false;
                }
                GraphicsEvent::Scroll {
                    direction,
                    top,
                    bottom,
                    lines,
                    old_screen_top,
                    new_screen_top,
                    screen_lines,
                    ..
                } if !self.alternate => {
                    // Scroll rotates whole rows, including full-width DECLRMM;
                    // the VT engine emits PartialScroll for narrower margins.
                    self.scroll(GraphicsScroll {
                        direction,
                        top,
                        bottom,
                        lines,
                        old_screen_top,
                        new_screen_top,
                        screen_lines,
                        columns: None,
                    });
                }
                GraphicsEvent::PartialScroll {
                    direction,
                    top,
                    bottom,
                    left,
                    right,
                    lines,
                    screen_top,
                    screen_lines,
                } if !self.alternate => {
                    self.scroll(GraphicsScroll {
                        direction,
                        top,
                        bottom,
                        lines,
                        old_screen_top: screen_top,
                        new_screen_top: screen_top,
                        screen_lines,
                        columns: Some(left..right),
                    });
                }
                _ => {}
            }
        }
        if self.alternate != batch.alternate_screen {
            self.invalidate();
            self.alternate = batch.alternate_screen;
        }
    }

    fn scroll(&mut self, scroll: GraphicsScroll) {
        let GraphicsScroll {
            direction,
            top,
            bottom,
            lines,
            old_screen_top,
            new_screen_top,
            screen_lines,
            columns,
        } = scroll;
        if top >= bottom || bottom > screen_lines || lines == 0 || lines > bottom - top {
            self.invalidate();
            return;
        }
        let shift = |row: u64| {
            let Some(relative) = row.checked_sub(old_screen_top) else {
                return Some(row);
            };
            if relative >= screen_lines as u64 {
                return Some(row);
            }
            let relative = relative as usize;
            let moved = if (top..bottom).contains(&relative) {
                // A row-only mark cannot follow text split by horizontal margins.
                if columns.is_some() {
                    return None;
                }
                if direction == GraphicsScrollDirection::Up
                    && top == 0
                    && new_screen_top > old_screen_top
                {
                    return Some(row);
                }
                match direction {
                    GraphicsScrollDirection::Up => {
                        relative.checked_sub(lines).filter(|&r| r >= top)
                    }
                    GraphicsScrollDirection::Down => {
                        relative.checked_add(lines).filter(|&r| r < bottom)
                    }
                }
            } else {
                Some(relative)
            };
            moved.and_then(|row| new_screen_top.checked_add(row as u64))
        };
        self.prompt = self.prompt.and_then(shift);
        if let Some(row) = self.pending.as_mut() {
            *row = row.and_then(shift);
        }
        self.marks.retain_mut(|mark| {
            if let Some(row) = shift(mark.row) {
                mark.row = row;
                true
            } else {
                false
            }
        });
    }

    fn invalidate(&mut self) {
        self.prompt = None;
        self.pending = self.pending.map(|_| None);
        self.marks.clear();
    }

    pub(super) fn cancel_pending(&mut self) {
        self.pending = None;
    }

    pub(super) fn clear(&mut self) {
        self.prompt = None;
        self.pending = None;
        self.marks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_status_requires_a_command_and_known_exit_code() {
        let mut marks = CommandMarks::default();
        marks.end(Some(1));
        marks.begin(Some(10));
        marks.begin(Some(11)); // duplicate C from a compound bash command
        marks.end(Some(0));
        marks.end(Some(9)); // duplicate D cannot change a completed command
        marks.begin(Some(12));
        marks.end(Some(-1));
        marks.begin(Some(13));
        marks.end(None);
        assert_eq!(
            marks.retained(0, 20),
            vec![
                CommandMark {
                    row: 10,
                    exit_code: 0
                },
                CommandMark {
                    row: 12,
                    exit_code: -1
                },
            ]
        );
        assert_eq!(
            marks.retained(11, 20),
            vec![CommandMark {
                row: 12,
                exit_code: -1
            }]
        );
        marks.clear();
        assert!(marks.retained(0, 20).is_empty());
    }

    #[test]
    fn outcomes_are_bounded_and_reflow_cancels_pending_commands() {
        let mut marks = CommandMarks::default();
        for row in 0..3000 {
            marks.begin(Some(row));
            marks.end(Some(2));
        }
        let retained = marks.retained(0, 4000);
        assert_eq!(retained.len(), 2048);
        assert_eq!(retained[0].row, 952);
        marks.begin(Some(3001));
        marks.clear();
        marks.end(Some(0));
        assert!(marks.retained(0, 4000).is_empty());
    }

    const FIXTURE_ENV: &str = "KETTLE_TEST_COMMAND_MARK_WIRE";

    #[test]
    fn command_mark_wire_child() {
        let Ok(wire) = std::env::var(FIXTURE_ENV) else {
            return;
        };
        if wire == "REPLY_FIXTURE" {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut reply = String::new();
                std::io::stdin().read_line(&mut reply).unwrap();
                let _ = tx.send(reply);
            });
            let received = rx.recv_timeout(std::time::Duration::from_secs(2)).ok();
            std::process::exit(if received.as_deref().map(str::trim) == Some("REPLY") {
                0
            } else {
                7
            });
        }
        use std::io::Write;
        let mut output = std::io::stdout().lock();
        output.write_all(b"\x1b[H\x1b[2J").unwrap();
        output.write_all(wire.as_bytes()).unwrap();
        output.flush().unwrap();
        // Avoid libtest's trailing status text changing the six-row fixture.
        std::process::exit(0);
    }

    fn pane(wire: &str, history: usize) -> super::super::Terminal {
        pane_observed(wire, history, std::sync::Arc::new(|| {}), |_, _| {})
    }

    fn pane_observed(
        wire: &str,
        history: usize,
        waker: crate::Waker,
        observe: impl FnOnce(
            &super::super::Terminal,
            &crossbeam_channel::Receiver<super::super::TermEvent>,
        ),
    ) -> super::super::Terminal {
        use super::super::{CursorShape, Terminal};
        use std::time::{Duration, Instant};
        let (tx, rx) = crossbeam_channel::unbounded();
        if wire == "REPLY_FIXTURE" {
            tx.send(super::super::TermEvent::PtyWrite("REPLY\r\n".into()))
                .unwrap();
        }
        let pane = Terminal::new_with_env_and_output(
            &[
                std::env::current_exe().unwrap().to_str().unwrap().into(),
                "--exact".into(),
                "term::command_marks::tests::command_mark_wire_child".into(),
                "--nocapture".into(),
                "--quiet".into(),
            ],
            None,
            history,
            0,
            80,
            6,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            "xterm-256color",
            "truecolor",
            &[(FIXTURE_ENV.into(), wire.into())],
            false,
            false,
            tx,
            waker,
            None,
        )
        .expect("command-mark PTY fixture");
        #[cfg(windows)]
        let mut pane = pane;
        eprintln!("command-mark fixture started PID {:?}", pane.child_pid());
        let observed =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observe(&pane, &rx)));
        let deadline = Instant::now() + Duration::from_secs(15);
        while !pane.take_exited() {
            forward_replies(&pane, &rx);
            #[cfg(windows)]
            if pane.child_exit_code().is_some() {
                pane.begin_pty_output_close().expect("close fixture ConPTY");
            }
            assert!(
                Instant::now() < deadline,
                "command-mark reader did not reach EOF"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        while pane.child_exit_code().is_none() {
            forward_replies(&pane, &rx);
            assert!(Instant::now() < deadline, "command-mark child did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
        eprintln!("command-mark fixture reaped PID {:?}", pane.child_pid());
        assert_eq!(pane.child_exit_code(), Some(0));
        if let Err(panic) = observed {
            std::panic::resume_unwind(panic);
        }
        pane
    }

    fn forward_replies(
        pane: &super::super::Terminal,
        rx: &crossbeam_channel::Receiver<super::super::TermEvent>,
    ) {
        while let Ok(event) = rx.try_recv() {
            if let super::super::TermEvent::PtyWrite(reply) = event {
                pane.write(reply.as_bytes());
            }
        }
    }

    fn command(row: usize, text: &str, code: i32) -> String {
        format!("\x1b[{row};1H\x1b]133;A\x07{text}\x1b]133;C\x07\x1b]133;D;{code}\x07")
    }

    fn setup(pending: bool) -> String {
        let mut wire = command(1, "ABOVE", 11);
        wire.push_str(&command(6, "BELOW", 66));
        wire.push_str("\x1b[3;1H\x1b]133;A\x07TARGET\x1b]133;C\x07");
        if !pending {
            wire.push_str("\x1b]133;D;7\x07");
        }
        wire
    }

    fn finish(wire: &mut String, pending: bool) {
        if pending {
            wire.push_str("\x1b]133;C\x07\x1b]133;D;7\x07");
        }
    }

    fn assert_rows(pane: &super::super::Terminal, rows: &[(i64, i32)]) {
        use alacritty_terminal::grid::Dimensions;
        let screen_top = {
            let term = pane.term.lock().unwrap();
            term.grid().history_origin() + term.grid().history_size() as u64
        };
        let actual: Vec<_> = pane
            .command_marks()
            .iter()
            .map(|m| (i128::from(m.row) - i128::from(screen_top), m.exit_code))
            .collect();
        let expected: Vec<_> = rows
            .iter()
            .map(|&(row, code)| (i128::from(row), code))
            .collect();
        assert_eq!(
            actual, expected,
            "command rows relative to active screen top {screen_top}"
        );
    }

    fn assert_text(pane: &super::super::Terminal, row: i32, expected: &str) {
        use alacritty_terminal::index::{Column, Line};
        let term = pane.term.lock().unwrap();
        let text: String = (0..expected.len())
            .map(|col| term.grid()[Line(row)][Column(col)].c)
            .collect();
        assert_eq!(text, expected);
    }

    #[test]
    fn command_marks_follow_region_su_sd_ri_il_and_dl() {
        for pending in [false, true] {
            for (scroll, row) in [
                ("\x1b[S", 1),
                ("\x1b[?69h\x1b[1;80s\x1b[S", 1),
                ("\x1b[T", 3),
                ("\x1b[2;1H\x1bM", 3),
                ("\x1b[2;1H\x1b[L", 3),
                ("\x1b[2;1H\x1b[M", 1),
            ] {
                let mut wire = setup(pending);
                wire.push_str("\x1b[2;5r");
                wire.push_str(scroll);
                finish(&mut wire, pending);
                let pane = pane(&wire, 4);
                assert_text(&pane, row, "TARGET");
                assert_text(&pane, 0, "ABOVE");
                assert_text(&pane, 5, "BELOW");
                assert_rows(&pane, &[(0, 11), (5, 66), (i64::from(row), 7)]);
            }
        }
    }

    #[test]
    fn command_marks_scrolled_out_of_a_region_are_retired() {
        for pending in [true, false] {
            for scroll in ["\x1b[2S", "\x1b[3T", "\x1b[999S", "\x1b[999T"] {
                let mut wire = setup(pending);
                wire.push_str("\x1b[2;5r");
                wire.push_str(scroll);
                finish(&mut wire, pending);
                let pane = pane(&wire, 4);
                assert_rows(&pane, &[(0, 11), (5, 66)]);
            }
        }
    }

    #[test]
    fn command_marks_apply_multiple_mutations_in_wire_order() {
        for pending in [false, true] {
            let mut wire = setup(pending);
            wire.push_str("\x1b[2;5r\x1b[S\x1b[3;6r\x1b[S\x1b[3;4r\x1b[T");
            finish(&mut wire, pending);
            let first = pane(&wire, 4);
            assert_text(&first, 1, "TARGET");
            assert_text(&first, 4, "BELOW");
            assert_rows(&first, &[(0, 11), (4, 66), (1, 7)]);

            let mut wire = setup(pending);
            wire.push_str("\x1b[2;5r\x1b[2S\x1b[2T");
            finish(&mut wire, pending);
            assert_rows(&pane(&wire, 4), &[(0, 11), (5, 66)]);
        }
    }

    #[test]
    fn command_marks_keep_document_ids_through_history_scrolls() {
        for pending in [false, true] {
            for history in [0, 2, 8] {
                let mut wire = setup(pending);
                wire.push_str("\x1b[2;5r\x1b[S\x1b[1;5r\x1b[S");
                finish(&mut wire, pending);
                let pane = pane(&wire, history);
                assert_text(&pane, 0, "TARGET");
                assert_text(&pane, 5, "BELOW");
                let mut expected = vec![(5, 66), (0, 7)];
                if history > 0 {
                    expected.insert(0, (-1, 11));
                }
                assert_rows(&pane, &expected);
            }
            let mut wire = setup(pending);
            wire.push_str("\x1b[2;5r\x1b[S\x1b[r\x1b[6;1H\n\n\n\n");
            finish(&mut wire, pending);
            let pane = pane(&wire, 2);
            assert_rows(&pane, &[(1, 66)]);
        }
    }

    #[test]
    fn command_marks_ignore_alternate_scrolls_and_resume_primary_movement() {
        for pending in [false, true] {
            for mode in [47, 1047, 1049] {
                let mut wire = setup(pending);
                wire.push_str(&format!(
                    "\x1b[?{mode}h\x1b[2;5r\x1b[S\x1b[T\x1b[?{mode}l\x1b[2;5r\x1b[S"
                ));
                finish(&mut wire, pending);
                let pane = pane(&wire, 4);
                assert_text(&pane, 1, "TARGET");
                assert_rows(&pane, &[(0, 11), (5, 66), (1, 7)]);
            }
        }
    }

    #[test]
    fn command_marks_follow_committed_synchronized_scrolls() {
        for pending in [false, true] {
            let mut wire = setup(pending);
            wire.push_str("\x1b[?2026h\x1b[2;5r\x1b[S\x1b[?2026l");
            finish(&mut wire, pending);
            let pane = pane(&wire, 4);
            assert_text(&pane, 1, "TARGET");
            assert_rows(&pane, &[(0, 11), (5, 66), (1, 7)]);
        }
    }

    #[test]
    fn command_marks_retire_ambiguous_partial_width_rows_only() {
        for pending in [false, true] {
            let mut wire = setup(pending);
            wire.push_str("\x1b[?69h\x1b[2;5s\x1b[2;5r\x1b[S");
            finish(&mut wire, pending);
            let pane = pane(&wire, 4);
            assert_rows(&pane, &[(0, 11), (5, 66)]);
        }
    }

    #[test]
    fn command_marks_fail_closed_on_event_overflow_then_recover() {
        for pending in [false, true] {
            let mut wire = setup(pending);
            wire.push_str("\x1b[2;5r");
            wire.push_str(&"\x1b[S\x1b[T".repeat(150));
            finish(&mut wire, pending);
            wire.push_str(&command(6, "FRESH", 9));
            let pane = pane(&wire, 4);
            assert_text(&pane, 5, "FRESH");
            assert_rows(&pane, &[(5, 9)]);
        }
    }

    #[test]
    fn r2_sync_new_command_after_scroll_keeps_its_row() {
        for (pending, explicit_end) in [(false, true), (true, true), (false, false), (true, false)]
        {
            let mut wire = "\x1b[2;5r\x1b[3;1H\x1b[?2026h\x1b[S".to_string();
            wire.push_str("\x1b]133;A\x07NEW\x1b]133;C\x07");
            if !pending {
                wire.push_str("\x1b]133;D;7\x07");
            }
            if explicit_end {
                wire.push_str("\x1b[?2026l");
            }
            finish(&mut wire, pending);
            let pane = pane(&wire, 4);
            assert_text(&pane, 2, "NEW");
            assert_rows(&pane, &[(2, 7)]);
        }
    }

    #[test]
    fn r2_sync_commands_before_between_and_after_scrolls() {
        for pending in [false, true] {
            let mut wire = command(1, "ABOVE", 11);
            wire.push_str(&command(6, "BELOW", 66));
            wire.push_str(&command(4, "BEFORE", 8));
            wire.push_str("\x1b[2;5r\x1b[?2026h\x1b[S");
            wire.push_str("\x1b[4;1H\x1b]133;A\x07MIDDLE\x1b]133;C\x07");
            if !pending {
                wire.push_str("\x1b]133;D;7\x07");
            }
            wire.push_str("\x1b[S");
            finish(&mut wire, pending);
            wire.push_str(&command(5, "AFTER", 9));
            wire.push_str("\x1b[?2026l");
            let pane = pane(&wire, 4);
            assert_text(&pane, 1, "BEFORE");
            assert_text(&pane, 2, "MIDDLE");
            assert_text(&pane, 4, "AFTER");
            assert_text(&pane, 0, "ABOVE");
            assert_text(&pane, 5, "BELOW");
            assert_rows(&pane, &[(0, 11), (5, 66), (1, 8), (2, 7), (4, 9)]);
        }
    }

    #[test]
    fn r2_sync_prompt_anchor_survives_scroll_before_c_and_cursor_movement() {
        for prompt_inside in [true, false] {
            let mut wire = "\x1b[2;5r\x1b[3;1H".to_string();
            if prompt_inside {
                wire.push_str("\x1b[?2026h\x1b[S\x1b[4;1H");
            }
            wire.push_str("\x1b]133;A\x07PROMPT");
            if !prompt_inside {
                wire.push_str("\x1b[?2026h\x1b[S");
            }
            wire.push_str("\x1b[5;1H\x1b]133;C\x07\x1b[T\x1b]133;D;7\x07\x1b[?2026l");
            let pane = pane(&wire, 4);
            let row = if prompt_inside { 4 } else { 2 };
            assert_text(&pane, row, "PROMPT");
            assert_rows(&pane, &[(i64::from(row), 7)]);
        }
    }

    #[test]
    fn r2_sync_new_marks_with_full_history() {
        for history in [0, 2, 8] {
            for pending in [false, true] {
                let mut wire = "\x1b[6;1H\n\n\n\n\n\n\n\n\n\n".to_string();
                wire.push_str(&command(3, "BEFORE", 8));
                wire.push_str("\x1b[6;1H\x1b[?2026h\n\r");
                wire.push_str("\x1b]133;A\x07NEW\x1b]133;C\x07");
                if !pending {
                    wire.push_str("\x1b]133;D;7\x07");
                }
                wire.push_str("\n\x1b[?2026l");
                finish(&mut wire, pending);
                let pane = pane(&wire, history);
                assert_text(&pane, 0, "BEFORE");
                assert_text(&pane, 4, "NEW");
                assert_rows(&pane, &[(0, 8), (4, 7)]);
            }
        }
    }

    #[test]
    fn r2_sync_alternate_markers_do_not_create_primary_results() {
        for mode in [47, 1047, 1049] {
            let mut wire = command(5, "BEFORE", 8);
            wire.push_str(&format!("\x1b[?2026h\x1b[?{mode}h\x1b[2;5r\x1b[S"));
            wire.push_str(&command(3, "ALT", 9));
            wire.push_str(&format!("\x1b[?{mode}l\x1b[2;5r\x1b[S"));
            wire.push_str(&command(3, "NEW", 7));
            wire.push_str("\x1b[?2026l");
            let pane = pane(&wire, 4);
            assert_text(&pane, 3, "BEFORE");
            assert_text(&pane, 2, "NEW");
            assert_rows(&pane, &[(3, 8), (2, 7)]);
        }
    }

    #[test]
    fn r2_native_fixture_forwards_replies_to_a_waiting_child() {
        pane("REPLY_FIXTURE", 4);
    }

    #[test]
    fn r2_sync_marks_and_text_stay_invisible_until_commit() {
        use std::time::Duration;
        let (wake_tx, wake_rx) = crossbeam_channel::bounded(1);
        let (resume_tx, resume_rx) = crossbeam_channel::bounded(1);
        let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let armed_for_wake = armed.clone();
        let waker = std::sync::Arc::new(move || {
            if armed_for_wake.load(std::sync::atomic::Ordering::Acquire) {
                wake_tx.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            }
        });
        struct Resume(
            crossbeam_channel::Sender<()>,
            std::sync::Arc<std::sync::atomic::AtomicBool>,
        );
        impl Drop for Resume {
            fn drop(&mut self) {
                self.1.store(false, std::sync::atomic::Ordering::Release);
                let _ = self.0.send(());
            }
        }
        let wire = "\x1b[2;5r\x1b[3;1H\x1b[?2026h\x1b[S\x1b]133;A\x07NEW\x1b]133;C\x07\x1b]133;D;7\x07\x1b]9;HOLD\x07\x1b[?2026l";
        // Stop the reader at the notification wake, before it can parse ESU
        // or service a timeout. This makes the mid-update observation atomic.
        let pane = pane_observed(wire, 4, waker, |pane, rx| {
            loop {
                wake_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                forward_replies(pane, rx);
                let held = pane
                    .drain_protocol_notifications()
                    .iter()
                    .any(|n| n.title == "HOLD");
                if held {
                    let resume = Resume(resume_tx.clone(), armed.clone());
                    assert_text(pane, 2, "   ");
                    assert_rows(pane, &[]);
                    drop(resume);
                    break;
                }
                resume_tx.send(()).unwrap();
            }
        });
        assert_text(&pane, 2, "NEW");
        assert_rows(&pane, &[(2, 7)]);
    }

    #[test]
    fn r2_sync_marker_bound_preserves_old_results_and_recovers() {
        let mut wire = command(1, "KEEP", 11);
        wire.push_str("\x1b[?2026h");
        for _ in 0..100 {
            wire.push_str(&command(3, "NEW", 7));
        }
        wire.push_str("\x1b[?2026l");
        wire.push_str(&command(6, "FRESH", 9));
        let pane = pane(&wire, 4);
        assert_text(&pane, 0, "KEEP");
        assert_text(&pane, 5, "FRESH");
        assert_rows(&pane, &[(0, 11), (5, 9)]);
    }

    #[test]
    fn r2_sync_alternate_markers_preserve_a_primary_pending_command() {
        for mode in [47, 1047, 1049] {
            let mut wire = setup(true);
            wire.push_str(&format!("\x1b[?2026h\x1b[?{mode}h\x1b[2;5r\x1b[S"));
            wire.push_str(&command(3, "ALT", 9));
            wire.push_str("\x1b]133;B\x07");
            wire.push_str(&format!(
                "\x1b[?{mode}l\x1b[2;5r\x1b[S\x1b]133;D;7\x07\x1b[?2026l"
            ));
            let pane = pane(&wire, 4);
            assert_text(&pane, 1, "TARGET");
            assert_rows(&pane, &[(0, 11), (5, 66), (1, 7)]);
        }
    }
}
