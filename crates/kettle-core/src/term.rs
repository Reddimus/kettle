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
                                        Chunk::Image(data) => {
                                            place_image(
                                                &term,
                                                &images,
                                                &cell_px,
                                                &mut processor,
                                                data,
                                            );
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
    data: kettle_vt::ImageData,
) {
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
