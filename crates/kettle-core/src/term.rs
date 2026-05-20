//! A single terminal instance: PTY + `alacritty_terminal` grid + VT parser,
//! driven by a dedicated reader thread.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Processor};
use anyhow::Result;
use kettle_vt::placeholder::{self, CellDiacritics, RawCell};
use kettle_vt::{Chunk, Extractor, PromptKind};
use portable_pty::{CommandBuilder, PtySize};

use crate::event::{EventProxy, TermEvent, Waker};
use crate::images::{
    AnimEntry, Animations, Images, Placement, RelEntry, Relatives, VirtualEntry, Virtuals,
    relative_origin, resolve_chain,
};

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

/// Best-effort "user home" directory for a freshly spawned shell whose
/// recorded cwd is missing or no longer on disk. Probes the platform-
/// conventional env vars in order:
/// - `HOME` — always set on Linux / macOS
/// - `USERPROFILE` — the Windows-native home (`C:\Users\Bob`)
/// - `APPDATA` — Windows last-ditch fallback (`...\AppData\Roaming`)
///
/// Returns `None` only on a stripped-down environment where none are
/// set; callers leave `CommandBuilder::cwd` unset in that case, which
/// makes `portable_pty` inherit kettle's launch directory.
///
/// `lookup` is passed in so the env-probe order is unit-testable
/// without touching the process env (which would race with parallel
/// tests). Production code calls with `|k| std::env::var_os(k)`.
pub(crate) fn home_dir_fallback(
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    lookup("HOME")
        .or_else(|| lookup("USERPROFILE"))
        .or_else(|| lookup("APPDATA"))
        .map(std::path::PathBuf::from)
}

pub struct Terminal {
    pub term: SharedTerm,
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    reader_thread: Option<JoinHandle<()>>,
    pub cols: usize,
    pub rows: usize,
    pub images: Images,
    /// kitty `U=1` virtual images, keyed by image id (for placeholder draw).
    pub virtuals: Virtuals,
    /// kitty animations, keyed by image id (frame substituted at draw time).
    pub anims: Animations,
    /// kitty relative placements, keyed by `(child img, child placement)`.
    pub relatives: Relatives,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        argv: &[String],
        cwd: Option<&str>,
        scrollback: usize,
        cols: usize,
        rows: usize,
        cell_w: u16,
        cell_h: u16,
        cursor_blink: bool,
        cursor_shape: CursorShape,
        word_delimiters: Option<&str>,
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
        // `TERM_PROGRAM_VERSION` is the de-facto pair to `TERM_PROGRAM`
        // (iTerm2 / kitty / WezTerm / Ghostty all set it). Neovim's
        // `:checkhealth provider`, fish's prompt themers, and various
        // diagnostic tools key off the pair when probing whether they're
        // running under a known modern terminal. Kettle's own crate
        // version is the obvious answer — populated from Cargo at build
        // time so a bumped `kettle/Cargo.toml` flows through with no
        // separate version string to keep in sync.
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        match cwd {
            Some(d) if std::path::Path::new(d).is_dir() => cmd.cwd(d),
            _ => {
                // Recorded cwd is missing or no longer on disk (e.g.,
                // user moved the repo between sessions, or the `-d` arg
                // pointed at a since-deleted path). Fall back to the OS
                // home directory. The previous version only checked
                // `HOME`, which is unset on Windows by default — so
                // Windows users with a stale recorded cwd silently
                // ended up in whatever directory they happened to
                // launch kettle from. `home_dir_fallback` probes
                // `HOME` then `USERPROFILE` then `APPDATA`, in that
                // order, so all three platforms (Linux/macOS/Windows)
                // converge on the same "user-home" intent. Same shape
                // as cycle 159's macOS universal2 fix — Linux+macOS
                // worked, Windows didn't, the env var probe order is
                // the difference.
                if let Some(home) = home_dir_fallback(|k| std::env::var_os(k)) {
                    cmd.cwd(home);
                }
            }
        }
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;

