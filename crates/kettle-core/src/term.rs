//! A single terminal instance: PTY + `alacritty_terminal` grid + VT parser,
//! driven by a dedicated reader thread.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::Processor;
use anyhow::Result;
use kettle_vt::{Chunk, Extractor, PromptKind};
use portable_pty::{CommandBuilder, PtySize};

use crate::event::{EventProxy, TermEvent, Waker};
use crate::images::{Images, Placement};

/// Grid dimensions passed to `alacritty_terminal` (implements `Dimensions`).
#[derive(Clone, Copy)]
pub struct TermSize {
    pub columns: usize,
    pub screen_lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

pub type SharedTerm = Arc<Mutex<Term<EventProxy>>>;

pub struct Terminal {
    pub term: SharedTerm,
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    reader_thread: Option<JoinHandle<()>>,
    pub cols: usize,
    pub rows: usize,
    pub images: Images,
    /// Absolute lines (history-aware) where OSC 133 prompts started.
    pub prompts: Arc<Mutex<Vec<i64>>>,
    /// Latest working directory reported via OSC 7.
    pub cwd: Arc<Mutex<Option<String>>>,
    /// The argv this pane was launched with (empty = default shell);
    /// persisted so SSH/remote panes can be restored.
    pub argv: Vec<String>,
    cell_px: Arc<Mutex<(u16, u16)>>,
}

impl Terminal {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        argv: &[String],
        cwd: Option<&str>,
        scrollback: usize,
        cols: usize,
        rows: usize,
        cell_w: u16,
        cell_h: u16,
        event_tx: crossbeam_channel::Sender<TermEvent>,
        waker: Waker,
    ) -> Result<Terminal> {
        let pty = portable_pty::native_pty_system();
        let pair = pty.openpty(PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: cell_w * cols as u16,
            pixel_height: cell_h * rows as u16,
        })?;

