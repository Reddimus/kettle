//! Bounded command outcomes anchored to primary-grid prompt rows.
use std::collections::VecDeque;

use alacritty_terminal::grid::Dimensions;

use super::{GraphicsEvent, GraphicsEventBatch, GraphicsScroll, GraphicsScrollDirection};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommandMark {
    pub row: u64,
    pub exit_code: i32,
}

/// One entry of the ring: a completed command's mark, and its output rows
/// `output..end` as stable row ids. `output` is the first row written after
/// OSC 133 C and `end` the first row after the output at D; a cursor past
/// column 0 at either mark sits on a row holding the command line or the
/// output's last line, so that row falls before or inside the span. Empty,
/// as `row..row`, when the command printed on no row of its own or a scroll
/// split the span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Completed {
    mark: CommandMark,
    output: u64,
    end: u64,
}

/// A command between C and D: its prompt row, and where its output began
/// unless a scroll took that row away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Started {
    row: u64,
    output: Option<u64>,
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
    pending: Option<Option<Started>>,
    marks: VecDeque<Completed>,
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
                self.begin(
                    self.prompt.filter(|row| *row >= grid.history_origin()),
                    row_after_cursor_text(grid),
                );
            }
            Boundary::End(code) => self.end(code, row_after_cursor_text(grid)),
            Boundary::Cancel => self.cancel_pending(),
        }
    }

    pub(super) fn discard_boundaries(&mut self) {
        self.prompt = None;
        self.pending = Some(None);
    }

    /// OSC 133 C: the first since the last D or prompt starts the command,
    /// with its output at row `output`.
    pub(super) fn begin(&mut self, row: Option<u64>, output: u64) {
        if self.pending.is_none() {
            self.pending = Some(row.map(|row| Started {
                row,
                output: Some(output),
            }));
        }
    }

    /// OSC 133 D with the row after the output, `end`.
    pub(super) fn end(&mut self, code: Option<i32>, end: u64) {
        if let Some(Some(Started { row, output })) = self.pending.take()
            && let Some(exit_code) = code
        {
            if self.marks.len() == 2048 {
                self.marks.pop_front();
            }
            let (output, end) = match output {
                Some(output) if output < end => (output, end),
                _ => (row, row),
            };
            self.marks.push_back(Completed {
                mark: CommandMark { row, exit_code },
                output,
                end,
            });
        }
    }

    fn prune(&mut self, origin: u64, end: u64) {
        self.prompt = self.prompt.filter(|row| (origin..end).contains(row));
        if let Some(started) = self.pending.as_mut() {
            *started = started.filter(|started| (origin..end).contains(&started.row));
        }
        self.marks
            .retain(|completed| (origin..end).contains(&completed.mark.row));
    }

    pub(super) fn retained(&mut self, origin: u64, end: u64) -> Vec<CommandMark> {
        self.prune(origin, end);
        self.marks.iter().map(|completed| completed.mark).collect()
    }

    /// The output rows `start..end` of the newest retained command whose
    /// output holds row `at`, or with no row, of the newest command. The
    /// span is cut to the retained rows `origin..retained_end`; `None` when
    /// nothing of it is left or the command printed nothing.
    pub(super) fn output_rows(
        &mut self,
        at: Option<u64>,
        origin: u64,
        retained_end: u64,
    ) -> Option<(u64, u64)> {
        self.prune(origin, retained_end);
        let span = |completed: &Completed| {
            let start = completed.output.max(origin);
            let end = completed.end.min(retained_end);
            (start < end).then_some((start, end))
        };
        match at {
            Some(at) => self
                .marks
                .iter()
                .rev()
                .filter_map(span)
                .find(|(start, end)| (*start..*end).contains(&at)),
            None => self.marks.back().and_then(span),
        }
    }

    /// [`Self::output_rows`] over `term`'s retained rows, as
    /// [`super::Terminal::command_output_rows`] answers: nothing while the
    /// alternate screen is up.
    pub(super) fn output_rows_in(
        &mut self,
        term: &super::Term<crate::EventProxy>,
        at: Option<u64>,
    ) -> Option<(u64, u64)> {
        if term
            .mode()
            .contains(alacritty_terminal::term::TermMode::ALT_SCREEN)
        {
            return None;
        }
        let grid = term.grid();
        let origin = grid.history_origin();
        let end = origin
            .saturating_add(grid.history_size() as u64)
            .saturating_add(grid.screen_lines() as u64);
        self.output_rows(at, origin, end)
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
        // A span survives only when every row in it moves by the same amount:
        // the map is one translation on each side of the screen and region
        // edges and inside the region, so testing the rows next to those
        // edges, and the span's own ends, covers every row between.
        let edges = [
            Some(old_screen_top),
            old_screen_top.checked_add(top as u64),
            old_screen_top.checked_add(bottom as u64),
            old_screen_top.checked_add(screen_lines as u64),
        ];
        let shift_span = |start: u64, end: u64| -> Option<(u64, u64)> {
            let last = end.checked_sub(1).filter(|&last| last >= start)?;
            let delta = |row: u64| Some(i128::from(shift(row)?) - i128::from(row));
            let moved = delta(start)?;
            let inside = |row: &u64| (start..=last).contains(row);
            let probes = edges
                .into_iter()
                .flatten()
                .flat_map(|edge| [edge.checked_sub(1), Some(edge)])
                .flatten()
                .filter(inside)
                .chain([last]);
            for row in probes {
                if delta(row)? != moved {
                    return None;
                }
            }
            Some((shift(start)?, shift(last)?.checked_add(1)?))
        };
        // A running command's output so far starts at `output` and ends at
        // the cursor, which can be anywhere on screen, so the same rule holds
        // it over every row from `output` to the bottom of the screen. An
        // `output` still below the screen has no rows to split yet.
        let screen_end = old_screen_top.saturating_add(screen_lines as u64);
        let shift_output = |output: u64| {
            if output >= screen_end {
                shift(output)
            } else {
                shift_span(output, screen_end).map(|(start, _)| start)
            }
        };
        self.prompt = self.prompt.and_then(shift);
        if let Some(started) = self.pending.as_mut() {
            *started = started.and_then(|Started { row, output }| {
                Some(Started {
                    row: shift(row)?,
                    output: output.and_then(shift_output),
                })
            });
        }
        self.marks.retain_mut(|completed| {
            let Some(row) = shift(completed.mark.row) else {
                return false;
            };
            (completed.output, completed.end) = if completed.output < completed.end {
                shift_span(completed.output, completed.end).unwrap_or((row, row))
            } else {
                (row, row)
            };
            completed.mark.row = row;
            true
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

/// The first row at or below the writing cursor with nothing written before
/// the cursor: its own row at column 0, else the row below.
fn row_after_cursor_text(
    grid: &alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>,
) -> u64 {
    let row = super::stable_grid_line_id(
        grid.history_origin(),
        grid.history_size(),
        grid.cursor.point.line.0,
    );
    if grid.cursor.point.column.0 == 0 {
        row
    } else {
        row.saturating_add(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet(row: u64, exit_code: i32) -> CommandMark {
        CommandMark { row, exit_code }
    }

    fn spans(marks: &CommandMarks) -> Vec<(u64, u64, u64)> {
        marks
            .marks
            .iter()
            .map(|completed| (completed.mark.row, completed.output, completed.end))
            .collect()
    }

    #[test]
    fn completed_status_requires_a_command_and_known_exit_code() {
        let mut marks = CommandMarks::default();
        marks.end(Some(1), 1);
        marks.begin(Some(10), 11);
        marks.begin(Some(11), 12); // duplicate C from a compound bash command
        marks.end(Some(0), 11);
        marks.end(Some(9), 11); // duplicate D cannot change a completed command
        marks.begin(Some(12), 13);
        marks.end(Some(-1), 13);
        marks.begin(Some(13), 14);
        marks.end(None, 14);
        assert_eq!(marks.retained(0, 20), vec![quiet(10, 0), quiet(12, -1)]);
        assert_eq!(marks.retained(11, 20), vec![quiet(12, -1)]);
        marks.clear();
        assert!(marks.retained(0, 20).is_empty());
    }

    #[test]
    fn outcomes_are_bounded_and_reflow_cancels_pending_commands() {
        let mut marks = CommandMarks::default();
        for row in 0..3000 {
            marks.begin(Some(row), row + 1);
            marks.end(Some(2), row + 3);
        }
        let retained = marks.retained(0, 4000);
        assert_eq!(retained.len(), 2048);
        assert_eq!(retained[0].row, 952);
        assert_eq!(spans(&marks).len(), 2048, "spans live in the same ring");
        assert_eq!(spans(&marks)[0], (952, 953, 955));
        marks.begin(Some(3001), 3002);
        marks.clear();
        marks.end(Some(0), 3003);
        assert!(marks.retained(0, 4000).is_empty());
    }

    /// The first C starts the output, D ends it, and a lookup by row finds
    /// the newest span holding that row. The span is cut to the retained
    /// rows; a command with an empty or inverted span has none.
    #[test]
    fn output_rows_come_from_the_first_c_and_the_d() {
        let mut marks = CommandMarks::default();
        marks.begin(Some(10), 11);
        marks.begin(Some(10), 13); // bash's C before PROMPT_COMMAND
        marks.end(Some(0), 14);
        marks.begin(Some(14), 15);
        marks.end(Some(0), 15); // `cd`: nothing printed
        marks.begin(Some(15), 18);
        marks.end(Some(1), 16); // the program moved the cursor up
        assert_eq!(
            marks.retained(0, 30),
            vec![quiet(10, 0), quiet(14, 0), quiet(15, 1)]
        );
        assert_eq!(
            spans(&marks),
            vec![(10, 11, 14), (14, 14, 14), (15, 15, 15)],
            "an empty or inverted span is kept as the prompt row"
        );
        assert_eq!(marks.output_rows(None, 0, 30), None, "newest is empty");
        assert_eq!(marks.output_rows(Some(11), 0, 30), Some((11, 14)));
        assert_eq!(marks.output_rows(Some(13), 0, 30), Some((11, 14)));
        assert_eq!(marks.output_rows(Some(10), 0, 30), None, "prompt row");
        assert_eq!(marks.output_rows(Some(14), 0, 30), None, "next prompt");
        marks.begin(Some(16), 17);
        marks.end(Some(0), 19);
        assert_eq!(marks.output_rows(None, 0, 30), Some((17, 19)));
        assert_eq!(marks.output_rows(Some(18), 0, 30), Some((17, 19)));
        // Rows past the retained grid are cut off, and marks there dropped.
        assert_eq!(marks.output_rows(Some(11), 0, 13), Some((11, 13)));
        assert_eq!(marks.retained(0, 30).len(), 1);
        // The prompt row left history: the mark and its span go with it.
        assert_eq!(marks.output_rows(Some(11), 11, 30), None);
        assert!(marks.retained(0, 30).is_empty());
    }

    /// A span is cut to the rows scrollback still holds, so a selection gets
    /// the rows that remain. A span normally leaves with its prompt row; one
    /// that starts above its prompt row can outlive its first rows.
    #[test]
    fn a_span_partly_gone_from_scrollback_keeps_the_rows_that_remain() {
        let mut marks = CommandMarks::default();
        marks.begin(Some(10), 8); // C written above the prompt row
        marks.end(Some(0), 12);
        assert_eq!(marks.output_rows(None, 0, 30), Some((8, 12)));
        assert_eq!(marks.output_rows(None, 9, 30), Some((9, 12)));
        assert_eq!(marks.output_rows(Some(8), 9, 30), None, "row 8 is gone");
        assert_eq!(marks.output_rows(Some(9), 9, 30), Some((9, 12)));
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
        let raw = pane_observed(wire, history, std::sync::Arc::new(|| {}), |_, _| {});
        // Unix PTYs preserve the wire. ConPTY renders it into a new VT stream;
        // its native contract is covered separately below.
        #[cfg(unix)]
        {
            let native = native_pane(wire, history);
            assert_eq!(relative_rows(&raw), relative_rows(&native));
            use alacritty_terminal::index::{Column, Line};
            let raw_term = raw.term.lock().unwrap();
            let native_term = native.term.lock().unwrap();
            for row in 0..6 {
                for col in 0..80 {
                    assert_eq!(
                        raw_term.grid()[Line(row)][Column(col)].c,
                        native_term.grid()[Line(row)][Column(col)].c,
                        "raw/Unix PTY text at row {row}, column {col}"
                    );
                }
            }
        }
        raw
    }

    fn native_pane(wire: &str, history: usize) -> super::super::Terminal {
        fixture(wire, history, std::sync::Arc::new(|| {}), |_, _| {}, false)
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
        fixture(wire, history, waker, observe, true)
    }

    fn fixture(
        wire: &str,
        history: usize,
        waker: crate::Waker,
        observe: impl FnOnce(
            &super::super::Terminal,
            &crossbeam_channel::Receiver<super::super::TermEvent>,
        ),
        raw: bool,
    ) -> super::super::Terminal {
        use super::super::{
            CursorShape, LaunchOverrides, PtyGeometry, PtyOutputSender, Terminal,
            TerminalCapabilities, WorkingDirectoryPolicy,
        };
        use std::time::{Duration, Instant};
        let (tx, rx) = crossbeam_channel::unbounded();
        let (input_tx, input_rx) = crossbeam_channel::bounded(1);
        let (output_tx, output_rx) = crossbeam_channel::bounded(2);
        if wire == "REPLY_FIXTURE" {
            tx.send(super::super::TermEvent::PtyWrite("REPLY\r\n".into()))
                .unwrap();
        }
        // Opening /dev/ptmx (posix_openpt, under openpty) on macOS fails now
        // and then with errno -6 while other threads or processes open
        // pseudoterminals at the same moment, with far fewer in use than
        // kern.tty.ptmx_max allows. Open one fixture terminal at a time, and
        // retry that failure a few times for opens elsewhere on the machine.
        static OPENING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let opening = OPENING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut retries = 0;
        let pane = loop {
            let created =
                super::super::test_input::with_input(raw.then(|| input_tx.clone()), || {
                    Terminal::new_with_launch_environment(
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
                        PtyGeometry::from_cell_size(80, 6, 8, 16),
                        false,
                        CursorShape::Block,
                        None,
                        "xterm-256color",
                        "truecolor",
                        &[(FIXTURE_ENV.into(), if raw { "" } else { wire }.into())],
                        &LaunchOverrides::default(),
                        false,
                        false,
                        &crate::shell_startup::ShellStartup::default(),
                        TerminalCapabilities::default(),
                        WorkingDirectoryPolicy::FallbackToHome,
                        tx.clone(),
                        waker.clone(),
                        raw.then(|| PtyOutputSender::lossless(output_tx.clone())),
                    )
                });
            match created {
                Err(error) if retries < 4 && format!("{error:#}").contains("failed to openpty") => {
                    eprintln!("command-mark fixture: retrying after {error:#}");
                    retries += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                created => break created,
            }
        };
        drop(opening);
        // The terminal holds the only senders, as it did before a retry.
        drop((tx, input_tx, output_tx));
        let pane = pane.expect("command-mark fixture");
        #[cfg(windows)]
        let mut pane = pane;
        eprintln!(
            "command-mark fixture owner={} started PID {:?}, raw={raw}",
            std::process::id(),
            pane.child_pid()
        );
        let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let bytes = [b"\x1b[H\x1b[2J".as_slice(), wire.as_bytes()].concat();
            if raw {
                assert!(bytes.len() <= super::super::PTY_READ_BUFFER_BYTES);
                let input = input_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                let generation = pane.pty_read_progress.mark_chunk_read();
                input
                    .send_timeout(
                        Some((generation, bytes.clone(), bytes.len())),
                        Duration::from_secs(2),
                    )
                    .unwrap();
                input.send_timeout(None, Duration::from_secs(2)).unwrap();
                drop(input);
                // Native output is drained independently. Answer ConPTY's
                // inherited-cursor query even though its bytes bypass this parser.
                #[cfg(windows)]
                pane.write(b"\x1b[1;1R");
            }
            observe(&pane, &rx);
            let deadline = Instant::now() + Duration::from_secs(15);
            while !pane.take_exited() {
                forward_replies(&pane, &rx);
                #[cfg(windows)]
                if !raw && pane.child_exit_code().is_some() {
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
            assert_eq!(pane.child_exit_code(), Some(0));
            if raw {
                let captured: Vec<_> = output_rx.try_iter().flatten().collect();
                assert_eq!(captured, bytes, "raw fixture changed the original wire");
            }
        }));
        if observed.is_err() && pane.child_exit_code().is_none() {
            pane.kill().expect("kill owned command-mark fixture");
            let deadline = Instant::now() + Duration::from_secs(2);
            while pane.child_exit_code().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        #[cfg(windows)]
        pane.begin_pty_output_close().expect("close fixture ConPTY");
        let code = pane.child_exit_code();
        eprintln!(
            "command-mark fixture reaped PID {:?}, code={code:?}",
            pane.child_pid()
        );
        assert!(code.is_some(), "owned command-mark child was not reaped");
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

    #[test]
    fn command_mark_raw_input_preserves_wire_and_scroll_semantics() {
        let mut wire = setup(false);
        wire.push_str("\x1b[2;5r\x1b[S");
        let pane = pane_observed(&wire, 4, std::sync::Arc::new(|| {}), |_, _| {});
        assert_text(&pane, 1, "TARGET");
        assert_rows(&pane, &[(0, 11), (5, 66), (1, 7)]);
    }

    #[test]
    fn command_mark_native_transport_delivers_text_and_outcome() {
        let pane = native_pane(&command(3, "TARGET", 7), 4);
        assert_text(&pane, 2, "TARGET");
        assert_rows(&pane, &[(2, 7)]);
    }

    /// `wire` parsed into a bare 80x6 engine with `history` rows of
    /// scrollback, after the fixture's clear, and its command marks kept as
    /// the reader keeps them: the real extractor splits off OSC 133, the VT
    /// parser takes the rest, and each text chunk's scroll journal reaches the
    /// marks before the next mark applies. There is no pseudoterminal, child
    /// or reader thread, and no DEC 2026: these wires never synchronize.
    struct Parsed {
        term: super::super::Term<crate::EventProxy>,
        marks: CommandMarks,
        _events: crossbeam_channel::Receiver<super::super::TermEvent>,
    }

    fn parse(wire: &str, history: usize) -> Parsed {
        use kettle_vt::{Chunk, Extractor, PromptKind};
        let (tx, events) = crossbeam_channel::unbounded();
        let mut term = super::super::Term::new(
            super::super::TermConfig {
                scrolling_history: history,
                ..super::super::TermConfig::default()
            },
            &super::super::TermSize::new(80, 6),
            crate::EventProxy::new(tx, std::sync::Arc::new(|| {})),
        );
        let mut processor: alacritty_terminal::vte::ansi::Processor = Default::default();
        let mut marks = CommandMarks::default();
        let wire = [b"\x1b[H\x1b[2J".as_slice(), wire.as_bytes()].concat();
        for chunk in Extractor::for_shell().feed(&wire) {
            // The reader's OSC 133 arms.
            let boundary = match chunk {
                Chunk::Pass(bytes) | Chunk::Terminal(bytes) => {
                    processor.advance(&mut term, &bytes);
                    marks.apply_batch(&term.take_graphics_events());
                    continue;
                }
                Chunk::Prompt(
                    PromptKind::PromptStart
                    | PromptKind::IntegratedPromptStart(_)
                    | PromptKind::CorrelationDisabledPromptStart,
                ) => Boundary::PromptStart,
                Chunk::Prompt(PromptKind::OutputStart | PromptKind::CorrelatedOutputStart(_)) => {
                    Boundary::Begin
                }
                Chunk::Prompt(
                    PromptKind::CommandEnd(code) | PromptKind::CorrelatedCommandEnd(code, _),
                ) => Boundary::End(code),
                Chunk::Prompt(PromptKind::CommandStart) => Boundary::Cancel,
                _ => continue,
            };
            marks.apply_boundary(boundary, &term);
        }
        Parsed {
            term,
            marks,
            _events: events,
        }
    }

    impl Parsed {
        fn screen_top(&self) -> u64 {
            use alacritty_terminal::grid::Dimensions;
            let grid = self.term.grid();
            grid.history_origin() + grid.history_size() as u64
        }

        /// The output rows of the command holding relative row `at`, or of
        /// the newest command, relative to the active screen's top, as
        /// `Terminal::command_output_rows` finds them.
        fn output(&mut self, at: Option<i64>) -> Option<(i128, i128)> {
            let top = self.screen_top();
            let at = at.map(|row| top.checked_add_signed(row).unwrap());
            let (start, end) = self.marks.output_rows_in(&self.term, at)?;
            Some((
                i128::from(start) - i128::from(top),
                i128::from(end) - i128::from(top),
            ))
        }

        /// Completed commands' prompt rows, relative to the screen's top, and
        /// exit codes.
        fn rows(&mut self) -> Vec<(i128, i32)> {
            use alacritty_terminal::grid::Dimensions;
            let top = self.screen_top();
            let end = top + self.term.grid().screen_lines() as u64;
            let origin = self.term.grid().history_origin();
            self.marks
                .retained(origin, end)
                .iter()
                .map(|mark| (i128::from(mark.row) - i128::from(top), mark.exit_code))
                .collect()
        }

        fn text(&self, row: i32) -> String {
            use alacritty_terminal::index::{Column, Line};
            (0..80)
                .map(|col| self.term.grid()[Line(row)][Column(col)].c)
                .collect::<String>()
                .trim_end()
                .to_string()
        }
    }

    const PROMPT: &str = "\x1b]133;A\x07$ ";

    fn two_line_command() -> String {
        format!("{PROMPT}cmd\x1b]133;C\x07\r\nout1\r\nout2\r\n\x1b]133;D;0\x07{PROMPT}")
    }

    /// `A … C \r\n out1 \r\n out2 \r\n D;0 A …`: the output is exactly the
    /// two rows between the command line and the next prompt, wherever the
    /// shell puts C and D on the row it is writing, and only rows inside
    /// those find it.
    #[test]
    fn command_output_rows_are_the_rows_between_the_command_line_and_the_next_prompt() {
        for wire in [
            // C right after the command line, D at the start of the next row.
            two_line_command(),
            // C at the start of the first output row, as bash, zsh and fish do.
            format!("{PROMPT}cmd\r\n\x1b]133;C\x07out1\r\nout2\r\n\x1b]133;D;0\x07{PROMPT}"),
            // Output without a final newline: D on its last row.
            format!("{PROMPT}cmd\x1b]133;C\x07\r\nout1\r\nout2\x1b]133;D;0\x07\r\n{PROMPT}"),
        ] {
            let mut parsed = parse(&wire, 4);
            let text: Vec<_> = (0..4).map(|row| parsed.text(row)).collect();
            assert_eq!(text, ["$ cmd", "out1", "out2", "$"], "{wire:?}");
            assert_eq!(parsed.rows(), [(0, 0)]);
            assert_eq!(parsed.output(None), Some((1, 3)), "{wire:?}");
            assert_eq!(parsed.output(Some(1)), Some((1, 3)));
            assert_eq!(parsed.output(Some(2)), Some((1, 3)));
            assert_eq!(parsed.output(Some(0)), None, "the command line");
            assert_eq!(parsed.output(Some(3)), None, "the next prompt");
        }
    }

    /// No C, no command: nothing to select, though the rows hold text. A
    /// command that printed nothing has no output either.
    #[test]
    fn a_command_without_c_or_output_has_no_output_rows() {
        let mut no_c = parse(
            &format!("{PROMPT}cmd\r\nout1\r\n\x1b]133;D;0\x07{PROMPT}"),
            4,
        );
        assert_eq!(no_c.text(1), "out1");
        assert_eq!(no_c.rows(), []);
        assert_eq!(no_c.output(None), None);
        assert_eq!(no_c.output(Some(1)), None);

        let mut quiet = parse(
            &format!("{PROMPT}cd\r\n\x1b]133;C\x07\x1b]133;D;0\x07{PROMPT}"),
            4,
        );
        assert_eq!(quiet.rows(), [(0, 0)]);
        assert_eq!(quiet.output(None), None);
    }

    /// The alternate screen hides the output as it hides the marks, and
    /// leaving it shows them again.
    #[test]
    fn output_rows_are_hidden_on_the_alternate_screen() {
        for mode in [47, 1047, 1049] {
            let mut alternate = parse(&format!("{}\x1b[?{mode}h", two_line_command()), 4);
            assert_eq!(alternate.output(None), None, "?{mode}h");
            let mut back = parse(
                &format!("{}\x1b[?{mode}h\x1b[?{mode}l", two_line_command()),
                4,
            );
            assert_eq!(back.output(None), Some((1, 3)), "?{mode}l");
        }
    }

    /// A column reflow drops the output rows with the marks. This one runs
    /// the wire through a real `Terminal` (one raw-input fixture, so one
    /// pseudoterminal): reflow is `Terminal::try_resize_geometry`'s work, and
    /// every `Terminal` owns a pseudoterminal. It is also the end-to-end run of
    /// the wire through the reader thread to `Terminal::command_output_rows`.
    #[test]
    fn output_rows_are_dropped_by_reflow() {
        let mut pane = pane_observed(
            &two_line_command(),
            4,
            std::sync::Arc::new(|| {}),
            |_, _| {},
        );
        let top = {
            use alacritty_terminal::grid::Dimensions;
            let term = pane.term.lock().unwrap();
            term.grid().history_origin() + term.grid().history_size() as u64
        };
        assert_eq!(pane.command_output_rows(None), Some((top + 1, top + 3)));
        assert_eq!(
            pane.command_output_rows(Some(top + 2)),
            Some((top + 1, top + 3))
        );
        // The grid reflows before the native resize, which may refuse the
        // pseudoterminal of a child that has exited.
        let _ = pane.try_resize_geometry(super::super::PtyGeometry::from_cell_size(60, 6, 8, 16));
        assert_eq!(pane.cols, 60, "the grid reflowed");
        assert_eq!(pane.command_output_rows(None), None);
        assert_eq!(relative_rows(&pane), vec![]);
    }

    /// The span follows scrolls that move all of it together, into history
    /// as well, and is dropped, leaving the tick, by one that moves part of
    /// it or scrolls a row of it away.
    #[test]
    fn output_rows_follow_whole_scrolls_and_drop_split_ones() {
        for (scroll, span, prompt) in [
            // The whole screen moves down a row.
            ("\x1b[T", Some((2, 4)), 1),
            // A region scroll pushes the span's first row out of the region;
            // row 0 is outside it.
            ("\x1b[2;5r\x1b[2;1H\x1b[M", None, 0),
            ("\x1b[3;6r\x1b[S", None, 0),
            // A region holding the span moves it down a row.
            ("\x1b[2;6r\x1b[T", Some((2, 4)), 0),
            // A region edge splits the span.
            ("\x1b[3;6r\x1b[T", None, 0),
            // Text pushed up into history keeps its ids.
            ("\x1b[6;1H\n\n", Some((-1, 1)), -2),
            // Narrow margins cannot move whole rows.
            ("\x1b[?69h\x1b[2;5s\x1b[2;5r\x1b[S", None, 0),
        ] {
            let mut scrolled = parse(&format!("{}{scroll}", two_line_command()), 4);
            assert_eq!(scrolled.output(None), span, "{scroll:?}");
            assert_eq!(scrolled.rows(), [(prompt, 0)], "{scroll:?}");
        }
    }

    /// A running command's output so far is held to the same rule: a region
    /// scroll that moves part of it before D leaves the command no output
    /// rows and keeps its tick. Output pushed up into history whole, or a
    /// region scroll above it, keeps them.
    #[test]
    fn a_scroll_that_splits_a_running_commands_output_drops_it() {
        let running = format!("{PROMPT}cmd\x1b]133;C\x07\r\nout1\r\nout2\r\nout3\r\n");
        // Rows 2 to 5 scroll up: out2 goes, out3 moves up beside out1.
        let mut split = parse(
            &format!("{running}\x1b[3;6r\x1b[S\x1b[r\x1b[5;1H\x1b]133;D;0\x07{PROMPT}"),
            4,
        );
        assert_eq!(split.text(1), "out1");
        assert_eq!(split.text(2), "out3");
        assert_eq!(split.output(None), None);
        assert_eq!(split.rows(), [(0, 0)]);

        // Eight rows of output push the command line four rows into history.
        let lines: String = (1..=8).map(|n| format!("l{n}\r\n")).collect();
        let mut long = parse(
            &format!("{PROMPT}cmd\x1b]133;C\x07\r\n{lines}\x1b]133;D;0\x07{PROMPT}"),
            4,
        );
        assert_eq!(long.text(-3), "l1");
        assert_eq!(long.output(None), Some((-3, 5)));
        assert_eq!(long.rows(), [(-4, 0)]);

        // Rows 1 and 2 scroll; the command line and output start below them.
        let mut above = parse(
            &format!(
                "\x1b[4;1H{PROMPT}cmd\x1b]133;C\x07\r\nout1\x1b[2;3r\x1b[S\x1b[r\x1b[6;1H\x1b]133;D;0\x07"
            ),
            4,
        );
        assert_eq!(above.text(4), "out1");
        assert_eq!(above.output(None), Some((4, 5)));
        assert_eq!(above.rows(), [(3, 0)]);
    }

    /// A full reset (RIS) through the parser clears the output rows with the
    /// marks; a command after it gets rows of its own.
    #[test]
    fn a_full_reset_clears_output_rows() {
        let mut reset = parse(&format!("{}\x1bc", two_line_command()), 4);
        assert_eq!(reset.output(None), None);
        assert_eq!(reset.rows(), []);
        let mut again = parse(
            &format!("{}\x1bc{}", two_line_command(), two_line_command()),
            4,
        );
        assert_eq!(again.output(None), Some((1, 3)));
        assert_eq!(again.rows(), [(0, 0)]);
    }

    #[test]
    fn command_mark_fixture_reaps_child_after_observer_panics() {
        let panic = std::panic::catch_unwind(|| {
            fixture(
                "REPLY_FIXTURE",
                4,
                std::sync::Arc::new(|| {}),
                |_, _| panic!("observer sentinel"),
                false,
            );
        })
        .expect_err("fixture must resume the observer panic after reaping");
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"observer sentinel"));
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

    fn relative_rows(pane: &super::super::Terminal) -> Vec<(i128, i32)> {
        use alacritty_terminal::grid::Dimensions;
        let screen_top = {
            let term = pane.term.lock().unwrap();
            term.grid().history_origin() + term.grid().history_size() as u64
        };
        pane.command_marks()
            .iter()
            .map(|m| (i128::from(m.row) - i128::from(screen_top), m.exit_code))
            .collect()
    }

    fn assert_rows(pane: &super::super::Terminal, rows: &[(i64, i32)]) {
        let actual = relative_rows(pane);
        let expected: Vec<_> = rows
            .iter()
            .map(|&(row, code)| (i128::from(row), code))
            .collect();
        assert_eq!(
            actual, expected,
            "command rows relative to active screen top"
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
        native_pane("REPLY_FIXTURE", 4);
    }

    #[test]
    fn r2_sync_marks_and_text_stay_invisible_until_commit() {
        sync_marks_and_text_stay_invisible_until_commit(true);
        #[cfg(unix)]
        sync_marks_and_text_stay_invisible_until_commit(false);
    }

    fn sync_marks_and_text_stay_invisible_until_commit(raw: bool) {
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
        let pane = fixture(
            wire,
            4,
            waker,
            |pane, rx| {
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
            },
            raw,
        );
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