        let proxy = EventProxy::new(event_tx, waker.clone());
        // Seed the engine's *default* cursor style from the user config; the
        // engine seeds `cursor_style` lazily from this, and programs can flip
        // both fields at runtime — `?12 h/l` for blinking (honored live via
        // `cursor_blinking()` below) and DECSCUSR `CSI Ps SP q` for shape
        // (honored live via the engine's `renderable_content().cursor.shape`,
        // read by the renderer per-frame).
        let default_cursor_style = alacritty_terminal::vte::ansi::CursorStyle {
            blinking: cursor_blink,
            shape: cursor_shape,
        };
        let mut tconf = TermConfig {
            scrolling_history: scrollback,
            kitty_keyboard: true,
            default_cursor_style,
            ..TermConfig::default()
        };
        // Word delimiters drive double-click word selection (and the
        // jump-to-prompt search). An empty config means "use the engine
        // default" — `",│`|:\"' ()[]{}<>\t"` — so users that don't set
        // anything still get sensible word boundaries.
        if let Some(wd) = word_delimiters
            && !wd.is_empty()
        {
            tconf.semantic_escape_chars = wd.to_string();
        }
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
        let virtuals: Virtuals = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let anims: Animations = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let relatives: Relatives = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let prompts: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
        let cwd_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(cwd.map(|s| s.to_string())));
        let cell_px = Arc::new(Mutex::new((cell_w.max(1), cell_h.max(1))));

        let reader_thread = {
            let term = term.clone();
            let images = images.clone();
            let virtuals = virtuals.clone();
            let anims = anims.clone();
            let relatives = relatives.clone();
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
                                            if let Ok(mut vm) = virtuals.lock() {
                                                match (all, id) {
                                                    (true, _) => vm.clear(),
                                                    (false, Some(x)) => {
                                                        vm.remove(&x);
                                                    }
                                                    (false, None) => {}
                                                }
                                            }
                                            if let Ok(mut am) = anims.lock() {
                                                match (all, id) {
                                                    (true, _) => am.clear(),
                                                    (false, Some(x)) => {
                                                        am.remove(&x);
                                                    }
                                                    (false, None) => {}
                                                }
                                            }
                                            if let Ok(mut rm) = relatives.lock() {
                                                match (all, id) {
                                                    (true, _) => rm.clear(),
                                                    // Group dies with parent:
                                                    // drop the child and any
                                                    // child parented to it.
                                                    (false, Some(x)) => {
                                                        rm.retain(|&(cimg, _), e| {
                                                            cimg != x && e.parent_img != x
                                                        })
                                                    }
                                                    (false, None) => {}
                                                }
                                            }
                                        }
                                        Chunk::RelativePlacement {
                                            id,
                                            placement,
                                            img,
                                            parent_img,
                                            parent_placement,
                                            h,
                                            v,
                                        } => {
                                            if let Ok(mut rm) = relatives.lock() {
                                                rm.insert(
                                                    (id, placement),
                                                    RelEntry {
                                                        img,
                                                        parent_img,
                                                        parent_placement,
                                                        h,
                                                        v,
                                                    },
                                                );
                                            }
                                            (waker)();
                                        }
                                        Chunk::VirtualImage {
                                            id,
                                            img,
                                            cols,
                                            rows,
                                            z,
                                        } => {
                                            if let Ok(mut vm) = virtuals.lock() {
                                                vm.insert(id, VirtualEntry { img, cols, rows, z });
                                            }
                                            (waker)();
                                        }
                                        Chunk::Animation {
                                            id,
                                            imgs,
                                            gaps,
                                            state,
                                        } => {
                                            if let Ok(mut am) = anims.lock() {
                                                // An empty/single-image, not-
                                                // running snapshot = cleared.
                                                if imgs.len() <= 1 && !state.running {
                                                    am.remove(&id);
                                                } else {
                                                    // Keep the clock unless the
                                                    // run state flipped.
                                                    let started = match am.get(&id) {
                                                        Some(p)
                                                            if p.state.running == state.running =>
                                                        {
                                                            p.started
                                                        }
                                                        _ => std::time::Instant::now(),
                                                    };
                                                    am.insert(
                                                        id,
                                                        AnimEntry {
                                                            imgs,
                                                            gaps,
                                                            state,
                                                            started,
                                                        },
                                                    );
                                                }
                                            }
                                            (waker)();
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
            virtuals,
            anims,
            relatives,
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

    /// Live cursor-blink state. Defaults to whatever the config seeded at
    /// pane creation; programs flip it at runtime via DEC private mode 12
    /// (`CSI ?12 h` blink / `?12 l` solid) — the engine raises
    /// `TermEvent::CursorBlinkingChange` and we re-read this on next redraw
    /// so the cursor obeys the running app, not just the config.
    pub fn cursor_blinking(&self) -> bool {
        self.term
            .lock()
            .map(|t| t.cursor_style().blinking)
            .unwrap_or(false)
    }

    /// Absolute prompt-start lines recorded via OSC 133.
    pub fn prompt_marks(&self) -> Vec<i64> {
        self.prompts.lock().map(|m| m.clone()).unwrap_or_default()
    }

    /// Image placements for this terminal (cloned cheaply; `ImageData` is
    /// `Arc`-backed). For placements whose kitty id has a registered
    /// animation, the image is swapped for the frame the playback clock
    /// selects right now, so animations play wherever the image sits.
    pub fn placements(&self) -> Vec<Placement> {
        let mut v = self.images.lock().map(|v| v.clone()).unwrap_or_default();
        if let Ok(am) = self.anims.lock()
            && !am.is_empty()
        {
            for p in &mut v {
                if let Some(id) = p.id
                    && let Some(e) = am.get(&id)
                {
                    p.img = e.current().clone();
                }
            }
        }
        v
    }

    /// `true` if any registered kitty animation is currently running (so the
    /// UI knows to schedule frame-paced redraws).
    pub fn has_running_animation(&self) -> bool {
        self.anims
            .lock()
            .map(|am| am.values().any(|e| e.state.running))
            .unwrap_or(false)
    }

    /// Per-cell image tiles for the kitty Unicode placeholders (`U+10EEEE`)
    /// currently visible: decode each cell's `(image-id, row, column)` from
    /// its foreground color + combining diacritics, apply the left-
    /// inheritance rules over contiguous runs, and slice the referenced
    /// virtual image into one `Placement` per cell. Recomputed per frame —
    /// cheap: `ImageData` is `Arc`-backed and only the shown tiles are
    /// cropped. The placement id is decoded from the cell's underline
    /// color (used for run grouping / inheritance per the spec); a single
    /// virtual placement is stored per image id, so it also selects it.
    /// Scan the visible grid for `U+10EEEE` placeholder cells and resolve
    /// each one (image id + in-image row/col after diacritic inheritance) to
    /// its absolute line and column. Shared by placeholder + relative tiles.
    fn placeholder_cells(&self) -> Vec<(i64, usize, placeholder::ResolvedCell)> {
        let Ok(t) = self.term.lock() else {
            return Vec::new();
        };
        let top = t.grid().history_size() as i64 - t.grid().display_offset() as i64;
        let content = t.renderable_content();

        // Maximal same-row contiguous runs of placeholder cells.
        let mut runs: Vec<Vec<(RawCell, i64, usize)>> = Vec::new();
        let mut last: Option<(i32, i32)> = None;
        for ind in content.display_iter {
            let (cell, p) = (ind.cell, ind.point);
            if cell.c == placeholder::PLACEHOLDER {
                let contiguous = matches!(
                    last,
                    Some((r, c)) if r == p.line.0 && c + 1 == p.column.0 as i32
                );
                if !contiguous || runs.is_empty() {
                    runs.push(Vec::new());
                }
                let marks: Vec<char> = cell.zerowidth().map(|z| z.to_vec()).unwrap_or_default();
                runs.last_mut().unwrap().push((
                    RawCell {
                        fg: fg_id_bits(cell.fg),
                        // Underline color carries the placement id (0/absent
                        // ⇒ any placement); spec §"Unicode placeholders".
                        placement_id: cell.underline_color().map(fg_id_bits).unwrap_or(0),
                        diacritics: CellDiacritics::parse(&marks),
                    },
                    top + p.line.0 as i64,
                    p.column.0,
                ));
                last = Some((p.line.0, p.column.0 as i32));
            } else {
                last = None;
            }
        }

        let mut out = Vec::new();
        for run in &runs {
            let cells: Vec<RawCell> = run.iter().map(|(rc, _, _)| *rc).collect();
            for (res, &(_, abs, col)) in
                placeholder::resolve_run(&cells).into_iter().zip(run.iter())
            {
                out.push((abs, col, res));
            }
        }
        out
    }

    pub fn placeholder_tiles(&self) -> Vec<Placement> {
        let Ok(virtuals) = self.virtuals.lock() else {
            return Vec::new();
        };
        if virtuals.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (abs, col, res) in self.placeholder_cells() {
            let Some(v) = virtuals.get(&res.image_id) else {
                continue;
            };
            let pcols = v.cols.max(1).min(u16::MAX as u32) as u16;
            let prows = v.rows.max(1).min(u16::MAX as u32) as u16;
            if let Some((x, y, w, h)) = placeholder::tile_src_rect(
                v.img.width,
                v.img.height,
                pcols,
                prows,
                res.row,
                res.col,
            ) && let Some(crop) = v.img.crop(x, y, w, h)
            {
                out.push(Placement {
                    abs_line: abs,
                    col,
                    cell_cols: 1,
                    cell_rows: 1,
                    img: crop,
                    id: Some(res.image_id),
                    z: v.z,
                });
            }
        }
        out
    }

    /// Placements for kitty relative placements whose parent is a visible
    /// Unicode-placeholder (virtual) image: the parent's origin is the
    /// top-left of its placeholder cells, and the child image is drawn
    /// `(h, v)` cells from there. Parents that aren't on screen this frame
    /// are skipped (the relative is simply not shown). Non-placeholder /
    /// chained parents are a later sub-item (see ROADMAP).
    pub fn relative_tiles(&self) -> Vec<Placement> {
        // Snapshot the relatives, then drop the lock before taking the
        // grid / images locks (keeps a single lock-acquisition order).
        let entries: Vec<(u32, RelEntry)> = {
            let Ok(rel) = self.relatives.lock() else {
                return Vec::new();
            };
            if rel.is_empty() {
                return Vec::new();
            }
            rel.iter().map(|(&(c, _), e)| (c, e.clone())).collect()
        };
        // Concrete origins: a parent is either a placeholder/virtual image
        // (top-left of its cells) or a regular placement (its abs_line/col).
        let mut origins: std::collections::HashMap<u32, (i64, usize)> =
            std::collections::HashMap::new();
        let mut note = |id: u32, abs: i64, col: usize| {
            origins
                .entry(id)
                .and_modify(|o: &mut (i64, usize)| {
                    o.0 = o.0.min(abs);
                    o.1 = o.1.min(col);
                })
                .or_insert((abs, col));
        };
        for (abs, col, res) in self.placeholder_cells() {
            note(res.image_id, abs, col);
        }
        if let Ok(imgs) = self.images.lock() {
            for p in imgs.iter() {
                if let Some(id) = p.id {
                    note(id, p.abs_line, p.col);
                }
            }
        }
        // child image id -> (parent image id, h, v), for chain walking.
        let rels: std::collections::HashMap<u32, (u32, i32, i32)> = entries
            .iter()
            .map(|(c, e)| (*c, (e.parent_img, e.h, e.v)))
            .collect();
        let (cw, chh) = self.cell_px.lock().map(|p| *p).unwrap_or((8, 16));
        let (cw, chh) = (cw.max(1) as u32, chh.max(1) as u32);
        let mut out = Vec::new();
        for (cimg, e) in &entries {
            // kitty requires a chain depth of at least 8.
            let Some((pa, pc)) = resolve_chain(e.parent_img, &rels, &origins, 8) else {
                continue;
            };
            let (abs, col) = relative_origin(pa, pc, e.h, e.v);
            out.push(Placement {
                abs_line: abs,
                col,
                cell_cols: e.img.width.div_ceil(cw) as usize,
                cell_rows: e.img.height.div_ceil(chh) as usize,
                img: e.img.clone(),
                id: Some(*cimg),
                z: 0,
            });
        }
        out
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

/// The kitty image-id bits a placeholder cell's foreground color carries:
/// a 256-palette index is the low byte, a truecolor spec is the low 24
/// bits, and the 16 ANSI named colors map to indices 0..=15
/// (`graphics-protocol.rst:589`). Non-id named slots (default fg/bg/cursor)
/// have no id → 0.
fn fg_id_bits(c: AnsiColor) -> u32 {
    use NamedColor::*;
    match c {
        AnsiColor::Indexed(i) => i as u32,
        AnsiColor::Spec(rgb) => ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32,
        AnsiColor::Named(n) => match n {
            Black => 0,
            Red => 1,
            Green => 2,
            Yellow => 3,
            Blue => 4,
            Magenta => 5,
            Cyan => 6,
            White => 7,
            BrightBlack | DimBlack => 8,
            BrightRed | DimRed => 9,
            BrightGreen | DimGreen => 10,
            BrightYellow | DimYellow => 11,
            BrightBlue | DimBlue => 12,
            BrightMagenta | DimMagenta => 13,
            BrightCyan | DimCyan => 14,
            BrightWhite | DimWhite => 15,
            _ => 0,
        },
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
mod home_dir_tests {
    use super::home_dir_fallback;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| OsString::from(*v))
        }
    }

    #[test]
    fn prefers_home_then_userprofile_then_appdata() {
        // All three set (rare — a WSL user with both env worlds bleeding
        // through) → HOME wins. This is the Linux / macOS branch.
        assert_eq!(
            home_dir_fallback(from(&[
                ("HOME", "/h"),
                ("USERPROFILE", r"C:\u"),
                ("APPDATA", r"C:\a"),
            ])),
            Some(PathBuf::from("/h")),
        );
        // No HOME → USERPROFILE. This is the *Windows* branch — exactly
        // the gap the previous `var_os("HOME")`-only fallback missed.
        assert_eq!(
            home_dir_fallback(from(&[("USERPROFILE", r"C:\u"), ("APPDATA", r"C:\a"),])),
            Some(PathBuf::from(r"C:\u")),
        );
        // Only APPDATA set (very stripped Windows session) → APPDATA.
        assert_eq!(
            home_dir_fallback(from(&[("APPDATA", r"C:\a")])),
            Some(PathBuf::from(r"C:\a")),
        );
        // Nothing set (minimal Linux container without HOME) → None;
        // caller leaves cmd.cwd() untouched.
        assert_eq!(home_dir_fallback(from(&[])), None);
    }
}

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

    #[test]
    fn ris_full_reset_clears_origin_mode() {
        let (mut t, mut p) = harness(6, 4);
        // Origin mode on + scroll region, then RIS (ESC c) — a full reset —
        // so 1;1 is absolute home again.
        feed(&mut t, &mut p, b"\x1b[2;4r\x1b[?6hzz\x1bc\x1b[1;1HX");
        assert_eq!(row_text(&t, 0), "X", "RIS cleared origin mode + region");
    }

    #[test]
    fn el_erase_to_left() {
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"ABCDE");
        // Cursor to col 3 (1-based), EL 1 = erase start..=cursor.
        feed(&mut t, &mut p, b"\x1b[1;3H\x1b[1K");
        // cols 0..=2 cleared; "DE" remains at cols 3,4.
        assert_eq!(row_text(&t, 0), "   DE");
    }

    #[test]
    fn ed_erase_below() {
        let (mut t, mut p) = harness(4, 3);
        feed(&mut t, &mut p, b"r0\r\nr1\r\nr2");
        // Cursor to row 2 col 1 (1-based), ED 0 = erase cursor..=end.
        feed(&mut t, &mut p, b"\x1b[2;1H\x1b[0J");
        assert_eq!(row_text(&t, 0), "r0", "row above the cursor kept");
        assert_eq!(row_text(&t, 1), "", "cursor row erased");
        assert_eq!(row_text(&t, 2), "", "rows below erased");
    }

    #[test]
    fn da2_secondary_device_attributes() {
        let (mut t, mut p, rx) = harness_rx(10, 3);
        feed(&mut t, &mut p, b"\x1b[>c"); // Secondary DA
        let reply = drain_pty(&rx);
        assert!(
            reply.starts_with("\x1b[>") && reply.ends_with('c'),
            "DA2 reply should be CSI > … c, got {reply:?}"
        );
    }

    #[test]
    fn ech_erases_in_place() {
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"ABCDE");
        // Cursor col 2 (1-based), ECH 2 clears 2 cells, cursor unmoved.
        feed(&mut t, &mut p, b"\x1b[1;2H\x1b[2X");
        assert_eq!(row_text(&t, 0), "A  DE");
    }

    #[test]
    fn ich_shifts_right_off_edge() {
        let (mut t, mut p) = harness(5, 2);
        feed(&mut t, &mut p, b"abcde");
        // ICH 2 at home pushes cells right; the line is 5 wide so d,e fall off.
        feed(&mut t, &mut p, b"\x1b[1;1H\x1b[2@");
        assert_eq!(row_text(&t, 0), "  abc");
    }

    #[test]
    fn absolute_cursor_moves_cha_hpa_vpa() {
        let (mut t, mut p) = harness(6, 4);
        // CHA: column-absolute to col 3 then write.
        feed(&mut t, &mut p, b"abcde\x1b[3GZ");
        assert_eq!(row_text(&t, 0), "abZde");
        // HPA (ESC[`) col 2 on row 1, VPA (ESC[d) row 3.
        feed(&mut t, &mut p, b"\x1b[1;1H\x1b[2`Q\x1b[3dW");
        assert_eq!(row_text(&t, 0).chars().nth(1), Some('Q'), "HPA col 2");
        // VPA changes the row only; column stays where it was (col 3 after Q).
        assert_eq!(row_text(&t, 2).chars().nth(2), Some('W'), "VPA row 3");
    }

    #[test]
    fn decsc_restores_sgr_attributes() {
        let (mut t, mut p) = harness(6, 3);
        // Bold on, DECSC (saves cursor + pen), reset SGR + move away,
        // DECRC restores both, then write — must be bold at the saved cell.
        feed(&mut t, &mut p, b"\x1b[1m\x1b7\x1b[0m\x1b[3;4HZ\x1b8A");
        let a = &t.grid()[Point::new(Line(0), Column(0))];
        assert_eq!(a.c, 'A');
        assert!(
            a.flags.contains(Flags::BOLD),
            "DECRC must restore the saved SGR pen"
        );
    }

    #[test]
    fn su_sd_scroll_up_and_down() {
        let (mut t, mut p) = harness(4, 3);
        feed(&mut t, &mut p, b"r0\r\nr1\r\nr2");
        feed(&mut t, &mut p, b"\x1b[1S"); // SU 1: content moves up
        assert_eq!(row_text(&t, 0), "r1");
        assert_eq!(row_text(&t, 1), "r2");
        assert_eq!(row_text(&t, 2), "");

        let (mut t2, mut p2) = harness(4, 3);
        feed(&mut t2, &mut p2, b"a\r\nb\r\nc");
        feed(&mut t2, &mut p2, b"\x1b[1T"); // SD 1: content moves down
        assert_eq!(row_text(&t2, 0), "");
        assert_eq!(row_text(&t2, 1), "a");
        assert_eq!(row_text(&t2, 2), "b");
    }

    #[test]
    fn decscusr_sets_cursor_shape() {
        use alacritty_terminal::vte::ansi::CursorShape;
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"\x1b[3 q"); // DECSCUSR 3 = (blinking) underline
        assert_eq!(t.renderable_content().cursor.shape, CursorShape::Underline);
        feed(&mut t, &mut p, b"\x1b[5 q"); // 5 = (blinking) bar/beam
        assert_eq!(t.renderable_content().cursor.shape, CursorShape::Beam);
        feed(&mut t, &mut p, b"\x1b[1 q"); // 1 = (blinking) block
        assert_eq!(t.renderable_content().cursor.shape, CursorShape::Block);
    }

    #[test]
    fn dec_mode_25_hide_collapses_renderable_cursor_to_hidden() {
        // Cursor visibility (DEC ?25) and cursor shape (DECSCUSR `q`) are
        // tracked in different places in the engine; `RenderableContent`
        // *folds* them so the renderer only has to look at one field. This
        // test pins that contract — what the renderer reads is `Hidden` the
        // moment a program clears ?25, even if the shape was set to
        // something else first. Otherwise we'd silently keep drawing a
        // cursor at TUI apps that asked us not to (less, fzf full-screen).
        use alacritty_terminal::vte::ansi::CursorShape;
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"\x1b[1 q"); // shape = block (visible default)
        assert_eq!(t.renderable_content().cursor.shape, CursorShape::Block);
        feed(&mut t, &mut p, b"\x1b[?25l"); // ?25 cleared = hide cursor
        assert_eq!(
            t.renderable_content().cursor.shape,
            CursorShape::Hidden,
            "DEC ?25 l must collapse the renderable cursor to Hidden"
        );
        feed(&mut t, &mut p, b"\x1b[?25h"); // ?25 set = show again
        assert_eq!(
            t.renderable_content().cursor.shape,
            CursorShape::Block,
            "DEC ?25 h restores the previous shape"
        );
    }

    #[test]
    fn wide_cjk_char_occupies_two_cells() {
        let (mut t, mut p) = harness(8, 2);
        feed(&mut t, &mut p, "世A".as_bytes());
        let g = t.grid();
        let c0 = &g[Point::new(Line(0), Column(0))];
        assert_eq!(c0.c, '世');
        assert!(c0.flags.contains(Flags::WIDE_CHAR), "CJK = wide");
        assert!(
            g[Point::new(Line(0), Column(1))]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER),
            "second cell is the wide spacer"
        );
        assert_eq!(g[Point::new(Line(0), Column(2))].c, 'A');
    }

    #[test]
    fn wide_char_wraps_when_it_does_not_fit() {
        let (mut t, mut p) = harness(3, 3);
        // 2 narrow + 1 wide: the wide char can't fit in the last column,
        // so it wraps to the next row.
        feed(&mut t, &mut p, "ab世".as_bytes());
        assert_eq!(row_text(&t, 0), "ab");
        assert_eq!(t.grid()[Point::new(Line(1), Column(0))].c, '世');
    }

    #[test]
    fn combining_mark_is_zero_width() {
        let (mut t, mut p) = harness(6, 2);
        // 'e' + combining acute accent: one cell, mark stored as zerowidth.
        feed(&mut t, &mut p, "e\u{0301}X".as_bytes());
        let g = t.grid();
        let base = &g[Point::new(Line(0), Column(0))];
        assert_eq!(base.c, 'e');
        assert_eq!(
            base.zerowidth(),
            Some(&['\u{0301}'][..]),
            "combining mark attaches to the base cell"
        );
        assert_eq!(
            g[Point::new(Line(0), Column(1))].c,
            'X',
            "next glyph is in the very next cell (mark took no column)"
        );
    }

    #[test]
    fn osc4_palette_query_emits_color_request() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        // Query palette entry 1.
        feed(&mut t, &mut p, b"\x1b]4;1;?\x07");
        let got_idx = rx.try_iter().find_map(|ev| match ev {
            TermEvent::ColorRequest(idx, _) => Some(idx),
            _ => None,
        });
        assert_eq!(got_idx, Some(1), "OSC 4 ; 1 ; ? requests palette index 1");
    }

    #[test]
    fn sgr_underline_style_variants_set_distinct_flags() {
        // Five style bits in the engine: `\e[4m` (single), `\e[21m` or
        // `\e[4:2m` (double), `\e[4:3m` (curl), `\e[4:4m` (dotted),
        // `\e[4:5m` (dashed). The renderer (cycle 81) reads these and
        // draws differently per style — this pins each one reaching the
        // engine's cell flags so a future engine bump can't silently
        // drop a variant.
        let (mut t, mut p) = harness(20, 2);
        feed(
            &mut t,
            &mut p,
            b"\x1b[4ma\x1b[4:2mb\x1b[4:3mc\x1b[4:4md\x1b[4:5me",
        );
        let g = t.grid();
        let f = |c: usize| g[Point::new(Line(0), Column(c))].flags;
        assert!(f(0).contains(Flags::UNDERLINE), "[4m → UNDERLINE on `a`");
        assert!(
            f(1).contains(Flags::DOUBLE_UNDERLINE),
            "[4:2m → DOUBLE_UNDERLINE on `b`"
        );
        assert!(f(2).contains(Flags::UNDERCURL), "[4:3m → UNDERCURL on `c`");
        assert!(
            f(3).contains(Flags::DOTTED_UNDERLINE),
            "[4:4m → DOTTED_UNDERLINE on `d`"
        );
        assert!(
            f(4).contains(Flags::DASHED_UNDERLINE),
            "[4:5m → DASHED_UNDERLINE on `e`"
        );
        // Each variant is mutually-exclusive: setting DOUBLE clears the
        // previous UNDERLINE bit (alacritty single-underline-flag model).
        // Confirm by checking `b` doesn't still carry plain UNDERLINE.
        assert!(
            !f(1).contains(Flags::UNDERLINE),
            "[4:2m must clear plain UNDERLINE"
        );
    }

    #[test]
    fn sgr_58_sets_per_cell_underline_color() {
        // Neovim spell-check / git diff / lsp diagnostics emit per-cell
        // underline color via SGR 58. The engine stores it on the cell;
        // the renderer reads it (cycle 80) so the squiggle color follows
        // the request instead of using the text fg. Confirms truecolor
        // form `\e[58;2;r;g;bm` reaches `cell.underline_color()`.
        use alacritty_terminal::vte::ansi::{Color as AnsiColor, Rgb as AnsiRgb};
        let (mut t, mut p) = harness(8, 2);
        // Underline on + red underline color, then write a glyph.
        feed(&mut t, &mut p, b"\x1b[4m\x1b[58;2;200;30;30mX");
        let grid = t.grid();
        let cell = &grid[Point::new(Line(0), Column(0))];
        assert_eq!(cell.c, 'X');
        assert!(
            cell.flags.contains(Flags::UNDERLINE),
            "SGR 4 must set UNDERLINE"
        );
        assert_eq!(
            cell.underline_color(),
            Some(AnsiColor::Spec(AnsiRgb {
                r: 200,
                g: 30,
                b: 30
            })),
            "SGR 58 must store the per-cell underline color"
        );
        // SGR 59 resets to default (None), leaving UNDERLINE intact.
        feed(&mut t, &mut p, b"\x1b[59mY");
        let cell2 = &t.grid()[Point::new(Line(0), Column(1))];
        assert_eq!(cell2.c, 'Y');
        assert!(cell2.flags.contains(Flags::UNDERLINE));
        assert_eq!(
            cell2.underline_color(),
            None,
            "SGR 59 must clear the per-cell underline color"
        );
    }

    #[test]
    fn osc4_multi_index_query_emits_one_request_per_index() {
        // vte's OSC 4 handler chunks the params in pairs (`;idx;val`), so
        // a single `OSC 4 ; 1 ; ? ; 7 ; ?` should ask for *two* colors in
        // one go. tmux, neovim 0.10+ and base16-shell-hook all batch
        // palette probes this way — without per-pair dispatch they'd see
        // only the first reply and assume the rest of the palette equals
        // the engine default, breaking the dark/light detection they rely
        // on.
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, b"\x1b]4;1;?;7;?\x07");
        let mut indices: Vec<usize> = rx
            .try_iter()
            .filter_map(|ev| match ev {
                TermEvent::ColorRequest(idx, _) => Some(idx),
                _ => None,
            })
            .collect();
        indices.sort_unstable();
        assert_eq!(
            indices,
            vec![1, 7],
            "multi-index OSC 4 must fire one ColorRequest per `;idx;?` pair"
        );
    }

    #[test]
    fn osc_10_11_12_set_populate_default_color_slots() {
        // OSC 10/11/12 SET should populate the engine's `Colors[256..=258]`
        // slots (default fg, default bg, cursor) so the renderer's
        // `resolve_query` reflects the override on the next frame. Without
        // this round-trip, OSC 12 (set cursor color) was a silent drop in
        // the render path — see commit notes for cycle 56. Confirms the
        // pair: OSC 4 set was tested in cycle 47; OSC 10/11/12 are the
        // close siblings that use the same Colors slots.
        for (input, idx) in &[
            (b"\x1b]10;rgb:11/22/33\x07" as &[u8], 256usize),
            (b"\x1b]11;rgb:44/55/66\x07", 257),
            (b"\x1b]12;rgb:77/88/99\x07", 258),
        ] {
            let (mut t, mut p, _rx) = harness_rx(8, 2);
            assert!(t.colors()[*idx].is_none(), "slot {idx} clean pre-set");
            feed(&mut t, &mut p, input);
            let c = t.colors()[*idx].unwrap_or_else(|| panic!("slot {idx} unset after {input:?}"));
            // The exact values from the xparsecolor input (engine packs
            // each `RR` byte pair into a single u8).
            let want = match idx {
                256 => (0x11, 0x22, 0x33),
                257 => (0x44, 0x55, 0x66),
                258 => (0x77, 0x88, 0x99),
                _ => unreachable!(),
            };
            assert_eq!((c.r, c.g, c.b), want, "wrong color for slot {idx}");
        }
    }

    #[test]
    fn osc_104_no_params_resets_all_256_palette_slots() {
        // OSC 104 with no parameters (just `\e]104\a` or `\e]104;\a`)
        // resets *every* palette index (0..256), not just one. xterm
        // documents this: "OSC 104 ; c → reset color number c (default
        // restore palette)." Tools like `colorls`/`zsh-colorize`'s
        // theme-changers emit it to undo their session-wide palette
        // overrides on exit. The kettle conformance test from cycle 47
        // covered only the indexed form (`\e]104;1\a`); pin the
        // no-arg-resets-all branch too so it can't quietly regress
        // (e.g. if alacritty/vte upstream change the dispatch table).
        let (mut t, mut p, _rx) = harness_rx(8, 2);
        // Populate three slots so we have something to confirm reset against.
        feed(&mut t, &mut p, b"\x1b]4;1;rgb:11/22/33\x07");
        feed(&mut t, &mut p, b"\x1b]4;2;rgb:44/55/66\x07");
        feed(&mut t, &mut p, b"\x1b]4;200;rgb:77/88/99\x07");
        assert!(t.colors()[1].is_some(), "slot 1 should be set");
        assert!(t.colors()[2].is_some(), "slot 2 should be set");
        assert!(t.colors()[200].is_some(), "slot 200 should be set");
        // OSC 104 with no parameters → reset all 256 palette indices.
        feed(&mut t, &mut p, b"\x1b]104\x07");
        for idx in 0..256 {
            assert!(
                t.colors()[idx].is_none(),
                "slot {idx} should be cleared after OSC 104 (no params)"
            );
        }
    }

    #[test]
    fn osc_110_111_112_reset_default_fg_bg_cursor_slots() {
        // OSC 110 / 111 / 112 are the reset siblings of OSC 10/11/12 (set
        // default fg/bg/cursor). They tell the engine to throw away any
        // override the user-program set so the renderer falls back to the
        // theme's defaults. Kettle's render path reads `t.colors()[256..=258]`
        // each frame; if the engine didn't honor these resets, a program
        // that did `\e]10;rgb:11/22/33\a` then `\e]110\a` to undo would
        // leave the (red) override in place — a real bug class that
        // matches the cycle-56/65/66 "set went through but reset was
        // silently dropped" shape (cycles fixed the set path; this test
        // pins the reset path so it can't regress in the other
        // direction). Same loop covers all three indices in one
        // declarative table.
        for (idx, set, reset) in &[
            (
                256usize,
                &b"\x1b]10;rgb:11/22/33\x07"[..],
                &b"\x1b]110\x07"[..],
            ),
            (
                257usize,
                &b"\x1b]11;rgb:44/55/66\x07"[..],
                &b"\x1b]111\x07"[..],
            ),
            (
                258usize,
                &b"\x1b]12;rgb:77/88/99\x07"[..],
                &b"\x1b]112\x07"[..],
            ),
        ] {
            let (mut t, mut p, _rx) = harness_rx(8, 2);
            assert!(t.colors()[*idx].is_none(), "slot {idx} clean pre-set");
            feed(&mut t, &mut p, set);
            assert!(
                t.colors()[*idx].is_some(),
                "slot {idx} should be populated after OSC set"
            );
            feed(&mut t, &mut p, reset);
            assert!(
                t.colors()[*idx].is_none(),
                "slot {idx} should be cleared after the OSC reset sibling"
            );
        }
    }

    #[test]
    fn osc_color_set_query_reset_round_trip_through_engine() {
        // Round-trip companion to the OSC query test: confirm OSC 4 set +
        // OSC 104 reset actually move the engine's `Colors` slot (so our
        // `kettle_render::resolve_query` will reflect changes live —
        // tested separately in kettle-render). This guards against an
        // upstream regression silently disconnecting the set/reset path
        // from the OSC 4 query path we ship.
        let (mut t, mut p, _rx) = harness_rx(8, 2);
        // Initially the engine has no override for palette 1.
        assert!(t.colors()[1].is_none(), "expected no override pre-set");
        // OSC 4 ; 1 ; rgb:11/22/33  → set.
        feed(&mut t, &mut p, b"\x1b]4;1;rgb:11/22/33\x07");
        let after_set = t.colors()[1].expect("OSC 4 set must populate slot 1");
        assert_eq!(
            (after_set.r, after_set.g, after_set.b),
            (0x11, 0x22, 0x33),
            "engine must store the override exactly"
        );
        // OSC 104 ; 1  → reset that index only.
        feed(&mut t, &mut p, b"\x1b]104;1\x07");
        assert!(t.colors()[1].is_none(), "OSC 104 ; 1 must clear slot 1");
    }

    #[test]
    fn xtwinops_text_area_size_pixels_formats_csi4_reply() {
        // CSI 14 t — text-area pixel size. The engine raises
        // TextAreaSizeRequest(fmt) and expects the caller to plug in cell
        // dimensions + grid size; the formatter then produces the standard
        // `CSI 4 ; h ; w t` xtwinops reply (h = rows × cell_h, w = cols ×
        // cell_w). Sixel/kitty/iTerm2 image apps depend on this to compute
        // pixel-accurate placements.
        let (mut t, mut p, rx) = harness_rx(40, 10);
        feed(&mut t, &mut p, b"\x1b[14t");
        let fmt = rx
            .try_iter()
            .find_map(|ev| match ev {
                TermEvent::TextAreaSizeRequest(f) => Some(f),
                _ => None,
            })
            .expect("CSI 14 t must raise a TextAreaSizeRequest");
        // 9 px wide × 18 px tall cells on a 40×10 grid → 360 × 180 px.
        let reply = fmt(alacritty_terminal::event::WindowSize {
            num_lines: 10,
            num_cols: 40,
            cell_width: 9,
            cell_height: 18,
        });
        assert_eq!(
            reply, "\x1b[4;180;360t",
            "CSI 14 t reply must be CSI 4 ; <height-px> ; <width-px> t"
        );
    }

    #[test]
    fn osc_color_queries_carry_index_and_format_xparsecolor_reply() {
        // OSC 10 / 11 / 12 (default fg / bg / cursor) and OSC 4 ; n ; ? are
        // the four queries shells and TUIs use to detect light-vs-dark and
        // theme colors. Each must (a) emit a `ColorRequest` carrying the
        // correct index — 256 / 257 / 258 / palette-idx — and (b) hand back
        // an engine-supplied formatter that renders the canonical xparsecolor
        // reply `\e]<prefix>;rgb:RRRR/GGGG/BBBB\` so apps that probe for the
        // exact wire format (mc / neovim / gnome-terminal probes) accept it.
        let cases: &[(&[u8], usize, &str)] = &[
            (b"\x1b]10;?\x07", 256, "\x1b]10;rgb:"), // OSC 10 — fg
            (b"\x1b]11;?\x07", 257, "\x1b]11;rgb:"), // OSC 11 — bg
            (b"\x1b]12;?\x07", 258, "\x1b]12;rgb:"), // OSC 12 — cursor
            (b"\x1b]4;7;?\x07", 7, "\x1b]4;7;rgb:"), // OSC 4 ; 7 — palette
        ];
        for (input, want_idx, want_prefix) in cases {
            let (mut t, mut p, rx) = harness_rx(8, 2);
            feed(&mut t, &mut p, input);
            let (idx, fmt) = rx
                .try_iter()
                .find_map(|ev| match ev {
                    TermEvent::ColorRequest(i, f) => Some((i, f)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no ColorRequest for {input:?}"));
            assert_eq!(idx, *want_idx, "wrong index for {input:?}");

            // Format with a known value and verify the wire shape. The
            // 8-bit channels are doubled to 16-bit per xparsecolor.
            let reply = fmt(alacritty_terminal::vte::ansi::Rgb {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            });
            let want_payload = format!("{want_prefix}1212/3434/5656");
            assert!(
                reply.starts_with(&want_payload),
                "{input:?} reply must start with {want_payload:?}, got {reply:?}"
            );
        }
    }

    #[test]
    fn decrqm_reports_mode_state() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        // Enable bracketed paste, then DECRQM-query it (CSI ? 2004 $ p).
        feed(&mut t, &mut p, b"\x1b[?2004h\x1b[?2004$p");
        let reply = drain_pty(&rx);
        assert!(
            reply.contains("2004;1") && reply.ends_with("$y"),
            "DECRPM should report mode 2004 as set, got {reply:?}"
        );
    }

    #[test]
    fn osc52_copy_emits_clipboard_store() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        // base64("hi") = "aGk=" ; OSC 52 ; c ; <b64> ST
        feed(&mut t, &mut p, b"\x1b]52;c;aGk=\x07");
        let stored = rx.try_iter().find_map(|ev| match ev {
            TermEvent::ClipboardStore(_, s) => Some(s),
            _ => None,
        });
        assert_eq!(stored.as_deref(), Some("hi"), "OSC 52 c sets clipboard");
    }

    #[test]
    fn osc8_hyperlink_carries_on_cells() {
        let (mut t, mut p) = harness(8, 2);
        feed(
            &mut t,
            &mut p,
            b"\x1b]8;;https://x.example\x07Z\x1b]8;;\x07W",
        );
        let g = t.grid();
        let z = &g[Point::new(Line(0), Column(0))];
        assert_eq!(z.c, 'Z');
        assert_eq!(
            z.hyperlink().map(|h| h.uri().to_string()).as_deref(),
            Some("https://x.example"),
            "OSC 8 URI attaches to the cell"
        );
        // After the closing OSC 8 ; ; the link is cleared.
        assert!(g[Point::new(Line(0), Column(1))].hyperlink().is_none());
    }

    #[test]
    fn alt_screen_preserves_primary_content() {
        let (mut t, mut p) = harness(8, 3);
        feed(&mut t, &mut p, b"main");
        feed(&mut t, &mut p, b"\x1b[?1049h"); // enter alt screen
        assert_eq!(row_text(&t, 0), "", "alt screen starts blank");
        feed(&mut t, &mut p, b"\x1b[2J\x1b[1;1Halt");
        assert_eq!(row_text(&t, 0), "alt");
        feed(&mut t, &mut p, b"\x1b[?1049l"); // back to primary
        assert_eq!(row_text(&t, 0), "main", "primary content restored");
    }

    #[test]
    fn synchronized_output_applies_content() {
        let (mut t, mut p) = harness(8, 2);
        // DECSET 2026 brackets an atomic update; the content must be present
        // and correct once the synchronized update ends.
        feed(&mut t, &mut p, b"\x1b[?2026hhello\x1b[?2026l");
        assert_eq!(row_text(&t, 0), "hello");
    }

    #[test]
    fn decrqm_reports_synchronized_output_mode() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, b"\x1b[?2026$p"); // DECRQM query of mode 2026
        let reply = drain_pty(&rx);
        assert!(
            reply.contains("2026;") && reply.ends_with("$y"),
            "DECRPM should report mode 2026, got {reply:?}"
        );
    }

    #[test]
    fn nel_index_reverse_index() {
        // NEL (ESC E): CR+LF to the next line.
        let (mut t, mut p) = harness(6, 3);
        feed(&mut t, &mut p, b"ab\x1bEcd");
        assert_eq!(row_text(&t, 0), "ab");
        assert_eq!(row_text(&t, 1), "cd");

        // IND (ESC D): down one line, column preserved.
        let (mut t2, mut p2) = harness(6, 3);
        feed(&mut t2, &mut p2, b"X\x1bDY");
        assert_eq!(row_text(&t2, 0), "X");
        assert_eq!(row_text(&t2, 1), " Y", "IND keeps the column");

        // RI (ESC M): up one line, column preserved.
        let (mut t3, mut p3) = harness(6, 3);
        feed(&mut t3, &mut p3, b"\x1b[2;1Hb\x1bMa");
        assert_eq!(row_text(&t3, 0), " a", "RI moved up, kept column");
        assert_eq!(row_text(&t3, 1), "b");
    }

    #[test]
    fn decid_replies_like_da1() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, b"\x1bZ"); // DECID
        let reply = drain_pty(&rx);
        assert!(
            reply.starts_with("\x1b[?") && reply.ends_with('c'),
            "DECID should reply like DA1 (CSI ? … c), got {reply:?}"
        );
    }

    #[test]
    fn cursor_blink_mode_emits_event() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, b"\x1b[?12h"); // DECSET 12 = cursor blink on
        let got = rx
            .try_iter()
            .any(|ev| matches!(ev, TermEvent::CursorBlinkingChange));
        assert!(got, "?12h should signal a cursor-blink change");
    }

    #[test]
    fn dec_mode_12_toggles_engine_cursor_blink_state() {
        // The companion to the event test above: confirm the engine actually
        // tracks the blink state on `cursor_style().blinking` so the UI can
        // read it live (we honor the *running* program's wish for solid vs.
        // blinking cursor, not just the static config). This is what
        // `Terminal::cursor_blinking()` returns — exercised through the real
        // vte parser so the mode-flip path is real.
        let (mut t, mut p) = harness(8, 2);
        let initial = t.cursor_style().blinking;
        feed(&mut t, &mut p, b"\x1b[?12h"); // request blink
        assert!(
            t.cursor_style().blinking,
            "DEC mode 12 set must turn cursor blink on (was {initial})"
        );
        feed(&mut t, &mut p, b"\x1b[?12l"); // request solid
        assert!(
            !t.cursor_style().blinking,
            "DEC mode 12 reset must turn cursor blink off"
        );
    }

    #[test]
    fn cht_cbt_tab_navigation() {
        // CHT (CSI I): forward N tab stops (default stops every 8).
        let (mut t, mut p) = harness(40, 2);
        feed(&mut t, &mut p, b"\x1b[3I*");
        assert_eq!(
            row_text(&t, 0).chars().nth(24),
            Some('*'),
            "CHT 3 → column 24"
        );
        // CBT (CSI Z): backward N tab stops.
        let (mut t2, mut p2) = harness(40, 2);
        feed(&mut t2, &mut p2, b"\x1b[1;21H\x1b[1ZB");
        assert_eq!(
            row_text(&t2, 0).chars().nth(16),
            Some('B'),
            "CBT 1 from col 20 → column 16"
        );
    }

    #[test]
    fn xtwinops_text_area_size_chars() {
        // XTWINOPS CSI 18 t → report text area size in characters as
        // CSI 8 ; rows ; cols t (DA-style, deterministic — no window needed).
        let (mut t, mut p, rx) = harness_rx(40, 10);
        feed(&mut t, &mut p, b"\x1b[18t");
        assert_eq!(
            drain_pty(&rx),
            "\x1b[8;10;40t",
            "CSI 18 t must report 8;<rows>;<cols>t"
        );
    }

    #[test]
    fn dsr_device_status_ok() {
        // DSR CSI 5 n → "terminal OK" = CSI 0 n (no malfunction).
        let (mut t, mut p, rx) = harness_rx(8, 3);
        feed(&mut t, &mut p, b"\x1b[5n");
        assert_eq!(
            drain_pty(&rx),
            "\x1b[0n",
            "CSI 5 n must reply CSI 0 n (ready)"
        );
    }

    #[test]
    fn da1_primary_attributes_exact_params() {
        // Primary DA (CSI c) must reply exactly CSI ? 6 c — VT2xx-class id
        // with no extensions — so apps don't probe for features we lack.
        let (mut t, mut p, rx) = harness_rx(10, 3);
        feed(&mut t, &mut p, b"\x1b[c");
        assert_eq!(
            drain_pty(&rx),
            "\x1b[?6c",
            "DA1 reply must be exactly CSI ? 6 c"
        );
        // CSI 0 c is an explicit-parameter alias for the same query.
        let (mut t2, mut p2, rx2) = harness_rx(10, 3);
        feed(&mut t2, &mut p2, b"\x1b[0c");
        assert_eq!(drain_pty(&rx2), "\x1b[?6c", "CSI 0 c == CSI c");
    }

    #[test]
    fn irm_insert_mode_shifts_right() {
        // Default (replace) vs IRM (CSI 4 h): inserting pushes text right.
        let (mut t, mut p) = harness(10, 2);
        feed(&mut t, &mut p, b"ABCD\x1b[1;1H\x1b[4hX");
        assert_eq!(row_text(&t, 0), "XABCD", "IRM inserts, shifting right");
        feed(&mut t, &mut p, b"\x1b[4l\x1b[1;1HZ");
        assert_eq!(row_text(&t, 0), "ZABCD", "4 l → back to replace");
    }

    #[test]
    fn dectcem_cursor_visibility_mode() {
        let (mut t, mut p) = harness(6, 2);
        assert!(t.mode().contains(TermMode::SHOW_CURSOR), "shown by default");
        feed(&mut t, &mut p, b"\x1b[?25l");
        assert!(!t.mode().contains(TermMode::SHOW_CURSOR), "?25 l hides");
        feed(&mut t, &mut p, b"\x1b[?25h");
        assert!(t.mode().contains(TermMode::SHOW_CURSOR), "?25 h shows");
    }

    #[test]
    fn lnm_newline_mode_sets_flag() {
        // CSI 20 h sets LNM; 20 l clears it. (alacritty_terminal tracks the
        // mode but does not itself translate LF→CRLF on output, so only the
        // mode bit — the conformant, observable part — is asserted here.)
        let (mut t, mut p) = harness(8, 2);
        assert!(!t.mode().contains(TermMode::LINE_FEED_NEW_LINE));
        feed(&mut t, &mut p, b"\x1b[20h");
        assert!(t.mode().contains(TermMode::LINE_FEED_NEW_LINE), "20 h sets");
        feed(&mut t, &mut p, b"\x1b[20l");
        assert!(
            !t.mode().contains(TermMode::LINE_FEED_NEW_LINE),
            "20 l clears LNM"
        );
    }

    #[test]
    fn app_cursor_and_keypad_modes() {
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"\x1b[?1h");
        assert!(t.mode().contains(TermMode::APP_CURSOR), "DECCKM set");
        feed(&mut t, &mut p, b"\x1b=");
        assert!(t.mode().contains(TermMode::APP_KEYPAD), "DECKPAM set");
        feed(&mut t, &mut p, b"\x1b[?1l\x1b>");
        assert!(!t.mode().contains(TermMode::APP_CURSOR));
        assert!(!t.mode().contains(TermMode::APP_KEYPAD), "DECKPNM clears");
    }

    #[test]
    fn mouse_tracking_modes_set_and_clear_flags() {
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"\x1b[?1000h");
        assert!(t.mode().contains(TermMode::MOUSE_REPORT_CLICK));
        feed(&mut t, &mut p, b"\x1b[?1002h\x1b[?1006h");
        assert!(t.mode().contains(TermMode::MOUSE_DRAG), "?1002 = drag");
        assert!(t.mode().contains(TermMode::SGR_MOUSE), "?1006 = SGR enc");
        feed(&mut t, &mut p, b"\x1b[?1003h");
        assert!(
            t.mode().contains(TermMode::MOUSE_MOTION),
            "?1003 = any-motion"
        );
        feed(&mut t, &mut p, b"\x1b[?1000l\x1b[?1002l\x1b[?1003l");
        assert!(
            !t.mode().intersects(TermMode::MOUSE_MODE),
            "all tracking off"
        );
    }

    // SS2/SS3 single-shift (ESC N / ESC O), HTS (ESC H, custom tab
    // stops), DECSCA/DECSEL selective-erase and LNM LF→CRLF *output*
    // translation are not applied by alacritty_terminal, so no conformance
    // test asserts those behaviors (only LNM's mode bit) — see ROADMAP.
}
