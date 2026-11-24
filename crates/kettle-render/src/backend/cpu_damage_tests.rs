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
use crate::cpu_present::damage::{DamageHistory, normalize};
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

/// What the harness reads back from the surface.
#[derive(Default)]
struct Shared {
    /// The pixels of the buffer last presented, and its size.
    presented: Option<(Vec<u32>, u32, u32)>,
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
}

/// Up to three shared-memory buffers as a Wayland compositor treats them:
/// the newest committed buffer is held until the next commit, and sometimes
/// the one before it too, so frames rotate through two or three buffers of
/// different ages.
struct RotatingSurface {
    shared: Arc<Mutex<Shared>>,
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
    /// A broken presenter for the harness's negative control: the repaint
    /// region is only this frame's damage, whatever the buffer's age.
    ignores_age: bool,
}

impl RotatingSurface {
    fn new(seed: u64) -> (Self, Arc<Mutex<Shared>>) {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let surface = Self {
            shared: shared.clone(),
            size: (0, 0),
            buffers: Vec::new(),
            held: Vec::new(),
            history: DamageHistory::default(),
            frames: 0,
            translucent: false,
            rng: Rng(seed ^ 0x5bd1_e995),
            checked: true,
            ignores_age: false,
        };
        (surface, shared)
    }

    /// Every buffer's contents become unknown, as after a resize or a
    /// format change.
    fn forget(&mut self) {
        self.history.clear();
        for buffer in &mut self.buffers {
            buffer.drawn = None;
        }
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
        self.release();
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
        let damage: Vec<crate::cpu_present::Rect> = draw
            .begin(w, h)
            .iter()
            .map(|r| crate::cpu_present::Rect::new(r.x, r.y, r.w, r.h))
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
        self.history.push(current);
        self.frames += 1;
        buffer.drawn = Some(self.frames);
        pre_commit();
        let mut shared = self.shared.lock().unwrap();
        if self.checked {
            shared.presented = Some((buffer.pixels.clone(), w, h));
        }
        shared.ages[usize::from(age.min(3))] += 1;
        let whole = repaint.len() == 1 && repaint[0] == PixelRect::new(0, 0, w, h);
        shared.partial += usize::from(!whole);
        shared.empty += usize::from(repaint.is_empty());
        shared.repainted += repainted;
        shared.total += u64::from(w) * u64::from(h);
        shared.most_buffers = shared.most_buffers.max(self.buffers.len());
        drop(shared);
        self.held.retain(|&i| i != index);
        self.held.push(index);
        CpuPresentOutcome::Presented
    }

    fn flush(&mut self, _pre_commit: &mut dyn FnMut()) -> HeldFrameFlush {
        HeldFrameFlush::Nothing
    }

    fn has_deferred(&self) -> bool {
        false
    }

    fn pending_deadline(&self) -> Option<Instant> {
        None
    }

    fn frame_pending(&mut self) -> bool {
        false
    }

