//! Kettle-only restore clear guard; see `restore_clear_guard.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::event::VoidListener;
use crate::term::test::TermSize;

const SHORT_SEED: &str = "old one\r\nold two\r\n-- Restored --\r\n";

struct Pane {
    term: Term<VoidListener>,
    parser: ansi::Processor,
    input: Arc<AtomicBool>,
}

impl Pane {
    /// A pane seeded like a restore; `arm` decides whether the guard is armed.
    fn seeded(columns: usize, lines: usize, seed: &str, arm: bool) -> Self {
        let mut term = Term::new(
            Config::default(),
            &TermSize::new(columns, lines),
            VoidListener,
        );
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, seed.as_bytes());
        let input = Arc::new(AtomicBool::new(false));
        if arm {
            term.arm_restore_clear_guard(input.clone());
            assert!(term.restore_clear_guard_armed());
        }
        Self {
            term,
            parser,
            input,
        }
    }

    /// Program output, as Kettle's reader thread delivers it.
    fn output(&mut self, bytes: &[u8]) {
        self.term.note_restore_clear_guard_output();
        self.parser.advance(&mut self.term, bytes);
    }

    fn rows(&self) -> Vec<String> {
        (0..self.term.screen_lines() as i32)
            .map(|line| {
                let row: String = self.term.grid[Line(line)][..]
                    .iter()
                    .map(|cell| cell.c)
                    .collect();
                row.trim_end().to_owned()
            })
            .collect()
    }

    fn state(&self) -> (Vec<String>, usize, Point) {
        (
            self.rows(),
            self.term.history_size(),
            self.term.grid.cursor.point,
        )
    }
}

/// Feed the same output to an armed pane and an unarmed one.
fn twins(seed: &str) -> (Pane, Pane) {
    (
        Pane::seeded(20, 6, seed, true),
        Pane::seeded(20, 6, seed, false),
    )
}

fn assert_kept_with_prompt(pane: &Pane, prompt_row: usize) {
    let rows = pane.rows();
    assert_eq!(rows[prompt_row - 1], "-- Restored --", "{rows:?}");
    assert_eq!(rows[prompt_row], "prompt$", "{rows:?}");
    assert!(
        rows[prompt_row + 1..].iter().all(String::is_empty),
        "{rows:?}"
    );
    assert_eq!(
        pane.term.grid.cursor.point,
        Point::new(Line(prompt_row as i32), Column(8))
    );
    assert!(!pane.term.restore_clear_guard_armed());
}

#[test]
fn tput_clear_keeps_the_restored_rows_above_the_prompt() {
    let mut pane = Pane::seeded(20, 6, SHORT_SEED, true);
    pane.output(b"\x1b[H\x1b[2Jprompt$ ");
    assert_eq!(&pane.rows()[..2], ["old one", "old two"]);
    assert_kept_with_prompt(&pane, 3);
    assert_eq!(pane.term.history_size(), 0);
}

#[test]
fn macos_clear_honours_ed3_and_keeps_the_rows_on_screen() {
    let mut pane = Pane::seeded(20, 6, SHORT_SEED, true);
    pane.output(b"\x1b[3J\x1b[H\x1b[2Jprompt$ ");
    assert_eq!(&pane.rows()[..2], ["old one", "old two"]);
    assert_kept_with_prompt(&pane, 3);
}

#[test]
fn linux_clear_keeps_the_rows_and_ed3_still_drops_history() {
    let mut pane = Pane::seeded(20, 6, SHORT_SEED, true);
    pane.output(b"\x1b[H\x1b[2J\x1b[3Jprompt$ ");
    assert_eq!(&pane.rows()[..2], ["old one", "old two"]);
    assert_kept_with_prompt(&pane, 3);
    assert_eq!(pane.term.history_size(), 0);
}

