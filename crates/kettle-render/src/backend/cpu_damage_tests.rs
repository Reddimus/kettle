//! The damage-tracking equivalence harness.
//!
//! A real CPU window [`Renderer`] is driven through random sequences of the
//! state changes a session makes (typing, cursor moves and blinks, selection
//! drags, scrolling, resizes, themes and opacity, Settings, tabs, splits,
//! toasts, the search bar, image placements, the starfield, font size and
//! HiDPI scale). It presents into [`RotatingSurface`]: up to three buffers
//! that a model compositor holds and releases, with the presenter's own
//! buffer-age history (`cpu_present::damage`), and with every pixel the
//! renderer is told to repaint filled with garbage first.
//!
//! After every frame the buffer the renderer presented must equal, byte for
//! byte, a full repaint of the same frame by a second rasterizer that has
//! drawn every frame whole. Any state change a full repaint shows is
//! therefore shown with damage tracking.
//!
//! `KETTLE_DAMAGE_STEPS` and `KETTLE_DAMAGE_SEEDS` lengthen the run.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use kettle_core::EventProxy;

use super::cpu::{CpuPresentOutcome, CpuRaster, CpuSurface, FrameDraw, HeldFrameFlush};
use super::cpu_draw::SoftRaster;
use crate::cpu_present::damage::{DamageHistory, Exposures, Rect, normalize};
use crate::cpu_raster::PixelRect;
use crate::headless_tests::{pane, snapshot_of};
use crate::*;

/// A deterministic xorshift generator, so a failing seed replays.
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }
}

struct Buffer {
    pixels: Vec<u32>,
    size: (u32, u32),
    /// The frame count after this buffer was last drawn, or `None` when its
    /// contents are unknown.
    drawn: Option<u64>,
}

/// Which presenter [`RotatingSurface`] behaves as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Model {
    /// `wl_shm`: up to three buffers a compositor holds and releases, a frame
    /// callback that paces frames and sometimes holds a drawn one for a later
    /// commit, no commit for a frame with nothing new, and a compositor that
    /// takes from each commit only what it damages.
    Wayland,
    /// softbuffer on X11 with MIT-SHM and no compositing manager: one
    /// retained buffer, each present sends only its rectangles, and other
    /// windows cover and uncover parts of the window, whose pixels the
    /// server then drops (`Expose`).
    X11,
}

/// What the harness reads back from the surface.
#[derive(Default)]
struct Shared {
    /// The pixels of the buffer last drawn into, and its size.
    drawn: Option<(Vec<u32>, u32, u32)>,
    /// What the window shows: the compositor's copy (Wayland) or the X
    /// server's (X11).
    screen: Vec<u32>,
    screen_size: (u32, u32),
    /// The next this many frames are paced, whatever the callback.
    force_paced: u32,
    /// Frames presented with each buffer age (0, 1, 2, 3+).
    ages: [usize; 4],
    /// Frames whose repaint region was not the whole buffer.
    partial: usize,
    /// Frames with an empty repaint region (nothing drawn).
    empty: usize,
    /// Frames that drew an image from the rasterizer's cache.
    cached: usize,
    repainted: u64,
    total: u64,
    most_buffers: usize,
    /// Attempts the compositor was not ready for.
    paced: usize,
    /// Frames drawn and held for a later commit.
    deferred: usize,
    /// Frames drawn with nothing to commit.
    unchanged: usize,
    /// Regions another window uncovered (X11).
    exposed: usize,
    /// Presents with no damage of their own that sent exposed pixels.
    exposures_sent: usize,
}

/// A window surface as one of the presenters treats it ([`Model`]), with
/// the presenter's own buffer-age history (`cpu_present::damage`).
struct RotatingSurface {
    shared: Arc<Mutex<Shared>>,
    model: Model,
    size: (u32, u32),
    buffers: Vec<Buffer>,
    held: Vec<usize>,
    history: DamageHistory,
    frames: u64,
    translucent: bool,
    rng: Rng,
    /// Off for timing runs: garbage-filling and copying out 4K frames would
    /// cost more than drawing them.
    checked: bool,
    /// Wayland: frame callbacks come late and drawn frames are sometimes
    /// held. Off for tests that count what each frame repaints.
    pacing: bool,
    /// Wayland: the last commit's frame callback has not come.
    callback_out: bool,
    /// Wayland: the buffer holding a frame drawn but not committed.
    deferred: Option<usize>,
    /// Damage drawn since the last commit (Wayland) or present (X11).
    uncommitted: Vec<Rect>,
    /// The next commit takes the whole buffer: the first, and after a
    /// resize or a format change.
    force_full: bool,
    /// X11: what other windows uncovered since the last present.
    exposures: Exposures,
    /// A broken presenter for the harness's negative control: the repaint
    /// region is only this frame's damage, whatever the buffer's age.
    ignores_age: bool,
    /// A broken X11 presenter for a negative control: exposed pixels are
    /// never sent again.
    ignores_exposures: bool,
}

impl RotatingSurface {
    fn new(seed: u64, model: Model) -> (Self, Arc<Mutex<Shared>>) {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let surface = Self {
            shared: shared.clone(),
            model,
            size: (0, 0),
            buffers: Vec::new(),
            held: Vec::new(),
            history: DamageHistory::default(),
            frames: 0,
            translucent: false,
            rng: Rng(seed ^ 0x5bd1_e995),
            checked: true,
            pacing: true,
            callback_out: false,
            deferred: None,
            uncommitted: Vec::new(),
            force_full: true,
            exposures: Exposures::default(),
            ignores_age: false,
            ignores_exposures: false,
        };
        (surface, shared)
    }

    /// A surface whose frames are all drawn and committed at once.
    fn steady(seed: u64) -> (Self, Arc<Mutex<Shared>>) {
        let (mut surface, shared) = Self::new(seed, Model::Wayland);
        surface.pacing = false;
        (surface, shared)
    }

    /// Every buffer's contents become unknown, as after a resize or a
    /// format change.
    fn forget(&mut self) {
        self.history.clear();
        for buffer in &mut self.buffers {
            buffer.drawn = None;
        }
        self.deferred = None;
        self.uncommitted.clear();
        self.force_full = true;
    }

    /// The compositor lets go of what it no longer shows. Like mutter it
    /// often has released even the newest buffer by the next frame (age 1);
    /// otherwise it still holds the newest (age 2), and sometimes the one
    /// before it too (age 3, a third buffer).
    fn release(&mut self) {
        let keep = match self.rng.below(6) {
            0 | 1 => 0,
            2 => 2,
            _ => 1,
        };
        while self.held.len() > keep {
            self.held.remove(0);
        }
    }