        let mut cmd = match argv.split_first() {
            Some((prog, rest)) => {
                let mut c = CommandBuilder::new(prog);
                for a in rest {
                    c.arg(a);
                }
                c
            }
            None => CommandBuilder::new_default_prog(),
        };
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "kettle");
        match cwd {
            Some(d) if std::path::Path::new(d).is_dir() => cmd.cwd(d),
            _ => {
                if let Some(home) = std::env::var_os("HOME") {
                    cmd.cwd(home);
                }
            }
        }
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;

        let proxy = EventProxy::new(event_tx, waker.clone());
        let tconf = TermConfig {
            scrolling_history: scrollback,
            kitty_keyboard: true,
            ..TermConfig::default()
        };
        let term = Term::new(
            tconf,
            &TermSize {
                columns: cols,
                screen_lines: rows,
            },
            proxy.clone(),
        );
        let term: SharedTerm = Arc::new(Mutex::new(term));

        let images: Images = Arc::new(Mutex::new(Vec::new()));
        let prompts: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
        let cwd_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(cwd.map(|s| s.to_string())));
        let cell_px = Arc::new(Mutex::new((cell_w.max(1), cell_h.max(1))));

        let reader_thread = {
            let term = term.clone();
            let images = images.clone();
            let prompts = prompts.clone();
            let cwd_cell = cwd_cell.clone();
            let cell_px = cell_px.clone();
            std::thread::Builder::new()
                .name("kettle-pty-reader".into())
                .spawn(move || {
                    let mut processor: Processor = Processor::new();
                    let mut extractor = Extractor::new();
                    let mut buf = [0u8; 65536];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => {
                                proxy.send_event_exit();
                                break;
                            }
                            Ok(n) => {
                                for chunk in extractor.feed(&buf[..n]) {
                                    match chunk {
                                        Chunk::Pass(bytes) => {
                                            if let Ok(mut t) = term.lock() {
                                                processor.advance(&mut *t, &bytes);
                                            }
                                        }
                                        Chunk::Image(placed) => {
                                            place_image(
                                                &term,
                                                &images,
                                                &cell_px,
                                                &mut processor,
                                                placed,
                                            );
                                        }
                                        Chunk::DeleteImages { all, id } => {
                                            if let Ok(mut v) = images.lock() {
                                                if all {
                                                    v.clear();
                                                } else {
                                                    v.retain(|p| {
                                                        id.is_none_or(|x| p.id != Some(x))
                                                    });
                                                }
                                            }
                                        }
                                        Chunk::Prompt(PromptKind::PromptStart) => {
                                            if let Ok(t) = term.lock() {
                                                let rc = t.renderable_content();
                                                let line = rc.cursor.point.line.0 as i64;
                                                let abs = t.grid().history_size() as i64 + line;
                                                if let Ok(mut m) = prompts.lock()
                                                    && m.last() != Some(&abs)
                                                {
                                                    m.push(abs);
                                                    if m.len() > 2048 {
                                                        let d = m.len() - 2048;
                                                        m.drain(0..d);
                                                    }
                                                }
                                            }
                                        }
                                        Chunk::Prompt(_) => {}
                                        Chunk::Cwd(path) => {
                                            if let Ok(mut c) = cwd_cell.lock() {
                                                *c = Some(path);
                                            }
                                        }
                                    }
                                }
                                (waker)();
                            }
                        }
                    }
                })?
        };

        Ok(Terminal {
            term,
            master: pair.master,
            writer: Arc::new(Mutex::new(writer)),
            child: Arc::new(Mutex::new(child)),
            reader_thread: Some(reader_thread),
            cols,
            rows,
            images,
            prompts,
            cwd: cwd_cell,
            argv: argv.to_vec(),
            cell_px,
        })
    }

    /// Last working directory reported via OSC 7, if any.
    pub fn current_dir(&self) -> Option<String> {
        self.cwd.lock().ok().and_then(|c| c.clone())
    }

    /// Absolute prompt-start lines recorded via OSC 133.
    pub fn prompt_marks(&self) -> Vec<i64> {
        self.prompts.lock().map(|m| m.clone()).unwrap_or_default()
    }

    /// Image placements for this terminal (cloned cheaply; `ImageData` is
    /// `Arc`-backed).
    pub fn placements(&self) -> Vec<Placement> {
        self.images.lock().map(|v| v.clone()).unwrap_or_default()
    }

    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize, cell_w: u16, cell_h: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        let _ = self.master.resize(PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: cell_w * cols as u16,
            pixel_height: cell_h * rows as u16,
        });
        if let Ok(mut p) = self.cell_px.lock() {
            *p = (cell_w.max(1), cell_h.max(1));
        }
        if let Ok(mut t) = self.term.lock() {
            t.resize(TermSize {
                columns: cols,
                screen_lines: rows,
            });
        }
    }

    /// Has the child process exited?
    pub fn child_exited(&self) -> bool {
        self.child
            .lock()
            .ok()
            .and_then(|mut c| c.try_wait().ok().flatten())
            .is_some()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
        }
        if let Some(h) = self.reader_thread.take() {
            let _ = h.join();
        }
    }
}

impl EventProxy {
    fn send_event_exit(&self) {
        use alacritty_terminal::event::EventListener;
        self.send_event(TermEvent::Exit);
    }
}

/// Anchor a decoded image at the cursor, then push the cursor below it so
/// subsequent shell output flows after the image (kitty/iTerm2/Sixel all
/// place at the cursor and advance).
fn place_image(
    term: &SharedTerm,
    images: &Images,
    cell_px: &Arc<Mutex<(u16, u16)>>,
    processor: &mut Processor,
    placed: kettle_vt::Placed,
) {
    let kettle_vt::Placed { img: data, id, z } = placed;
    let (cw, chh) = cell_px.lock().map(|p| *p).unwrap_or((8, 16));
    let cw = cw.max(1) as u32;
    let chh = chh.max(1) as u32;
    let cell_cols = data.width.div_ceil(cw) as usize;
    let cell_rows = data.height.div_ceil(chh) as usize;

    let Ok(mut t) = term.lock() else {
        return;
    };
    let (abs_line, col) = {
        let rc = t.renderable_content();
        let cur = rc.cursor.point;
        let hist = t.grid().history_size() as i64;
        (hist + cur.line.0 as i64, cur.column.0)
    };
    if let Ok(mut v) = images.lock() {
        v.push(Placement {
            abs_line,
            col,
            cell_cols,
            cell_rows,
            img: data,
            id,
            z,
        });
        if v.len() > 512 {
            let drop = v.len() - 512;
            v.drain(0..drop);
        }
    }
    // Reserve the rows the image occupies.
    let nl = "\r\n".repeat(cell_rows.clamp(1, 256));
    processor.advance(&mut *t, nl.as_bytes());
}

