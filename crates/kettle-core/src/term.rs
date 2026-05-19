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
use portable_pty::{CommandBuilder, PtySize};

use crate::event::{EventProxy, TermEvent, Waker};

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
}

impl Terminal {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        shell: Option<&str>,
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

        let mut cmd = match shell {
            Some(s) => CommandBuilder::new(s),
            None => CommandBuilder::new_default_prog(),
        };
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "kettle");
        if let Some(home) = std::env::var_os("HOME") {
            cmd.cwd(home);
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

        let reader_thread = {
            let term = term.clone();
            std::thread::Builder::new()
                .name("kettle-pty-reader".into())
                .spawn(move || {
                    let mut processor: Processor = Processor::new();
                    let mut buf = [0u8; 65536];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => {
                                proxy.send_event_exit();
                                break;
                            }
                            Ok(n) => {
                                if let Ok(mut t) = term.lock() {
                                    processor.advance(&mut *t, &buf[..n]);
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
        })
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