    fn pick(&mut self) -> usize {
        if self.model == Model::X11 {
            if self.buffers.is_empty() {
                self.buffers.push(Buffer {
                    pixels: Vec::new(),
                    size: (0, 0),
                    drawn: None,
                });
            }
            return 0;
        }
        let (w, h) = self.size;
        let free = (0..self.buffers.len())
            .filter(|i| !self.held.contains(i))
            .max_by_key(|&i| {
                let b = &self.buffers[i];
                (b.size == (w, h), b.drawn.unwrap_or(0))
            });
        if let Some(i) = free {
            return i;
        }
        if self.buffers.len() < 3 {
            self.buffers.push(Buffer {
                pixels: Vec::new(),
                size: (0, 0),
                drawn: None,
            });
            return self.buffers.len() - 1;
        }
        self.held.remove(0)
    }

    /// Whether the compositor is not ready for a frame. Its callback for the
    /// last commit comes, at random, by the time it is asked.
    fn paced(&mut self) -> bool {
        let mut shared = self.shared.lock().unwrap();
        if shared.force_paced > 0 {
            shared.force_paced -= 1;
            return true;
        }
        drop(shared);
        if self.callback_out && (!self.pacing || self.rng.chance(2)) {
            self.callback_out = false;
        }
        self.callback_out
    }

    /// Another window covers part of this one and goes: the X server drops
    /// those pixels and asks for them again.
    fn expose_some(&mut self) {
        let (w, h) = self.size;
        let mut shared = self.shared.lock().unwrap();
        if !self.checked || shared.screen_size != (w, h) || w == 0 || h == 0 || !self.rng.chance(3)
        {
            return;
        }
        let x = self.rng.below(u64::from(w)) as u32;
        let y = self.rng.below(u64::from(h)) as u32;
        let rw = 1 + self.rng.below(u64::from((w - x).min(128))) as u32;
        let rh = 1 + self.rng.below(u64::from((h - y).min(64))) as u32;
        for row in y..y + rh {
            let at = (row * w) as usize;
            for p in &mut shared.screen[at + x as usize..at + (x + rw) as usize] {
                *p = self.rng.next() as u32;
            }
        }
        shared.exposed += 1;
        drop(shared);
        self.exposures.add(Rect::new(x, y, rw, rh));
    }

    /// Show `rects` of buffer `index` on the screen.
    fn show(&mut self, index: usize, rects: &[Rect]) {
        if !self.checked {
            return;
        }
        let (w, h) = self.size;
        let buffer = &self.buffers[index];
        let mut shared = self.shared.lock().unwrap();
        if shared.screen_size != (w, h) {
            shared.screen_size = (w, h);
            shared.screen = vec![0xbad0_bad0; w as usize * h as usize];
        }
        for r in rects {
            for y in r.y..r.y + r.height {
                let row = (y * w) as usize;
                let span = row + r.x as usize..row + (r.x + r.width) as usize;
                shared.screen[span.clone()].copy_from_slice(&buffer.pixels[span]);
            }
        }
    }

    /// Wayland: commit buffer `index` with every change not yet on screen.
    fn commit(&mut self, index: usize, pre_commit: &mut dyn FnMut()) {
        let (w, h) = self.size;
        let damage = if self.force_full {
            vec![Rect::full(w, h)]
        } else {
            std::mem::take(&mut self.uncommitted)
        };
        pre_commit();
        self.show(index, &damage);
        self.uncommitted.clear();
        self.force_full = false;
        self.deferred = None;
        self.callback_out = true;
        self.held.retain(|&i| i != index);
        self.held.push(index);
    }
}

impl CpuSurface for RotatingSurface {
    fn resize(&mut self, width: u32, height: u32, _scale: f64) {
        self.size = (width, height);
        self.forget();
    }

    fn buffer_size(&self) -> (u32, u32) {
        self.size
    }

    fn supports_translucency(&self) -> bool {
        true
    }

    fn set_translucent(&mut self, translucent: bool) -> bool {
        if translucent != self.translucent {
            self.translucent = translucent;
            self.forget();
        }
        translucent
    }

    fn present_frame(
        &mut self,
        draw: &mut dyn FrameDraw,
        pre_commit: &mut dyn FnMut(),
    ) -> CpuPresentOutcome {
        if self.model == Model::Wayland && self.paced() {
            self.shared.lock().unwrap().paced += 1;
            return CpuPresentOutcome::Paced;
        }
        if self.model == Model::X11 {
            self.expose_some();
        } else {
            self.release();
        }
        let (w, h) = self.size;
        let index = self.pick();
        let frames = self.frames;
        let buffer = &mut self.buffers[index];
        if buffer.size != (w, h) {
            buffer.size = (w, h);
            buffer.pixels = vec![0; w as usize * h as usize];
            buffer.drawn = None;
        }
        let age = match buffer.drawn {
            Some(drawn) => u8::try_from(frames - drawn + 1).unwrap_or(0),
            None => 0,
        };
        let damage: Vec<Rect> = draw
            .begin(w, h)
            .iter()
            .map(|r| Rect::new(r.x, r.y, r.w, r.h))
            .collect();
        let current = normalize(&damage, w, h);
        let age = if self.ignores_age && age > 1 { 1 } else { age };
        let repaint: Vec<PixelRect> = self
            .history
            .repaint_region(age, &current, w, h)
            .iter()
            .map(|r| PixelRect::new(r.x, r.y, r.width, r.height))
            .collect();
        // What the renderer must repaint is undefined: fill it with garbage.
        let buffer = &mut self.buffers[index];
        for r in repaint.iter().filter(|_| self.checked) {
            for y in r.y..r.y + r.h {
                let row = (y * w) as usize;
                for p in &mut buffer.pixels[row + r.x as usize..row + (r.x + r.w) as usize] {
                    *p = self.rng.next() as u32;
                }
            }
        }
        let repainted: u64 = repaint
            .iter()
            .map(|r| u64::from(r.w) * u64::from(r.h))
            .sum();
        if !repaint.is_empty() && !draw.draw(&mut buffer.pixels, w, h, &repaint) {
            buffer.drawn = None;
            return CpuPresentOutcome::NotDrawn;
        }
        self.history.push(current.clone());
        self.frames += 1;
        buffer.drawn = Some(self.frames);
        {
            let mut shared = self.shared.lock().unwrap();
            if self.checked {
                shared.drawn = Some((buffer.pixels.clone(), w, h));
            }
            shared.ages[usize::from(age.min(3))] += 1;
            let whole = repaint.len() == 1 && repaint[0] == PixelRect::new(0, 0, w, h);
            shared.partial += usize::from(!whole);
            shared.empty += usize::from(repaint.is_empty());
            shared.repainted += repainted;
            shared.total += u64::from(w) * u64::from(h);
            shared.most_buffers = shared.most_buffers.max(self.buffers.len());
        }
        match self.model {
            Model::Wayland => {
                // This buffer now holds every change not on screen; a frame
                // held in another is superseded.
                self.uncommitted.extend(current);
                self.uncommitted = normalize(&self.uncommitted, w, h);
                self.deferred = None;
                if self.uncommitted.is_empty() && !self.force_full {
                    self.shared.lock().unwrap().unchanged += 1;
                    return CpuPresentOutcome::Unchanged;
                }
                if self.pacing && self.rng.chance(8) {
                    // The compositor's callback is out after all: the frame
                    // waits for it.
                    self.deferred = Some(index);
                    self.callback_out = true;
                    self.shared.lock().unwrap().deferred += 1;
                    return CpuPresentOutcome::Deferred;
                }
                self.commit(index, pre_commit);
                CpuPresentOutcome::Presented
            }
            Model::X11 => {
                let upload = if std::mem::take(&mut self.force_full) {
                    self.exposures.clear();
                    vec![Rect::full(w, h)]
                } else if self.ignores_exposures {
                    self.exposures.clear();
                    current.clone()
                } else {
                    let exposed = !self.exposures.is_empty();
                    let upload = self.exposures.upload(&current, w, h);
                    if exposed && current.is_empty() {
                        self.shared.lock().unwrap().exposures_sent += 1;
                    }
                    upload
                };
                pre_commit();
                self.show(index, &upload);
                if upload.is_empty() {
                    self.shared.lock().unwrap().unchanged += 1;
                    CpuPresentOutcome::Unchanged
                } else {
                    CpuPresentOutcome::Presented
                }
            }
        }
    }