    fn frame_deadline(&self) -> Option<Instant> {
        None
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
    /// A placed image and the cells it spans.
    image: Option<(kettle_core::ImageData, usize, usize)>,
    /// The image placements made so far: they cycle through a small
    /// image, a large one, and none.
    images_placed: u64,
    tabs: Option<usize>,
    scale: f32,
    rng: Rng,
}

impl World {
    fn new(seed: u64) -> Self {
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
        text.extend_from_slice(b"$ ");
        world.feed(0, &text);
        world.feed(1, b"other pane\r\n$ ");
        world
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
                        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                        for y in 0..h {
                            for x in 0..w {
                                let a = match (large, (x + y) % 5) {
                                    (true, _) => 190,
                                    (false, 0) => 120,
                                    (false, _) => 255,
                                };
                                rgba.extend_from_slice(&[(x * 9) as u8, (y * 13) as u8, 200, a]);
                            }
                        }
                        let cells = if large { (60, 20) } else { (4, 2) };
                        kettle_core::ImageData::new(w, h, rgba).map(|img| (img, cells.0, cells.1))
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
        let images: Vec<kettle_core::Placement> = self
            .image
            .iter()
            .map(|(img, cols, rows)| kettle_core::Placement {
                abs_line: snaps[0].history_origin + 1,
                col: 2,
                cell_cols: *cols,
                cell_rows: *rows,
                x_offset_cells: 0.0,
                y_offset_cells: 0.0,
                display_cols: *cols as f32,
                display_rows: *rows as f32,
                img: img.clone(),
                source_rect: None,
                source_crop: None,
                id: Some(7),
                placement_id: 0,
                kitty_params: None,
                z: 0,
            })
            .collect();
        let mut panes = Vec::new();
        let split_w = if self.split {
            (w / 2.0 - 2.0).floor()
        } else {
            w
        };
        let mut first = pane(&snaps[0], self.size.0, self.size.1);
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

/// Every kind of change, in an order that also opens, scrolls and closes
/// Settings, switches and hides tabs, and turns each toggle on and off:
/// the scripted start of every session, so every session covers them all.
const SCRIPT: [u64; 40] = [
    0, 30, 25, 20, 40, 41, 41, 46, 47, 47, 51, 54, 56, 58, 59, 59, 61, 61, 62, 64, 66, 0, 67, 69,
    // One pane again, a small image, then a large one that small changes
    // leave in place, so its second frame draws it from the cache.
    64, 71, 71, 30, 30, 74, 0, 75, 77, 78, 84, 67, 69, 74, 56, 58,
];

/// Every change's name, which the harness must all see.
const CHANGES: [&str; 23] = [
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

/// Run one seeded session of `steps` changes, checking every frame.
fn run_session(seed: u64, steps: u64) -> (Shared, Vec<&'static str>) {
    session(seed, steps, false)
}

fn session(seed: u64, steps: u64, ignores_age: bool) -> (Shared, Vec<&'static str>) {
    let mut world = World::new(seed);
    let (mut surface, shared) = RotatingSurface::new(seed);
    surface.ignores_age = ignores_age;
    let mut renderer =
        Renderer::cpu_window_for_tests(&world.cfg, world.size.0, world.size.1, Box::new(surface))
            .expect("CPU window");
    // The reference rasterizer draws every frame whole.
    let mut reference: Box<dyn CpuRaster> = Box::new(SoftRaster::new());
    let mut log: Vec<&'static str> = Vec::new();
    for step in 0..=steps {
        let change = match step {
            0 => "first frame",
            1..=40 => world.apply(&mut renderer, SCRIPT[step as usize - 1]),
            _ => world.change(&mut renderer),
        };
        log.push(change);
        let outcome = world.render(&mut renderer);
        assert!(
            matches!(outcome, FrameOutcome::Presented),
            "seed {seed} step {step} ({change}): {outcome:?}"
        );
        let want = renderer
            .cpu_full_repaint_for_tests(reference.as_mut(), &world.cfg, world.cursor_on)
            .expect("a full repaint");
        let planned = renderer
            .cpu_backend_for_tests()
            .map_or(0, |cpu| cpu.damage.last.planned);
        shared.lock().unwrap().cached += usize::from(planned > 0);
        let (got, w, h) = shared
            .lock()
            .unwrap()
            .presented
            .take()
            .expect("a frame was presented");
        assert_eq!(
            (w, h),
            (want.1, want.2),
            "seed {seed} step {step} ({change}): buffer size"
        );
        if got != want.0 {
            let recent = &log[log.len().saturating_sub(8)..];
            panic!(
                "seed {seed} step {step} ({change}): the damage-tracked buffer differs from a \
                 full repaint: {}; damage {:?}; recent changes {recent:?}",
                describe_difference(&got, &want.0, w),
                renderer.cpu_backend_for_tests().map(|cpu| &cpu.damage.last),
            );
        }
    }
    let stats = std::mem::take(&mut *shared.lock().unwrap());
    (stats, log)
}

#[test]
fn damage_tracked_frames_equal_full_repaints_through_random_sessions() {
    let seeds = env_count("KETTLE_DAMAGE_SEEDS", 3);
    let steps = env_count("KETTLE_DAMAGE_STEPS", 70).max(40);
    let mut total = Shared::default();
    let mut seen = std::collections::BTreeSet::new();
    for seed in 1..=seeds {
        let (s, log) = run_session(seed * 0x9e37_79b9, steps);
        seen.extend(log);
        for (t, a) in total.ages.iter_mut().zip(s.ages) {
            *t += a;
        }
        total.partial += s.partial;
        total.cached += s.cached;
        total.empty += s.empty;
        total.repainted += s.repainted;
        total.total += s.total;
        total.most_buffers = total.most_buffers.max(s.most_buffers);
    }
    let frames: usize = total.ages.iter().sum();
    eprintln!(
        "damage harness: {seeds} seeds x {steps} steps, {frames} frames; ages 0/1/2/3+ {:?}; \
         {} partial, {} empty, {} with a cached image; repainted {:.1}% of the pixels presented; \
         up to {} buffers",
        total.ages,
        total.partial,
        total.empty,
        total.cached,
        100.0 * total.repainted as f64 / total.total.max(1) as f64,
        total.most_buffers,
    );
    // The harness must exercise what it claims to: every change, buffers of
    // every age, and frames that repaint only part of the buffer.
    let missing: Vec<_> = CHANGES.iter().filter(|c| !seen.contains(*c)).collect();
    assert!(missing.is_empty(), "changes never made: {missing:?}");
    assert!(total.most_buffers >= 2, "frames rotated through one buffer");
    assert!(
        total.ages[1] > 0 && total.ages[2] > 0,
        "ages {:?}",
        total.ages
    );
    assert!(
        total.partial * 4 > frames,
        "too few partial frames: {}",
        total.partial
    );
    assert!(
        total.cached > 0,
        "no frame drew an image from the rasterizer's cache"
    );
}

/// The harness catches a presenter that hands back an old buffer without
/// the damage of the frames it missed.
#[test]
#[should_panic(expected = "differs from a full repaint")]
fn the_harness_fails_a_presenter_that_ignores_buffer_age() {
    session(5, 60, true);
}

/// One typed character and one blink at `size`: the frame repaints only
/// the cells involved, not the window.
fn small_changes_repaint_little(size: (u32, u32), scale: f32) {
    let cfg = kettle_config::Config {
        background_opacity: 1.0,
        ..crate::gpu_tests::gpu_test_config()
    };
    let (surface, shared) = RotatingSurface::new(7);
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
    let (surface, shared) = RotatingSurface::new(3);
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
            let (mut surface, shared) = RotatingSurface::new(11);
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