/// A restore taller than the pane leaves the cursor on the bottom row with
/// older rows in history. ED 3 still deletes that history in either order.
#[test]
fn tall_restore_keeps_the_visible_rows_in_each_clear_order() {
    let seed: String =
        (0..9).map(|n| format!("old {n}\r\n")).collect::<String>() + "-- Restored --\r\n";
    let orders: [(&[u8], usize); 3] = [
        (b"\x1b[H\x1b[2J", 5),
        (b"\x1b[3J\x1b[H\x1b[2J", 0),
        (b"\x1b[H\x1b[2J\x1b[3J", 0),
    ];
    for (clear, history) in orders {
        let mut pane = Pane::seeded(20, 6, &seed, true);
        assert_eq!(pane.term.grid.cursor.point.line, Line(5));
        assert_eq!(pane.term.history_size(), 5);
        pane.output(clear);
        pane.output(b"prompt$ ");
        assert_eq!(&pane.rows()[..4], ["old 5", "old 6", "old 7", "old 8"]);
        assert_kept_with_prompt(&pane, 5);
        assert_eq!(pane.term.history_size(), history);
        // Later output scrolls as usual.
        pane.output(b"\r\nnext");
        assert_eq!(pane.rows()[5], "next");
    }
}

#[test]
fn the_guard_is_one_shot() {
    let mut pane = Pane::seeded(20, 6, SHORT_SEED, true);
    pane.output(b"\x1b[H\x1b[2Jprompt$ ");
    assert_kept_with_prompt(&pane, 3);
    // A second clear behaves as upstream: every row goes to history.
    pane.output(b"\x1b[H\x1b[2J");
    assert_eq!(pane.term.history_size(), 4);
    assert!(pane.rows().iter().all(String::is_empty));
}

/// Characterisation: an unarmed terminal (main's behaviour) moves the rows to
/// history on `ESC[H ESC[2J`.
#[test]
fn an_unarmed_clear_moves_the_rows_to_history() {
    let mut pane = Pane::seeded(20, 6, SHORT_SEED, false);
    pane.output(b"\x1b[H\x1b[2Jprompt$ ");
    assert_eq!(pane.rows()[0], "prompt$");
    assert_eq!(pane.term.history_size(), 3);
}

#[test]
fn ed3_alone_never_touches_the_guard() {
    let tall: String =
        (0..9).map(|n| format!("old {n}\r\n")).collect::<String>() + "-- Restored --\r\n";
    let mut pane = Pane::seeded(20, 6, &tall, true);
    pane.output(b"\x1b[3J");
    assert_eq!(pane.term.history_size(), 0);
    assert!(pane.term.restore_clear_guard_armed());
    pane.output(b"\x1b[3J\x1b[H\x1b[2Jprompt$ ");
    assert_kept_with_prompt(&pane, 5);
}

/// Each of these must clear exactly as an unarmed terminal does and disarm,
/// so a later `ESC[H ESC[2J` also behaves as upstream.
#[test]
fn other_erases_behave_as_upstream_and_disarm() {
    let cases: [(&str, &[u8]); 8] = [
        ("ED 2 then home", b"\x1b[2J\x1b[H"),
        ("bare ED 2 away from home", b"\x1b[2J"),
        ("ED 0 from home", b"\x1b[H\x1b[J"),
        ("ED 1", b"\x1b[1J"),
        ("ISO protected ED 2", b"\x1bV\x1bW\x1b[H\x1b[2J"),
        ("DECSED", b"\x1b[H\x1b[?2J"),
        ("RIS", b"\x1bc"),
        ("DECALN", b"\x1b#8"),
    ];
    for (name, bytes) in cases {
        let (mut armed, mut plain) = twins(SHORT_SEED);
        for pane in [&mut armed, &mut plain] {
            pane.output(bytes);
        }
        assert_eq!(armed.state(), plain.state(), "{name}");
        assert!(!armed.term.restore_clear_guard_armed(), "{name}");
        for pane in [&mut armed, &mut plain] {
            pane.output(b"\x1b[H\x1b[2Jprompt$ ");
        }
        assert_eq!(armed.state(), plain.state(), "{name} then a clear");
    }
}

#[test]
fn the_alternate_screen_disarms_and_a_later_clear_behaves_as_upstream() {
    let (mut armed, mut plain) = twins(SHORT_SEED);
    for pane in [&mut armed, &mut plain] {
        pane.output(b"\x1b[?1049h\x1b[H\x1b[2Jvim\x1b[?1049l");
    }
    assert_eq!(armed.state(), plain.state());
    assert!(!armed.term.restore_clear_guard_armed());
    for pane in [&mut armed, &mut plain] {
        pane.output(b"\x1b[H\x1b[2Jprompt$ ");
    }
    assert_eq!(armed.state(), plain.state());
    assert_eq!(armed.rows()[0], "prompt$");
}