    fn flush(&mut self, pre_commit: &mut dyn FnMut()) -> HeldFrameFlush {
        let Some(index) = self.deferred else {
            return HeldFrameFlush::Nothing;
        };
        if self.paced() {
            return HeldFrameFlush::StillHeld;
        }
        self.commit(index, pre_commit);
        HeldFrameFlush::Presented
    }

    fn has_deferred(&self) -> bool {
        self.deferred.is_some()
    }

    fn pending_deadline(&self) -> Option<Instant> {
        self.deferred.map(|_| Instant::now())
    }

    fn frame_pending(&mut self) -> bool {
        self.model == Model::Wayland && self.callback_out
    }

    fn frame_deadline(&self) -> Option<Instant> {
        self.callback_out.then(Instant::now)
    }

    fn release_idle_buffers(&mut self) -> usize {
        0
    }

    fn idle_release_deadline(&self) -> Option<Instant> {
        None
    }

    fn set_occluded(&mut self, _occluded: bool) {}

    fn describe(&self) -> &'static str {
        "rotating"
    }
}

struct Grid {
    cols: usize,
    rows: usize,
}

impl Dimensions for Grid {
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

fn term(cols: usize, rows: usize) -> alacritty_terminal::Term<EventProxy> {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let proxy = EventProxy::new(tx, Arc::new(|| {}));
    alacritty_terminal::Term::new(TermConfig::default(), &Grid { cols, rows }, proxy)
}

const THEMES: [&str; 4] = [
    "TokyoNight Night",
    "TokyoNight Day",
    "Dracula",
    "Solarized Dark",
];

/// The session the harness drives.
struct World {
    cfg: kettle_config::Config,
    size: (u32, u32),
    terms: [alacritty_terminal::Term<EventProxy>; 2],
    parser: Processor<StdSyncHandler>,
    split: bool,
    focus: usize,
    cursor_on: bool,
    selection: Option<(Point, Point)>,
    settings: Option<SettingsOverlay>,
    toast: bool,
    search: bool,
    /// A placed image, the cells it spans, and the absolute line it was
    /// placed on.
    image: Option<(kettle_core::ImageData, usize, usize, u64)>,
    /// The image placements made so far: they cycle through a small
    /// image, a large one, and none.
    images_placed: u64,
    /// A second large translucent image, drawn after the first and
    /// overlapping it: the rasterizer caches one image at a time.
    image2: Option<(kettle_core::ImageData, u64)>,
    /// Two tabs that look alike, each with a pending card at the same place
    /// and concealed text (right-to-left in one, left-to-right in the
    /// other): `Some(t)` shows tab `t` in the first pane, tab bar hidden.
    twins: Option<usize>,
    twin_terms: [alacritty_terminal::Term<EventProxy>; 2],
    twin_cards: crate::inline_cards::InlineCards,
    tabs: Option<usize>,
    scale: f32,
    rng: Rng,
}

impl World {
    fn new(seed: u64) -> Self {
        let (twin_cards, twin_terms, _) = crate::inline_cards::tests::twin_fixture();
        let mut world = Self {
            cfg: kettle_config::Config {
                background_opacity: 1.0,
                ..crate::gpu_tests::gpu_test_config()
            },
            size: (560, 380),
            terms: [term(64, 22), term(30, 22)],
            parser: Processor::new(),
            split: false,
            focus: 0,
            cursor_on: true,
            selection: None,
            settings: None,
            toast: false,
            search: false,
            image: None,
            images_placed: 0,
            image2: None,
            twins: None,
            twin_terms,
            twin_cards,
            tabs: None,
            scale: 1.0,
            rng: Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) | 1),
        };
        // Enough history to scroll, and a prompt.
        let mut text = Vec::new();
        for i in 0..30 {
            text.extend_from_slice(
                format!("\x1b[3{}mline {i} \x1b[1mbold\x1b[0m text\r\n", i % 8).as_bytes(),
            );
        }
        // A right-to-left row, drawn in bidi order while `bidi` is on.
        text.extend_from_slice("\u{05e9}\u{05dc}\u{05d5}\u{05dd} (1)\r\n".as_bytes());
        text.extend_from_slice(b"$ ");
        world.feed(0, &text);
        world.feed(1, b"other pane\r\n$ ");
        world
    }

    /// The absolute line `row` rows below the first pane's viewport top.
    fn line_at(&self, row: u64) -> u64 {
        let mut snap = PaneSnapshot::default();
        snap.capture(&self.terms[0]);
        crate::geometry::snapshot_viewport_top(&snap) + row
    }

    fn feed(&mut self, pane: usize, bytes: &[u8]) {
        self.parser.advance(&mut self.terms[pane], bytes);
    }

    fn settings_overlay(focused_row: usize) -> SettingsOverlay {
        SettingsOverlay {
            categories: ["Appearance", "Background", "Behavior", "Search"]
                .map(str::to_string)
                .to_vec(),
            rows: (0..14)
                .map(|i| SettingsRow {
                    label: format!("Setting {i}"),
                    value: format!("{} pt", 10 + i),
                    description: "What this setting does.".into(),
                    control: SettingsControl::Value,
                    ..Default::default()
                })
                .collect(),
            focused_row,
            title: "Appearance".into(),
            search_placeholder: "Search settings".into(),
            no_matches: "No settings match".into(),
            hints: vec![("Esc".into(), "close".into())],
            ..Default::default()
        }
    }

    /// Make one random change, as `renderer` sees it; returns its name.
    fn change(&mut self, renderer: &mut Renderer) -> &'static str {
        let pick = self.rng.below(100);
        self.apply(renderer, pick)
    }

    /// Make change `pick` (0..100, weighted by range).
    fn apply(&mut self, renderer: &mut Renderer, pick: u64) -> &'static str {
        let pane = self.focus;
        match pick {
            0..=19 => {
                let c = b"abcdefghijklmnopqrstuvwxyz0123456789 -=./"[self.rng.below(41) as usize];
                self.feed(pane, &[c]);
                "type"
            }
            20..=24 => {
                self.feed(pane, b"\r\n$ ");
                "enter"
            }
            25..=29 => {
                let (r, c) = (1 + self.rng.below(22), 1 + self.rng.below(60));
                self.feed(pane, format!("\x1b[{r};{c}H").as_bytes());
                "cursor move"
            }
            30..=39 => {
                self.cursor_on = !self.cursor_on;
                "blink"
            }
            40..=45 => {
                let line = self.rng.below(22) as i32;
                let col = self.rng.below(64) as usize;
                let end = Point::new(Line(line), Column(col));
                self.selection = match self.selection {
                    Some((start, _)) if !self.rng.chance(4) => Some((start, end)),
                    _ => Some((end, end)),
                };
                "selection drag"
            }
            46 => {
                self.selection = None;
                "selection clear"
            }
            47..=50 => {
                let delta = if self.rng.chance(2) { 3 } else { -3 };
                self.terms[pane].scroll_display(Scroll::Delta(delta));
                "scroll"
            }
            51..=53 => {
                self.size = (
                    360 + self.rng.below(320) as u32,
                    240 + self.rng.below(220) as u32,
                );
                renderer.resize(self.size.0, self.size.1);
                "resize"
            }
            54..=55 => {
                self.cfg.theme = kettle_config::Theme::by_name(THEMES[self.rng.below(4) as usize]);
                "theme"
            }
            56..=57 => {
                self.cfg.background_opacity = if self.cfg.background_opacity < 1.0 {
                    1.0
                } else {
                    0.85
                };
                "opacity"
            }
            58..=60 => {
                self.settings = match self.settings.take() {
                    None => Some(Self::settings_overlay(0)),
                    Some(_) if self.rng.chance(3) => None,
                    Some(mut s) => {
                        s.focused_row = self.rng.below(14) as usize;
                        Some(s)
                    }
                };
                "settings"
            }
            61..=63 => {
                self.tabs = match self.tabs {
                    None => Some(0),
                    Some(_) if self.rng.chance(4) => None,
                    Some(active) => Some(1 - active),
                };
                "tab switch"
            }
            64..=65 => {
                self.split = !self.split;
                self.focus = 0;
                "split"
            }
            66 => {
                if self.split {
                    self.focus = 1 - self.focus;
                }
                "focus pane"
            }
            67..=68 => {
                self.toast = !self.toast;
                "toast"
            }
            69..=70 => {
                self.search = !self.search;
                "search bar"
            }
            71..=73 => {
                // A small inline image, partly translucent; then one large
                // and translucent enough that the rasterizer caches it from
                // its second frame (`cpu_draw.rs`); then none.
                self.images_placed += 1;
                self.image = match self.images_placed % 3 {
                    0 => None,
                    phase => {
                        let large = phase == 2;
                        let (w, h) = (
                            24 + self.rng.below(24) as u32,
                            16 + self.rng.below(16) as u32,
                        );
                        let cells = if large { (60, 20) } else { (4, 2) };
                        let img = if large {
                            translucent_image(w, h, 0)
                        } else {
                            let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                            for y in 0..h {
                                for x in 0..w {
                                    let a = if (x + y) % 5 == 0 { 120 } else { 255 };
                                    rgba.extend_from_slice(&[
                                        (x * 9) as u8,
                                        (y * 13) as u8,
                                        200,
                                        a,
                                    ]);
                                }
                            }
                            kettle_core::ImageData::new(w, h, rgba).expect("a small image")
                        };
                        Some((img, cells.0, cells.1, self.line_at(1)))
                    }
                };
                "image placement"
            }
            74 => {
                self.cfg.background_type = match self.cfg.background_type {
                    kettle_config::BackgroundType::Starfield => {
                        kettle_config::BackgroundType::Solid
                    }
                    _ => kettle_config::BackgroundType::Starfield,
                };
                "starfield"
            }
            75..=76 => {
                renderer.set_font_size(11.0 + self.rng.below(7) as f32);
                "font size"
            }
            77 => {
                // Always a different scale: 1 -> 1.5 or 2, and so on.
                let scales = [1.0, 1.5, 2.0];
                let now = scales.iter().position(|&s| s == self.scale).unwrap_or(0);
                self.scale = scales[(now + 1 + self.rng.below(2) as usize) % 3];
                renderer.set_scale(self.scale);
                "scale"
            }
            78..=83 => {
                let line = 1 + self.rng.below(22);
                self.feed(
                    pane,
                    format!(
                        "\x1b7\x1b[{line};20H\x1b[4{}m status \x1b[0m\x1b8",
                        line % 8
                    )
                    .as_bytes(),
                );
                "small update"
            }
            84..=85 => {
                self.image2 = match self.image2 {
                    Some(_) => None,
                    None => Some((translucent_image(40, 28, 7), self.line_at(3))),
                };
                "second image"
            }
            86 => {
                self.twins = Some(self.twins.map_or(0, |t| 1 - t));
                "twin switch"
            }
            87 => {
                self.twins = None;
                "twins off"
            }
            88 => {
                self.cfg.bidi = !self.cfg.bidi;
                "bidi"
            }
            90 => {
                // Large enough that a large image is cached.
                self.size = (640, 420);
                renderer.resize(self.size.0, self.size.1);
                "resize"
            }
            _ => "idle",
        }
    }

    /// Draw one frame of the session.
    fn render(&self, renderer: &mut Renderer) -> FrameOutcome {
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let mut snaps: Vec<PaneSnapshot> = self
            .terms
            .iter()
            .map(|t| {
                let mut snap = PaneSnapshot::default();
                snap.capture(t);
                snap
            })
            .collect();
        if let Some((start, end)) = self.selection {
            let snap = &mut snaps[self.focus];
            let origin = -(snap.display_offset as i32);
            let at = |p: Point| Point::new(Line(p.line.0 + origin), p.column);
            let (a, b) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            snap.selection = Some(SelectionRange::new(at(a), at(b), false));
        }
        let top = if self.tabs.is_some() { 28.0 } else { 0.0 };
        let mut tabbar = TabBar::hidden();
        if let Some(active) = self.tabs {
            tabbar.height = 28.0;
            tabbar.band = (0.0, 0.0, w, 28.0);
            for i in 0..2 {
                tabbar.segments.push(TabSeg {
                    idx: i,
                    rect: (i as f32 * 120.0, 0.0, 118.0, 28.0),
                    title_rect: (i as f32 * 120.0 + 8.0, 0.0, 90.0, 28.0),
                    hint: None,
                    close: (i as f32 * 120.0 + 98.0, 4.0, 18.0, 18.0),
                    title: format!("tab {i}"),
                    path: None,
                    active: i == active,
                    activity: TabActivity::Normal,
                });
            }
        }
        let place =
            |img: &kettle_core::ImageData, line: u64, col: usize, cells: (usize, usize), id| {
                kettle_core::Placement {
                    abs_line: line,
                    col,
                    cell_cols: cells.0,
                    cell_rows: cells.1,
                    x_offset_cells: 0.0,
                    y_offset_cells: 0.0,
                    display_cols: cells.0 as f32,
                    display_rows: cells.1 as f32,
                    img: img.clone(),
                    source_rect: None,
                    source_crop: None,
                    id: Some(id),
                    placement_id: 0,
                    kitty_params: None,
                    z: 0,
                }
            };
        let images: Vec<kettle_core::Placement> = self
            .image
            .iter()
            .map(|(img, cols, rows, line)| place(img, *line, 2, (*cols, *rows), 7))
            .chain(
                self.image2
                    .iter()
                    .map(|(img, line)| place(img, *line, 5, (60, 20), 8)),
            )
            .collect();
        let twin = self.twins.map(|t| {
            let mut snap = PaneSnapshot::default();
            snap.capture_with_card_marks(&self.twin_terms[t], true);
            (t, snap)
        });
        let mut panes = Vec::new();
        let split_w = if self.split {
            (w / 2.0 - 2.0).floor()
        } else {
            w
        };
        let mut first = match &twin {
            Some((t, snap)) => {
                let mut view = pane(snap, self.size.0, self.size.1);
                view.id = 100 + *t as u64;
                view.inline_cards = Some(&self.twin_cards);
                view
            }
            None => pane(&snaps[0], self.size.0, self.size.1),
        };
        first.rect = (0.0, top, split_w, h - top);
        first.terminal = first.rect;
        first.images = &images;
        first.focused = self.focus == 0;
        panes.push(first);
        if self.split {
            let mut second = pane(&snaps[1], self.size.0, self.size.1);
            second.id = 2;
            second.rect = (split_w + 4.0, top, w - split_w - 4.0, h - top);
            second.terminal = second.rect;
            second.focused = self.focus == 1;
            panes.push(second);
        }
        let mut overlay = Overlay {
            window_focused: true,
            cursor_visible: self.cursor_on,
            settings: self.settings.clone(),
            ..Overlay::default()
        };
        if self.toast {
            let rect = (w - 200.0, h - 60.0, 190.0, 50.0);
            overlay.update_toast = Some(UpdateToastOverlay {
                geometry: UpdateToastGeometry {
                    rect,
                    message: (rect.0 + 10.0, rect.1 + 8.0, 120.0, 18.0),
                    details: (rect.0 + 10.0, rect.1 + 28.0, 80.0, 16.0),
                    close: (rect.0 + 160.0, rect.1 + 8.0, 20.0, 20.0),
                    radius: 8.0,
                    unit: 1.0,
                },
                message: "Kettle updated".into(),
                details: "Details".into(),
                close: "x".into(),
                opacity: 1.0,
                focused: None,
            });
        }
        if self.search {
            overlay.search = Some(SearchOverlay {
                query: "line".into(),
                cursor_byte: 4,
                focused: SearchControl::Editor,
                ..SearchOverlay::default()
            });
        }
        renderer
            .render_frame_with_status_and_pre_present(
                &panes,
                &tabbar,
                &self.cfg,
                &overlay,
                &StatusBar::hidden(),
                || {},
            )
            .expect("a damage-tracked frame renders")
    }
}