/// End-to-end VT conformance: drives the *same* parser path the PTY reader
/// uses (alacritty_terminal + vte) over a battery of escape sequences and
/// asserts the resulting grid/cursor/mode. This is the automatable,
/// regression-proof core of a `vttest` sweep.
#[cfg(test)]
mod conformance {
    use super::*;
    use alacritty_terminal::Term;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::TermMode;
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vte::ansi::Processor;

    type Rx = crossbeam_channel::Receiver<TermEvent>;

    fn harness_rx(cols: usize, rows: usize) -> (Term<EventProxy>, Processor, Rx) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let proxy = EventProxy::new(tx, waker);
        let term = Term::new(
            TermConfig::default(),
            &TermSize {
                columns: cols,
                screen_lines: rows,
            },
            proxy,
        );
        (term, Processor::new(), rx)
    }

    fn harness(cols: usize, rows: usize) -> (Term<EventProxy>, Processor) {
        let (t, p, _rx) = harness_rx(cols, rows);
        (t, p)
    }

    /// Concatenate everything the terminal wrote back to the PTY.
    fn drain_pty(rx: &Rx) -> String {
        let mut out = String::new();
        while let Ok(ev) = rx.try_recv() {
            if let TermEvent::PtyWrite(s) = ev {
                out.push_str(&s);
            }
        }
        out
    }

    fn feed(term: &mut Term<EventProxy>, p: &mut Processor, bytes: &[u8]) {
        p.advance(term, bytes);
    }

    fn row_text(term: &Term<EventProxy>, row: i32) -> String {
        let g = term.grid();
        (0..g.columns())
            .map(|c| g[Point::new(Line(row), Column(c))].c)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn text_newline_and_cursor_addressing() {
        let (mut t, mut p) = harness(20, 5);
        feed(&mut t, &mut p, b"hello\r\nworld");
        assert_eq!(row_text(&t, 0), "hello");
        assert_eq!(row_text(&t, 1), "world");
        // CUP: ESC[3;2H then write — 1-based row/col.
        feed(&mut t, &mut p, b"\x1b[3;2HX");
        assert_eq!(row_text(&t, 2), " X");
    }

    #[test]
    fn erase_line_and_display() {
        let (mut t, mut p) = harness(10, 3);
        feed(&mut t, &mut p, b"ABCDEFG");
        feed(&mut t, &mut p, b"\x1b[1;4H\x1b[K"); // cursor col4, erase to EOL
        assert_eq!(row_text(&t, 0), "ABC");
        feed(&mut t, &mut p, b"\x1b[2J"); // erase whole display
        assert_eq!(row_text(&t, 0), "");
    }

    #[test]
    fn sgr_truecolor_bold_and_reset() {
        use alacritty_terminal::vte::ansi::Color;
        let (mut t, mut p) = harness(8, 2);
        feed(&mut t, &mut p, b"\x1b[1;38;2;10;20;30mZ\x1b[0mz");
        let g = t.grid();
        let z = &g[Point::new(Line(0), Column(0))];
        assert!(z.flags.contains(Flags::BOLD));
        match z.fg {
            Color::Spec(rgb) => assert_eq!((rgb.r, rgb.g, rgb.b), (10, 20, 30)),
            other => panic!("expected truecolor, got {other:?}"),
        }
        // After SGR reset, the next cell is back to default fg + no bold.
        let z2 = &g[Point::new(Line(0), Column(1))];
        assert!(!z2.flags.contains(Flags::BOLD));
    }

    #[test]
    fn tab_stops_and_carriage_return() {
        let (mut t, mut p) = harness(20, 2);
        feed(&mut t, &mut p, b"a\tb");
        let s = row_text(&t, 0);
        assert_eq!(&s[..1], "a");
        assert_eq!(s.chars().nth(8), Some('b')); // default tab stop at col 8
        feed(&mut t, &mut p, b"\rZ");
        assert_eq!(row_text(&t, 0).chars().next(), Some('Z'));
    }

    #[test]
    fn alt_screen_and_bracketed_paste_modes() {
        let (mut t, mut p) = harness(10, 3);
        feed(&mut t, &mut p, b"\x1b[?1049h");
        assert!(t.mode().contains(TermMode::ALT_SCREEN));
        feed(&mut t, &mut p, b"\x1b[?2004h");
        assert!(t.mode().contains(TermMode::BRACKETED_PASTE));
        feed(&mut t, &mut p, b"\x1b[?1049l");
        assert!(!t.mode().contains(TermMode::ALT_SCREEN));
    }

    #[test]
    fn scroll_region_and_index() {
        let (mut t, mut p) = harness(6, 4);
        // Restrict scrolling to rows 1..=2 (DECSTBM, 1-based), then make it
        // scroll: cursor to row2, newlines push row1 content up within region.
        feed(
            &mut t,
            &mut p,
            b"\x1b[1;2r\x1b[1;1Hone\x1b[2;1Htwo\r\n\r\nthree",
        );
        // Row 3 (outside the region) stays empty; region scrolled.
        assert_eq!(row_text(&t, 3), "");
    }

    #[test]
    fn dec_special_graphics_charset() {
        let (mut t, mut p) = harness(6, 2);
        // ESC ( 0 = DEC line-drawing into G0; q->─ x->│; ESC ( B back to ASCII.
        feed(&mut t, &mut p, b"\x1b(0qx\x1b(By");
        let s = row_text(&t, 0);
        let cs: Vec<char> = s.chars().collect();
        assert_eq!(cs[0], '\u{2500}', "q -> light horizontal");
        assert_eq!(cs[1], '\u{2502}', "x -> light vertical");
        assert_eq!(cs[2], 'y', "ASCII restored after ESC(B");
    }

    #[test]
    fn insert_and_delete_char() {
        let (mut t, mut p) = harness(8, 2);
        feed(&mut t, &mut p, b"abcde");
        // Cursor to col 2 ('b'), delete it (DCH).
        feed(&mut t, &mut p, b"\x1b[1;2H\x1b[P");
        assert_eq!(row_text(&t, 0), "acde");
        // Insert a blank at col 1 (ICH), then type at it.
        feed(&mut t, &mut p, b"\x1b[1;1H\x1b[@Z");
        assert_eq!(row_text(&t, 0), "Zacde");
    }

    #[test]
    fn insert_and_delete_line() {
        let (mut t, mut p) = harness(6, 4);
        feed(&mut t, &mut p, b"r0\r\nr1\r\nr2");
        // Delete line 2 (DL): r2 moves up into row 1.
        feed(&mut t, &mut p, b"\x1b[2;1H\x1b[M");
        assert_eq!(row_text(&t, 0), "r0");
        assert_eq!(row_text(&t, 1), "r2");
        // Insert a line at row 1 (IL): pushes r2 back down.
        feed(&mut t, &mut p, b"\x1b[2;1H\x1b[L");
        assert_eq!(row_text(&t, 1), "");
        assert_eq!(row_text(&t, 2), "r2");
    }

    #[test]
    fn save_restore_cursor_and_autowrap() {
        let (mut t, mut p) = harness(3, 3);
        // DECSC at row1col1, move away, write, DECRC back, overwrite.
        feed(&mut t, &mut p, b"\x1b7\x1b[3;3HX\x1b8A");
        assert_eq!(row_text(&t, 0).chars().next(), Some('A'));
        // Autowrap (DECAWM, on by default): 4 chars into 3 columns wraps.
        let (mut t2, mut p2) = harness(3, 3);
        feed(&mut t2, &mut p2, b"abcd");
        assert_eq!(row_text(&t2, 0), "abc");
        assert_eq!(row_text(&t2, 1), "d");
    }

    #[test]
    fn origin_mode_addresses_within_margins() {
        let (mut t, mut p) = harness(6, 5);
        // Scroll region rows 2..=4, enable origin mode, then home (1;1) is
        // the region top (absolute row index 1).
        feed(&mut t, &mut p, b"\x1b[2;4r\x1b[?6h\x1b[1;1HO");
        assert_eq!(row_text(&t, 0), "", "row 0 is above the margin");
        assert_eq!(row_text(&t, 1), "O", "origin-mode home = top margin");
    }

    #[test]
    fn dsr_cursor_position_report() {
        let (mut t, mut p, rx) = harness_rx(40, 10);
        // Move to row 3, col 5 (1-based), then DSR 6n.
        feed(&mut t, &mut p, b"\x1b[3;5H\x1b[6n");
        let reply = drain_pty(&rx);
        assert_eq!(reply, "\x1b[3;5R", "CPR must echo the 1-based cursor");
    }

    #[test]
    fn device_attributes_reply() {
        let (mut t, mut p, rx) = harness_rx(10, 3);
        feed(&mut t, &mut p, b"\x1b[c"); // Primary DA
        let reply = drain_pty(&rx);
        assert!(
            reply.starts_with("\x1b[?"),
            "DA1 reply should be a CSI ? … c, got {reply:?}"
        );
        assert!(reply.ends_with('c'));
    }

    #[test]
    fn sgr_underline_dim_strike() {
        let (mut t, mut p) = harness(8, 2);
        // dim + single underline + strikeout, then a curly underline cell.
        feed(&mut t, &mut p, b"\x1b[2;4;9mA\x1b[0m\x1b[4:3mB");
        let g = t.grid();
        let a = &g[Point::new(Line(0), Column(0))];
        assert!(a.flags.contains(Flags::DIM));
        assert!(a.flags.contains(Flags::UNDERLINE));
        assert!(a.flags.contains(Flags::STRIKEOUT));
        let b = &g[Point::new(Line(0), Column(1))];
        assert!(
            b.flags.contains(Flags::UNDERCURL),
            "SGR 4:3 = curly underline"
        );
        assert!(!b.flags.contains(Flags::DIM), "SGR 0 reset cleared dim");
    }

    #[test]
    fn decaln_fills_screen_with_e() {
        let (mut t, mut p) = harness(4, 3);
        feed(&mut t, &mut p, b"\x1b#8"); // DEC screen alignment test
        for r in 0..3 {
            assert_eq!(row_text(&t, r), "EEEE", "DECALN fills row {r}");
        }
    }

    #[test]
    fn rep_repeats_last_graphic_char() {
        let (mut t, mut p) = harness(8, 2);
        // 'A' then REP 3 -> "AAAA".
        feed(&mut t, &mut p, b"A\x1b[3b");
        assert_eq!(row_text(&t, 0), "AAAA");
    }

    #[test]
    fn charset_g1_via_so_si() {
        let (mut t, mut p) = harness(6, 2);
        // Designate DEC special graphics into G1, SO -> G1, SI -> back to G0.
        feed(&mut t, &mut p, b"\x1b)0\x0eqx\x0fy");
        let cs: Vec<char> = row_text(&t, 0).chars().collect();
        assert_eq!(cs[0], '\u{2500}', "G1 q -> horizontal line");
        assert_eq!(cs[1], '\u{2502}', "G1 x -> vertical line");
        assert_eq!(cs[2], 'y', "SI returned to ASCII G0");
    }

    // NOTE: SS2/SS3 single-shift (ESC N / ESC O) is not implemented by
    // alacritty_terminal, so no conformance test asserts it — see ROADMAP.
}