#[test]
fn a_primary_screen_scroll_disarms() {
    let cases: [(&str, &[u8]); 4] = [
        ("line feed at the bottom", b"\n\n\n"),
        ("scroll up", b"\x1b[S"),
        ("scroll down", b"\x1b[T"),
        ("insert line", b"\x1b[L"),
    ];
    for (name, bytes) in cases {
        let (mut armed, mut plain) = twins(SHORT_SEED);
        for pane in [&mut armed, &mut plain] {
            pane.output(bytes);
            pane.output(b"\x1b[H\x1b[2Jprompt$ ");
        }
        assert_eq!(armed.state(), plain.state(), "{name}");
    }
    // Plain output that does not scroll keeps the guard.
    let mut pane = Pane::seeded(20, 6, SHORT_SEED, true);
    pane.output(b"Last login\r\n");
    assert!(pane.term.restore_clear_guard_armed());
}

#[test]
fn user_input_disarms_at_the_next_decision() {
    let (mut armed, mut plain) = twins(SHORT_SEED);
    armed.input.store(true, Ordering::Release);
    assert!(!armed.term.restore_clear_guard_armed());
    for pane in [&mut armed, &mut plain] {
        pane.parser
            .advance(&mut pane.term, b"\x1b[H\x1b[2Jprompt$ ");
    }
    assert_eq!(armed.state(), plain.state());
}

#[test]
fn the_embedder_can_disarm() {
    let (mut armed, mut plain) = twins(SHORT_SEED);
    armed.term.disarm_restore_clear_guard();
    for pane in [&mut armed, &mut plain] {
        pane.output(b"\x1b[H\x1b[2Jprompt$ ");
    }
    assert_eq!(armed.state(), plain.state());
}

#[test]
fn the_alternate_screen_cannot_be_armed() {
    let mut pane = Pane::seeded(20, 6, "\x1b[?1049h", false);
    pane.term.arm_restore_clear_guard(pane.input.clone());
    assert!(!pane.term.restore_clear_guard_armed());
}

/// A resize before any output re-anchors to the cursor, which reflow keeps
/// directly below the divider.
#[test]
fn a_resize_before_output_reanchors() {
    let long = "a long restored row!".repeat(2);
    let seed = format!("{long}\r\nold two\r\n-- Restored --\r\n");
    for (columns, lines) in [(16, 6), (40, 6), (20, 4), (16, 3)] {
        let mut pane = Pane::seeded(20, 6, &seed, true);
        pane.term.resize(TermSize::new(columns, lines));
        assert!(pane.term.restore_clear_guard_armed(), "{columns}x{lines}");
        pane.output(b"\x1b[H\x1b[2Jprompt$ ");
        let row = pane.term.grid.cursor.point.line.0 as usize;
        assert!(row > 0, "{columns}x{lines}");
        let rows = pane.rows();
        assert_eq!(rows[row - 1], "-- Restored --", "{columns}x{lines}");
        assert_eq!(rows[row], "prompt$", "{columns}x{lines}");
        assert_eq!(rows[row - 2], "old two", "{columns}x{lines}");
    }
}

#[test]
fn a_resize_after_output_disarms() {
    let (mut armed, mut plain) = twins(SHORT_SEED);
    for pane in [&mut armed, &mut plain] {
        pane.output(b"x");
        pane.term.resize(TermSize::new(12, 6));
    }
    assert!(!armed.term.restore_clear_guard_armed());
    for pane in [&mut armed, &mut plain] {
        pane.output(b"\x1b[H\x1b[2Jprompt$ ");
    }
    assert_eq!(armed.state(), plain.state());
}

/// A reflow that bypasses `Term::resize` must not leave a stale anchor.
#[test]
fn a_geometry_change_outside_resize_falls_back() {
    let (mut armed, mut plain) = twins(SHORT_SEED);
    for pane in [&mut armed, &mut plain] {
        pane.term.grid_mut().resize(true, 6, 30);
        pane.output(b"\x1b[H\x1b[2Jprompt$ ");
    }
    assert_eq!(armed.state(), plain.state());
}