/// A `w` x `h` image translucent enough that the rasterizer caches a large
/// draw of it from its second frame (`cpu_draw.rs`), tinted by `tint`.
fn translucent_image(w: u32, h: u32, tint: u8) -> kettle_core::ImageData {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 9) as u8 ^ tint,
                (y * 13) as u8,
                200u8.wrapping_sub(tint.wrapping_mul(9)),
                190,
            ]);
        }
    }
    kettle_core::ImageData::new(w, h, rgba).expect("a small image")
}

/// Every kind of change, in an order that also opens, scrolls and closes
/// Settings, switches and hides tabs, and turns each toggle on and off:
/// the scripted start of every session, so every session covers them all.
const SCRIPT: [u64; 54] = [
    0, 30, 25, 20, 40, 41, 41, 46, 47, 47, 51, 54, 56, 58, 59, 59, 61, 61, 62, 64, 66, 0, 67, 69,
    // One pane again, a small image, then a large one that small changes
    // leave in place, so its second frame draws it from the cache; then a
    // second large one over it for a few frames, with a blink between.
    64, 90, 71, 71, 30, 30, 84, 89, 30, 89, 84,
    // Two tabs that look alike, switched, a blink, bidi off and on again.
    86, 89, 86, 30, 88, 89, 88, 87, 74, 0, 75, 77, 78, 89, 67, 69, 74, 56, 58,
];

/// Every change's name, which the harness must all see.
const CHANGES: [&str; 27] = [
    "type",
    "enter",
    "cursor move",
    "blink",
    "selection drag",
    "selection clear",
    "scroll",
    "resize",
    "theme",
    "opacity",
    "settings",
    "tab switch",
    "split",
    "focus pane",
    "toast",
    "search bar",
    "image placement",
    "starfield",
    "font size",
    "scale",
    "small update",
    "second image",
    "twin switch",
    "twins off",
    "bidi",
    "idle",
    "first frame",
];

/// The first differing pixels of two frames, for a failure message.
fn describe_difference(got: &[u32], want: &[u32], w: u32) -> String {
    let mut count = 0usize;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (i, (a, b)) in got.iter().zip(want).enumerate() {
        if a != b {
            count += 1;
            let (x, y) = (i as u32 % w, i as u32 / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    format!("{count} pixels differ, within x {x0}..={x1}, y {y0}..={y1}")
}

fn env_count(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A frame's painted state, comparable: its cards, and each pane's bidi
/// rows.
type Painted = (Vec<PaintedCard>, Vec<BidiDigest>);
type BidiDigest = (
    (u32, u32, u32, u32),
    Vec<Option<kettle_core::bidi::RowBidi>>,
);

fn bidi_digest(painted: &[crate::PaintedBidi]) -> Vec<BidiDigest> {
    painted
        .iter()
        .map(|p| {
            let (x, y, w, h) = p.terminal;
            (
                (x.to_bits(), y.to_bits(), w.to_bits(), h.to_bits()),
                (0..64).map(|row| p.row(row).cloned()).collect(),
            )
        })
        .collect()
}

fn drawn_paint(renderer: &Renderer) -> Painted {
    (
        renderer.drawn_cards().to_vec(),
        bidi_digest(&renderer.drawn_bidi),
    )
}

fn painted_paint(renderer: &Renderer) -> Painted {
    (
        renderer.painted_cards().to_vec(),
        bidi_digest(&renderer.painted_bidi),
    )
}

/// What a session's broken presenter gets wrong, for the negative controls.
#[derive(Clone, Copy, Default)]
struct Broken {
    ignores_age: bool,
    ignores_exposures: bool,
}

/// Run one seeded session of `steps` changes on `model`, checking every frame.
fn run_session(seed: u64, steps: u64, model: Model) -> (Shared, Vec<&'static str>) {
    let script: Vec<u64> = SCRIPT.to_vec();
    session(seed, steps, model, &script, Broken::default())
}

/// A session: the first frame, `script`'s changes, then random ones up to
/// `steps`. After every frame drawn the buffer must equal a full repaint of
/// it, and whenever no frame is held, the screen must show it, with the
/// cards and bidi rows it painted on screen.
fn session(
    seed: u64,
    steps: u64,
    model: Model,
    script: &[u64],
    broken: Broken,
) -> (Shared, Vec<&'static str>) {
    let mut world = World::new(seed);
    let (mut surface, shared) = RotatingSurface::new(seed, model);
    surface.ignores_age = broken.ignores_age;
    surface.ignores_exposures = broken.ignores_exposures;
    let mut renderer =
        Renderer::cpu_window_for_tests(&world.cfg, world.size.0, world.size.1, Box::new(surface))
            .expect("CPU window");
    // The reference rasterizer draws every frame drawn, whole.
    let mut reference: Box<dyn CpuRaster> = Box::new(SoftRaster::new());
    let mut log: Vec<&'static str> = Vec::new();
    let mut harness = Rng(seed ^ 0x2f0d_1c4b);
    for step in 0..=steps {
        let change = match step {
            0 => "first frame",
            n if n as usize <= script.len() => world.apply(&mut renderer, script[n as usize - 1]),
            _ => world.change(&mut renderer),
        };
        log.push(change);
        let context = |log: &[&str]| {
            let recent = &log[log.len().saturating_sub(8)..];
            format!("seed {seed} {model:?} step {step} ({change}), recent changes {recent:?}")
        };
        // Draw the frame; an attempt the compositor is not ready for is drawn
        // again once it is, and the loop flushes a held frame meanwhile.
        let mut attempts = 0;
        loop {
            match world.render(&mut renderer) {
                FrameOutcome::Presented => break,
                FrameOutcome::Paced => {
                    attempts += 1;
                    assert!(attempts < 64, "{}: paced forever", context(&log));
                    if harness.chance(2) {
                        renderer.flush_held_frame(|| {});
                    }
                }
                outcome => panic!("{}: {outcome:?}", context(&log)),
            }
        }
        let want = renderer
            .cpu_full_repaint_for_tests(reference.as_mut(), &world.cfg, world.cursor_on)
            .expect("a full repaint");
        let painted = drawn_paint(&renderer);
        let planned = renderer
            .cpu_backend_for_tests()
            .map_or(0, |cpu| cpu.damage.last.planned);
        let mut s = shared.lock().unwrap();
        s.cached += usize::from(planned > 0);
        let (got, w, h) = s.drawn.take().expect("a frame was drawn");
        drop(s);
        assert_eq!((w, h), (want.1, want.2), "{}: buffer size", context(&log));
        if got != want.0 {
            panic!(
                "{}: the damage-tracked buffer differs from a full repaint: {}; damage {:?}",
                context(&log),
                describe_difference(&got, &want.0, w),
                renderer.cpu_backend_for_tests().map(|cpu| &cpu.damage.last),
            );
        }
        if renderer.has_held_frame() && harness.chance(2) {
            renderer.flush_held_frame(|| {});
        }
        if renderer.has_held_frame() {
            // The screen shows an older frame until the held one goes out.
            continue;
        }
        let s = shared.lock().unwrap();
        assert_eq!(s.screen_size, (w, h), "{}: screen size", context(&log));
        if s.screen != want.0 {
            panic!(
                "{}: the screen differs from a full repaint: {}; damage {:?}",
                context(&log),
                describe_difference(&s.screen, &want.0, w),
                renderer.cpu_backend_for_tests().map(|cpu| &cpu.damage.last),
            );
        }
        drop(s);
        assert!(
            painted_paint(&renderer) == painted,
            "{}: what is on screen is not what the frame on screen painted: cards {:?}, \
             drawn {:?}",
            context(&log),
            renderer.painted_cards(),
            painted.0,
        );
    }
    let stats = std::mem::take(&mut *shared.lock().unwrap());
    (stats, log)
}

#[test]
fn damage_tracked_frames_equal_full_repaints_through_random_sessions() {
    let seeds = env_count("KETTLE_DAMAGE_SEEDS", 4);
    let steps = env_count("KETTLE_DAMAGE_STEPS", 80).max(SCRIPT.len() as u64);
    let mut totals = [Shared::default(), Shared::default()];
    let mut seen = std::collections::BTreeSet::new();
    for seed in 1..=seeds {
        // Odd seeds present as on Wayland, even ones as on X11.
        let model = if seed % 2 == 1 {
            Model::Wayland
        } else {
            Model::X11
        };
        let (s, log) = run_session(seed * 0x9e37_79b9, steps, model);
        seen.extend(log);
        let total = &mut totals[usize::from(model == Model::X11)];
        for (t, a) in total.ages.iter_mut().zip(s.ages) {
            *t += a;
        }
        total.partial += s.partial;
        total.cached += s.cached;
        total.empty += s.empty;
        total.repainted += s.repainted;
        total.total += s.total;
        total.most_buffers = total.most_buffers.max(s.most_buffers);
        total.paced += s.paced;
        total.deferred += s.deferred;
        total.unchanged += s.unchanged;
        total.exposed += s.exposed;
        total.exposures_sent += s.exposures_sent;
    }
    for (model, total) in [Model::Wayland, Model::X11].iter().zip(&totals) {
        let frames: usize = total.ages.iter().sum();
        eprintln!(
            "damage harness ({model:?}): {steps} steps a seed, {frames} frames drawn; ages \
             0/1/2/3+ {:?}; {} partial, {} empty, {} with a cached image, {} unchanged; {} paced \
             attempts, {} held; {} exposures, {} sent by frames with no damage; repainted \
             {:.1}% of the pixels drawn; up to {} buffers",
            total.ages,
            total.partial,
            total.empty,
            total.cached,
            total.unchanged,
            total.paced,
            total.deferred,
            total.exposed,
            total.exposures_sent,
            100.0 * total.repainted as f64 / total.total.max(1) as f64,
            total.most_buffers,
        );
        // The harness must exercise what it claims to: every change, buffers
        // of every age, frames that repaint only part of the buffer, frames
        // with nothing to commit, and for each presenter what it alone does.
        assert!(
            total.partial * 4 > frames,
            "{model:?}: too few partial frames: {}",
            total.partial
        );
        assert!(
            total.cached > 0,
            "{model:?}: no frame drew an image from the cache"
        );
        assert!(total.unchanged > 0, "{model:?}: no frame was unchanged");
        match model {
            Model::Wayland => {
                assert!(total.most_buffers >= 2, "frames rotated through one buffer");
                assert!(
                    total.ages[1] > 0 && total.ages[2] > 0,
                    "ages {:?}",
                    total.ages
                );
                assert!(total.paced > 0, "no attempt was paced");
                assert!(total.deferred > 0, "no frame was held");
            }
            Model::X11 => {
                assert!(
                    total.exposures_sent > 0,
                    "no frame without damage sent exposed pixels"
                );
            }
        }
    }
    let missing: Vec<_> = CHANGES.iter().filter(|c| !seen.contains(*c)).collect();
    assert!(missing.is_empty(), "changes never made: {missing:?}");
}

/// The harness catches a presenter that hands back an old buffer without
/// the damage of the frames it missed.
#[test]
#[should_panic(expected = "differs from a full repaint")]
fn the_harness_fails_a_presenter_that_ignores_buffer_age() {
    let broken = Broken {
        ignores_age: true,
        ..Broken::default()
    };
    session(5, 60, Model::Wayland, &SCRIPT, broken);
}

/// The harness catches an X11 presenter that sends only each frame's
/// damage, so what another window uncovered stays lost.
#[test]
#[should_panic(expected = "the screen differs from a full repaint")]
fn the_harness_fails_an_x11_presenter_that_ignores_exposures() {
    let broken = Broken {
        ignores_exposures: true,
        ..Broken::default()
    };
    session(6, 60, Model::X11, &SCRIPT, broken);
}

/// A large translucent image, then a second one drawn after it, both drawn
/// from the cache on their second frame if they could be: the second must
/// never take the cache from the first while a frame draws the first from
/// it, or the tiles the frame repaints (the second image, a blink) draw the
/// first directly next to tiles that keep its cached pixels.
#[test]
fn a_second_image_never_takes_the_cache_from_the_first_mid_frame() {
    for model in [Model::Wayland, Model::X11] {
        // Two placements: small, then large; idle; the second image; idle;
        // blinks; the second image off; blinks.
        let script = [90, 71, 71, 89, 89, 84, 89, 89, 30, 30, 89, 84, 30, 30, 89];
        let (stats, _) = session(11, script.len() as u64, model, &script, Broken::default());
        assert!(
            stats.cached >= 6,
            "{model:?}: {} frames drew from the cache",
            stats.cached
        );
    }
}

/// An attempt the compositor is not ready for draws nothing, so it must
/// leave the rasterizer's caches as they were: with one between every two
/// frames drawn, a still image must still be cached from its second frame.
#[test]
fn a_paced_attempt_leaves_the_image_cache_alone() {
    let cfg = kettle_config::Config {
        background_opacity: 1.0,
        ..crate::gpu_tests::gpu_test_config()
    };
    let (surface, shared) = RotatingSurface::steady(17);
    let mut renderer =
        Renderer::cpu_window_for_tests(&cfg, 560, 380, Box::new(surface)).expect("CPU window");
    let snap = snapshot_of(64, 22, b"$ ");
    let image = translucent_image(40, 28, 0);
    let placement = kettle_core::Placement {
        abs_line: snap.history_origin + 1,
        col: 2,
        cell_cols: 60,
        cell_rows: 20,
        x_offset_cells: 0.0,
        y_offset_cells: 0.0,
        display_cols: 60.0,
        display_rows: 20.0,
        img: image,
        source_rect: None,
        source_crop: None,
        id: Some(7),
        placement_id: 0,
        kitty_params: None,
        z: 0,
    };
    let images = [placement];
    let mut cached = Vec::new();
    for frame in 0..6 {
        for attempt in 0..2 {
            // Every frame after the first meets the compositor busy once.
            shared.lock().unwrap().force_paced = u32::from(frame > 0 && attempt == 0);
            let mut view = pane(&snap, 560, 380);
            view.images = &images;
            let outcome = renderer
                .render_frame_with_status_and_pre_present(
                    &[view],
                    &TabBar::hidden(),
                    &cfg,
                    &Overlay {
                        window_focused: true,
                        cursor_visible: frame % 2 == 0,
                        ..Overlay::default()
                    },
                    &StatusBar::hidden(),
                    || {},
                )
                .expect("frame");
            if outcome == FrameOutcome::Presented {
                break;
            }
            assert_eq!(outcome, FrameOutcome::Paced, "frame {frame}");
        }
        let cpu = renderer.cpu_backend_for_tests().expect("CPU");
        cached.push(cpu.damage.last.planned > 0);
    }
    assert_eq!(
        shared.lock().unwrap().paced,
        5,
        "every frame after the first met one"
    );
    assert_eq!(
        cached,
        [false, true, true, true, true, true],
        "the image is drawn from the cache from its second frame on"
    );
}

/// Two tabs that look alike, each with a pending card at the same place,
/// with the tab bar hidden: switching draws nothing new, but the cards on
/// screen (which a click or assistive technology reaches) and the bidi rows
/// (which map the pointer to cells) are the new tab's.
#[test]
fn switching_between_tabs_that_look_alike_puts_the_new_tab_on_screen() {
    for model in [Model::Wayland, Model::X11] {
        let mut world = World::new(3);
        world.cfg.bidi = true;
        let (mut surface, _shared) = RotatingSurface::new(3, model);
        surface.pacing = false;
        let mut renderer = Renderer::cpu_window_for_tests(
            &world.cfg,
            world.size.0,
            world.size.1,
            Box::new(surface),
        )
        .expect("CPU window");
        let (_, _, nonces) = crate::inline_cards::tests::twin_fixture();
        for (step, pick) in [86u64, 89, 86, 86, 86].into_iter().enumerate() {
            world.apply(&mut renderer, pick);
            assert_eq!(world.render(&mut renderer), FrameOutcome::Presented);
            let unchanged = renderer
                .cpu_backend_for_tests()
                .is_some_and(|cpu| cpu.damage.last.damage.is_empty());
            let t = world.twins.expect("a twin shown");
            if step >= 2 {
                assert!(unchanged, "{model:?} step {step}: the twins look alike");
            }
            let cards = renderer.painted_cards();
            assert_eq!(cards.len(), 1, "{model:?} step {step}");
            assert_eq!(
                (cards[0].pane, cards[0].nonce),
                (100 + t as u64, nonces[t]),
                "{model:?} step {step}: the card on screen is the shown tab's"
            );
            assert!(painted_paint(&renderer) == drawn_paint(&renderer));
            let rtl = renderer
                .painted_row_bidi((0.0, 0.0, world.size.0 as f32, world.size.1 as f32), 0)
                .is_some();
            assert_eq!(rtl, t == 0, "{model:?} step {step}: tab {t}'s first row");
        }
    }
}

/// One typed character and one blink at `size`: the frame repaints only
/// the cells involved, not the window.
fn small_changes_repaint_little(size: (u32, u32), scale: f32) {
    let cfg = kettle_config::Config {
        background_opacity: 1.0,
        ..crate::gpu_tests::gpu_test_config()
    };
    let (surface, shared) = RotatingSurface::steady(7);
    let mut renderer = Renderer::cpu_window_for_tests(&cfg, size.0, size.1, Box::new(surface))
        .expect("CPU window");
    renderer.set_scale(scale);
    let mut t = term(80, 24);
    let mut parser = Processor::<StdSyncHandler>::new();
    parser.advance(&mut t, b"$ echo hello\r\nhello\r\n$ ");
    let frame = |renderer: &mut Renderer, t: &alacritty_terminal::Term<EventProxy>, on: bool| {
        let mut snap = PaneSnapshot::default();
        snap.capture(t);
        let view = pane(&snap, size.0, size.1);
        let outcome = renderer
            .render_frame_with_status_and_pre_present(
                &[view],
                &TabBar::hidden(),
                &cfg,
                &Overlay {
                    window_focused: true,
                    cursor_visible: on,
                    ..Overlay::default()
                },
                &StatusBar::hidden(),
                || {},
            )
            .expect("frame");
        assert!(matches!(outcome, FrameOutcome::Presented), "{outcome:?}");
        renderer
            .cpu_backend_for_tests()
            .expect("CPU backend")
            .damage
            .last
            .clone()
    };
    for _ in 0..3 {
        frame(&mut renderer, &t, true);
    }
    let whole = u64::from(size.0) * u64::from(size.1);
    parser.advance(&mut t, b"l");
    let typed = frame(&mut renderer, &t, true);
    let blink = frame(&mut renderer, &t, false);
    let unchanged = frame(&mut renderer, &t, false);
    for (what, stats) in [("typing", &typed), ("a blink", &blink)] {
        assert!(
            !stats.full,
            "{what} at {size:?} repainted in full: {stats:?}"
        );
        assert!(
            stats.repainted_pixels() * 50 < whole,
            "{what} at {size:?} repainted {} of {whole} pixels: {stats:?}",
            stats.repainted_pixels()
        );
    }
    assert!(
        unchanged.damage.is_empty(),
        "an unchanged frame damages nothing"
    );
    drop(shared);
}

#[test]
fn typing_and_a_blink_repaint_a_few_cells() {
    small_changes_repaint_little((800, 500), 1.0);
    small_changes_repaint_little((1280, 800), 2.0);
}

#[test]
fn the_full_repaint_switch_repaints_every_frame_whole() {
    let cfg = kettle_config::Config {
        background_opacity: 1.0,
        ..crate::gpu_tests::gpu_test_config()
    };
    let (surface, shared) = RotatingSurface::steady(3);
    let mut renderer =
        Renderer::cpu_window_for_tests(&cfg, 200, 80, Box::new(surface)).expect("CPU window");
    renderer.set_cpu_full_repaint_for_tests(true);
    let snap = snapshot_of(20, 4, b"hello");
    for on in [true, false, true] {
        let outcome = renderer
            .render_frame_with_status_and_pre_present(
                &[pane(&snap, 200, 80)],
                &TabBar::hidden(),
                &cfg,
                &Overlay {
                    window_focused: true,
                    cursor_visible: on,
                    ..Overlay::default()
                },
                &StatusBar::hidden(),
                || {},
            )
            .expect("frame");
        assert!(matches!(outcome, FrameOutcome::Presented), "{outcome:?}");
        let last = &renderer.cpu_backend_for_tests().expect("CPU").damage.last;
        assert!(last.full, "{last:?}");
    }
    assert_eq!(shared.lock().unwrap().partial, 0);
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// What one typed character and one cursor blink cost a CPU window with
/// damage tracking and with full repaints, on a screen of dense text: the
/// whole frame call, and its signature and raster passes. Run by hand:
/// `cargo test -p kettle-render --release --lib damage_frame_cost --
/// --ignored --nocapture` (or with `CARGO_PROFILE_DEV_OPT_LEVEL=3`).
#[test]
#[ignore = "timing; run by hand with optimizations"]
fn damage_frame_cost() {
    let cfg = kettle_config::Config {
        background_opacity: 1.0,
        ..crate::gpu_tests::gpu_test_config()
    };
    for (w, h, scale) in [(1600u32, 1000u32, 1.0f32), (3840, 2160, 2.0)] {
        for full in [true, false] {
            let (mut surface, shared) = RotatingSurface::steady(11);
            surface.checked = false;
            let mut renderer =
                Renderer::cpu_window_for_tests(&cfg, w, h, Box::new(surface)).expect("CPU window");
            renderer.set_scale(scale);
            renderer.set_cpu_full_repaint_for_tests(full);
            let cols = (w as f32 / renderer.cell_w).floor() as usize;
            let rows = (h as f32 / renderer.cell_h).floor() as usize;
            let mut t = term(cols, rows);
            let mut parser = Processor::<StdSyncHandler>::new();
            let mut text = Vec::new();
            for row in 0..rows.saturating_sub(1) {
                let line: String = (0..cols)
                    .map(|c| (b'!' + ((row * 7 + c * 13) % 90) as u8) as char)
                    .collect();
                text.extend_from_slice(format!("\x1b[3{}m{line}\r\n", row % 8).as_bytes());
            }
            text.extend_from_slice(b"\x1b[0m$ ");
            parser.advance(&mut t, &text);
            let mut frame = |t: &alacritty_terminal::Term<EventProxy>, on: bool| {
                let mut snap = PaneSnapshot::default();
                snap.capture(t);
                let view = pane(&snap, w, h);
                let started = Instant::now();
                let outcome = renderer
                    .render_frame_with_status_and_pre_present(
                        &[view],
                        &TabBar::hidden(),
                        &cfg,
                        &Overlay {
                            window_focused: true,
                            cursor_visible: on,
                            ..Overlay::default()
                        },
                        &StatusBar::hidden(),
                        || {},
                    )
                    .expect("frame");
                assert!(matches!(outcome, FrameOutcome::Presented), "{outcome:?}");
                let call = started.elapsed();
                let stats = renderer
                    .cpu_backend_for_tests()
                    .expect("CPU")
                    .damage
                    .last
                    .clone();
                (call, stats)
            };
            for _ in 0..5 {
                frame(&t, true);
            }
            let mut typing = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let mut blink = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for i in 0..40 {
                parser.advance(&mut t, &[b"abcdefgh"[i % 8]]);
                let (call, stats) = frame(&t, true);
                typing.0.push(call.as_secs_f64() * 1e3);
                typing.1.push(stats.sign.as_secs_f64() * 1e3);
                typing.2.push(stats.raster.as_secs_f64() * 1e3);
                typing.3.push(stats.repainted_pixels() as f64);
                let (call, stats) = frame(&t, i % 2 == 1);
                blink.0.push(call.as_secs_f64() * 1e3);
                blink.1.push(stats.sign.as_secs_f64() * 1e3);
                blink.2.push(stats.raster.as_secs_f64() * 1e3);
                blink.3.push(stats.repainted_pixels() as f64);
            }
            let mode = if full { "full repaint" } else { "damage" };
            for (what, v) in [("type a char", typing), ("cursor blink", blink)] {
                println!(
                    "DAMAGE_COST {w}x{h}@{scale} {cols}x{rows} cells {mode:12} {what:12}: frame call {:.3} ms, \
                     sign {:.3} ms, clear+raster {:.3} ms, repainted {:.0} px of {}",
                    median(v.0),
                    median(v.1),
                    median(v.2),
                    median(v.3),
                    u64::from(w) * u64::from(h),
                );
            }
            drop(shared);
        }
    }
}
